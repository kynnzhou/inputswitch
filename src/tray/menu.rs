//! 托盘菜单：输入口列表、内容指纹、整棵菜单树。
//!
//! 三件事必须放在一起看：
//! 1. `PortRow`/`port_rows` —— 菜单列哪些口，判据是"有没有本机证据"；
//! 2. `menu_signature`/`refresh_menu`/`retire_menu` —— 内容没变就一个字节都不碰；
//! 3. `build_menu` —— 真正画出来。指纹算法和画法必须同步改，分开在两个
//!    文件里迟早出现"签名一样但内容变了"（菜单不刷新）这种最难查的 bug。

use std::cell::RefCell;
use std::sync::atomic::Ordering as AtomicOrdering;
use std::time::{Duration, Instant};

use tray_icon::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::TrayIcon;
use crate::config::Config;
use crate::kvm;
use crate::models::Presence;
use crate::peer::{Mesh, Peer};

use super::kvmctl::perms_ok;
use super::{kvm_now, HAS_DISPLAY, ID_PANEL, ID_PEER, ID_PEER_FILE, ID_PEER_HAND, ID_PEER_SIDE,
            ID_SWITCH, MENU_ASK_BACK, MENU_KVM, MENU_KVM_HOP, MENU_KVM_MODE, MENU_KVM_SIDE,
            MENU_QUIT, MENU_RESTART};

/// 一行菜单项：口的名字、菜单显示文本、VCP 取值、状态。
#[derive(Clone)]
pub(super) struct PortRow {
    /// 存进配置与命令里的名字
    pub(super) name: String,
    /// 菜单上显示的文字
    pub(super) label: String,
    pub(super) value: u16,
    pub(super) verified: bool,
    pub(super) current: bool,
    /// 这个口上有没有主机（只由本机实测得到）
    pub(super) presence: Presence,
}

impl PortRow {
    /// 这个口有没有任何"本机证据"。
    ///
    /// 有证据 = 本机真的切过去过（`verified`），或者切/扫出过结论
    /// （有信号、无信号、该口不存在）。都没有就只是固件报了个编码 ——
    /// 那种行不该出现在「输入源」里：它看起来像"本机的一个输入源"，
    /// 实际上可能连物理接口都没有（厂商常把同一块面板所有 SKU 的
    /// 编码都写进 `vcp(60(...))`）。
    pub(super) fn has_evidence(&self) -> bool {
        self.current || self.verified || self.presence != Presence::Unknown
    }
}

/// 菜单里的输入口列表完全来自这台显示器自己的 profile。
///
/// profile 的输入源由显示器自报的能力表（VCP 0xDF）生成，见
/// `Switcher::sync_profile`。所以换成任何显示器都能列出它自己的口，
/// 不需要改一行代码 —— 这就是"通用"的落点。
///
/// `include_unverified = false` 时滤掉没有任何本机证据的口（见
/// [`PortRow::has_evidence`]）。菜单和面板都走这条：这种行不是"本机的
/// 输入源"，只是固件报的编码，露出来就会误导。真要用到它们有两条正路 ——
/// 某个口接了新设备就扫一次（扫到即出现），整块换了显示器就重新检测
/// （拓扑变化时也会自动检测一次，见 `Command::Heartbeat`）。
pub(super) fn port_rows(
    config: &Config,
    profile_key: &str,
    current: Option<u16>,
    include_unverified: bool,
) -> Vec<PortRow> {
    let Some(profile) = config.profile_for(profile_key) else {
        return Vec::new();
    };
    let mut rows: Vec<PortRow> = profile
        .inputs
        .iter()
        .map(|src| PortRow {
            name: src.name.clone(),
            label: config.label_for(&src.name),
            value: src.value,
            verified: src.verified,
            current: current == Some(src.value),
            presence: src.presence,
        })
        .collect();
    if !include_unverified {
        rows.retain(PortRow::has_evidence);
    }
    rows.sort_by_key(|r| r.value);
    rows
}

/// 按名字在一台显示器的 profile 里查取值。
pub(super) fn configured_value(config: &Config, profile_key: &str, name: &str) -> Option<u16> {
    config.profile_for(profile_key)?.find(name).map(|s| s.value)
}

