//! 共用键鼠：一套键鼠控制两台机器。**两种模式，覆盖两种真实场景。**
//!
//! ```text
//! 模式 A「跟随画面」= 软件 KVM
//!   显示器 ──┬── 主机 A（键鼠插在这台）   ← Console
//!            └── 主机 B                  ← Remote
//!   画面在 A：A 正常用，程序什么都不做
//!   画面切到 B：A 把自己收到的键鼠事件全部转发给 B，B 注入回本机
//!   画面切回 A：A 停止转发，键鼠立刻回到本机
//!
//! 模式 B「边缘跨界」= 多屏共享
//!   [ 屏幕 A ][ 屏幕 B ]   两台各有屏幕、各有键鼠
//!   光标推到 A 的右边缘 → 越界到 B；在 B 推回左边缘 → 回到 A
//! ```
//!
//! **两者不能混用，因为物理前提不同：**
//!
//! - 模式 A 只有**一块屏、一套键鼠**，另一台机器根本没显示器（画面切走时系统
//!   会认为显示器被拔掉了）。这时**没有"边缘"可撞** —— 控制端的画面已经切走，
//!   用户看不见它也不知道光标在哪，靠撞边缘触发等于让人盲操作。所以触发点
//!   只能是「画面切换」本身，而且角色是固定的：键鼠插在谁身上谁是 Console。
//! - 模式 B 两台都有屏幕、都有键鼠，谁也不欠谁，角色是对等的，靠边缘判定越界。
//!
//! 早期版本只实现了模式 B，却当成通用方案——在那台没显示器的机器上
//! 永远等不到"鼠标撞边缘"，于是看起来就像完全没工作。
//! 架构参照两个项目的公开设计，代码都是自己写的（两者都是 GPL，只能借鉴）：
//!
//! - **lan-mouse**（Rust，GPL-3.0）：事件流形态、注入打标记防回环、交还补 key-up。
//! - **Deskflow**（C++，GPL-2.0，Synergy 血统）：心跳 + 空闲超时、事件流加密、
//!   断线自动交还。这三条是只看 lan-mouse 时漏掉的。
//!
//! 四条关键规则：
//! 1. **注入要打标记**：注入的事件仍会经过自己的抓取钩子，必须能认出来并放行，
//!    否则 B 注入的鼠标会被 A 当成真实输入再发回去，来回放大。
//! 2. **松手要补 key-up**：转发期间按下的键，对端只收到 down 没收到 up，
//!    交还时如果不补，对面会一直以为 Ctrl 还按着，之后所有点击都变组合键。
//! 3. **必须有心跳**：UDP 无连接，对端崩了本机不知道。没有心跳的话本机会
//!    永远停在"转发中"，把自己的键鼠全吞掉 —— 等于失联。Deskflow 用 2 秒
//!    空闲超时判定断线，这里给 2.5 秒。
//! 4. **开转之前要先确认对端活着**：否则画面切过去、键鼠也交出去，结果对端
//!    没开程序 —— 用户面前是一块显示着 B 的屏，键鼠却谁也控制不了。
//!
//! 输入事件走独立 UDP 端口（`network.port + 3`），不进发现层的 JSON 通道：
//! 鼠标 1000Hz，JSON 序列化扛不住。配对后用配对码派生的密钥做
//! ChaCha20-Poly1305 加密（Deskflow 是 TLS，效果等价：都保证只有配对过的
//! 那两台读得懂）。

pub mod event;

#[cfg(target_os = "macos")]
mod mac;
#[cfg(windows)]
mod win;

use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use event::{Event, Opener, Sealer, Side};

/// 多久发一次心跳。缩短它能更快发现断线，代价是多几个包。
const PING_INTERVAL: Duration = Duration::from_millis(800);
/// 多久收不到对端的任何包就判定断线。
/// Deskflow 的 `USYNERGY_IDLE_TIMEOUT` 是 2000ms，这里给 2500ms：
/// 机器从睡眠唤醒时偶尔会抖一下，太敏感会无谓地打断用户。
const IDLE_TIMEOUT_MS: u64 = 2500;
/// 开转之前，对端的包必须在这么久以内到达过。
/// 比断线超时宽一点，容忍一次丢包。
const PEER_ALIVE_MS: u64 = 6000;

/// 跑哪种模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// 跟随画面（软件 KVM）：一套键鼠，画面切到哪台键鼠跟到哪台。
    /// 需要 `role` 指明键鼠插在谁身上。
    Follow,
    /// 边缘跨界（多屏共享）：两台各有屏幕各有键鼠，光标推到 `side` 那侧就越界。
    /// 两边角色对等，`role` 被忽略。
    Edge,
}

impl Mode {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "follow" | "kvm" | "跟随" => Some(Mode::Follow),
            "edge" | "share" | "边缘" | "共享" => Some(Mode::Edge),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Follow => "follow",
            Mode::Edge => "edge",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Mode::Follow => "跟随画面（软件 KVM）",
            Mode::Edge => "边缘跨界（多屏共享）",
        }
    }
}

/// 本机在这套键鼠里的角色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// 物理键鼠接在本机。画面切走时抓取并转发。
    Console,
    /// 物理键鼠在对面。只接收事件并注入，**不抓取** ——
    /// 抓了也没用（本来就没有键鼠插在它身上），还会跟真控制端抢。
    Remote,
}

impl Role {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "console" | "host" | "本机" | "主" => Some(Role::Console),
            "remote" | "client" | "对端" | "从" => Some(Role::Remote),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Console => "console",
            Role::Remote => "remote",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Role::Console => "键鼠在本机",
            Role::Remote => "键鼠在对面",
        }
    }
}

/// 进门落点离门边的距离（点）。太近会压到交还触发线（1.5 点）里，
/// 手一抖刚进门就被弹回去；16 点看得见"光标过来了"又足够安全。
const ENTRY_MARGIN: f64 = 16.0;
/// 进门后要先往里走这么远，"推门交还"判定才武装。
/// 否则进门落点本身就贴着门边，稍微往外推就交还，刚进来就被弹回去
/// —— Deskflow 的 avoidJumpZone 处理的就是这个问题。
const EDGE_REARM_DIST: f64 = 32.0;
/// 推门交还要累计朝门外推这么远（点，按屏宽缩放）才认账。
///
/// 旧实现要求"顶住门边 300ms"，纯时间判据 —— 手都顶到墙上了程序还要
/// 再等满 300 毫秒，用户感知就是"Windows 回 Mac 要顿一下"，而
/// Mac→Windows 是撞边即走，两边手感不对称。
///
/// 改成累计位移后：一次果断的推门三五十毫秒就够（60~125Hz 的位移流里
/// 几笔就到阈值），而扫过门口那一两笔杂音攒不满 —— 既快又不会误切。
const RETURN_PUSH: f64 = 26.0;

/// 抓取侧发来的消息
#[derive(Clone, Debug)]
pub enum Captured {
    /// 鼠标撞到边缘，请求越界到对端（只在 Edge 模式发生）。
    /// `along` = 出门瞬间光标沿门轴的位置比例（0..65535）：
    /// 左右门是纵坐标、上下门是横坐标，让对端从对应高度进门。
    Begin { along: u16 },
    /// 要转发给对端的事件
    Input(Event),
    /// 钩子线程发现异常，直接落日志（比如被系统超时禁用）
    Note(String),
    /// 按到了紧急收回（Ctrl+Alt+Esc）
    End,
    /// 转发期间按下了 Ctrl+V，而本机手上有"本次复制的文件清单"。
    /// 这个键已经被 tap 吞掉，交给上层去传文件、传完再补按。
    PasteIntent,
}

#[cfg(target_os = "macos")]
use mac as plat;
#[cfg(windows)]
use win as plat;

pub use plat::Inject;

/// 本机有没有抓取键鼠的权限
pub fn trusted() -> bool {
    plat::trusted()
}

/// 能不能往系统里注入事件
pub fn can_post() -> bool {
    plat::can_post()
}

/// 打开系统的授权设置页（只有 macOS 需要）
pub fn open_settings() {
    plat::open_accessibility_settings();
}

/// 主屏尺寸（点）
pub fn screen_size() -> (u32, u32) {
    let (w, h) = plat::screen_size();
    (w as u32, h as u32)
}

/// 当前处于什么状态
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// 空闲：键鼠在本机，画面也在本机
    Idle = 0,
    /// 正在把键鼠转发给对端（画面在对端那边）
    Sending = 1,
    /// 正被对端控制（画面在本机，键鼠在对面）
    Receiving = 2,
}

