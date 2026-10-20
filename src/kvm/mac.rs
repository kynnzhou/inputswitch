//! macOS 的键鼠抓取与注入。
//!
//! 抓取用 `CGEventTap`：它能在事件送到系统之前看到并**吞掉**事件
//! （回调返回 NULL 即拦下），这是做 KVM 的唯一办法 —— `rdev` 这类库
//! 只能旁观，拦不住本地光标。
//!
//! 冻结本机光标是三层防线（v0.6.21）：
//! ① 位移事件**改写后放行**：原始 delta 先读出来转发给对端，再把事件的
//!    delta 清零、位置钉在钉扎点后放行。单纯吞掉（返回 NULL）在新系统上
//!    拦不住光标 —— tap 看到的 mouse-moved 是光标动过之后的"结果"，
//!    位置早已更新；而改写会被系统采纳（BetterMouse / LinearMouse 改
//!    加速度曲线就是这个做法），零 delta = 光标走不动；若系统按事件的
//!    location 摆光标，这笔事件本身就是一次 1000Hz 的纠偏。
//! ② `CGAssociateMouseAndMouseCursorPosition(false)`：鼠标与光标解耦。
//!    它会被系统反复重置，由钉扎线程周期性重申。
//! ③ 钉扎线程兜底：跑偏超阈值就把光标拉回钉扎点。
//! 按键/点击/滚轮不走①：直接吞掉，本机 app 不该收到它们。
//! 松开时把光标接回去，并摆回屏幕边缘内侧，手感才连续。
//!
//! 权限：吞事件属于"辅助功能"范畴，未授权时 tap 会被系统静默禁用，
//! 表现为"看着在跑但没反应"。所以启动前先用 `AXIsProcessTrusted()` 查一次。

use std::ffi::c_void;
use std::os::raw::{c_int, c_long};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::event::{hit_edge_in, Event, Side};

type CFTypeRef = *mut c_void;
type CGEventRef = *mut c_void;
type CGEventSourceRef = *mut c_void;
type CGEventTapProxy = *mut c_void;
type CFMachPortRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CGDirectDisplayID = u32;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

type CGEventTapCallBack = extern "C" fn(
    proxy: CGEventTapProxy,
    type_: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef;

/// 注入位置：HID 层最先收到，兼容性最好。
const CG_HID_EVENT_TAP: u32 = 0;
/// 抓取位置：会话层，能拿到所有 app 的输入。
const CG_SESSION_EVENT_TAP: u32 = 1;
const CG_HEAD_INSERT: u32 = 0;
const CG_TAP_DEFAULT: u32 = 0;
const CG_SOURCE_HID: u32 = 1;

const EV_LEFT_DOWN: u32 = 1;
const EV_LEFT_UP: u32 = 2;
const EV_RIGHT_DOWN: u32 = 3;
const EV_RIGHT_UP: u32 = 4;
const EV_MOUSE_MOVED: u32 = 5;
const EV_LEFT_DRAGGED: u32 = 6;
const EV_RIGHT_DRAGGED: u32 = 7;
const EV_KEY_DOWN: u32 = 10;
const EV_KEY_UP: u32 = 11;
const EV_FLAGS_CHANGED: u32 = 12;
const EV_SCROLL: u32 = 22;
const EV_OTHER_DOWN: u32 = 25;
const EV_OTHER_UP: u32 = 26;
const EV_OTHER_DRAGGED: u32 = 27;
const EV_TAP_DISABLED_TIMEOUT: u32 = 0xFFFF_FFFE;
const EV_TAP_DISABLED_USER: u32 = 0xFFFF_FFFF;

const FIELD_MOUSE_DELTA_X: u32 = 4;
const FIELD_MOUSE_DELTA_Y: u32 = 5;
const FIELD_KEYBOARD_KEYCODE: u32 = 9;
const FIELD_MOUSE_BUTTON: u32 = 3;
const FIELD_SCROLL_AXIS1: u32 = 11;
const FIELD_SCROLL_AXIS2: u32 = 12;
/// CGEvent 自带的 user data 字段，用来给自己的注入事件打标记
const FIELD_SOURCE_USER_DATA: u32 = 42;
/// 标记值，随便取的魔数，只要不和别的软件撞
const INJECT_TAG: c_long = 0x4953_5756;

const BUTTON_LEFT: u32 = 0;
const BUTTON_RIGHT: u32 = 1;
const BUTTON_CENTER: u32 = 2;

/// 修饰键在 CGEventFlags 里的位
const FLAG_SHIFT: u64 = 0x2_0000;
const FLAG_CONTROL: u64 = 0x4_0000;
const FLAG_OPTION: u64 = 0x8_0000;
const FLAG_COMMAND: u64 = 0x10_0000;
const FLAG_CAPS: u64 = 0x1_0000;

/// Esc 的 keycode，配合 Ctrl+Alt 作为"交还控制"的手势
const KEY_ESCAPE: u16 = 53;
/// V 的 keycode。Windows 上的"粘贴"是 Ctrl+V，Mac 键盘的 Control 键
/// （0x3B）转发到 Windows 是 VK_CONTROL —— 所以拦的是 Control，不是 Command。
/// Command 在 Windows 侧映射成 Win 键（Win+V 是剪贴板历史，不是粘贴）。
const KEY_V: u16 = 0x09;
/// 左 Control 的 keycode（重放粘贴时补按用）
const KEY_CONTROL_LEFT: u16 = 0x3B;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventTapIsEnabled(tap: CFMachPortRef) -> bool;
    fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
    fn CGEventGetFlags(event: CGEventRef) -> u64;
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> c_long;
    fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: c_long);
    fn CGEventSetType(event: CGEventRef, type_: u32);
    fn CGEventCreate(source: CGEventSourceRef) -> CGEventRef;
    fn CGEventCreateMouseEvent(
        source: CGEventSourceRef,
        mouse_type: u32,
        position: CGPoint,
        button: u32,
    ) -> CGEventRef;
    fn CGEventCreateKeyboardEvent(source: CGEventSourceRef, key: u16, key_down: bool) -> CGEventRef;
    fn CGEventPost(tap: u32, event: CGEventRef);
    fn CGEventSourceCreate(state: u32) -> CGEventSourceRef;
    fn CGEventSourceSetLocalEventsSuppressionInterval(source: CGEventSourceRef, seconds: f64);
    fn CGWarpMouseCursorPosition(position: CGPoint) -> c_int;
    fn CGAssociateMouseAndMouseCursorPosition(connected: bool) -> c_int;
    /// 改写事件的绝对位置（全局显示坐标）。钉扎位移事件用。
    fn CGEventSetLocation(event: CGEventRef, location: CGPoint);
    /// **无声移动光标**：把光标摆到指定位置，但不产生任何鼠标事件
    ///（文档明说不生成事件）。所以不会回流进自己的 HID 钩子、不会被
    /// 转发给对端 —— 这是唯一能绕开"事件改写影响不了 WindowServer
    /// 原始指针通路"的手段。游戏锁鼠标（associate + 每帧回中）用的就是它。
    fn CGDisplayMoveCursorToPoint(display: CGDirectDisplayID, point: CGPoint) -> c_int;
    fn CGMainDisplayID() -> CGDirectDisplayID;
    fn CGDisplayBounds(display: CGDirectDisplayID) -> CGRect;
    fn CGPreflightPostEventAccess() -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFMachPortCreateRunLoopSource(
        allocator: CFTypeRef,
        port: CFMachPortRef,
        order: c_long,
    ) -> CFRunLoopSourceRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFTypeRef);
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopRun();
    fn CFRunLoopStop(rl: CFRunLoopRef);
    fn CFRelease(cf: *const c_void);
    static kCFRunLoopCommonModes: CFTypeRef;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

