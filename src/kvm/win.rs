//! Windows 的键鼠抓取与注入。
//!
//! 抓取用低级钩子 `WH_MOUSE_LL` / `WH_KEYBOARD_LL`：钩子回调返回非零即
//! 拦下事件 —— **按键和点击拦得住，指针移动拦不住**（v0.6.41 实锤：
//! 吞掉 WM_MOUSEMOVE 之后光标照走，一路顶到屏幕边上被系统钳死，位置不再
//! 变化，算出来的位移退化成 0/±1）。所以抓取期间必须自己把光标按在
//! 钉扎点上，见 `set_pin` / `pin_back_if_drifted`。
//! 钩子必须跑在**有消息循环**的线程上，所以这里单开一个线程跑 `GetMessageW`。
//!
//! 注入用 `SendInput`，并且给自己的事件打上 `dwExtraInfo` 标记：
//! 否则注入的事件会被自己的钩子再抓一遍，形成 1 秒几万次的回环。
//!
//! 键码：网络上统一走 macOS virtual keycode，这里维护一张双向映射表
//! （见 `mac_to_vk` / `vk_to_mac`）。修饰键在 Windows 上必须用
//! 扩展键标志发送，不然 AltGr、右侧 Ctrl 会错。

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::event::{hit_edge_in, within_edge, Event, Side, REARM_DIST};

