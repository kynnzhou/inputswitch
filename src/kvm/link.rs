//! 链路判据：一张链路表在手，能算出来的所有决定。
//!
//! 从 `mod.rs` 拆出来。这里全是**自由函数**—— 不吃共享状态、不碰网络，
//! 输入（`&[Link]`、当前状态字节、方向）到输出（掩码 / 下标 / 动作）一一
//! 对应，所以也是 `kvm` 里唯一带单元测试覆盖的一层（见 `mod.rs` 末尾
//! `mod tests`）。
//!
//! 三条判据（`link_mask` / `pick_retarget` / `link_alive_at`）各有三处以上
//! 调用方，原作者把它们抽成函数就是为了「面板、抓取线程、接收线程说的是
//! 同一回事」—— 拆文件时这个性质必须保住，所以全部 `pub(super)`，
//! 不新增任何包装。

use std::sync::atomic::{AtomicU8, Ordering};

use super::event::Side;
use super::rx::RxTable;
use super::state::{Mode, State};
use super::tuning::{ENTER_FLAG_LANDING_ONLY, PEER_ALIVE_MS, RETARGET_GUARD_MS};
use super::Link;

/// 本机状态在**某个具体对端**眼里的样子。
///
/// 我在跟 B 开会时，对 A 而言就该是 Idle。若照实把 `Sending` 报给 A，
/// A 那边期待的是 `Receiving`，2.5 秒后判"状态对不上（交还消息丢了）"
/// 并自愈 —— 它会把正在跟我开会的那条会话一起踢掉。多对端下必现，
/// 单对端下这个函数恒等于原值，行为不变。
pub(super) fn state_for(state: &AtomicU8, active: Option<usize>, from: usize) -> u8 {
    if active == Some(from) {
        state.load(Ordering::Relaxed)
    } else {
        State::Idle as u8
    }
}

/// 四向掩码只来自 **Edge 链路**。
///
/// Follow 链路（"和本机共用一块屏"的那台）不能占边：它靠画面切换触发，
/// 一旦混进掩码，用户朝那侧推一下就会在画面没切走的情况下被吞掉键鼠。
/// 同一条边配了两条链路时按位或天然合并 —— 具体走哪条由"撞到的那条边"
/// 决定，这是掩码该有的语义。
pub(super) fn link_mask(links: &[Link]) -> u8 {
    links
        .iter()
        .filter(|l| l.mode == Mode::Edge)
        .filter_map(|l| l.side)
        .fold(0u8, |m, s| m | s.bit())
}

/// 第 `index` 条链路对端活不活。
///
/// 抽成自由函数是因为三处都要同一份判据：`Kvm::link_alive`（面板/托盘）、
/// 抓取线程（撞边改指向）、接收线程（回程改指向）。三份各写一遍的下场是
/// "面板说在线、撞边说不在线"。
pub(super) fn link_alive_at(peer_rx: &RxTable, links: &[Link], index: usize) -> bool {
    let Some(link) = links.get(index) else {
        return false;
    };
    let (idle, seen) = peer_rx.alive(link.ip);
    seen && idle <= PEER_ALIVE_MS
}

/// 撞到 `side` 这条边时，该把键鼠改指向谁。`None` = 不动作。
///
/// **查到的就是当前会话目标时返回 `None`** —— 单条 Edge 链路的两人拓扑
/// 永远走这一条，行为与改动前逐字节一致（那时撞边只会打一句"忽略"）。
///
/// 只有 `Sending` 才有"改指向"可言：Idle 是正常越界（另有一套建场流程）、
/// Receiving 是抢回（目标必须是当前那台，按 side 查表会让两台同时注入）。
pub(super) fn pick_retarget(cur: u8, active: Option<usize>, links: &[Link], side: Side) -> Option<usize> {
    if cur != State::Sending as u8 {
        return None;
    }
    let to = links
        .iter()
        .position(|l| l.mode == Mode::Edge && l.side == Some(side))?;
    if Some(to) == active {
        None
    } else {
        Some(to)
    }
}

