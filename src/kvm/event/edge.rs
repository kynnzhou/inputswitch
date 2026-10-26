//! `Side` 与边界几何：光标撞到哪条边、角落死区、门的撤防圈。
//!
//! 三个公开判定（hit_edge / hit_edge_in / within_edge）共用私有的
//! `dist_to`；两个魔数（MARGIN / CORNER）都有实测来由，注释里写着，
//! 别随手改。几何测试跟着被测代码住在本文件。

/// 屏幕上的一侧。多屏共享模式下，鼠标撞到这一侧就切到隔壁那台。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Side {
    #[default]
    Right,
    Left,
    Top,
    Bottom,
}

impl Side {
    /// 协议里编码用的标记：旧版对端不带你想要的这一位
    pub const CODE_UNKNOWN: u8 = 0xFF;

    pub fn code(&self) -> u8 {
        match self {
            Side::Right => 0,
            Side::Left => 1,
            Side::Top => 2,
            Side::Bottom => 3,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Side::Right),
            1 => Some(Side::Left),
            2 => Some(Side::Top),
            3 => Some(Side::Bottom),
            _ => None,
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "right" | "右" => Some(Side::Right),
            "left" | "左" => Some(Side::Left),
            "top" | "上" => Some(Side::Top),
            "bottom" | "下" => Some(Side::Bottom),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Side::Right => "right",
            Side::Left => "left",
            Side::Top => "top",
            Side::Bottom => "bottom",
        }
    }

    pub fn opposite(&self) -> Self {
        match self {
            Side::Right => Side::Left,
            Side::Left => Side::Right,
            Side::Top => Side::Bottom,
            Side::Bottom => Side::Top,
        }
    }

    /// 四向掩码里的一位。一机配多台时，平台层拿到的不是"我这条边通向谁"，
    /// 而是"哪几条边真的有邻居"的掩码 —— 没有邻居的边连判定都不该做，
    /// 否则光标撞上去会发一个没人接的 Begin。
    pub fn bit(&self) -> u8 {
        1 << self.code()
    }

    /// 掩码里恰好只有一位时取出那条边。多位/零位都返回 None ——
    /// 调用方要的是"唯一那条边"，含糊的值不能猜。
    pub fn from_bit(mask: u8) -> Option<Self> {
        if mask.count_ones() != 1 {
            return None;
        }
        Self::from_code(mask.trailing_zeros() as u8)
    }

    /// 掩码里编码最小的那条边。只用于"挑一个代表"的场合（比如几何兜底
    /// 猜本机工作屏），不用于路由判定。
    pub fn first_in(mask: u8) -> Option<Self> {
        Self::all().into_iter().find(|s| mask & s.bit() != 0)
    }

    pub fn all() -> [Side; 4] {
        [Side::Right, Side::Left, Side::Top, Side::Bottom]
    }

    /// 出门瞬间沿门轴的位置比例（0..65535）：左右门看纵坐标，
    /// 上下门看横坐标。对端据此从对应高度/位置进门，不居中猛跳。
    pub fn along(&self, x: f64, y: f64, w: f64, h: f64) -> u16 {
        let f = match self {
            Side::Right | Side::Left => y / h,
            Side::Top | Side::Bottom => x / w,
        };
        (f * 65535.0).clamp(0.0, 65535.0) as u16
    }
}

/// 鼠标撞到哪一侧了。x/y 是当前光标位置，w/h 是屏幕尺寸，
/// 判定用"离边缘不超过 margin 点"：太严会滑不出去，太松会误触发。
pub fn hit_edge(side: Side, x: f64, y: f64, w: f64, h: f64) -> bool {
    dist_to(side, x, y, w, h) <= MARGIN
}

/// 撞边的判定余量（点）。1.5 是历史值，动它会让"滑不出去 / 误触发"
/// 两种投诉同时出现，别随手改。
const MARGIN: f64 = 1.5;

