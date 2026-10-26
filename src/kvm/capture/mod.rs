//! 抓取线程：本机键鼠的入口。
//!
//! 从 `start()` 里拆出来（原来是 `start()` 内一个 540 行的 `move` 闭包）。
//! 拆法：闭包捕获的那 18 个变量原样变成 `spawn()` 的参数，**闭包体一行
//! 未改** —— 参数名保留当年的 `_cap` 后缀，就是为了让"哪个参数对应当年
//! 哪个 `let x_cap = …clone()`"一眼对得上。
//!
//! 这个线程干的事：从平台钩子收 `Captured` 消息，按状态机决定
//!
//! - **Idle**：只等 `Begin`（鼠标撞到 Edge 链路的门）→ 建场转发；
//! - **Sending**：把 `Input`（位移/按键）转发给当前会话目标，顺带
//!   - 喂 `EdgeSim` 判"用户想把光标顶过邻居那侧"（推边改指向），
//!   - 每 `ANCHOR_MS` 补一个绝对锚点校漂移，
//!   - 认 `Claim`（被控期间真动物理键鼠）与紧急收回热键；
//! - **Receiving**：本机被对面控制，只盯抢回信号。
//!
//! 权限不足（`trusted() == false`）时 `start()` 根本不 spawn 这个线程。
//!
//! ---- 切分轴（2026-10-26，原 600 行一个文件）----
//! `spawn` 的 18 个参数加闭包里跨臂存活的 17 个局部状态收进 `Ctx`
//! 结构体，`run()` 只留收包 → 分发骨架，7 个 `Captured` 分支按
//! "一个变体一个文件"提成 `on_*` 方法（分支中间的 `continue` 搬进
//! 方法后变成 `return`）：
//!   begin   越界建会话：Begin（Idle 正常越界 / Receiving 抢回 /
//!          Sending 改指向，三条路分流）
//!   input   转发：Input（位移流 + 绝对锚点 + 推边影子光标 + 记账采样）
//!   claim   被控期间物理抢回：Claim（时间窗 + 去抖）
//!   end     紧急收回：End（先补 key-up 再通知对端）
//!   hop     会话中改指向 / 回起点：Hop（三态表态 + 限流 + 回家判断）
//! `Note` / `PasteIntent` 两条不动任何循环状态（一行落日志 / 调一次
//! 回调），留在 `run()` 的骨架里内联，提成方法只是多一层跳转。
//! `take_back_here` 闭包和 `linker!` 宏也改成 `Ctx` 方法 —— 前者三个
//! 调用方（热键 / 物理键鼠 / 推门）共用同一份收回逻辑，后者让
//! `Linker` 的十个借用字段在一处出现。

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::event::Event;
use super::linker::Linker;
use super::plat;
use super::rx::RxTable;
use super::session::{Active, EdgeSim, Out};
use super::state::{Captured, State};
use super::util::now_ms;
use super::Link;

mod begin;
mod claim;
mod end;
mod hop;
mod input;