pub use super::Captured;

/// 抓取回调与主逻辑之间共享的状态
struct Shared {
    /// 四向掩码：哪几条边通向邻居。0 = 没有门（跟随画面模式）。
    /// 用掩码而不是单个 `Side`：一机配多台时"右边有邻居"和"下边有邻居"
    /// 可以同时成立，判定时取最近的那条（见 `hit_edge_in`）。
    sides: u8,
    /// 最近一次**实际走过**的那条门：越界时由钩子写入，被控进门时由
    /// `set_portal` 写入。交还 warp 的落点按它算 —— 掩码是"我以为的门"，
    /// 这个才是"这一趟走的那道门"。
    last_out: Side,
    /// 没抓取时要不要盯着屏幕边缘（只有多屏共享模式需要）
    edge_trigger: bool,
    grabbing: Arc<AtomicBool>,
    width: Arc<AtomicU64>,
    height: Arc<AtomicU64>,
    tap: Arc<AtomicUsize>,
    tx: Sender<Captured>,
    /// 光标钉扎点（点）。Some = 正在钉扎：转发期间每笔位移事件都会被
    /// 改写"delta=0、位置=钉扎点"后放行，钉扎线程也以它为拉回目标。
    /// 放在 Shared 里（而不是单独一把锁）：回调本来就已经拿着这把锁，
    /// 读钉扎点不用多进一次锁；锁内只做字段读写，不会有死锁。
    pin: Option<(f64, f64)>,
    /// v0.6.39：本机手上有"本次复制的文件清单"，转发期间按 Ctrl+V 要拦下来，
    /// 先传文件再补按。这是唯一会让 tap 主动吞键的新条件。
    paste_ready: Arc<AtomicBool>,
    /// 刚被吞掉的那个键码：它的 key-up 也要一起吞，否则对端会收到一个
    /// 没有对应 down 的孤立 up（部分程序按这个判"按键卡住"）。
    swallowed_key: Option<u16>,
}

pub struct Capture {
    shared: Arc<Mutex<Shared>>,
    grabbing: Arc<AtomicBool>,
    /// set_grabbing 的互斥锁：改标志位和重申 CGAssociate 必须绑在一起，
    /// 并发交错会留下「标志=true（在转发）但光标关联还开着」的错位 ——
    /// 表现是两边光标同步移动、控制权像还在对端（实测踩过）。
    grab_lock: Mutex<()>,
    /// 交还 warp 专用的私有事件源（存 usize 保 Send）。见 set_grabbing 里
    /// 关于 0.25 秒抑制窗口的注释。
    warp_source: usize,
    /// 钉扎拉回次数（associate 失效的证据，日志诊断用）
    corrections: Arc<AtomicU64>,
    width: Arc<AtomicU64>,
    height: Arc<AtomicU64>,
    tap: Arc<AtomicUsize>,
    runloop: Arc<Mutex<Option<usize>>>,
    /// 与 Shared 里同名标志共享：tray 手上有没有"待传的文件清单"
    paste_ready: Arc<AtomicBool>,
}