/// 角落死区（点）。两条互相垂直的边**都通向邻居**时，靠近角落的这一圈
/// 不切换 —— 斜着推向角落时用户意图不明，硬挑一条会让他"想往上却去了右"。
/// 只有一条边有邻居时这个死区不生效：那时方向是确定的，不该刁难用户。
const CORNER: f64 = 24.0;

fn dist_to(side: Side, x: f64, y: f64, w: f64, h: f64) -> f64 {
    match side {
        Side::Right => w - x,
        Side::Left => x,
        Side::Top => y,
        Side::Bottom => h - y,
    }
}

/// 在一组"通向邻居的边"（掩码）里，看光标撞上了哪一条。
///
/// 单条边时与 `hit_edge` 完全等价（掩码只有一位 → 垂直线都不在掩码里 →
/// 角落死区永不生效），所以两机场景的行为一个字节都没变。
/// 多条边时取**最近的那条**；两条垂直边同时都在附近则判为角落，不切换。
pub fn hit_edge_in(mask: u8, x: f64, y: f64, w: f64, h: f64) -> Option<Side> {
    let mut best: Option<(f64, Side)> = None;
    for side in Side::all() {
        if mask & side.bit() == 0 {
            continue;
        }
        let d = dist_to(side, x, y, w, h);
        if d > MARGIN {
            continue;
        }
        if best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, side));
        }
    }
    let (_, side) = best?;
    // 角落死区：只看"垂直方向上那两条边"，且只有它们也通向邻居时才算
    // 歧义 —— 掩码单边时这里恒为 false。
    let near = |s: Side| mask & s.bit() != 0 && dist_to(s, x, y, w, h) <= CORNER;
    let ambiguous = match side {
        Side::Right | Side::Left => near(Side::Top) || near(Side::Bottom),
        Side::Top | Side::Bottom => near(Side::Left) || near(Side::Right),
    };
    if ambiguous {
        None
    } else {
        Some(side)
    }
}

/// 门重新武装所需的"离开边缘"距离（点）。
///
/// 必须**明显大于**交还落点到边缘的距离（16 点），否则落点本身就算
/// "已经离开门口"，撤防立刻失效 —— 那正是 v0.6.49 实测的来回弹：
/// 被对端物理抢回后，本机光标被摆在边缘内侧 16 点，钩子一看到"还在
/// 边上一圈"就再次越界，用户体感是"按了键没用，鼠标又跑回去了"。
pub const REARM_DIST: f64 = 48.0;

