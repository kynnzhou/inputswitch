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

use std::collections::HashMap;
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

/// 被控期间"物理抢回"的保护窗（毫秒）。
///
/// 进入被控后这个窗口内不认抢回信号：KVM 切输入源、显示器重新握手的那
/// 一两秒里，键盘会送来一串来源不明的按键（切源瞬间的 ghost key 很常见），
/// 认了就是"刚过去立刻被弹回来"。认过一次后同样静默这么久，免得连按两下
/// 连发两次 Takeback。
const CLAIM_ARM_MS: u64 = 1500;

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

/// `Enter.flags` 的 bit0：`side` 只用来定**进门落点**，不表示这条链路有门。
///
/// 回程专用。共用一块屏那台（Follow 链路）本来没有门，但用户从邻居那边
/// 把光标推回来时，它会希望光标从**自己刚推出去的那一侧**进来 —— 不带这
/// 一位的话，接收端只能把它摆到屏幕正中（v0.6.62 用户实测："从 0DFF1JU
/// 回来，光标直接跳到 MC5K9SJ 中间"）。
///
/// 为什么必须单独一位、不能直接发 side：接收端现在把"带有效 side"当成
/// "这条链路有门"的证据（v0.6.61 的修复，用于对方没摆位的情况）。回程
/// 借 side 传落点，如果不说清是哪种，那台共用屏机器就会开始判推门交还，
/// 光标在它上面一顶右边缘就把键鼠弹回 Mac。
const ENTER_FLAG_LANDING_ONLY: u16 = 1;

/// 改指向后，旧目标回来的 Takeback 要忽略多久（毫秒）。
///
/// 改指向的顺序是「Leave 旧 → Enter 新 → 换 active」，这中间旧目标可能还
/// 在收到几笔在途位移，它会按"没在被控状态却收到输入"的救援逻辑回一条
/// Takeback。v0.6.60 之前那种包会被 `active != from` 天然挡掉，只有
/// "刚改过去又改回来"这种极窄的情形才需要这条时间闸 —— 但那条路一旦踩中
/// 就是"改个方向键鼠全回来了"，不值得省这 20 行。
const RETARGET_GUARD_MS: u64 = 1000;

/// 两次改指向之间至少隔多久（毫秒）。
///
/// 按住 Ctrl+Alt+→ 会以键盘重复速率连发 keydown，没有这道闸就是每 30ms
/// 跳一次。也挡住"推边模拟"在同一趟顶门里连续触发。
const HOP_THROTTLE_MS: u64 = 1500;

/// 抓取侧发来的消息
#[derive(Clone, Debug)]
pub enum Captured {
    /// 鼠标撞到边缘，请求越界到对端（只在 Edge 模式发生）。
    /// `along` = 出门瞬间光标沿门轴的位置比例（0..65535）：
    /// 左右门是纵坐标、上下门是横坐标，让对端从对应高度进门。
    /// `side` = 撞的是哪条边 —— 一机配多台时平台层手上有四向掩码，
    /// 具体撞了哪一条要由它带回来，主循环据此选目标、并把它写进 Enter。
    Begin { along: u16, side: Side },
    /// 要转发给对端的事件
    Input(Event),
    /// 钩子线程发现异常，直接落日志（比如被系统超时禁用）
    Note(String),
    /// 按到了紧急收回（Ctrl+Alt+Esc）
    End,
    /// 被控期间本机用户真的动了物理键鼠（按下一个键 / 点了一下鼠标键）。
    ///
    /// 与"推门抢回"互补：推门要求光标跑到门口，键盘用户根本做不到。
    /// 判据全在平台层（注入事件带标记，钩子入口就把它们放行掉），
    /// 主循环只做时间窗与去抖 —— 那一层拿不到 Enter 的到达时刻。
    Claim,
    /// 转发期间按下了 Ctrl+V，而本机手上有"本次复制的文件清单"。
    /// 这个键已经被 tap 吞掉，交给上层去传文件、传完再补按。
    PasteIntent,
    /// 转发期间按下了 Ctrl+Alt+方向键：把键鼠改指向那一侧的机器。
    ///
    /// 为什么必须在 tap 里识别：抓取期间所有按键都被吞掉再转发，系统级
    /// 热键注册（global-hotkey）在这个阶段收不到任何东西 —— 和
    /// Ctrl+Alt+Esc 必须写在钩子里是同一个原因。
    ///
    /// `side` = 想去的方向（本机哪一侧的邻居），主循环按它查本机的 Edge 链路。
    Hop(Side),
}

#[cfg(target_os = "macos")]
use mac as plat;
#[cfg(windows)]
use win as plat;

pub use plat::Inject;

/// 平台层抓取参数。
///
/// 用命名结构体而不是几个位置参数：Mac 与 Windows 两份实现要逐个对位，
/// `sides`（u8）和 `edge_trigger`（bool）一旦传反，只有交叉编译那次才会
/// 暴露出来，而且症状是"某条边莫名不响应"，排查成本极高。
pub struct CaptureOpts {
    /// 四向掩码：哪几条边通向邻居。0 = 没有门（跟随画面模式）。
    /// P3 阶段由单一 `side` 折算，P4 之后由链路表生成。
    pub sides: u8,
    /// 没抓取时要不要盯着屏幕边缘（只有多屏共享模式需要）
    pub edge_trigger: bool,
}

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

/// 一条链路 = 一台对端。
///
/// `side` 是**本机**从哪一侧出去到它。`None` = 这条链路没有门（跟随画面
/// 链路：靠画面切换触发，不占边、不进掩码）。
pub struct Link {
    pub node: String,
    pub ip: IpAddr,
    pub side: Option<Side>,
    pub mode: Mode,
}

pub struct Options {
    /// 链路表：一台对端一条。第一台是主链路（面板显示/跟随画面的目标）。
    pub links: Vec<Link>,
    /// 全局模式。链路自己的 `mode` 优先（P6 才真正按链路分流，
    /// 之前两者同值，行为与单链路版一致）。
    pub mode: Mode,
    /// 本机角色。`Mode::Follow` 时才起作用。
    pub role: Role,
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
    /// 会话中"把光标顶到邻居那侧的边 → 把键鼠改指向过去"（v0.6.60，默认开）。
    ///
    /// 判据见 `EdgeSim`：没抓取时平台层才盯边缘，抓取期间只能拿我们自己
    /// 转出去的位移在本地复刻光标。误判（拖窗口到边、拉滚动条）时关掉它，
    /// 只剩 Ctrl+Alt+方向 热键。
    pub push_hop: bool,
}

/// 当前会话目标是哪条链路。
///
/// 存的是**链路下标 + 1**（0 = 没有会话），不是 IP：链路表在整个实例生命
/// 周期内不可变，"第几条"比"哪个地址"更稳（也顺带支持 IPv6）。抓取线程
/// 以 1000Hz 读它，所以只用原子量、不加锁。
///
/// 两条不变式靠它守着（见方案 §5）：
///   I1 `Sending ⇔ grabbing && active.is_some()`
///   I2 `state != Idle ⇒ active.is_some()`
/// 因此**建场先 set active 再改 state，拆场先改 state 再 clear active**。
#[derive(Clone)]
struct Active(Arc<AtomicU64>);

impl Active {
    fn new() -> Self {
        Self(Arc::new(AtomicU64::new(0)))
    }

    fn get(&self) -> Option<usize> {
        match self.0.load(Ordering::Relaxed) {
            0 => None,
            n => Some((n - 1) as usize),
        }
    }

    fn set(&self, index: Option<usize>) {
        let v = match index {
            Some(i) => i as u64 + 1,
            None => 0,
        };
        self.0.store(v, Ordering::SeqCst);
    }
}

/// 发送侧的共享状态。几个线程都要往里写，所以里面全是 Mutex。
struct Out {
    socket: UdpSocket,
    /// KVM 端口（= 每条链路的收件端口，全网一致）
    port: u16,
    links: Arc<Vec<Link>>,
    active: Active,
    /// None = 没配对，走明文
    sealer: Mutex<Option<Sealer>>,
    seq: Mutex<u16>,
}

impl Out {
    fn addr_of(&self, index: usize) -> Option<SocketAddr> {
        self.links
            .get(index)
            .map(|l| SocketAddr::new(l.ip, self.port))
    }

    /// 发一个事件给**当前会话目标**。会话流（位移/按键/Enter/Leave）走这条。
    ///
    /// 没有会话目标就丢掉：多对端下"发给谁"没有默认答案，猜一个等于
    /// 把 A 的键码打进 B 的窗口。
    fn send(&self, ev: &Event) {
        if let Some(i) = self.active.get() {
            self.to_link(i, ev);
        }
    }

    /// 按链路下标发送。**回包与救援必须用它**：多对端下"当前会话目标"
    /// 只是其中一台，把给 A 的 Pong 发给它，实际收件人可能是 B。
    fn to_link(&self, index: usize, ev: &Event) {
        if let Some(addr) = self.addr_of(index) {
            self.to(addr, ev);
        }
    }

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
fn state_for(state: &AtomicU8, active: Option<usize>, from: usize) -> u8 {
    if active == Some(from) {
        state.load(Ordering::Relaxed)
    } else {
        State::Idle as u8
    }
}

/// 四向掩码只来自 **Edge 链路**。
///
/// Follow 链路（"和本机共用一块屏"的那台）不能占边：它靠画面切换触发，
/// 一旦混进掩码，用户朝那侧推一下就会在画面没切走的情况下被吞掉键鼠。
/// 同一条边配了两条链路时按位或天然合并 —— 具体走哪条由"撞到的那条边"
/// 决定，这是掩码该有的语义。
fn link_mask(links: &[Link]) -> u8 {
    links
        .iter()
        .filter(|l| l.mode == Mode::Edge)
        .filter_map(|l| l.side)
        .fold(0u8, |m, s| m | s.bit())
}

/// 第 `index` 条链路对端活不活。
///
/// 抽成自由函数是因为三处都要同一份判据：`Kvm::link_alive`（面板/托盘）、
/// 抓取线程（撞边改指向）、接收线程（回程改指向）。三份各写一遍的下场是
/// "面板说在线、撞边说不在线"。
fn link_alive_at(peer_rx: &RxTable, links: &[Link], index: usize) -> bool {
    let Some(link) = links.get(index) else {
        return false;
    };
    let (idle, seen) = peer_rx.alive(link.ip);
    seen && idle <= PEER_ALIVE_MS
}

/// 撞到 `side` 这条边时，该把键鼠改指向谁。`None` = 不动作。
///
/// **查到的就是当前会话目标时返回 `None`** —— 单条 Edge 链路的两人拓扑
/// 永远走这一条，行为与改动前逐字节一致（那时撞边只会打一句"忽略"）。
///
/// 只有 `Sending` 才有"改指向"可言：Idle 是正常越界（另有一套建场流程）、
/// Receiving 是抢回（目标必须是当前那台，按 side 查表会让两台同时注入）。
fn pick_retarget(cur: u8, active: Option<usize>, links: &[Link], side: Side) -> Option<usize> {
    if cur != State::Sending as u8 {
        return None;
    }
    let to = links
        .iter()
        .position(|l| l.mode == Mode::Edge && l.side == Some(side))?;
    if Some(to) == active {
        None
    } else {
        Some(to)
    }
}

/// 回程 Enter 该带的方向字节（语义是"发送方从哪一侧出门"）。
///
/// 直接取目标链路自己的 `side`：门是**本机**通向那台的门。回到 Follow
/// 链路（共用一块屏那台）时它没有门，回 `CODE_UNKNOWN` —— 对端会沿用
/// 自己配置的方向，和 `follow()` 逐字一样。
/// 改指向时往 `Enter` 里塞的「进门侧」和标志位。
///
/// 两种情况完全不是一回事：
///
/// - 目标那台**有门**（Edge 链路）：就是那条边。对端据此把自己的门口
///   校正到我们的对面，光标落在那侧边缘。
/// - 目标那台**没有门**（Follow / 共用一块屏）：这是一次**回程**。拿
///   "来路那条边"的**反面**告诉它 —— `Enter.side` 的语义是"我方从哪侧
///   出门"，接收端会把门口取反，所以给反面才能让光标落回我们自己出去
///   的那一侧（给正面就成了屏幕另一头）。同时带上
///   `ENTER_FLAG_LANDING_ONLY`，否则对端会把这次回程当成"这条链路有
///   门"，从此开始判推门交还。
fn enter_aim(links: &[Link], from: usize, to: usize) -> (u8, u16) {
    if let Some(s) = links.get(to).and_then(|l| l.side) {
        return (s.code(), 0);
    }
    match links.get(from).and_then(|l| l.side) {
        Some(s) => (s.opposite().code(), ENTER_FLAG_LANDING_ONLY),
        None => (Side::CODE_UNKNOWN, 0),
    }
}

/// 这场会话算不算"有门"。
///
/// 两条证据取或：本地配置说它是多屏共享，**或者**对端的 `Enter` 带了
/// 真实出门侧。后者才是权威 —— 门是发送方拓扑的一部分，只有 Edge 链路
/// 发得出 side（Follow 恒发 `CODE_UNKNOWN`）。
///
/// 这个"或"不是宽容，是**必需**：发送方改了摆位、接收方没改（用户很自然
/// 地只在正在用的那台上面拖一下），按本地判就会把一条有门的链路当成
/// 共用一块屏 —— 推门交还整段不跑，人从那台回不来。
fn session_edge(local_edge: bool, peer_side: u8) -> bool {
    local_edge || Side::from_code(peer_side).is_some()
}

/// 用户按的那个方向，是不是"来路"。
///
/// 当前这台是我朝 `link_side` 出门才到的 ⇒ 按它的反面就等于想回去。
/// 反过来按同一个方向是"继续往外走"，那才该报"这一侧没有机器"。
fn is_homeward(link_side: Option<Side>, pressed: Side) -> bool {
    link_side.is_some_and(|s| s.opposite() == pressed)
}

/// 收到 Takeback 该怎么处理。
///
/// 三种结局分开表态的意义：以前只有"收回本机"一条路，而键鼠进了多跳
/// 会话之后，"对面交还"往往意味着**回到起点那台**（画面还在起点那台上，
/// 收回本机等于键鼠看不见）。
enum TakebackAction {
    /// 不认：不是当前目标，或刚改指向时旧目标的残包
    Ignore,
    /// 常规：收回本机
    ToIdle,
    /// 跳到这条链路（本次会话的起点）
    ToRoot(usize),
}

fn takeback_action(
    state: u8,
    active: Option<usize>,
    from: usize,
    guard: Option<(usize, u64)>,
    now: u64,
    root: Option<usize>,
) -> TakebackAction {
    // 只认当前会话目标：正给 A 转发时收到 B 的 Takeback（上一场残包）
    // 就收回键鼠，等于被已结束的会话打断
    if active != Some(from) {
        return TakebackAction::Ignore;
    }
    if state != State::Sending as u8 {
        return TakebackAction::Ignore;
    }
    if let Some((who, at)) = guard {
        if who == from && now.saturating_sub(at) < RETARGET_GUARD_MS {
            return TakebackAction::Ignore;
        }
    }
    match root {
        // 起点就是这台：常规交还。`root` 只在多跳里有意义，
        // 单链路两人拓扑恒等于 active，永远走 ToIdle
        Some(r) if r != from => TakebackAction::ToRoot(r),
        _ => TakebackAction::ToIdle,
    }
}

/// 「把会话改指向另一台」要用到的那一小撮共享状态。
///
/// 抓取线程（撞边、热键）、接收线程（回程）、托盘/面板（点名改指向）三处
/// 都要做同一件事，而它们手里只有各自 clone 出来的 Arc。抽成这个借用
/// 结构体，是为了让**动作只有一份实现** —— 三份复制的下场是某一份忘了
/// 补 key-up 或者忘了堵 Takeback，症状还是"改个方向键鼠全没了"。
struct Linker<'a> {
    state: &'a AtomicU8,
    out: &'a Out,
    links: &'a [Link],
    active: &'a Active,
    /// 本次会话的起点（多跳的回程目标）
    root: &'a Active,
    capture: Option<&'a Arc<plat::Capture>>,
    /// 刚改指向的目标与时刻（旧目标残包要忽略）
    guard: &'a Mutex<Option<(usize, u64)>>,
    hop_ms: &'a AtomicU64,
    peer_rx: &'a RxTable,
    log: &'a (dyn Fn(String) + Send + Sync),
}