/// 起一个后台线程跑 CGEventTap。返回的对象活着时抓取就有效。
pub fn start_capture(opts: super::CaptureOpts, tx: Sender<Captured>) -> Result<Capture, String> {
    if !trusted() {
        return Err(
            "没有辅助功能权限：系统设置 → 隐私与安全性 → 辅助功能，把 InputSwitch 勾上".into(),
        );
    }
    let (width, height) = screen_size();
    // 用定点数存进 Atomic：宽度千分之一，精度远够判边缘
    let width_atom = Arc::new(AtomicU64::new((width * 1000.0) as u64));
    let height_atom = Arc::new(AtomicU64::new((height * 1000.0) as u64));
    let grabbing = Arc::new(AtomicBool::new(false));
    let tap_slot: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
    let runloop: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));
    let corrections: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    // 交还 warp 用的私有事件源。**必须在此刻就把 local events suppression
    // interval 设成 0**：CGWarpMouseCursorPosition 之后系统默认会忽略硬件
    // 鼠标事件 0.25 秒（Apple 未写进文档，Ventura/Sonoma/Sequoia 实测都在），
    // 用户感知就是「从对端回到 Mac，光标先冻一下才跟着手走」。
    // 抑制间隔挂在事件源上，所以给 warp 单独开一个源、把它清零，
    // 别去动 CGEventSourceCreate(null) 那个全局默认源。
    let warp_source = unsafe {
        let src = CGEventSourceCreate(0 /* kCGEventSourceStateCombinedSessionState */);
        if !src.is_null() {
            CGEventSourceSetLocalEventsSuppressionInterval(src, 0.0);
        }
        src as usize
    };
    let grab_lock: Mutex<()> = Mutex::new(());
    let paste_ready = Arc::new(AtomicBool::new(false));

    let shared = Arc::new(Mutex::new(Shared {
        sides: opts.sides,
        last_out: Side::from_bit(opts.sides).unwrap_or_default(),
        edge_trigger: opts.edge_trigger,
        grabbing: grabbing.clone(),
        width: width_atom.clone(),
        height: height_atom.clone(),
        tap: tap_slot.clone(),
        tx,
        pin: None,
        paste_ready: paste_ready.clone(),
        swallowed_key: None,
    }));

    // 光标钉扎线程（v0.6.21 调整为兜底层）：转发期间光标必须停在钉扎点。
    // 第一道防线在回调里（每笔位移事件改写"delta=0、位置=钉扎点"后放行），
    // 这一线程负责两件事：①每 16ms 无条件重申 CGAssociate(false)——
    // 它会被系统悄悄重置，重申把漂移按死在一帧内；②万一光标仍跑偏超过
    // 2 点（改写与 associate 同时失效的极端情况），拉回钉扎点。
    // 拉回不能用 CGWarp：warp 产生的 mouse-moved 会从 HID 层进事件流、
    // 不带 INJECT_TAG，会被自己的钩子当真实手移转发给对端。改为 post
    // 一笔带标签的绝对定位事件：走会话层（我们 HID 钩子的下游），
    // 物理上不可能被自己的钩子看到转发。
    {
        let shared_for_pin = shared.clone();
        let corr_for_pin = corrections.clone();
        // 监测节拍：每 64 个 tick（约 1 秒）落一条"账面位置 vs 钉扎点"，
        // 用来对账"系统 API 读数"和"用户肉眼所见"是否一致（v0.6.22 排障用）
        let mut tick: u32 = 0;
        thread::spawn(move || loop {
            thread::sleep(Duration::from_millis(16));
            tick = tick.wrapping_add(1);
            // 钉扎点与抓取标志**一起**读，且必须真的在转发才动手。
            //
            // v0.6.40：以前只读 pin。释放路径和这一拍并发时，这里可能
            // 拿着"释放前读到的 pin"继续往下跑，最后那句无条件的
            // CGAssociate(false) 落在 set_grabbing(false) 的
            // CGAssociate(true) **之后** —— 终态是"光标与鼠标断开"。
            // 而之后没有任何路径会把它接回去：进被控状态时抓取标志
            // 本来就是 false，守卫看着"一致"，不会动它。结果 = Mac 被
            // 控时注入的事件推不动真实光标（日志实锤：账本走 271 点、
            // 真实光标动 0.0 点），几秒后被判"注入失效"把键鼠抢回去。
            let (target, grabbing) = match shared_for_pin.lock() {
                Ok(s) => (s.pin, s.grabbing.load(Ordering::Relaxed)),
                Err(_) => continue,
            };
            if !grabbing {
                continue;
            }
            let Some(target) = target else {
                continue;
            };
            unsafe {
                let ev = CGEventCreate(std::ptr::null_mut());
                if ev.is_null() {
                    continue;
                }
                let cur = CGEventGetLocation(ev);
                CFRelease(ev as *const c_void);
                let drift = (cur.x - target.0).abs() + (cur.y - target.1).abs();
                if tick % 64 == 0 {
                    if let Ok(s) = shared_for_pin.lock() {
                        let n = corr_for_pin.load(Ordering::Relaxed);
                        let _ = s.tx.send(Captured::Note(format!(
                            "钉扎监测：账面光标({:.0},{:.0}) 钉扎点({:.0},{:.0}) 漂移{drift:.1} 累计拉回{n}",
                            cur.x, cur.y, target.0, target.1
                        )));
                    }
                }
                if drift > 2.0 {
                    // 带标签的绝对定位事件，从会话层入流（理由见线程头注释）
                    let warp = CGEventCreateMouseEvent(
                        std::ptr::null_mut(),
                        EV_MOUSE_MOVED,
                        CGPoint {
                            x: target.0,
                            y: target.1,
                        },
                        BUTTON_LEFT,
                    );
                    if !warp.is_null() {
                        CGEventSetIntegerValueField(warp, FIELD_SOURCE_USER_DATA, INJECT_TAG);
                        CGEventPost(CG_SESSION_EVENT_TAP, warp);
                        CFRelease(warp as *const c_void);
                    }
                    // post 完再断开关联：post 的事件可能隐式重连光标，
                    // associate 必须放在最后保证最终态是"断开"
                    CGAssociateMouseAndMouseCursorPosition(false);
                    let n = corr_for_pin.fetch_add(1, Ordering::Relaxed) + 1;
                    // 前 10 次都落日志（看清失效频率），之后每 25 次一条
                    if n <= 10 || n % 25 == 0 {
                        if let Ok(s) = shared_for_pin.lock() {
                            let _ = s.tx.send(Captured::Note(format!(
                                "⚠️ 光标关联失效（已拉回 {n} 次），转发期间本机光标已被钉扎"
                            )));
                        }
                    }
                }
                // 无条件重申断开：系统会悄悄重置关联，每 16ms 顶回去，
                // 把漂移按死在一帧内。**断言前再确认一次抓取还开着**：
                // 释放可能刚好落在这一拍中间（v0.6.40），这时反而要把
                // 光标接回去并立刻收手，别把断开状态留给被控期。
                let still = shared_for_pin
                    .lock()
                    .map(|s| s.grabbing.load(Ordering::Relaxed))
                    .unwrap_or(false);
                if still {
                    CGAssociateMouseAndMouseCursorPosition(false);
                } else {
                    CGAssociateMouseAndMouseCursorPosition(true);
                }
            }
        });
    }

    // 这块内存故意不回收：它要活到进程结束，作为 tap 的 userInfo。
    // 存成 usize 再进线程：裸指针不是 Send，Rust 不让跨线程传。
    let boxed_addr = Box::into_raw(Box::new(shared.clone())) as usize;
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let tap_for_thread = tap_slot.clone();
    let runloop_for_thread = runloop.clone();

    thread::spawn(move || unsafe {
        let types = [
            EV_LEFT_DOWN,
            EV_LEFT_UP,
            EV_RIGHT_DOWN,
            EV_RIGHT_UP,
            EV_MOUSE_MOVED,
            EV_LEFT_DRAGGED,
            EV_RIGHT_DRAGGED,
            EV_KEY_DOWN,
            EV_KEY_UP,
            EV_FLAGS_CHANGED,
            EV_SCROLL,
            EV_OTHER_DOWN,
            EV_OTHER_UP,
            EV_OTHER_DRAGGED,
        ];
        let mut mask: u64 = 0;
        for t in types {
            mask |= 1u64 << t;
        }

        // 挂在 HID 层（v0.6.19）：会话层 tap 吞掉事件也拦不住
        // WindowServer 挪光标。HID 层的位置在光标更新路径上：把位移事件
        // 改写成"零位移、钉在钉扎点"后放行（v0.6.21），光标就走不动；
        // 吞掉（NULL）拦不住光标，但按键/点击/滚轮仍靠它拦 —— 本机 app
        // 不该收到它们。我们自己注入的事件（INJECT_TAG）在回调最顶上放行。
        let tap = CGEventTapCreate(
            CG_HID_EVENT_TAP,
            CG_HEAD_INSERT,
            CG_TAP_DEFAULT,
            mask,
            callback,
            boxed_addr as *mut c_void,
        );
        if tap.is_null() {
            let _ = ready_tx.send(Err(
                "CGEventTap 创建失败（通常是还没给辅助功能权限）".to_string()
            ));
            return;
        }
        tap_for_thread.store(tap as usize, Ordering::SeqCst);

        let source = CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0);
        if source.is_null() {
            let _ = ready_tx.send(Err("创建 runloop source 失败".to_string()));
            return;
        }
        let rl = CFRunLoopGetCurrent();
        CFRunLoopAddSource(rl, source, kCFRunLoopCommonModes);
        CGEventTapEnable(tap, true);
        if let Ok(mut slot) = runloop_for_thread.lock() {
            *slot = Some(rl as usize);
        }
        let _ = ready_tx.send(Ok(()));

        CFRunLoopRun();

        // 收工：一定把鼠标重新接回光标，否则用户会发现鼠标"失灵"
        CGAssociateMouseAndMouseCursorPosition(true);
        CFRelease(source as *const c_void);
        CFRelease(tap as *const c_void);
    });

    match ready_rx.recv_timeout(Duration::from_secs(3)) {
        Ok(Ok(())) => Ok(Capture {
            shared,
            grabbing,
            grab_lock,
            warp_source,
            corrections,
            width: width_atom,
            height: height_atom,
            tap: tap_slot,
            runloop,
            paste_ready,
        }),
        Ok(Err(err)) => Err(err),
        Err(_) => Err("抓取线程没有在 3 秒内就绪".to_string()),
    }
}

