//! 状态查询助手：身份键、状态文案、本机占口同步、体检结论。
//!
//! 四个都是 `pub(super)` 自由函数（不是 `Ctx` 的方法）—— `crate::tray::switch`
//! 的 `do_switch` 也要用 `sync_own`，方法化反而要把 `Ctx` 暴露过去。
//! 从 `engine.rs` 原样搬来，一行未改。

use crate::kvm;
use crate::models::{VCP_BRIGHTNESS, VCP_INPUT_SOURCE};
use crate::peer::Mesh;
use crate::Switcher;

use crate::tray::kvm_now;

/// 当前显示器的身份键；屏幕已经切走时为空串。
pub(super) fn profile_key_of(switcher: &Switcher) -> String {
    switcher
        .pick_display(None)
        .map(|d| switcher.identity(&d))
        .unwrap_or_default()
}

pub(super) fn status_text(switcher: &mut Switcher) -> String {
    let displays = switcher.displays();
    if displays.is_empty() {
        return "本机没有接通的显示器（画面在对面那台）".to_string();
    }
    let display = &displays[0];
    // 静默期（显示器刚切回来）用记忆里的口拼状态，绝不再发 DDC 读
    if switcher.ddc_quiet() {
        // 走 label_for：`own_input` 是配置键（`usbc` / `hdmi1`），直接印出来
        // 就是「P275MV PLUS · usbc」——托盘第一行最显眼的位置放一个内部标识，
        // 用户既看不懂也不知道跟下面那行「USB-C」是同一个口。
        let known = if switcher.config.own_input.is_empty() {
            format!("输入源 0x{:02X}", switcher.config.own_value)
        } else {
            switcher.config.label_for(&switcher.config.own_input)
        };
        return format!("{} · {}", display.name, known);
    }
    let cap = switcher.capability(display);
    // `current_input_name` 读到的是**配置键**（`usbc`）或者查不到时的
    // `输入源 0x12`。两者都过一遍 label_for：键换成口名，查不到的原样返回。
    // 托盘第一行是用户看得最多的一行，不该出现内部标识。
    let known = switcher
        .current_input_name(display)
        .map(|name| switcher.config.label_for(&name))
        .unwrap_or_else(|| "输入源未知".to_string());
    let read_ok = switcher
        .backend()
        .get_vcp(display, VCP_BRIGHTNESS)
        .is_some();
    if read_ok {
        format!("{} · {}", display.name, known)
    } else {
        format!(
            "{} · {} {}",
            display.name,
            cap.mark(),
            "只能切，读不到当前输入"
        )
    }
}

/// 把"本机现在占哪个口"同步给局域网广播。
///
/// 只有屏幕在本机时才读得到 VCP 0x60；读不到就保持上一次的值，
/// 对面仍然能看到"这台机器上次占的是哪个口"。
///
/// 返回本次读到的当前输入值（读不到返回 None）。不管有没有落盘，
/// 调用方都可以拿它判断"画面现在在哪"。
pub(super) fn sync_own(switcher: &mut Switcher, mesh: &Mesh) -> Option<u16> {
    // 静默期（显示器刚离开/回来）不读 DDC，保持上一次的值即可
    if switcher.ddc_quiet() {
        return None;
    }
    // 画面正显示对端时，0x60 读到的是**对端的口**。有的显示器切走后
    // 仍挂在本机拓扑里、DDC 照答不误（实测），照单全收会把对端的口记成
    // "本机的口"——之后对端请我们把画面切回去时，value != own 走错分支，
    // 键鼠收不回来（实测踩过）。两个信号任一命中就跳过：
    // 1) KVM 正在转发（键鼠已跟过去 = 画面在对端那边）
    // 2) 对端广播过它自己就挂在这个口
    let sending = kvm_now().is_some_and(|h| h.state() == kvm::State::Sending);
    let display = switcher.pick_display(None)?;
    let value = switcher.backend().get_vcp(&display, VCP_INPUT_SOURCE)?;
    let peer_port = mesh
        .live_peers()
        .iter()
        .any(|p| p.knows_port() && p.own_value == value);
    if !sending && !peer_port {
        let key = switcher.identity(&display);
        let name = switcher
            .config
            .input_name_of(&key, value)
            .unwrap_or_else(|| format!("0x{value:02X}"));
        // 顺手落盘：屏幕被切走之后就读不到了，这个值是请求对端切回来的唯一依据
        switcher.config.remember_own(&name, value);
        mesh.set_own(&name, value);
    }
    Some(value)
}

/// 体检结论：第一个字符串放菜单第二行，第二个是完整说明（进日志/状态栏）。
///
/// `support::check` 会做一次写探测，约 1 秒，所以调用方必须缓存结果，
/// 不能挂在每 5 秒一次的状态刷新里。
pub(super) fn inspect(switcher: &Switcher) -> (String, String) {
    let Some(display) = switcher.pick_display(None) else {
        return (String::new(), "屏幕不在本机，无法检测".to_string());
    };
    let caps = switcher.capabilities(&display);
    let report = crate::support::check(switcher.backend(), &display, caps);
    let verdict = report.verdict();

    let line = format!("{} {}", verdict.mark(), verdict.label());

    let mut detail = report.headline();
    if !verdict.advice().is_empty() {
        detail.push('。');
        detail.push_str(verdict.advice());
    }
    (line, detail)
}

// 这里曾经有 absorb_peer_ports：把对端广播的"我挂在某个口"直接写进本机
// 这块显示器的输入口表。
//
// v0.6.35 删掉。原因是它把**别台显示器的编号**搬了过来：VCP 0x60 的取值
// 是每台显示器固件自己定义的一套编号，对端在它那块屏上是 0x12，写进本机
// 这块屏就成了"0x12 有主机"——可本机这个口根本没接东西。用户看到的就是
// 一个绿点幽灵口，点「切换」直接黑屏。
//
// 「输入源」这个区域只回答一个问题：**这台显示器**的几个口各自接了什么。
// 答案只能来自本机的 DDC（自己读、自己切、自己扫）。对端那台机器接在
// 它自己的显示器上，它的口编号对本机没有任何意义，所以一条都不该进来。
//
// 对端的口仍然在广播里（KVM 的 sync_own 要用它防误记），只是不再变成
// 本机的输入源结论。
