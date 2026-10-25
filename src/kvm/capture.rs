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

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::event::{Event, Side};
use super::link::*;
use super::linker::Linker;
use super::plat;
use super::rx::RxTable;
use super::session::{Active, EdgeSim, Out};
use super::state::{Captured, Mode, State};
use super::tuning::*;
use super::util::*;
use super::Link;

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
            // ---- 绝对锚点（v0.6.67）----
            //
            // 传输仍然是**相对位移**（丢包只少走几像素，不会跳），但每隔
            // 400ms 补一个"虚拟光标在本屏的位置比例"。接收端只在偏差超过
            // 阈值时才纠正 —— 这样长会话里"比例换算每笔四舍五入"攒出来的
            // 漂移会被抹掉，而正常移动完全不受影响。
            //
            // 初始值取**接收端的落点**（follow 是居中），不是发送端真实
            // 光标 —— 两边要对齐的是"接收端光标在哪"，不是"发送端光标在哪"。
            // edge 会话由 Begin 分支按 side/along 覆盖成门口位置。
            let mut anchor_pos: Option<(f64, f64)> = None;
            let mut anchor_at: u64 = 0;

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
                        // 锚点起点 = 接收端的落点（门口那侧 + 出门时的高度比例），
                        // 不是本机光标位置 —— 两边要对齐的是"接收端光标在哪"。
                        // along 未知（0xFFFF）时按居中算。
                        let af = if along == 0xFFFF {
                            0.5
                        } else {
                            along as f64 / 65535.0
                        };
                        anchor_pos = Some(match side {
                            Side::Left => (0.0, af),
                            Side::Right => (1.0, af),
                            Side::Top => (af, 0.0),
                            Side::Bottom => (af, 1.0),
                        });
                        anchor_at = 0; // 下一笔输入立刻补一发锚点，把落点钉死
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
                            // 会话结束：锚点模型作废，下一场重新按落点初始化
                            anchor_pos = None;
                            continue;
                        }
                        // 非 edge 起点（热键/面板切换）进来的会话：接收端是
                        // **居中**落点，所以虚拟光标也从中心开始 —— 两边对齐
                        // 的是接收端，不是本机光标。
                        if anchor_pos.is_none() {
                            anchor_pos = Some((0.5, 0.5));
                            anchor_at = 0;
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
                        // 积分虚拟光标，并按固定节奏补一发绝对锚点。
                        // 只在位移上做：按键/滚轮不改位置。
                        if let Event::Motion { dx, dy } = ev {
                            let (aw, ah) = capture_ref.screen();
                            if aw > 0 && ah > 0 {
                                if let Some((ax, ay)) = anchor_pos {
                                    anchor_pos = Some((
                                        (ax + dx as f64 / aw as f64).clamp(0.0, 1.0),
                                        (ay + dy as f64 / ah as f64).clamp(0.0, 1.0),
                                    ));
                                }
                            }
                            let now = now_ms();
                            if now.saturating_sub(anchor_at) >= ANCHOR_MS {
                                anchor_at = now;
                                if let Some((ax, ay)) = anchor_pos {
                                    out.send(&Event::Anchor {
                                        fx: (ax * 65535.0).round().clamp(0.0, 65535.0) as u16,
                                        fy: (ay * 65535.0).round().clamp(0.0, 65535.0) as u16,
                                    });
                                }
                            }
                        }
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
                                        // 这条边没有别的机器：什么都不做，
                                        // 手继续推也只是把对端光标顶在边上
                                        if let Some(t) = pick_retarget(
                                            State::Sending as u8,
                                            act,
                                            &links_cap[..],
                                            side,
                                        ) {
                                            if let Err(err) = linker!()
                                                .retarget(t, along, "推边改指向")
                                            {
                                                (log)(err);
                                            }
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
    })
}