/// 回程 Enter 该带的方向字节（语义是"发送方从哪一侧出门"）。
///
/// 直接取目标链路自己的 `side`：门是**本机**通向那台的门。回到 Follow
/// 链路（共用一块屏那台）时它没有门，回 `CODE_UNKNOWN` —— 对端会沿用
/// 自己配置的方向，和 `follow()` 逐字一样。
/// 改指向时往 `Enter` 里塞的「进门侧」和标志位。
///
/// 两种情况完全不是一回事：
///
/// - 目标那台**有门**（Edge 链路）：就是那条边。对端据此把自己的门口
///   校正到我们的对面，光标落在那侧边缘。
/// - 目标那台**没有门**（Follow / 共用一块屏）：这是一次**回程**。拿
///   "来路那条边"的**反面**告诉它 —— `Enter.side` 的语义是"我方从哪侧
///   出门"，接收端会把门口取反，所以给反面才能让光标落回我们自己出去
///   的那一侧（给正面就成了屏幕另一头）。同时带上
///   `ENTER_FLAG_LANDING_ONLY`，否则对端会把这次回程当成"这条链路有
///   门"，从此开始判推门交还。
pub(super) fn enter_aim(links: &[Link], from: usize, to: usize) -> (u8, u16) {
    if let Some(s) = links.get(to).and_then(|l| l.side) {
        return (s.code(), 0);
    }
    match links.get(from).and_then(|l| l.side) {
        Some(s) => (s.opposite().code(), ENTER_FLAG_LANDING_ONLY),
        None => (Side::CODE_UNKNOWN, 0),
    }
}

/// 这场会话算不算"有门"。
///
/// 两条证据取或：本地配置说它是多屏共享，**或者**对端的 `Enter` 带了
/// 真实出门侧。后者才是权威 —— 门是发送方拓扑的一部分，只有 Edge 链路
/// 发得出 side（Follow 恒发 `CODE_UNKNOWN`）。
///
/// 这个"或"不是宽容，是**必需**：发送方改了摆位、接收方没改（用户很自然
/// 地只在正在用的那台上面拖一下），按本地判就会把一条有门的链路当成
/// 共用一块屏 —— 推门交还整段不跑，人从那台回不来。
pub(super) fn session_edge(local_edge: bool, peer_side: u8) -> bool {
    local_edge || Side::from_code(peer_side).is_some()
}

/// 用户按的那个方向，是不是"来路"。
///
/// 当前这台是我朝 `link_side` 出门才到的 ⇒ 按它的反面就等于想回去。
/// 反过来按同一个方向是"继续往外走"，那才该报"这一侧没有机器"。
pub(super) fn is_homeward(link_side: Option<Side>, pressed: Side) -> bool {
    link_side.is_some_and(|s| s.opposite() == pressed)
}

/// 收到 Takeback 该怎么处理。
///
/// 三种结局分开表态的意义：以前只有"收回本机"一条路，而键鼠进了多跳
/// 会话之后，"对面交还"往往意味着**回到起点那台**（画面还在起点那台上，
/// 收回本机等于键鼠看不见）。
pub(super) enum TakebackAction {
    /// 不认：不是当前目标，或刚改指向时旧目标的残包
    Ignore,
    /// 常规：收回本机
    ToIdle,
    /// 跳到这条链路（本次会话的起点）
    ToRoot(usize),
}

pub(super) fn takeback_action(
    state: u8,
    active: Option<usize>,
    from: usize,
    guard: Option<(usize, u64)>,
    now: u64,
    root: Option<usize>,
) -> TakebackAction {
    // 只认当前会话目标：正给 A 转发时收到 B 的 Takeback（上一场残包）
    // 就收回键鼠，等于被已结束的会话打断
    if active != Some(from) {
        return TakebackAction::Ignore;
    }
    if state != State::Sending as u8 {
        return TakebackAction::Ignore;
    }
    if let Some((who, at)) = guard {
        if who == from && now.saturating_sub(at) < RETARGET_GUARD_MS {
            return TakebackAction::Ignore;
        }
    }
    match root {
        // 起点就是这台：常规交还。`root` 只在多跳里有意义，
        // 单链路两人拓扑恒等于 active，永远走 ToIdle
        Some(r) if r != from => TakebackAction::ToRoot(r),
        _ => TakebackAction::ToIdle,
    }
}
