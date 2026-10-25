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

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use super::event::{self, Event, Opener, Side};
use super::link::*;
use super::linker::Linker;
use super::plat;
use super::rx::RxTable;
use super::session::{Active, Out, Session};
use super::state::{Mode, State};
use super::tuning::*;
use super::util::*;
use super::Link;

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
                            // 把对端报的屏幕尺寸和算出来的位移倍率写进日志：
                            // 「回到本机鼠标变慢/变快」只能靠这两个数定位 ——
                            // 是分辨率比例算错了，还是两系统加速曲线不同。
                            let (lw, lh) = inject.screen();
                            (log)(format!(
                                "对端屏幕 {}x{}，本机 {}x{} ⇒ 位移倍率 x{:.3}（接收侧微调 x{recv_scale:.2}）",
                                width,
                                height,
                                lw as u32,
                                lh as u32,
                                (lw / width as f64) * recv_scale
                            ));
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
                    // 绝对锚点：只用来**纠漂移**，不参与正常移动。
                    //
                    // 传输还是相对位移（丢包只少走几像素），锚点每 400ms 来
                    // 一发"发送端虚拟光标的比例"。偏差小于阈值就完全不管 ——
                    // 逐帧纠会让光标看起来在抖；超过阈值（≈ 屏宽 0.6%）才
                    // 直接摆过去，修的是长会话累积漂移和"切回来不在原位"。
                    Event::Anchor { fx, fy } => {
                        if state.load(Ordering::Relaxed) != State::Receiving as u8
                            || active_rx.get() != Some(from_link)
                        {
                            continue;
                        }
                        // 进门 300ms 内不纠：落点刚摆好，锚点还在路上，
                        // 这时候纠等于跟自己的落点逻辑打架
                        if now_ms().saturating_sub(sess.enter_ms) < 300 {
                            continue;
                        }
                        let Some((cx, cy)) = inject.cursor() else {
                            continue;
                        };
                        let (w, h) = inject.screen();
                        if let Some((tx, ty)) = anchor_target(fx, fy, w, h, (cx, cy)) {
                            inject.place_cursor(tx, ty);
                            (log)(format!(
                                "锚点校正：({cx:.0},{cy:.0}) → ({tx:.0},{ty:.0})"
                            ));
                        }
                        continue;
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
                                    dx: rescale_delta(*dx, (lw / sess.sender_size.0) * recv_scale),
                                    dy: rescale_delta(*dy, (lh / sess.sender_size.1) * recv_scale),
                                }
                            }
                            _ => ev,
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
    })
}
