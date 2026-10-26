//! 紧急收回：`Captured::End`（Ctrl+Alt+Esc）。
//!
//! Sending 时**先快照会话目标再清状态**：清空之后补的 up 全丢，
//! 对端会一直以为某个键还按着。先改状态再放抓取 —— 顺序反过来的话，
//! 心跳守卫可能正好读到"状态还是 Sending、抓取已放开"这个瞬时错位，
//! 把刚交还的光标又抓回来。Receiving 时按热键也要能收回：原来这里
//! 只处理 Sending，被控方按了悄无声息。

use std::sync::atomic::Ordering;

use crate::kvm::event::Event;
use crate::kvm::state::State;
use crate::kvm::util::up_of;

use super::Ctx;

impl Ctx {
    pub(super) fn on_end(&mut self) {
        // 紧急收回：先把按着的键补 up，再告诉对端松手
        let cur_end = self.state.load(Ordering::Relaxed);
        if cur_end == State::Receiving as u8 {
            // 被控时按热键也要能收回。原来这里只处理 Sending，
            // 被控方按 Ctrl+Alt+Esc 悄无声息 —— 用户唯一的
            // 出路是等对端松手，或者拔网线。
            self.take_back_here("键鼠已收回（Ctrl+Alt+Esc）");
        } else if cur_end == State::Sending as u8 {
            // **先快照会话目标**：清空之后补的 up 全丢，
            // 对端会一直以为某个键还按着。
            let target = self.active_cap.get();
            // 先改状态再放抓取：顺序反过来的话，心跳守卫
            // 可能正好读到"状态还是 Sending、抓取已放开"
            // 这个瞬时错位，把刚交还的光标又抓回来。
            self.state.store(State::Idle as u8, Ordering::SeqCst);
            self.capture_ref.set_grabbing(false);
            if let Some(t) = target {
                for ev in self.pressed.drain(..) {
                    self.out.to_link(t, &up_of(&ev));
                }
                self.out.to_link(t, &Event::Leave);
            }
            self.active_cap.set(None);
            self.root_cap.set(None);
            (self.log)("键鼠已收回（Ctrl+Alt+Esc）".to_string());
            // 屏幕还在对端的话，一起切回来
            if let Some(cb) = self.on_panic.as_ref() {
                cb();
            }
        }
    }
}