impl Capture {
    pub fn stop(&self) {
        self.set_grabbing(false);
        if let Ok(slot) = self.runloop.lock() {
            if let Some(rl) = *slot {
                unsafe { CFRunLoopStop(rl as CFRunLoopRef) };
            }
        }
        let _ = &self.tap;
    }

    #[allow(dead_code)]
    pub fn release(&self) {
        self.set_grabbing(false);
    }

    /// 告诉 tap："本机手上有本次复制的文件清单"。
    /// 只在转发期间、按下 Ctrl+V 时才拦 —— 光标还在本机时按键不经过这段逻辑。
    pub fn set_paste_ready(&self, on: bool) {
        self.paste_ready.store(on, Ordering::Relaxed);
    }

    #[allow(dead_code)]
    pub fn is_grabbing(&self) -> bool {
        self.grabbing.load(Ordering::Relaxed)
    }

    /// 守卫用：钩子被系统禁用时强制重开。钩子一死，物理事件全部漏到
    /// 本机（转发也没了）。返回 true = 发现被禁用并已重开（调用方落日志）。
    pub fn revive_tap(&self) -> bool {
        let tap = self
            .shared
            .lock()
            .map(|s| s.tap.load(Ordering::SeqCst))
            .unwrap_or(0);
        if tap == 0 {
            return false;
        }
        unsafe {
            if CGEventTapIsEnabled(tap as CFMachPortRef) {
                false
            } else {
                CGEventTapEnable(tap as CFMachPortRef, true);
                true
            }
        }
    }

