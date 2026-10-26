//! 接收线程：对端发来的输入 → 注入本机。
//!
//! 从 `start()` 里拆出来（原来是 `start()` 内一个 730 行的 `move` 闭包，
//! 三个线程里最大的一个）。拆法与 `capture.rs` 相同：闭包捕获的 16 个
//! 变量原样变成 `spawn()` 的参数，**闭包体一行未改**。
//!
//! 这个线程是整条事件流的**消费端**，也是最繁的一处 —— 收包、验址、
//! 解密、按状态机分流：
//!
//! - **Enter**：接管开始。按拓扑校正门口方向、重建 `Session`（换对端
//!   必须整体重建）、把光标摆到进门落点；
//! - **Input（位移/按键）**：只在 `Receiving` 时注入。位移按两边屏幕
//!   尺寸比例缩放，乱序（seq 回退）丢弃，按下的键记进 `sess.pressed`，
//!   交还时统一补 up；
//! - **Leave / Takeback**：交还。补 up、清会话、状态回 Idle；
//! - **Pong**：心跳回包，顺手把对端报的状态写进心跳表；
//! - 没在被控却收到输入：按救援逻辑回一条 Takeback（对面以为它还
//!   控制着本机时的解围）。
//!
//! 每个对端一份解密器（`openers`），换人换一个；`Sealer` 相反，全局
//! 单实例（见方案 I9）。
//!
//! ---- 切分轴（2026-10-26，原 793 行一个文件）----
//! 730 行的 `move` 闭包按 match 分支各归一个文件，闭包捕获的 16 个变量
//! 加 5 个局部状态收进 `Ctx` 结构体（spawn 参数一个字段一个），事件处理
//! 方法按 `&mut self` 拿它们：
//!   enter    接管：抢占矩阵、会话重建、门口校正、落点
//!   input    注入：救援、乱序、缩放、有效性验证、推门交还（最大的一块）
//!   handoff  交还：Leave / Takeback（含回程改指向）
//!   upkeep   维护类：Ping/Pong 应答、Anchor 纠漂
//! `run()` 只留骨架：收包 → 验址 → 解密 → 按事件分发。分支中间的
//! continue 搬进方法后变成 return（方法返回即回到 while 顶部）。

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use super::event::{Event, Opener};
use super::plat;
use super::rx::RxTable;
use super::session::{Active, Out, Session};
use super::state::Mode;
use super::util::now_ms;
use super::Link;

mod enter;
mod handoff;
mod input;
mod upkeep;

/// 接收线程的全部状态：`spawn()` 的 16 个参数一个字段一个，原闭包里的
/// 5 个局部可变状态（inject / openers / sess / 两个告警限流时钟）也是字段
/// —— 事件处理方法按 `&mut self` 拿它们。字段私有即可：行为文件是本模块
/// 的后代，看得见。
struct Ctx {
    state: Arc<AtomicU8>,
    stop: Arc<AtomicBool>,
    out: Arc<Out>,
    peer_rx_rx: RxTable,
    log: Arc<dyn Fn(String) + Send + Sync>,
    key: Option<[u8; 32]>,
    capture_rx: Option<Arc<plat::Capture>>,
    recv_rx: Arc<AtomicU64>,
    links_rx: Arc<Vec<Link>>,
    active_rx: Active,
    root_rx: Active,
    guard_rx: Arc<Mutex<Option<(usize, u64)>>>,
    hop_rx: Arc<AtomicU64>,
    recv_scale: f64,
    reclaim_rx: Arc<AtomicU64>,
    inject: plat::Inject,
    openers: HashMap<IpAddr, Opener>,
    sess: Session,
    last_ignore_ms: u64,
    last_stranger_ms: u64,
}