/// 抓取线程的全部状态：`spawn` 的 18 个参数一个字段一个（`_cap` 后缀
/// 保留，对得上当年闭包的捕获清单），闭包里跨臂存活的 17 个局部状态
/// 也是字段 —— 分支处理方法按 `&mut self` 拿它们。字段私有即可：
/// 行为文件是本模块的后代，看得见。
struct Ctx {
    state: Arc<AtomicU8>,
    stop: Arc<AtomicBool>,
    out: Arc<Out>,
    capture_ref: Arc<plat::Capture>,
    log: Arc<dyn Fn(String) + Send + Sync>,
    on_panic: Option<Arc<dyn Fn() + Send + Sync>>,
    rx_cap: RxTable,
    reclaim_cap: Arc<AtomicU64>,
    on_paste: Option<Arc<dyn Fn() + Send + Sync>>,
    links_cap: Arc<Vec<Link>>,
    active_cap: Active,
    root_cap: Active,
    guard_cap: Arc<Mutex<Option<(usize, u64)>>>,
    hop_cap: Arc<AtomicU64>,
    recv_cap: Arc<AtomicU64>,
    push_hop: bool,
    sides_mask: u8,
    rx: Receiver<Captured>,
    pressed: Vec<Event>,
    /// 会话中"推边改指向"的影子光标（见 EdgeSim）。只在 Follow 会话
    /// 里喂它，换目标就复位。
    edge_sim: EdgeSim,
    sim_active: Option<usize>,
    was_sending: bool,
    /// 转发统计：排障「键鼠到了对面却不动」时，需要知道 Mac 这边
    /// 到底发出去多少笔、鼠标位移长什么样。Sending→Idle 翻转时打汇总。
    forwarded: usize,
    motions: usize,
    /// 转发位移流采样（前 30 笔，与接收端收到的流对比定罪负位移源头）
    fwd_sample: String,
    /// 整轮位移记账：净位移 / 负向笔数与累计。
    /// "推过去 1 秒就被弹回来"必须分清两件事 —— 是我们的流真的
    /// 带着负位移把对端光标推回门口（Mac 侧钉扎 warp 泄漏/手飘），
    /// 还是流是正的、对端注入没生效。前 30 笔采样看不出来这个。
    sum_dx: i64,
    sum_dy: i64,
    neg_dx_count: usize,
    neg_dx_sum: i64,
    min_dx: i32,
    max_dx: i32,
    /// 越界被拒（对端没回应）的告警限流
    last_begin_warn_ms: u64,
    /// 上一次认"物理抢回"的时刻（去抖：连按两下只发一次 Takeback）
    last_claim_ms: u64,
    /// ---- 绝对锚点（v0.6.67）----
    ///
    /// 传输仍然是**相对位移**（丢包只少走几像素，不会跳），但每隔
    /// 400ms 补一个"虚拟光标在本屏的位置比例"。接收端只在偏差超过
    /// 阈值时才纠正 —— 这样长会话里"比例换算每笔四舍五入"攒出来的
    /// 漂移会被抹掉，而正常移动完全不受影响。
    ///
    /// 初始值取**接收端的落点**（follow 是居中），不是发送端真实
    /// 光标 —— 两边要对齐的是"接收端光标在哪"，不是"发送端光标在哪"。
    /// edge 会话由 Begin 分支按 side/along 覆盖成门口位置。
    anchor_pos: Option<(f64, f64)>,
    anchor_at: u64,
}

impl Ctx {
    /// 被控期间把控制权要回来。三条入口动作完全一样：热键、
    /// 真动物理键鼠、以及主循环那条"推门"（走 Begin/Receiving 分支，
    /// 不经过这里，因为那边还要处理 Enter 与落点）。
    ///
    /// 必须**先快照目标再动状态**：active 一清，后面的 Takeback
    /// 就不知道该发给谁了 —— 对端会一直以为它还控制着本机。
    fn take_back_here(&self, why: &str) {
        let Some(t) = self.active_cap.get() else {
            return;
        };
        self.state.store(State::Idle as u8, Ordering::SeqCst);
        self.capture_ref.set_claim_armed(false);
        // 单发 UDP 会丢：丢一包对端就卡在 Sending 连键鼠一起吞掉。
        // 接收端有状态闸门，重复的 Takeback 是幂等的。
        self.out.to_link(t, &Event::Takeback { along: 0xFFFF });
        self.out.to_link(t, &Event::Takeback { along: 0xFFFF });
        self.active_cap.set(None);
        self.root_cap.set(None);
        self.reclaim_cap.store(now_ms(), Ordering::SeqCst);
        (self.log)(format!("{why}，已请对端交还键鼠"));
    }

    /// 改指向：撞边（Begin@Sending）与热键（Hop）共用同一份实现。
    /// 三处入口各自复制一遍的下场是其中一处忘了补 key-up / 忘了堵
    /// Takeback 残包，而症状永远是那句"改个方向键鼠全没了"。
    fn linker(&self) -> Linker<'_> {
        Linker {
            state: &self.state,
            out: &self.out,
            links: &self.links_cap[..],
            active: &self.active_cap,
            root: &self.root_cap,
            capture: Some(&self.capture_ref),
            guard: &self.guard_cap,
            hop_ms: &self.hop_cap,
            peer_rx: &self.rx_cap,
            log: &*self.log,
        }
    }
}

