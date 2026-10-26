//! 会话中改指向 / 回起点：`Captured::Hop`（Ctrl+Alt+方向）。
//!
//! 三态表态：Idle 时不抢系统的「Ctrl+方向」；Receiving 时键鼠是
//! 别人的，本机没权力把它转走；只有 Sending 才认。按住不放会以键盘
//! 重复速率连发 keydown，要限流。那一侧没有邻居时先分清"想再找一个
//! 邻居"和"想回家"（当前这台正是我从 `side` 的反面出门才到的）——
//! 回家是**唯一不依赖对端配合**的回程手段：对端没升级、或它不判门，
//! 推边交还就发不出来，人至少还能按这个键回来。

use std::sync::atomic::Ordering;

use crate::kvm::event::Side;
use crate::kvm::link::{is_homeward, pick_retarget};
use crate::kvm::state::State;
use crate::kvm::tuning::HOP_THROTTLE_MS;
use crate::kvm::util::now_ms;

use super::Ctx;

impl Ctx {
    pub(super) fn on_hop(&mut self, side: Side) {
        // 会话中按 Ctrl+Alt+方向：把键鼠改指向那一侧的机器。
        //
        // 三态表态：Idle 时不抢系统的「Ctrl+方向」（那是
        // 切换桌面/翻页的常用键）；Receiving 时键鼠是别人的，
        // 本机没权力把它转走；只有 Sending 才认。
        let cur = self.state.load(Ordering::Relaxed);
        if cur != State::Sending as u8 {
            (self.log)(if cur == State::Receiving as u8 {
                "键鼠在对面手里，这个热键不生效（按 Ctrl+Alt+Esc 先收回）".to_string()
            } else {
                "Ctrl+Alt+方向 只在「键鼠已交给对端」时才生效".to_string()
            });
            return;
        }
        // 按住不放会以键盘重复速率连发 keydown：没有这道闸
        // 就是每 30 毫秒跳一次，最后一台完全不可预料
        if now_ms().saturating_sub(self.hop_cap.load(Ordering::Relaxed)) < HOP_THROTTLE_MS {
            return;
        }
        match pick_retarget(cur, self.active_cap.get(), &self.links_cap[..], side) {
            Some(t) => {
                if let Err(err) = self.linker().retarget(t, 0xFFFF, "热键改指向") {
                    (self.log)(err);
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
                    self.active_cap
                        .get()
                        .and_then(|i| self.links_cap.get(i))
                        .and_then(|l| l.side),
                    side,
                );
                if !homeward {
                    (self.log)(format!(
                        "{}边没有可以转过去的机器（摆位图里给它拖个方向先）",
                        match side {
                            Side::Left => "左",
                            Side::Right => "右",
                            Side::Top => "上",
                            Side::Bottom => "下",
                        }
                    ));
                    return;
                }
                match self.root_cap.get() {
                    Some(r) => {
                        if let Err(err) = self.linker().retarget(r, 0xFFFF, "热键回起点") {
                            (self.log)(format!("{err}；改回本机"));
                            self.take_back_here("热键回家");
                        }
                    }
                    // 这一场不是从别台跳过来的（空闲时直接越界
                    // 出去），没有"起点"可言 —— 收回本机
                    None => self.take_back_here("热键回家"),
                }
            }
        }
    }
}