impl State {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => State::Sending,
            2 => State::Receiving,
            _ => State::Idle,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            State::Idle => "空闲",
            State::Sending => "键鼠已交给对端",
            State::Receiving => "键鼠在对面",
        }
    }
}

pub struct Options {
    /// 对端 IP。KVM 端口固定是对端 `network.port + 3`。
    pub peer: IpAddr,
    /// 跑哪种模式
    pub mode: Mode,
    /// 本机角色。`Mode::Follow` 时才起作用。
    pub role: Role,
    /// 鼠标撞哪一侧越界。`Mode::Edge` 时才起作用。
    pub side: Side,
    /// 发现层用的端口，KVM 用 port + 3
    pub base_port: u16,
    /// 配对密钥。有就加密事件流，没有就裸奔（并在日志里警告）。
    pub key: Option<[u8; 32]>,
    /// 紧急收回时执行的额外动作（把画面切回本机）。
    ///
    /// 没了它，用户按 Ctrl+Alt+Esc 只会收回键鼠，屏幕还停在对端 ——
    /// 于是他"能操作本机了，但看不见本机"。
    pub on_panic: Option<Arc<dyn Fn() + Send + Sync>>,
    /// 日志回调（从后台线程调，实现方自己要保证线程安全）
    pub log: Arc<dyn Fn(String) + Send + Sync>,
    /// 转发期间用户按下 Ctrl+V 且手上有待传清单时的通知。
    /// 从抓取线程的接收循环里调，实现方自己做线程安全（通常只是往事件队列塞一条）。
    pub on_paste: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// 发送侧的共享状态。几个线程都要往里写，所以里面全是 Mutex。
struct Out {
    socket: UdpSocket,
    peer: SocketAddr,
    /// None = 没配对，走明文
    sealer: Mutex<Option<Sealer>>,
    seq: Mutex<u16>,
}

impl Out {
    /// 发一个事件给**当前会话目标**（`self.peer`）。会话流（位移/按键/
    /// Enter/Leave）走这条。
    fn send(&self, ev: &Event) {
        self.to(self.peer, ev);
    }

