//! 会话牵动的共享状态：当前目标、发送通道、两侧会话状态。
//!
//! 从 `mod.rs` 拆出来。这四件东西有一个共同点 —— **生命周期横跨整个
//! KVM 实例、被多个线程同时摸**，所以当年都住在 `mod.rs` 里。按关注点
//! 分成四组：
//!
//! - [`Active`]：当前会话目标（原子量，抓取线程 1000Hz 读）。
//! - [`Out`]：发送通道（socket + 加密 + 序号），所有出口包的必经之路。
//! - [`Session`]：**接收侧**一场会话的全部易脏状态（对端正把键鼠转发
//!   给我）。换对端必须整体重建，理由见类型上的注释。
//! - [`EdgeSim`]：**发送侧**的"门口"模拟器。抓取期间平台层不产边缘事件，
//!   只能拿自己转出去的位移在本地复刻对端光标。
//!
//! 后两者是同一场会话的两侧，判据逐条对齐（同一条 `RETURN_PUSH`），
//! 所以放同一个文件 —— 改一头忘了另一头，两边手感就会不对称。
//!
//! 全部 `pub(super)`：这些类型的字段被 `start()` 主循环直接读写（18 个
//! 字段逐个加访问器只会更难读），但**一个都不许漏出 `kvm`** —— 外部只
//! 该通过 `Kvm` 句柄和 `Options` 打交道。

use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::event::{self, Event, Sealer, Side};
use super::tuning::{EDGE_REARM_DIST, HOP_THROTTLE_MS, RETURN_PUSH};
use super::Link;

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
pub(super) struct Active(Arc<AtomicU64>);

impl Active {
    pub(super) fn new() -> Self {
        Self(Arc::new(AtomicU64::new(0)))
    }

    pub(super) fn get(&self) -> Option<usize> {
        match self.0.load(Ordering::Relaxed) {
            0 => None,
            n => Some((n - 1) as usize),
        }
    }

    pub(super) fn set(&self, index: Option<usize>) {
        let v = match index {
            Some(i) => i as u64 + 1,
            None => 0,
        };
        self.0.store(v, Ordering::SeqCst);
    }
}

/// 发送侧的共享状态。几个线程都要往里写，所以里面全是 Mutex。
pub(super) struct Out {
    pub(super) socket: UdpSocket,
    /// KVM 端口（= 每条链路的收件端口，全网一致）
    pub(super) port: u16,
    pub(super) links: Arc<Vec<Link>>,
    pub(super) active: Active,
    /// None = 没配对，走明文
    pub(super) sealer: Mutex<Option<Sealer>>,
    pub(super) seq: Mutex<u16>,
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
    pub(super) fn send(&self, ev: &Event) {
        if let Some(i) = self.active.get() {
            self.to_link(i, ev);
        }
    }

    /// 按链路下标发送。**回包与救援必须用它**：多对端下"当前会话目标"
    /// 只是其中一台，把给 A 的 Pong 发给它，实际收件人可能是 B。
    pub(super) fn to_link(&self, index: usize, ev: &Event) {
        if let Some(addr) = self.addr_of(index) {
            self.to(addr, ev);
        }
    }

    /// 直发单个地址。只有"回包"（Pong / Takeback）用 —— 它们是对**刚收到
    /// 的那包**的回应，收件人就是来源地址，不能按会话目标猜。
    pub(super) fn to(&self, addr: SocketAddr, ev: &Event) {
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

/// 一场会话（"对端正在把键鼠转发给我"）的全部易脏状态。
///
/// 为什么打包成一件事，而不是散成一堆局部变量：多对端下会换对端，而
/// 上一场残留下来的时钟、尺寸、按键栈会变成很难理解的故障 ——
/// 「刚进门 2 秒就被交还」（带着上一场的 `last_giveback_ms`）、
/// 「位移按另一台的分辨率缩放」（带着上一场的 `sender_size`）、
/// 「一到对端就卡住某个键」（带着上一场没补 up 的 `pressed`）。
/// 打包之后，换对端 = 整体重建，不再依赖"谁记得逐个清零"。
pub(super) struct Session {
    /// 这场会话的对端。换对端必须整体重建 —— 判断"要不要重建"就看它。
    pub(super) peer: IpAddr,
    /// 对端节点名（日志用）
    pub(super) node: String,
    /// 这条链路是不是"有门的"（Edge）。注入落点、推门交还都只对
    /// 有门的链路成立 —— 跟随画面的那条不能按边判定，否则用户朝任意
    /// 一侧推一下就被当成交还了。
    pub(super) edge: bool,
    /// 本机门口方向（朝对端那一侧）。Enter 带来的"对端出门侧"会校正它。
    pub(super) portal: Side,
    /// 上一笔的 seq（位移乱序丢弃用）
    pub(super) last_seq: u16,
    /// 本机按下尚未松开的键/钮，交还时统一补 up（否则对端卡键）
    pub(super) pressed: Vec<Event>,
    /// 本会话注入了多少笔。0 = 传输断在 Enter 之后，是排障的第一手证据
    pub(super) injected: u64,
    /// 回 Takeback 的限流时间戳（没在被控状态却收到输入事件时用）
    pub(super) last_takeback_ms: u64,
    /// "推门交还"的武装状态：进门后要先往里走够远才武装，
    /// 防止进门落点贴着门边、手一抖就被弹回去
    pub(super) edge_armed: bool,
    /// 进门时刻：交还判定要加冷却，刚进门那一两秒手还在门口惯性里摆动
    pub(super) enter_ms: u64,
    /// 顶住门边的起始时刻（0=没在顶）：日志用
    pub(super) edge_hold_ms: u64,
    /// 朝门外累计推了多少点（见 RETURN_PUSH）：交还的**主判据**。
    /// 离开门边就清零，所以只统计"贴着门还在往外推"的那一段。
    pub(super) out_push: i32,
    /// 进门后前 30 笔位移的采样串（诊断「谁把光标按在门口」）
    pub(super) motion_sample: String,
    /// 上次交还时刻：交还后 2 秒内不再触发交还 —— 弹跳循环
    ///（交还→光标回门口→手没停→立刻再进门→又被按回门口）的破窗器
    pub(super) last_giveback_ms: u64,
    /// 对端屏幕尺寸（Enter 带来的，0 = 旧版或未知）。位移缩放用：
    /// 对端像素 / 本端点 就是每单位对端位移该在本端走多少。
    pub(super) sender_size: (f64, f64),
    /// 注入有效性验证窗口：(起始毫秒, 起始真实光标位置, 起始账本位移, 笔数)
    pub(super) verify: Option<(u64, (f64, f64), f64, u32)>,
    /// 疑似失效计数：净位移判据在光标来回摆动时会假阳性（摆回起点
    /// 净位移=0），必须连续两窗都判失效才交还，单窗只复检
    pub(super) suspect: u32,
}

impl Session {
    pub(super) fn new(peer: IpAddr, node: String, portal: Side, edge: bool) -> Self {
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
pub(super) struct EdgeSim {
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
    pub(super) fn new() -> Self {
        Self {
            pos: None,
            armed: false,
            out_push: 0,
            last_ms: 0,
        }
    }

    /// 换会话目标时复位：新的起点没有"上一趟推了多少"可言。
    pub(super) fn reset(&mut self) {
        self.pos = None;
        self.armed = false;
        self.out_push = 0;
    }

    /// 喂一笔转发出去的位移。返回 `Some((边, 沿门轴比例))` = 用户顶到门口了。
    pub(super) fn feed(
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
