//! 维护类事件：Ping/Pong 心跳应答、Anchor 漂移校正。
//!
//! 三者都不改变控制权归属 —— Ping 必须回给来源（多对端下回错人等于
//! 让对端 2.5 秒后判它不理我）；Pong 顺手把对端状态写进心跳表；
//! Anchor 只纠长会话累积漂移，不参与正常移动。

use std::net::SocketAddr;
use std::sync::atomic::Ordering;

use super::Ctx;
use crate::kvm::event::Event;
use crate::kvm::link::state_for;
use crate::kvm::state::State;
use crate::kvm::util::{anchor_target, now_ms};

impl Ctx {
    pub(super) fn on_ping(&mut self, from: SocketAddr, from_link: usize) {
        // 心跳：last_rx 上面已经刷新了，Pong 里带上自己的状态 ——
        // 对端拿它做"状态错位自愈"的依据（见心跳线程）。
        // **必须回给来源**，不能走"当前会话目标"：多对端下 4 台
        // 都会发 Ping，回错人等于让对端 2.5 秒后判它不理我。
        // 状态也要按对端投影，理由见 state_for。
        let st = state_for(&self.state, self.active_rx.get(), from_link);
        self.out.to(from, &Event::Pong { state: st });
    }

    pub(super) fn on_pong(&mut self, from: SocketAddr, peer: u8) {
        self.peer_rx_rx.set_state(from.ip(), peer);
    }

    pub(super) fn on_anchor(&mut self, from_link: usize, fx: u16, fy: u16) {
        // 绝对锚点：只用来**纠漂移**，不参与正常移动。
        //
        // 传输还是相对位移（丢包只少走几像素），锚点每 400ms 来
        // 一发"发送端虚拟光标的比例"。偏差小于阈值就完全不管 ——
        // 逐帧纠会让光标看起来在抖；超过阈值（≈ 屏宽 0.6%）才
        // 直接摆过去，修的是长会话累积漂移和"切回来不在原位"。
        if self.state.load(Ordering::Relaxed) != State::Receiving as u8
            || self.active_rx.get() != Some(from_link)
        {
            return;
        }
        // 进门 300ms 内不纠：落点刚摆好，锚点还在路上，
        // 这时候纠等于跟自己的落点逻辑打架
        if now_ms().saturating_sub(self.sess.enter_ms) < 300 {
            return;
        }
        let Some((cx, cy)) = self.inject.cursor() else {
            return;
        };
        let (w, h) = self.inject.screen();
        if let Some((tx, ty)) = anchor_target(fx, fy, w, h, (cx, cy)) {
            self.inject.place_cursor(tx, ty);
            (self.log)(format!(
                "锚点校正：({cx:.0},{cy:.0}) → ({tx:.0},{ty:.0})"
            ));
        }
    }
}