/// 本机当前占用的 VCP 取值，未知时为 None。
pub(super) fn own_value(mesh: &Mesh) -> Option<u16> {
    let (_, value) = mesh.own();
    if value > 0 {
        Some(value)
    } else {
        None
    }
}

/// 菜单内容的指纹。
///
/// 存在的理由只有一个：**macOS 上 `NSStatusItem.menu` 一旦被替换，
/// 已经展开的菜单会立刻收起来**。而每 5 秒一次的状态刷新原本无条件
/// 重建菜单，于是用户点开菜单后最多 5 秒它自己就没了 —— 这就是那个 bug。
///
/// 先把"菜单会长成什么样"压缩成一个字符串，和上次比；一样就完全不碰菜单。
pub(super) fn menu_signature(
    status: &str,
    config: &Config,
    peers: &[Peer],
    current: Option<u16>,
    profile_key: &str,
    support: &str,
) -> String {
    use std::fmt::Write as _;
    let mut sig = String::with_capacity(384);
    let _ = write!(sig, "{status}\u{1f}{profile_key}\u{1f}{support}\u{1e}");
    for row in port_rows(config, profile_key, current, false) {
        let _ = write!(
            sig,
            "{}|{}|{}|{}|{}|{:?}\u{1e}",
            row.name, row.label, row.value, row.verified, row.current, row.presence
        );
    }
    for peer in peers {
        let _ = write!(
            sig,
            "{}|{}|{}|{}\u{1e}",
            peer.node, peer.own_input, peer.own_value, peer.ddc_ok
        );
    }
    // 共用键鼠的开关、模式与越界方向（菜单里这三样会变）
    let _ = write!(
        sig,
        "kvm={}\u{1e}mode={}\u{1e}side={}",
        config.kvm.enabled, config.kvm.mode, config.kvm.side
    );
    // 链路表进指纹：v3 起每台对端有自己的一行方向/模式，托盘按对端展开子菜单，
    // 改了某台的 side 而不进指纹，菜单会一直显示旧值
    for link in &config.kvm.links {
        let _ = write!(
            sig,
            "\u{1e}link={}|{}|{}",
            link.node, link.side, link.mode
        );
    }
    // 键鼠现在在哪台手上 —— 子菜单标题上要标"键鼠在它那儿"
    let active = kvm_now().and_then(|h| h.active_node().map(str::to_string));
    let _ = write!(sig, "\u{1e}active={}", active.unwrap_or_default());
    // 权限也要进指纹：勾完以后菜单里那条"重启应用"得自己消失，
    // 不然用户会盯着一条已经无意义的提示
    let _ = write!(sig, "\u{1e}perms={}", perms_ok());
    // 画面在不在本机决定「切换输入源」下面写什么、以及要不要出现
    // 「请对面把画面切回本机」—— 它变了必须重画
    let _ = write!(
        sig,
        "\u{1e}display={}",
        HAS_DISPLAY.load(AtomicOrdering::SeqCst)
    );
    sig
}

// 退场菜单的养老院。
//
// `set_menu` 替换旧菜单时会 drop 旧的 `Menu`（连同全部 `MenuChild`），
// 但 AppKit 侧还握着旧 NSMenuItem 的引用 —— 点击分发在菜单收起**之后**
// 才执行（`_performPostTrackingDismissalActions`），这几十毫秒的窗口里
// 旧菜单项的原生对象若还活着而 Rust 侧已释放，点击就会摸到悬空指针，
// panic → abort（实测崩了三次，菜单点击时必现窗口）。
//
// 所以每次替换菜单前，把旧菜单的克隆（`Menu` 内部是 Rc，克隆共享底层
// 对象）搬进这里住 60 秒再退役：在途点击依然摸得到有效的 MenuChild，
// 只是事件带着旧 id 回来，按 id 分发的处理器会安全忽略或照常执行。
thread_local! {
    static RETIRED_MENUS: RefCell<Vec<(Instant, tray_icon::menu::Menu)>> = const { RefCell::new(Vec::new()) };
    /// 当前挂在托盘上的菜单的克隆。下次替换时它就是要被 drop 的那个，
    /// 提前把克隆送进养老院。
    static ACTIVE_MENU: RefCell<Option<tray_icon::menu::Menu>> = const { RefCell::new(None) };
}

