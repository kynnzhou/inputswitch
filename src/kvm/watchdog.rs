//! 心跳与断线看守：UDP 世界里唯一知道"对面还在不在"的那个线程。
//!
//! 从 `start()` 里拆出来（原来是 `start()` 内一个 260 行的 `move` 闭包）。
//! 拆法与 `capture.rs` / `recv.rs` 相同：捕获清单变参数，闭包体未动。
//!
//! 干三件事：
//!
//! 1. **发心跳**：每条链路一个 Pong（带本机状态字节），≤4 包 / 800ms。
//!    状态字节让任何错位在 2.5 秒内被看见；
//! 2. **断线自愈**：对端超过 `IDLE_TIMEOUT_MS` 没动静就收回键鼠 ——
//!    不然本机会永远停在"转发中"，把用户的键鼠全吞掉；
//! 3. **状态错位自愈**：两边状态机对不上（我在 Sending 它却不在
//!    Receiving）时按矩阵纠正，被控期间平台层收窄的掩码在会话结束后
//!    铺回来。
//!
//! 睡 `PING_INTERVAL` 就醒，所以停止延迟最多一个间隔。

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread;

use super::event;
use super::event::Event;
use super::plat;
use super::rx::RxTable;
use super::session::{Active, Out};
use super::state::State;
use super::tuning::*;
use super::util::now_ms;
use super::Link;

/// 启动看守线程。参数清单 = 当年闭包捕获的全部变量，语义见 `start()`
/// 里对应的 `let` 行。
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn(
    state: Arc<AtomicU8>,
    stop: Arc<AtomicBool>,
    out: Arc<Out>,
    peer_rx_gd: RxTable,
    log: Arc<dyn Fn(String) + Send + Sync>,
    capture: Option<Arc<plat::Capture>>,
    reclaim_gd: Arc<AtomicU64>,
    links_gd: Arc<Vec<Link>>,
    active_gd: Active,
    root_gd: Active,
    mask_gd: u8,
) -> thread::JoinHandle<()> {
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
    })
}