/// 光标是否离**某条通向邻居的边**不超过 `dist` 点。
///
/// 和 `hit_edge_in` 的区别：这个只回答"在不在门口这一圈里"，不判角落、
/// 不挑边。用来做门的撤防/武装：交还之后光标就落在门口那一圈里，此时
/// 门必须保持撤防，直到用户把光标真正带回屏幕内部（≥ `REARM_DIST`）。
pub fn within_edge(mask: u8, x: f64, y: f64, w: f64, h: f64, dist: f64) -> bool {
    Side::all()
        .into_iter()
        .any(|s| mask & s.bit() != 0 && dist_to(s, x, y, w, h) <= dist)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_detection() {
        assert!(hit_edge(Side::Right, 1728.0, 500.0, 1728.0, 1117.0));
        assert!(!hit_edge(Side::Right, 1700.0, 500.0, 1728.0, 1117.0));
        assert!(hit_edge(Side::Left, 0.0, 500.0, 1728.0, 1117.0));
        assert!(hit_edge(Side::Top, 300.0, 1.0, 1728.0, 1117.0));
        assert!(hit_edge(Side::Bottom, 300.0, 1116.5, 1728.0, 1117.0));
        assert_eq!(Side::Right.opposite(), Side::Left);
        assert_eq!(Side::parse("下"), Some(Side::Bottom));
    }

    #[test]
    fn edge_rearm_zone_covers_landing_spot() {
        // 交还落点 = 离边 16 点。它必须落在"还没武装"的范围内，
        // 否则放开抓取的那一刻门就是开的，手一抖就又被推出去
        //（v0.6.49 实测：抢回后光标被弹回去，看起来像抢回没生效）
        let (w, h) = (1728.0, 1117.0);
        let right = Side::Right.bit();
        assert!(within_edge(right, w - 16.0, 500.0, w, h, REARM_DIST));
        // 出门必须比落点走得远得多才重新武装
        assert!(!within_edge(right, w - 60.0, 500.0, w, h, REARM_DIST));
        // 只看掩码里的边：掩码只有右边时，光标贴在左边不算"在门口"
        assert!(!within_edge(right, 16.0, 500.0, w, h, REARM_DIST));
        // 空掩码（跟随画面模式）任何位置都不算
        assert!(!within_edge(0, w - 1.0, 500.0, w, h, REARM_DIST));
        // 四向齐全时，每条边的落点都在圈里
        let all = Side::all().iter().fold(0u8, |m, s| m | s.bit());
        for (x, y) in [
            (w - 16.0, 500.0),
            (16.0, 500.0),
            (500.0, 16.0),
            (500.0, h - 16.0),
        ] {
            assert!(within_edge(all, x, y, w, h, REARM_DIST), "({x},{y}) 没被算在门口");
        }
    }

    #[test]
    fn edge_mask_matches_single_side() {
        // 掩码只有一位时，判定结果必须与老的 hit_edge 完全一致 ——
        // 两机场景（P3 上线时的全部用户）行为不许有任何变化
        for side in Side::all() {
            let mask = side.bit();
            for (x, y) in [
                (0.0, 0.0),
                (1728.0, 0.0),
                (0.0, 1117.0),
                (1728.0, 1117.0),
                (500.0, 500.0),
                (1727.0, 500.0),
                (1.0, 500.0),
                (500.0, 1.0),
                (500.0, 1116.0),
                (1727.0, 1116.5),
            ] {
                let want = if hit_edge(side, x, y, 1728.0, 1117.0) {
                    Some(side)
                } else {
                    None
                };
                assert_eq!(
                    hit_edge_in(mask, x, y, 1728.0, 1117.0),
                    want,
                    "单边掩码 {side:?} 在 ({x},{y}) 上判定变了"
                );
            }
        }
    }

    #[test]
    fn edge_mask_picks_nearest_and_skips_corners() {
        // 右边 + 下边都通向邻居：右下角 24 点内算歧义，不切换
        let mask = Side::Right.bit() | Side::Bottom.bit();
        assert_eq!(hit_edge_in(mask, 1727.0, 1116.0, 1728.0, 1117.0), None);
        // 离右边 100 多点、只碰到下边 → 不在角落，正常从下边出
        assert_eq!(
            hit_edge_in(mask, 1620.0, 1116.0, 1728.0, 1117.0),
            Some(Side::Bottom)
        );
        // 离开角落（纵向离底边 30 点）→ 右边照常
        assert_eq!(
            hit_edge_in(mask, 1727.0, 1087.0, 1728.0, 1117.0),
            Some(Side::Right)
        );
        // 离开角落（横向离右边 30 点）→ 下边照常
        assert_eq!(
            hit_edge_in(mask, 1698.0, 1116.0, 1728.0, 1117.0),
            Some(Side::Bottom)
        );
        // 没有邻居的边不参与判定
        assert_eq!(hit_edge_in(Side::Left.bit(), 1727.0, 500.0, 1728.0, 1117.0), None);
        assert_eq!(hit_edge_in(0, 0.0, 0.0, 1728.0, 1117.0), None);
        assert_eq!(Side::from_bit(Side::Top.bit()), Some(Side::Top));
        assert_eq!(Side::from_bit(mask), None, "两位不算唯一");
        assert_eq!(Side::first_in(mask), Some(Side::Right));
        // along：左右门看 y、上下门看 x
        assert_eq!(Side::Right.along(1728.0, 0.0, 1728.0, 1117.0), 0);
        assert_eq!(Side::Bottom.along(0.0, 1117.0, 1728.0, 1117.0), 0);
        assert!(Side::Left.along(0.0, 1117.0 / 2.0, 1728.0, 1117.0) > 32000);
    }
}