pub(super) fn retire_menu(menu: tray_icon::menu::Menu) {
    RETIRED_MENUS.with(|cell| {
        let mut list = cell.borrow_mut();
        list.retain(|(t, _)| t.elapsed() < Duration::from_secs(60));
        list.push((Instant::now(), menu));
    });
}

/// 内容变了才重建菜单。返回 true 表示这次真的换了。
// 8 个入参就是"画一个托盘菜单需要的全部输入"（托盘句柄 + 缓存签名 +
// 六个数据源）。clippy 的阈值是 7，这里不拆结构体：`menu_signature` /
// `build_menu` 同样收这 6 个数据源，只给 refresh_menu 套一层壳反而让
// 三处签名对不齐；真要拆就三个一起拆，那是独立的改动，别顺手做。
#[allow(clippy::too_many_arguments)]
pub(super) fn refresh_menu(
    tray: &mut Option<TrayIcon>,
    sig: &mut String,
    status: &str,
    config: &Config,
    peers: &[Peer],
    current: Option<u16>,
    profile_key: &str,
    support: &str,
) -> bool {
    let now = menu_signature(status, config, peers, current, profile_key, support);
    if now == *sig {
        return false;
    }
    *sig = now;
    if let Some(tray) = tray.as_mut() {
        let menu = build_menu(status, config, peers, current, profile_key, support);
        // set_menu 会 drop 上一个菜单（连同全部 MenuChild），而 AppKit 可能
        // 还握着旧菜单项等分发在途的点击 —— 先把它的克隆送进养老院，
        // 让底层对象再活 60 秒
        ACTIVE_MENU.with(|cell| {
            if let Some(old) = cell.borrow_mut().take() {
                retire_menu(old);
            }
            *cell.borrow_mut() = Some(menu.clone());
        });
        // set_menu 三个平台都返回 ()，没有 Result 要吞，不要写 `let _ =`
        tray.set_menu(Some(Box::new(menu)));
    }
    true
}

/// 对端占用的那个口，给人看的写法。
///
/// 有名字就只写名字（`DP`）。十六进制是**它那块屏**的编号，和本机列表里的
/// 编号各算各的 —— 并排写出来（`DP · 0x10`）会被读成"本机 0x10 那个口"，
/// 而按那个值切本机的屏就是黑屏。只有它没报名字时才退回写编号。
pub(super) fn peer_port_label(config: &Config, peer: &Peer) -> String {
    if peer.own_input.is_empty() {
        format!("0x{:02X}", peer.own_value)
    } else {
        config.label_for(&peer.own_input)
    }
}