    pub fn set_grabbing(&self, on: bool) {
        // 锁住整段：标志位 swap 与 CGAssociate 重申必须同进同出。
        // 并发交错的真实事故：让路线程 swap(false) 后还没来得及
        // associate(true)，Begin 线程 swap(true) 并 associate(false)，
        // 让路线程再 associate(true) —— 终态 = 在转发但光标还连着，
        // 两边光标同步移动（v0.6.15 时代的「两边同步」由此而来）。
        let _g = self.grab_lock.lock().unwrap();
        let was = self.grabbing.swap(on, Ordering::SeqCst);
        unsafe {
            // 无条件重申关联状态：幂等调用，顺带自愈历史遗留的错位
            CGAssociateMouseAndMouseCursorPosition(!on);
        }
        // 交还时的落点要沿用"出门时的高度"，所以先把钉扎点取下来再清空。
        // 钉扎点就是抓取那一刻光标的真实位置 —— 用户从哪个高度推出去的，
        // 回来就落回哪个高度。以前一律落在 h/2（屏幕正中），回来那一下
        // 光标会从半空跳一下，配合交还延迟就是"顿一下再蹦过来"的观感。
        let released_spot = self.shared.lock().ok().and_then(|s| s.pin);
        if on {
            // 开钉扎：把当前位置记为钉扎点。已经在钉扎中（心跳看守重申）
            // 就保持原钉扎点不动 —— 换点的话防线失效期间光标会
            // 一步步挪到屏幕边上。先读位置再进锁，锁内只做一次写入。
            let mut spot: Option<(f64, f64)> = None;
            unsafe {
                let ev = CGEventCreate(std::ptr::null_mut());
                if !ev.is_null() {
                    let p = CGEventGetLocation(ev);
                    CFRelease(ev as *const c_void);
                    spot = Some((p.x, p.y));
                }
            }
            if let Ok(mut s) = self.shared.lock() {
                if s.pin.is_none() {
                    s.pin = spot;
                }
            }
        } else {
            // 停钉扎：交还/被控时本机光标要能正常动，交还给 place_cursor 去摆位
            if let Ok(mut s) = self.shared.lock() {
                s.pin = None;
            }
        }
        if was == on {
            return;
        }
        unsafe {
            // 交还时把光标摆回边缘内侧 —— 只在多屏共享模式下有意义：
            // 那儿用户是从边缘越界过来的，回来也该落在边缘。
            // 离门 16 点：太贴门口的话手一抖就又越界了。
            // 跟随画面模式下屏幕刚切回来，光标原地不动才对，乱挪反而突兀。
            let edge_mode = self
                .shared
                .lock()
                .map(|s| s.edge_trigger)
                .unwrap_or(false);
            if !on && edge_mode {
                let w = self.width.load(Ordering::Relaxed) as f64 / 1000.0;
                let h = self.height.load(Ordering::Relaxed) as f64 / 1000.0;
                let side = self.shared.lock().map(|s| s.last_out).unwrap_or(Side::Right);
                // 沿门轴坐标沿用出门时的位置（released_spot），越界或缺失才
                // 退回屏幕中点。夹一下范围，别把光标甩到屏幕外。
                let along_y = released_spot
                    .map(|(_, y)| y.clamp(16.0, (h - 16.0).max(16.0)))
                    .unwrap_or(h / 2.0);
                let along_x = released_spot
                    .map(|(x, _)| x.clamp(16.0, (w - 16.0).max(16.0)))
                    .unwrap_or(w / 2.0);
                let spot = match side {
                    Side::Right => CGPoint {
                        x: w - 16.0,
                        y: along_y,
                    },
                    Side::Left => CGPoint {
                        x: 16.0,
                        y: along_y,
                    },
                    Side::Top => CGPoint {
                        x: along_x,
                        y: 16.0,
                    },
                    Side::Bottom => CGPoint {
                        x: along_x,
                        y: h - 16.0,
                    },
                };
                // 关键（v0.6.34）：warp 之后系统会在 **0.25 秒内忽略硬件
                // 鼠标事件** —— Apple 没写进文档，但 Ventura/Sonoma/
                // Sequoia 全都在，Hammerspoon 为此专门开过 issue。
                // 用户感知就是「从 Windows 推回 Mac，光标先冻一下才跟着手
                // 走」；而反方向（Mac→Windows）本机不做 warp，所以从不卡 ——
                // 「一边快一边慢」的根源就在这里。
                //
                // **只在这一瞬间放开，摆完位立刻复原 0.25**：那个抑制窗
                // 平时还在兼职压着另一件东西 —— 转发期间每笔位移都要靠
                // `CGDisplayMoveCursorToPoint` 把光标拽回钉扎点，拽回会
                // 产生一笔**反方向**的 mouse-moved；窗口一旦长期敞开，它
                // 就会被自己的 HID 钩子当成手移转发给对端（日志实锤：
                // v0.6.31 的位移流 `7,0 3,-1 12,-1 7,0…` 单向平滑，
                // 敞开之后变成 `3,0 -6,0 1,0 2,0 -1,0…` 正负交替）——
                // 对端光标被推回去再弹回来，就是「抖、不跟手」。
                let src = self.warp_source as CGEventSourceRef;
                if !src.is_null() {
                    CGEventSourceSetLocalEventsSuppressionInterval(src, 0.0);
                }
                // 顺序照 Deskflow/Barrier：先断开 → 摆位 → 再交还硬件。
                // 反过来的话，「已关联、光标还在门口 1 点内」的那几微秒里
                // 钩子会把撞边当成"又要出去"，立刻再抓一次 —— 门口乒乓。
                CGAssociateMouseAndMouseCursorPosition(false);
                CGWarpMouseCursorPosition(spot);
                CGAssociateMouseAndMouseCursorPosition(true);
                if !src.is_null() {
                    // 复原：下一次转发期间钉扎回中的反向量继续被压住
                    CGEventSourceSetLocalEventsSuppressionInterval(src, 0.25);
                }
            }
        }
    }

    /// 记下"这次是被对端从哪道门进来的"。
    ///
    /// 被控期间交还（`set_grabbing(false)`）要把光标摆到门内侧，摆哪条边不能
    /// 看配置 —— 主循环已经按对端出门侧把门口校正成对面了（拓扑定理），
    /// 平台层却还拿着配置里那个方向，两边口径不一致，落点就会跑到对侧去。
    /// 所以由主循环在进门时把校正后的门写进来。
    pub fn set_portal(&self, side: Side) {
        if let Ok(mut s) = self.shared.lock() {
            s.last_out = side;
            // 掩码也一并收成这一条：被控期间"把光标推到门口抢回"走的是
            // 同一个掩码，配置里那条边要是反的，交还落点对了、抢回方向
            // 仍然是反的（用户推门没反应，只有往反方向推才算数）。
            // P4 起链路表是唯一真源：这里收窄只是**临时**状态，会话结束
            // 后心跳会用 `set_mask` 把链路表那张铺回来，不会永久干掉其他边。
            s.sides = side.bit();
        }
    }

    /// 把掩码整张换掉（会话结束后恢复链路表那张）。
    ///
    /// 与 `set_portal` 分工：那边是"这一场该从哪条边抢回"，这里是"平时
    /// 哪几条边通向邻居"。两者共用同一个 `sides` 字段，顺序即语义 ——
    /// 所以只有心跳在**非 Receiving** 时调这里，被控期间不敢碰。
    pub fn set_mask(&self, mask: u8) {
        if let Ok(mut s) = self.shared.lock() {
            s.sides = mask;
        }
    }

    /// 抓取时记下的主屏尺寸（点）。
    ///
    /// 返回整数是为了和 Windows 那个 `Capture::screen()` 对齐 ——
    /// 两边签名不一致的话，`mod.rs` 里同一行代码在两边编不过，
    /// 而 Windows 那边只能在交叉编译时才发现。
    pub fn screen(&self) -> (u32, u32) {
        (
            (self.width.load(Ordering::Relaxed) / 1000) as u32,
            (self.height.load(Ordering::Relaxed) / 1000) as u32,
        )
    }
}

/// 按键 → 修饰位
fn modifier_flag(code: u16) -> u64 {
    match code {
        0x37 | 0x36 => FLAG_COMMAND,
        0x3B | 0x3E => FLAG_CONTROL,
        0x38 | 0x3C => FLAG_SHIFT,
        0x3A | 0x3D => FLAG_OPTION,
        0x39 => FLAG_CAPS,
        _ => 0,
    }
}