impl Ctx {
    #[allow(clippy::too_many_arguments)]
    fn new(
        state: Arc<AtomicU8>,
        stop: Arc<AtomicBool>,
        out: Arc<Out>,
        peer_rx_rx: RxTable,
        log: Arc<dyn Fn(String) + Send + Sync>,
        key: Option<[u8; 32]>,
        capture_rx: Option<Arc<plat::Capture>>,
        recv_rx: Arc<AtomicU64>,
        links_rx: Arc<Vec<Link>>,
        active_rx: Active,
        root_rx: Active,
        guard_rx: Arc<Mutex<Option<(usize, u64)>>>,
        hop_rx: Arc<AtomicU64>,
        recv_scale: f64,
        reclaim_rx: Arc<AtomicU64>,
        sides_mask: u8,
    ) -> Self {
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
        let openers: HashMap<IpAddr, Opener> = HashMap::new();
        // 一场会话的全部易脏状态打包在一起：换对端时整体重建，从结构
        // 上保证上一场的时钟、尺寸、按键栈、交还账本不会被带进下一场。
        let sess = Session::new(
            links_rx[0].ip,
            links_rx[0].node.clone(),
            links_rx[0].side.unwrap_or_default(),
            links_rx[0].mode == Mode::Edge,
        );
        // 上一次"越界被忽略"的告警时间（抢占矩阵里忽略第三方 Enter 时用）
        let last_ignore_ms: u64 = 0;
        // 上一次"陌生来源"的告警时间。同上面一样限流：不做的话一台没配
        // 链路的机器一秒能刷几十行。
        let last_stranger_ms: u64 = 0;
        Self {
            state,
            stop,
            out,
            peer_rx_rx,
            log,
            key,
            capture_rx,
            recv_rx,
            links_rx,
            active_rx,
            root_rx,
            guard_rx,
            hop_rx,
            recv_scale,
            reclaim_rx,
            inject,
            openers,
            sess,
            last_ignore_ms,
            last_stranger_ms,
        }
    }
}

/// 启动接收线程。参数清单 = 当年闭包捕获的全部变量，语义见 `start()`
/// 里对应的 `let` 行。
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn(
    state: Arc<AtomicU8>,
    stop: Arc<AtomicBool>,
    out: Arc<Out>,
    peer_rx_rx: RxTable,
    log: Arc<dyn Fn(String) + Send + Sync>,
    key: Option<[u8; 32]>,
    capture_rx: Option<Arc<plat::Capture>>,
    recv_rx: Arc<AtomicU64>,
    links_rx: Arc<Vec<Link>>,
    active_rx: Active,
    root_rx: Active,
    guard_rx: Arc<Mutex<Option<(usize, u64)>>>,
    hop_rx: Arc<AtomicU64>,
    recv_scale: f64,
    reclaim_rx: Arc<AtomicU64>,
    sides_mask: u8,
) -> thread::JoinHandle<()> {
    let mut ctx = Ctx::new(
        state, stop, out, peer_rx_rx, log, key, capture_rx, recv_rx, links_rx, active_rx, root_rx,
        guard_rx, hop_rx, recv_scale, reclaim_rx, sides_mask,
    );
    thread::spawn(move || ctx.run())
}

impl Ctx {
    fn run(&mut self) {
        let mut buf = [0u8; 64];
        while !self.stop.load(Ordering::Relaxed) {
                let (len, from) = match self.out.socket.recv_from(&mut buf) {
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
                let Some(from_link) = self.links_rx.iter().position(|l| l.ip == from.ip()) else {
                    if now_ms().saturating_sub(self.last_stranger_ms) > 5000 {
                        self.last_stranger_ms = now_ms();
                        (self.log)(format!(
                            "收到 {} 发来的键鼠包，但它不在这台的链路表里（已丢弃）。\
                             去那台的摆位图上把本机拖进某个方向，否则它的键鼠推过来会消失",
                            from.ip()
                        ));
                    }
                    continue;
                };
                let data = &buf[..len];
                let parsed = match self.key {
                    Some(k) => {
                        let op = self.openers.entry(from.ip()).or_insert_with(|| Opener::new(&k));
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
                self.peer_rx_rx.touch(from.ip());

                match ev {
                    Event::Enter {
                        width,
                        height,
                        along,
                        side: peer_side,
                        flags,
                        ..
                    } => {
                        self.on_enter(seq, from_link, width, height, along, peer_side, flags);
                    }
                    Event::Leave => self.on_leave(from_link),
                    Event::Takeback { along } => self.on_takeback(from_link, along),
                    Event::Ping => self.on_ping(from, from_link),
                    Event::Pong { state: peer } => self.on_pong(from, peer),
                    Event::Anchor { fx, fy } => self.on_anchor(from_link, fx, fy),
                    _ => self.on_input(seq, ev, from, from_link),
                }
        }
    }
}
