//! 启动参数：链路表与全局选项。
//!
//! 从 `mod.rs` 拆出来。这两个类型是 `kvm::start()` 的**唯一入口签名**
//! （`start(opts: Options)`），也是外部（`tray.rs` 的配置装配）唯一需要
//! 构造的东西 —— 所以全是 `pub` 字段，没有一个方法。字段上的注释就是
//! 这份 API 的文档，改动时连注释一起改。

use std::net::IpAddr;
use std::sync::Arc;

use super::event::Side;
use super::state::{Mode, Role};

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
    /// 接收位移倍率（默认 1.0）：对端发来的位移在注入前额外乘一次。
    ///
    /// 分辨率比例那一步是"几何正确"，这一位是"手感微调" —— 两个系统的
    /// 指针加速曲线不同，同一串位移流在两边速度不可能一样。见
    /// `config::Kvm::recv_scale` 的注释。
    pub recv_scale: f64,
}