/// tap 回调。跑在抓取线程的 runloop 上，不能在这里做耗时操作。
extern "C" fn callback(
    _proxy: CGEventTapProxy,
    type_: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    unsafe {
        if user_info.is_null() {
            return event;
        }
        let shared_ref = &*(user_info as *const Arc<Mutex<Shared>>);

        // 自己注入的事件：放行，否则对端发来的鼠标会把自己推到边缘再切回去，死循环
        if CGEventGetIntegerValueField(event, FIELD_SOURCE_USER_DATA) == INJECT_TAG {
            return event;
        }

        // 系统把 tap 关了（超时或用户输入）：立刻重开，否则从此静默失效
        if type_ == EV_TAP_DISABLED_TIMEOUT || type_ == EV_TAP_DISABLED_USER {
            let why = if type_ == EV_TAP_DISABLED_TIMEOUT {
                "回调超时"
            } else {
                "用户输入保护"
            };
            if let Ok(shared) = shared_ref.lock() {
                let tap = shared.tap.load(Ordering::SeqCst);
                if tap != 0 {
                    CGEventTapEnable(tap as CFMachPortRef, true);
                }
                // 落日志：钩子被禁用期间事件全漏到本机（"两边一起动"），
                // 频率与时机是定位这类问题的关键证据
                let _ = shared.tx.send(Captured::Note(format!(
                    "⚠️ 事件钩子被系统禁用（{why}），已重新开启"
                )));
            }
            return event;
        }

        let Ok(mut shared) = shared_ref.lock() else {
            return event;
        };
        let grabbing = shared.grabbing.load(Ordering::Relaxed);

        if !grabbing {
            // 只有多屏共享模式才盯边缘。跟随画面模式下装了钩子但完全不动，
            // 因为那台机器的画面已经切走了：光标停在哪都不代表用户想过去。
            if shared.edge_trigger && type_ == EV_MOUSE_MOVED {
                let pos = CGEventGetLocation(event);
                let w = shared.width.load(Ordering::Relaxed) as f64 / 1000.0;
                let h = shared.height.load(Ordering::Relaxed) as f64 / 1000.0;
                if let Some(side) = hit_edge_in(shared.sides, pos.x, pos.y, w, h) {
                    // 记下"这一趟走的是哪道门"：交还时按它摆落点
                    shared.last_out = side;
                    // 沿门轴的位置比例：对端从对应高度进门
                    let along = side.along(pos.x, pos.y, w, h);
                    let _ = shared.tx.send(Captured::Begin { along, side });
                }
            }
            return event;
        }

        let flags = CGEventGetFlags(event);
        let mut emitted: Option<Event> = None;
        // 位移事件改写后放行（见文件头注释）：其余类型吞掉。
        // 放行的这笔事件 delta 已清零、位置钉在钉扎点，对本机等于"没动"。
        let mut pass_through = false;
        match type_ {
            EV_MOUSE_MOVED | EV_LEFT_DRAGGED | EV_RIGHT_DRAGGED | EV_OTHER_DRAGGED => {
                let dx = CGEventGetIntegerValueField(event, FIELD_MOUSE_DELTA_X) as i32;
                let dy = CGEventGetIntegerValueField(event, FIELD_MOUSE_DELTA_Y) as i32;
                if dx != 0 || dy != 0 {
                    emitted = Some(Event::Motion { dx, dy });
                }
                pass_through = true;
            }
            EV_LEFT_DOWN | EV_LEFT_UP | EV_RIGHT_DOWN | EV_RIGHT_UP | EV_OTHER_DOWN
            | EV_OTHER_UP => {
                let button_no = CGEventGetIntegerValueField(event, FIELD_MOUSE_BUTTON);
                let (button, down) = match type_ {
                    EV_LEFT_DOWN => (1u8, true),
                    EV_LEFT_UP => (1, false),
                    EV_RIGHT_DOWN => (2, true),
                    EV_RIGHT_UP => (2, false),
                    EV_OTHER_DOWN => (if button_no == 2 { 3 } else { 4 }, true),
                    _ => (if button_no == 2 { 3 } else { 4 }, false),
                };
                emitted = Some(Event::Button { button, down });
            }
            EV_SCROLL => {
                let dy = CGEventGetIntegerValueField(event, FIELD_SCROLL_AXIS1) as i32;
                let dx = CGEventGetIntegerValueField(event, FIELD_SCROLL_AXIS2) as i32;
                emitted = Some(Event::Wheel {
                    dx: dx * 120,
                    dy: dy * 120,
                });
            }
            EV_KEY_DOWN | EV_KEY_UP => {
                let code = CGEventGetIntegerValueField(event, FIELD_KEYBOARD_KEYCODE) as u16;
                let down = type_ == EV_KEY_DOWN;
                // Ctrl+Alt+Esc：交还控制权。这个组合很少和别的快捷键冲突。
                if down
                    && code == KEY_ESCAPE
                    && (flags & (FLAG_CONTROL | FLAG_OPTION)) == (FLAG_CONTROL | FLAG_OPTION)
                {
                    let _ = shared.tx.send(Captured::End);
                    return std::ptr::null_mut();
                }
                // v0.6.39：剪贴板文件按需传输。
                // 转发期间（grabbing，上面已经判过）按下 **Ctrl+V**，且本机手上有
                // "本次复制的文件清单" —— 把这个键吞掉，交给主线程去传文件；
                // 传完由主线程补发一次 Ctrl+V。用户视角只有一个动作：按了一下粘贴。
                //
                // 为什么是 Control 不是 Command：Command(0x37) 在 win.rs 的映射表里
                // 是 Win 键，Win+V 是剪贴板历史，不是粘贴。用户在 Windows 屏上
                // 粘贴时按的就是 Mac 键盘的 Control 键（0x3B → VK_CONTROL）。
                if down && code == KEY_V && (flags & FLAG_CONTROL) != 0
                    && shared.paste_ready.load(Ordering::Relaxed)
                {
                    shared.swallowed_key = Some(code);
                    let _ = shared.tx.send(Captured::PasteIntent);
                    return std::ptr::null_mut();
                }
                // 被吞掉那个键的抬起也一起吞，别给对端留一个孤立的 key-up
                if !down && shared.swallowed_key == Some(code) {
                    shared.swallowed_key = None;
                    return std::ptr::null_mut();
                }
                emitted = Some(Event::Key { code, down });
            }
            EV_FLAGS_CHANGED => {
                let code = CGEventGetIntegerValueField(event, FIELD_KEYBOARD_KEYCODE) as u16;
                let bit = modifier_flag(code);
                if bit != 0 {
                    emitted = Some(Event::Key {
                        code,
                        down: (flags & bit) != 0,
                    });
                }
            }
            _ => {}
        }

        if let Some(ev) = emitted {
            let _ = shared.tx.send(Captured::Input(ev));
        }
        if pass_through {
            // 原始 delta 已经转发走了，这里把事件本身改成"原地不动"再放行
            //（对"系统按事件摆光标"的实现仍是防线；实测 Darwin 27 不吃这套，
            // 光标走的是 WindowServer 的原始指针通路）。
            CGEventSetIntegerValueField(event, FIELD_MOUSE_DELTA_X, 0);
            CGEventSetIntegerValueField(event, FIELD_MOUSE_DELTA_Y, 0);
            if let Some((px, py)) = shared.pin {
                CGEventSetLocation(event, CGPoint { x: px, y: py });
                // v0.6.22 无声回中（真正起作用的防线）：每笔位移事件转发完，
                // 立刻把光标无声拉回钉扎点。CGDisplayMoveCursorToPoint 不产生
                // 事件，不会回流自己的钩子、不会被转发；1000Hz 的回中让光标
                // 物理上离不开钉扎点几像素 —— associate 失不失效都拦得住。
                CGDisplayMoveCursorToPoint(CGMainDisplayID(), CGPoint { x: px, y: py });
            }
            return event;
        }
    }
    // 其余抓住的事件（按键/点击/滚轮）一律吞掉，别让它们落到本机 app
    std::ptr::null_mut()
}

