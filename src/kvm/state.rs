//! 状态机词汇表：模式、角色、抓取侧消息、本机状态。
//!
//! 这四个枚举是 KVM 状态机的全部词汇，合在这里不到 160 行：
//!
//! - [`Mode`] / [`Role`] 是**配置维度** —— 跑哪种模式、键鼠插在谁身上，
//!   都由用户/面板指定，运行期不变。三者正交：模式定物理场景，角色定
//!   本机在场景里的位置（详见 `mod.rs` 顶部两种模式的说明）。
//! - [`Captured`] 是**抓取侧送上来的消息**，平台层（`win.rs` / `mac.rs`）
//!   产生、主循环消费。
//! - [`State`] 是**本机当前状态**，跨线程共享，存的是 `AtomicU8`，
//!   所以带 `as u8` / `from_u8` 一对转换。
//!
//! 它们全是纯数据、不碰网络不碰平台 API，这是能整块搬出 `mod.rs` 的前提。
//! 引用方式：`mod.rs` 里 `pub use state::{Captured, Mode, Role, State};`
//! 再导出，外部（`tray.rs` 等）仍走 `kvm::Mode` 原路径。

use super::event::{Event, Side};

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
    /// `pub(super)`：调用方全在 `kvm/mod.rs`（`Kvm::state()` 和 `start()`
    /// 主循环），平台层用不到 —— 它们只认 `Captured`。
    pub(super) fn from_u8(v: u8) -> Self {
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
