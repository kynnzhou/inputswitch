//! 改指向：把正在进行的会话挪到另一台机器上。
//!
//! 从 `mod.rs` 拆出来，只装 [`Linker`] 一个类型。给它单开一个文件的理由：
//! `retarget()` 是整个 KVM 里**最不能出错的一个动作** —— 四个步骤的先后
//! 顺序各自防一个已实测的故障（见方法上的注释），而它有三个调用方
//! （抓取线程、接收线程、托盘/面板）。独立成文件让"改顺序之前先读这
//! 个文件"变成唯一入口。

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use super::event::Event;
use super::link::{enter_aim, link_alive_at};
use super::now_ms;
use super::plat;
use super::rx::RxTable;
use super::session::{Active, Out};
use super::state::State;
use super::Link;

/// 「把会话改指向另一台」要用到的那一小撮共享状态。
///
/// 抓取线程（撞边、热键）、接收线程（回程）、托盘/面板（点名改指向）三处
/// 都要做同一件事，而它们手里只有各自 clone 出来的 Arc。抽成这个借用
/// 结构体，是为了让**动作只有一份实现** —— 三份复制的下场是某一份忘了
/// 补 key-up 或者忘了堵 Takeback，症状还是"改个方向键鼠全没了"。
pub(super) struct Linker<'a> {
    pub(super) state: &'a AtomicU8,
    pub(super) out: &'a Out,
    pub(super) links: &'a [Link],
    pub(super) active: &'a Active,
    /// 本次会话的起点（多跳的回程目标）
    pub(super) root: &'a Active,
    pub(super) capture: Option<&'a Arc<plat::Capture>>,
    /// 刚改指向的目标与时刻（旧目标残包要忽略）
    pub(super) guard: &'a Mutex<Option<(usize, u64)>>,
    pub(super) hop_ms: &'a AtomicU64,
    pub(super) peer_rx: &'a RxTable,
    pub(super) log: &'a (dyn Fn(String) + Send + Sync),
}

impl Linker<'_> {
    pub(super) fn alive(&self, index: usize) -> bool {
        link_alive_at(self.peer_rx, self.links, index)
    }

    /// 把正在进行的会话改指向 `to`：**不重启、不重新抓取、不碰画面**。
    ///
    /// 顺序不能换（四个动作各自防一个已实测的故障）：
    /// 1. 登记 guard —— 旧目标手上有在途位移，会回一条 Takeback 解围；
    /// 2. 旧目标补 key-up —— 不补的话对面一直以为 Ctrl 还按着；
    /// 3. `Leave` 双发 —— 单发丢一包旧目标就卡在 Receiving；
    /// 4. `Enter` 双发 —— **早于切 active**：反过来的话，Enter 还没到就把
    ///    位移灌给一台还在 Idle 的机器，它会按救援逻辑回 Takeback。
    pub(super) fn retarget(&self, to: usize, along: u16, why: &str) -> Result<(), String> {
        if self.state.load(Ordering::Relaxed) != State::Sending as u8 {
            return Err("只有键鼠已经在本机对面时才谈得上改指向（现在空闲或被控）".to_string());
        }
        let Some(from) = self.active.get() else {
            return Err("当前没有会话目标".to_string());
        };
        if from == to {
            return Err("键鼠已经在那台".to_string());
        }
        let Some(link) = self.links.get(to) else {
            return Err("那条链路不在链路表里".to_string());
        };
        if !self.alive(to) {
            return Err(format!("{} 没有回应，键鼠还在原处", link.node));
        }
        // 起点只在第一跳时定：之后无论跳几次，回程都回这里
        if self.root.get().is_none() {
            self.root.set(Some(from));
        }
        if let Ok(mut g) = self.guard.lock() {
            *g = Some((from, now_ms()));
        }
        for code in plat::MODIFIER_KEYS {
            self.out
                .to_link(from, &Event::Key { code, down: false });
        }
        // **不补发 button-up。** 按下状态的清理是接收端自己的事：它的
        // `pressed` 里记着真正按下的键，收到 `Leave` 会逐笔补 up。这边
        // 盲发「1/2/3 全松开」会被对端当成**一次真实点击** —— Windows
        // 桌面上收到一个孤立的 WM_RBUTTONUP 就弹右键菜单（v0.6.62 用户
        // 实测：从 MC5K9SJ 转到 0DFF1JU 时，MC5K9SJ 上凭空弹一次右键）。
        // 键盘的 up 仍然补：多补一次无害，卡住一个键的代价大得多。
        self.out.to_link(from, &Event::Leave);
        self.out.to_link(from, &Event::Leave);
        let (sw, sh) = match self.capture {
            Some(c) => c.screen(),
            // 没有抓取模块时不可能在 Sending（上面已挡），这里只是兜底
            None => (0u32, 0u32),
        };
        let (enter_side, enter_flags) = enter_aim(self.links, from, to);
        let enter = Event::Enter {
            width: sw.min(65535) as u16,
            height: sh.min(65535) as u16,
            along,
            flags: enter_flags,
            side: enter_side,
        };
        self.out.to_link(to, &enter);
        self.out.to_link(to, &enter);
        // **最后**才切 active：上面几笔仍打在旧目标上（它刚收到 Leave，
        // 多出的几笔会被它当"没在被控却收到输入"丢掉），切完之后的流才
        // 打向新目标（它已经收到 Enter 了）。
        self.active.set(Some(to));
        self.hop_ms.store(now_ms(), Ordering::Relaxed);
        (self.log)(format!(
            "键鼠从 {} 转到 {}（本机直发，不经中转；{why}）",
            self.links[from].node, link.node
        ));
        Ok(())
    }
}