/// 有没有辅助功能权限。没有的话 tap 会被静默禁用，必须提前拦。
pub fn trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// 能不能往系统里注入事件（10.15 起是单独的权限项）
pub fn can_post() -> bool {
    unsafe { CGPreflightPostEventAccess() }
}

/// 直接把用户送到辅助功能设置页。没权限时点菜单会走这里。
pub fn open_accessibility_settings() {
    let _ = std::process::Command::new("/usr/bin/open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .spawn();
}

/// 主屏的逻辑尺寸（点）。CGDisplayBounds 给的就是点，和事件坐标同一套。
pub fn screen_size() -> (f64, f64) {
    unsafe {
        let b = CGDisplayBounds(CGMainDisplayID());
        if b.size.width <= 0.0 {
            (1440.0, 900.0)
        } else {
            (b.size.width, b.size.height)
        }
    }
}

/// 接口对齐：Mac 只有一块主屏，布局就它自己（点）。
pub fn monitor_layout() -> Vec<(i32, i32, i32, i32)> {
    let (w, h) = screen_size();
    vec![(0, 0, w as i32, h as i32)]
}

/// 接口对齐：Mac 没有多屏工作屏判定，恒返回 1（物理活动屏=主屏）。
pub fn work_screen_source() -> u8 {
    1
}

/// 把事件注入回本机。对端发来的东西全走这里。
pub struct Inject {
    source: CGEventSourceRef,
    /// 本地跟踪的光标位置（点）。SendInput/CGEventPost 都是异步的：
    /// 每笔位移都去 CGEventGetLocation 读回的话，高频下会读到
    /// WindowServer 还没更新的旧位置，位移就被吞了（Windows 端
    /// v0.6.5b 踩过的同一个坑）。所以自己记账，place_cursor 时对齐，
    /// 与真实光标偏离过大时（本机物理鼠标动过）再重新对齐。
    pos: Mutex<(f64, f64)>,
    /// 账本累计位移（点，曼哈顿距离）。注入失效判定用：真实光标没动
    /// + 账本走了 = 注入真的死了；真实光标没动 + 账本也没走 = 光标
    /// 被钳在屏幕边上（物理事实，不该判失效）。
    walked: Mutex<f64>,
}

impl Inject {
    pub fn new() -> Self {
        unsafe {
            let source = CGEventSourceCreate(CG_SOURCE_HID);
            if !source.is_null() {
                // 默认会压掉本机 0.25 秒内的鼠标事件，注入不需要这个抑制
                CGEventSourceSetLocalEventsSuppressionInterval(source, 0.0);
            }
            let start = CGEventCreate(std::ptr::null_mut());
            let at = if start.is_null() {
                CGPoint::default()
            } else {
                let p = CGEventGetLocation(start);
                CFRelease(start as *const c_void);
                p
            };
            Self {
                source,
                pos: Mutex::new((at.x, at.y)),
                walked: Mutex::new(0.0),
            }
        }
    }

    /// 账本累计位移（点）。见 walked 字段注释。
    pub fn walked(&self) -> f64 {
        self.walked.lock().map(|w| *w).unwrap_or(0.0)
    }

    pub fn apply(&self, event: &Event) {
        unsafe {
            match *event {
                Event::Motion { dx, dy } => self.move_by(dx, dy),
                Event::Button { button, down } => self.button(button, down),
                Event::Wheel { dx, dy } => self.scroll(dx, dy),
                Event::Key { code, down } => self.key(code, down),
                // 心跳、进出控制都不产生输入，不该被注入
                Event::Enter { .. } | Event::Leave | Event::Ping | Event::Pong { .. }
                | Event::Takeback => {}
            }
        }
    }

    /// 当前光标位置（点）。用来判断对端的光标是不是撞回了本机这一侧。
    pub fn cursor(&self) -> Option<(f64, f64)> {
        unsafe {
            let ev = CGEventCreate(std::ptr::null_mut());
            if ev.is_null() {
                return None;
            }
            let p = CGEventGetLocation(ev);
            CFRelease(ev as *const c_void);
            Some((p.x, p.y))
        }
    }

    /// 本地账本位置。与 cursor()（真实光标）对比：两者差得远 =
    /// 有外力绕过注入直接挪了真实光标。
    pub fn tracked(&self) -> Option<(f64, f64)> {
        self.pos.lock().ok().map(|p| *p)
    }

    pub fn screen(&self) -> (f64, f64) {
        screen_size()
    }

    /// 多屏共享时 Windows 端会把注入限制在"本机工作屏"内；Mac 只有一块
    /// 主屏，无需处理（接口对齐用）。返回 None 表示没有多屏锚定。
    pub fn enable_own_screen(&self, _side: Side) -> Option<(i32, i32, i32, i32)> {
        None
    }

    /// 把光标摆到屏幕正中。
    /// 对端刚接管时调用：否则光标还停在上次离开的角落，看着像"鼠标没接上"。
    pub fn center(&self) {
        let (w, h) = screen_size();
        self.place_cursor(w / 2.0, h / 2.0);
    }

    /// 把光标摆到指定位置（进门落点：出门高度的对应点）。
    pub fn place_cursor(&self, x: f64, y: f64) {
        unsafe {
            CGWarpMouseCursorPosition(CGPoint { x, y });
        }
        // 本地账本跟着对齐，后续位移从这儿起步
        if let Ok(mut pos) = self.pos.lock() {
            *pos = (x, y);
        }
    }

    unsafe fn cursor_at(&self) -> CGPoint {
        let ev = CGEventCreate(std::ptr::null_mut());
        if ev.is_null() {
            return CGPoint::default();
        }
        let p = CGEventGetLocation(ev);
        CFRelease(ev as *const c_void);
        p
    }

    unsafe fn move_by(&self, dx: i32, dy: i32) {
        // 用本地账本算目标位置，不读回。真实光标若偏离账本超过阈值
        //（说明有本机物理鼠标之类的旁路输入），先对齐再走，免得跳。
        let (w, h) = screen_size();
        let cur = {
            let mut pos = match self.pos.lock() {
                Ok(p) => p,
                Err(_) => return,
            };
            let real = self.cursor_at();
            if (real.x - pos.0).abs() > 48.0 || (real.y - pos.1).abs() > 48.0 {
                *pos = (real.x, real.y);
            }
            *pos
        };
        let target = CGPoint {
            x: (cur.0 + dx as f64).clamp(0.0, w - 1.0),
            y: (cur.1 + dy as f64).clamp(0.0, h - 1.0),
        };
        if let Ok(mut pos) = self.pos.lock() {
            *pos = (target.x, target.y);
        }
        // 账本实际走了多少（钳位后的净位移）：光标顶死在屏幕边上时
        // 这里几乎不涨，是"没动是物理事实"的判据
        if let Ok(mut walked) = self.walked.lock() {
            *walked += (target.x - cur.0).abs() + (target.y - cur.1).abs();
        }
        let ev = CGEventCreateMouseEvent(self.source, EV_MOUSE_MOVED, target, BUTTON_LEFT);
        if ev.is_null() {
            return;
        }
        // 显式带上 delta：有些 app 只认 delta 不认绝对位置
        CGEventSetIntegerValueField(ev, FIELD_MOUSE_DELTA_X, dx as c_long);
        CGEventSetIntegerValueField(ev, FIELD_MOUSE_DELTA_Y, dy as c_long);
        CGEventSetIntegerValueField(ev, FIELD_SOURCE_USER_DATA, INJECT_TAG);
        // 从会话层入流（v0.6.20）：不走 HID 层的话，注入事件要经过我们
        // 自己的 HID 钩子，靠 INJECT_TAG 放行——实测标签会失效（输入法/
        // 其他装钩子的工具介入时事件被转手），钩子把自己的注入吞掉，
        // 表现就是"被控几十秒后光标突然死了"。会话层在我们钩子的下游，
        // 注入事件根本不经过它，物理上不可能被自己吞。
        CGEventPost(CG_SESSION_EVENT_TAP, ev);
        CFRelease(ev as *const c_void);
    }

    unsafe fn button(&self, button: u8, down: bool) {
        let cur = self.cursor_at();
        let (ev_type, cg_button) = match (button, down) {
            (1, true) => (EV_LEFT_DOWN, BUTTON_LEFT),
            (1, false) => (EV_LEFT_UP, BUTTON_LEFT),
            (2, true) => (EV_RIGHT_DOWN, BUTTON_RIGHT),
            (2, false) => (EV_RIGHT_UP, BUTTON_RIGHT),
            (_, true) => (EV_OTHER_DOWN, BUTTON_CENTER),
            (_, false) => (EV_OTHER_UP, BUTTON_CENTER),
        };
        let ev = CGEventCreateMouseEvent(self.source, ev_type, cur, cg_button);
        if ev.is_null() {
            return;
        }
        if button >= 3 {
            CGEventSetIntegerValueField(ev, FIELD_MOUSE_BUTTON, 2);
        }
        CGEventSetIntegerValueField(ev, FIELD_SOURCE_USER_DATA, INJECT_TAG);
        // 从会话层入流（v0.6.20）：不走 HID 层的话，注入事件要经过我们
        // 自己的 HID 钩子，靠 INJECT_TAG 放行——实测标签会失效（输入法/
        // 其他装钩子的工具介入时事件被转手），钩子把自己的注入吞掉，
        // 表现就是"被控几十秒后光标突然死了"。会话层在我们钩子的下游，
        // 注入事件根本不经过它，物理上不可能被自己吞。
        CGEventPost(CG_SESSION_EVENT_TAP, ev);
        CFRelease(ev as *const c_void);
    }

    unsafe fn scroll(&self, dx: i32, dy: i32) {
        // 用 CGEventCreate + SetType，绕开 CGEventCreateScrollWheelEvent 的变参签名
        let ev = CGEventCreate(self.source);
        if ev.is_null() {
            return;
        }
        CGEventSetType(ev, EV_SCROLL);
        CGEventSetIntegerValueField(ev, FIELD_SCROLL_AXIS1, (dy / 120) as c_long);
        CGEventSetIntegerValueField(ev, FIELD_SCROLL_AXIS2, (dx / 120) as c_long);
        CGEventSetIntegerValueField(ev, FIELD_SOURCE_USER_DATA, INJECT_TAG);
        // 从会话层入流（v0.6.20）：不走 HID 层的话，注入事件要经过我们
        // 自己的 HID 钩子，靠 INJECT_TAG 放行——实测标签会失效（输入法/
        // 其他装钩子的工具介入时事件被转手），钩子把自己的注入吞掉，
        // 表现就是"被控几十秒后光标突然死了"。会话层在我们钩子的下游，
        // 注入事件根本不经过它，物理上不可能被自己吞。
        CGEventPost(CG_SESSION_EVENT_TAP, ev);
        CFRelease(ev as *const c_void);
    }

    unsafe fn key(&self, code: u16, down: bool) {
        let ev = CGEventCreateKeyboardEvent(self.source, code, down);
        if ev.is_null() {
            return;
        }
        CGEventSetIntegerValueField(ev, FIELD_SOURCE_USER_DATA, INJECT_TAG);
        // 从会话层入流（v0.6.20）：不走 HID 层的话，注入事件要经过我们
        // 自己的 HID 钩子，靠 INJECT_TAG 放行——实测标签会失效（输入法/
        // 其他装钩子的工具介入时事件被转手），钩子把自己的注入吞掉，
        // 表现就是"被控几十秒后光标突然死了"。会话层在我们钩子的下游，
        // 注入事件根本不经过它，物理上不可能被自己吞。
        CGEventPost(CG_SESSION_EVENT_TAP, ev);
        CFRelease(ev as *const c_void);
    }
}

impl Default for Inject {
    fn default() -> Self {
        Self::new()
    }
}

/// 收工时要补发 key-up 的修饰键。
/// 不补的话对端会一直以为 Ctrl 还按着，之后所有点击都变成 Ctrl+点击。
pub const MODIFIER_KEYS: [u16; 8] = [0x37, 0x36, 0x3B, 0x3E, 0x38, 0x3C, 0x3A, 0x3D];