/// 启动抓取线程。参数清单 = 当年闭包捕获的全部变量（含直接移入的
/// `rx` 通道），语义见 `start()` 里对应的 `let` 行。
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn(
    state: Arc<AtomicU8>,
    stop: Arc<AtomicBool>,
    out: Arc<Out>,
    capture_ref: Arc<plat::Capture>,
    log: Arc<dyn Fn(String) + Send + Sync>,
    on_panic: Option<Arc<dyn Fn() + Send + Sync>>,
    rx_cap: RxTable,
    reclaim_cap: Arc<AtomicU64>,
    on_paste: Option<Arc<dyn Fn() + Send + Sync>>,
    links_cap: Arc<Vec<Link>>,
    active_cap: Active,
    root_cap: Active,
    guard_cap: Arc<Mutex<Option<(usize, u64)>>>,
    hop_cap: Arc<AtomicU64>,
    recv_cap: Arc<AtomicU64>,
    push_hop: bool,
    sides_mask: u8,
    rx: Receiver<Captured>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut ctx = Ctx {
            state,
            stop,
            out,
            capture_ref,
            log,
            on_panic,
            rx_cap,
            reclaim_cap,
            on_paste,
            links_cap,
            active_cap,
            root_cap,
            guard_cap,
            hop_cap,
            recv_cap,
            push_hop,
            sides_mask,
            rx,
            // ---- 以下是闭包里跨臂存活的循环状态（初值 = 当年的 let 行）----
            pressed: Vec::new(),
            edge_sim: EdgeSim::new(),
            sim_active: None,
            was_sending: false,
            forwarded: 0,
            motions: 0,
            fwd_sample: String::new(),
            sum_dx: 0,
            sum_dy: 0,
            neg_dx_count: 0,
            neg_dx_sum: 0,
            min_dx: 0,
            max_dx: 0,
            last_begin_warn_ms: 0,
            last_claim_ms: 0,
            anchor_pos: None,
            anchor_at: 0,
        };
        ctx.run()
    })
}

impl Ctx {
    fn run(&mut self) {
        while !self.stop.load(Ordering::Relaxed) {
            // 状态翻转检查放在收包之前，超时唤醒的空转也能看到翻转
            let sending = self.state.load(Ordering::Relaxed) == State::Sending as u8;
            if self.was_sending && !sending && self.forwarded > 0 {
                (self.log)(format!(
                    "本轮转发结束：共 {} 笔（鼠标位移 {} 笔，\
                 净 dx={} dy={}，负向 {} 笔/{}，\
                 dx 范围 {}~{}）",
                    self.forwarded,
                    self.motions,
                    self.sum_dx,
                    self.sum_dy,
                    self.neg_dx_count,
                    self.neg_dx_sum,
                    self.min_dx,
                    self.max_dx
                ));
                self.forwarded = 0;
                self.motions = 0;
                self.fwd_sample.clear();
                self.sum_dx = 0;
                self.sum_dy = 0;
                self.neg_dx_count = 0;
                self.neg_dx_sum = 0;
                self.min_dx = 0;
                self.max_dx = 0;
            }
            self.was_sending = sending;

            let Ok(msg) = self.rx.recv_timeout(Duration::from_millis(200)) else {
                continue;
            };
            match msg {
                Captured::Begin { along, side } => {
                    self.on_begin(along, side);
                }
                Captured::Input(ev) => {
                    self.on_input(ev);
                }
                Captured::Note(text) => {
                    (self.log)(text);
                }
                Captured::PasteIntent => {
                    // 键已经被 tap 吞掉了，这里只负责通知上层去传文件。
                    // 不在这里做任何耗时动作 —— 这是抓取线程的接收循环。
                    if let Some(cb) = self.on_paste.as_ref() {
                        cb();
                    }
                }
                Captured::Claim => {
                    self.on_claim();
                }
                Captured::End => {
                    self.on_end();
                }
                Captured::Hop(side) => {
                    self.on_hop(side);
                }
            }
        }
    }
}