type Hhook = isize;
type Hinstance = isize;
type Hwnd = isize;
type Hmonitor = *mut c_void;
type Lparam = isize;
type Wparam = usize;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RectL {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct MonitorInfoW {
    cb_size: u32,
    monitor: RectL,
    work: RectL,
    flags: u32,
}

/// GetMonitorInfoW 的 flags 位：这是主屏（任务栏/登录画面所在）
const MONITORINFOF_PRIMARY: u32 = 1;

const WH_KEYBOARD_LL: i32 = 13;
const WH_MOUSE_LL: i32 = 14;

const WM_MOUSEMOVE: usize = 0x0200;
const WM_LBUTTONDOWN: usize = 0x0201;
const WM_LBUTTONUP: usize = 0x0202;
const WM_RBUTTONDOWN: usize = 0x0204;
const WM_RBUTTONUP: usize = 0x0205;
const WM_MBUTTONDOWN: usize = 0x0207;
const WM_MBUTTONUP: usize = 0x0208;
const WM_MOUSEWHEEL: usize = 0x020A;
const WM_XBUTTONDOWN: usize = 0x020B;
const WM_XBUTTONUP: usize = 0x020C;
const WM_KEYDOWN: usize = 0x0100;
const WM_KEYUP: usize = 0x0101;
const WM_SYSKEYDOWN: usize = 0x0104;
const WM_SYSKEYUP: usize = 0x0105;
const WM_QUIT: u32 = 0x0012;
const WM_INPUT: u32 = 0x00FF;

/// 注册原始输入：即使不在前台也收（钩子线程本来就没窗口在前台）
const RIDEV_INPUTSINK: u32 = 0x0000_0100;
const RIM_TYPEMOUSE: u32 = 0;
const RID_INPUT: u32 = 0x1000_0003;
/// RAWMOUSE.usFlags：置位表示 lLastX/lLastY 是绝对坐标（远程桌面 / 某些
/// 虚拟鼠标），不是相对位移 —— 那种情况直接跳过，别把绝对坐标当位移发
const MOUSE_MOVE_ABSOLUTE: u16 = 0x0001;
/// 窗口样式：不可见的弹出窗，只为给原始输入当 hwndTarget
const WS_POPUP: u32 = 0x8000_0000;

const INPUT_MOUSE: u32 = 0;
const INPUT_KEYBOARD: u32 = 1;
const MOUSEEVENTF_MOVE: u32 = 0x0001;
const MOUSEEVENTF_ABSOLUTE: u32 = 0x8000;
const MOUSEEVENTF_VIRTUALDESK: u32 = 0x4000;
const MOUSEEVENTF_WHEEL: u32 = 0x0800;
const MOUSEEVENTF_HWHEEL: u32 = 0x1000;
const MOUSEEVENTF_LEFTDOWN: u32 = 0x0002;
const MOUSEEVENTF_LEFTUP: u32 = 0x0004;
const MOUSEEVENTF_RIGHTDOWN: u32 = 0x0008;
const MOUSEEVENTF_RIGHTUP: u32 = 0x0010;
const MOUSEEVENTF_MIDDLEDOWN: u32 = 0x0020;
const MOUSEEVENTF_MIDDLEUP: u32 = 0x0040;
const KEYEVENTF_KEYUP: u32 = 0x0002;
const KEYEVENTF_EXTENDEDKEY: u32 = 0x0001;

const SM_CXSCREEN: i32 = 0;
const SM_CYSCREEN: i32 = 1;
const SM_XVIRTUALSCREEN: i32 = 76;
const SM_YVIRTUALSCREEN: i32 = 77;
const SM_CXVIRTUALSCREEN: i32 = 78;
const SM_CYVIRTUALSCREEN: i32 = 79;

/// 自己注入的事件带这个标记，钩子里看到就放行（避免把注入的事件又抓回来）
const INJECT_TAG: usize = 0x4953_5756; // "ISWV"

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct POINT {
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct MSLLHOOKSTRUCT {
    pt: POINT,
    mouse_data: u32,
    flags: u32,
    time: u32,
    extra: usize,
}

/// 原始输入的头部（x64：4+4+8+8 = 24 字节）
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RawInputHeader {
    dw_type: u32,
    dw_size: u32,
    h_device: isize,
    w_param: isize,
}

/// RAWMOUSE（x64：24 字节）。联合体里我们只关心 ul_buttons 那一支，
/// 按键照旧走低级钩子，这里只取位移。
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RawMouse {
    us_flags: u16,
    ul_buttons: u32,
    ul_raw_buttons: u32,
    last_x: i32,
    last_y: i32,
    extra: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RawInputDevice {
    us_usage_page: u16,
    us_usage: u16,
    dw_flags: u32,
    hwnd_target: Hwnd,
}

#[repr(C)]
struct WndClassW {
    style: u32,
    lpfn_wnd_proc: isize,
    cb_cls_extra: i32,
    cb_wnd_extra: i32,
    h_instance: Hinstance,
    h_icon: isize,
    h_cursor: isize,
    hbr_background: isize,
    lpsz_menu_name: *const u16,
    lpsz_class_name: *const u16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct KBDLLHOOKSTRUCT {
    vk_code: u32,
    scan_code: u32,
    flags: u32,
    time: u32,
    extra: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MOUSEINPUT {
    dx: i32,
    dy: i32,
    mouse_data: u32,
    flags: u32,
    time: u32,
    extra: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct KEYBDINPUT {
    vk: u16,
    scan: u16,
    flags: u32,
    time: u32,
    extra: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct HARDWAREINPUT {
    msg: u32,
    param_l: u16,
    param_h: u16,
}

#[repr(C)]
union InputUnion {
    mi: MOUSEINPUT,
    ki: KEYBDINPUT,
    hi: HARDWAREINPUT,
}

#[repr(C)]
struct INPUT {
    kind: u32,
    data: InputUnion,
}

impl Default for MOUSEINPUT {
    fn default() -> Self {
        Self {
            dx: 0,
            dy: 0,
            mouse_data: 0,
            flags: 0,
            time: 0,
            extra: INJECT_TAG,
        }
    }
}

impl Default for KEYBDINPUT {
    fn default() -> Self {
        Self {
            vk: 0,
            scan: 0,
            flags: 0,
            time: 0,
            extra: INJECT_TAG,
        }
    }
}

#[link(name = "user32")]
extern "system" {
    fn SetWindowsHookExW(id: i32, proc_: isize, module: Hinstance, thread: u32) -> Hhook;
    fn UnhookWindowsHookEx(hook: Hhook) -> i32;
    fn CallNextHookEx(hook: Hhook, code: i32, wparam: Wparam, lparam: Lparam) -> isize;
    fn GetMessageW(msg: *mut c_void, hwnd: Hwnd, min: u32, max: u32) -> i32;
    fn TranslateMessage(msg: *const c_void) -> i32;
    fn DispatchMessageW(msg: *const c_void) -> isize;
    fn PostThreadMessageW(thread: u32, msg: u32, wparam: Wparam, lparam: Lparam) -> i32;
    fn GetCursorPos(pt: *mut POINT) -> i32;
    fn SetCursorPos(x: i32, y: i32) -> i32;
    fn GetSystemMetrics(index: i32) -> i32;
    fn SendInput(count: u32, inputs: *const INPUT, size: i32) -> u32;
    fn GetForegroundWindow() -> Hwnd;
    fn EnumDisplayMonitors(
        dc: *mut c_void,
        clip: *mut c_void,
        cb: unsafe extern "system" fn(Hmonitor, *mut c_void, *mut RectL, *mut c_void) -> i32,
        data: *mut c_void,
    ) -> i32;
    fn GetMonitorInfoW(m: Hmonitor, info: *mut MonitorInfoW) -> i32;

    fn EnumWindows(cb: unsafe extern "system" fn(Hwnd, *mut c_void) -> i32, data: *mut c_void) -> i32;
    fn IsWindowVisible(hwnd: Hwnd) -> i32;
    fn IsIconic(hwnd: Hwnd) -> i32;
    fn GetWindowRect(hwnd: Hwnd, rect: *mut RectL) -> i32;
    fn GetClassNameW(hwnd: Hwnd, buf: *mut u16, max: i32) -> i32;

    fn RegisterClassW(class: *const WndClassW) -> u16;
    fn CreateWindowExW(
        ex_style: u32,
        class_name: *const u16,
        window_name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: Hwnd,
        menu: isize,
        instance: Hinstance,
        param: *mut c_void,
    ) -> Hwnd;
    fn DefWindowProcW(hwnd: Hwnd, msg: u32, wparam: Wparam, lparam: Lparam) -> isize;
    fn RegisterRawInputDevices(devices: *const RawInputDevice, count: u32, size: u32) -> i32;
    fn GetRawInputData(
        raw: isize,
        command: u32,
        data: *mut c_void,
        size: *mut u32,
        header_size: u32,
    ) -> u32;
}

#[link(name = "dwmapi")]
extern "system" {
    /// UWP 挂起窗口是 cloaked：IsWindowVisible 说可见、屏幕上却没有，
    /// 不滤掉会把计数带偏（它们常驻主屏，主屏可能正是黑屏）
    fn DwmGetWindowAttribute(hwnd: Hwnd, attr: u32, out: *mut c_void, size: u32) -> i32;
}

/// DWMWA_CLOAKED
const DWMWA_CLOAKED: u32 = 14;

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentThreadId() -> u32;
    fn GetModuleHandleW(name: *const u16) -> Hinstance;
}

pub use super::Captured;

struct Shared {
    /// 四向掩码：哪几条边通向邻居。0 = 没有门（跟随画面模式）。
    /// 用掩码而不是单个 `Side`：一机配多台时"右边有邻居"和"下边有邻居"
    /// 可以同时成立，判定时取最近的那条（见 `hit_edge_in`）。
    sides: u8,
    /// 最近一次**实际走过**的那条门（存 `Side::code()`）。越界时由钩子写，
    /// 被控进门时由 `set_portal` 写；交还把光标摆回门内侧时按它算。
    /// 用原子量而不是普通字段：钩子走的是 `with_shared` 只读借用，
    /// 为了一个 u8 把全链路改成可变借用不值当（而且钩子里拿锁的时间要短）。
    last_out: AtomicU8,
    /// 没抓取时要不要盯着屏幕边缘（只有多屏共享模式需要）
    edge_trigger: bool,
    grabbing: Arc<AtomicBool>,
    width: Arc<AtomicU32>,
    height: Arc<AtomicU32>,
    /// 多屏共享模式下"本机工作屏"的原点（全局坐标）。屏幕坐标一律先
    /// 减掉它再参与边缘判定；跟随画面模式恒 (0,0)。
    ox: AtomicI32,
    oy: AtomicI32,
    /// 被控期间是否认"本机物理按键 = 我要收回"。只在 Receiving 时开。
    ///
    /// 判据只能看钩子里的按键：**绝不能**用 `read_raw_motion` 的增量 ——
    /// 那是设备上报流、不经过钩子，对端注入的位移会被当成"本机用户在动"，
    /// 立刻 Takeback → 对端继续注入 → 自激死循环。注入事件带 `INJECT_TAG`，
    /// 在 `mouse_proc`/`key_proc` 第一行就放行了，所以走到判定点的按键
    /// 一定是人手按的。
    claim: Arc<AtomicBool>,
    /// 门是否已武装：只有武装时撞边才算"要出去"。
    ///
    /// 每次放开抓取都撤防，之后必须看到光标离开边缘 `REARM_DIST` 点才
    /// 重新武装。交还时光标按设计摆在边缘内侧 16 点，那仍在"门口"这一圈
    /// 里，手一抖就又越界出去 —— 不撤防就是"交还/抢回立刻被弹回去"。
    armed: AtomicBool,
    tx: Sender<Captured>,
}

/// 全局只能有一个钩子，用静态量把状态挂上去（钩子回调没法带 user data）
static STATE: Mutex<Option<Arc<Mutex<Shared>>>> = Mutex::new(None);

pub struct Capture {
    grabbing: Arc<AtomicBool>,
    claim: Arc<AtomicBool>,
    width: Arc<AtomicU32>,
    height: Arc<AtomicU32>,
    thread_id: Arc<AtomicU32>,
}

pub fn start_capture(opts: super::CaptureOpts, tx: Sender<Captured>) -> Result<Capture, String> {
    let (mut width, mut height) = screen_size();
    // 几何兜底（`own_rect`）只能一条边一条边地试，所以从掩码里挑一个代表。
    // 掩码为空（跟随画面模式）时恒等于老行为。
    let primary = Side::first_in(opts.sides).unwrap_or_default();
    // 多屏共享：锁定"本机工作屏"（物理用户实际看着的那块），别把主屏
    // （可能是挂着的对端 4K 黑屏）当成自己的屏幕
    let mut origin = (0i32, 0i32);
    if opts.edge_trigger {
        if let Some((ox, oy, ow, oh)) = work_screen(primary) {
            origin = (ox, oy);
            width = ow as u32;
            height = oh as u32;
            let _ = LAST_WORK
                .lock()
                .map(|mut g| *g = Some(((ox, oy, ow, oh), now_unix_ms())));
        }
    }
    let grabbing = Arc::new(AtomicBool::new(false));
    let claim = Arc::new(AtomicBool::new(false));
    let width_atom = Arc::new(AtomicU32::new(width));
    let height_atom = Arc::new(AtomicU32::new(height));
    let thread_id = Arc::new(AtomicU32::new(0));

    let shared = Arc::new(Mutex::new(Shared {
        sides: opts.sides,
        last_out: AtomicU8::new(primary.code()),
        edge_trigger: opts.edge_trigger,
        grabbing: grabbing.clone(),
        width: width_atom.clone(),
        height: height_atom.clone(),
        ox: AtomicI32::new(origin.0),
        oy: AtomicI32::new(origin.1),
        claim: claim.clone(),
        // 启动时门是武装的：光标在屏幕中间，撞边就该出去
        armed: AtomicBool::new(true),
        tx,
    }));
    *STATE.lock().map_err(|_| "状态锁坏了".to_string())? = Some(shared);

    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let tid = thread_id.clone();

    thread::spawn(move || unsafe {
        let id = GetCurrentThreadId();
        tid.store(id, Ordering::SeqCst);

        let module = GetModuleHandleW(std::ptr::null());
        let mouse = SetWindowsHookExW(WH_MOUSE_LL, mouse_proc as isize, module, 0);
        let key = SetWindowsHookExW(WH_KEYBOARD_LL, key_proc as isize, module, 0);
        if mouse == 0 || key == 0 {
            let _ = ready_tx.send(Err("SetWindowsHookEx 失败（可能被安全软件拦了）".to_string()));
            return;
        }

        // 原始输入（Raw Input）：位移的**唯一**来源。
        //
        // 低级钩子返回 1 确实拦得住光标（v0.6.42 实锤：抓取后光标停在原地，
        // 差分只剩手抖的 ±1），但也正因为光标不动了，"相邻两笔 pt 相减"
        // 这条信息链就彻底断了 —— 光标不动 ≠ 手没动。手动了多少，只有
        // 原始输入知道（lLastX/lLastY 是设备上报的增量，与光标位置无关，
        // 也不受钳位、SetCursorPos 影响）。
        let class_name: Vec<u16> = "InputSwitchRaw\0".encode_utf16().collect();
        let wc = WndClassW {
            style: 0,
            lpfn_wnd_proc: raw_wnd_proc as isize,
            cb_cls_extra: 0,
            cb_wnd_extra: 0,
            h_instance: module,
            h_icon: 0,
            h_cursor: 0,
            hbr_background: 0,
            lpsz_menu_name: std::ptr::null(),
            lpsz_class_name: class_name.as_ptr(),
        };
        RegisterClassW(&wc);
        // 0x0 的隐藏弹出窗。不用 message-only（HWND_MESSAGE）—— 那种窗口
        // 收不到原始输入，必须是有独立消息队列的普通窗口。
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            0,
            0,
            module,
            std::ptr::null_mut(),
        );
        if hwnd == 0 {
            let _ = ready_tx.send(Err("建接收窗口失败，拿不到原始输入".to_string()));
            UnhookWindowsHookEx(mouse);
            UnhookWindowsHookEx(key);
            return;
        }
        let rid = RawInputDevice {
            us_usage_page: 1, // Generic Desktop
            us_usage: 2,      // Mouse
            dw_flags: RIDEV_INPUTSINK,
            hwnd_target: hwnd,
        };
        if RegisterRawInputDevices(&rid, 1, std::mem::size_of::<RawInputDevice>() as u32) == 0 {
            let _ = ready_tx.send(Err("注册原始输入失败，键鼠共享无法工作".to_string()));
            UnhookWindowsHookEx(mouse);
            UnhookWindowsHookEx(key);
            return;
        }
        let _ = ready_tx.send(Ok(()));

        // 低级钩子要求线程有消息循环，否则事件永远送不过来
        let mut msg = std::mem::MaybeUninit::<[u8; 48]>::uninit();
        while GetMessageW(msg.as_mut_ptr() as *mut c_void, 0, 0, 0) > 0 {
            let ptr = msg.as_ptr() as *const c_void;
            TranslateMessage(ptr);
            DispatchMessageW(ptr);
        }

        UnhookWindowsHookEx(mouse);
        UnhookWindowsHookEx(key);
    });

    match ready_rx.recv_timeout(Duration::from_secs(3)) {
        Ok(Ok(())) => Ok(Capture {
            grabbing,
            claim,
            width: width_atom,
            height: height_atom,
            thread_id,
        }),
        Ok(Err(err)) => Err(err),
        Err(_) => Err("钩子线程没有在 3 秒内就绪".to_string()),
    }
}

impl Capture {
    pub fn stop(&self) {
        self.set_grabbing(false);
        let id = self.thread_id.load(Ordering::SeqCst);
        if id != 0 {
            unsafe { PostThreadMessageW(id, WM_QUIT, 0, 0) };
        }
    }

    pub fn release(&self) {
        self.set_grabbing(false);
    }

    pub fn is_grabbing(&self) -> bool {
        self.grabbing.load(Ordering::Relaxed)
    }

    /// 剪贴板"按需传文件"的粘贴键拦截只做在 macOS 一侧：
    /// 物理键盘永远接着 Mac，任何方向粘贴的按键都先经过它的钩子。
    /// Windows 端这里留空实现，保持两端接口一致。
    pub fn set_paste_ready(&self, _on: bool) {}

    /// Windows 钩子没有"被系统禁用"的机制（低级钩子被拔是静默的，
    /// 另有兜底），恒返回 false。
    pub fn revive_tap(&self) -> bool {
        false
    }

    pub fn set_grabbing(&self, on: bool) {
        let was = self.grabbing.swap(on, Ordering::SeqCst);
        if was == on {
            return;
        }
        // 不再钉扎光标：原始输入（WM_INPUT）取位移，光标停哪儿都不影响
        // 计数的准确性。抓取/释放只打一行诊断，方便判断"这条路通不通"。
        if on {
            RAW_COUNT.store(0, Ordering::Relaxed);
        } else {
            log_file(&format!(
                "抓取结束：本次收到原始位移 {} 笔",
                RAW_COUNT.load(Ordering::Relaxed)
            ));
        }
        // 松手时把光标挪回边缘内侧 —— 只在多屏共享模式下有意义。
        // 离门 16 点：太贴门口的话手一抖就又越界了。
        // 跟随画面模式下屏幕刚切回来，光标原地不动才对。
        let edge_mode = STATE
            .lock()
            .ok()
            .and_then(|s| s.as_ref().map(|m| m.lock().map(|x| x.edge_trigger).unwrap_or(false)))
            .unwrap_or(false);
        // 撤防（不论模式）：接下来光标会被摆到门口那一圈里，不撤防的话
        // 下一笔轻微移动就又出去。重新武装要等光标离开边缘 REARM_DIST 点。
        if !on {
            with_shared(|s| s.armed.store(false, Ordering::Relaxed));
        }
        if !on && edge_mode {
            let (w, h) = self.screen();
            let (ox, oy, side) = STATE
                .lock()
                .ok()
                .and_then(|s| {
                    s.as_ref().map(|m| {
                        m.lock()
                            .map(|x| {
                                (
                                    x.ox.load(Ordering::Relaxed),
                                    x.oy.load(Ordering::Relaxed),
                                    // 落点按"这一趟实际走的那道门"，不是配置里的
                                    // 那个：被控时门口方向已按对端出门侧校正成
                                    // 对面，两边口径不一致就会把光标摆到对侧
                                    Side::from_code(x.last_out.load(Ordering::Relaxed))
                                        .unwrap_or_default(),
                                )
                            })
                            .unwrap_or((0, 0, Side::Right))
                    })
                })
                .unwrap_or((0, 0, Side::Right));
            let (x, y) = match side {
                Side::Right => (w as i32 - 16, h as i32 / 2),
                Side::Left => (16, h as i32 / 2),
                Side::Top => (w as i32 / 2, 16),
                Side::Bottom => (w as i32 / 2, h as i32 - 16),
            };
            // 相对坐标 → 全局坐标（多屏时工作屏原点可能不是 0）
            unsafe { SetCursorPos(ox + x, oy + y) };
        }
    }

    /// 记下"这次是被对端从哪道门进来的"。
    ///
    /// 被控期间交还（`set_grabbing(false)`）要把光标摆到门内侧，摆哪条边不能
    /// 看配置 —— 主循环已经按对端出门侧把门口校正成对面了，平台层却还拿着
    /// 配置里那个方向，两边口径不一致，落点就会跑到对侧去。
    pub fn set_portal(&self, side: Side) {
        with_shared(|shared| {
            shared.last_out.store(side.code(), Ordering::Relaxed);
        });
        // 掩码也一并收成这一条：被控期间"把光标推到门口抢回"走的是同一个
        // 掩码（Shared 里那个 u8 是构造时定死的，这里直接改它 —— 钩子每次
        // 都在同一把锁里读，不存在半更新）。配置里那条边要是反的，交还落点
        // 对了、抢回方向仍然是反的。P4 起链路表是唯一真源：这里收窄只是
        // **临时**状态，会话结束后心跳会用 `set_mask` 把链路表那张铺回来。
        if let Ok(guard) = STATE.lock() {
            if let Some(m) = guard.as_ref() {
                if let Ok(mut s) = m.lock() {
                    s.sides = side.bit();
                }
            }
        }
    }

    /// 把掩码整张换掉（会话结束后恢复链路表那张）。
    ///
    /// 与 `set_portal` 分工：那边是"这一场该从哪条边抢回"，这里是"平时
    /// 哪几条边通向邻居"。两者共用同一个 `sides` 字段，顺序即语义 ——
    /// 所以只有心跳在**非 Receiving** 时调这里，被控期间不敢碰。
    pub fn set_mask(&self, mask: u8) {
        if let Ok(guard) = STATE.lock() {
            if let Some(m) = guard.as_ref() {
                if let Ok(mut s) = m.lock() {
                    s.sides = mask;
                }
            }
        }
    }

    /// 被控期间是否认"本机物理按键 = 我要收回"。只有 Receiving 时该开。
    ///
    /// 开了之后钩子在"没抓取"分支里只认**按下**。位移一概不看 —— 光标此时
    /// 的移动可能来自显示器切换时的坐标重置，拿它当"用户在动鼠标"会立刻
    /// 把控制权抢回来，然后又被按回去，来回弹。
    pub fn set_claim_armed(&self, on: bool) {
        self.claim.store(on, Ordering::Relaxed);
    }

    pub fn screen(&self) -> (u32, u32) {
        (
            self.width.load(Ordering::Relaxed),
            self.height.load(Ordering::Relaxed),
        )
    }
}

fn with_shared<F: FnOnce(&Shared)>(f: F) {
    if let Ok(guard) = STATE.lock() {
        if let Some(shared) = guard.as_ref() {
            if let Ok(state) = shared.lock() {
                f(&state);
            }
        }
    }
}

/// 接收原始输入的窗口过程。只有 WM_INPUT 有用，其余原样交还系统。
extern "system" fn raw_wnd_proc(hwnd: Hwnd, msg: u32, wparam: Wparam, lparam: Lparam) -> isize {
    if msg == WM_INPUT {
        read_raw_motion(lparam);
    }
    // WM_INPUT 处理完必须走 DefWindowProc，否则系统不会回收这块缓冲
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// 从 WM_INPUT 里取出鼠标的相对位移并转发。
///
/// 注意这是**设备增量**，与光标在哪儿毫无关系：钩子把光标拦在原地不动，
/// 这里照样能拿到"手移动了多少" —— 这正是 v0.6.43 要解决的事。
fn read_raw_motion(lparam: Lparam) {
    let hdr_size = std::mem::size_of::<RawInputHeader>() as u32;
    let mut size: u32 = 0;
    unsafe {
        if GetRawInputData(
            lparam as isize,
            RID_INPUT,
            std::ptr::null_mut(),
            &mut size,
            hdr_size,
        ) == u32::MAX
        {
            return;
        }
    }
    if size == 0 || size > 256 {
        return;
    }
    // 12 个 u64 = 96 字节，足够放一条鼠标 RAWINPUT（x64 约 48 字节），
    // 且天然 8 字节对齐
    let mut buf = [0u64; 12];
    let mut got = size;
    let ret = unsafe {
        GetRawInputData(
            lparam as isize,
            RID_INPUT,
            buf.as_mut_ptr() as *mut c_void,
            &mut got,
            hdr_size,
        )
    };
    if ret == u32::MAX || got < hdr_size {
        return;
    }
    let hdr = unsafe { &*(buf.as_ptr() as *const RawInputHeader) };
    if hdr.dw_type != RIM_TYPEMOUSE {
        return;
    }
    let mouse = unsafe { &*((buf.as_ptr() as *const u8).add(hdr_size as usize) as *const RawMouse) };
    // 远端/虚拟鼠标上报的是绝对坐标，当位移发出去会把对端光标瞬间甩飞
    if mouse.us_flags & MOUSE_MOVE_ABSOLUTE != 0 {
        return;
    }
    if mouse.last_x == 0 && mouse.last_y == 0 {
        return;
    }
    RAW_COUNT.fetch_add(1, Ordering::Relaxed);
    with_shared(|shared| {
        if !shared.grabbing.load(Ordering::Relaxed) {
            return;
        }
        let _ = shared.tx.send(Captured::Input(Event::Motion {
            dx: mouse.last_x,
            dy: mouse.last_y,
        }));
    });
}

/// 抓取期间收到的原始位移笔数。只用来判断"这条路通不通"（交还时落日志）。
static RAW_COUNT: AtomicU32 = AtomicU32::new(0);

extern "system" fn mouse_proc(code: i32, wparam: Wparam, lparam: Lparam) -> isize {
    if code < 0 {
        return unsafe { CallNextHookEx(0, code, wparam, lparam) };
    }
    let info = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
    // 自己注入的事件：放行
    if info.extra == INJECT_TAG {
        return unsafe { CallNextHookEx(0, code, wparam, lparam) };
    }

    let mut swallow = false;
    with_shared(|shared| {
        let grabbing = shared.grabbing.load(Ordering::Relaxed);
        if !grabbing {
            // 被控期间：本机用户真按下鼠标键 = 想把控制权拿回来。
            // 只认"按下"：切输入源时显示器会把光标坐标重置，位移在那一秒
            // 完全不可信（拿它当判据就是"抢回来又被按回去"的来回弹）。
            if shared.claim.load(Ordering::Relaxed)
                && matches!(
                    wparam,
                    WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
                )
            {
                let _ = shared.tx.send(Captured::Claim);
            }
            // 只有多屏共享模式才盯边缘。跟随画面模式下那台机器的画面已经切走，
            // 光标停在哪都不代表用户想过去。
            if shared.edge_trigger && wparam == WM_MOUSEMOVE {
                // 物理用户在哪块屏上动，哪块就是工作屏（权威信号，见
                // LAST_WORK）。换屏时同步刷新 Shared 的原点/尺寸，注入端
                // 交还 warp 用的也是这组值。
                if let Some((mx, my, mw, mh)) = monitor_at(info.pt.x, info.pt.y) {
                    if let Ok(mut guard) = LAST_WORK.lock() {
                        *guard = Some(((mx, my, mw, mh), now_unix_ms()));
                    }
                    if shared.ox.load(Ordering::Relaxed) != mx {
                        shared.ox.store(mx, Ordering::Relaxed);
                        shared.oy.store(my, Ordering::Relaxed);
                        shared.width.store(mw as u32, Ordering::Relaxed);
                        shared.height.store(mh as u32, Ordering::Relaxed);
                    } else if shared.oy.load(Ordering::Relaxed) != my {
                        shared.oy.store(my, Ordering::Relaxed);
                        shared.width.store(mw as u32, Ordering::Relaxed);
                        shared.height.store(mh as u32, Ordering::Relaxed);
                    }
                }
                let ox = shared.ox.load(Ordering::Relaxed);
                let oy = shared.oy.load(Ordering::Relaxed);
                let w = shared.width.load(Ordering::Relaxed) as f64;
                let h = shared.height.load(Ordering::Relaxed) as f64;
                // 屏幕坐标 → 本机工作屏相对坐标（多屏时全局 pt 可能落在
                // 别的屏上，不换算的话边缘判定全错）
                let rx = (info.pt.x - ox) as f64;
                let ry = (info.pt.y - oy) as f64;
                // 门的武装/撤防：光标回到屏幕内部才武装。交还/被抢回之后
                // 光标就停在门口那一圈（离边 16 点），此时**不该**再触发 ——
                // 否则"交还"和"再出去"几乎同时发生，用户看到的是鼠标被
                // 弹回去、抢回像没生效（v0.6.49 实测的来回弹）。
                if !within_edge(shared.sides, rx, ry, w, h, REARM_DIST) {
                    shared.armed.store(true, Ordering::Relaxed);
                } else if shared.armed.load(Ordering::Relaxed) {
                    if let Some(side) = hit_edge_in(shared.sides, rx, ry, w, h) {
                        // 记下"这一趟走的是哪道门"：交还时按它摆落点
                        shared.last_out.store(side.code(), Ordering::Relaxed);
                        // 沿门轴的位置比例：对端从对应高度进门
                        let along = side.along(rx, ry, w, h);
                        // 一次撞边只出一个 Begin：对端没回应时不该跟着每笔
                        // 位移重复尝试（日志每 3 秒才打一条，看不出被刷了）
                        shared.armed.store(false, Ordering::Relaxed);
                        let _ = shared.tx.send(Captured::Begin { along, side });
                    }
                }
            }
            return;
        }
        swallow = true;
        let emitted = match wparam {
            // 位移不从这里出：钩子把光标拦在原地，`pt` 跟手已经没关系了
            //（v0.6.42 实测抓取后只剩手抖的 ±1）。位移由 `read_raw_motion`
            // 从原始输入里取，这里只负责把消息吞掉，不让本机光标乱跑。
            WM_MOUSEMOVE => None,
            WM_LBUTTONDOWN => Some(Event::Button {
                button: 1,
                down: true,
            }),
            WM_LBUTTONUP => Some(Event::Button {
                button: 1,
                down: false,
            }),
            WM_RBUTTONDOWN => Some(Event::Button {
                button: 2,
                down: true,
            }),
            WM_RBUTTONUP => Some(Event::Button {
                button: 2,
                down: false,
            }),
            WM_MBUTTONDOWN => Some(Event::Button {
                button: 3,
                down: true,
            }),
            WM_MBUTTONUP => Some(Event::Button {
                button: 3,
                down: false,
            }),
            WM_XBUTTONDOWN => Some(Event::Button {
                button: if (info.mouse_data >> 16) == 2 { 5 } else { 4 },
                down: true,
            }),
            WM_XBUTTONUP => Some(Event::Button {
                button: if (info.mouse_data >> 16) == 2 { 5 } else { 4 },
                down: false,
            }),
            WM_MOUSEWHEEL => {
                let delta = ((info.mouse_data >> 16) as i16) as i32;
                Some(Event::Wheel { dx: 0, dy: delta })
            }
            _ => None,
        };
        if let Some(ev) = emitted {
            let _ = shared.tx.send(Captured::Input(ev));
        }
    });

    if swallow {
        1
    } else {
        unsafe { CallNextHookEx(0, code, wparam, lparam) }
    }
}

/// 位移不再由"相邻两笔 pt 相减"得到。
///
/// 历史：v0.6.41 以为钩子拦不住光标，于是自己钉扎 + 拽回；v0.6.42 又发现
/// 拽回本身会伪造一笔位移。v0.6.43 用 Mac 日志（首笔 -960 = 屏宽一半、
/// 之后全是 ±1）确认了真相：**钩子返回 1 是拦得住光标的**，光标停在原地
/// 不动 → pt 恒定 → 差分为 0 → 手推多久都没东西可发。
/// 所以位移改从 `read_raw_motion`（WM_INPUT / Raw Input）取，那是设备
/// 上报的增量，与光标位置、钳位、SetCursorPos 全部无关。

extern "system" fn key_proc(code: i32, wparam: Wparam, lparam: Lparam) -> isize {
    if code < 0 {
        return unsafe { CallNextHookEx(0, code, wparam, lparam) };
    }
    let info = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
    if info.extra == INJECT_TAG {
        return unsafe { CallNextHookEx(0, code, wparam, lparam) };
    }

    let mut swallow = false;
    with_shared(|shared| {
        if !shared.grabbing.load(Ordering::Relaxed) {
            // 被控期间：本机用户真按下键 = 想把控制权拿回来。
            // `info.extra == INJECT_TAG` 在上面就 return 了，所以能到这里的
            // 按键一定是人按的（对端注入的键不会被当成本机用户）。
            if shared.claim.load(Ordering::Relaxed)
                && (wparam == WM_KEYDOWN || wparam == WM_SYSKEYDOWN)
            {
                let _ = shared.tx.send(Captured::Claim);
            }
            return;
        }
        swallow = true;
        let down = wparam == WM_KEYDOWN || wparam == WM_SYSKEYDOWN;
        if let Some(mac) = vk_to_mac(info.vk_code as u16, info.scan_code as u16) {
            // Ctrl+Alt+Esc：交还控制
            if down && mac == 53 && ctrl_alt_down() {
                let _ = shared.tx.send(Captured::End);
                return;
            }
            // Ctrl+Alt+方向：把键鼠改指向那一侧的机器（v0.6.60）。
            // 抓取期间一律 swallow（上面已置 true），所以不用管抬起。
            if down && ctrl_alt_down() {
                if let Some(side) = hop_side(mac) {
                    let _ = shared.tx.send(Captured::Hop(side));
                    return;
                }
            }
            let _ = shared.tx.send(Captured::Input(Event::Key { code: mac, down }));
        }
    });

    if swallow {
        1
    } else {
        unsafe { CallNextHookEx(0, code, wparam, lparam) }
    }
}

/// 方向键（Mac keycode）→ 那一侧。Ctrl+Alt+方向 = 把键鼠改指向那一侧的机器。
///
/// 用 Mac keycode 而不是 VK：`vk_to_mac` 之后两边是同一套编码，判据只写
/// 一份（Mac 那边 `mac.rs::hop_side` 是同一张表）。
fn hop_side(code: u16) -> Option<Side> {
    match code {
        0x7B => Some(Side::Left),
        0x7C => Some(Side::Right),
        0x7E => Some(Side::Top),
        0x7D => Some(Side::Bottom),
        _ => None,
    }
}

/// 当前 Ctrl 和 Alt 是不是都按着。查键盘状态比追事件可靠。
fn ctrl_alt_down() -> bool {    const VK_CONTROL: i32 = 0x11;
    const VK_MENU: i32 = 0x12;
    unsafe {
        #[link(name = "user32")]
        extern "system" {
            fn GetAsyncKeyState(vk: i32) -> i16;
        }
        let ctrl = GetAsyncKeyState(VK_CONTROL) as u16 & 0x8000 != 0;
        let alt = GetAsyncKeyState(VK_MENU) as u16 & 0x8000 != 0;
        ctrl && alt
    }
}

/// Windows 没有辅助功能授权这一步，什么都不用做
pub fn open_accessibility_settings() {}

/// 主屏尺寸
pub fn screen_size() -> (u32, u32) {
    unsafe {
        let w = GetSystemMetrics(SM_CXSCREEN);
        let h = GetSystemMetrics(SM_CYSCREEN);
        // 画面切走时 Windows 认为一台显示器都没有，这里可能返回 0。
        // 0 会让下面所有除法和 clamp 炸掉，所以给一个保守兜底。
        if w <= 0 || h <= 0 {
            return (1920, 1080);
        }
        if w <= 0 || h <= 0 {
            (1920, 1080)
        } else {
            (w as u32, h as u32)
        }
    }
}

/// 枚举虚拟桌面上的所有显示器，返回 (x, y, 宽, 高) 列表（全局坐标）。
/// 排障日志用：两块屏的时候，布局一打出来，落点/门口落错屏的问题一眼可见。
pub fn monitor_layout() -> Vec<(i32, i32, i32, i32)> {
    let mut rects: Vec<(i32, i32, i32, i32)> = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            collect_monitors,
            &mut rects as *mut _ as *mut c_void,
        );
    }
    rects
}

unsafe extern "system" fn collect_monitors(
    m: Hmonitor,
    _dc: *mut c_void,
    _rect: *mut RectL,
    data: *mut c_void,
) -> i32 {
    let mut info = MonitorInfoW {
        cb_size: std::mem::size_of::<MonitorInfoW>() as u32,
        monitor: RectL::default(),
        work: RectL::default(),
        flags: 0,
    };
    if GetMonitorInfoW(m, &mut info) != 0 {
        let rects = &mut *(data as *mut Vec<(i32, i32, i32, i32)>);
        let r = info.monitor;
        if info.flags & MONITORINFOF_PRIMARY != 0 {
            if let Ok(mut guard) = PRIMARY_RECT.lock() {
                *guard = Some((r.left, r.top, r.right - r.left, r.bottom - r.top));
            }
        }
        rects.push((r.left, r.top, r.right - r.left, r.bottom - r.top));
    }
    1
}

/// 物理用户最近一次活动所在的显示器 —— "本机工作屏"的权威信号。
///
/// 对端那块 4K 从不下机（EDID 一直挂着），纯几何规则（最左/最右）
/// 会把它的黑屏当成工作屏，光标注进去完全不可见（v0.6.9 实测踩过）。
/// 但用户拿真鼠标在哪块屏上动，那块就是他看着的屏 —— 这个信号不会骗人。
/// 注入事件带 INJECT_TAG，在钩子入口就被放行，不会污染这个值。
///
/// **必须带时间戳**：用户可能一直待在对端（Mac）侧操作，本机物理鼠标
/// 几十分钟不动 —— 陈旧的 LAST_WORK 一样会指向黑屏（v0.6.12 首日实测：
/// 事件全发到了 Windows、注入全进黑屏，屏幕毫无反应）。过期就回退主屏。
static LAST_WORK: Mutex<Option<((i32, i32, i32, i32), u64)>> = Mutex::new(None);

/// LAST_WORK 超过这么久没刷新（本机物理鼠标没动过）就视为不可信
const LAST_WORK_FRESH_MS: u64 = 60_000;

/// 主屏（任务栏所在）的矩形。collect_monitors 枚举时顺手记录。
/// 工作屏兜底链里排在过期 LAST_WORK 之后：主屏通常是用户看着的那块，
/// 比几何规则可靠（v0.6.9 教训）也比光标位置可靠（光标可能被上次注入
/// 停在黑屏里）。
static PRIMARY_RECT: Mutex<Option<(i32, i32, i32, i32)>> = Mutex::new(None);

/// 工作屏兜底链最后一步的判定来源，排障日志用。
/// 1=物理活动屏 2=主屏 3=光标所在屏 4=几何规则
static ANCHOR_SRC: AtomicU8 = AtomicU8::new(0);

/// 最近一次工作屏判定的来源描述（排障日志用）。
pub fn work_screen_source() -> u8 {
    ANCHOR_SRC.load(Ordering::Relaxed)
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 直接落到 app.log（托盘特性关闭时静默跳过）。Inject 没有日志回调，
/// 注入层的异常只能在这里补一手。
fn log_file(msg: &str) {
    #[cfg(feature = "tray")]
    crate::tray::log_line(msg);
    #[cfg(not(feature = "tray"))]
    let _ = msg;
}

/// 包含全局坐标 (px, py) 的显示器。
fn monitor_at(px: i32, py: i32) -> Option<(i32, i32, i32, i32)> {
    monitor_layout().into_iter().find(|(x, y, w, h)| {
        px >= *x && px < x + w && py >= *y && py < y + h
    })
}

/// 每块屏上的可见顶层窗口计数（v0.6.14 的"窗口分布"信号）。
///
/// **用户的窗口永远开在他自己看着的那块屏上**——挂着的对端黑屏上
/// 没有窗口。这是唯一不依赖"用户最近在不在本机"（LAST_WORK 会过期）
/// 也不依赖"主屏设置对不对"（主屏可能恰是那块 4K，v0.6.13 兜底也踩空）
/// 的自动信号。两块屏都是 4K 时连分辨率都没法区分，只能靠它。
unsafe extern "system" fn count_window(hwnd: Hwnd, data: *mut c_void) -> i32 {
    if IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 {
        return 1;
    }
    // UWP 挂起窗口 cloaked：名义可见、屏幕上没有
    let mut cloaked: u32 = 0;
    if DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED,
        &mut cloaked as *mut u32 as *mut c_void,
        4,
    ) == 0
        && cloaked != 0
    {
        return 1;
    }
    let mut r = RectL::default();
    if GetWindowRect(hwnd, &mut r) == 0 || r.right - r.left <= 0 || r.bottom - r.top <= 0 {
        return 1;
    }
    // 壳窗口（任务栏/桌面）永远钉在主屏：主屏若是黑屏会把计数带偏
    let mut buf = [0u16; 64];
    let n = GetClassNameW(hwnd, buf.as_mut_ptr(), 64);
    if n > 0 {
        match String::from_utf16_lossy(&buf[..n as usize]).as_str() {
            "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "Progman" | "WorkerW" => return 1,
            _ => {}
        }
    }
    // 按窗口中心点归属计数
    let cx = (r.left + r.right) / 2;
    let cy = (r.top + r.bottom) / 2;
    let counts = &mut *(data as *mut Vec<(i32, i32, i32, i32, u32)>);
    for (x, y, w, h, n) in counts.iter_mut() {
        if cx >= *x && cx < *x + *w && cy >= *y && cy < *y + *h {
            *n += 1;
            break;
        }
    }
    1
}

/// 可见窗口最多的那块屏。一块窗口都没有时返回 None。
fn busiest_screen() -> Option<(i32, i32, i32, i32)> {
    let mut counts: Vec<(i32, i32, i32, i32, u32)> = monitor_layout()
        .into_iter()
        .map(|(x, y, w, h)| (x, y, w, h, 0u32))
        .collect();
    unsafe {
        EnumWindows(count_window, &mut counts as *mut _ as *mut c_void);
    }
    counts
        .into_iter()
        .max_by_key(|(_, _, _, _, n)| *n)
        .filter(|(_, _, _, _, n)| *n > 0)
        .map(|(x, y, w, h, _)| (x, y, w, h))
}

/// 本机工作屏。信号优先级：
/// 1. 物理用户**最近 60 秒内**活动的那块屏（钩子持续刷新；更早就过期）
/// 2. 可见窗口最多的那块屏（用户的窗口永远开在他看着的屏上）
/// 3. 主屏（任务栏所在 —— 但主屏可能恰是挂着的对端 4K，仅供参考）
/// 4. 当前光标所在的那块屏
/// 5. 几何规则：门口朝对端，取虚拟桌面上朝对端边界的那块（最后兜底）
///
/// 来源记进 ANCHOR_SRC 供日志判读 —— 排障时"选没选对屏"一眼可见。
fn work_screen(side: Side) -> Option<(i32, i32, i32, i32)> {
    if let Ok(guard) = LAST_WORK.lock() {
        if let Some((m, at)) = *guard {
            if now_unix_ms().saturating_sub(at) <= LAST_WORK_FRESH_MS {
                ANCHOR_SRC.store(1, Ordering::Relaxed);
                return Some(m);
            }
        }
    }
    if let Some(m) = busiest_screen() {
        ANCHOR_SRC.store(5, Ordering::Relaxed);
        return Some(m);
    }
    if let Ok(guard) = PRIMARY_RECT.lock() {
        if let Some(m) = *guard {
            ANCHOR_SRC.store(2, Ordering::Relaxed);
            return Some(m);
        }
    }
    unsafe {
        let mut pt = POINT::default();
        if GetCursorPos(&mut pt) != 0 {
            if let Some(m) = monitor_at(pt.x, pt.y) {
                ANCHOR_SRC.store(3, Ordering::Relaxed);
                return Some(m);
            }
        }
    }
    ANCHOR_SRC.store(4, Ordering::Relaxed);
    own_rect(side)
}

/// 几何兜底：门口朝对端方向，取虚拟桌面上"朝对端那侧边界"的显示器。
/// 对端在左 → 最左那块（它的左边缘才是朝对端的虚拟桌面边界）。
fn own_rect(side: Side) -> Option<(i32, i32, i32, i32)> {
    let rects = monitor_layout();
    if rects.is_empty() {
        return None;
    }
    match side {
        Side::Left => rects.iter().copied().min_by_key(|r| r.0),
        Side::Right => rects.iter().copied().max_by_key(|r| r.0 + r.2),
        Side::Top => rects.iter().copied().min_by_key(|r| r.1),
        Side::Bottom => rects.iter().copied().max_by_key(|r| r.1 + r.3),
    }
}

pub fn trusted() -> bool {    // Windows 不需要 macOS 那种辅助功能授权；能装钩子就能用
    true
}

pub fn can_post() -> bool {
    true
}

// ---------------------------------------------------------------------------
// 键码映射：macOS virtual keycode <-> Windows VK
// ---------------------------------------------------------------------------

/// macOS keycode → (VK, 是否需要扩展键)
fn mac_to_vk(code: u16) -> Option<(u16, bool)> {
    let pair = match code {
        0x00 => (0x41, false), // A
        0x01 => (0x53, false), // S
        0x02 => (0x44, false), // D
        0x03 => (0x46, false), // F
        0x04 => (0x48, false), // H
        0x05 => (0x47, false), // G
        0x06 => (0x5A, false), // Z
        0x07 => (0x58, false), // X
        0x08 => (0x43, false), // C
        0x09 => (0x56, false), // V
        0x0B => (0x42, false), // B
        0x0C => (0x51, false), // Q
        0x0D => (0x57, false), // W
        0x0E => (0x45, false), // E
        0x0F => (0x52, false), // R
        0x10 => (0x59, false), // Y
        0x11 => (0x54, false), // T
        0x12 => (0x31, false), // 1
        0x13 => (0x32, false), // 2
        0x14 => (0x33, false), // 3
        0x15 => (0x34, false), // 4
        0x16 => (0x36, false), // 6
        0x17 => (0x35, false), // 5
        0x18 => (0xBB, false), // =
        0x19 => (0x39, false), // 9
        0x1A => (0x37, false), // 7
        0x1B => (0xBD, false), // -
        0x1C => (0x38, false), // 8
        0x1D => (0x30, false), // 0
        0x1E => (0xDD, false), // ]
        0x1F => (0x4F, false), // O
        0x20 => (0x55, false), // U
        0x21 => (0xDB, false), // [
        0x22 => (0x49, false), // I
        0x23 => (0x50, false), // P
        0x25 => (0x4C, false), // L
        0x26 => (0x4A, false), // J
        0x27 => (0xDE, false), // '
        0x28 => (0x4B, false), // K
        0x29 => (0xBA, false), // ;
        0x2A => (0xDC, false), // backslash
        0x2B => (0xBC, false), // ,
        0x2C => (0xBF, false), // /
        0x2D => (0x4E, false), // N
        0x2E => (0x4D, false), // M
        0x2F => (0xBE, false), // .
        0x32 => (0xC0, false), // `
        0x24 => (0x0D, false), // Return
        0x30 => (0x09, false), // Tab
        0x31 => (0x20, false), // Space
        0x33 => (0x08, false), // Delete(backspace)
        0x35 => (0x1B, false), // Esc
        0x75 => (0x2E, true),  // forward delete
        0x7B => (0x25, true),  // Left
        0x7C => (0x27, true),  // Right
        0x7D => (0x28, true),  // Down
        0x7E => (0x26, true),  // Up
        0x73 => (0x24, true),  // Home
        0x77 => (0x23, true),  // End
        0x74 => (0x21, true),  // PageUp
        0x79 => (0x22, true),  // PageDown
        0x7A => (0x70, false), // F1
        0x78 => (0x71, false), // F2
        0x63 => (0x72, false), // F3
        0x76 => (0x73, false), // F4
        0x60 => (0x74, false), // F5
        0x61 => (0x75, false), // F6
        0x62 => (0x76, false), // F7
        0x64 => (0x77, false), // F8
        0x65 => (0x78, false), // F9
        0x6D => (0x79, false), // F10
        0x67 => (0x7A, false), // F11
        0x6F => (0x7B, false), // F12
        0x38 => (0x10, false), // Shift 左
        0x3C => (0x10, false), // Shift 右（Windows 不区分左右）
        0x3B => (0x11, false), // Control 左
        0x3E => (0x11, true),  // Control 右
        0x3A => (0x12, false), // Option -> Alt
        0x3D => (0x12, true),  // 右 Option -> AltGr
        0x37 => (0x5B, true),  // Command -> Win
        0x36 => (0x5C, true),  // 右 Command -> 右 Win
        0x39 => (0x14, false), // CapsLock
        _ => return None,
    };
    Some(pair)
}

/// Windows (VK, scanCode) → macOS keycode
fn vk_to_mac(vk: u16, scan: u16) -> Option<u16> {
    // 左右修饰键靠 scanCode 区分
    let code = match vk {
        0x10 => {
            if scan == 0x36 {
                0x3C
            } else {
                0x38
            }
        }
        0x11 => {
            if scan == 0x1D && is_extended(scan) {
                0x3E
            } else {
                0x3B
            }
        }
        0x12 => {
            if scan == 0x38 && is_extended(scan) {
                0x3D
            } else {
                0x3A
            }
        }
        0x5B => 0x37,
        0x5C => 0x36,
        0x14 => 0x39,
        0x0D => 0x24,
        0x09 => 0x30,
        0x20 => 0x31,
        0x08 => 0x33,
        0x1B => 0x35,
        0x2E => 0x75,
        0x25 => 0x7B,
        0x27 => 0x7C,
        0x28 => 0x7D,
        0x26 => 0x7E,
        0x24 => 0x73,
        0x23 => 0x77,
        0x21 => 0x74,
        0x22 => 0x79,
        0x70 => 0x7A,
        0x71 => 0x78,
        0x72 => 0x63,
        0x73 => 0x76,
        0x74 => 0x60,
        0x75 => 0x61,
        0x76 => 0x62,
        0x77 => 0x64,
        0x78 => 0x65,
        0x79 => 0x6D,
        0x7A => 0x67,
        0x7B => 0x6F,
        0x41 => 0x00,
        0x53 => 0x01,
        0x44 => 0x02,
        0x46 => 0x03,
        0x48 => 0x04,
        0x47 => 0x05,
        0x5A => 0x06,
        0x58 => 0x07,
        0x43 => 0x08,
        0x56 => 0x09,
        0x42 => 0x0B,
        0x51 => 0x0C,
        0x57 => 0x0D,
        0x45 => 0x0E,
        0x52 => 0x0F,
        0x59 => 0x10,
        0x54 => 0x11,
        0x31 => 0x12,
        0x32 => 0x13,
        0x33 => 0x14,
        0x34 => 0x15,
        0x36 => 0x16,
        0x35 => 0x17,
        0xBB => 0x18,
        0x39 => 0x19,
        0x37 => 0x1A,
        0xBD => 0x1B,
        0x38 => 0x1C,
        0x30 => 0x1D,
        0xDD => 0x1E,
        0x4F => 0x1F,
        0x55 => 0x20,
        0xDB => 0x21,
        0x49 => 0x22,
        0x50 => 0x23,
        0x4C => 0x25,
        0x4A => 0x26,
        0xDE => 0x27,
        0x4B => 0x28,
        0xBA => 0x29,
        0xDC => 0x2A,
        0xBC => 0x2B,
        0xBF => 0x2C,
        0x4E => 0x2D,
        0x4D => 0x2E,
        0xBE => 0x2F,
        0xC0 => 0x32,
        _ => return None,
    };
    Some(code)
}

fn is_extended(_scan: u16) -> bool {
    // 简化处理：右侧修饰键的区分已经在上面的 match 里按 scanCode 做过了
    false
}

/// 把事件注入本机
pub struct Inject {
    /// 本地跟踪的光标位置（像素，全局坐标）。move_by 不能每次
    /// GetCursorPos 再绝对定位——SendInput 是异步排队的，读回的是
    /// 上一笔还没处理完的旧位置，高速移动时位移被吞，表现就是
    /// "鼠标迟钝"（实测踩过）。
    /// (i32::MIN, i32::MIN) = 还没建立跟踪，先读一次实际位置。
    pos: std::sync::Mutex<(i32, i32)>,
    /// 多屏共享模式：把光标限制在"本机工作屏"内（见 own_rect）。
    /// 跟随画面模式 false，一切按主屏的老规矩走。
    edge: AtomicBool,
    own: std::sync::Mutex<Option<(i32, i32, i32, i32)>>,
    /// 账本累计位移（像素，曼哈顿距离）。注入失效判定用：
    /// 真实光标没动 + 账本走了 = 注入真的死了；
    /// 真实光标没动 + 账本也没走 = 光标被钳在屏幕边上（物理事实）。
    walked: std::sync::Mutex<f64>,
}

impl Inject {
    pub fn new() -> Self {
        Self {
            pos: std::sync::Mutex::new((i32::MIN, i32::MIN)),
            edge: AtomicBool::new(false),
            own: std::sync::Mutex::new(None),
            walked: std::sync::Mutex::new(0.0),
        }
    }

    /// 账本累计位移（像素）。见 walked 字段注释。
    pub fn walked(&self) -> f64 {
        self.walked.lock().map(|w| *w).unwrap_or(0.0)
    }

    /// 多屏共享模式下启用"本机工作屏"限定。每次接管（收到 Enter）时
    /// 调用：物理用户最后活动的那块屏就是工作屏（v0.6.9 的纯几何规则
    /// 会选中挂着的对端 4K 黑屏，光标全注进看不见的屏里）。返回锚定的
    /// 工作屏矩形供日志。
    pub fn enable_own_screen(&self, side: Side) -> Option<(i32, i32, i32, i32)> {
        let rect = work_screen(side);
        if let Some(rect) = rect {
            if let Ok(mut own) = self.own.lock() {
                *own = Some(rect);
            }
            self.edge.store(true, Ordering::Relaxed);
            // 工作屏可能和上一轮不是同一块，跟踪坐标重新读真实光标
            if let Ok(mut pos) = self.pos.lock() {
                *pos = (i32::MIN, i32::MIN);
            }
        }
        rect
    }

    fn own(&self) -> Option<(i32, i32, i32, i32)> {
        if !self.edge.load(Ordering::Relaxed) {
            return None;
        }
        self.own.lock().ok().and_then(|o| *o)
    }

    pub fn apply(&self, event: &Event) {
        match *event {
            Event::Motion { dx, dy } => self.move_by(dx, dy),
            Event::Button { button, down } => self.button(button, down),
            Event::Wheel { dx, dy } => self.scroll(dx, dy),
            Event::Key { code, down } => self.key(code, down),
            // 心跳、进出控制都不产生输入，不该被注入
            Event::Enter { .. }
            | Event::Leave
            | Event::Ping
            | Event::Pong { .. }
            | Event::Takeback { .. } => {}
        }
    }

    pub fn cursor(&self) -> Option<(f64, f64)> {
        unsafe {
            let mut pt = POINT::default();
            if GetCursorPos(&mut pt) == 0 {
                return None;
            }
            // 工作屏相对坐标：门口/边缘判定都拿它比
            if let Some((ox, oy, _, _)) = self.own() {
                return Some(((pt.x - ox) as f64, (pt.y - oy) as f64));
            }
            Some((pt.x as f64, pt.y as f64))
        }
    }

    /// 本地账本位置（工作屏相对坐标）。与 cursor()（真实光标）对比：
    /// 两者差得远 = 有外力绕过注入直接挪了真实光标。
    pub fn tracked(&self) -> Option<(f64, f64)> {
        let pos = *self.pos.lock().ok()?;
        if pos == (i32::MIN, i32::MIN) {
            return None;
        }
        if let Some((ox, oy, _, _)) = self.own() {
            return Some(((pos.0 - ox) as f64, (pos.1 - oy) as f64));
        }
        Some((pos.0 as f64, pos.1 as f64))
    }

    pub fn screen(&self) -> (f64, f64) {
        if let Some((_, _, w, h)) = self.own() {
            return (w as f64, h as f64);
        }
        let (w, h) = screen_size();
        (w as f64, h as f64)
    }

    /// 把光标摆到屏幕正中。对端刚接管时调用，
    /// 否则光标还停在上次离开的角落，看着像"鼠标没接上"。
    pub fn center(&self) {
        let (w, h) = self.screen();
        self.place_cursor(w / 2.0, h / 2.0);
    }

    /// 把光标摆到指定位置（进门落点：出门高度的对应点）。
    /// 本地跟踪坐标一并更新，后续位移从这儿累加。
    /// Edge 模式下入参是工作屏相对坐标。
    pub fn place_cursor(&self, x: f64, y: f64) {
        let (ix, iy) = if let Some((ox, oy, _, _)) = self.own() {
            (ox + x.round() as i32, oy + y.round() as i32)
        } else {
            (x.round() as i32, y.round() as i32)
        };
        unsafe { SetCursorPos(ix, iy) };
        if let Ok(mut pos) = self.pos.lock() {
            *pos = (ix, iy);
        }
    }

    unsafe fn send(&self, input: INPUT) {
        let n = SendInput(1, &input, std::mem::size_of::<INPUT>() as i32);
        if n == 0 {
            // 返回 0 = 事件被系统拒收（安全桌面/UAC 弹窗、被安全软件拦）。
            // 注入无效最终会被 mod.rs 的有效性验证发现并自动交还，
            // 这里只负责把证据落进 app.log（限流，别刷爆）。
            static LAST_WARN: AtomicU64 = AtomicU64::new(0);
            let now = now_unix_ms();
            if now.saturating_sub(LAST_WARN.load(Ordering::Relaxed)) > 3000 {
                LAST_WARN.store(now, Ordering::Relaxed);
                log_file("SendInput 返回 0：注入事件被系统拒收（安全桌面或被拦截），本次注入无效");
            }
        }
    }

    fn move_by(&self, dx: i32, dy: i32) {
        unsafe {
            // Edge 模式：钳位与绝对注入都以"本机工作屏"为准，并把绝对
            // 坐标铺满整个虚拟桌面（VIRTUALDESK）——只铺主屏的话，工作屏
            // 不是主屏时坐标会被 Windows 掰到别的屏上去。
            let own = self.own();
            let (minx, miny, maxx, maxy) = if let Some((ox, oy, w, h)) = own {
                (ox, oy, ox + w - 1, oy + h - 1)
            } else {
                let (w, h) = screen_size();
                (0, 0, (w as i32 - 1).max(0), (h as i32 - 1).max(0))
            };
            // 用本地跟踪的位置累加，不要 GetCursorPos 读回——绝对注入是
            // 异步的，读回旧坐标会吞位移（迟钝的根因）。
            let mut tracked = self.pos.lock().map(|p| *p).unwrap_or((i32::MIN, i32::MIN));
            if tracked == (i32::MIN, i32::MIN) {
                let mut pt = POINT::default();
                GetCursorPos(&mut pt);
                tracked = (pt.x, pt.y);
            }
            let nx = (tracked.0 + dx).clamp(minx, maxx.max(minx));
            let ny = (tracked.1 + dy).clamp(miny, maxy.max(miny));
            if let Ok(mut pos) = self.pos.lock() {
                *pos = (nx, ny);
            }
            // 账本实际走了多少（钳位后的净位移）：光标顶死在屏幕边上时
            // 这里几乎不涨，是"没动是物理事实"的判据
            if let Ok(mut walked) = self.walked.lock() {
                *walked += (nx - tracked.0).abs() as f64 + (ny - tracked.1).abs() as f64;
            }
            // 绝对坐标要归一化到 0..65535。分母必须保证 >= 1，
            // 否则尺寸异常时这里会直接除零。
            let (ax, ay, flags) = if let Some((_, _, w, h)) = own {
                let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
                let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
                let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2);
                let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2);
                (
                    ((nx - vx) as i64 * 65535 / (vw as i64 - 1)) as i32,
                    ((ny - vy) as i64 * 65535 / (vh as i64 - 1)) as i32,
                    MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                )
            } else {
                let (w, h) = screen_size();
                (
                    (nx as i64 * 65535 / (w.max(2) as i64 - 1)) as i32,
                    (ny as i64 * 65535 / (h.max(2) as i64 - 1)) as i32,
                    MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE,
                )
            };
            let input = INPUT {
                kind: INPUT_MOUSE,
                data: InputUnion {
                    mi: MOUSEINPUT {
                        dx: ax,
                        dy: ay,
                        flags,
                        ..Default::default()
                    },
                },
            };
            self.send(input);
        }
    }

    fn button(&self, button: u8, down: bool) {
        let flag = match (button, down) {
            (1, true) => MOUSEEVENTF_LEFTDOWN,
            (1, false) => MOUSEEVENTF_LEFTUP,
            (2, true) => MOUSEEVENTF_RIGHTDOWN,
            (2, false) => MOUSEEVENTF_RIGHTUP,
            (_, true) => MOUSEEVENTF_MIDDLEDOWN,
            (_, false) => MOUSEEVENTF_MIDDLEUP,
        };
        unsafe {
            let input = INPUT {
                kind: INPUT_MOUSE,
                data: InputUnion {
                    mi: MOUSEINPUT {
                        flags: flag,
                        ..Default::default()
                    },
                },
            };
            self.send(input);
        }
    }

    fn scroll(&self, dx: i32, dy: i32) {
        unsafe {
            if dy != 0 {
                let input = INPUT {
                    kind: INPUT_MOUSE,
                    data: InputUnion {
                        mi: MOUSEINPUT {
                            mouse_data: dy as u32,
                            flags: MOUSEEVENTF_WHEEL,
                            ..Default::default()
                        },
                    },
                };
                self.send(input);
            }
            if dx != 0 {
                let input = INPUT {
                    kind: INPUT_MOUSE,
                    data: InputUnion {
                        mi: MOUSEINPUT {
                            mouse_data: dx as u32,
                            flags: MOUSEEVENTF_HWHEEL,
                            ..Default::default()
                        },
                    },
                };
                self.send(input);
            }
        }
    }

    fn key(&self, code: u16, down: bool) {
        let Some((vk, extended)) = mac_to_vk(code) else {
            return;
        };
        let mut flags = if down { 0 } else { KEYEVENTF_KEYUP };
        if extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        unsafe {
            let input = INPUT {
                kind: INPUT_KEYBOARD,
                data: InputUnion {
                    ki: KEYBDINPUT {
                        vk,
                        scan: 0,
                        flags,
                        ..Default::default()
                    },
                },
            };
            self.send(input);
        }
    }
}

impl Default for Inject {
    fn default() -> Self {
        Self::new()
    }
}

pub const MODIFIER_KEYS: [u16; 8] = [0x37, 0x36, 0x3B, 0x3E, 0x38, 0x3C, 0x3A, 0x3D];