    /// 显式寻址发送。**回包与救援必须用它**：多对端下 `self.peer` 只是
    /// "当前会话那台"，把给 A 的 Pong 发给它，实际收件人可能是 B。
    fn to(&self, addr: SocketAddr, ev: &Event) {
        let mut seq = self.seq.lock().unwrap();
        *seq = seq.wrapping_add(1);
        let n = *seq;
        let bytes = match self.sealer.lock().unwrap().as_mut() {
            Some(sealer) => sealer.seal(n, ev),
            None => ev.encode(n),
        };
        // 加密失败会返回空，这种情况下丢掉这个包 —— 绝不退回明文，那等于静默降级
        if !bytes.is_empty() {
            let _ = self.socket.send_to(&bytes, addr);
        }
    }
}

/// 本机状态在**某个具体对端**眼里的样子。
///
/// 我在跟 B 开会时，对 A 而言就该是 Idle。若照实把 `Sending` 报给 A，
/// A 那边期待的是 `Receiving`，2.5 秒后判"状态对不上（交还消息丢了）"
/// 并自愈 —— 它会把正在跟我开会的那条会话一起踢掉。多对端下必现，
/// 单对端下这个函数恒等于原值，行为不变。
fn state_for(state: &AtomicU8, active: IpAddr, from: IpAddr) -> u8 {
    if from == active {
        state.load(Ordering::Relaxed)
    } else {
        State::Idle as u8
    }
}

pub struct Kvm {
    state: Arc<AtomicU8>,
    stop: Arc<AtomicBool>,
    mode: Mode,
    role: Role,
    /// Remote 角色下是 None：不抓取，只等对端发事件来
    capture: Option<Arc<plat::Capture>>,
    out: Arc<Out>,
    /// 最后一次收到对端合法包的时刻（毫秒时间戳）
    last_rx: Arc<AtomicU64>,
    /// 有没有真的收到过对端的包。启动时 last_rx 就是当前时间，
    /// 单看它会把"从没连上"误判成"刚刚连上"。
    peer_seen: Arc<AtomicBool>,
    log: Arc<dyn Fn(String) + Send + Sync>,
}

impl Kvm {
    pub fn state(&self) -> State {
        State::from_u8(self.state.load(Ordering::Relaxed))
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// 距离上次收到对端消息多久了（毫秒）
    pub fn idle_ms(&self) -> u64 {
        now_ms().saturating_sub(self.last_rx.load(Ordering::Relaxed))
    }

    /// 对端是不是活着（最近收到过它的包）
    pub fn peer_alive(&self) -> bool {
        self.peer_seen.load(Ordering::Relaxed) && self.idle_ms() <= PEER_ALIVE_MS
    }

    /// 本机手上有"本次复制的文件清单" → 转发期间按 Ctrl+V 会被拦下来。
    /// 只在托盘侧真的读到文件剪贴板时置 true，其余时间必须是 false，
    /// 否则会误吞用户正常的 Ctrl+V。
    pub fn set_paste_ready(&self, on: bool) {
        if let Some(c) = self.capture.as_ref() {
            c.set_paste_ready(on);
        }
    }

    /// 补发一次 Ctrl+V 给对端：用户按的那一次已经被我们吞掉了，
    /// 文件落盘之后由这里替他把粘贴动作做完。
    ///
    /// 固定补 Ctrl down → V down → V up → Ctrl up：用户按完 28 秒的传输，
    /// 手早就松开了；重复发一次 Ctrl down 在 Windows 上只是重复按键，无害。
    pub fn replay_paste_to_peer(&self) {
        self.out.send(&Event::Key {
            code: 0x3B,
            down: true,
        });
        self.out.send(&Event::Key {
            code: 0x09,
            down: true,
        });
        self.out.send(&Event::Key {
            code: 0x09,
            down: false,
        });
        self.out.send(&Event::Key {
            code: 0x3B,
            down: false,
        });
    }

    /// 把一次按键转发给对端。传输失败/取消时用它把用户原本那一下补回去，
    /// 不让人感觉"按了没反应"。
    pub fn send_key_to_peer(&self, code: u16) {
        self.out.send(&Event::Key { code, down: true });
        self.out.send(&Event::Key {
            code,
            down: false,
        });
    }

    /// **画面切到对端了 → 把键鼠交过去。**
    ///
    /// 这是唯一进入转发状态的入口。切屏动作成功之后调用它。
    ///
    /// 开转之前先确认对端活着：否则画面已经切过去、键鼠也被吞掉，
    /// 用户面前是一块亮着 B 的屏，却谁也控制不了 —— 比不做还糟。
    pub fn follow(&self) -> Result<(), String> {
        // Edge 模式下两边对等，谁都可以越界；Follow 模式只有键鼠端能往外发
        if self.mode == Mode::Follow && self.role != Role::Console {
            return Err("这台机器的键鼠不在本机，不能往外转发".to_string());
        }
        if self.state() != State::Idle {
            return Ok(());
        }
        if !self.peer_alive() {
            return Err(
                "对端没有回应（对面没开程序、不在同一网段，或防火墙拦了）。\
                 键鼠没有交出去，你还能操作本机"
                    .to_string(),
            );
        }
        let Some(capture) = self.capture.as_ref() else {
            return Err("抓取模块没有启动".to_string());
        };
        capture.set_grabbing(true);
        self.state.store(State::Sending as u8, Ordering::SeqCst);
        // 跟随画面模式下没有"门"的概念，出门位置未知（0xFFFF），
        // 对端居中落点即可；屏幕尺寸照带，位移缩放照样生效
        let (sw, sh) = capture.screen();
        self.out.send(&Event::Enter {
            width: sw.min(65535) as u16,
            height: sh.min(65535) as u16,
            along: 0xFFFF,
            flags: 0,
            // 跟随画面模式没有门，门口方向留给对端自己决定
            side: Side::CODE_UNKNOWN,
        });
        (self.log)("键鼠已交给对端（画面在那边）".to_string());
        Ok(())
    }

    /// **画面切回本机了 / 要拿回键鼠 → 停止转发。**
    ///
    /// 幂等：本来就在本机时什么都不做。
    pub fn give_back(&self) {
        self.release(None);
    }

    fn release(&self, reason: Option<&str>) {
        let now = self.state.load(Ordering::Relaxed);
        if now != State::Sending as u8 {
            return;
        }
        // CAS 抢一下，避免两个线程同时交还、重复补 key-up
        if self
            .state
            .compare_exchange(now, State::Idle as u8, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        if let Some(capture) = self.capture.as_ref() {
            capture.set_grabbing(false);
        }
        // 先把可能还按着的修饰键和按钮补 up，再发 Leave。
        // 不补的话对面会一直以为 Cmd/Ctrl 还压着，之后所有点击都变成组合键。
        for code in plat::MODIFIER_KEYS {
            self.out.send(&Event::Key {
                code,
                down: false,
            });
        }
        for button in 1u8..=3 {
            self.out.send(&Event::Button {
                button,
                down: false,
            });
        }
        self.out.send(&Event::Leave);

        match reason {
            Some(why) => (self.log)(format!("键鼠已收回：{why}")),
            None => (self.log)("键鼠已收回本机".to_string()),
        }
    }

    pub fn stop(&self) {
        self.release(None);
        self.stop.store(true, Ordering::SeqCst);
        if let Some(capture) = self.capture.as_ref() {
            capture.stop();
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 启动。Console 会装上抓取钩子但**不吞事件**；只有 `follow()` 之后才开始抓。
pub fn start(opts: Options) -> Result<Kvm, String> {
    let port = opts.base_port.saturating_add(3);
    let peer_addr = SocketAddr::new(opts.peer, port);

    // 收事件要在固定端口上等着；发送用同一个 socket 发回去即可
    let socket = UdpSocket::bind(("0.0.0.0", port))
        .map_err(|e| format!("KVM 端口 {port} 绑定失败：{e}"))?;
    socket
        .set_read_timeout(Some(Duration::from_millis(200)))
        .map_err(|e| format!("设置超时失败：{e}"))?;

    // 两个角色要的权限不一样，别把 Console 的检查套到 Remote 头上：
    // Remote 只需要"能注入"，它根本没有键鼠可抓。
    // Edge 模式下两边都是对等的，两边都要能抓能注入。
    let needs_capture = opts.mode == Mode::Edge || opts.role == Role::Console;
    match opts.role {
        Role::Console => {
            if !plat::trusted() {
                return Err(
                    "缺少权限，抓不到键鼠（macOS：系统设置 → 隐私与安全性 → 辅助功能）".into(),
                );
            }
        }
        Role::Remote if needs_capture => {
            if !plat::trusted() {
                return Err(
                    "缺少权限，抓不到键鼠（macOS：系统设置 → 隐私与安全性 → 辅助功能）".into(),
                );
            }
        }
        Role::Remote => {
            if !plat::can_post() {
                return Err(
                    "缺少权限，没法把对端的键鼠注入本机（macOS：系统设置 → 隐私与安全性 → 辅助功能）"
                        .into(),
                );
            }
        }
    }

    let state = Arc::new(AtomicU8::new(State::Idle as u8));
    let stop = Arc::new(AtomicBool::new(false));
    let last_rx = Arc::new(AtomicU64::new(now_ms()));
    let peer_seen = Arc::new(AtomicBool::new(false));
    // 对端在最近一次 Pong 里报告的状态（0=Idle 1=Sending 2=Receiving）。
    // 控制消息单发 UDP 会丢，丢一包两边状态机就错位卡死；心跳 0.8s 一次
    // 每次都带状态，任何错位 2.5 秒内必然被看见并自愈。
    let peer_state = Arc::new(AtomicU8::new(0));
    // 本机最近一次"收回键鼠"的时间戳：收回后短暂冷却 Begin，防止用户的
    // 手还压在门口就立刻再次越界（日志实测 0.1s 内反复乒乓）
    let last_reclaim_ms = Arc::new(AtomicU64::new(0));

    let sealer = opts
        .key
        .map(|key| Sealer::new(&key, Sealer::fresh_prefix()));
    let encrypted = sealer.is_some();
    let out = Arc::new(Out {
        socket,
        peer: peer_addr,
        sealer: Mutex::new(sealer),
        seq: Mutex::new(0),
    });

    // ---- 抓取方向 ----
    //
    // Follow 模式只有键鼠端需要抓；Edge 模式两边对等，都要抓。
    // `edge_trigger` 决定"没抓取时"要不要盯着边缘：只有 Edge 模式需要，
    // Follow 模式装了钩子但完全不判断位置（否则切屏后光标停在边缘就会误触发）。
    let mut capture: Option<Arc<plat::Capture>> = None;
    if needs_capture {
        let (tx, rx) = mpsc::channel::<Captured>();
        let cap = Arc::new(plat::start_capture(
            opts.side,
            opts.mode == Mode::Edge,
            tx,
        )?);
        let state = state.clone();
        let stop = stop.clone();
        let out = out.clone();
        let capture_ref = cap.clone();
        let log = opts.log.clone();
        let on_panic = opts.on_panic.clone();
        let last_rx_cap = last_rx.clone();
        let peer_seen_cap = peer_seen.clone();
        let reclaim_cap = last_reclaim_ms.clone();
        let on_paste = opts.on_paste.clone();
        thread::spawn(move || {
            let mut pressed: Vec<Event> = Vec::new();
            // 转发统计：排障「键鼠到了对面却不动」时，需要知道 Mac 这边
            // 到底发出去多少笔、鼠标位移长什么样。Sending→Idle 翻转时打汇总。
            let mut was_sending = false;
            let mut forwarded: usize = 0;
            let mut motions: usize = 0;
            // 转发位移流采样（前 30 笔，与接收端收到的流对比定罪负位移源头）
            let mut fwd_sample = String::new();
            // 整轮位移记账：净位移 / 负向笔数与累计。
            // "推过去 1 秒就被弹回来"必须分清两件事 —— 是我们的流真的
            // 带着负位移把对端光标推回门口（Mac 侧钉扎 warp 泄漏/手飘），
            // 还是流是正的、对端注入没生效。前 30 笔采样看不出来这个。
            let mut sum_dx: i64 = 0;
            let mut sum_dy: i64 = 0;
            let mut neg_dx_count: usize = 0;
            let mut neg_dx_sum: i64 = 0;
            let mut min_dx: i32 = 0;
            let mut max_dx: i32 = 0;
            // 越界被拒（对端没回应）的告警限流
            let mut last_begin_warn_ms: u64 = 0;
            while !stop.load(Ordering::Relaxed) {
                // 状态翻转检查放在收包之前，超时唤醒的空转也能看到翻转
                let sending = state.load(Ordering::Relaxed) == State::Sending as u8;
                if was_sending && !sending && forwarded > 0 {
                    (log)(format!(
                        "本轮转发结束：共 {forwarded} 笔（鼠标位移 {motions} 笔，\
                         净 dx={sum_dx} dy={sum_dy}，负向 {neg_dx_count} 笔/{neg_dx_sum}，\
                         dx 范围 {min_dx}~{max_dx}）"
                    ));
                    forwarded = 0;
                    motions = 0;
                    fwd_sample.clear();
                    sum_dx = 0;
                    sum_dy = 0;
                    neg_dx_count = 0;
                    neg_dx_sum = 0;
                    min_dx = 0;
                    max_dx = 0;
                }
                was_sending = sending;

                let Ok(msg) = rx.recv_timeout(Duration::from_millis(200)) else {
                    continue;
                };
                match msg {
                    Captured::Begin { along } => {
                        // 边缘越界：只在 Edge 模式发生。撞到了就按 follow 的流程
                        // 走一遍（同样要先确认对端活着，否则不敢吞键鼠）。
                        // Idle = 正常越界；Receiving = 对端正控制着本机，
                        // 本地用户把光标推到门口 = 想把控制权抢回来（多屏共享
                        // 两边各有键鼠，这是唯一不靠热键的夺回方式）。
                        // 刚收回的短暂冷却：收回时光标就放在门边 16pt，用户
                        // 的手多半还压着门口，不冷却的话 0.1 秒内就再次越界，
                        // 日志实测来回乒乓。
                        if now_ms().saturating_sub(reclaim_cap.load(Ordering::Relaxed)) < 600 {
                            continue;
                        }
                        let cur = state.load(Ordering::Relaxed);
                        if cur != State::Idle as u8 && cur != State::Receiving as u8 {
                            continue;
                        }
                        // 对端必须活着才敢吞键鼠：吞了又送不出去，要等 2.5 秒
                        // 断线看守放回来，用户的体感就是"卡一下键鼠全没了"。
                        // follow() 有这道检查，这里（边缘越界）同样要有。
                        let alive = peer_seen_cap.load(Ordering::Relaxed)
                            && now_ms().saturating_sub(last_rx_cap.load(Ordering::Relaxed))
                                <= PEER_ALIVE_MS;
                        if !alive {
                            if now_ms().saturating_sub(last_begin_warn_ms) > 3000 {
                                last_begin_warn_ms = now_ms();
                                (log)("光标到了边缘，但对端没有回应，不越界".to_string());
                            }
                            continue;
                        }
                        capture_ref.set_grabbing(true);
                        state.store(State::Sending as u8, Ordering::SeqCst);
                        // 把出门位置和本屏尺寸带给对端：它从对应高度进门，
                        // 并按两屏比例缩放位移（不然两块屏分辨率不同时，
                        // 一边的 delta 到另一边会被放大/缩小得不成样子）
                        let (sw, sh) = capture_ref.screen();
                        // Enter 双发：丢一包对端就收不到开工信号（好在
                        // Pong 状态自愈 2.5s 兜底，双发把卡顿窗口压到最小）
                        out.send(&Event::Enter {
                            width: sw.min(65535) as u16,
                            height: sh.min(65535) as u16,
                            along,
                            flags: 0,
                            side: opts.side.code(),
                        });
                        out.send(&Event::Enter {
                            width: sw.min(65535) as u16,
                            height: sh.min(65535) as u16,
                            along,
                            flags: 0,
                            side: opts.side.code(),
                        });
                        if cur == State::Idle as u8 {
                            (log)("鼠标已越界到对端，Ctrl+Alt+Esc 可以交还".to_string());
                        } else {
                            (log)("光标推到门口，已从对端手里拿回控制权".to_string());
                        }
                    }
                    Captured::Input(ev) => {
                        if state.load(Ordering::Relaxed) != State::Sending as u8 {
                            continue;
                        }
                        // 按键/键盘逐笔落日志：定位"自发右键"这类问题
                        //（位移每秒几百笔，不打）
                        if let Event::Button { button, down } = ev {
                            (log)(format!(
                                "转发按键：button={button} {}",
                                if down { "按下" } else { "松开" }
                            ));
                        }
                        out.send(&ev);
                        forwarded += 1;
                        if let Event::Motion { dx, dy } = ev {
                            motions += 1;
                            sum_dx += dx as i64;
                            sum_dy += dy as i64;
                            if dx < 0 {
                                neg_dx_count += 1;
                                neg_dx_sum += dx as i64;
                            }
                            min_dx = min_dx.min(dx);
                            max_dx = max_dx.max(dx);
                            // 首笔位移打点：位移值异常（0/超大）一眼可见
                            if motions == 1 {
                                (log)(format!("开始转发鼠标位移（首笔 dx={dx}, dy={dy}）"));
                            }
                            // 前 30 笔转发流采样：与 Windows 收到的流对比，
                            // 定罪负位移是 Mac 发出的还是传输环节冒出来的
                            if motions <= 30 {
                                if motions == 1 {
                                    fwd_sample.clear();
                                }
                                fwd_sample.push_str(&format!(" {dx},{dy}"));
                                if motions == 30 {
                                    (log)(format!(
                                        "转发位移流（前 30 笔 dx,dy）：{fwd_sample}"
                                    ));
                                }
                            }
                        }
                        // 记住按下的键，松手时补 up
                        if ev.is_down() {
                            pressed.push(ev);
                        } else {
                            pressed.retain(|p| !same_key(p, &ev));
                        }
                    }
                    Captured::Note(text) => {
                        (log)(text);
                    }
                    Captured::PasteIntent => {
                        // 键已经被 tap 吞掉了，这里只负责通知上层去传文件。
                        // 不在这里做任何耗时动作 —— 这是抓取线程的接收循环。
                        if let Some(cb) = on_paste.as_ref() {
                            cb();
                        }
                    }
                    Captured::End => {
                        // 紧急收回：先把按着的键补 up，再告诉对端松手
                        if state.load(Ordering::Relaxed) == State::Sending as u8 {
                            // 先改状态再放抓取：顺序反过来的话，心跳守卫
                            // 可能正好读到"状态还是 Sending、抓取已放开"
                            // 这个瞬时错位，把刚交还的光标又抓回来。
                            state.store(State::Idle as u8, Ordering::SeqCst);
                            capture_ref.set_grabbing(false);
                            for ev in pressed.drain(..) {
                                out.send(&up_of(&ev));
                            }
                            out.send(&Event::Leave);
                            (log)("键鼠已收回（Ctrl+Alt+Esc）".to_string());
                            // 屏幕还在对端的话，一起切回来
                            if let Some(cb) = on_panic.as_ref() {
                                cb();
                            }
                        }
                    }
                }
            }
        });
        capture = Some(cap);
    }

    // ---- 接收方向：对端发来的输入 → 注入本机 ----
    {
        let state = state.clone();
        let stop = stop.clone();
        let out = out.clone();
        let last_rx = last_rx.clone();
        let peer_seen = peer_seen.clone();
        let log = opts.log.clone();
        let key = opts.key;
        let mode = opts.mode;
        let side = opts.side;
        let capture_rx = capture.clone();
        let peer_state_rx = peer_state.clone();
        let reclaim_rx = last_reclaim_ms.clone();
        thread::spawn(move || {
            // 门口方向可变：每当对端带着它自己的出门侧过来，就按拓扑改一次。
            // 两台屏水平相邻时，它从右边出去 ⇒ 我这边必然在它左边 —— 这是
            // 几何定理，不该让两台各自配、配反了就 1 秒弹回去。
            let mut side = side;
            let inject = plat::Inject::new();
            if mode == Mode::Edge {
                // 多屏共享：注入限制在"本机工作屏"内。工作屏不在这里定 ——
                // 物理用户可能换屏活动，每次收到 Enter（接管开始）时按
                // "物理用户最后活动的那块屏"重新锚定（见 win.rs LAST_WORK）。
                let layout = plat::monitor_layout();
                (log)(format!("显示器布局（x, y, 宽, 高）：{layout:?}"));
            }
            let mut opener = key.map(|k| Opener::new(&k));
            let mut buf = [0u8; 64];
            let mut last_seq: u16 = 0;
            let mut pressed: Vec<Event> = Vec::new();
            let mut injected: u64 = 0;
            // 回 Takeback 的限流时间戳（没在被控状态却收到输入事件时用）
            let mut last_takeback_ms: u64 = 0;
            // "推门交还"的武装状态：进门后要先往里走够远才武装，
            // 防止进门落点贴着门边、手一抖就被弹回去
            let mut edge_armed = false;
            // 进门时刻：交还判定要加冷却 —— 刚进门那一两秒手还在
            // 门口区域惯性摆动，不算"推门回去"
            let mut enter_ms: u64 = 0;
            // 顶住门边的起始时刻（0=没在顶）：日志用
            let mut edge_hold_ms: u64 = 0;
            // 朝门外累计推了多少点（见 RETURN_PUSH）：交还的**主判据**。
            // 离开门边就清零，所以只统计"贴着门还在往外推"的那一段。
            let mut out_push: i32 = 0;
            // 进门后前 30 笔位移的采样串（诊断「谁把光标按在门口」）
            let mut motion_sample = String::new();
            // 上次交还时刻：交还后 2 秒内不再触发交还 —— 弹跳循环
            //（交还→Mac 光标回门口→手没停→立刻再进门→又被按回门口）
            // 的破窗器：至少给用户留出可用窗口
            let mut last_giveback_ms: u64 = 0;
            // 对端屏幕尺寸（Enter 带来的，0 = 旧版或未知）。
            // 位移缩放用：对端宽度像素、本端宽度点，比例就是每单位
            // 对端位移该在本端走多少。
            let mut sender_size = (0.0_f64, 0.0_f64);
            // 注入有效性验证（v0.6.15，v0.6.18 改进判据）：Edge 模式被控
            // 期间，持续注入位移、真实光标却纹丝不动 = 注入环节失效
            //（SendInput 被安全软件拒收、事件进了看不见的屏、权限被吊销
            // ……）。此时对端还抓着键鼠往这里转发，用户两边全卡死、只能
            // 强制杀进程 —— 必须自动发现、立刻交还。探测需要**新发生的
            // 位移**：光标静置不动不会触发。
            // 判据（v0.6.18）：**账本走了 + 真实光标没动**才算失效。光标
            // 被推到屏幕对头钳死时，用户继续往那个方向推，位移照收、
            // 光标顶死不动 —— 账本也没走（钳位后目标=原地）。v0.6.15 只看
            // 真实光标，把这种物理事实误判成失效，自动交还 → 用户还在推
            // → 又越界 → 又顶死 → 又交还……来回横跳死循环（实测踩过：
            // 两秒一次交还，连着四五轮）。账本（注入器本地坐标跟踪）
            // 顶死在边上时不涨，天然区分这两种情况。
            // (窗口起始毫秒, 起始真实光标位置, 起始账本累计位移, 窗口内位移笔数)
            let mut verify: Option<(u64, (f64, f64), f64, u32)> = None;
            // 疑似失效计数：净位移判据在光标来回摆动时会假阳性（摆回
            // 起点净位移=0），必须连续两窗都判失效才交还，单窗只复检
            let mut suspect: u32 = 0;
            while !stop.load(Ordering::Relaxed) {
                let (len, from) = match out.socket.recv_from(&mut buf) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                // 只认对端的包，别的一律丢
                if from.ip() != out.peer.ip() {
                    continue;
                }
                let data = &buf[..len];
                let parsed = match opener.as_mut() {
                    Some(op) => op.open(data),
                    None => Event::decode(data).map(|ev| {
                        let seq = u16::from_le_bytes([data[2], data[3]]);
                        (seq, ev)
                    }),
                };
                let Some((seq, ev)) = parsed else {
                    continue;
                };
                // 收到任何合法包都算"对端还活着"
                last_rx.store(now_ms(), Ordering::Relaxed);
                peer_seen.store(true, Ordering::Relaxed);

                match ev {
                    Event::Enter {
                        width,
                        height,
                        along,
                        side: peer_side,
                        ..
                    } => {
                        if width > 0 && height > 0 {
                            sender_size = (width as f64, height as f64);
                        }
                        // 对端带了自己的出门侧：本机门口 = 它的对面。
                        // 这一条同时兜住「两台配置里 side 各配一半」的情况
                        //（v0.6.42 实测：Mac→Windows 每 0.5~2 秒就被交还，
                        // 就是因为 Windows 那边把门口当成了自己出门的那侧）。
                        if let Some(ps) = Side::from_code(peer_side) {
                            let want = ps.opposite();
                            if want != side {
                                side = want;
                                (log)(format!(
                                    "门口方向按对端校正：对端从 {} 出门 → 本机门口改到 {}",
                                    ps.as_str(),
                                    want.as_str()
                                ));
                            }
                        }
                        if state.load(Ordering::Relaxed) == State::Sending as u8 {
                            if mode == Mode::Edge {
                                // 多屏共享：我们正在转发，对端却发来 Enter =
                                // 它要拿走控制权（它的用户推门抢回，或它越界
                                // 过来）。让路：停止抓取、补齐按键，然后进入
                                // **被控**状态接它的事件 —— 必须是 Receiving 而
                                // 不是 Idle：设成 Idle 的话，对端接下来转发的
                                // 事件没人注入，两边就都卡死了（v0.6.5 踩过）。
                                // 不比较先后——双方同时推门时两边都让路，
                                // 最坏结果是这一轮谁也没拿到，再推一次即可。
                                // 先改状态再放抓取（见 Esc 分支同一说明）
                                state.store(State::Receiving as u8, Ordering::SeqCst);
                                if let Some(cap) = capture_rx.as_ref() {
                                    cap.set_grabbing(false);
                                }
                                for ev in pressed.drain(..) {
                                    inject.apply(&up_of(&ev));
                                }
                                out.send(&Event::Leave);
                                last_seq = seq;
                                injected = 0;
                                verify = None;
                                suspect = 0;
                                // 每次接管重新锚定工作屏：物理用户实际看着的那块
                                let own = inject.enable_own_screen(side);
                                let src = plat::work_screen_source();
                                (log)(format!("本机工作屏锚定：{own:?}（来源：{}，1=物理活动屏 5=窗口分布 2=主屏 3=光标屏 4=几何）", src));
                                // 从对端出门时的对应高度进门，别居中猛跳
                                let (ex, ey) = entry_spot(&inject, side, along);
                                inject.place_cursor(ex, ey);
                                edge_armed = false;
                                enter_ms = now_ms();
                                edge_hold_ms = 0;
                                motion_sample.clear();
                                (log)("对端过来接管了，键鼠已让给它".to_string());
                                continue;
                            }
                            // Follow 模式维持原语义：两边同时越界抢，让先动的
                            // 那个继续，后到的忽略。
                            (log)("对端也想过来了，先让它等".to_string());
                            continue;
                        }
                        let was = state.swap(State::Receiving as u8, Ordering::SeqCst);
                        last_seq = seq;
                        if was != State::Receiving as u8 {
                            injected = 0;
                            verify = None;
                            suspect = 0;
                            // 接管这一刻强制"放开抓取"：幂等调用，作用是
                            // 无条件重申 CGAssociate(true) + 清钉扎点。
                            // 光标若不是关联着的，注入的位移一个像素都推
                            // 不动（v0.6.40 的根因），进来先把这件事做实，
                            // 别等 0.8 秒后的守卫。
                            if let Some(cap) = capture_rx.as_ref() {
                                cap.set_grabbing(false);
                            }
                            // 光标落点：多屏共享按出门高度进门；跟随画面模式
                            // 没有"门"的概念，居中。重复的 Enter 不再挪，
                            // 免得光标莫名跳一下。
                            if mode == Mode::Edge {
                                // 每次接管重新锚定工作屏：物理用户实际看着的那块
                                let own = inject.enable_own_screen(side);
                                let src = plat::work_screen_source();
                                (log)(format!("本机工作屏锚定：{own:?}（来源：{}，1=物理活动屏 5=窗口分布 2=主屏 3=光标屏 4=几何）", src));
                                let (ex, ey) = entry_spot(&inject, side, along);
                                inject.place_cursor(ex, ey);
                            } else {
                                inject.center();
                            }
                            edge_armed = false;
                            enter_ms = now_ms();
                            edge_hold_ms = 0;
                            motion_sample.clear();
                            (log)("对端接管了键鼠（画面在你这边）".to_string());
                        }
                    }
                    Event::Leave => {
                        // 对端松手了，把可能残留的按下状态清掉
                        release_local(&inject, &mut pressed);
                        match State::from_u8(state.load(Ordering::Relaxed)) {
                            State::Receiving => {
                                state.store(State::Idle as u8, Ordering::SeqCst);
                                // 事件计数是排障的关键证据：有数 = 传输通、注入环节有问题；
                                // 为 0 = Enter 之后一条事件都没到（传输问题）
                                if injected > 0 {
                                    (log)(format!("对端已收回键鼠（本次注入 {injected} 笔事件）"));
                                } else {
                                    (log)("对端已收回键鼠（⚠️ 本次一条事件都没收到，传输断了）".to_string());
                                }
                                injected = 0;
                            }
                            State::Sending => {
                                // 我们在转发时收到 Leave = 对端接受了我们的
                                // 抢占（它推门被我们按下去了，主动让位）。
                                // **保持 Sending** —— 真正的交还是 Takeback。
                                (log)("对端已让位，控制权在我们这边".to_string());
                            }
                            State::Idle => {}
                        }
                    }
                    Event::Takeback => {
                        // 接收方主动交还（它的用户把光标推回门口，或它发现
                        // 状态错乱）。必须真的收回：否则我们一直抓着键鼠往
                        // 没人接收的方向转发，两台机器的键鼠全卡死 ——
                        // 这就是"键鼠突然都不能动了"的根源（v0.6.5 踩过）。
                        if state.load(Ordering::Relaxed) == State::Sending as u8 {
                            // 先改状态再放抓取：心跳守卫每秒查一次不变式，
                            // 顺序反了就会撞见"还在 Sending、抓取已放开"
                            // 的瞬时错位，反手再抓一次 —— 用户感知是交还
                            // 之后光标还卡半秒（实测日志有这串）。
                            state.store(State::Idle as u8, Ordering::SeqCst);
                            if let Some(cap) = capture_rx.as_ref() {
                                cap.set_grabbing(false);
                            }
                            reclaim_rx.store(now_ms(), Ordering::SeqCst);
                            (log)("对端把控制权还回来了，键鼠已收回本机".to_string());
                        }
                    }
                    // 心跳：last_rx 上面已经刷新了，Pong 里带上自己的状态 ——
                    // 对端拿它做"状态错位自愈"的依据（见心跳线程）。
                    // **必须回给来源**，不能走"当前会话目标"：多对端下 4 台
                    // 都会发 Ping，回错人等于让对端 2.5 秒后判它不理我。
                    // 状态也要按对端投影，理由见 state_for。
                    Event::Ping => {
                        let st = state_for(&state, out.peer.ip(), from.ip());
                        out.to(from, &Event::Pong { state: st });
                    }
                    Event::Pong { state: peer } => {
                        peer_state_rx.store(peer, Ordering::Relaxed);
                    }
                    _ => {
                        // 只有真在被控制时才注入。否则对端明明已经松手，
                        // 我们还在把它的事件打进系统 —— 注入的事件和本机
                        // 真实的键鼠打架，看起来就是"鼠标乱窜、点不了东西"。
                        if state.load(Ordering::Relaxed) != State::Receiving as u8 {
                            // 对端还在发输入事件 = 它以为它控制着我们
                            // （Enter 丢了，或它那边卡在 Sending）。回一条
                            // Takeback 让它收回；限流，别被每秒几百笔的
                            // 位移事件刷爆。
                            if now_ms().saturating_sub(last_takeback_ms) > 1000 {
                                last_takeback_ms = now_ms();
                                // 救援也要回给来源：把 Takeback 发给会话目标，
                                // 真正该收回的那台就永远收不到
                                out.to(from, &Event::Takeback);
                                out.to(from, &Event::Takeback);
                                (log)("没在被控状态却收到输入事件，已通知对端收回".to_string());
                            }
                            continue;
                        }
                        // 乱序的旧包丢掉 —— **只对位移生效**：按键（尤其
                        // up）被丢的代价是本机卡键/卡住左键，之后点什么
                        // 都没反应，比重复一笔点击严重得多。高频流下 UDP
                        // 偶发乱序，Button-up 被吞是实测踩过的坑。
                        if let Event::Motion { .. } = ev {
                            if seq.wrapping_sub(last_seq) > 0x8000 && seq != 0 {
                                continue;
                            }
                            last_seq = seq;
                        }
                        // 按键/键盘逐笔落日志：区分"Mac 端发来了右键"和
                        // "注入环节自己生出右键"（位移量大不打）
                        if let Event::Button { button, down } = ev {
                            (log)(format!(
                                "注入按键：button={button} {}",
                                if down { "按下" } else { "松开" }
                            ));
                        }
                        // 位移按两屏比例缩放：对端屏幕 1920 像素、本端
                        // 1728 点的话，对端 1px 的位移在本端该走 0.9 点，
                        // 而不是原样 1:1 硬灌 —— 那样小屏的 delta 到大屏
                        // 上会被放大，光标一动就冲出门被弹回去。
                        // （Deskflow 用绝对坐标映射屏幕形状，解决的是同一
                        // 个问题；我们是 delta 流，所以在注入前按比例换算。）
                        let to_inject = match &ev {
                            Event::Motion { dx, dy }
                                if sender_size.0 >= 1.0 && sender_size.1 >= 1.0 =>
                            {
                                let (lw, lh) = inject.screen();
                                Event::Motion {
                                    dx: rescale_delta(*dx, lw / sender_size.0),
                                    dy: rescale_delta(*dy, lh / sender_size.1),
                                }
                            }
                            _ => ev.clone(),
                        };
                        inject.apply(&to_inject);
                        injected += 1;
                        if injected == 1 {
                            // 把首笔事件的具体数值打进日志：位移是 0、键码是
                            // 乱码这类「内容坏掉」的问题，只有这里能看出来
                            (log)(format!("事件流开始注入（首笔：{to_inject:?}）"));
                        }
                        // 进门后前 30 笔位移流采样：Windows 日志实锤光标被
                        // 按死在门口 x=0 顶住 300ms，必须分辨是"位移流里
                        // 持续来负数"还是"外力直接挪真实光标"
                        if let Event::Motion { dx, dy } = to_inject {
                            if injected <= 30 {
                                if injected == 1 {
                                    motion_sample.clear();
                                }
                                motion_sample.push_str(&format!(" {dx},{dy}"));
                                if injected == 30 {
                                    (log)(format!(
                                        "进门位移流（前 30 笔 dx,dy）：{motion_sample}"
                                    ));
                                }
                            }
                        }
                        // ---- 注入有效性验证（Edge 模式）----
                        // 只看位移事件：攒够 40 笔且窗口 ≥1 秒后比对真实
                        // 光标与账本。真实光标没动 + 账本走了 = 注入全打了
                        // 水漂 —— 交还控制权保命。真实光标没动 + 账本也没
                        // 走 = 光标顶死在屏幕边上，是物理事实不是失效。
                        // 注意「没动」用的是净位移，光标来回摆动时净位移
                        // 会归零（实测 07:04 横跳轮就是被这个误杀的，每轮
                        // 恰好 1 秒 = 判定窗口关闭时刻）—— 所以判失效必须
                        // 连续两个窗口都成立才动手，单窗假象只记日志。
                        if mode == Mode::Edge {
                            if let Event::Motion { .. } = ev {
                                // (是否判失效, 真实光标移动, 账本移动)
                                let mut verdict: Option<(bool, f64, f64)> = None;
                                if verify.is_none() {
                                    let p = inject.cursor().unwrap_or((0.0, 0.0));
                                    verify = Some((now_ms(), p, inject.walked(), 1));
                                } else if let Some(slot) = verify.as_mut() {
                                    slot.3 += 1;
                                    if slot.3 >= 40 && now_ms().saturating_sub(slot.0) >= 1000 {
                                        let cur = inject.cursor().unwrap_or(slot.1);
                                        let real_moved =
                                            (cur.0 - slot.1.0).abs() + (cur.1 - slot.1.1).abs();
                                        let ledger_moved = inject.walked() - slot.2;
                                        let invalid =
                                            real_moved < 2.0 && ledger_moved >= 2.0;
                                        verdict = Some((invalid, real_moved, ledger_moved));
                                    }
                                }
                                match verdict {
                                    // 注入无效：先复检。连续第二个窗口仍失效才交还
                                    Some((true, real_moved, ledger_moved)) => {
                                        verify = None;
                                        if suspect >= 1 {
                                            release_local(&inject, &mut pressed);
                                            out.send(&Event::Takeback);
                                            out.send(&Event::Takeback);
                                            state.store(State::Idle as u8, Ordering::SeqCst);
                                            reclaim_rx.store(now_ms(), Ordering::SeqCst);
                                            (log)(format!(
                                                "⚠️ 连续两窗复检注入均失效（账本走了 \
                                                 {ledger_moved:.0} 点、真实光标只动 \
                                                 {real_moved:.1} 点），已通知对端收回键鼠"
                                            ));
                                            continue;
                                        }
                                        suspect += 1;
                                        // 判"失效"之前先做一次最廉价的自愈：
                                        // 重申光标关联（幂等）。v0.6.40 的根因
                                        // 就是光标被留在"断开"状态，注入推不动；
                                        // 接回去之后第二个窗口往往就正常了，
                                        // 比直接把键鼠抢回来对用户友好得多。
                                        if let Some(cap) = capture_rx.as_ref() {
                                            cap.set_grabbing(false);
                                        }
                                        (log)(format!(
                                            "疑似注入失效（账本走 {ledger_moved:.0} 点、\
                                             真实光标动 {real_moved:.1} 点），\
                                             已重申光标关联，复检中……"
                                        ));
                                    }
                                    // 注入正常（或只是顶死在屏幕边上）：重开窗口
                                    Some((false, ..)) => {
                                        verify = None;
                                        suspect = 0;
                                    }
                                    None => {}
                                }
                            }
                        }
                        if ev.is_down() {
                            pressed.push(ev);
                        } else {
                            pressed.retain(|p| !same_key(p, &ev));
                        }
                        // Edge 模式：对端光标撞回"朝着对端"的那条边（本机
                        // `side` 这一侧），说明用户想把控制权还回去了 ——
                        // 必须回 Takeback 让对端收回键鼠，否则它会一直抓着
                        // 键鼠往这边转发，两边全卡死。以前有两个 bug：判的
                        // 是 opposite（用户得横穿整个屏幕到最远端才触发），
                        // 而且触发后只是自己悄悄 Idle、对端毫不知情。
                        // Follow 模式不判断这个 —— 那边没有"边缘"概念，
                        // 交还是靠画面切回来或热键。
                        if mode == Mode::Edge {
                            // 用**缩放后**的位移（to_inject）：out_push 的阈值
                            // 是按本机屏幕算的，拿对端的原始 delta 记账会差一
                            // 整个缩放比（4K 被控端刚好差一倍）。
                            if let Event::Motion { dx, dy } = to_inject {
                                if let Some((x, y)) = inject.cursor() {
                                    let (w, h) = inject.screen();
                                    // 武装/交还的距离都按屏幕宽度缩放：4K 物理像素屏
                                    // 上固定的 32px 只等效 16 逻辑点，太小 —— 进门
                                    // 3 秒内正常来回移动就能穿越整个门口区，被误判
                                    // "推回门口"弹回去（实锤：Windows 日志每轮会话
                                    // 2~3 秒就以交还收场）。
                                    let scale = (w / 1920.0).max(1.0);
                                    // 距门边的向内距离。进门落点贴着门边：必须先
                                    // 往里走够远才武装交还判定，否则刚进门手一抖
                                    // 就被弹回去（Deskflow avoidJumpZone 同款问题）。
                                    let inside = match side {
                                        Side::Left => x,
                                        Side::Right => w - x,
                                        Side::Top => y,
                                        Side::Bottom => h - y,
                                    };
                                    if !edge_armed {
                                        if inside > EDGE_REARM_DIST * scale {
                                            edge_armed = true;
                                        }
                                        edge_hold_ms = 0;
                                        out_push = 0;
                                    } else if event::hit_edge(side, x, y, w, h) {
                                        // 交还判据（v0.6.30 重做）：**累计朝门外
                                        // 推了多远**，不再"顶住 300ms"。
                                        //
                                        // 纯时间判据的死穴是手感：手已经顶到墙
                                        // 上了，程序还要再等满 300 毫秒才认，
                                        // 而反方向（Mac→Windows）是撞边即走 ——
                                        // 用户感知就是"回来的时候卡一下"。
                                        //
                                        // 按位移记账后，一次果断推门只需三五十
                                        // 毫秒；门口扫过的一两笔杂音（每笔 2~8
                                        // 点）攒不到阈值，误切照样挡住。
                                        let outward = match side {
                                            Side::Left => -dx,
                                            Side::Right => dx,
                                            Side::Top => -dy,
                                            Side::Bottom => dy,
                                        };
                                        if outward > 0 {
                                            out_push += outward;
                                        }
                                        let now = now_ms();
                                        if edge_hold_ms == 0 {
                                            edge_hold_ms = now;
                                        }
                                        if out_push as f64 >= RETURN_PUSH * scale
                                            // 进门 0.5 秒内不算：手还在门口惯性里
                                            && now.saturating_sub(enter_ms) >= 500
                                            // 交还后 2 秒内不重复交还：打断
                                            // 门口弹跳循环（见变量注释）
                                            && now.saturating_sub(last_giveback_ms) >= 2000
                                        {
                                            let held = now.saturating_sub(edge_hold_ms);
                                            let pushed = out_push;
                                            edge_hold_ms = 0;
                                            out_push = 0;
                                            last_giveback_ms = now;
                                            release_local(&inject, &mut pressed);
                                            // 单发 UDP 会丢：丢一包对端就卡在 Sending
                                            // 连发两笔，接收端有状态闸门，幂等
                                            out.send(&Event::Takeback);
                                            out.send(&Event::Takeback);
                                            state.store(State::Idle as u8, Ordering::SeqCst);
                                            reclaim_rx.store(now_ms(), Ordering::SeqCst);
                                            // 数字落日志：位置（门口=0）+ 账本位置
                                            //（两者差得远 = 有外力直接挪真实光标，
                                            // 而不是注入的负位移流）
                                            let ledger = inject
                                                .tracked()
                                                .map(|(lx, ly)| format!("{lx:.0},{ly:.0}"))
                                                .unwrap_or_else(|| "?".to_string());
                                            (log)(format!(
                                                "推门交还（真实 {x:.0},{y:.0} / 账本 {ledger}，推了 {pushed} 点 {held}ms，进门 {:.1}s）",
                                                now.saturating_sub(enter_ms) as f64 / 1000.0,
                                            ));
                                        }
                                    } else {
                                        // 离开门边：累计位移与计时都清零 —— 攒的是
                                        // "这一顶"的量，不是历史总量
                                        edge_hold_ms = 0;
                                        out_push = 0;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    // ---- 心跳 + 断线看守 ----
    //
    // UDP 没有连接状态。对端进程崩了、网线拔了、Wi-Fi 断了，本机这边
    // 一个字节都收不到，但仍然停在 Sending —— 于是把自己的键鼠全吞了，
    // 用户看到的是"鼠标没了、键盘也死了"。
    {
        let state = state.clone();
        let stop = stop.clone();
        let out = out.clone();
        let last_rx = last_rx.clone();
        let log = opts.log.clone();
        let capture = capture.clone();
        let peer_state_gd = peer_state.clone();
        let reclaim_gd = last_reclaim_ms.clone();
        thread::spawn(move || {
            let mut warned = false;
            // 状态错位自愈的跟踪：进入当前状态的时间、对端最近一次报告
            // "配对状态"（我在 Sending 它在 Receiving，或反之）的时间
            let mut prev_me: u8 = State::Idle as u8;
            let mut entered_ms: u64 = now_ms();
            let mut peer_paired_ms: u64 = now_ms();
            // 对端旧版本（状态未知）只提示一次，别刷屏
            let mut degraded_logged = false;
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(PING_INTERVAL);
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                out.send(&Event::Ping);

                let me = state.load(Ordering::Relaxed);
                if me != prev_me {
                    prev_me = me;
                    entered_ms = now_ms();
                    // 乐观起点：刚进 Sending/Receiving 时对端可能还没收到
                    // Enter/还没换状态回 Pong，给它 2.5 秒表态时间
                    peer_paired_ms = now_ms();
                    warned = false;
                }

                // ---- 抓取状态一致性守卫 ----
                // 不变式：state==Sending ⇔ 抓取开着，其余 ⇔ 抓取关着。
                // 任何竞态留下的错位都是用户看得见的故障：
                //   在转发但没抓 = 本机光标照动、事件照转发 → 两边光标
                //                  同步移动、控制权像还在对端（实测踩过）；
                //   不在转发却抓着 = 键盘鼠标被吞、什么都点不动。
                // 每 0.8s 校一次，错位最多活一个心跳周期。已一致且在
                // Sending 时也重申一次抓取：Mac 端这会顺带重申光标关联
                // 状态，自愈可能留存的错位（幂等调用，无副作用）。
                if let Some(c) = capture.as_ref() {
                    // 钩子生命体征：被系统禁用 = 物理事件全漏本机 + 转发断供
                    if c.revive_tap() {
                        (log)("⚠️ 事件钩子被系统禁用（守卫发现），已强制重开".to_string());
                    }
                    // **现场重读状态，不用本轮开头缓存的 `me`**。
                    // 缓存的是这句 Ping 之前的值，而状态是收包线程在改，
                    // 中间可能刚好发生了一次交还：缓存说还在 Sending，
                    // 抓取却已经放开 → 判成"在转发却没抓" → 立刻反手
                    // 又抓回来。用户感知就是「回到 Mac 之后光标还要卡一下」
                    //（实测日志：交还同一秒内先"已重新抓住"、下一秒
                    // "不在转发却还抓着，已放开"）。
                    //
                    // 两次读夹住 is_grabbing()：前后不一致说明检查期间
                    // 状态在变，这一拍不判定。配合各条释放路径"先改
                    // state 再放开抓取"的顺序，误判窗口才真正关死。
                    let s_before = state.load(Ordering::SeqCst);
                    let have = c.is_grabbing();
                    let s_after = state.load(Ordering::SeqCst);
                    let want = s_before == State::Sending as u8;
                    if s_before == s_after && want != have {
                        c.set_grabbing(want);
                        (log)(if want {
                            "⚠️ 状态错位已纠正：在转发但抓取标志是关的（两边光标同步的根源），已重新抓住"
                                .to_string()
                        } else {
                            "⚠️ 状态错位已纠正：不在转发却还抓着键鼠，已放开".to_string()
                        });
                    } else if want {
                        c.set_grabbing(true);
                    } else if s_before == s_after {
                        // v0.6.40：**没在转发、抓取标志也是关的**，看着一切
                        // 一致，但光标可能还挂在"已断开"的残留状态里 —— 钉扎
                        // 线程最后一次 CGAssociate(false) 落在了释放之后（见
                        // mac.rs 钉扎线程注释）。这时本机被对端控制，注入的
                        // 位移完全推不动真实光标，几秒后就会被判"注入失效"把
                        // 键鼠抢回去，用户感知是「Windows 推过来控制不了 Mac」。
                        // set_grabbing(false) 是幂等的：它无条件重申
                        // CGAssociate(true)，顺手把 pin 清空。每 0.8 秒一次，
                        // 残留错位最多活一个心跳周期。
                        c.set_grabbing(false);
                    }
                }

                // ---- 状态错位自愈 ----
                // 控制消息（Enter/Takeback/Leave）是单发 UDP，丢一包两边
                // 状态机就各持一半真相：我们卡在 Sending 吞着键鼠，对端已经
                // Idle —— 心跳照常走，下面的"完全静音"看守永远不触发
                //（实测：来回多切几次必中一次，只能热键救）。
                // Pong 现在每 0.8s 带一次对端状态，任何错位 2.5 秒内可见。
                let peer = peer_state_gd.load(Ordering::Relaxed);
                // 对端状态未知（旧版本，Pong 不带状态字节）时不判定错位：
                // 没法判断就别动手，此时自愈降级为 v0.6.10 的行为
                //（完全静音看守 + Takeback 自愈仍然有效）。
                if peer == event::PEER_UNKNOWN && me != State::Idle as u8 && !degraded_logged {
                    degraded_logged = true;
                    (log)("对端是旧版本（心跳不带状态），状态错位自愈暂不生效，建议对端也升级到本版".to_string());
                }
                let paired = peer == event::PEER_UNKNOWN
                    || match State::from_u8(me) {
                        State::Sending => peer == State::Receiving as u8,
                        State::Receiving => peer == State::Sending as u8,
                        _ => true,
                    };
                if paired {
                    peer_paired_ms = now_ms();
                } else if now_ms().saturating_sub(peer_paired_ms) > 2500
                    && now_ms().saturating_sub(entered_ms) > 2500
                {
                    if me == State::Sending as u8 {
                        if let Some(capture) = capture.as_ref() {
                            capture.set_grabbing(false);
                        }
                        for code in plat::MODIFIER_KEYS {
                            out.send(&Event::Key { code, down: false });
                        }
                        for button in 1u8..=3 {
                            out.send(&Event::Button { button, down: false });
                        }
                        out.send(&Event::Leave);
                        state.store(State::Idle as u8, Ordering::SeqCst);
                        reclaim_gd.store(now_ms(), Ordering::SeqCst);
                        (log)("对端状态对不上（交还消息丢了？），键鼠已自动收回".to_string());
                    } else {
                        state.store(State::Idle as u8, Ordering::SeqCst);
                        reclaim_gd.store(now_ms(), Ordering::SeqCst);
                        (log)("对端已不在发送状态，本机退出被控".to_string());
                    }
                    peer_paired_ms = now_ms();
                    entered_ms = now_ms();
                    continue;
                }

                // ---- 完全静音看守 ----
                // Sending **和 Receiving** 都要看对端死活：对端进程崩了/被
                // 关掉，Pong 停了，peer_state 冻结在最后的合法值 —— 上面
                // 的"错位自愈"看到的是配对状态，永远不会触发。此前只救
                // Sending，Receiving 侧会永远卡在被控状态（Windows 端一关，
                // Mac 键鼠全冻，只能强制杀进程 —— 实测踩过）。
                if me == State::Idle as u8 {
                    warned = false;
                    continue;
                }
                if now_ms().saturating_sub(last_rx.load(Ordering::Relaxed)) > IDLE_TIMEOUT_MS
                    && !warned
                {
                    warned = true;
                    if me == State::Sending as u8 {
                        if let Some(capture) = capture.as_ref() {
                            capture.set_grabbing(false);
                        }
                        for code in plat::MODIFIER_KEYS {
                            out.send(&Event::Key {
                                code,
                                down: false,
                            });
                        }
                        for button in 1u8..=3 {
                            out.send(&Event::Button {
                                button,
                                down: false,
                            });
                        }
                        out.send(&Event::Leave);
                    }
                    state.store(State::Idle as u8, Ordering::SeqCst);
                    reclaim_gd.store(now_ms(), Ordering::SeqCst);
                    (log)(format!(
                        "对端 2.5 秒没有回应（{}），已自动回到空闲",
                        if me == State::Sending as u8 {
                            "键鼠已收回本机"
                        } else {
                            "退出被控状态"
                        }
                    ));
                }
            }
        });
    }

    let secret = if encrypted {
        "事件流已加密"
    } else {
        "未配对，事件流是明文，同网段可嗅探"
    };
    (opts.log)(match opts.mode {
        Mode::Follow => match opts.role {
            Role::Console => format!(
                "共用键鼠已就绪（{secret}）· 跟随画面模式，本机是键鼠端 —— \
                 画面切到对端时自动跟过去"
            ),
            Role::Remote => format!(
                "共用键鼠已就绪（{secret}）· 跟随画面模式，本机不抓键鼠，等对端接管"
            ),
        },
        Mode::Edge => format!(
            "共用键鼠已就绪（{secret}）· 多屏共享模式，鼠标推到屏幕{}侧越界",
            match opts.side {
                Side::Right => "右",
                Side::Left => "左",
                Side::Top => "上",
                Side::Bottom => "下",
            }
        ),
    });

    Ok(Kvm {
        state,
        stop,
        mode: opts.mode,
        role: opts.role,
        capture,
        out,
        last_rx,
        peer_seen,
        log: opts.log,
    })
}

fn same_key(a: &Event, b: &Event) -> bool {
    match (a, b) {
        (Event::Key { code: x, .. }, Event::Key { code: y, .. }) => x == y,
        (Event::Button { button: x, .. }, Event::Button { button: y, .. }) => x == y,
        _ => false,
    }
}

/// 把一个"按下"事件翻成对应的"抬起"
fn up_of(ev: &Event) -> Event {
    match *ev {
        Event::Key { code, .. } => Event::Key { code, down: false },
        Event::Button { button, .. } => Event::Button {
            button,
            down: false,
        },
        other => other,
    }
}

/// 本地这侧被控制结束：把注入过但没收 up 的键补上，免得本机卡键
fn release_local(inject: &plat::Inject, pressed: &mut Vec<Event>) {
    for ev in pressed.drain(..) {
        inject.apply(&up_of(&ev));
    }
}

/// 按比例缩放单轴位移，保底 ±1：比例小于 1 时（大屏→小屏）很小的
/// delta 换算完会变成 0，直接丢掉的话光标就挪不动了。
fn rescale_delta(v: i32, scale: f64) -> i32 {
    if v == 0 || scale <= 0.0 {
        return v;
    }
    let scaled = (v as f64 * scale).round() as i32;
    if scaled == 0 {
        v.signum()
    } else {
        scaled
    }
}

/// 进门落点：出门时光标沿门轴的位置比例，换算成对端进门边上的/// 对应坐标（Deskflow 的 jump cursor + mapToFraction 思路 ——
/// 从哪个高度出去，就从哪个高度进来）。比例未知（0xFFFF，
/// 跟随画面模式没有"门"）时退回该轴居中。
fn entry_spot(inject: &plat::Inject, side: Side, along: u16) -> (f64, f64) {
    let (w, h) = inject.screen();
    // 门口距离按屏幕宽度缩放：16 这个数是在 1080p 逻辑点上定的，
    // 4K 物理像素屏（3840 宽）上等效只有 8 逻辑点 —— 贴门太近，
    // 手一回拉就撞边。按宽度比例放大（4K = 32px）。
    let margin = ENTRY_MARGIN * (w / 1920.0).max(1.0);
    let f = if along == 0xFFFF {
        None
    } else {
        Some(along as f64 / 65535.0)
    };
    match side {
        Side::Left => (margin, f.map_or(h / 2.0, |f| (f * h).clamp(0.0, h))),
        Side::Right => (w - margin, f.map_or(h / 2.0, |f| (f * h).clamp(0.0, h))),
        Side::Top => (f.map_or(w / 2.0, |f| (f * w).clamp(0.0, w)), margin),
        Side::Bottom => (f.map_or(w / 2.0, |f| (f * w).clamp(0.0, w)), h - margin),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_roundtrip() {
        for s in [State::Idle, State::Sending, State::Receiving] {
            assert_eq!(State::from_u8(s as u8), s);
        }
    }

    #[test]
    fn role_parse() {
        assert_eq!(Role::parse("console"), Some(Role::Console));
        assert_eq!(Role::parse("本机"), Some(Role::Console));
        assert_eq!(Role::parse("remote"), Some(Role::Remote));
        assert_eq!(Role::parse("对端"), Some(Role::Remote));
        assert_eq!(Role::parse("nonsense"), None);
    }

    #[test]
    fn idle_timeout_is_sane() {
        // 必须明显大于心跳间隔，否则每个包都判超时
        assert!(IDLE_TIMEOUT_MS as u128 > PING_INTERVAL.as_millis() * 2);
        // 又不该长到用户以为程序死了
        assert!(IDLE_TIMEOUT_MS <= 5000);
        // 判定"对端活着"的窗口必须比断线超时宽，否则会反复抖动
        assert!(PEER_ALIVE_MS >= IDLE_TIMEOUT_MS);
    }
}