impl Linker<'_> {
    fn alive(&self, index: usize) -> bool {
        link_alive_at(self.peer_rx, self.links, index)
    }

    /// 把正在进行的会话改指向 `to`：**不重启、不重新抓取、不碰画面**。
    ///
    /// 顺序不能换（四个动作各自防一个已实测的故障）：
    /// 1. 登记 guard —— 旧目标手上有在途位移，会回一条 Takeback 解围；
    /// 2. 旧目标补 key-up —— 不补的话对面一直以为 Ctrl 还按着；
    /// 3. `Leave` 双发 —— 单发丢一包旧目标就卡在 Receiving；
    /// 4. `Enter` 双发 —— **早于切 active**：反过来的话，Enter 还没到就把
    ///    位移灌给一台还在 Idle 的机器，它会按救援逻辑回 Takeback。
    fn retarget(&self, to: usize, along: u16, why: &str) -> Result<(), String> {
        if self.state.load(Ordering::Relaxed) != State::Sending as u8 {
            return Err("只有键鼠已经在本机对面时才谈得上改指向（现在空闲或被控）".to_string());
        }
        let Some(from) = self.active.get() else {
            return Err("当前没有会话目标".to_string());
        };
        if from == to {
            return Err("键鼠已经在那台".to_string());
        }
        let Some(link) = self.links.get(to) else {
            return Err("那条链路不在链路表里".to_string());
        };
        if !self.alive(to) {
            return Err(format!("{} 没有回应，键鼠还在原处", link.node));
        }
        // 起点只在第一跳时定：之后无论跳几次，回程都回这里
        if self.root.get().is_none() {
            self.root.set(Some(from));
        }
        if let Ok(mut g) = self.guard.lock() {
            *g = Some((from, now_ms()));
        }
        for code in plat::MODIFIER_KEYS {
            self.out
                .to_link(from, &Event::Key { code, down: false });
        }
        // **不补发 button-up。** 按下状态的清理是接收端自己的事：它的
        // `pressed` 里记着真正按下的键，收到 `Leave` 会逐笔补 up。这边
        // 盲发「1/2/3 全松开」会被对端当成**一次真实点击** —— Windows
        // 桌面上收到一个孤立的 WM_RBUTTONUP 就弹右键菜单（v0.6.62 用户
        // 实测：从 MC5K9SJ 转到 0DFF1JU 时，MC5K9SJ 上凭空弹一次右键）。
        // 键盘的 up 仍然补：多补一次无害，卡住一个键的代价大得多。
        self.out.to_link(from, &Event::Leave);
        self.out.to_link(from, &Event::Leave);
        let (sw, sh) = match self.capture {
            Some(c) => c.screen(),
            // 没有抓取模块时不可能在 Sending（上面已挡），这里只是兜底
            None => (0u32, 0u32),
        };
        let (enter_side, enter_flags) = enter_aim(self.links, from, to);
        let enter = Event::Enter {
            width: sw.min(65535) as u16,
            height: sh.min(65535) as u16,
            along,
            flags: enter_flags,
            side: enter_side,
        };
        self.out.to_link(to, &enter);
        self.out.to_link(to, &enter);
        // **最后**才切 active：上面几笔仍打在旧目标上（它刚收到 Leave，
        // 多出的几笔会被它当"没在被控却收到输入"丢掉），切完之后的流才
        // 打向新目标（它已经收到 Enter 了）。
        self.active.set(Some(to));
        self.hop_ms.store(now_ms(), Ordering::Relaxed);
        (self.log)(format!(
            "键鼠从 {} 转到 {}（本机直发，不经中转；{why}）",
            self.links[from].node, link.node
        ));
        Ok(())
    }
}

/// 一场会话（"对端正在把键鼠转发给我"）的全部易脏状态。
///
/// 为什么打包成一件事，而不是散成一堆局部变量：多对端下会换对端，而
/// 上一场残留下来的时钟、尺寸、按键栈会变成很难理解的故障 ——
/// 「刚进门 2 秒就被交还」（带着上一场的 `last_giveback_ms`）、
/// 「位移按另一台的分辨率缩放」（带着上一场的 `sender_size`）、
/// 「一到对端就卡住某个键」（带着上一场没补 up 的 `pressed`）。
/// 打包之后，换对端 = 整体重建，不再依赖"谁记得逐个清零"。
struct Session {
    /// 这场会话的对端。换对端必须整体重建 —— 判断"要不要重建"就看它。
    peer: IpAddr,
    /// 对端节点名（日志用）
    node: String,
    /// 这条链路是不是"有门的"（Edge）。注入落点、推门交还都只对
    /// 有门的链路成立 —— 跟随画面的那条不能按边判定，否则用户朝任意
    /// 一侧推一下就被当成交还了。
    edge: bool,
    /// 本机门口方向（朝对端那一侧）。Enter 带来的"对端出门侧"会校正它。
    portal: Side,
    /// 上一笔的 seq（位移乱序丢弃用）
    last_seq: u16,
    /// 本机按下尚未松开的键/钮，交还时统一补 up（否则对端卡键）
    pressed: Vec<Event>,
    /// 本会话注入了多少笔。0 = 传输断在 Enter 之后，是排障的第一手证据
    injected: u64,
    /// 回 Takeback 的限流时间戳（没在被控状态却收到输入事件时用）
    last_takeback_ms: u64,
    /// "推门交还"的武装状态：进门后要先往里走够远才武装，
    /// 防止进门落点贴着门边、手一抖就被弹回去
    edge_armed: bool,
    /// 进门时刻：交还判定要加冷却，刚进门那一两秒手还在门口惯性里摆动
    enter_ms: u64,
    /// 顶住门边的起始时刻（0=没在顶）：日志用
    edge_hold_ms: u64,
    /// 朝门外累计推了多少点（见 RETURN_PUSH）：交还的**主判据**。
    /// 离开门边就清零，所以只统计"贴着门还在往外推"的那一段。
    out_push: i32,
    /// 进门后前 30 笔位移的采样串（诊断「谁把光标按在门口」）
    motion_sample: String,
    /// 上次交还时刻：交还后 2 秒内不再触发交还 —— 弹跳循环
    ///（交还→光标回门口→手没停→立刻再进门→又被按回门口）的破窗器
    last_giveback_ms: u64,
    /// 对端屏幕尺寸（Enter 带来的，0 = 旧版或未知）。位移缩放用：
    /// 对端像素 / 本端点 就是每单位对端位移该在本端走多少。
    sender_size: (f64, f64),
    /// 注入有效性验证窗口：(起始毫秒, 起始真实光标位置, 起始账本位移, 笔数)
    verify: Option<(u64, (f64, f64), f64, u32)>,
    /// 疑似失效计数：净位移判据在光标来回摆动时会假阳性（摆回起点
    /// 净位移=0），必须连续两窗都判失效才交还，单窗只复检
    suspect: u32,
}

impl Session {
    fn new(peer: IpAddr, node: String, portal: Side, edge: bool) -> Self {
        Self {
            peer,
            node,
            edge,
            portal,
            last_seq: 0,
            pressed: Vec::new(),
            injected: 0,
            last_takeback_ms: 0,
            edge_armed: false,
            enter_ms: 0,
            edge_hold_ms: 0,
            out_push: 0,
            motion_sample: String::new(),
            last_giveback_ms: 0,
            sender_size: (0.0, 0.0),
            verify: None,
            suspect: 0,
        }
    }
}

/// **发送侧的"门口"模拟器**：会话中判断"用户是不是把光标顶到邻居那侧了"。
///
/// 为什么非得自己算：`Sending` 一定处于 grabbing，而平台层把边缘判定写在
/// `!grabbing` 分支里（`mac.rs:797` / `win.rs:761`），抓取期间连 `Begin`
/// 都产生不出来；系统级热键在同一段代码里被吞掉。**唯一剩下的信号就是
/// 我们自己转出去的那串位移** —— 共用一块屏意味着两块屏尺寸、比例一致，
/// 本地按同一份位移推一遍"对端光标在哪"才有意义（这也是为什么只在
/// `Follow` 会话里跑：两块屏并排时比例可能不同）。
///
/// 判据与接收侧（对端推门交还，见 `Session` 那段 `out_push`）逐条对齐：
/// 先离边够远才武装 → 贴边 → 累计朝外推够 `RETURN_PUSH` → 节流。那边是
/// "用户想把键鼠交回去"，这边是"用户想跨过去"，同一只手该有同一套手感。
struct EdgeSim {
    /// 复刻出来的光标位置（点）。`None` = 还没初始化，会话开始时居中。
    pos: Option<(f64, f64)>,
    /// 离边够远才武装：进门落点就在门口，不设这道闸一进门就被判"要出去"
    armed: bool,
    /// 朝门外累计推了多少点
    out_push: i32,
    /// 上次命中时刻（节流基准）
    last_ms: u64,
}

impl EdgeSim {
    fn new() -> Self {
        Self {
            pos: None,
            armed: false,
            out_push: 0,
            last_ms: 0,
        }
    }

    /// 换会话目标时复位：新的起点没有"上一趟推了多少"可言。
    fn reset(&mut self) {
        self.pos = None;
        self.armed = false;
        self.out_push = 0;
    }

    /// 喂一笔转发出去的位移。返回 `Some((边, 沿门轴比例))` = 用户顶到门口了。
    fn feed(
        &mut self,
        dx: i32,
        dy: i32,
        w: f64,
        h: f64,
        mask: u8,
        now: u64,
    ) -> Option<(Side, u16)> {
        if w < 2.0 || h < 2.0 {
            return None;
        }
        // Follow 会话的进门落点是**屏幕中心**（对端 `inject.center()`），
        // 本机也从中心起算，两边才是同一个坐标系
        let (mut x, mut y) = self.pos.unwrap_or((w / 2.0, h / 2.0));
        x = (x + dx as f64).clamp(0.0, w - 1.0);
        y = (y + dy as f64).clamp(0.0, h - 1.0);
        self.pos = Some((x, y));

        let scale = (w / 1920.0).max(1.0);
        if !self.armed {
            if !event::within_edge(mask, x, y, w, h, EDGE_REARM_DIST * scale) {
                self.armed = true;
            }
            self.out_push = 0;
            return None;
        }
        let Some(side) = event::hit_edge_in(mask, x, y, w, h) else {
            // 离开门边：攒的是"这一顶"的量，不是历史总量
            self.out_push = 0;
            return None;
        };
        let outward = match side {
            Side::Left => -dx,
            Side::Right => dx,
            Side::Top => -dy,
            Side::Bottom => dy,
        };
        if outward > 0 {
            self.out_push += outward;
        }
        if (self.out_push as f64) < RETURN_PUSH * scale
            || now.saturating_sub(self.last_ms) < HOP_THROTTLE_MS
        {
            return None;
        }
        self.out_push = 0;
        // 这一趟走完了，重新武装得先离开门边 —— 否则刚转过去那一瞬，
        // 手的惯性还在继续朝同一侧推，会立刻再转一次
        self.armed = false;
        self.last_ms = now;
        Some((side, side.along(x, y, w, h)))
    }
}

/// 某个对端的心跳与存活信息
struct PeerRx {    /// 最后一次收到合法包的时刻（毫秒）
    last_rx: u64,
    /// 有没有真的收到过包。启动时 last_rx 就是当前时间，单看它会把
    /// "从没连上"误判成"刚刚连上"。
    seen: bool,
    /// 最近一次 Pong 里报的状态（`PEER_UNKNOWN` = 老版本，不带这个字节）
    state: u8,
}

type PeerRxMap = Arc<Mutex<HashMap<IpAddr, PeerRx>>>;

/// 每对端一份的心跳表。多对端下这三个值都必须按来源分开记，
/// 否则「谁还活着」和「谁报的什么状态」会互相覆盖。
#[derive(Clone)]
struct RxTable(PeerRxMap);

