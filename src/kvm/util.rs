//! 零散辅助函数：时间戳、按键栈清理、位移缩放、落点与锚点计算、平台尺寸收窄。
//!
//! 从 `mod.rs` 拆出来。共同点：**无状态、无依赖**（除了 `plat::Inject`
//! 的平台能力），被三个线程体和测试共用。放一起是为了让「改缩放公式」
//! 「改锚点阈值」这类改动只有一个落点。

use std::time::{SystemTime, UNIX_EPOCH};

use super::event::Side;
use super::plat;
use super::tuning::{ANCHOR_TOL, ENTRY_MARGIN};
use super::Event;

/// 当前毫秒时间戳。`pub(super)`：`rx.rs` 的心跳表也要用。
pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub(super) fn same_key(a: &Event, b: &Event) -> bool {
    match (a, b) {
        (Event::Key { code: x, .. }, Event::Key { code: y, .. }) => x == y,
        (Event::Button { button: x, .. }, Event::Button { button: y, .. }) => x == y,
        _ => false,
    }
}

/// 把一个"按下"事件翻成对应的"抬起"
pub(super) fn up_of(ev: &Event) -> Event {
    match *ev {
        Event::Key { code, .. } => Event::Key { code, down: false },
        Event::Button { button, .. } => Event::Button {
            button,
            down: false,
        },
        other => other,
    }
}

/// 本地这侧被控制结束：把注入过但没收 up 的键补上，免得本机卡键
pub(super) fn release_local(inject: &plat::Inject, pressed: &mut Vec<Event>) {
    for ev in pressed.drain(..) {
        inject.apply(&up_of(&ev));
    }
}

/// 按比例缩放单轴位移，保底 ±1：比例小于 1 时（大屏→小屏）很小的
/// delta 换算完会变成 0，直接丢掉的话光标就挪不动了。
pub(super) fn rescale_delta(v: i32, scale: f64) -> i32 {
    if v == 0 || scale <= 0.0 {
        return v;
    }
    let scaled = (v as f64 * scale).round() as i32;
    if scaled == 0 {
        v.signum()
    } else {
        scaled
    }
}

/// 进门落点：出门时光标沿门轴的位置比例，换算成对端进门边上的
/// 对应坐标（Deskflow 的 jump cursor + mapToFraction 思路 ——
/// 从哪个高度出去，就从哪个高度进来）。比例未知（0xFFFF，
/// 跟随画面模式没有"门"）时退回该轴居中。
pub(super) fn entry_spot(inject: &plat::Inject, side: Side, along: u16) -> (f64, f64) {
    let (w, h) = inject.screen();
    // 门口距离按屏幕宽度缩放：16 这个数是在 1080p 逻辑点上定的，
    // 4K 物理像素屏（3840 宽）上等效只有 8 逻辑点 —— 贴门太近，
    // 手一回拉就撞边。按宽度比例放大（4K = 32px）。
    let margin = ENTRY_MARGIN * (w / 1920.0).max(1.0);
    let f = if along == 0xFFFF {
        None
    } else {
        Some(along as f64 / 65535.0)
    };
    match side {
        Side::Left => (margin, f.map_or(h / 2.0, |f| (f * h).clamp(0.0, h))),
        Side::Right => (w - margin, f.map_or(h / 2.0, |f| (f * h).clamp(0.0, h))),
        Side::Top => (f.map_or(w / 2.0, |f| (f * w).clamp(0.0, w)), margin),
        Side::Bottom => (f.map_or(w / 2.0, |f| (f * w).clamp(0.0, w)), h - margin),
    }
}

/// 锚点校正的判定：该不该把光标摆到锚点上。
///
/// 返回 `Some((x, y))` = 偏差超过阈值，该摆；`None` = 在容差内，别动
/// （逐帧纠会让光标看起来在抖）。
///
/// `fx/fy == 0xFFFF` 表示这一维未知：该维不参与判定。
pub(super) fn anchor_target(
    fx: u16,
    fy: u16,
    w: f64,
    h: f64,
    cur: (f64, f64),
) -> Option<(f64, f64)> {
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let tx = if fx == 0xFFFF {
        cur.0
    } else {
        fx as f64 / 65535.0 * w
    };
    let ty = if fy == 0xFFFF {
        cur.1
    } else {
        fy as f64 / 65535.0 * h
    };
    // 阈值按宽度算（高度常常被任务栏/刘海吃掉一截，按宽度更稳），
    // 8 点是下限：小屏上 0.6% 只有几个像素，太敏感。
    let tol = (w * ANCHOR_TOL).max(8.0);
    if (cur.0 - tx).abs() > tol || (cur.1 - ty).abs() > tol {
        Some((tx, ty))
    } else {
        None
    }
}

/// 收窄成 u32。平台层两份 `screen_size` 的返回类型**故意**不一样：macOS
/// 那边 CGDisplayBounds 给的就是 f64 的点（与事件坐标同一套），Windows
/// 那边 GetSystemMetrics 给整数像素。调用处不想为此分平台写两遍。
///
/// 用 `impl Into<f64>` 同时接住两种类型，而不是在调用处直接 `as u32`：
/// 后者在 Windows 上会踩 clippy::unnecessary_cast（那边本来就是 u32）。
/// 屏幕尺寸远小于 2^24，f64 表示整数无损，round-trip 不掉精度。
pub(super) fn narrow(v: impl Into<f64>) -> u32 {
    v.into() as u32
}
