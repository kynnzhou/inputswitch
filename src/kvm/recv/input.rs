//! 注入（Input）：位移/按键的落地。救援、乱序丢弃、按两屏比例缩放、
//! 注入有效性验证、Edge 模式推门交还都在这条路上。
//!
//! 这是整个消费端最大的一块，也是 v0.6.x 实测教训最密的一段：
//! 按键 up 不能丢、缩放比按屏幕宽算、判失效要连续两个窗口、推门
//! 交还按累计位移不按顶住时长 —— 每条注释都是踩过的坑，改前先读。

use std::net::SocketAddr;
use std::sync::atomic::Ordering;

use super::Ctx;
use crate::kvm::event::{self, Event, Side};
use crate::kvm::plat;
use crate::kvm::state::State;
use crate::kvm::tuning::{EDGE_REARM_DIST, RETURN_PUSH};
use crate::kvm::util::{now_ms, release_local, rescale_delta, same_key};

impl Ctx {
    pub(super) fn on_input(
        &mut self,
        inject: &mut plat::Inject,
        seq: u16,
        ev: Event,
        from: SocketAddr,
        from_link: usize,
    ) {
        // 只有真在被控制时才注入。否则对端明明已经松手，
        // 我们还在把它的事件打进系统 —— 注入的事件和本机
        // 真实的键鼠打架，看起来就是"鼠标乱窜、点不了东西"。
        // 而且必须是**当前会话那台**：被 A 控制时收到 B 的
        // 事件（B 还在 Sending，它的 Takeback 丢了）一律不注入。
        if self.state.load(Ordering::Relaxed) != State::Receiving as u8
            || self.active_rx.get() != Some(from_link)
        {
            // 对端还在发输入事件 = 它以为它控制着我们
            // （Enter 丢了，或它那边卡在 Sending）。回一条
            // Takeback 让它收回；限流，别被每秒几百笔的
            // 位移事件刷爆。
            if now_ms().saturating_sub(self.sess.last_takeback_ms) > 1000 {
                self.sess.last_takeback_ms = now_ms();
                // 救援也要回给来源：把 Takeback 发给会话目标，
                // 真正该收回的那台就永远收不到
                self.out.to(from, &Event::Takeback { along: 0xFFFF });
                self.out.to(from, &Event::Takeback { along: 0xFFFF });
                (self.log)(format!(
                    "没在被控状态却收到 {} 的输入事件，已通知它收回",
                    self.links_rx[from_link].node
                ));
            }
            return;
        }
        // 乱序的旧包丢掉 —— **只对位移生效**：按键（尤其
        // up）被丢的代价是本机卡键/卡住左键，之后点什么
        // 都没反应，比重复一笔点击严重得多。高频流下 UDP
        // 偶发乱序，Button-up 被吞是实测踩过的坑。
        if let Event::Motion { .. } = ev {
            if seq.wrapping_sub(self.sess.last_seq) > 0x8000 && seq != 0 {
                return;
            }
            self.sess.last_seq = seq;
        }
        // 按键/键盘逐笔落日志：区分"Mac 端发来了右键"和
        // "注入环节自己生出右键"（位移量大不打）
        if let Event::Button { button, down } = ev {
            (self.log)(format!(
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
                if self.sess.sender_size.0 >= 1.0 && self.sess.sender_size.1 >= 1.0 =>
            {
                let (lw, lh) = inject.screen();
                Event::Motion {
                    dx: rescale_delta(*dx, (lw / self.sess.sender_size.0) * self.recv_scale),
                    dy: rescale_delta(*dy, (lh / self.sess.sender_size.1) * self.recv_scale),
                }
            }
            _ => ev,
        };
        inject.apply(&to_inject);
        self.sess.injected += 1;
        if self.sess.injected == 1 {
            // 把首笔事件的具体数值打进日志：位移是 0、键码是
            // 乱码这类「内容坏掉」的问题，只有这里能看出来
            (self.log)(format!("事件流开始注入（首笔：{to_inject:?}）"));
        }
        // 进门后前 30 笔位移流采样：Windows 日志实锤光标被
        // 按死在门口 x=0 顶住 300ms，必须分辨是"位移流里
        // 持续来负数"还是"外力直接挪真实光标"
        if let Event::Motion { dx, dy } = to_inject {
            if self.sess.injected <= 30 {
                if self.sess.injected == 1 {
                    self.sess.motion_sample.clear();
                }
                self.sess.motion_sample.push_str(&format!(" {dx},{dy}"));
                if self.sess.injected == 30 {
                    (self.log)(format!(
                        "进门位移流（前 30 笔 dx,dy）：{}",
                        self.sess.motion_sample
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
        if self.sess.edge {
            if let Event::Motion { .. } = ev {
                // (是否判失效, 真实光标移动, 账本移动)
                let mut verdict: Option<(bool, f64, f64)> = None;
                if self.sess.verify.is_none() {
                    let p = inject.cursor().unwrap_or((0.0, 0.0));
                    self.sess.verify = Some((now_ms(), p, inject.walked(), 1));
                } else if let Some(slot) = self.sess.verify.as_mut() {
                    slot.3 += 1;
                    if slot.3 >= 40 && now_ms().saturating_sub(slot.0) >= 1000 {
                        let cur = inject.cursor().unwrap_or(slot.1);
                        let real_moved = (cur.0 - slot.1 .0).abs() + (cur.1 - slot.1 .1).abs();
                        let ledger_moved = inject.walked() - slot.2;
                        let invalid = real_moved < 2.0 && ledger_moved >= 2.0;
                        verdict = Some((invalid, real_moved, ledger_moved));
                    }
                }
                match verdict {
                    // 注入无效：先复检。连续第二个窗口仍失效才交还
                    Some((true, real_moved, ledger_moved)) => {
                        self.sess.verify = None;
                        if self.sess.suspect >= 1 {
                            release_local(inject, &mut self.sess.pressed);
                            self.out
                                .to_link(from_link, &Event::Takeback { along: 0xFFFF });
                            self.out
                                .to_link(from_link, &Event::Takeback { along: 0xFFFF });
                            self.state.store(State::Idle as u8, Ordering::SeqCst);
                            self.active_rx.set(None);
                            self.reclaim_rx.store(now_ms(), Ordering::SeqCst);
                            (self.log)(format!(
                                "⚠️ 连续两窗复检注入均失效（账本走了 \
                                 {ledger_moved:.0} 点、真实光标只动 \
                                 {real_moved:.1} 点），已通知 {} 收回键鼠",
                                self.sess.node
                            ));
                            return;
                        }
                        self.sess.suspect += 1;
                        // 判"失效"之前先做一次最廉价的自愈：
                        // 重申光标关联（幂等）。v0.6.40 的根因
                        // 就是光标被留在"断开"状态，注入推不动；
                        // 接回去之后第二个窗口往往就正常了，
                        // 比直接把键鼠抢回来对用户友好得多。
                        if let Some(cap) = self.capture_rx.as_ref() {
                            cap.set_grabbing(false);
                        }
                        (self.log)(format!(
                            "疑似注入失效（账本走 {ledger_moved:.0} 点、\
                             真实光标动 {real_moved:.1} 点），\
                             已重申光标关联，复检中……"
                        ));
                    }
                    // 注入正常（或只是顶死在屏幕边上）：重开窗口
                    Some((false, ..)) => {
                        self.sess.verify = None;
                        self.sess.suspect = 0;
                    }
                    None => {}
                }
            }
        }
        if ev.is_down() {
            self.sess.pressed.push(ev);
        } else {
            self.sess.pressed.retain(|p| !same_key(p, &ev));
        }
        // Edge 模式：对端光标撞回"朝着对端"的那条边（本机
        // 门口那一侧），说明用户想把控制权还回去了 ——
        // 必须回 Takeback 让对端收回键鼠，否则它会一直抓着
        // 键鼠往这边转发，两边全卡死。以前有两个 bug：判的
        // 是 opposite（用户得横穿整个屏幕到最远端才触发），
        // 而且触发后只是自己悄悄 Idle、对端毫不知情。
        // Follow 模式不判断这个 —— 那边没有"边缘"概念，
        // 交还是靠画面切回来或热键。
        if self.sess.edge {
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
                    let inside = match self.sess.portal {
                        Side::Left => x,
                        Side::Right => w - x,
                        Side::Top => y,
                        Side::Bottom => h - y,
                    };
                    if !self.sess.edge_armed {
                        if inside > EDGE_REARM_DIST * scale {
                            self.sess.edge_armed = true;
                        }
                        self.sess.edge_hold_ms = 0;
                        self.sess.out_push = 0;
                    } else if event::hit_edge(self.sess.portal, x, y, w, h) {
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
                        let outward = match self.sess.portal {
                            Side::Left => -dx,
                            Side::Right => dx,
                            Side::Top => -dy,
                            Side::Bottom => dy,
                        };
                        if outward > 0 {
                            self.sess.out_push += outward;
                        }
                        let now = now_ms();
                        if self.sess.edge_hold_ms == 0 {
                            self.sess.edge_hold_ms = now;
                        }
                        if self.sess.out_push as f64 >= RETURN_PUSH * scale
                            // 进门 0.5 秒内不算：手还在门口惯性里
                            && now.saturating_sub(self.sess.enter_ms) >= 500
                            // 交还后 2 秒内不重复交还：打断
                            // 门口弹跳循环（见变量注释）
                            && now.saturating_sub(self.sess.last_giveback_ms) >= 2000
                        {
                            let held = now.saturating_sub(self.sess.edge_hold_ms);
                            let pushed = self.sess.out_push;
                            self.sess.edge_hold_ms = 0;
                            self.sess.out_push = 0;
                            self.sess.last_giveback_ms = now;
                            release_local(inject, &mut self.sess.pressed);
                            // 单发 UDP 会丢：丢一包对端就卡在 Sending
                            // 连发两笔，接收端有状态闸门，幂等。
                            // **显式寻址到这台**：交还是还给
                            // 正被释放的那条链路（I4）。
                            self.out.to_link(
                                from_link,
                                &Event::Takeback {
                                    along: self.sess.portal.along(x, y, w, h),
                                },
                            );
                            self.out.to_link(
                                from_link,
                                &Event::Takeback {
                                    along: self.sess.portal.along(x, y, w, h),
                                },
                            );
                            self.state.store(State::Idle as u8, Ordering::SeqCst);
                            // I2：拆场先改 state 再 clear active
                            self.active_rx.set(None);
                            self.reclaim_rx.store(now_ms(), Ordering::SeqCst);
                            // 数字落日志：位置（门口=0）+ 账本位置
                            //（两者差得远 = 有外力直接挪真实光标，
                            // 而不是注入的负位移流）
                            let ledger = inject
                                .tracked()
                                .map(|(lx, ly)| format!("{lx:.0},{ly:.0}"))
                                .unwrap_or_else(|| "?".to_string());
                            (self.log)(format!(
                                "推门交还（真实 {x:.0},{y:.0} / 账本 {ledger}，推了 {pushed} 点 {held}ms，进门 {:.1}s）",
                                now.saturating_sub(self.sess.enter_ms) as f64 / 1000.0,
                            ));
                        }
                    } else {
                        // 离开门边：累计位移与计时都清零 —— 攒的是
                        // "这一顶"的量，不是历史总量
                        self.sess.edge_hold_ms = 0;
                        self.sess.out_push = 0;
                    }
                }
            }
        }
    }
}