impl RxTable {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(HashMap::new())))
    }

    fn entry(&self, ip: IpAddr) -> PeerRx {
        self.0
            .lock()
            .map(|t| {
                t.get(&ip).map(|r| PeerRx {
                    last_rx: r.last_rx,
                    seen: r.seen,
                    state: r.state,
                })
            })
            .ok()
            .flatten()
            .unwrap_or(PeerRx {
                // 没记录过：当作"刚刚收到过"（避免一上来就判超时），但
                // seen=false 让调用方知道它从没说过话
                last_rx: now_ms(),
                seen: false,
                state: State::Idle as u8,
            })
    }

    /// 收到该对端的任意合法包
    fn touch(&self, ip: IpAddr) {
        if let Ok(mut t) = self.0.lock() {
            let e = t.entry(ip).or_insert(PeerRx {
                last_rx: 0,
                seen: false,
                state: State::Idle as u8,
            });
            e.last_rx = now_ms();
            e.seen = true;
        }
    }

    fn idle_ms(&self, ip: IpAddr) -> u64 {
        now_ms().saturating_sub(self.entry(ip).last_rx)
    }

    fn alive(&self, ip: IpAddr) -> (u64, bool) {
        let e = self.entry(ip);
        (now_ms().saturating_sub(e.last_rx), e.seen)
    }

    fn state(&self, ip: IpAddr) -> u8 {
        self.entry(ip).state
    }

    fn set_state(&self, ip: IpAddr, st: u8) {
        if let Ok(mut t) = self.0.lock() {
            let e = t.entry(ip).or_insert(PeerRx {
                last_rx: now_ms(),
                seen: true,
                state: st,
            });
            e.state = st;
        }
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
    /// 每个对端一份的心跳/存活记录。**不能是单槽**：静音看守只该看
    /// 当前会话那台，别台的心跳替它续命的话，它崩了本机会一直卡在
    /// 转发里（键鼠全吞）。
    peer_rx: RxTable,
    /// 链路表（与 `out.links` 同一份）
    links: Arc<Vec<Link>>,
    /// 当前会话目标
    active: Active,
    /// **本次会话的起点**（多跳的回程目标）。
    ///
    /// 只有"共用屏那台 → 并排那台"这种一跳才用得上：对面交还时如果
    /// 直接收回本机，画面还在起点那台上，用户看不见光标。有了它，
    /// 交还 = 改指向回起点，画面和键鼠重新一致。
    root: Active,
    /// 刚改指向到的链路与时刻。旧目标手上有在途位移，会回 Takeback 解围，
    /// 这一毫秒级的残包不能当真（否则刚改过去就被打回 Idle）。
    retarget_guard: Arc<Mutex<Option<(usize, u64)>>>,
    /// 上次改指向的时刻（热键重复、推边连击的节流基准）
    hop_ms: Arc<AtomicU64>,
    log: Arc<dyn Fn(String) + Send + Sync>,
}

impl Kvm {
    pub fn state(&self) -> State {
        State::from_u8(self.state.load(Ordering::Relaxed))
    }

    pub fn role(&self) -> Role {
        self.role
    }

    /// 全局模式。多链路时返回主链路的模式（P6 起才按链路分）。
    pub fn mode(&self) -> Mode {
        self.links.first().map(|l| l.mode).unwrap_or(self.mode)
    }

    /// 有没有"跟随画面"链路。切输入源之后要不要顺手把键鼠交过去，
    /// 问的是这个而不是 `mode()`：一机两链时（共享屏 + 跨屏并排），
    /// 主链路是 Edge 也不该让切换屏时的键鼠跟随失效。
    pub fn has_follow_link(&self) -> bool {
        self.links.iter().any(|l| l.mode == Mode::Follow)
    }

    pub fn links(&self) -> &[Link] {
        &self.links
    }

    /// 当前握着键鼠的那台（Sending/Receiving 时有值）
    pub fn active_node(&self) -> Option<&str> {
        self.active
            .get()
            .and_then(|i| self.links.get(i))
            .map(|l| l.node.as_str())
    }

    /// 当前会话走的是不是"有门"的链路（Edge）。
    ///
    /// 托盘那条"VCP 已经是本机口 → 收回键鼠"的自证兜底只对跟随画面会话
    /// 成立：Edge 链路根本不切输入源，VCP 永远等于本机，按它收回等于
    /// 每 5 秒把键鼠抢回来一次。
    pub fn active_is_edge(&self) -> bool {
        self.active
            .get()
            .and_then(|i| self.links.get(i))
            .map(|l| l.mode == Mode::Edge)
            .unwrap_or(false)
    }

    fn link_alive(&self, index: usize) -> bool {
        link_alive_at(&self.peer_rx, &self.links, index)
    }