pub(super) fn build_menu(
    status: &str,
    config: &Config,
    peers: &[Peer],
    current: Option<u16>,
    profile_key: &str,
    support: &str,
) -> Menu {
    // 本机此刻有没有接通显示器。没有 = 画面在对面那台，这时菜单里
    // 「切换输入源」必然是空的（本机枚举不到这块屏），得换一条出路。
    let has_display = HAS_DISPLAY.load(AtomicOrdering::SeqCst);
    let menu = Menu::new();
    let _ = menu.append(&MenuItem::new(status, false, None));
    // 第二行只放「需要你知道的」：
    //   · ✓ 完整支持是默认状态，天天挂着没人看，还和"没有其他输入口"这类
    //     情况摆在一起时自相矛盾（能切，但没口可切）；
    //   · "3 个输入口，1 个有主机" 是真正有用的一眼信息，留在这一行。
    // 两件事同时成立时并排放，句子还是短的。
    {
        let mut parts: Vec<String> = Vec::new();
        if !support.is_empty() && !support.starts_with('✓') {
            parts.push(support.to_string());
        }
        if let Some(profile) = config.profile_for(profile_key) {
            let total = profile.inputs.len();
            let occupied = profile
                .inputs
                .iter()
                .filter(|s| s.presence == Presence::Occupied)
                .count();
            if total > 0 {
                parts.push(if occupied > 0 {
                    format!("{total} 个输入口 · {occupied} 个有主机")
                } else {
                    format!("{total} 个输入口")
                });
            }
        }
        if !parts.is_empty() {
            let _ = menu.append(&MenuItem::new(parts.join(" · "), false, None));
        }
    }

    // ---- 每台对端一个子菜单 ----
    //
    // 以前是顶层平铺「切到 X（口）」，另加一个全局四选一「对端在右侧 ✓/左侧/…」。
    // 一机四台时这两块都塌了：平铺的几行看不出哪一行属于哪台；而"对端在右侧"
    // 只对**一条链路**成立（v0.6.47 起每台对端各有一条边），全局只有一个值
    // 本身就是错的 —— 改一次会把四台的方向一起改掉。
    let active_node = kvm_now().and_then(|h| h.active_node().map(str::to_string));
    if config.kvm.links.is_empty() {
        // 还没配链路表（只切输入源、没开过共用键鼠）：保留老的平铺写法，
        // 切画面本来就不依赖共用键鼠，不能因为没配链路就没了入口
        let known: Vec<&Peer> = peers.iter().filter(|p| p.knows_port()).collect();
        if !known.is_empty() {
            let _ = menu.append(&PredefinedMenuItem::separator());
            for peer in known {
                // 口名只写名字，不并排写十六进制 —— 那个号是**它那块屏**的，
                // 和本机列表里的 0x10 不是一回事，摆在一起只会被当成同一个口。
                let port_label = peer_port_label(config, peer);
                let id = format!("{}{}:{}", ID_PEER, &peer.node, peer.own_value);
                let _ = menu.append(&MenuItem::with_id(
                    id,
                    format!("切到 {}（{port_label}）", peer.node),
                    true,
                    None,
                ));
            }
        }
    } else {
        let _ = menu.append(&PredefinedMenuItem::separator());
        for link in &config.kvm.links {
            let live = peers.iter().find(|p| p.node == link.node);
            let mut head = link.node.clone();
            if live.is_none() {
                head.push_str("（不在线）");
            } else if link.side.is_empty() {
                head.push_str(" · 共用一块屏");
            } else {
                head.push_str(&format!(" · 在{}侧", side_label(&link.side)));
            }
            if active_node.as_deref() == Some(link.node.as_str()) {
                head.push_str(" · 键鼠在它那儿");
            }
            let sub = Submenu::new(head, true);

            // 1) 切到它。它自己广播了占用的口，不用猜口名
            match live.filter(|p| p.knows_port()) {
                Some(peer) => {
                    let port_label = peer_port_label(config, peer);
                    let _ = sub.append(&MenuItem::with_id(
                        format!("{}{}:{}", ID_PEER, &peer.node, peer.own_value),
                        format!("切到它（{port_label}）"),
                        true,
                        None,
                    ));
                }
                None => {
                    let _ = sub.append(&MenuItem::new("它还没报告自己在哪个口", false, None));
                }
            }

            // 2) 交键鼠 / 发文件 —— 都点名到这一台。
            // v0.6.52 之前这两件事在多机时会"猜"，猜错的表现是"点了 A，东西到了 B"
            let _ = sub.append(&MenuItem::with_id(
                format!("{ID_PEER_HAND}{}", link.node),
                "把键鼠交给它",
                true,
                None,
            ));
            let _ = sub.append(&MenuItem::with_id(
                format!("{ID_PEER_FILE}{}", link.node),
                "发文件给它…",
                true,
                None,
            ));

            // 3) 它摆在本机的哪一侧 —— 每条链路各有一份，不再是全局一个值。
            //
            // 用原生勾选项而不是 "✓ " 前缀：前缀那一版靠全角空格对齐，
            // 换字体/换系统就歪，而且 macOS 的勾选列本来就在左边。
            let _ = sub.append(&PredefinedMenuItem::separator());
            let _ = sub.append(&MenuItem::new("它在本机的方位", false, None));
            for (value, label) in [
                ("right", "右侧"),
                ("left", "左侧"),
                ("top", "上方"),
                ("bottom", "下方"),
                ("", "共用一块屏（跟随画面）"),
            ] {
                // 空值走 `none`：`peer-side:<node>:` 尾部空串在 rsplit 里
                // 与"没带值"分不开，两种错法表现不同、排查很烦
                let id = if value.is_empty() {
                    format!("{ID_PEER_SIDE}{}:none", link.node)
                } else {
                    format!("{ID_PEER_SIDE}{}:{value}", link.node)
                };
                let _ = sub.append(&CheckMenuItem::with_id(
                    id,
                    label,
                    true,
                    link.side == value,
                    None,
                ));
            }
            let _ = menu.append(&sub);
        }
    }

    let _ = menu.append(&PredefinedMenuItem::separator());

    // 切换子菜单：口列表来自显示器自报的能力表，不是硬编码。
    //
    // 画面在对面那台时**整个子菜单都不建**。注意这不是"列表本来就空"——
    // `port_rows` 读的是配置里的 profile，它记着上次读到的那些口，画面切走
    // 之后照样在。照常列出来的话，每一项点下去都必然失败（本机没有 DDC
    // 通道），用户会以为程序坏了。那种情况下面单独给一条
    // 「请对面把画面切回本机」才是这条路唯一走得通的写法。
    let rows = if has_display {
        port_rows(config, profile_key, current, false)
    } else {
        Vec::new()
    };
    let switch_menu = Submenu::new("切换输入源", true);
    if has_display && rows.is_empty() {
        let _ = switch_menu.append(&MenuItem::new(
            "没读到输入口 · 到控制面板点「重新检测」",
            false,
            None,
        ));
    }
    for row in &rows {
        let id = format!("{}{}:{}", ID_SWITCH, &row.name, row.value);
        let mut tail = String::new();
        // **不再写「· 本机」**：勾选标记本身就是"当前口"，再写一遍是重复。
        // 有主机 / 无信号 / 该口不存在这三样才是真正要多说的一句。
        if row.presence != Presence::Occupied {
            tail.push_str(row.presence.mark());
        } else if !row.current {
            tail.push_str(" · 有主机");
        }
        if !row.verified && row.presence == Presence::Unknown {
            tail.push_str(" · 待确认");
        }
        // 十六进制只写一次：标签就是十六进制、或者根本没有名字时。
        // 有名字还并排写一遍，等于把内部编码摊在用户面前；而且两套编号
        // 各算各的（对端报的是它那块屏的号），摆在一起容易被当成同一个口。
        let named = !row.label.trim().is_empty() && row.label != row.name;
        let text = if named {
            format!("{}{tail}", row.label)
        } else {
            format!("0x{:02X}{tail}", row.value)
        };
        // 「该口不存在」不能点切换：固件不接受那个值，点了就是黑屏。
        // 保留这一行（它说明"这个口读过、但不通"），只是禁用 —— 直接藏掉
        // 会让人以为程序没检测到那个口，反而会去反复重新检测。
        let dead = row.presence == Presence::Invalid;
        let _ = switch_menu.append(&CheckMenuItem::with_id(
            id,
            text,
            !dead,
            row.current,
            None,
        ));
    }
    // 没有扫描过就提示一句，别让人以为"没有主机"是本来就有的信息
    if rows
        .iter()
        .any(|r| !r.current && r.presence == Presence::Unknown)
    {
        let _ = switch_menu.append(&PredefinedMenuItem::separator());
        let _ = switch_menu.append(&MenuItem::new(
            "哪个口接了主机：到控制面板点「扫描输入口」",
            false,
            None,
        ));
    }
    if has_display || !rows.is_empty() {
        let _ = menu.append(&switch_menu);
    }

    // ---- 画面在对面时：唯一还能做的事 ----
    //
    // 本机没接显示器 = 画面在另一台机器上。这时本机既读不到也写不了这块屏的
    // DDC，上面的「切换输入源」列表也是空的 —— 如果不给这条，用户看着一个
    // 什么都点不动的菜单，只能伸手去按显示器按钮。让握着画面的那台替我们发
    // 指令，是这条物理限制下唯一的路（面板主按钮走的是同一件事）。
    if !has_display && config.own_value > 0 {
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&MenuItem::with_id(
            MENU_ASK_BACK,
            "请对面把画面切回本机",
            true,
            None,
        ));
    }

    // ---- 从这里开始，所有能改配置的东西都在控制面板里，菜单不再重复第二份 ----
    // 热键、热键目标、检测支持性、扫描口、配对、传文件、开机自启、剪贴板、网络，
    // 面板里各有对应卡片；同一件事有两个入口，出问题时最难查是哪边生效。

    let _ = menu.append(&PredefinedMenuItem::separator());
    // 开关本身只写"开/关"和后果，不再把模式塞进括号里。
    // 模式是**每台对端**的属性（摆位图决定），一个全局值放在这里会让人以为
    // 它管着所有链路 —— 面板正因为这个删掉了全局模式按钮（v0.6.58：
    // 一点就把"这台跟随、那台并排"的混拓扑压平成清一色，并排那台跟着掉进
    // 共用一块屏）。托盘上留着同一个全局开关，等于把那个坑原样搬回来。
    //
    // 要看模式/改模式：每台对端的子菜单里各有「它在本机的方位」，或者摆位图。
    let kvm_text = if config.kvm.enabled {
        "共用键鼠 · 已开启 · 点此关闭"
    } else {
        "共用键鼠 · 已关闭 · 点此开启"
    };
    let _ = menu.append(&MenuItem::with_id(MENU_KVM, kvm_text, true, None));
    // 全局模式切换**只在还没有链路表时**出现（老的单对端配置）。
    // 有链路表时模式是每台对端各一份（摆位图/子菜单里改），一个全局开关
    // 会把"这台跟随、那台并排"的混拓扑压平 —— 面板正是因此删掉了它。
    if config.kvm.links.is_empty() {
        let other = if config.kvm.mode == "edge" {
            "改用跟随画面：一套键鼠跟着画面走"
        } else {
            "改用多屏共享：两台各用各的屏和键鼠"
        };
        let _ = menu.append(&MenuItem::with_id(MENU_KVM_MODE, other, true, None));
    }
    // 会话中"换收件人"：这是"共用屏那台 → 并排那台"唯一能在托盘上按的入口
    // （画面一走就看不见面板）。只有一条链路时没意义，藏掉。
    if config.kvm.enabled && config.kvm.links.len() > 1 {
        let _ = menu.append(&MenuItem::with_id(
            MENU_KVM_HOP,
            "把键鼠转给另一台（画面不动）",
            true,
            None,
        ));
    }

    // 权限没生效时给一条明路。macOS 把授权结论缓存在进程里，刚在设置里
    // 勾上时本进程照样报"没权限"，这时点上面的开关只会再弹一次设置页 ——
    // 用户会觉得"勾了没用/装错版本"。唯一解法是重启进程。
    if !perms_ok() {
        let _ = menu.append(&MenuItem::with_id(
            MENU_RESTART,
            "⚠ 系统授权没生效 · 点这里重启应用",
            true,
            None,
        ));
    }

    // 多屏共享必须知道对端屏幕在本机哪一侧，不然鼠标推错边永远越不过去。
    // 两台要各配各的：Mac 选"对端在右"，Windows 那台就要选"对端在左"。
    //
    // v3 起**方向是每条链路各一份**（上面每台对端的子菜单里），这里只在
    // 还没配链路表时留着 —— 那种情况共用键鼠本来也开不起来，纯粹是给
    // 手改配置的人一个入口。两个入口同时出现会让人以为改一个没用。
    if config.kvm.mode == "edge" && config.kvm.links.is_empty() {
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&MenuItem::new("对端屏幕在本机哪一侧", false, None));
        for (value, label) in [
            ("right", "右侧"),
            ("left", "左侧"),
            ("top", "上方"),
            ("bottom", "下方"),
        ] {
            let _ = menu.append(&CheckMenuItem::with_id(
                format!("{MENU_KVM_SIDE}{value}"),
                format!("对端在{label}"),
                true,
                config.kvm.side == value,
                None,
            ));
        }
    }

    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&MenuItem::with_id(ID_PANEL, "打开控制面板…", true, None));
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&MenuItem::with_id(MENU_QUIT, "退出", true, None));
    menu
}

/// 方向的中文名，菜单和状态文字都用它
pub(super) fn side_label(side: &str) -> &'static str {
    match kvm::event::Side::parse(side) {
        Some(kvm::event::Side::Left) => "左",
        Some(kvm::event::Side::Top) => "上",
        Some(kvm::event::Side::Bottom) => "下",
        _ => "右",
    }
}
