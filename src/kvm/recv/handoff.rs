//! 交还：Leave（对端松手）与 Takeback（对端主动交还，含回程改指向）。
//!
//! Takeback 是两台机器键鼠不全卡死的保证：对面交还时必须真的把键鼠
//! 收回来，或者按回程逻辑把会话改指向起点那台。

use std::sync::atomic::Ordering;

use super::Ctx;
use crate::kvm::link::{takeback_action, TakebackAction};
use crate::kvm::linker::Linker;
use crate::kvm::state::State;
use crate::kvm::util::{now_ms, release_local};

impl Ctx {
    pub(super) fn on_leave(&mut self, from_link: usize) {
        // 只认当前会话目标：正被 A 控制时收到 B 的 Leave
        // （上一场的残包）不该动本机状态。
        if self.active_rx.get() != Some(from_link) {
            return;
        }
        // 对端松手了，把可能残留的按下状态清掉
        release_local(&self.inject, &mut self.sess.pressed);
        match State::from_u8(self.state.load(Ordering::Relaxed)) {
            State::Receiving => {
                self.state.store(State::Idle as u8, Ordering::SeqCst);
                // I2：拆场先改 state 再 clear active
                self.active_rx.set(None);
                // 事件计数是排障的关键证据：有数 = 传输通、注入环节有问题；
                // 为 0 = Enter 之后一条事件都没到（传输问题）
                if self.sess.injected > 0 {
                    (self.log)(format!(
                        "{} 已收回键鼠（本次注入 {} 笔事件）",
                        self.sess.node, self.sess.injected
                    ));
                } else {
                    (self.log)(format!(
                        "{} 已收回键鼠（⚠️ 本次一条事件都没收到，传输断了）",
                        self.sess.node
                    ));
                }
                self.sess.injected = 0;
            }
            State::Sending => {
                // 我们在转发时收到 Leave = 对端接受了我们的
                // 抢占（它推门被我们按下去了，主动让位）。
                // **保持 Sending** —— 真正的交还是 Takeback。
                (self.log)(format!("{} 已让位，控制权在我们这边", self.sess.node));
            }
            State::Idle => {}
        }
    }

    pub(super) fn on_takeback(&mut self, from_link: usize, along: u16) {
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
        let guard = self.guard_rx.lock().ok().and_then(|g| *g);
        match takeback_action(
            self.state.load(Ordering::Relaxed),
            self.active_rx.get(),
            from_link,
            guard,
            now,
            self.root_rx.get(),
        ) {
            TakebackAction::Ignore => return,
            TakebackAction::ToRoot(to) => {
                // 改指向回起点。失败（起点那台不在线、链路
                // 表变了）就老实收回本机 —— 那种情况下用户
                // 至少能用 Mac 自己的键盘把人叫回来。
                let back = Linker {
                    state: &self.state,
                    out: &self.out,
                    links: &self.links_rx[..],
                    active: &self.active_rx,
                    root: &self.root_rx,
                    capture: self.capture_rx.as_ref(),
                    guard: &self.guard_rx,
                    hop_ms: &self.hop_rx,
                    peer_rx: &self.peer_rx_rx,
                    log: &*self.log,
                }
                // 把对面交还时的光标位置一路带回去：起点那台
                // 按这个比例进门，光标就落在用户推出去的那个
                // 高度上，而不是屏幕正中。
                .retarget(to, along, "对面交还，回到起点");
                match back {
                    Ok(()) => return,
                    Err(err) => {
                        (self.log)(format!("{err}；改回本机"));
                    }
                }
            }
            TakebackAction::ToIdle => {}
        }
        if self.state.load(Ordering::Relaxed) == State::Sending as u8 {
            // 先改状态再放抓取：心跳守卫每秒查一次不变式，
            // 顺序反了就会撞见"还在 Sending、抓取已放开"
            // 的瞬时错位，反手再抓一次 —— 用户感知是交还
            // 之后光标还卡半秒（实测日志有这串）。
            self.state.store(State::Idle as u8, Ordering::SeqCst);
            if let Some(cap) = self.capture_rx.as_ref() {
                cap.set_grabbing(false);
            }
            // I2：拆场先改 state 再 clear active
            self.active_rx.set(None);
            self.root_rx.set(None);
            self.reclaim_rx.store(now_ms(), Ordering::SeqCst);
            (self.log)(format!(
                // **必须用 from_link**，不能用 `sess.node`：那个
                // 是启动时按 `peer_node` 建好的默认会话，只要
                // 本机没收到过 Enter 就一直是同一个名字。用它
                // 的结果是 0DFF1JU 交还也写成"MC5K9SJ 把控制权
                // 还回来了"（v0.6.62 排查时被这条日志带偏：
                // 明明会话目标是 0DFF1JU，日志却咬定 MC5K9SJ）。
                "{} 把控制权还回来了，键鼠已收回本机",
                self.links_rx[from_link].node
            ));
        }
    }
}
