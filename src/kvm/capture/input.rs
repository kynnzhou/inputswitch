//! 转发：`Captured::Input`（位移/按键送给当前会话目标）。
//!
//! 顺带四件事：积分虚拟光标并按 `ANCHOR_MS` 补一个绝对锚点校漂移、
//! 整轮位移记账加前 30 笔采样（排障"推过去 1 秒就被弹回来"）、
//! 按键逐笔落日志、松手补 up。Follow 会话里还喂 `EdgeSim` 判
//! "用户想把光标顶过邻居那侧"（推边改指向）—— Edge 会话两块屏
//! 比例可能不同，一律不跑。

use std::sync::atomic::Ordering;

use crate::kvm::event::Event;
use crate::kvm::link::pick_retarget;
use crate::kvm::state::{Mode, State};
use crate::kvm::tuning::ANCHOR_MS;
use crate::kvm::util::{now_ms, same_key};

use super::Ctx;

impl Ctx {
    pub(super) fn on_input(&mut self, ev: Event) {
        if self.state.load(Ordering::Relaxed) != State::Sending as u8 {
            // 会话结束：锚点模型作废，下一场重新按落点初始化
            self.anchor_pos = None;
            return;
        }
        // 非 edge 起点（热键/面板切换）进来的会话：接收端是
        // **居中**落点，所以虚拟光标也从中心开始 —— 两边对齐
        // 的是接收端，不是本机光标。
        if self.anchor_pos.is_none() {
            self.anchor_pos = Some((0.5, 0.5));
            self.anchor_at = 0;
        }
        // 按键/键盘逐笔落日志：定位"自发右键"这类问题
        //（位移每秒几百笔，不打）
        if let Event::Button { button, down } = ev {
            (self.log)(format!(
                "转发按键：button={button} {}",
                if down { "按下" } else { "松开" }
            ));
        }
        self.out.send(&ev);
        self.forwarded += 1;
        // 积分虚拟光标，并按固定节奏补一发绝对锚点。
        // 只在位移上做：按键/滚轮不改位置。
        if let Event::Motion { dx, dy } = ev {
            let (aw, ah) = self.capture_ref.screen();
            if aw > 0 && ah > 0 {
                if let Some((ax, ay)) = self.anchor_pos {
                    self.anchor_pos = Some((
                        (ax + dx as f64 / aw as f64).clamp(0.0, 1.0),
                        (ay + dy as f64 / ah as f64).clamp(0.0, 1.0),
                    ));
                }
            }
            let now = now_ms();
            if now.saturating_sub(self.anchor_at) >= ANCHOR_MS {
                self.anchor_at = now;
                if let Some((ax, ay)) = self.anchor_pos {
                    self.out.send(&Event::Anchor {
                        fx: (ax * 65535.0).round().clamp(0.0, 65535.0) as u16,
                        fy: (ay * 65535.0).round().clamp(0.0, 65535.0) as u16,
                    });
                }
            }
        }
        if let Event::Motion { dx, dy } = ev {
            self.motions += 1;
            self.sum_dx += dx as i64;
            self.sum_dy += dy as i64;
            if dx < 0 {
                self.neg_dx_count += 1;
                self.neg_dx_sum += dx as i64;
            }
            self.min_dx = self.min_dx.min(dx);
            self.max_dx = self.max_dx.max(dx);
            // 首笔位移打点：位移值异常（0/超大）一眼可见
            if self.motions == 1 {
                (self.log)(format!("开始转发鼠标位移（首笔 dx={dx}, dy={dy}）"));
            }
            // 前 30 笔转发流采样：与 Windows 收到的流对比，
            // 定罪负位移是 Mac 发出的还是传输环节冒出来的
            if self.motions <= 30 {
                if self.motions == 1 {
                    self.fwd_sample.clear();
                }
                self.fwd_sample.push_str(&format!(" {dx},{dy}"));
                if self.motions == 30 {
                    (self.log)(format!("转发位移流（前 30 笔 dx,dy）：{}", self.fwd_sample));
                }
            }
            // ---- 会话里"推边改指向"（v0.6.60）----
            //
            // 只在**共用屏那台**上跑：它的画面和本机是同一块
            // 屏，两块屏尺寸比例一致，本地复刻光标位置才有
            // 意义。Edge 会话（两块屏并排）比例可能不同，
            // 一律不跑 —— 那边推边本来就是"交还"的语义。
            if self.push_hop && self.sides_mask != 0 {
                let act = self.active_cap.get();
                let follow = act
                    .and_then(|i| self.links_cap.get(i))
                    .map(|l| l.mode == Mode::Follow)
                    .unwrap_or(false);
                // 换过目标（含刚改指向）就复位：新的起点没有
                // "上一趟推了多少"可言
                if act != self.sim_active {
                    self.sim_active = act;
                    self.edge_sim.reset();
                }
                // 刚收回来的 600ms 里不判：收回时光标就摆在
                // 门口，手的惯性还在，一判就再出去
                let ready =
                    now_ms().saturating_sub(self.reclaim_cap.load(Ordering::Relaxed)) >= 600;
                if follow && ready {
                    let (w, h) = self.capture_ref.screen();
                    let hit =
                        self.edge_sim
                            .feed(dx, dy, w as f64, h as f64, self.sides_mask, now_ms());
                    if let Some((side, along)) = hit {
                        // 这条边没有别的机器：什么都不做，
                        // 手继续推也只是把对端光标顶在边上
                        if let Some(t) =
                            pick_retarget(State::Sending as u8, act, &self.links_cap[..], side)
                        {
                            if let Err(err) = self.linker().retarget(t, along, "推边改指向") {
                                (self.log)(err);
                            }
                        }
                    }
                }
            }
        }
        // 记住按下的键，松手时补 up
        if ev.is_down() {
            self.pressed.push(ev);
        } else {
            self.pressed.retain(|p| !same_key(p, &ev));
        }
    }
}
