//! 被控期间物理抢回：`Captured::Claim`（本机用户真的动了物理键鼠）。
//!
//! 判据全在平台层：注入来的事件在钩子入口就带标记被放行了，能走到
//! 这里的按下一定是人按的。这里只做时间窗与去抖 —— 进入被控后的
//! 1.5 秒（KVM 切输入源时显示器重新握手，键盘常吐一串 ghost key）
//! 与认过之后的 1.5 秒（连按两下不连发两次 Takeback）都不算。

use std::sync::atomic::Ordering;

use crate::kvm::state::State;
use crate::kvm::tuning::CLAIM_ARM_MS;
use crate::kvm::util::now_ms;

use super::Ctx;

impl Ctx {
    pub(super) fn on_claim(&mut self) {
        // 被控期间本机用户真的动了物理键鼠。
        //
        // 为什么这个信号必须由平台层给：注入来的事件在钩子
        // 入口就带标记被放行了（Win 的 `INJECT_TAG` / Mac 的
        // `FIELD_SOURCE_USER_DATA`），所以能走到这里的按下
        // 一定是人按的。**不能**拿 Raw Input 的位移当判据 ——
        // 那是设备增量流、不过钩子，对端注入的位移会被当成
        // 本机用户在动，立刻 Takeback → 对端再注入 → 自激。
        if self.state.load(Ordering::Relaxed) != State::Receiving as u8 {
            return;
        }
        let now = now_ms();
        // 保护窗两头都要：进入被控后的 1.5 秒（KVM 切输入源
        // 时显示器重新握手，键盘常吐一串 ghost key），以及
        // 认过之后的 1.5 秒（连按两下不连发两次 Takeback）。
        if now.saturating_sub(self.recv_cap.load(Ordering::Relaxed)) < CLAIM_ARM_MS
            || now.saturating_sub(self.last_claim_ms) < CLAIM_ARM_MS
        {
            return;
        }
        self.last_claim_ms = now;
        self.take_back_here("本机被物理键鼠操作");
    }
}