    /// 把这三个 Arc 借成一份可执行"改指向"的视图。
    fn linker(&self) -> Linker<'_> {
        Linker {
            state: &self.state,
            out: &self.out,
            links: &self.links,
            active: &self.active,
            root: &self.root,
            capture: self.capture.as_ref(),
            guard: &self.retarget_guard,
            hop_ms: &self.hop_ms,
            peer_rx: &self.peer_rx,
            log: &*self.log,
        }
    }

    /// 本次会话是从哪台开始的（多跳的回程目标）。没有会话 = None。
    pub fn root_node(&self) -> Option<&str> {
        self.root.get().and_then(|i| self.links.get(i)).map(|l| l.node.as_str())
    }

    /// 本次会话的起点是不是就是当前这台（单跳/单链路拓扑恒为 true）。
    pub fn at_root(&self) -> bool {
        match (self.root.get(), self.active.get()) {
            (Some(r), Some(a)) => r == a,
            _ => true,
        }
    }

    /// **会话中把键鼠改指向另一台**（画面不动）。
    ///
    /// 与 `hand_over_to` 的分工：那个负责"从空闲开始建一场会话"，这个负责
    /// "会话已经开着，只换收件人"。多跳的一切触发面（热键、推边、面板
    /// 按钮）最终都落到这里。
    pub fn retarget_to(&self, node: &str) -> Result<(), String> {
        let Some(target) = self.links.iter().position(|l| l.node == node) else {
            return Err(format!("没有叫 {node} 的链路"));
        };
        self.linker().retarget(target, 0xFFFF, "手动改指向")
    }

    /// 把键鼠转给「下一条有门的链路」：热键和托盘菜单用。
    ///
    /// 多台并排时"下一条"没有唯一答案，这里按链路表顺序取第一条既不是
    /// 当前目标、又活着的 Edge 链路 —— 两台并排（最常见的拓扑）下它
    /// 就是另外那台。
    pub fn retarget_next_edge(&self) -> Result<(), String> {
        let cur = self.active.get();
        let next = self
            .links
            .iter()
            .enumerate()
            .filter(|(i, l)| l.mode == Mode::Edge && Some(*i) != cur)
            .find(|(i, _)| self.link_alive(*i))
            .map(|(i, _)| i);
        match next {
            Some(i) => self.linker().retarget(i, 0xFFFF, "托盘/热键改指向"),
            None => Err("没有别的有门链路可转（先把它摆到摆位图的某个方向）".to_string()),
        }
    }

    /// 距离上次收到**当前会话对端**消息多久了（毫秒）。没有会话 = u64::MAX。
    pub fn idle_ms(&self) -> u64 {
        match self.active.get().and_then(|i| self.links.get(i)) {
            Some(l) => self.peer_rx.idle_ms(l.ip),
            None => u64::MAX,
        }
    }

    /// 有没有任何一条链路在对端活着。面板用它回答"对面在不在"——
    /// 空闲时也是 true，不能只看当前会话目标。
    pub fn peer_alive(&self) -> bool {
        (0..self.links.len()).any(|i| self.link_alive(i))
    }

    /// 跟随画面模式的目标链路：优先第一条 Follow 链路，没有就用第一条。
    ///
    /// 切屏跟键鼠跟的是"画面切到了哪台"，而 v3 的链路表里 Follow 链路
    /// 正是那些"和本机共用一块屏"的机器。多条 Follow（一屏两机以上的
    /// 少见拓扑）时取第一条，更复杂的选择留给 P6 的按口分流。
    fn follow_target(&self) -> Option<usize> {
        self.links
            .iter()
            .position(|l| l.mode == Mode::Follow)
            .or(if self.links.is_empty() { None } else { Some(0) })
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
    ///
    /// 返回 `Ok(true)` = 真的开始转发了；`Ok(false)` = 已经有会话在进行，
    /// 这次什么都没做。以前空转也返回 `Ok(())`，调用方（`do_switch`、
    /// 面板「交给对端」）一律当成交接成功 —— 那是"键鼠已跟过去"这句
    /// 谎报的源头。调用方拿到的 `false` 必须原样说给用户听。
    pub fn follow(&self) -> Result<bool, String> {
        // P5：不再按 role 挡。键鼠插在哪台是**物理事实**，不是配置项 ——
        // 用户可以随时把键盘从这台拔到那台，程序不该要求他先回来改配置。
        // 能不能往外发只取决于"抓没抓到键鼠"（下面的 capture），而那件事
        // 在 start() 时已经按权限判过了。真要没抓取，那句"抓取模块没有
        // 启动"比"你的键鼠不在本机"准确得多。
        if self.state() != State::Idle {
            return Ok(false);
        }
        let Some(target) = self.follow_target() else {
            return Err("没有配置任何对端链路".to_string());
        };
        if !self.link_alive(target) {
            return Err(
                "对端没有回应（对面没开程序、不在同一网段，或防火墙拦了）。\
                 键鼠没有交出去，你还能操作本机"
                    .to_string(),
            );
        }
        let Some(capture) = self.capture.as_ref() else {
            return Err("抓取模块没有启动".to_string());
        };
        // I2：先把会话目标写上，再改状态 —— 反过来会有一个窗口，
        // 状态已经是 Sending 而 active 还是空，转发流找不到收件人被丢掉。
        self.active.set(Some(target));
        // 起点就是这里：之后从共用屏那台再转去并排那台时，回程要回这儿
        self.root.set(Some(target));
        capture.set_grabbing(true);
        self.state.store(State::Sending as u8, Ordering::SeqCst);
        // 跟随画面模式下没有"门"的概念，出门位置未知（0xFFFF），
        // 对端居中落点即可；屏幕尺寸照带，位移缩放照样生效
        let (sw, sh) = capture.screen();
        self.out.to_link(
            target,
            &Event::Enter {
                width: sw.min(65535) as u16,
                height: sh.min(65535) as u16,
                along: 0xFFFF,
                flags: 0,
                // 跟随画面模式没有门，门口方向留给对端自己决定
                side: Side::CODE_UNKNOWN,
            },
        );
        (self.log)(format!(
            "键鼠已交给 {}（画面在那边）",
            self.links[target].node
        ));
        Ok(true)
    }

    /// **把键鼠交给指定的那台。**
    ///
    /// 和 `follow()` 的差别只在"目标是哪台"：`follow` 是切屏成功之后跟画面走
    /// （靠 `follow_target()` 猜一条 Follow 链路），这个是人手指名道姓。
    /// 一机多台时不能再猜 —— 猜错的表现是"点了 A 结果键鼠跑到 B 去了"。
    ///
    /// v0.6.60 起两条路合成一条：**空闲时**建一场新会话；**已经在转发时**
    /// 换成改指向（以前这里一律拒绝，于是"面板上看得到却点不动"，用户只能
    /// 先收回再交出去 —— 而收回时画面还在对面，看不见面板）。
    pub fn hand_over_to(&self, node: &str) -> Result<(), String> {
        let Some(target) = self.links.iter().position(|l| l.node == node) else {
            return Err(format!("没有叫 {node} 的链路"));
        };
        match self.state() {
            // 被控时"交给别人"等于把别人的控制权转走，本机没这个权力：
            // 得先请对端交还（Ctrl+Alt+Esc），再交出去
            State::Receiving => {
                Err("键鼠现在在对面手里，先按 Ctrl+Alt+Esc 收回来再交给别的机器".to_string())
            }
            State::Sending => self.linker().retarget(target, 0xFFFF, "手动改指向"),
            State::Idle => {
                if !self.link_alive(target) {
                    return Err(format!(
                        "{node} 没有回应（对面没开程序、不在同一网段，或防火墙拦了）。\
                         键鼠没有交出去，你还能操作本机"
                    ));
                }
                let Some(capture) = self.capture.as_ref() else {
                    return Err("抓取模块没有启动".to_string());
                };
                // I2：先写会话目标再改状态，否则转发流有一瞬间找不到收件人
                self.active.set(Some(target));
                self.root.set(Some(target));
                capture.set_grabbing(true);
                self.state.store(State::Sending as u8, Ordering::SeqCst);
                let (sw, sh) = capture.screen();
                let link = &self.links[target];
                // 有门的链路把门的方向告诉对端（拓扑决定了它一定在相反那侧）；
                // Follow 链路没有门，交给对端沿用自己配置的方向。
                let side = link.side.map(|s| s.code()).unwrap_or(Side::CODE_UNKNOWN);
                self.out.to_link(
                    target,
                    &Event::Enter {
                        width: sw.min(65535) as u16,
                        height: sh.min(65535) as u16,
                        along: 0xFFFF,
                        flags: 0,
                        side,
                    },
                );
                (self.log)(format!("键鼠已交给 {}", link.node));
                Ok(())
            }
        }
    }

    /// 第 `index` 条链路对端活不活。面板每行都要显示，所以公开出去。
    pub fn alive_at(&self, index: usize) -> bool {
        self.link_alive(index)
    }

    /// 按节点名查对端活不活。`None` = 链路表里没有这台。
    ///
    /// 切画面之前要问这一句：**对端离线时切画面是条单行道** —— 画面走了、
    /// 键鼠还在本机，DDC 又读不到（实测切走后 9/9 失败），用户既看不见
    /// 本机也没法操作对端，只能去按显示器自己的按钮。v0.6.62 用户实测
    /// "Mac 切 MC5K9SJ 键鼠用不了"就是这么来的（那台机器当时整机不在网）。
    pub fn link_alive_by_node(&self, node: &str) -> Option<bool> {
        self.links
            .iter()
            .position(|l| l.node == node)
            .map(|i| self.link_alive(i))
    }

    /// 现在握着键鼠的是第几条链路。没有会话就是 None。
    pub fn active_index(&self) -> Option<usize> {
        self.active.get()
    }

    /// **画面切回本机了 / 要拿回键鼠 → 停止转发。**
    ///
    /// 幂等：本来就在本机时什么都不做。
    ///
    /// 注意：**只处理"键鼠在我们手里"这一种**。被控时的收回走
    /// `take_back_from_peer()` —— 两条路都是"把控制权拿回本机"，但
    /// 前者是本地停手、后者是求人交还，混在一起会让三条"画面切回来了"
    /// 的自动路径在 Receiving 时误发 Takeback。
    ///
    /// 返回 false = 本来就没什么可收的（不是 Sending）。调用方要据此决定
    /// 能不能说"键鼠已收回本机" —— 以前它是静默空转，界面照样报收回成功。
    pub fn give_back(&self) -> bool {
        self.release(None)
    }

    /// 本机有没有抓取能力。没有就只能被控（`follow()` 会直接报
    /// 「抓取模块没有启动」），面板据此不要说"键鼠跟过去"。
    pub fn can_capture(&self) -> bool {
        self.capture.is_some()
    }

    /// 被控时（对端正控制本机）把控制权要回来。
    ///
    /// 返回 false = 现在不是被控状态（或没有会话），什么都没做。
    ///
    /// 这条路的用户是**键盘用户**：面板上点「收回键鼠」是他们够不到
    /// 鼠标、也没记住热键时唯一的出路。原来 `give_back()` 在 Receiving
    /// 直接 return，点了没反应。
    pub fn take_back_from_peer(&self, why: &str) -> bool {
        if self.state() != State::Receiving {
            return false;
        }
        // **先快照目标再动状态** —— active 一清，Takeback 就不知道发给
        // 谁了，对端会一直以为它还控制着本机。
        let Some(target) = self.active.get() else {
            return false;
        };
        self.state.store(State::Idle as u8, Ordering::SeqCst);
        if let Some(capture) = self.capture.as_ref() {
            capture.set_claim_armed(false);
        }
        // 单发 UDP 会丢：丢一包对端就卡在 Sending，键鼠全被它吞掉。
        // 接收端有状态闸门，重复的 Takeback 是幂等的。
        self.out.to_link(target, &Event::Takeback { along: 0xFFFF });
        self.out.to_link(target, &Event::Takeback { along: 0xFFFF });
        self.active.set(None);
        // 会话结束了，起点也一并忘了：下一场重新定
        self.root.set(None);
        (self.log)(format!("{why}，已请对端交还键鼠"));
        true
    }

    fn release(&self, reason: Option<&str>) -> bool {
        let now = self.state.load(Ordering::Relaxed);
        if now != State::Sending as u8 {
            return false;
        }
        // CAS 抢一下，避免两个线程同时交还、重复补 key-up
        if self
            .state
            .compare_exchange(now, State::Idle as u8, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return false;
        }
        // **先快照再发**：清空 active 之后补的 up 与 Leave 就找不到收件人
        // 了，对端会一直以为 Cmd/Ctrl 还压着（I4）。
        let target = self.active.get();
        if let Some(capture) = self.capture.as_ref() {
            capture.set_grabbing(false);
        }
        // 先把可能还按着的修饰键和按钮补 up，再发 Leave。
        // 不补的话对面会一直以为 Cmd/Ctrl 还压着，之后所有点击都变成组合键。
        if let Some(t) = target {
            for code in plat::MODIFIER_KEYS {
                self.out.to_link(
                    t,
                    &Event::Key {
                        code,
                        down: false,
                    },
                );
            }
            // 按钮不盲发 up（理由见 `Linker::retarget`）：对端会把孤立的
            // button-up 当成一次点击
            self.out.to_link(t, &Event::Leave);
        }
        // I2：拆场先改 state 再 clear active（上面 CAS 已改过 state）
        self.active.set(None);
        self.root.set(None);

        match reason {
            Some(why) => (self.log)(format!("键鼠已收回：{why}")),
            None => (self.log)("键鼠已收回本机".to_string()),
        }
        true
    }

    pub fn stop(&self) {
        let _ = self.release(None);
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
    if opts.links.is_empty() {
        return Err("没有配置任何对端链路".to_string());
    }
    let links = Arc::new(opts.links);
    let active = Active::new();
    // 会话起点：多跳的回程目标（见 Kvm::root）
    let root = Active::new();
    // 改指向的残包闸与节流
    let retarget_guard: Arc<Mutex<Option<(usize, u64)>>> = Arc::new(Mutex::new(None));
    let hop_ms = Arc::new(AtomicU64::new(0));
    // 四向掩码：只有 Edge 链路占边。Follow 链路靠画面切换触发，
    // 不产生"撞边缘"事件 —— 混进掩码的话，用户朝那侧推一下就会
    // 在画面没切走的情况下被吞掉键鼠。
    let sides_mask = link_mask(&links);
    let any_edge = links.iter().any(|l| l.mode == Mode::Edge);
    // 链路级判据，不是全局 opts.mode：v0.6.58 起模式下沉到每台对端，
    // 「全局 edge + 单条 follow 链路」是合法配置，这时本机照样要抓键鼠
    // 才能在切屏时把它送过去。用全局值会让那种配置下 follow() 直接报
    // 「抓取模块没有启动」。
    let any_follow = links.iter().any(|l| l.mode == Mode::Follow);
    if sides_mask == 0 && any_edge {
        return Err(
            "边缘跨界链路没有填方向：每条 edge 链路都要说清从本机哪一侧出去".to_string(),
        );
    }

    // 收事件要在固定端口上等着；发送用同一个 socket 发回去即可
    let socket = UdpSocket::bind(("0.0.0.0", port))
        .map_err(|e| format!("KVM 端口 {port} 绑定失败：{e}"))?;
    socket
        .set_read_timeout(Some(Duration::from_millis(200)))
        .map_err(|e| format!("设置超时失败：{e}"))?;

    // 两个角色要的权限不一样，别把 Console 的检查套到 Remote 头上：
    // Remote 只需要"能注入"，它根本没有键鼠可抓。
    // Edge 模式下两边都是对等的，两边都要能抓能注入。
    // 权限按**能力**分两块，不再按 role 一刀切（P5 控制台迁移的前提）：
    //
    //   can_post（注入）= 硬需求：没有它连"被控制"都做不到，直接报错
    //   trusted（抓取）= 软需求：没有它本机不能主动把键鼠推出去，但仍然
    //                    可以被对面控制 —— 降级，不罢工
    //
    // 这两项在 macOS 上确实是两个独立授权（AXIsProcessTrusted vs
    // CGPreflightPostEventAccess），所以"一个有一个没有"是真实状态。
    if !plat::can_post() {
        return Err(
            "缺少权限，没法把对端的键鼠注入本机（macOS：系统设置 → 隐私与安全性 → 辅助功能）。\
             **勾完必须重启本程序**：macOS 把 TCC 结论缓存在进程里，已经在跑的进程\
             即使你刚勾上也还是「没权限」，从托盘菜单点「重启应用」即可"
                .into(),
        );
    }
    let capture_ok = plat::trusted();
    if !capture_ok {
        (opts.log)(
            "⚠️ 缺少抓取权限：本机只能被对端控制，键鼠推不出去\
             （macOS：系统设置 → 隐私与安全性 → 辅助功能）——\
             如果刚勾上，从托盘菜单点「重启应用」让它生效"
                .to_string(),
        );
    }
    // Follow 模式两端都要能抓：键鼠插在哪台是不确定的（用户随时可能把
    // 键盘挪到另一台），所以不能用 role 决定谁装钩子。
    let needs_capture =
        capture_ok && (any_edge || any_follow || opts.role == Role::Console);

    let state = Arc::new(AtomicU8::new(State::Idle as u8));
    let stop = Arc::new(AtomicBool::new(false));
    // 每对端一份：最后一次收到包的时刻 + 它报的状态（0=Idle 1=Sending
    // 2=Receiving）。控制消息单发 UDP 会丢，丢一包两边状态机就错位卡死；
    // 心跳 0.8s 一次每次都带状态，任何错位 2.5 秒内必然被看见并自愈。
    let peer_rx = RxTable::new();
    // 本机最近一次"收回键鼠"的时间戳：收回后短暂冷却 Begin，防止用户的
    // 手还压在门口就立刻再次越界（日志实测 0.1s 内反复乒乓）
    let last_reclaim_ms = Arc::new(AtomicU64::new(0));
    // 最近一次进入**被控**（Receiving）的时刻：物理抢回要对它算保护窗。
    //
    // 为什么不放 `Session` 里：抢回信号是抓取线程收的，而 `Session` 归
    // 接收线程独占；两边共用一个原子量比把 Session 上锁便宜得多。
    let last_recv_ms = Arc::new(AtomicU64::new(0));

    let sealer = opts
        .key
        .map(|key| Sealer::new(&key, Sealer::fresh_prefix()));
    let encrypted = sealer.is_some();
    let out = Arc::new(Out {
        socket,
        port,
        links: links.clone(),
        active: active.clone(),
        sealer: Mutex::new(sealer),
        seq: Mutex::new(0),
    });

    // ---- 抓取方向 ----
    //
    // Follow 模式只有键鼠端需要抓；Edge 模式两边对等，都要抓。
    // `edge_trigger` 决定"没抓取时"要不要盯着边缘：只要有 Edge 链路就需要，
    // Follow 链路装了钩子但完全不判断位置（否则切屏后光标停在边缘就会误触发）。
    let mut capture: Option<Arc<plat::Capture>> = None;
    if needs_capture {
        let (tx, rx) = mpsc::channel::<Captured>();
        let cap = Arc::new(plat::start_capture(
            CaptureOpts {
                sides: sides_mask,
                edge_trigger: any_edge,
            },
            tx,
        )?);
        let state = state.clone();
        let stop = stop.clone();
        let out = out.clone();
        let capture_ref = cap.clone();
        let log = opts.log.clone();
        let on_panic = opts.on_panic.clone();
        let rx_cap = peer_rx.clone();
        let reclaim_cap = last_reclaim_ms.clone();
        let on_paste = opts.on_paste.clone();
        let links_cap = links.clone();
        let active_cap = active.clone();
        let root_cap = root.clone();
        let guard_cap = retarget_guard.clone();
        let hop_cap = hop_ms.clone();
        let recv_cap = last_recv_ms.clone();
        let push_hop = opts.push_hop;
        thread::spawn(move || {
            let mut pressed: Vec<Event> = Vec::new();
            // 会话中"推边改指向"的影子光标（见 EdgeSim）。只在 Follow 会话
            // 里喂它，换目标就复位。
            let mut edge_sim = EdgeSim::new();
            let mut sim_active: Option<usize> = None;
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
            // 上一次认"物理抢回"的时刻（去抖：连按两下只发一次 Takeback）
            let mut last_claim_ms: u64 = 0;

            // 被控期间把控制权要回来。三条入口动作完全一样：热键、
            // 真动物理键鼠、以及主循环那条"推门"（走 Begin/Receiving 分支，
            // 不经过这里，因为那边还要处理 Enter 与落点）。
            //
            // 必须**先快照目标再动状态**：active 一清，后面的 Takeback
            // 就不知道该发给谁了 —— 对端会一直以为它还控制着本机。
            let take_back_here = |why: &str| {
                let Some(t) = active_cap.get() else {
                    return;
                };
                state.store(State::Idle as u8, Ordering::SeqCst);
                capture_ref.set_claim_armed(false);
                // 单发 UDP 会丢：丢一包对端就卡在 Sending 连键鼠一起吞掉。
                // 接收端有状态闸门，重复的 Takeback 是幂等的。
                out.to_link(t, &Event::Takeback { along: 0xFFFF });
                out.to_link(t, &Event::Takeback { along: 0xFFFF });
                active_cap.set(None);
                root_cap.set(None);
                reclaim_cap.store(now_ms(), Ordering::SeqCst);
                (log)(format!("{why}，已请对端交还键鼠"));
            };

            // 改指向：撞边（Begin@Sending）与热键（Hop）共用同一份实现。
            // 三处入口各自复制一遍的下场是其中一处忘了补 key-up / 忘了堵
            // Takeback 残包，而症状永远是那句"改个方向键鼠全没了"。
            macro_rules! linker {
                () => {
                    Linker {
                        state: &state,
                        out: &out,
                        links: &links_cap[..],
                        active: &active_cap,
                        root: &root_cap,
                        capture: Some(&capture_ref),
                        guard: &guard_cap,
                        hop_ms: &hop_cap,
                        peer_rx: &rx_cap,
                        log: &*log,
                    }
                };
            }
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
                    Captured::Begin { along, side } => {
                        // 边缘越界：只在 Edge 模式发生。撞到了就按 follow 的流程
                        // 走一遍（同样要先确认对端活着，否则不敢吞键鼠）。
                        //
                        // 三条路：
                        //   Idle      → 按撞到的那条边查链路表，正常越界
                        //   Receiving → 对端正控制着本机，本地用户把光标推到门口
                        //               = 想把控制权抢回来（多屏共享两边各有键鼠，
                        //               这是唯一不靠热键的夺回方式）。**目标必须是
                        //               当前那台**：按 side 查表会变成"一边被 A
                        //               控制、一边往 C 转发"，两台同时注入
                        //   Sending   → 已经有一条会话在往外送：**改指向**（见
                        //               pick_retarget）。这条边查不到别的机器时
                        //               保持老行为（忽略 + 限流日志）——
                        //               两人单链路拓扑永远走这条，行为不变
                        //
                        // 刚收回的短暂冷却：收回时光标就放在门边 16pt，用户
                        // 的手多半还压着门口，不冷却的话 0.1 秒内就再次越界，
                        // 日志实测来回乒乓。
                        if now_ms().saturating_sub(reclaim_cap.load(Ordering::Relaxed)) < 600 {
                            continue;
                        }
                        let cur = state.load(Ordering::Relaxed);
                        if cur == State::Sending as u8 {
                            match pick_retarget(cur, active_cap.get(), &links_cap[..], side) {
                                Some(t) => {
                                    if let Err(err) = linker!().retarget(t, along, "推边改指向") {
                                        if now_ms().saturating_sub(last_begin_warn_ms) > 3000 {
                                            last_begin_warn_ms = now_ms();
                                            (log)(err);
                                        }
                                    }
                                }
                                None => {
                                    if now_ms().saturating_sub(last_begin_warn_ms) > 3000 {
                                        last_begin_warn_ms = now_ms();
                                        let node = active_cap
                                            .get()
                                            .and_then(|i| links_cap.get(i))
                                            .map(|l| l.node.as_str())
                                            .unwrap_or("对端");
                                        (log)(format!(
                                            "键鼠正交给 {node}，{}这条边没有可转的机器\
                                             （要交还按 Ctrl+Alt+Esc）",
                                            match side {
                                                Side::Left => "左",
                                                Side::Right => "右",
                                                Side::Top => "上",
                                                Side::Bottom => "下",
                                            }
                                        ));
                                    }
                                }
                            }
                            continue;
                        }
                        let target = if cur == State::Receiving as u8 {
                            match active_cap.get() {
                                Some(i) => Some(i),
                                None => continue,
                            }
                        } else {
                            // 掩码里有这条边 ⇒ 一定有链路负责它
                            links_cap
                                .iter()
                                .position(|l| l.mode == Mode::Edge && l.side == Some(side))
                        };
                        let Some(t) = target else {
                            // 掩码里没有这条边 = 平台层不该发来。有就是掩码漂了。
                            if now_ms().saturating_sub(last_begin_warn_ms) > 3000 {
                                last_begin_warn_ms = now_ms();
                                (log)(format!(
                                    "撞到了{}侧，但那条边没有配对机器，忽略",
                                    match side {
                                        Side::Left => "左",
                                        Side::Right => "右",
                                        Side::Top => "上",
                                        Side::Bottom => "下",
                                    }
                                ));
                            }
                            continue;
                        };
                        // 对端必须活着才敢吞键鼠：吞了又送不出去，要等 2.5 秒
                        // 断线看守放回来，用户的体感就是"卡一下键鼠全没了"。
                        // follow() 有这道检查，这里（边缘越界）同样要有。
                        let (idle_cap, seen_cap) = rx_cap.alive(links_cap[t].ip);
                        let alive = seen_cap && idle_cap <= PEER_ALIVE_MS;
                        if !alive {
                            if now_ms().saturating_sub(last_begin_warn_ms) > 3000 {
                                last_begin_warn_ms = now_ms();
                                (log)(format!(
                                    "光标到了边缘，{} 没有回应，不越界",
                                    links_cap[t].node
                                ));
                            }
                            continue;
                        }
                        // I2：先写会话目标，再改状态
                        active_cap.set(Some(t));
                        // 正常越界建的会话：起点就是它（多跳的回程目标）
                        root_cap.set(Some(t));
                        capture_ref.set_grabbing(true);
                        state.store(State::Sending as u8, Ordering::SeqCst);
                        // 把出门位置和本屏尺寸带给对端：它从对应高度进门，
                        // 并按两屏比例缩放位移（不然两块屏分辨率不同时，
                        // 一边的 delta 到另一边会被放大/缩小得不成样子）
                        let (sw, sh) = capture_ref.screen();
                        // Enter 双发：丢一包对端就收不到开工信号（好在
                        // Pong 状态自愈 2.5s 兜底，双发把卡顿窗口压到最小）
                        // 出门侧用**平台层报回来的那条边**，不是配置里那个：
                        // 掩码有多位时撞的到底是哪条边只有它能回答，而对端
                        // 靠这个字节把门口定到对面（v0.6.42 的拓扑定理）。
                        out.to_link(
                            t,
                            &Event::Enter {
                                width: sw.min(65535) as u16,
                                height: sh.min(65535) as u16,
                                along,
                                flags: 0,
                                side: side.code(),
                            },
                        );
                        out.to_link(
                            t,
                            &Event::Enter {
                                width: sw.min(65535) as u16,
                                height: sh.min(65535) as u16,
                                along,
                                flags: 0,
                                side: side.code(),
                            },
                        );
                        if cur == State::Idle as u8 {
                            (log)(format!(
                                "鼠标已越界到 {}，Ctrl+Alt+Esc 可以交还",
                                links_cap[t].node
                            ));
                        } else {
                            (log)(format!(
                                "光标推到门口，已从 {} 手里拿回控制权",
                                links_cap[t].node
                            ));
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
                            // ---- 会话里"推边改指向"（v0.6.60）----
                            //
                            // 只在**共用屏那台**上跑：它的画面和本机是同一块
                            // 屏，两块屏尺寸比例一致，本地复刻光标位置才有
                            // 意义。Edge 会话（两块屏并排）比例可能不同，
                            // 一律不跑 —— 那边推边本来就是"交还"的语义。
                            if push_hop && sides_mask != 0 {
                                let act = active_cap.get();
                                let follow = act
                                    .and_then(|i| links_cap.get(i))
                                    .map(|l| l.mode == Mode::Follow)
                                    .unwrap_or(false);
                                // 换过目标（含刚改指向）就复位：新的起点没有
                                // "上一趟推了多少"可言
                                if act != sim_active {
                                    sim_active = act;
                                    edge_sim.reset();
                                }
                                // 刚收回来的 600ms 里不判：收回时光标就摆在
                                // 门口，手的惯性还在，一判就再出去
                                let ready = now_ms()
                                    .saturating_sub(reclaim_cap.load(Ordering::Relaxed))
                                    >= 600;
                                if follow && ready {
                                    let (w, h) = capture_ref.screen();
                                    let hit = edge_sim.feed(
                                        dx,
                                        dy,
                                        w as f64,
                                        h as f64,
                                        sides_mask,
                                        now_ms(),
                                    );
                                    if let Some((side, along)) = hit {
                                        match pick_retarget(
                                            State::Sending as u8,
                                            act,
                                            &links_cap[..],
                                            side,
                                        ) {
                                            Some(t) => {
                                                if let Err(err) = linker!()
                                                    .retarget(t, along, "推边改指向")
                                                {
                                                    (log)(err);
                                                }
                                            }
                                            // 这条边没有别的机器：什么都不做，
                                            // 手继续推也只是把对端光标顶在边上
                                            None => {}
                                        }
                                    }
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
                    Captured::Claim => {
                        // 被控期间本机用户真的动了物理键鼠。
                        //
                        // 为什么这个信号必须由平台层给：注入来的事件在钩子
                        // 入口就带标记被放行了（Win 的 `INJECT_TAG` / Mac 的
                        // `FIELD_SOURCE_USER_DATA`），所以能走到这里的按下
                        // 一定是人按的。**不能**拿 Raw Input 的位移当判据 ——
                        // 那是设备增量流、不过钩子，对端注入的位移会被当成
                        // 本机用户在动，立刻 Takeback → 对端再注入 → 自激。
                        if state.load(Ordering::Relaxed) != State::Receiving as u8 {
                            continue;
                        }
                        let now = now_ms();
                        // 保护窗两头都要：进入被控后的 1.5 秒（KVM 切输入源
                        // 时显示器重新握手，键盘常吐一串 ghost key），以及
                        // 认过之后的 1.5 秒（连按两下不连发两次 Takeback）。
                        if now.saturating_sub(recv_cap.load(Ordering::Relaxed)) < CLAIM_ARM_MS
                            || now.saturating_sub(last_claim_ms) < CLAIM_ARM_MS
                        {
                            continue;
                        }
                        last_claim_ms = now;
                        take_back_here("本机被物理键鼠操作");
                    }
                    Captured::End => {
                        // 紧急收回：先把按着的键补 up，再告诉对端松手
                        let cur_end = state.load(Ordering::Relaxed);
                        if cur_end == State::Receiving as u8 {
                            // 被控时按热键也要能收回。原来这里只处理 Sending，
                            // 被控方按 Ctrl+Alt+Esc 悄无声息 —— 用户唯一的
                            // 出路是等对端松手，或者拔网线。
                            take_back_here("键鼠已收回（Ctrl+Alt+Esc）");
                        } else if cur_end == State::Sending as u8 {
                            // **先快照会话目标**：清空之后补的 up 全丢，
                            // 对端会一直以为某个键还按着。
                            let target = active_cap.get();
                            // 先改状态再放抓取：顺序反过来的话，心跳守卫
                            // 可能正好读到"状态还是 Sending、抓取已放开"
                            // 这个瞬时错位，把刚交还的光标又抓回来。
                            state.store(State::Idle as u8, Ordering::SeqCst);
                            capture_ref.set_grabbing(false);
                            if let Some(t) = target {
                                for ev in pressed.drain(..) {
                                    out.to_link(t, &up_of(&ev));
                                }
                                out.to_link(t, &Event::Leave);
                            }
                            active_cap.set(None);
                            root_cap.set(None);
                            (log)("键鼠已收回（Ctrl+Alt+Esc）".to_string());
                            // 屏幕还在对端的话，一起切回来
                            if let Some(cb) = on_panic.as_ref() {
                                cb();
                            }
                        }
                    }
                    Captured::Hop(side) => {
                        // 会话中按 Ctrl+Alt+方向：把键鼠改指向那一侧的机器。
                        //
                        // 三态表态：Idle 时不抢系统的「Ctrl+方向」（那是
                        // 切换桌面/翻页的常用键）；Receiving 时键鼠是别人的，
                        // 本机没权力把它转走；只有 Sending 才认。
                        let cur = state.load(Ordering::Relaxed);
                        if cur != State::Sending as u8 {
                            (log)(if cur == State::Receiving as u8 {
                                "键鼠在对面手里，这个热键不生效（按 Ctrl+Alt+Esc 先收回）"
                                    .to_string()
                            } else {
                                "Ctrl+Alt+方向 只在「键鼠已交给对端」时才生效".to_string()
                            });
                            continue;
                        }
                        // 按住不放会以键盘重复速率连发 keydown：没有这道闸
                        // 就是每 30 毫秒跳一次，最后一台完全不可预料
                        if now_ms().saturating_sub(hop_cap.load(Ordering::Relaxed))
                            < HOP_THROTTLE_MS
                        {
                            continue;
                        }
                        match pick_retarget(cur, active_cap.get(), &links_cap[..], side) {
                            Some(t) => {
                                if let Err(err) = linker!().retarget(t, 0xFFFF, "热键改指向") {
                                    (log)(err);
                                }
                            }
                            None => {
                                // 那一侧没有邻居 —— 但先分清"想再找一个邻居"和
                                // "想回家"：当前这台正是我从 `side` 的反面出门
                                // 才到的，用户朝着来路按，意思就是回去。
                                //
                                // 这一步是**唯一不依赖对端配合**的回程手段：
                                // 对端没升级、或它不判门（把自己那边当成共用
                                // 一块屏），推边交还就发不出来，人至少还能按
                                // 这个键回来。回起点而不是回本机，是因为画面
                                // 还在起点那台上（P275MV 显示的正是它）。
                                let homeward = is_homeward(
                                    active_cap
                                        .get()
                                        .and_then(|i| links_cap.get(i))
                                        .and_then(|l| l.side),
                                    side,
                                );
                                if !homeward {
                                    (log)(format!(
                                        "{}边没有可以转过去的机器（摆位图里给它拖个方向先）",
                                        match side {
                                            Side::Left => "左",
                                            Side::Right => "右",
                                            Side::Top => "上",
                                            Side::Bottom => "下",
                                        }
                                    ));
                                    continue;
                                }
                                match root_cap.get() {
                                    Some(r) => {
                                        if let Err(err) =
                                            linker!().retarget(r, 0xFFFF, "热键回起点")
                                        {
                                            (log)(format!("{err}；改回本机"));
                                            take_back_here("热键回家");
                                        }
                                    }
                                    // 这一场不是从别台跳过来的（空闲时直接越界
                                    // 出去），没有"起点"可言 —— 收回本机
                                    None => take_back_here("热键回家"),
                                }
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
        let peer_rx_rx = peer_rx.clone();
        let log = opts.log.clone();
        let key = opts.key;
        let capture_rx = capture.clone();
        let recv_rx = last_recv_ms.clone();
        let links_rx = links.clone();
        let active_rx = active.clone();
        let root_rx = root.clone();
        let guard_rx = retarget_guard.clone();
        let hop_rx = hop_ms.clone();

        let reclaim_rx = last_reclaim_ms.clone();
        thread::spawn(move || {
            // 门口方向可变：每当对端带着它自己的出门侧过来，就按拓扑改一次。
            // 两台屏水平相邻时，它从右边出去 ⇒ 我这边必然在它左边 —— 这是
            // 几何定理，不该让两台各自配、配反了就 1 秒弹回去。
            let inject = plat::Inject::new();
            if sides_mask != 0 {
                // 多屏共享：注入限制在"本机工作屏"内。工作屏不在这里定 ——
                // 物理用户可能换屏活动，每次收到 Enter（接管开始）时按
                // "物理用户最后活动的那块屏"重新锚定（见 win.rs LAST_WORK）。
                let layout = plat::monitor_layout();
                (log)(format!("显示器布局（x, y, 宽, 高）：{layout:?}"));
            }
            // **每个对端一份解密器**：重放水位是给谁用的，换人就该换一个。
            // （Sealer 则相反 —— 全局单实例、绝不重建，见方案 I9。）
            let mut openers: HashMap<IpAddr, Opener> = HashMap::new();
            // 一场会话的全部易脏状态打包在一起：换对端时整体重建，从结构
            // 上保证上一场的时钟、尺寸、按键栈、交还账本不会被带进下一场。
            let mut sess = Session::new(
                links_rx[0].ip,
                links_rx[0].node.clone(),
                links_rx[0].side.unwrap_or_default(),
                links_rx[0].mode == Mode::Edge,
            );
            // 上一次"越界被忽略"的告警时间（抢占矩阵里忽略第三方 Enter 时用）
            let mut last_ignore_ms: u64 = 0;
            // 上一次"陌生来源"的告警时间。同上面一样限流：不做的话一台没配
            // 链路的机器一秒能刷几十行。
            let mut last_stranger_ms: u64 = 0;
            let mut buf = [0u8; 64];
            while !stop.load(Ordering::Relaxed) {
                let (len, from) = match out.socket.recv_from(&mut buf) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                // 只认链路表里的地址：别台装过本程序的机器、扫描器、别的
                // 端口误撞过来的包，一律丢。5 台同网时这条是安全边界 ——
                // 密钥全机共用，光靠签名分不出是谁发的。
                //
                // **但丢了必须说出来**：三台互连时，A 推给 B 的前提不只是"B
                // 在线"，而是 **B 的链路表里也有 A**。少配一边的表现是
                // "推过去键鼠就没了" —— 光标卡在边上、画面也不切，日志里
                // 一片安静（这台只能看到自己把包丢掉了）。所以这里留一句话
                // 说明是谁发来的、该去哪补。
                let Some(from_link) = links_rx.iter().position(|l| l.ip == from.ip()) else {
                    if now_ms().saturating_sub(last_stranger_ms) > 5000 {
                        last_stranger_ms = now_ms();
                        (log)(format!(
                            "收到 {} 发来的键鼠包，但它不在这台的链路表里（已丢弃）。\
                             去那台的摆位图上把本机拖进某个方向，否则它的键鼠推过来会消失",
                            from.ip()
                        ));
                    }
                    continue;
                };
                let data = &buf[..len];
                let parsed = match key {
                    Some(k) => {
                        let op = openers.entry(from.ip()).or_insert_with(|| Opener::new(&k));
                        op.open(data)
                    }
                    None => Event::decode(data).map(|ev| {
                        let seq = u16::from_le_bytes([data[2], data[3]]);
                        (seq, ev)
                    }),
                };
                let Some((seq, ev)) = parsed else {
                    continue;
                };
                // 收到任何合法包都算"对端还活着"
                peer_rx_rx.touch(from.ip());

                match ev {
                    Event::Enter {
                        width,
                        height,
                        along,
                        side: peer_side,
                        flags,
                        ..
                    } => {
                        let cur = state.load(Ordering::Relaxed);
                        let act = active_rx.get();

                        // ---- 抢占矩阵（方案 §3 P4）----
                        // | 本机         | Enter 来自 active | Enter 来自其它链路 |
                        // | Idle         | 进 Receiving      | 同左              |
                        // | Sending→A    | 让路：转 Receiving | **忽略**          |
                        // | Receiving←A  | 幂等：只更新门口   | 先拆 A，再进 Receiving(B) |
                        if cur == State::Sending as u8 && act != Some(from_link) {
                            // 我们正在给 A 转发，B 也发来 Enter。接受的话 A 还在
                            // 收我们的键鼠、B 同时开始注入，两台一起动 —— 必须拒。
                            if now_ms().saturating_sub(last_ignore_ms) > 2000 {
                                last_ignore_ms = now_ms();
                                let held = act
                                    .and_then(|i| links_rx.get(i))
                                    .map(|l| l.node.as_str())
                                    .unwrap_or("别的机器");
                                (log)(format!(
                                    "{} 也想过来了，但键鼠已交给 {held}，这次忽略",
                                    links_rx[from_link].node
                                ));
                            }
                            continue;
                        }
                        if cur == State::Sending as u8 {
                            // 同一个对端在我们转发时又发 Enter：它的用户也在
                            // 推门抢。Edge 模式让路（换成它控制我们），Follow
                            // 模式两边同时越界抢，让先动的那个继续。
                            if links_rx[from_link].mode != Mode::Edge {
                                (log)("对端也想过来了，先让它等".to_string());
                                continue;
                            }
                            // 让路：停止抓取，把还按着的键补 up，然后进入
                            // **被控**状态接它的事件 —— 必须是 Receiving 而不是
                            // Idle：设成 Idle 的话，对端接下来转发的事件没人
                            // 注入，两边就都卡死了（v0.6.5 踩过）。
                            // 不比较先后 —— 双方同时推门时两边都让路，最坏
                            // 结果是这一轮谁也没拿到，再推一次即可。
                            //
                            // **顺序不能反**：先改状态再放抓取。反过来的话，
                            // 心跳守卫那一拍会读到"状态还是 Sending、抓取已
                            // 放开"，判成错位并反手再抓一次 —— 用户感知是
                            // 交还之后光标还卡半秒（v0.6.31 实测）。
                            state.store(State::Receiving as u8, Ordering::SeqCst);
                            // 被控开始：武装"物理抢回"并记时刻（保护窗起点）
                            recv_rx.store(now_ms(), Ordering::Relaxed);
                            if let Some(cap) = capture_rx.as_ref() {
                                cap.set_grabbing(false);
                                cap.set_claim_armed(true);
                            }
                            release_local(&inject, &mut sess.pressed);
                            out.to_link(from_link, &Event::Leave);
                        }
                        if cur == State::Receiving as u8 && act != Some(from_link) {
                            // 正被 A 控制着，B 也发 Enter（B 的用户推门抢）。
                            // 先把 A 拆干净：它的键鼠必须收回，否则我们一边
                            // 注入 B 的位移、一边还在收 A 的 —— 两台抢一只光标。
                            release_local(&inject, &mut sess.pressed);
                            if let Some(old) = act {
                                out.to_link(old, &Event::Takeback { along: 0xFFFF });
                                out.to_link(old, &Event::Takeback { along: 0xFFFF });
                                (log)(format!(
                                    "{} 抢走了控制权，已通知 {} 收回",
                                    links_rx[from_link].node,
                                    links_rx
                                        .get(old)
                                        .map(|l| l.node.as_str())
                                        .unwrap_or("对端")
                                ));
                            }
                        }

                        // 幂等判断要在改状态之前算：重复的 Enter 不该再挪光标
                        let entering = cur != State::Receiving as u8 || act != Some(from_link);
                        // 建场顺序（I2）：先写会话目标，再改状态
                        active_rx.set(Some(from_link));
                        state.store(State::Receiving as u8, Ordering::SeqCst);
                        // 被控开始（或换了对端）：武装"物理抢回"并记时刻。
                        // 只在真正进入时才刷新 —— 重复 Enter 不该把保护窗
                        // 一直往后推，否则用户按 Shift 永远抢不回来。
                        if entering {
                            recv_rx.store(now_ms(), Ordering::Relaxed);
                            if let Some(cap) = capture_rx.as_ref() {
                                cap.set_claim_armed(true);
                            }
                        }
                        // 换对端 = 整体重建，把上一场的时钟、尺寸、按键栈丢掉
                        if sess.peer != links_rx[from_link].ip {
                            sess = Session::new(
                                links_rx[from_link].ip,
                                links_rx[from_link].node.clone(),
                                links_rx[from_link].side.unwrap_or_default(),
                                links_rx[from_link].mode == Mode::Edge,
                            );
                        }
                        sess.last_seq = seq;
                        if width > 0 && height > 0 {
                            sess.sender_size = (width as f64, height as f64);
                        }
                        // 对端带了自己的出门侧：本机门口 = 它的对面。
                        // 这一条同时兜住「两台配置里门口方向各配一半」的情况
                        //（v0.6.42 实测：Mac→Windows 每 0.5~2 秒就被交还，
                        // 就是因为 Windows 那边把门口当成了自己出门的那侧）。
                        // 拓扑定理在这里也升级成四向版：它从右边出门 ⇒ 我在
                        // 它左边；它从上面出门 ⇒ 我在它下面。
                        // `side` 现在有两种含义，靠 flags 分清：
                        //   ① 真·出门侧 —— 这条链路有门；
                        //   ② 只用来定落点 —— 回程（从邻居回到共用屏那台）。
                        // ②必须排除在"有门证据"之外，否则那台共用屏机器
                        // 会开始判推门交还，光标在它上面一顶边就把键鼠
                        // 弹回 Mac（画面还在它那，用户等于摸黑）。
                        let landing = flags & ENTER_FLAG_LANDING_ONLY != 0;
                        if let Some(ps) = Side::from_code(peer_side) {
                            // **有没有门，发送方比本地配置更有发言权。**
                            //
                            // 只有 Edge 链路发得出真实的出门侧（Follow 恒发
                            // CODE_UNKNOWN，见 `follow()`）。所以"带了一个
                            // 有效 side"这件事本身就是"那条链路有门"的证据。
                            //
                            // 反过来按本地配置判会出一个死局：用户往往只在
                            // **发送方那台**摆过位（"把 0DFF1JU 拖到右侧"），
                            // 接收方自己那份还是默认的"共用一块屏" → 这里
                            // 判定 edge=false → 推门交还整段不跑 → 光标顶死
                            // 在门口也不交还，症状就是"回不去上一台"
                            //（v0.6.61 实测）。门是发送方拓扑的一部分，
                            // 不该要求接收方把同一件事再配一遍。
                            if !landing && session_edge(sess.edge, peer_side) && !sess.edge {
                                sess.edge = true;
                                (log)(format!(
                                    "{} 从 {} 出门 ⇒ 这条链路按「有门」处理（本机配置里它没占边，已按对端为准）",
                                    sess.node,
                                    ps.as_str()
                                ));
                            }
                            let want = ps.opposite();
                            if want != sess.portal {
                                sess.portal = want;
                                (log)(format!(
                                    "门口方向按对端校正：{} 从 {} 出门 → 本机门口改到 {}",
                                    sess.node,
                                    ps.as_str(),
                                    want.as_str()
                                ));
                            }
                        }
                        // 把"实际走的这道门"写给平台层：交还时要按它把光标摆回
                        // 门内侧。以前平台层读的是配置里那个 side，而门口方向
                        // 只在这里的局部变量里被 opposite() 校正过 —— 校正过的
                        // 门和配置的门相反时，交还就把光标摆到了对面那条边
                        //（被控方视角：键鼠回来了，光标却在屏幕另一头）。
                        if let Some(cap) = capture_rx.as_ref() {
                            cap.set_portal(sess.portal);
                        }
                        if entering {
                            sess.injected = 0;
                            sess.verify = None;
                            sess.suspect = 0;
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
                            //
                            // `landing` 是回程：共用屏那台没有门，但我们知道
                            // 用户是从哪一侧把光标推回来的，就摆到那侧边缘
                            // —— 否则它只能落在屏幕正中，看起来像"凭空闪现"。
                            if sess.edge || landing {
                                // 每次接管重新锚定工作屏：物理用户实际看着的那块
                                let own = inject.enable_own_screen(sess.portal);
                                let src = plat::work_screen_source();
                                (log)(format!("本机工作屏锚定：{own:?}（来源：{}，1=物理活动屏 5=窗口分布 2=主屏 3=光标屏 4=几何）", src));
                                let (ex, ey) = entry_spot(&inject, sess.portal, along);
                                inject.place_cursor(ex, ey);
                            } else {
                                inject.center();
                            }
                            sess.edge_armed = false;
                            sess.enter_ms = now_ms();
                            sess.edge_hold_ms = 0;
                            sess.motion_sample.clear();
                            // 把"这场会话有没有门"写进日志。**排障全靠这一行**：
                            // 「从这台推边推不回去」的第一个分叉就是它到底把
                            // 这条链路当成有门还是共用一块屏 —— 以前这只能靠
                            // 读代码推，用户贴不出证据来。
                            let kind = if sess.edge {
                                format!("有门，门口在{}", sess.portal.as_str())
                            } else if landing {
                                format!(
                                    "共用一块屏（不判门；回程从{}侧进门）",
                                    sess.portal.as_str()
                                )
                            } else {
                                "共用一块屏（不判门，交还只能靠对端叫回或热键）".to_string()
                            };
                            if cur == State::Sending as u8 {
                                (log)(format!(
                                    "{} 过来接管了，键鼠已让给它（{kind}）",
                                    sess.node
                                ));
                            } else {
                                (log)(format!(
                                    "{} 接管了键鼠（画面在你这边；{kind}）",
                                    sess.node
                                ));
                            }
                        }
                    }
                    Event::Leave => {
                        // 只认当前会话目标：正被 A 控制时收到 B 的 Leave
                        // （上一场的残包）不该动本机状态。
                        if active_rx.get() != Some(from_link) {
                            continue;
                        }
                        // 对端松手了，把可能残留的按下状态清掉
                        release_local(&inject, &mut sess.pressed);
                        match State::from_u8(state.load(Ordering::Relaxed)) {
                            State::Receiving => {
                                state.store(State::Idle as u8, Ordering::SeqCst);
                                // I2：拆场先改 state 再 clear active
                                active_rx.set(None);
                                // 事件计数是排障的关键证据：有数 = 传输通、注入环节有问题；
                                // 为 0 = Enter 之后一条事件都没到（传输问题）
                                if sess.injected > 0 {
                                    (log)(format!(
                                        "{} 已收回键鼠（本次注入 {} 笔事件）",
                                        sess.node, sess.injected
                                    ));
                                } else {
                                    (log)(format!(
                                        "{} 已收回键鼠（⚠️ 本次一条事件都没收到，传输断了）",
                                        sess.node
                                    ));
                                }
                                sess.injected = 0;
                            }
                            State::Sending => {
                                // 我们在转发时收到 Leave = 对端接受了我们的
                                // 抢占（它推门被我们按下去了，主动让位）。
                                // **保持 Sending** —— 真正的交还是 Takeback。
                                (log)(format!("{} 已让位，控制权在我们这边", sess.node));
                            }
                            State::Idle => {}
                        }
                    }
                    Event::Takeback { along } => {
                        // 接收方主动交还（它的用户把光标推回门口，或它发现
                        // 状态错乱）。必须真的把键鼠收回来：否则我们一直抓着
                        // 键鼠往没人接收的方向转发，两台机器的键鼠全卡死 ——
                        // 这就是"键鼠突然都不能动了"的根源（v0.6.5 踩过）。
                        //
                        // v0.6.60 起多了一种结局：**回程**。会话是从"共用屏
                        // 那台"跳过来的话，对面交还时不能收回本机 —— P275MV
                        // 上显示的还是起点那台的画面，键鼠回到 Mac 等于用户
                        // 看不见光标。这时改成"把会话改指向回起点"。
                        //
                        // **只认当前会话目标**：正在给 A 转发时收到 B 的
                        // Takeback（B 是上一场留下的残包）就收回键鼠，等于
                        // 被已结束的会话打断。
                        let now = now_ms();
                        let guard = guard_rx.lock().ok().and_then(|g| *g);
                        match takeback_action(
                            state.load(Ordering::Relaxed),
                            active_rx.get(),
                            from_link,
                            guard,
                            now,
                            root_rx.get(),
                        ) {
                            TakebackAction::Ignore => continue,
                            TakebackAction::ToRoot(to) => {
                                // 改指向回起点。失败（起点那台不在线、链路
                                // 表变了）就老实收回本机 —— 那种情况下用户
                                // 至少能用 Mac 自己的键盘把人叫回来。
                                let back = Linker {
                                    state: &state,
                                    out: &out,
                                    links: &links_rx[..],
                                    active: &active_rx,
                                    root: &root_rx,
                                    capture: capture_rx.as_ref(),
                                    guard: &guard_rx,
                                    hop_ms: &hop_rx,
                                    peer_rx: &peer_rx_rx,
                                    log: &*log,
                                }
                                // 把对面交还时的光标位置一路带回去：起点那台
                                // 按这个比例进门，光标就落在用户推出去的那个
                                // 高度上，而不是屏幕正中。
                                .retarget(to, along, "对面交还，回到起点");
                                match back {
                                    Ok(()) => continue,
                                    Err(err) => {
                                        (log)(format!("{err}；改回本机"));
                                    }
                                }
                            }
                            TakebackAction::ToIdle => {}
                        }
                        if state.load(Ordering::Relaxed) == State::Sending as u8 {
                            // 先改状态再放抓取：心跳守卫每秒查一次不变式，
                            // 顺序反了就会撞见"还在 Sending、抓取已放开"
                            // 的瞬时错位，反手再抓一次 —— 用户感知是交还
                            // 之后光标还卡半秒（实测日志有这串）。
                            state.store(State::Idle as u8, Ordering::SeqCst);
                            if let Some(cap) = capture_rx.as_ref() {
                                cap.set_grabbing(false);
                            }
                            // I2：拆场先改 state 再 clear active
                            active_rx.set(None);
                            root_rx.set(None);
                            reclaim_rx.store(now_ms(), Ordering::SeqCst);
                            (log)(format!(
                                // **必须用 from_link**，不能用 `sess.node`：那个
                                // 是启动时按 `peer_node` 建好的默认会话，只要
                                // 本机没收到过 Enter 就一直是同一个名字。用它
                                // 的结果是 0DFF1JU 交还也写成"MC5K9SJ 把控制权
                                // 还回来了"（v0.6.62 排查时被这条日志带偏：
                                // 明明会话目标是 0DFF1JU，日志却咬定 MC5K9SJ）。
                                "{} 把控制权还回来了，键鼠已收回本机",
                                links_rx[from_link].node
                            ));
                        }
                    }
                    // 心跳：last_rx 上面已经刷新了，Pong 里带上自己的状态 ——
                    // 对端拿它做"状态错位自愈"的依据（见心跳线程）。
                    // **必须回给来源**，不能走"当前会话目标"：多对端下 4 台
                    // 都会发 Ping，回错人等于让对端 2.5 秒后判它不理我。
                    // 状态也要按对端投影，理由见 state_for。
                    Event::Ping => {
                        let st = state_for(&state, active_rx.get(), from_link);
                        out.to(from, &Event::Pong { state: st });
                    }
                    Event::Pong { state: peer } => {
                        peer_rx_rx.set_state(from.ip(), peer);
                    }
                    _ => {
                        // 只有真在被控制时才注入。否则对端明明已经松手，
                        // 我们还在把它的事件打进系统 —— 注入的事件和本机
                        // 真实的键鼠打架，看起来就是"鼠标乱窜、点不了东西"。
                        // 而且必须是**当前会话那台**：被 A 控制时收到 B 的
                        // 事件（B 还在 Sending，它的 Takeback 丢了）一律不注入。
                        if state.load(Ordering::Relaxed) != State::Receiving as u8
                            || active_rx.get() != Some(from_link)
                        {
                            // 对端还在发输入事件 = 它以为它控制着我们
                            // （Enter 丢了，或它那边卡在 Sending）。回一条
                            // Takeback 让它收回；限流，别被每秒几百笔的
                            // 位移事件刷爆。
                            if now_ms().saturating_sub(sess.last_takeback_ms) > 1000 {
                                sess.last_takeback_ms = now_ms();
                                // 救援也要回给来源：把 Takeback 发给会话目标，
                                // 真正该收回的那台就永远收不到
                                out.to(from, &Event::Takeback { along: 0xFFFF });
                                out.to(from, &Event::Takeback { along: 0xFFFF });
                                (log)(format!(
                                    "没在被控状态却收到 {} 的输入事件，已通知它收回",
                                    links_rx[from_link].node
                                ));
                            }
                            continue;
                        }
                        // 乱序的旧包丢掉 —— **只对位移生效**：按键（尤其
                        // up）被丢的代价是本机卡键/卡住左键，之后点什么
                        // 都没反应，比重复一笔点击严重得多。高频流下 UDP
                        // 偶发乱序，Button-up 被吞是实测踩过的坑。
                        if let Event::Motion { .. } = ev {
                            if seq.wrapping_sub(sess.last_seq) > 0x8000 && seq != 0 {
                                continue;
                            }
                            sess.last_seq = seq;
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
                                if sess.sender_size.0 >= 1.0 && sess.sender_size.1 >= 1.0 =>
                            {
                                let (lw, lh) = inject.screen();
                                Event::Motion {
                                    dx: rescale_delta(*dx, lw / sess.sender_size.0),
                                    dy: rescale_delta(*dy, lh / sess.sender_size.1),
                                }
                            }
                            _ => ev.clone(),
                        };
                        inject.apply(&to_inject);
                        sess.injected += 1;
                        if sess.injected == 1 {
                            // 把首笔事件的具体数值打进日志：位移是 0、键码是
                            // 乱码这类「内容坏掉」的问题，只有这里能看出来
                            (log)(format!("事件流开始注入（首笔：{to_inject:?}）"));
                        }
                        // 进门后前 30 笔位移流采样：Windows 日志实锤光标被
                        // 按死在门口 x=0 顶住 300ms，必须分辨是"位移流里
                        // 持续来负数"还是"外力直接挪真实光标"
                        if let Event::Motion { dx, dy } = to_inject {
                            if sess.injected <= 30 {
                                if sess.injected == 1 {
                                    sess.motion_sample.clear();
                                }
                                sess.motion_sample.push_str(&format!(" {dx},{dy}"));
                                if sess.injected == 30 {
                                    (log)(format!(
                                        "进门位移流（前 30 笔 dx,dy）：{}", sess.motion_sample
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
                        if sess.edge {
                            if let Event::Motion { .. } = ev {
                                // (是否判失效, 真实光标移动, 账本移动)
                                let mut verdict: Option<(bool, f64, f64)> = None;
                                if sess.verify.is_none() {
                                    let p = inject.cursor().unwrap_or((0.0, 0.0));
                                    sess.verify = Some((now_ms(), p, inject.walked(), 1));
                                } else if let Some(slot) = sess.verify.as_mut() {
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
                                        sess.verify = None;
                                        if sess.suspect >= 1 {
                                            release_local(&inject, &mut sess.pressed);
                                            out.to_link(from_link, &Event::Takeback { along: 0xFFFF });
                                            out.to_link(from_link, &Event::Takeback { along: 0xFFFF });
                                            state.store(State::Idle as u8, Ordering::SeqCst);
                                            active_rx.set(None);
                                            reclaim_rx.store(now_ms(), Ordering::SeqCst);
                                            (log)(format!(
                                                "⚠️ 连续两窗复检注入均失效（账本走了 \
                                                 {ledger_moved:.0} 点、真实光标只动 \
                                                 {real_moved:.1} 点），已通知 {} 收回键鼠",
                                                sess.node
                                            ));
                                            continue;
                                        }
                                        sess.suspect += 1;
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
                                        sess.verify = None;
                                        sess.suspect = 0;
                                    }
                                    None => {}
                                }
                            }
                        }
                        if ev.is_down() {
                            sess.pressed.push(ev);
                        } else {
                            sess.pressed.retain(|p| !same_key(p, &ev));
                        }
                        // Edge 模式：对端光标撞回"朝着对端"的那条边（本机
                        // 门口那一侧），说明用户想把控制权还回去了 ——
                        // 必须回 Takeback 让对端收回键鼠，否则它会一直抓着
                        // 键鼠往这边转发，两边全卡死。以前有两个 bug：判的
                        // 是 opposite（用户得横穿整个屏幕到最远端才触发），
                        // 而且触发后只是自己悄悄 Idle、对端毫不知情。
                        // Follow 模式不判断这个 —— 那边没有"边缘"概念，
                        // 交还是靠画面切回来或热键。
                        if sess.edge {
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
                                    let inside = match sess.portal {
                                        Side::Left => x,
                                        Side::Right => w - x,
                                        Side::Top => y,
                                        Side::Bottom => h - y,
                                    };
                                    if !sess.edge_armed {
                                        if inside > EDGE_REARM_DIST * scale {
                                            sess.edge_armed = true;
                                        }
                                        sess.edge_hold_ms = 0;
                                        sess.out_push = 0;
                                    } else if event::hit_edge(sess.portal, x, y, w, h) {
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
                                        let outward = match sess.portal {
                                            Side::Left => -dx,
                                            Side::Right => dx,
                                            Side::Top => -dy,
                                            Side::Bottom => dy,
                                        };
                                        if outward > 0 {
                                            sess.out_push += outward;
                                        }
                                        let now = now_ms();
                                        if sess.edge_hold_ms == 0 {
                                            sess.edge_hold_ms = now;
                                        }
                                        if sess.out_push as f64 >= RETURN_PUSH * scale
                                            // 进门 0.5 秒内不算：手还在门口惯性里
                                            && now.saturating_sub(sess.enter_ms) >= 500
                                            // 交还后 2 秒内不重复交还：打断
                                            // 门口弹跳循环（见变量注释）
                                            && now.saturating_sub(sess.last_giveback_ms) >= 2000
                                        {
                                            let held = now.saturating_sub(sess.edge_hold_ms);
                                            let pushed = sess.out_push;
                                            sess.edge_hold_ms = 0;
                                            sess.out_push = 0;
                                            sess.last_giveback_ms = now;
                                            release_local(&inject, &mut sess.pressed);
                                            // 单发 UDP 会丢：丢一包对端就卡在 Sending
                                            // 连发两笔，接收端有状态闸门，幂等。
                                            // **显式寻址到这台**：交还是还给
                                            // 正被释放的那条链路（I4）。
                                            out.to_link(from_link, &Event::Takeback { along: sess.portal.along(x, y, w, h) });
                                            out.to_link(from_link, &Event::Takeback { along: sess.portal.along(x, y, w, h) });
                                            state.store(State::Idle as u8, Ordering::SeqCst);
                                            // I2：拆场先改 state 再 clear active
                                            active_rx.set(None);
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
                                                now.saturating_sub(sess.enter_ms) as f64 / 1000.0,
                                            ));
                                        }
                                    } else {
                                        // 离开门边：累计位移与计时都清零 —— 攒的是
                                        // "这一顶"的量，不是历史总量
                                        sess.edge_hold_ms = 0;
                                        sess.out_push = 0;
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
        let peer_rx_gd = peer_rx.clone();
        let log = opts.log.clone();
        let capture = capture.clone();
        let reclaim_gd = last_reclaim_ms.clone();
        let links_gd = links.clone();
        let active_gd = active.clone();
        let root_gd = root.clone();
        // 链路表那张掩码一起带走：被控期间平台层会把它收窄成"抢回那条边"，
        // 会话结束后得铺回来（见下面的掩码恢复）
        let mask_gd = sides_mask;
        thread::spawn(move || {
            let mut warned = false;
            // 状态错位自愈的跟踪：进入当前状态的时间、对端最近一次报告
            // "配对状态"（我在 Sending 它在 Receiving，或反之）的时间
            let mut prev_me: u8 = State::Idle as u8;
            let mut entered_ms: u64 = now_ms();
            let mut peer_paired_ms: u64 = now_ms();
            // 上一轮的会话目标：换了人就要重置"错位/静音"的计时基准，
            // 否则新会话会继承旧会话的表态窗口
            let mut prev_act: Option<usize> = None;
            // 对端旧版本（状态未知）只提示一次，别刷屏
            let mut degraded_logged = false;
            // "会话目标丢了"的连续拍数（见下方的两拍确认）
            let mut unlinked_ticks: u32 = 0;
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(PING_INTERVAL);
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                // 每条链路都发：≤4 包 / 800ms 的代价换来"每台都一直知道
                // 我在不在线"，才能做到从任意一条边推过去都不必先探测。
                for i in 0..links_gd.len() {
                    out.to_link(i, &Event::Ping);
                }

                let me = state.load(Ordering::Relaxed);
                let act = active_gd.get();
                if me != prev_me {
                    prev_me = me;
                    entered_ms = now_ms();
                    // 乐观起点：刚进 Sending/Receiving 时对端可能还没收到
                    // Enter/还没换状态回 Pong，给它 2.5 秒表态时间
                    peer_paired_ms = now_ms();
                    warned = false;
                }
                // 会话目标被清掉却还停在 Sending/Receiving = 不变式破了。
                // 不纠正的话转发流找不到收件人（位移静默丢弃、键鼠全吞）。
                //
                // **必须连续两拍都成立才动手**：抢回与本机重新越界可能在同一
                // 毫秒里交错（Takeback 线程清 active，Begin 线程刚把它写回去），
                // 单拍看到的就是假阳性 —— 实测日志里"把控制权还回来了"和
                // "会话目标丢了"出现在同一秒，跟着把刚建立的会话一起拆掉。
                if me != State::Idle as u8 && act.is_none() && me == prev_me {
                    unlinked_ticks += 1;
                    if unlinked_ticks >= 2 {
                        unlinked_ticks = 0;
                        state.store(State::Idle as u8, Ordering::SeqCst);
                        if let Some(c) = capture.as_ref() {
                            c.set_grabbing(false);
                        }
                        reclaim_gd.store(now_ms(), Ordering::SeqCst);
                        (log)("会话目标丢了（状态机错位），已回到空闲".to_string());
                    }
                    continue;
                }
                unlinked_ticks = 0;

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

                // ---- 掩码恢复 ----
                // 收到 Enter 时平台层把掩码收窄成"对端所在的那条边"，为的是
                // 让被控期间「朝门口推 = 抢回」的方向跟着校正过的门走。
                // 但那张掩码是共享的：不铺回来，交还之后**其余几条边就永久
                // 推不出去了** —— 单链路时收的正好是唯一那条边，看不出来；
                // 一机配多台时表现为"去过一次 A，之后从 B 那条边推没反应"。
                //
                // 放在心跳里做而不是每个退出路径各写一遍：退出路径有六条
                //（Leave / Takeback / 推门交还 / 热键 / 错位自愈 / 静音看守），
                // 漏一条就是一个只在多机下复现的幽灵 bug。
                // Receiving 期间不动（那正是抢回要用的收窄掩码）；Sending
                // 期间铺开无害 —— 抓取着的时候钩子根本不判边缘。
                if let Some(c) = capture.as_ref() {
                    let recv = state.load(Ordering::Relaxed) == State::Receiving as u8;
                    // 抢回的"武装"跟掩码同步：只有被控期间才该把本机的物理
                    // 按键当成"我要收回"。进入时接收线程已经置过一次，这里是
                    // 兜底（状态从任何路径滑出 Receiving 都一定被关掉）。
                    c.set_claim_armed(recv);
                    if !recv {
                        c.set_mask(mask_gd);
                    }
                }

                // ---- 状态错位自愈 ----
                // 控制消息（Enter/Takeback/Leave）是单发 UDP，丢一包两边
                // 状态机就各持一半真相：我们卡在 Sending 吞着键鼠，对端已经
                // Idle —— 心跳照常走，下面的"完全静音"看守永远不触发
                //（实测：来回多切几次必中一次，只能热键救）。
                // Pong 现在每 0.8s 带一次对端状态，任何错位 2.5 秒内可见。
                //
                // **只看当前会话那台**（I8）：把别台的 Pong 拿来比对，
                // 空闲链路的 Idle 会被当成本机 Sending 的错位，2.5 秒后
                // 把正在进行的会话踢掉 —— 多对端下必现。
                // 状态可能在上面的抓取守卫里被改过，这里重新取。
                let me = state.load(Ordering::Relaxed);
                let act = active_gd.get();
                if act != prev_act {
                    prev_act = act;
                    peer_paired_ms = now_ms();
                    entered_ms = now_ms();
                }
                let peer = match act.and_then(|i| links_gd.get(i)) {
                    Some(l) => peer_rx_gd.state(l.ip),
                    None => State::Idle as u8,
                };
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
                    let node = act
                        .and_then(|i| links_gd.get(i))
                        .map(|l| l.node.clone())
                        .unwrap_or_else(|| "对端".to_string());
                    if me == State::Sending as u8 {
                        if let Some(capture) = capture.as_ref() {
                            capture.set_grabbing(false);
                        }
                        if let Some(t) = act {
                            for code in plat::MODIFIER_KEYS {
                                out.to_link(t, &Event::Key { code, down: false });
                            }
                            // 按钮不盲发 up（理由见 `Linker::retarget`）
                            out.to_link(t, &Event::Leave);
                        }
                        state.store(State::Idle as u8, Ordering::SeqCst);
                        active_gd.set(None);
                        root_gd.set(None);
                        reclaim_gd.store(now_ms(), Ordering::SeqCst);
                        (log)(format!(
                            "键鼠交给 {node} 后状态对不上（交还消息丢了？），已自动收回"
                        ));
                    } else {
                        state.store(State::Idle as u8, Ordering::SeqCst);
                        active_gd.set(None);
                        root_gd.set(None);
                        reclaim_gd.store(now_ms(), Ordering::SeqCst);
                        (log)(format!("{node} 已不在发送状态，本机退出被控"));
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
                //
                // **只看当前会话那台的 last_rx**（I7）：任何一台邻居的心跳
                // 都能把全局 last_rx 刷新，那样正在转发的那台崩了本机永远
                // 不会发现，键鼠就被永久吞掉。
                if me == State::Idle as u8 {
                    warned = false;
                    continue;
                }
                let Some(act) = act else {
                    // 上面那个不变式检查每轮都会兜住，这里只是不让它落空
                    continue;
                };
                let Some(held) = links_gd.get(act) else {
                    continue;
                };
                if peer_rx_gd.idle_ms(held.ip) > IDLE_TIMEOUT_MS && !warned {
                    warned = true;
                    if me == State::Sending as u8 {
                        if let Some(capture) = capture.as_ref() {
                            capture.set_grabbing(false);
                        }
                        for code in plat::MODIFIER_KEYS {
                            out.to_link(
                                act,
                                &Event::Key {
                                    code,
                                    down: false,
                                },
                            );
                        }
                        // 按钮不盲发 up（理由见 `Linker::retarget`）
                        out.to_link(act, &Event::Leave);
                    }
                    state.store(State::Idle as u8, Ordering::SeqCst);
                    active_gd.set(None);
                    root_gd.set(None);
                    reclaim_gd.store(now_ms(), Ordering::SeqCst);
                    (log)(format!(
                        "{} 2.5 秒没有回应（{}），已自动回到空闲",
                        held.node,
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
    // 每条链路一行说清楚"从哪侧出去能到谁"。多链路时这句话就是用户
    // 排障时唯一能对照拓扑的凭据 —— 方向配反了，日志里一眼能看出来。
    let routes: Vec<String> = links
        .iter()
        .map(|l| match l.side {
            Some(side) if l.mode == Mode::Edge => format!(
                "{}侧→{}",
                match side {
                    Side::Right => "右",
                    Side::Left => "左",
                    Side::Top => "上",
                    Side::Bottom => "下",
                },
                l.node
            ),
            _ => format!("随画面→{}", l.node),
        })
        .collect();
    // 文案按**链路表**说，不按全局 opts.mode：v0.6.58 起一台机器可以
    // 同时挂"共用一块屏"和"旁边并排"两种链路，全局值只能说出其中一半，
    // 用户照着日志排查时会被引到错的方向。
    let headline = match (any_follow, any_edge) {
        (true, true) => "混合模式：共用屏那台跟画面走，并排那台推到边才过去",
        (true, false) => match opts.role {
            Role::Console => "跟随画面模式，本机是键鼠端 —— 画面切到对端时自动跟过去",
            Role::Remote => "跟随画面模式，本机不抓键鼠，等对端接管",
        },
        (false, true) => "多屏共享模式：光标推到边缘越界",
        (false, false) => "还没有配置任何对端链路",
    };
    (opts.log)(format!(
        "共用键鼠已就绪（{secret}）· {headline}（{}）",
        routes.join("，")
    ));

    Ok(Kvm {
        state,
        stop,
        mode: opts.mode,
        role: opts.role,
        capture,
        out,
        peer_rx,
        links,
        active,
        root,
        retarget_guard,
        hop_ms,
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

    fn link(ip: &str, side: Option<Side>, mode: Mode) -> Link {
        Link {
            node: ip.to_string(),
            ip: ip.parse().unwrap(),
            side,
            mode,
        }
    }

    /// 掩码只来自 Edge 链路：跟随画面那条不能占边。
    #[test]
    fn mask_skips_follow_links() {
        let l = vec![
            link("10.0.0.1", Some(Side::Right), Mode::Edge),
            link("10.0.0.2", Some(Side::Left), Mode::Follow),
        ];
        assert_eq!(link_mask(&l), Side::Right.bit());
    }

    /// 四向齐全 = 0b1111。
    #[test]
    fn mask_covers_four_sides() {
        let l: Vec<Link> = Side::all()
            .iter()
            .enumerate()
            .map(|(i, s)| link(&format!("10.0.0.{}", i + 1), Some(*s), Mode::Edge))
            .collect();
        assert_eq!(link_mask(&l), 0b1111);
        for s in Side::all() {
            assert_eq!(Side::from_bit(link_mask(&l) & s.bit()), Some(s));
        }
    }

    /// 同一台机器配了同一个方向两次：掩码不会多出一位，也不会消失。
    #[test]
    fn mask_dedupes_same_side() {
        let l = vec![
            link("10.0.0.1", Some(Side::Bottom), Mode::Edge),
            link("10.0.0.2", Some(Side::Bottom), Mode::Edge),
        ];
        assert_eq!(link_mask(&l), Side::Bottom.bit());
    }

    /// 会话目标之外的链路必须被告知"我空闲"，否则它会 2.5 秒后误判错位。
    #[test]
    fn state_is_projected_per_peer() {
        let st = AtomicU8::new(State::Sending as u8);
        assert_eq!(state_for(&st, Some(0), 0), State::Sending as u8);
        assert_eq!(state_for(&st, Some(0), 1), State::Idle as u8);
        let st = AtomicU8::new(State::Receiving as u8);
        assert_eq!(state_for(&st, None, 0), State::Idle as u8);
    }

    /// 会话目标的下标编码：0 = 没有会话，其余 = 下标 + 1。
    #[test]
    fn active_index_roundtrip() {
        let a = Active::new();
        assert_eq!(a.get(), None);
        for i in 0..4 {
            a.set(Some(i));
            assert_eq!(a.get(), Some(i));
        }
        a.set(None);
        assert_eq!(a.get(), None);
    }

    /// 影子光标：一路往右推，累计够了才认"到门口了"，方向必须是右边。
    #[test]
    fn edge_sim_needs_accumulated_push() {
        let mut sim = EdgeSim::new();
        let mask = Side::Right.bit();
        let mut hit = None;
        for _ in 0..400 {
            hit = sim.feed(8, 0, 1920.0, 1080.0, mask, 10_000);
            if hit.is_some() {
                break;
            }
        }
        let (side, _along) = hit.expect("推到边上应该触发一次");
        assert_eq!(side, Side::Right);
    }

    /// 掩码里没有这条边 / 刚触发完还在门口 = 都不触发（防连跳）。
    #[test]
    fn edge_sim_respects_mask_and_throttle() {
        // 右边没有邻居：推多久都不动
        let mut sim = EdgeSim::new();
        for _ in 0..400 {
            assert!(sim.feed(8, 0, 1920.0, 1080.0, Side::Left.bit(), 10_000).is_none());
        }
        // 触发一次后手还在门口继续推（同一毫秒）= 不再触发，
        // 必须先离开门边重新武装
        let mut sim = EdgeSim::new();
        let mask = Side::Right.bit();
        let mut fired = 0;
        for i in 0..600 {
            if sim.feed(8, 0, 1920.0, 1080.0, mask, 10_000 + i).is_some() {
                fired += 1;
            }
        }
        assert_eq!(fired, 1, "一趟顶门只该跳一次");
    }

    /// 三条链路的真实拓扑：0 = 共用屏（follow）、1 = 右侧并排（edge）。
    fn three_way() -> Vec<Link> {
        vec![
            link("10.0.0.1", None, Mode::Follow),
            link("10.0.0.2", Some(Side::Right), Mode::Edge),
            link("10.0.0.3", Some(Side::Left), Mode::Edge),
        ]
    }

    /// 撞到的那条边通向的就是当前那台：不改指向（两人单链路的回归判据）。
    #[test]
    fn pick_retarget_skips_self() {
        let l = vec![
            link("10.0.0.1", Some(Side::Right), Mode::Edge),
            link("10.0.0.2", Some(Side::Left), Mode::Edge),
        ];
        let sending = State::Sending as u8;
        assert_eq!(pick_retarget(sending, Some(0), &l, Side::Right), None);
        assert_eq!(pick_retarget(sending, Some(1), &l, Side::Left), None);
        assert_eq!(pick_retarget(sending, Some(0), &l, Side::Left), Some(1));
    }

    /// 共用屏那台（follow）上撞右边缘 → 转去右侧那台。
    #[test]
    fn pick_retarget_from_follow_link() {
        let l = three_way();
        assert_eq!(
            pick_retarget(State::Sending as u8, Some(0), &l, Side::Right),
            Some(1)
        );
        // 那条边没有机器：不动作
        assert_eq!(
            pick_retarget(State::Sending as u8, Some(0), &l, Side::Bottom),
            None
        );
    }

    /// 只有"键鼠正在外面"才有改指向可言：空闲是正常越界、被控是抢回。
    #[test]
    fn pick_retarget_only_when_sending() {
        let l = three_way();
        for cur in [State::Idle as u8, State::Receiving as u8] {
            assert_eq!(pick_retarget(cur, Some(0), &l, Side::Right), None);
        }
    }

    /// 回程的门：回到 Edge 链路按它自己的 side；回到共用屏那台没有门。
    #[test]
    fn enter_aim_says_where_the_cursor_should_land() {
        let l = three_way();
        // 前进：目标是 Edge 链路，就用它自己那条边
        assert_eq!(enter_aim(&l, 0, 1), (Side::Right.code(), 0));
        assert_eq!(enter_aim(&l, 0, 2), (Side::Left.code(), 0));
        // 回程：从右边那台回到共用屏那台 → 落点得在**右边**。这里给的是
        // 反面（Left），因为接收端会把它取反成自己的门口 —— 给正面光标
        // 就跑到屏幕另一头去了。同时必须带 landing 标志，否则对端会把
        // 这次回程当成"这条链路有门"。
        assert_eq!(
            enter_aim(&l, 1, 0),
            (Side::Left.code(), ENTER_FLAG_LANDING_ONLY)
        );
        // 来路自己也没有门（两台共用屏互跳）：没有方向可依据，落点居中
        assert_eq!(enter_aim(&l, 0, 9), (Side::CODE_UNKNOWN, 0));
    }

    /// 对面交还：起点不是它 → 改指向回起点；起点就是它 → 常规收回。
    #[test]
    fn takeback_returns_to_root() {
        let sending = State::Sending as u8;
        let now = 10_000;
        assert!(matches!(
            takeback_action(sending, Some(1), 1, None, now, Some(0)),
            TakebackAction::ToRoot(0)
        ));
        assert!(matches!(
            takeback_action(sending, Some(1), 1, None, now, Some(1)),
            TakebackAction::ToIdle
        ));
        assert!(matches!(
            takeback_action(sending, Some(1), 1, None, now, None),
            TakebackAction::ToIdle
        ));
        // 不是当前目标：一律不认
        assert!(matches!(
            takeback_action(sending, Some(0), 1, None, now, Some(0)),
            TakebackAction::Ignore
        ));
    }

    /// 刚改指向那一秒内，旧目标回来的 Takeback 是残包，不当真。
    #[test]
    fn takeback_guard_is_short_lived() {
        let sending = State::Sending as u8;
        let now = 10_000;
        let guard = Some((1usize, now - 200));
        assert!(matches!(
            takeback_action(sending, Some(1), 1, guard, now, Some(0)),
            TakebackAction::Ignore
        ));
        // 过了窗口就恢复正常
        let old = Some((1usize, now - RETARGET_GUARD_MS - 1));
        assert!(matches!(
            takeback_action(sending, Some(1), 1, old, now, Some(0)),
            TakebackAction::ToRoot(0)
        ));
        // 闸只对着登记的那台：别的机器不受影响
        let other = Some((0usize, now));
        assert!(matches!(
            takeback_action(sending, Some(1), 1, other, now, Some(0)),
            TakebackAction::ToRoot(0)
        ));
    }

    /// 对端的 `Enter` 带了出门侧 ⇒ 这条链路一定有门，本地配错了也算有。
    ///
    /// 这是 v0.6.61 那个"回不去上一台"的修复点：用户只在发送方那台摆过位，
    /// 接收方自己那份还是"共用一块屏"，按本地判会让推门交还整段不跑。
    #[test]
    fn peer_side_proves_there_is_a_door() {
        // 本地说没有门，但对端从右边出门 ⇒ 有门，且门口在左
        assert!(session_edge(false, Side::Right.code()));
        // 对端没带（Follow 恒发 UNKNOWN）⇒ 只能听本地的
        assert!(!session_edge(false, Side::CODE_UNKNOWN));
        // 本地本来就有门：带不带都算
        assert!(session_edge(true, Side::CODE_UNKNOWN));
        assert!(session_edge(true, Side::Left.code()));
    }

    /// "按回头的方向"才算回家；继续往外推仍报"这一侧没有机器"。
    #[test]
    fn homeward_is_the_opposite_of_the_door_i_came_through() {
        // 我是朝右侧出门到的这台 ⇒ 按左 = 回家，按右 = 继续往外
        assert!(is_homeward(Some(Side::Right), Side::Left));
        assert!(!is_homeward(Some(Side::Right), Side::Right));
        // 上下同理
        assert!(is_homeward(Some(Side::Top), Side::Bottom));
        // 共用一块屏那台没有门：没有"来路"可言，不做回家判断
        assert!(!is_homeward(None, Side::Left));
    }
}
