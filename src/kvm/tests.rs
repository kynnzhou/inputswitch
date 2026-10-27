//! KVM 模块的单元测试。
//!
//! 从 `mod.rs` 末尾拆出来（`#[cfg(test)] mod tests;` 挂回去）。测的全是
//! **不碰硬件的纯逻辑**：链路判据（`link.rs`）、状态转换（`state.rs`）、
//! 门口模拟（`session.rs::EdgeSim`）、锚点与缩放（`util.rs`）。三个线程体
//! （`capture/mod.rs` / `recv/mod.rs` / `watchdog.rs`）没有直接测 —— 它们的判据
//! 都抽成了上面这些自由函数，测函数等于测线程。
//!
//! 约定：测试必须单线程跑（`cargo test --lib -- --test-threads=1`）——
//! 并行会 SIGABRT，原因见仓库根的 CLAUDE.md。

use super::*;
// 调优常量只被测试直接引用（运行期路径全走各线程文件），故在这里导。
use super::tuning::*;
// 只在测试里用：bin 构建不碰 EdgeSim（线程体在 capture/）
use super::session::EdgeSim;

    #[test]
    fn state_roundtrip() {
        for s in [State::Idle, State::Sending, State::Receiving] {
            assert_eq!(State::from_u8(s as u8), s);
        }
    }

    #[test]
    fn role_parse() {
        assert_eq!(Role::parse("console"), Some(Role::Console));
        assert_eq!(Role::parse("本机"), Some(Role::Console));
        assert_eq!(Role::parse("remote"), Some(Role::Remote));
        assert_eq!(Role::parse("对端"), Some(Role::Remote));
        assert_eq!(Role::parse("nonsense"), None);
    }

    #[test]
    fn idle_timeout_is_sane() {
        // 必须明显大于心跳间隔，否则每个包都判超时
        assert!(IDLE_TIMEOUT_MS as u128 > PING_INTERVAL.as_millis() * 2);
        // 又不该长到用户以为程序死了
        const _: () = assert!(IDLE_TIMEOUT_MS <= 5000);
        // 判定"对端活着"的窗口必须比断线超时宽，否则会反复抖动
        const _: () = assert!(PEER_ALIVE_MS >= IDLE_TIMEOUT_MS);
    }

    fn link(ip: &str, side: Option<Side>, mode: Mode) -> Link {
        Link {
            node: ip.to_string(),
            ip: ip.parse().unwrap(),
            side,
            mode,
        }
    }

    /// 掩码只来自 Edge 链路：跟随画面那条不能占边。
    #[test]
    fn mask_skips_follow_links() {
        let l = vec![
            link("10.0.0.1", Some(Side::Right), Mode::Edge),
            link("10.0.0.2", Some(Side::Left), Mode::Follow),
        ];
        assert_eq!(link_mask(&l), Side::Right.bit());
    }

    /// 四向齐全 = 0b1111。
    #[test]
    fn mask_covers_four_sides() {
        let l: Vec<Link> = Side::all()
            .iter()
            .enumerate()
            .map(|(i, s)| link(&format!("10.0.0.{}", i + 1), Some(*s), Mode::Edge))
            .collect();
        assert_eq!(link_mask(&l), 0b1111);
        for s in Side::all() {
            assert_eq!(Side::from_bit(link_mask(&l) & s.bit()), Some(s));
        }
    }

    /// 同一台机器配了同一个方向两次：掩码不会多出一位，也不会消失。
    #[test]
    fn mask_dedupes_same_side() {
        let l = vec![
            link("10.0.0.1", Some(Side::Bottom), Mode::Edge),
            link("10.0.0.2", Some(Side::Bottom), Mode::Edge),
        ];
        assert_eq!(link_mask(&l), Side::Bottom.bit());
    }

    /// 会话目标之外的链路必须被告知"我空闲"，否则它会 2.5 秒后误判错位。
    #[test]
    fn state_is_projected_per_peer() {
        let st = AtomicU8::new(State::Sending as u8);
        assert_eq!(state_for(&st, Some(0), 0), State::Sending as u8);
        assert_eq!(state_for(&st, Some(0), 1), State::Idle as u8);
        let st = AtomicU8::new(State::Receiving as u8);
        assert_eq!(state_for(&st, None, 0), State::Idle as u8);
    }

    /// 会话目标的下标编码：0 = 没有会话，其余 = 下标 + 1。
    #[test]
    fn active_index_roundtrip() {
        let a = Active::new();
        assert_eq!(a.get(), None);
        for i in 0..4 {
            a.set(Some(i));
            assert_eq!(a.get(), Some(i));
        }
        a.set(None);
        assert_eq!(a.get(), None);
    }

    /// 影子光标：一路往右推，累计够了才认"到门口了"，方向必须是右边。
    #[test]
    fn edge_sim_needs_accumulated_push() {
        let mut sim = EdgeSim::new();
        let mask = Side::Right.bit();
        let mut hit = None;
        for _ in 0..400 {
            hit = sim.feed(8, 0, 1920.0, 1080.0, mask, 10_000);
            if hit.is_some() {
                break;
            }
        }
        let (side, _along) = hit.expect("推到边上应该触发一次");
        assert_eq!(side, Side::Right);
    }

    /// 掩码里没有这条边 / 刚触发完还在门口 = 都不触发（防连跳）。
    #[test]
    fn edge_sim_respects_mask_and_throttle() {
        // 右边没有邻居：推多久都不动
        let mut sim = EdgeSim::new();
        for _ in 0..400 {
            assert!(sim.feed(8, 0, 1920.0, 1080.0, Side::Left.bit(), 10_000).is_none());
        }
        // 触发一次后手还在门口继续推（同一毫秒）= 不再触发，
        // 必须先离开门边重新武装
        let mut sim = EdgeSim::new();
        let mask = Side::Right.bit();
        let mut fired = 0;
        for i in 0..600 {
            if sim.feed(8, 0, 1920.0, 1080.0, mask, 10_000 + i).is_some() {
                fired += 1;
            }
        }
        assert_eq!(fired, 1, "一趟顶门只该跳一次");
    }

    /// 三条链路的真实拓扑：0 = 共用屏（follow）、1 = 右侧并排（edge）。
    fn three_way() -> Vec<Link> {
        vec![
            link("10.0.0.1", None, Mode::Follow),
            link("10.0.0.2", Some(Side::Right), Mode::Edge),
            link("10.0.0.3", Some(Side::Left), Mode::Edge),
        ]
    }

    /// 撞到的那条边通向的就是当前那台：不改指向（两人单链路的回归判据）。
    #[test]
    fn pick_retarget_skips_self() {
        let l = vec![
            link("10.0.0.1", Some(Side::Right), Mode::Edge),
            link("10.0.0.2", Some(Side::Left), Mode::Edge),
        ];
        let sending = State::Sending as u8;
        assert_eq!(pick_retarget(sending, Some(0), &l, Side::Right), None);
        assert_eq!(pick_retarget(sending, Some(1), &l, Side::Left), None);
        assert_eq!(pick_retarget(sending, Some(0), &l, Side::Left), Some(1));
    }

    /// 共用屏那台（follow）上撞右边缘 → 转去右侧那台。
    #[test]
    fn pick_retarget_from_follow_link() {
        let l = three_way();
        assert_eq!(
            pick_retarget(State::Sending as u8, Some(0), &l, Side::Right),
            Some(1)
        );
        // 那条边没有机器：不动作
        assert_eq!(
            pick_retarget(State::Sending as u8, Some(0), &l, Side::Bottom),
            None
        );
    }

    /// 只有"键鼠正在外面"才有改指向可言：空闲是正常越界、被控是抢回。
    #[test]
    fn pick_retarget_only_when_sending() {
        let l = three_way();
        for cur in [State::Idle as u8, State::Receiving as u8] {
            assert_eq!(pick_retarget(cur, Some(0), &l, Side::Right), None);
        }
    }

    /// 回程的门：回到 Edge 链路按它自己的 side；回到共用屏那台没有门。
    #[test]
    fn enter_aim_says_where_the_cursor_should_land() {
        let l = three_way();
        // 前进：目标是 Edge 链路，就用它自己那条边
        assert_eq!(enter_aim(&l, 0, 1), (Side::Right.code(), 0));
        assert_eq!(enter_aim(&l, 0, 2), (Side::Left.code(), 0));
        // 回程：从右边那台回到共用屏那台 → 落点得在**右边**。这里给的是
        // 反面（Left），因为接收端会把它取反成自己的门口 —— 给正面光标
        // 就跑到屏幕另一头去了。同时必须带 landing 标志，否则对端会把
        // 这次回程当成"这条链路有门"。
        assert_eq!(
            enter_aim(&l, 1, 0),
            (Side::Left.code(), ENTER_FLAG_LANDING_ONLY)
        );
        // 来路自己也没有门（两台共用屏互跳）：没有方向可依据，落点居中
        assert_eq!(enter_aim(&l, 0, 9), (Side::CODE_UNKNOWN, 0));
    }

    /// 锚点校正：容差内不动，超了才摆；未知维度不参与判定。
    #[test]
    fn anchor_target_only_corrects_real_drift() {
        let (w, h) = (1920.0, 1080.0);
        let center = (0x8000u16, 0x8000u16);
        // 偏 5 点：在容差（1920 × 0.006 ≈ 11.5）内，不许动 ——
        // 逐帧纠会让光标看起来在抖
        assert_eq!(
            anchor_target(center.0, center.1, w, h, (960.0 + 5.0, 540.0)),
            None
        );
        // 偏 40 点：明显是漂移，摆过去
        let t = anchor_target(center.0, center.1, w, h, (960.0 + 40.0, 540.0)).expect("该校正");
        assert!((t.0 - 960.0).abs() < 1.0 && (t.1 - 540.0).abs() < 1.0);
        // 两维都未知：等价于"没有锚点"，不校正
        assert_eq!(anchor_target(0xFFFF, 0xFFFF, w, h, (123.0, 100.0)), None);
        // 屏幕尺寸为 0（Windows 在画面切走时会报 0）：不校正，也别除零
        assert_eq!(anchor_target(center.0, center.1, 0.0, 0.0, (0.0, 0.0)), None);
    }

    /// 出门锚点必须落在**对面那一侧**：接收端从出门侧的对面进门，
    /// 锚点是它屏幕上的比例。写成出门侧本机比例时，Mac 右推出门
    /// 的第一颗锚点（fx=1.0）会把 Windows 上刚落在左缘的光标拽到
    /// 右缘 ——「鼠标一到对端就漂到出门方向那一侧」。
    #[test]
    fn anchor_at_entry_lands_on_the_opposite_side() {
        use super::event::Side;
        // 我从右出 ⇒ 对端左缘进门（x=0），出门高度比例直传
        assert_eq!(anchor_at_entry(Side::Right, 0.25), (0.0, 0.25));
        // 我从左出 ⇒ 对端右缘进门
        assert_eq!(anchor_at_entry(Side::Left, 0.75), (1.0, 0.75));
        // 上下门同理：门轴维（x）用出门比例，法轴维贴对面
        assert_eq!(anchor_at_entry(Side::Top, 0.4), (0.4, 1.0));
        assert_eq!(anchor_at_entry(Side::Bottom, 0.6), (0.6, 0.0));
    }

    /// 对面交还：起点不是它 → 改指向回起点；起点就是它 → 常规收回。
    #[test]
    fn takeback_returns_to_root() {
        let sending = State::Sending as u8;
        let now = 10_000;
        assert!(matches!(
            takeback_action(sending, Some(1), 1, None, now, Some(0)),
            TakebackAction::ToRoot(0)
        ));
        assert!(matches!(
            takeback_action(sending, Some(1), 1, None, now, Some(1)),
            TakebackAction::ToIdle
        ));
        assert!(matches!(
            takeback_action(sending, Some(1), 1, None, now, None),
            TakebackAction::ToIdle
        ));
        // 不是当前目标：一律不认
        assert!(matches!(
            takeback_action(sending, Some(0), 1, None, now, Some(0)),
            TakebackAction::Ignore
        ));
    }

    /// 刚改指向那一秒内，旧目标回来的 Takeback 是残包，不当真。
    #[test]
    fn takeback_guard_is_short_lived() {
        let sending = State::Sending as u8;
        let now = 10_000;
        let guard = Some((1usize, now - 200));
        assert!(matches!(
            takeback_action(sending, Some(1), 1, guard, now, Some(0)),
            TakebackAction::Ignore
        ));
        // 过了窗口就恢复正常
        let old = Some((1usize, now - RETARGET_GUARD_MS - 1));
        assert!(matches!(
            takeback_action(sending, Some(1), 1, old, now, Some(0)),
            TakebackAction::ToRoot(0)
        ));
        // 闸只对着登记的那台：别的机器不受影响
        let other = Some((0usize, now));
        assert!(matches!(
            takeback_action(sending, Some(1), 1, other, now, Some(0)),
            TakebackAction::ToRoot(0)
        ));
    }

    /// 对端的 `Enter` 带了出门侧 ⇒ 这条链路一定有门，本地配错了也算有。
    ///
    /// 这是 v0.6.61 那个"回不去上一台"的修复点：用户只在发送方那台摆过位，
    /// 接收方自己那份还是"共用一块屏"，按本地判会让推门交还整段不跑。
    #[test]
    fn peer_side_proves_there_is_a_door() {
        // 本地说没有门，但对端从右边出门 ⇒ 有门，且门口在左
        assert!(session_edge(false, Side::Right.code()));
        // 对端没带（Follow 恒发 UNKNOWN）⇒ 只能听本地的
        assert!(!session_edge(false, Side::CODE_UNKNOWN));
        // 本地本来就有门：带不带都算
        assert!(session_edge(true, Side::CODE_UNKNOWN));
        assert!(session_edge(true, Side::Left.code()));
    }

    /// "按回头的方向"才算回家；继续往外推仍报"这一侧没有机器"。
    #[test]
    fn homeward_is_the_opposite_of_the_door_i_came_through() {
        // 我是朝右侧出门到的这台 ⇒ 按左 = 回家，按右 = 继续往外
        assert!(is_homeward(Some(Side::Right), Side::Left));
        assert!(!is_homeward(Some(Side::Right), Side::Right));
        // 上下同理
        assert!(is_homeward(Some(Side::Top), Side::Bottom));
        // 共用一块屏那台没有门：没有"来路"可言，不做回家判断
        assert!(!is_homeward(None, Side::Left));
    }
