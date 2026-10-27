//! 越界建会话：`Captured::Begin`（鼠标撞到 Edge 链路的门）。
//!
//! 三条路按当前状态分流：Idle 正常越界、Receiving 是把控制权抢回来、
//! Sending 是改指向。共同的前置门：收回冷却 600ms（手的惯性还在，
//! 不冷却会 0.1 秒内再次越界，日志实测来回乒乓）、对端必须活着 ——
//! 吞了键鼠又送不出去，要等 2.5 秒断线看守放回来，用户体感就是
//! "卡一下键鼠全没了"。

use std::sync::atomic::Ordering;

use crate::kvm::event::{Event, Side};
use crate::kvm::link::pick_retarget;
use crate::kvm::state::{Mode, State};
use crate::kvm::tuning::PEER_ALIVE_MS;
use crate::kvm::util::{anchor_origin, now_ms};

use super::Ctx;

impl Ctx {
    pub(super) fn on_begin(&mut self, along: u16, side: Side) {
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
        if now_ms().saturating_sub(self.reclaim_cap.load(Ordering::Relaxed)) < 600 {
            return;
        }
        let cur = self.state.load(Ordering::Relaxed);
        if cur == State::Sending as u8 {
            match pick_retarget(cur, self.active_cap.get(), &self.links_cap[..], side) {
                Some(t) => {
                    if let Err(err) = self.linker().retarget(t, along, "推边改指向") {
                        if now_ms().saturating_sub(self.last_begin_warn_ms) > 3000 {
                            self.last_begin_warn_ms = now_ms();
                            (self.log)(err);
                        }
                    }
                }
                None => {
                    if now_ms().saturating_sub(self.last_begin_warn_ms) > 3000 {
                        self.last_begin_warn_ms = now_ms();
                        let node = self
                            .active_cap
                            .get()
                            .and_then(|i| self.links_cap.get(i))
                            .map(|l| l.node.as_str())
                            .unwrap_or("对端");
                        (self.log)(format!(
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
            return;
        }
        let target = if cur == State::Receiving as u8 {
            match self.active_cap.get() {
                Some(i) => Some(i),
                None => return,
            }
        } else {
            // 掩码里有这条边 ⇒ 一定有链路负责它
            self.links_cap
                .iter()
                .position(|l| l.mode == Mode::Edge && l.side == Some(side))
        };
        let Some(t) = target else {
            // 掩码里没有这条边 = 平台层不该发来。有就是掩码漂了。
            if now_ms().saturating_sub(self.last_begin_warn_ms) > 3000 {
                self.last_begin_warn_ms = now_ms();
                (self.log)(format!(
                    "撞到了{}侧，但那条边没有配对机器，忽略",
                    match side {
                        Side::Left => "左",
                        Side::Right => "右",
                        Side::Top => "上",
                        Side::Bottom => "下",
                    }
                ));
            }
            return;
        };
        // 对端必须活着才敢吞键鼠：吞了又送不出去，要等 2.5 秒
        // 断线看守放回来，用户的体感就是"卡一下键鼠全没了"。
        // follow() 有这道检查，这里（边缘越界）同样要有。
        let (idle_cap, seen_cap) = self.rx_cap.alive(self.links_cap[t].ip);
        let alive = seen_cap && idle_cap <= PEER_ALIVE_MS;
        if !alive {
            if now_ms().saturating_sub(self.last_begin_warn_ms) > 3000 {
                self.last_begin_warn_ms = now_ms();
                (self.log)(format!(
                    "光标到了边缘，{} 没有回应，不越界",
                    self.links_cap[t].node
                ));
            }
            return;
        }
        // I2：先写会话目标，再改状态
        self.active_cap.set(Some(t));
        // 正常越界建的会话：起点就是它（多跳的回程目标）
        self.root_cap.set(Some(t));
        self.capture_ref.set_grabbing(true);
        self.state.store(State::Sending as u8, Ordering::SeqCst);
        // 把出门位置和本屏尺寸带给对端：它从对应高度进门，
        // 并按两屏比例缩放位移（不然两块屏分辨率不同时，
        // 一边的 delta 到另一边会被放大/缩小得不成样子）
        let (sw, sh) = self.capture_ref.screen();
        // 锚点起点 = 接收端的落点比例（门口那侧 + 出门时的高度比例），
        // 不是本机光标位置 —— 两边要对齐的是"接收端光标在哪"。
        // 算式与四个 case 的由来见 util::anchor_origin：按发送端出缘
        // 比例初始化会把对端光标在 400ms 后纠到屏幕对侧（v0.6.90
        // 实测「Mac→Windows 光标漂到右侧」的根因）。
        self.anchor_pos = Some(anchor_origin(side, along));
        self.anchor_at = 0; // 下一笔输入立刻补一发锚点，把落点钉死
                            // Enter 双发：丢一包对端就收不到开工信号（好在
                            // Pong 状态自愈 2.5s 兜底，双发把卡顿窗口压到最小）
                            // 出门侧用**平台层报回来的那条边**，不是配置里那个：
                            // 掩码有多位时撞的到底是哪条边只有它能回答，而对端
                            // 靠这个字节把门口定到对面（v0.6.42 的拓扑定理）。
        self.out.to_link(
            t,
            &Event::Enter {
                width: sw.min(65535) as u16,
                height: sh.min(65535) as u16,
                along,
                flags: 0,
                side: side.code(),
            },
        );
        self.out.to_link(
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
            (self.log)(format!(
                "鼠标已越界到 {}，Ctrl+Alt+Esc 可以交还",
                self.links_cap[t].node
            ));
        } else {
            (self.log)(format!(
                "光标推到门口，已从 {} 手里拿回控制权",
                self.links_cap[t].node
            ));
        }
    }
}
