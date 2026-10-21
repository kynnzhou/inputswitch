//! 托盘 UI + 全局热键。
//!
//! 设计目标：**不打开终端就能用**。菜单里直接列出各输入口，点一下就切；
//! 切成功后自动把「名称→VCP 取值」记进 profile，并记下热键目标。
//!
//! 所有 DDC 操作都放到工作线程：一次切换要等 2–3 秒看显示器有没有离开本机，
//! 放在 UI 线程会卡住菜单。

use std::cell::RefCell;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering as AtomicOrdering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};
use tao::dpi::LogicalSize;
use tao::event::{ElementState, Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::keyboard::{KeyCode, ModifiersState};
use tao::window::{Window, WindowBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::clipboard::{ClipItem, Watcher};
use crate::kvm;
use crate::config::Config;
use crate::models::{Presence, VCP_BRIGHTNESS, VCP_INPUT_SOURCE};
use crate::panel::{self, Action, Snapshot};
use crate::panelwin::Panel;
use crate::peer::{Mesh, Peer};
use crate::{clipboard, transfer};
use crate::Switcher;

// 托盘的定位：**只放"不开窗口就能做完的事"**。
//
// 看一眼当前口、一键切过去、打开控制面板、退出。
// 凡是控制面板里已经有的（配对、传文件、热键、开机自启、检测、扫描、剪贴板），
// 这里一律不再重复一份 —— 两处都能改的东西，出问题时最难查是哪边生效。

const MENU_QUIT: &str = "quit";
/// 共用键鼠开关
const MENU_KVM: &str = "toggle-kvm";
/// 在两种模式之间切换：跟随画面 ↔ 多屏共享
const MENU_KVM_MODE: &str = "kvm-mode";
/// 会话中把键鼠转给下一条有门的链路（画面不动）
const MENU_KVM_HOP: &str = "kvm-hop";
const MENU_KVM_SIDE: &str = "kvm-side:";
/// `switch:<name>:<decimal value>`
const ID_SWITCH: &str = "switch:";
/// `peer:<node>:<value>` —— 直接切到某台对端机器占用的口
const ID_PEER: &str = "peer:";
/// `peer-hand:<node>` —— 把键鼠交给这台
const ID_PEER_HAND: &str = "peer-hand:";
/// `peer-file:<node>` —— 发文件给这台
const ID_PEER_FILE: &str = "peer-file:";
/// `peer-side:<node>:<side>` —— 这台摆在本机的哪一侧（`none` = 共用一块屏）
const ID_PEER_SIDE: &str = "peer-side:";
/// 打开控制面板（程序自己的窗口）
const ID_PANEL: &str = "open-panel";
/// 重启本程序（只在权限没生效时出现）
const MENU_RESTART: &str = "restart-app";
/// 面板实际监听的端口。build_menu 要拿它拼菜单项，所以放静态里。
static PANEL_PORT: AtomicU16 = AtomicU16::new(0);

/// 当前的共用键鼠实例。
///
/// 放静态而不是传参：整个进程只有一个，而需要它的地方分散在 UI 线程
/// （菜单开关）、工作线程（切屏后叫键鼠跟过去）、面板（按钮）。
/// 硬传下去要给 `engine_loop` 加一整串参数，收益不抵成本。
static KVM: Mutex<Option<Arc<kvm::Kvm>>> = Mutex::new(None);

/// 本机剪贴板里"等着按需传输"的文件清单。
///
/// 主线程轮询剪贴板时写入（只 stat，不读内容），引擎线程在用户按下
/// Ctrl+V 时取走并发起传输。复制那一刻网络上不下发任何东西。
static CLIP_PENDING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
/// 一次只有一个剪贴板传输在跑。用户连按几次粘贴不该叠出几条连接。
static CLIP_BUSY: AtomicBool = AtomicBool::new(false);

/// 取当前实例的克隆。没有就是共用键鼠没开。
fn kvm_now() -> Option<Arc<kvm::Kvm>> {
    KVM.lock().ok().and_then(|slot| slot.clone())
}

/// 从在线对端里挑出该用哪个：优先上次记住的那个，找不到才退回第一个。
/// 局域网里只要出现过第三台装过本程序的机器（旧电脑自启没关很常见），
/// peers.first() 就会稳定选错人 —— 两边各自对着错的机器发心跳，
/// 谁也等不到回应，表现就是"两边都空闲、对端没回应"。
fn pick_peer<'a>(peers: &'a [Peer], remembered: &str) -> Option<&'a Peer> {
    if !remembered.is_empty() {
        if let Some(p) = peers.iter().find(|p| p.node == remembered) {
            return Some(p);
        }
    }
    peers.first()
}

/// 启动共用键鼠。菜单和面板共用这一份。
///
/// 两处各写一遍的话，迟早会漂：菜单改完忘了改面板，面板点了没反应，
/// 而日志里看不出是哪条路径走的。
///
/// v3 起吃的是**整张链路表**：`config.kvm.links` 里每台对端一条，按在线
/// 情况解析成 IP。老配置（只有 `peer_node` + `side`）由 `Kvm::normalize`
/// 升成一条链路，所以两机场景的行为和 v0.6.46 完全一致。
fn kvm_boot(
    config: &mut Config,
    peers: &[Peer],
    tx: &mpsc::Sender<Command>,
    log: Arc<dyn Fn(String) + Send + Sync>,
) -> Result<(Arc<kvm::Kvm>, String), String> {
    // 紧急收回（Ctrl+Alt+Esc）要能连画面一起切回来。
    // 只收回键鼠的话，用户"能操作本机了，但看不见本机"，等于卡在原地。
    let panic_tx = tx.clone();
    let own_v = config.own_value;
    let own_n = config.own_input.clone();
    let on_panic: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        if own_v > 0 {
            let name = if own_n.is_empty() {
                format!("0x{own_v:02X}")
            } else {
                own_n.clone()
            };
            let _ = panic_tx.send(Command::SwitchRaw { name, value: own_v });
        }
    });

    let mode = kvm::Mode::parse(&config.kvm.mode).unwrap_or(kvm::Mode::Follow);
    let role = kvm::Role::parse(&config.kvm.role).unwrap_or(kvm::Role::Console);

    // ---- 链路表 → 在线 IP ----
    //
    // 链路解析分两遍：
    //
    // ① 在线的那几台 → 正常工作，顺手把 IP 记进配置（下次它离线时要用）
    // ② 离线但有旧 IP 的 → **仍然占住那条边**。不占的话，用户朝那边推会
    //    静默无反应，看起来跟程序坏了没区别；占住之后会得到一句
    //    「光标到了边缘，X 没有回应，不越界」。这一轮它不参与路由，
    //    所以也不会出现"先吞掉键鼠再等 2.5 秒"。
    //
    // 一台都不在线 → 退回老行为（pick_peer 挑一台），保持两机场景不变：
    // 单链路时对端换了 IP（DHCP）必须还能连上，不能被旧地址卡死。
    let mut links: Vec<kvm::Link> = Vec::new();
    let mut offline: Vec<String> = Vec::new(); // 连旧地址都没有，占不住边
    let mut dead: Vec<String> = Vec::new(); // 有旧地址，占边但只提醒
    let mut addr_fix: Vec<(String, String)> = Vec::new();

    let parse_side = |lc: &crate::config::LinkConfig, lmode: kvm::Mode| {
        if lmode == kvm::Mode::Edge {
            kvm::event::Side::parse(&lc.side)
                .or_else(|| kvm::event::Side::parse(&config.kvm.side))
                .or(Some(kvm::event::Side::default()))
        } else {
            None
        }
    };

    for lc in config.kvm.links.clone() {
        let node = lc.node.trim().to_string();
        if node.is_empty() {
            continue;
        }
        let Some(p) = peers.iter().find(|p| p.node == node) else {
            continue; // 第二遍处理
        };
        let lmode = kvm::Mode::parse(&lc.mode_or(&config.kvm.mode)).unwrap_or(mode);
        let ip = p.addr.ip();
        if lc.addr.trim() != ip.to_string() {
            addr_fix.push((node.clone(), ip.to_string()));
        }
        links.push(kvm::Link {
            node: p.node.clone(),
            ip,
            side: parse_side(&lc, lmode),
            mode: lmode,
        });
    }

    if !links.is_empty() {
        for lc in config.kvm.links.clone() {
            let node = lc.node.trim().to_string();
            if node.is_empty() || peers.iter().any(|p| p.node == node) {
                continue;
            }
            let Ok(ip) = lc.addr.trim().parse::<IpAddr>() else {
                offline.push(node);
                continue;
            };
            let lmode = kvm::Mode::parse(&lc.mode_or(&config.kvm.mode)).unwrap_or(mode);
            let side = parse_side(&lc, lmode);
            if side.is_none() {
                // 跟随画面链路没有门，占边没有任何意义
                continue;
            }
            links.push(kvm::Link {
                node: node.clone(),
                ip,
                side,
                mode: lmode,
            });
            dead.push(node);
        }
    }

    let main_node;
    if links.is_empty() {
        let Some(peer) = pick_peer(peers, &config.kvm.peer_node) else {
            return Err("没有发现任何已配对的在线对端".to_string());
        };
        let side = if mode == kvm::Mode::Edge {
            Some(kvm::event::Side::parse(&config.kvm.side).unwrap_or_default())
        } else {
            None
        };
        main_node = peer.node.clone();
        links.push(kvm::Link {
            node: peer.node.clone(),
            ip: peer.addr.ip(),
            side,
            mode,
        });
        // 顺手把它写进链路表：下次就有据可查，也是面板的起点
        config.kvm.links = vec![crate::config::LinkConfig {
            node: peer.node.clone(),
            side: config.kvm.side.clone(),
            mode: config.kvm.mode.clone(),
            input: String::new(),
            addr: peer.addr.ip().to_string(),
        }];
    } else {
        main_node = links[0].node.clone();
        for (node, addr) in addr_fix {
            if let Some(l) = config.kvm.links.iter_mut().find(|l| l.node == node) {
                l.addr = addr;
            }
        }
    }

    // 转发期间按 Ctrl+V 且手上有待传清单：抓取层吞掉键，这里把它转给引擎线程
    let paste_tx = tx.clone();
    let on_paste: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = paste_tx.send(Command::PasteIntent);
    });
    let routes: Vec<String> = links
        .iter()
        .map(|l| match l.side {
            Some(s) if l.mode == kvm::Mode::Edge => {
                format!("{}侧→{}", side_label(s.as_str()), l.node)
            }
            _ => format!("随画面→{}", l.node),
        })
        .collect();
    let handle = Arc::new(kvm::start(kvm::Options {
        links,
        mode,
        role,
        base_port: config.network.port,
        // 配对过就加密事件流（等价于 Deskflow 的默认 TLS）
        key: crate::pair::link_key(&config.pair.code),
        on_panic: Some(on_panic),
        log,
        on_paste: Some(on_paste),
        // 推边自动改指向（v0.6.60）：误判时可从配置里关掉，只剩热键
        push_hop: config.kvm.push_hop,
        // 接收位移倍率（v0.6.64）：两系统指针加速曲线不同，手感靠它微调
        recv_scale: config.kvm.recv_scale,
    })?);

    let text = match (mode, role) {
        (kvm::Mode::Follow, kvm::Role::Console) => format!(
            "共用键鼠已开启：本机是键鼠端，画面切走时键鼠自动跟过去（{}）",
            routes.join("，")
        ),
        (kvm::Mode::Follow, kvm::Role::Remote) => format!(
            "共用键鼠已开启：本机不抓键鼠，等对端切过来接管（{}）",
            routes.join("，")
        ),
        (kvm::Mode::Edge, _) => format!("共用键鼠已开启：{}", routes.join("，")),
    };
    if !offline.is_empty() {
        log_line(&format!(
            "{} 既不在线、也没有留下过地址，这两条边不占位（在面板里重新连一次它就会记下地址）",
            offline.join("、")
        ));
    }
    if !dead.is_empty() {
        log_line(&format!(
            "{} 不在线，那几条边仍占位：推过去只会提示「没有回应，不越界」，不会吞键鼠（配置保留）",
            dead.join("、")
        ));
    }
    // 记住这次用的主链路：下次自动恢复/重开时优先找它。
    // 记住的和这次选的不一样 = 上次那台不在线，退回了第一台 ——
    // 局域网里多一台装过本程序的机器就会发生，必须说清楚。
    if config.kvm.peer_node != main_node {
        if !config.kvm.peer_node.is_empty() {
            log_line(&format!(
                "上次的对端 {} 不在线，这次先用 {}（若局域网里还有别的机器装过本程序，建议关掉它的自启，免得又选错）",
                config.kvm.peer_node, main_node
            ));
        }
        config.kvm.peer_node = main_node;
    }
    // 链路表可能变了（地址回填 / 首次升上来），一并落盘
    let _ = config.save();
    Ok((handle, text))
}

/// 「重新检测显示器」的结论。工作线程写，UI 线程压快照时取走。
/// 用静态是因为这条结论要横跨两个线程、又不值得为它加一条事件通道。
static PORT_NOTE: Mutex<String> = Mutex::new(String::new());

/// 本机此刻有没有接通的显示器。
///
/// 画面在对面那台时，本机显卡这边就是"没接显示器"，枚举结果为 0 ——
/// 这不是故障，但必须让面板说清楚，否则用户只会看到一句"没报输入源"。
static HAS_DISPLAY: AtomicBool = AtomicBool::new(false);

/// 把 "ctrl+alt+s" 写成人看的 "Ctrl + Alt + S"。
fn pretty_hotkey(spec: &str) -> String {
    if spec.trim().is_empty() {
        return "未设置".to_string();
    }
    spec.split('+')
        .map(|part| match part.trim().to_lowercase().as_str() {
            "ctrl" | "control" => "Ctrl".to_string(),
            "alt" | "option" => "Alt".to_string(),
            "shift" => "Shift".to_string(),
            "cmd" | "super" | "win" | "meta" => "Cmd".to_string(),
            other => other.to_uppercase(),
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

/// 一行菜单项：口的名字、菜单显示文本、VCP 取值、状态。
#[derive(Clone)]
struct PortRow {
    /// 存进配置与命令里的名字
    name: String,
    /// 菜单上显示的文字
    label: String,
    value: u16,
    verified: bool,
    current: bool,
    /// 这个口上有没有主机（只由本机实测得到）
    presence: Presence,
}

impl PortRow {
    /// 这个口有没有任何"本机证据"。
    ///
    /// 有证据 = 本机真的切过去过（`verified`），或者切/扫出过结论
    /// （有信号、无信号、该口不存在）。都没有就只是固件报了个编码 ——
    /// 那种行不该出现在「输入源」里：它看起来像"本机的一个输入源"，
    /// 实际上可能连物理接口都没有（厂商常把同一块面板所有 SKU 的
    /// 编码都写进 `vcp(60(...))`）。
    fn has_evidence(&self) -> bool {
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
fn port_rows(
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
fn configured_value(config: &Config, profile_key: &str, name: &str) -> Option<u16> {
    config.profile_for(profile_key)?.find(name).map(|s| s.value)
}

enum Command {
    /// 切屏，并且如果键鼠开着就跟过去（正常路径）
    SwitchRaw { name: String, value: u16 },
    /// 只切画面，键鼠留在本机。面板上的「仅切换画面」走这条
    SwitchOnly { name: String, value: u16 },
    SetTarget { name: String, value: u16 },
    ClearTarget,
    Rescan,
    /// 每 5 秒的轻量心跳：只做广播同步和状态推送，**不碰重 DDC**。
    ///
    /// 以前这里发的是 Rescan —— 全量重读 EDID + 能力表，一次就是十几笔
    /// I2C 事务。显示器刚切回来、链路还在重新协商的那几秒正好撞上这波
    /// 读取，Apple Silicon 上会把 WindowServer 顶进看门狗超时（实测整机关机）。
    /// 重活只留给用户主动点的「重新检测显示器」。
    Heartbeat,
    /// 重新体检显示器对软件切换的支持情况（只读，不动画面）
    CheckSupport,
    /// 扫描每个口上有没有主机（会真的切屏）
    ScanPorts,
    /// 对端剪贴板传来了新文本，交给 UI 线程写进本机剪贴板
    RemoteClipboard(String),
    /// 共用键鼠转发期间用户按下了 Ctrl+V，且本机手上有待传的文件清单。
    /// 这个键已经被抓取层吞掉，这里负责把文件传过去、传完补按。
    PasteIntent,
    /// 本机读不到 DDC（画面在对面），请对端替我把画面切回这个口
    AskPeerSwitch { value: u16 },
}

enum UserEvent {
    Status {
        text: String,
        target: String,
        /// 当前显示器在配置里的身份键，菜单按它取输入口列表
        profile_key: String,
        /// 支持性体检结论，显示在状态行下面
        support: String,
    },
    /// 面板上点了需要主线程才能做的事（开文件选择器、开浏览器、改配置）
    Panel(Action),
    /// 对端发来的剪贴板文本
    RemoteClipboard(String),
    /// 传输进度 / 结果提示
    Transfer(String),
    /// 文件收完了，通知一下。第二个参数 = 这批是剪贴板按需传输
    /// （要写进剪贴板），不是面板手动发文件。
    Saved(Vec<PathBuf>, bool),
    /// 剪贴板按需传输结束（成功/失败），主线程据此补按粘贴键
    PasteDone(Result<String, String>),
}

/// 当前显示器的身份键；屏幕已经切走时为空串。
fn profile_key_of(switcher: &Switcher) -> String {
    switcher
        .pick_display(None)
        .map(|d| switcher.identity(&d))
        .unwrap_or_default()
}

fn load_icon() -> Option<Icon> {
    let bytes = include_bytes!("../assets/menubar@2x.png");
    let image = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (width, height) = image.dimensions();
    Icon::from_rgba(image.into_raw(), width, height).ok()
}

/// 追加一行日志。托盘应用没有控制台，出问题时用户能把这个文件发给我。
pub fn log_line(message: &str) {
    use std::io::Write;
    // 排他写：钩子线程、收包线程、UI 线程都会打日志，各自 open+append
    // 时两次写会交错成 "[时间[] 时间...文字]" 这种半截行 —— 排障时最
    // 需要的恰恰是时序，混一行就可能读反因果。
    static LOG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = LOG_LOCK.lock().ok();
    let dir = crate::config::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("app.log"))
    {
        let _ = writeln!(file, "[{}] {}", local_stamp(), message);
    }
}

/// 本地时间戳 YYYY-MM-DD HH:MM:SS.mmm，不引第三方日期库。
/// 毫秒是排障刚需：交还/进门这类"差几百毫秒"的手感问题，秒级时间戳
/// 根本看不出谁慢（v0.6.32 起统一带毫秒）。
fn local_stamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64 + local_offset_secs();
    let millis = now.subsec_millis();
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        y,
        m,
        d,
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60,
        millis
    )
}

/// UTC 与本地时区差（秒）。用系统 API 取，避免硬编码夏令时。
fn local_offset_secs() -> i64 {
    #[cfg(unix)]
    {
        // 用 libc 的 localtime_r 读 tm_gmtoff
        #[repr(C)]
        struct Tm {
            tm_sec: i32,
            tm_min: i32,
            tm_hour: i32,
            tm_mday: i32,
            tm_mon: i32,
            tm_year: i32,
            tm_wday: i32,
            tm_yday: i32,
            tm_isdst: i32,
            tm_gmtoff: i64,
            tm_zone: *const std::os::raw::c_char,
        }
        extern "C" {
            fn localtime_r(time: *const i64, result: *mut Tm) -> *mut Tm;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let mut tm = Tm {
            tm_sec: 0,
            tm_min: 0,
            tm_hour: 0,
            tm_mday: 1,
            tm_mon: 0,
            tm_year: 70,
            tm_wday: 0,
            tm_yday: 0,
            tm_isdst: 0,
            tm_gmtoff: 0,
            tm_zone: std::ptr::null(),
        };
        unsafe {
            localtime_r(&now, &mut tm);
        }
        tm.tm_gmtoff
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// Howard Hinnant 的 civil_from_days：把「1970-01-01 起的天数」转成公历。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 本机当前占用的 VCP 取值，未知时为 None。
fn own_value(mesh: &Mesh) -> Option<u16> {
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
fn menu_signature(
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

fn retire_menu(menu: tray_icon::menu::Menu) {
    RETIRED_MENUS.with(|cell| {
        let mut list = cell.borrow_mut();
        list.retain(|(t, _)| t.elapsed() < Duration::from_secs(60));
        list.push((Instant::now(), menu));
    });
}

/// 内容变了才重建菜单。返回 true 表示这次真的换了。
fn refresh_menu(
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
        let _ = tray.set_menu(Some(Box::new(menu)));
    }
    true
}

fn build_menu(
    status: &str,
    config: &Config,
    peers: &[Peer],
    current: Option<u16>,
    profile_key: &str,
    support: &str,
) -> Menu {
    let menu = Menu::new();
    let _ = menu.append(&MenuItem::new(status, false, None));
    // 第二行：这台显示器到底能不能被软件切，以及已经标出几个口有主机
    if !support.is_empty() {
        let mut line = support.to_string();
        if let Some(profile) = config.profile_for(profile_key) {
            let total = profile.inputs.len();
            let occupied = profile
                .inputs
                .iter()
                .filter(|s| s.presence == Presence::Occupied)
                .count();
            if total > 0 {
                line.push_str(&format!(" · {} 个口", total));
                if occupied > 0 {
                    line.push_str(&format!("，{} 个有主机", occupied));
                }
            }
        }
        let _ = menu.append(&MenuItem::new(line, false, None));
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
                let port_label = if peer.own_input.is_empty() {
                    format!("0x{:02X}", peer.own_value)
                } else {
                    format!("{} · 0x{:02X}", config.label_for(&peer.own_input), peer.own_value)
                };
                let id = format!("{}{}:{}", ID_PEER, &peer.node, peer.own_value);
                let _ = menu.append(&MenuItem::with_id(
                    id,
                    format!("切到 {}（{}）", peer.node, port_label),
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
                head.push_str("（没回应）");
            } else if link.side.is_empty() {
                head.push_str(" · 共用屏");
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
                    let port_label = if peer.own_input.is_empty() {
                        format!("0x{:02X}", peer.own_value)
                    } else {
                        format!(
                            "{} · 0x{:02X}",
                            config.label_for(&peer.own_input),
                            peer.own_value
                        )
                    };
                    let _ = sub.append(&MenuItem::with_id(
                        format!("{}{}:{}", ID_PEER, &peer.node, peer.own_value),
                        format!("切到它（{port_label}）"),
                        true,
                        None,
                    ));
                }
                None => {
                    let _ = sub.append(&MenuItem::new("它还没报告占用的口", false, None));
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

            // 3) 它摆在本机的哪一侧 —— 每条链路各有一份，不再是全局一个值
            let _ = sub.append(&PredefinedMenuItem::separator());
            let _ = sub.append(&MenuItem::new("它摆在本机的：", false, None));
            for (value, label) in [
                ("right", "右侧"),
                ("left", "左侧"),
                ("top", "上方"),
                ("bottom", "下方"),
                ("", "共用一块屏（跟随画面）"),
            ] {
                let mark = if link.side == value { "✓ " } else { "　 " };
                // 空值走 `none`：`peer-side:<node>:` 尾部空串在 rsplit 里
                // 与"没带值"分不开，两种错法表现不同、排查很烦
                let id = if value.is_empty() {
                    format!("{ID_PEER_SIDE}{}:none", link.node)
                } else {
                    format!("{ID_PEER_SIDE}{}:{value}", link.node)
                };
                let _ = sub.append(&MenuItem::with_id(id, format!("{mark}{label}"), true, None));
            }
            let _ = menu.append(&sub);
        }
    }

    let _ = menu.append(&PredefinedMenuItem::separator());

    // 切换子菜单：口列表来自显示器自报的能力表，不是硬编码
    let rows = port_rows(config, profile_key, current, false);
    let switch_menu = Submenu::new("切换输入源", true);
    if rows.is_empty() {
        let _ = switch_menu.append(&MenuItem::new(
            "没读到输入口 · 去控制面板点「重新检测显示器」",
            false,
            None,
        ));
    }
    for row in &rows {
        let id = format!("{}{}:{}", ID_SWITCH, &row.name, row.value);
        let head = if row.current { "✓ " } else { "　" };
        let mut tail = String::new();
        if row.current {
            tail.push_str(" · 本机");
        }
        // 有没有主机：这是用户最想知道的一件事，直接写在口名后面
        if row.presence != Presence::Occupied {
            tail.push_str(row.presence.mark());
        } else if !row.current {
            tail.push_str(" · 有主机");
        }
        if !row.verified && row.presence == Presence::Unknown {
            tail.push_str(" · 未确认");
        }
        let text = format!("{}{}  (0x{:02X}){}", head, row.label, row.value, tail);
        let _ = switch_menu.append(&MenuItem::with_id(id, text, true, None));
    }
    // 没有扫描过就提示一句，别让人以为"没有主机"是本来就有的信息
    if rows
        .iter()
        .any(|r| !r.current && r.presence == Presence::Unknown)
    {
        let _ = switch_menu.append(&PredefinedMenuItem::separator());
        let _ = switch_menu.append(&MenuItem::new(
            "哪个口接了主机：控制面板 → 扫描哪个口有主机",
            false,
            None,
        ));
    }
    let _ = menu.append(&switch_menu);

    // ---- 从这里开始，所有能改配置的东西都在控制面板里，菜单不再重复第二份 ----
    // 热键、热键目标、检测支持性、扫描口、配对、传文件、开机自启、剪贴板、网络，
    // 面板里各有对应卡片；同一件事有两个入口，出问题时最难查是哪边生效。

    let _ = menu.append(&PredefinedMenuItem::separator());
    // 两种模式的措辞必须不一样 —— 看菜单就知道自己在哪种场景里，
    // 而不是像之前那样无论什么场景都写"鼠标推到边缘"。
    let mode_is_follow = config.kvm.mode != "edge";
    let mode_hint = if mode_is_follow {
        "跟随画面：画面切到对端时键鼠跟过去".to_string()
    } else {
        format!("多屏共享：鼠标推到{}侧越界", side_label(&config.kvm.side))
    };
    let kvm_text = if config.kvm.enabled {
        format!("共用键鼠 · 已开启（{mode_hint}）点这里关闭")
    } else {
        format!("共用键鼠 · 开启（{mode_hint}）")
    };
    let _ = menu.append(&MenuItem::with_id(MENU_KVM, kvm_text, true, None));
    let other = if mode_is_follow {
        "  改用多屏共享（两台各有屏幕和键鼠）"
    } else {
        "  改用跟随画面（一套键鼠，跟屏幕走）"
    };
    let _ = menu.append(&MenuItem::with_id(MENU_KVM_MODE, other, true, None));
    // 会话中"换收件人"：这是"共用屏那台 → 并排那台"唯一能在托盘上按的入口
    // （画面一走就看不见面板）。只有一条链路时没意义，藏掉。
    if config.kvm.enabled && config.kvm.links.len() > 1 {
        let _ = menu.append(&MenuItem::with_id(
            MENU_KVM_HOP,
            "把键鼠转给下一条边（画面不动）",
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
    if !mode_is_follow && config.kvm.links.is_empty() {
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&MenuItem::new("对端屏幕在本机哪一侧？（两台各配各的）", false, None));
        for (value, label) in [
            ("right", "右侧"),
            ("left", "左侧"),
            ("top", "上方"),
            ("bottom", "下方"),
        ] {
            let mark = if config.kvm.side == value { "✓" } else { "　" };
            let _ = menu.append(&MenuItem::with_id(
                format!("{MENU_KVM_SIDE}{value}"),
                format!("{mark} 对端在{label}"),
                true,
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

/// 改完链路方向/模式之后重启共用键鼠实例。
///
/// 必须重启的理由：平台层的抓取钩子里**记死了边缘掩码**（哪几条边通向邻居），
/// 这是启动时按链路表算出来的。热改配置只会让"配置说的"和"钩子判的"不一致，
/// 表现是"改了方向但推那侧还是没反应"，而日志里一切正常。
///
/// `ok_text` 是成功时写进状态条的话（已含"改成了什么"），失败再往后拼。
/// 改链路时把正在进行的会话停下来，留下的是什么。
///
/// 一个 bool 会逼着文案替对端宣布它还没做的事：`take_back_from_peer`
/// 只是把 Takeback 发出去，对端收到才松手。所以这里必须分四态。
enum Handback {
    /// 本来就没有会话，没什么可交还的
    None,
    /// 本机在转发 → 已经收回本机
    Back,
    /// 本机被控 → 已请对端交还（异步，不能断言收回）
    Asked,
    /// 有会话但没能正常收场
    Stuck,
}

fn restart_kvm(
    config: &mut Config,
    kvm_handle: &mut Option<Arc<kvm::Kvm>>,
    mesh: &Mesh,
    tx: &mpsc::Sender<Command>,
    status: &mut String,
    ok_text: String,
) {
    let was_on = kvm_handle.take();
    let mut handback = Handback::None;
    if let Some(handle) = was_on.as_ref() {
        // 改链路必然重建实例（四向掩码是启动时按链路表算出来的），正在
        // 转发的会话会被掐断。**先把键鼠交还回去再停**：不交还的话两端
        // 都以为自己在转发 —— 本机在 Sending 直接停手，对端以为还在被
        // 控制；本机在 Receiving 停掉，对端仍往一个已经没人注入的实例上
        // 发。用户看到的就是"改个方向，鼠标键盘全部失去控制"。
        handback = match handle.state() {
            kvm::State::Sending => {
                if handle.give_back() {
                    Handback::Back
                } else {
                    Handback::Stuck
                }
            }
            kvm::State::Receiving => {
                if handle.take_back_from_peer("改链路") {
                    Handback::Asked
                } else {
                    Handback::Stuck
                }
            }
            kvm::State::Idle => Handback::None,
        };
        handle.stop();
    }
    // 说清楚"这次改动打断了正在进行的转发"。不说的话，用户看到的是
    // "我刚把方向改对，键鼠却没反应了"，而其实键鼠已经回到本机 ——
    // 需要的是重新推一次边、或再点一次「切换」。
    let ok_text = match handback {
        Handback::None => ok_text,
        Handback::Back => format!("{ok_text}（原先的转发已停下，键鼠收回本机）"),
        Handback::Asked => format!("{ok_text}（原先的转发已停下，已请对端交还键鼠）"),
        Handback::Stuck => format!("{ok_text}（原先的转发没能正常收场，请按热键收回键鼠）"),
    };
    if let Ok(mut slot) = KVM.lock() {
        *slot = None;
    }
    if was_on.is_none() {
        *status = format!("{ok_text}，下次开启共用键鼠生效");
        log_line(status);
        return;
    }
    let peers_now = mesh.live_peers();
    match pick_peer(&peers_now, &config.kvm.peer_node) {
        None => {
            config.kvm.enabled = false;
            let _ = config.save();
            *status = format!("{ok_text}；没找到对端，共用键鼠已关闭，请重新打开");
        }
        Some(_) => {
            let log: Arc<dyn Fn(String) + Send + Sync> = Arc::new(|text| log_line(&text));
            match kvm_boot(config, &peers_now, tx, log) {
                Ok((handle, text)) => {
                    *kvm_handle = Some(handle.clone());
                    if let Ok(mut slot) = KVM.lock() {
                        *slot = Some(handle);
                    }
                    *status = format!("{ok_text}，{text}");
                }
                Err(err) => {
                    config.kvm.enabled = false;
                    let _ = config.save();
                    *status = format!("{ok_text}，但共用键鼠重启失败：{err}");
                }
            }
        }
    }
    log_line(status);
}

/// 两个系统权限都齐了吗。缺任一，共用键鼠要么开不起来、要么只能单向。
fn perms_ok() -> bool {
    kvm::trusted() && kvm::can_post()
}

/// 重启本程序。
///
/// 存在的理由只有一个：**macOS 把 TCC 授权结论缓存在进程里**。用户在设置里
/// 勾上以后，正在跑的这个进程照样报"没权限"，点「开启共用键鼠」只会再弹一次
/// 设置页 —— 看起来就是"勾选无效 / 装错版本"（2026-10-20 连踩两次）。
/// 唯一解法是重启进程，所以把它做成一个按钮。
///
/// 延迟 1 秒再拉起新实例：本进程要先退出，否则托盘图标和面板端口还被占着，
/// 新实例起来抢不到，表现为"重启之后更没反应了"。
fn restart_app() {
    #[cfg(target_os = "macos")]
    {
        // 从 .../X.app/Contents/MacOS/X 反推 bundle 路径，装在别处也能重启
        let bundle = std::env::current_exe().ok().and_then(|exe| {
            exe.ancestors()
                .find(|p| p.extension().map(|e| e == "app").unwrap_or(false))
                .map(|p| p.to_path_buf())
        });
        let script = match bundle {
            Some(path) => format!("sleep 1; open -n '{}'", path.display()),
            None => "sleep 1; open -n -a InputSwitch".to_string(),
        };
        let _ = std::process::Command::new("sh").arg("-c").arg(script).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(exe) = std::env::current_exe() {
            let cmd = format!("timeout /t 1 >nul & start \"\" \"{}\"", exe.display());
            let mut child = std::process::Command::new("cmd");
            crate::hidden::quiet(&mut child);
            let _ = child.arg("/C").arg(cmd).spawn();
        }
    }
    log_line("重启中…（1 秒后自动起来）");
    // 日志是追加写的，退出前给文件系统一点时间把它落盘
    thread::sleep(Duration::from_millis(150));
    std::process::exit(0);
}

fn open_config_dir() {
    let dir = crate::config::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(target_os = "macos")]
    {
        let mut cmd = std::process::Command::new("open");
        let _ = crate::hidden::quiet(&mut cmd).arg(&dir).spawn();
    }
    #[cfg(windows)]
    {
        let mut cmd = std::process::Command::new("explorer");
        let _ = crate::hidden::quiet(&mut cmd).arg(&dir).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let mut cmd = std::process::Command::new("xdg-open");
        let _ = crate::hidden::quiet(&mut cmd).arg(&dir).spawn();
    }
}

/// 配对码显示在菜单里时只露头尾：菜单截图、录屏都可能把它带出去。
fn mask_code(code: &str) -> String {
    let code = code.trim();
    if code.len() <= 4 {
        return "*".repeat(code.chars().count());
    }
    let chars: Vec<char> = code.chars().collect();
    let head: String = chars.iter().take(2).collect();
    let tail: String = chars.iter().skip(chars.len() - 2).collect();
    format!("{head}**{tail}")
}

fn open_path(path: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(path);
        c
    };
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("explorer");
        c.arg(path);
        c
    };
    #[cfg(target_os = "linux")]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(path);
        c
    };
    let _ = crate::hidden::quiet(&mut cmd).spawn();
}

/// 调系统文件选择器。浏览器给不了本地绝对路径，只能走系统对话框。
fn pick_files() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let script = "set out to \"\"\nrepeat with f in (choose file with multiple selections allowed)\nset out to out & (POSIX path of f) & linefeed\nend repeat\nreturn out";
        let output = std::process::Command::new("/usr/bin/osascript")
            .args(["-e", script])
            .output();
        return match output {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(|line| line.trim())
                .filter(|line| !line.is_empty())
                .map(PathBuf::from)
                .collect(),
            _ => Vec::new(),
        };
    }
    #[cfg(windows)]
    {
        let script = "Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.OpenFileDialog; $d.Multiselect = $true; if ($d.ShowDialog() -eq 'OK') { $d.FileNames }";
        let output = std::process::Command::new("powershell")
            .args(["-NoProfile", "-STA", "-Command", script])
            .output();
        return match output {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(|line| line.trim())
                .filter(|line| !line.is_empty())
                .map(PathBuf::from)
                .collect(),
            _ => Vec::new(),
        };
    }
    #[cfg(target_os = "linux")]
    {
        let output = std::process::Command::new("zenity")
            .args(["--file-selection", "--multiple", "--separator", "\n"])
            .output();
        return match output {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(|line| line.trim())
                .filter(|line| !line.is_empty())
                .map(PathBuf::from)
                .collect(),
            _ => Vec::new(),
        };
    }
}

fn pick_folder() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("/usr/bin/osascript")
            .args(["-e", "POSIX path of (choose folder)"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if path.is_empty() {
            None
        } else {
            Some(PathBuf::from(path))
        }
    }
    #[cfg(windows)]
    {
        let script = "Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.FolderBrowserDialog; if ($d.ShowDialog() -eq 'OK') { $d.SelectedPath }";
        let output = std::process::Command::new("powershell")
            .args(["-NoProfile", "-STA", "-Command", script])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if path.is_empty() {
            None
        } else {
            Some(PathBuf::from(path))
        }
    }
    #[cfg(target_os = "linux")]
    {
        let output = std::process::Command::new("zenity")
            .args(["--file-selection", "--directory"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (!path.is_empty()).then(|| PathBuf::from(path))
    }
}

/// 选文件并发给对端。
///
/// 连"弹文件选择器"一起放到后台线程：系统对话框是阻塞的，
/// 挂在 UI 线程上会让菜单和面板在整个选择过程中都没有响应。
fn start_send(
    config: &Config,
    mesh: &Mesh,
    proxy: &EventLoopProxy<UserEvent>,
    status: &mut String,
    target: Option<&str>,
) {
    let peers = mesh.live_peers();
    if peers.is_empty() {
        *status = "局域网里没找到对端，先让对面那台也开着本程序".to_string();
        return;
    }
    // **发给谁**：点名的 > 现在握着键鼠的那台 > 只有一台在线时就是它。
    //
    // 原来是死写 `peers.first()`：一机四台时文件必然发给排序第一的那台，
    // 而且界面上没有任何地方能看出"到底发给谁了"。
    let wanted = target
        .map(str::to_string)
        .or_else(|| kvm_now().and_then(|h| h.active_node().map(str::to_string)));
    let peer = match wanted.as_deref() {
        Some(node) => match peers.iter().find(|p| p.node == node).cloned() {
            Some(peer) => peer,
            None => {
                *status = format!("{node} 没有回应（不在线或不在同一网段），文件没有发出去");
                return;
            }
        },
        None if peers.len() == 1 => peers[0].clone(),
        None => {
            let names = peers
                .iter()
                .map(|p| p.node.as_str())
                .collect::<Vec<_>>()
                .join("、");
            *status = format!(
                "有 {} 台对端在线（{names}）：先把键鼠交给要发的那台再发。\
                 以前固定发第一台，多机时必然发错人",
                peers.len()
            );
            return;
        }
    };
    let key = crate::pair::link_key(&config.pair.code);
    let node = config.node.clone();
    let ip = peer.addr.ip();
    let port = config.transfer.port;
    let peer_name = peer.node.clone();
    let proxy_ui = proxy.clone();
    let proxy_progress = proxy.clone();
    *status = "正在选择文件…".to_string();

    thread::spawn(move || {
        let files = pick_files();
        if files.is_empty() {
            let _ = proxy_ui.send_event(UserEvent::Transfer(String::new()));
            return;
        }
        let count = files.len();
        let _ = proxy_ui.send_event(UserEvent::Transfer(format!("正在发送 {count} 个文件…")));
        // 256KB 一个包，1GB 文件能回调四千次；只在百分比变化时才通知 UI
        let mut last_pct = u64::MAX;
        let result = transfer::send(ip, port, &node, &files, key, false, |sent, total| {
            let pct = if total == 0 { 100 } else { sent * 100 / total };
            if pct != last_pct {
                last_pct = pct;
                let _ =
                    proxy_progress.send_event(UserEvent::Transfer(format!("正在发送… {pct}%")));
            }
        });
        let note = match result {
            Ok(message) => format!("{message} → {peer_name}"),
            Err(err) => format!("发送失败：{err}"),
        };
        let _ = proxy_ui.send_event(UserEvent::Transfer(note));
    });
}

/// 把当前所有状态压成面板要的一份快照。
#[allow(clippy::too_many_arguments)]
fn update_snapshot(
    snapshot: &Arc<Mutex<Snapshot>>,
    status: &str,
    support: &str,
    target: &str,
    config: &Config,
    profile_key: &str,
    peers: &[Peer],
    current: Option<u16>,
    mesh: &Mesh,
    transfer_note: &str,
) {
    let mut snap = snapshot.lock().unwrap();
    snap.node = config.node.clone();
    snap.status = status.to_string();
    snap.support = support.to_string();
    snap.hotkey = pretty_hotkey(&config.hotkey);
    snap.target = if target.is_empty() {
        "未设置".to_string()
    } else {
        config.label_for(target)
    };
    snap.paired = config.pair.is_set();
    snap.pair_code = config.pair.code.clone();
    snap.rejected = mesh.rejected();
    snap.replayed = mesh.replayed();
    snap.peer_verified = peers.iter().filter(|p| p.paired).count();
    snap.has_display = HAS_DISPLAY.load(AtomicOrdering::SeqCst);
    snap.own_value = config.own_value;
    // 共用键鼠：开没开、跑哪种模式、现在键鼠在哪边、每条链路什么状态
    //
    // 顺手把"本机开没开"告诉网络层，随下一条 announce 广播出去。选在这里
    // 对齐是因为它跑得够勤（面板轮询 + 托盘 5s 刷新），而且 KVM 的所有
    // 启停路径最后都会反映到 `kvm_now()` —— 在七八个启停点上各写一遍，
    // 漏一个就是长期错误广播。
    mesh.set_kvm_on(kvm_now().is_some());
    snap.kvm = match kvm_now() {
        Some(handle) => {
            let active_idx = handle.active_index();
            panel::KvmView {
                on: true,
                mode: handle.mode().as_str().to_string(),
                role: handle.role().as_str().to_string(),
                state: handle.state().label().to_string(),
                peer_alive: handle.peer_alive(),
                active: handle.active_node().unwrap_or("").to_string(),
                // 两个布尔都由后端算：面板不该自己拿 mode/role 去推，
                // 那套判据（links[0] 的 mode）跟 do_switch 用的
                // has_follow_link()（任一链路）压根不是一回事
                follow_link: handle.has_follow_link(),
                will_follow: handle.has_follow_link() && handle.can_capture(),
                // 起点只在"键鼠已经转到别台"时才有意义，单跳会话不标
                // （否则每场会话都给当前那台挂个「起点」，纯噪音）
                root: if handle.at_root() {
                    String::new()
                } else {
                    handle.root_node().unwrap_or("").to_string()
                },
                links: handle
                    .links()
                    .iter()
                    .enumerate()
                    .map(|(i, link)| panel::LinkView {
                        node: link.node.clone(),
                        addr: link.ip.to_string(),
                        // 空 = 无门（Follow 链路），界面上不画方向
                        side: link
                            .side
                            .map(|s| s.as_str().to_string())
                            .unwrap_or_default(),
                        mode: link.mode.as_str().to_string(),
                        alive: handle.alive_at(i),
                        active: active_idx == Some(i),
                        // 键鼠已经在别台手上、这台又在线 ⇒ 点它是"改指向"
                        // （画面不动），不是"建一场新会话"
                        can_retarget: handle.state() == kvm::State::Sending
                            && active_idx != Some(i)
                            && handle.alive_at(i),
                    })
                    .collect(),
                // 手感旋钮的真实值：从配置读，不从实例读 —— 实例里的
                // 是启动快照，用户改完还没重启时两者会不一样，界面该显示
                // "即将生效"的那个值。
                recv_scale: config.kvm.recv_scale,
                auto_resume: config.kvm.auto_resume,
                keep_alive: config.kvm.keep_alive,
            }
        }
        None => panel::KvmView {
            on: false,
            mode: config.kvm.mode.clone(),
            role: config.kvm.role.clone(),
            state: "未启动".to_string(),
            peer_alive: false,
            active: String::new(),
            // 没起来时按**配置**判：摆位图和提示必须跟重启后的行为一致，
            // 否则用户会照着错的图去拖（地址/模式在 v3 里是链路级字段，
            // 空 mode 表示沿用 kvm.mode，所以要用 mode_or 折算）
            follow_link: config
                .kvm
                .links
                .iter()
                .any(|l| l.mode_or(&config.kvm.mode) == "follow"),
            // 没开着就没有任何东西会跟
            will_follow: false,
            root: String::new(),
            // 没启动时也要把配置里的链路画出来，否则用户看不到、也就改不了
            links: config
                .kvm
                .links
                .iter()
                .map(|link| panel::LinkView {
                    node: link.node.clone(),
                    addr: link.addr.clone(),
                    side: link.side.clone(),
                    mode: link.mode_or(&config.kvm.mode),
                    alive: false,
                    active: false,
                    can_retarget: false,
                })
                .collect(),
            recv_scale: config.kvm.recv_scale,
            auto_resume: config.kvm.auto_resume,
            keep_alive: config.kvm.keep_alive,
        },
    };
    snap.port_note = PORT_NOTE
        .lock()
        .map(|note| note.clone())
        .unwrap_or_default();
    snap.autostart = crate::autostart::is_enabled();
    snap.clipboard = config.clipboard.enabled;
    snap.inbox = config.transfer.inbox_dir().display().to_string();
    snap.port = PANEL_PORT.load(AtomicOrdering::SeqCst);
    // 权限由本进程（主程序）自判：CLI 探针是另一个二进制、另一个 cdhash，
    // TCC 只认主程序，它永远报 ✗，不能当判据
    snap.perms = panel::PermsView {
        capture: kvm::trusted(),
        inject: kvm::can_post(),
    };
    snap.transfer = transfer_note.to_string();
    snap.identity = profile_key.to_string();
    snap.display = config
        .profile_for(profile_key)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "—".to_string());
    snap.current = current.unwrap_or(0);
    snap.current_name = current
        .and_then(|value| config.input_name_of(profile_key, value))
        .map(|name| config.label_for(&name))
        .unwrap_or_default();

    snap.peers = peers
        .iter()
        .map(|peer| panel::PeerView {
            node: peer.node.clone(),
            addr: peer.addr.ip().to_string(),
            input: peer.own_input.clone(),
            label: if peer.own_input.is_empty() {
                String::new()
            } else {
                config.label_for(&peer.own_input)
            },
            value: peer.own_value,
            paired: peer.paired,
            knows_port: peer.knows_port(),
            needs_pair: peer.wants_pair && !config.pair.is_set(),
            kvm_on: peer.kvm_on,
        })
        .collect();

    let to_view = |row: &PortRow| panel::InputView {
        name: row.name.clone(),
        label: row.label.clone(),
        value: row.value,
        current: row.current,
        presence: match row.presence {
            Presence::Occupied => "occupied",
            Presence::Idle => "idle",
            Presence::Invalid => "invalid",
            Presence::Unknown => "unknown",
        }
        .to_string(),
        verified: row.verified,
    };
    // 面板只列有本机证据的口；其余（固件声明过但没验证、也没信号）留在
    // inputs_hidden 里仅供 /api/state 排查，界面上一律不出现 —— 包括不再
    // 提供"仍然显示"的入口。要用它们：接了新设备就扫描，换显示器就重新检测。
    let (shown, hidden): (Vec<PortRow>, Vec<PortRow>) = port_rows(config, profile_key, current, true)
        .into_iter()
        .partition(PortRow::has_evidence);
    snap.inputs = shown.iter().map(to_view).collect();
    snap.inputs_hidden = hidden.iter().map(to_view).collect();
}

fn status_text(switcher: &mut Switcher) -> String {
    let displays = switcher.displays();
    if displays.is_empty() {
        return "本机没有接通的显示器（画面在对面那台）".to_string();
    }
    let display = &displays[0];
    // 静默期（显示器刚切回来）用记忆里的口拼状态，绝不再发 DDC 读
    if switcher.ddc_quiet() {
        let known = if switcher.config.own_input.is_empty() {
            format!("输入源 0x{:02X}", switcher.config.own_value)
        } else {
            switcher.config.own_input.clone()
        };
        return format!("{} · {}", display.name, known);
    }
    let cap = switcher.capability(display);
    let known = switcher
        .current_input_name(display)
        .unwrap_or_else(|| "输入源未知".to_string());
    let read_ok = switcher.backend().get_vcp(display, VCP_BRIGHTNESS).is_some();
    if read_ok {
        format!("{} · {}", display.name, known)
    } else {
        format!("{} · {} {}", display.name, cap.mark(), "仅可写")
    }
}

fn parse_hotkey(spec: &str) -> Option<HotKey> {
    let mut modifiers = Modifiers::empty();
    let mut code = None;
    for token in spec.to_lowercase().split('+') {
        match token.trim() {
            "ctrl" | "control" => modifiers |= Modifiers::CONTROL,
            "alt" | "option" => modifiers |= Modifiers::ALT,
            "shift" => modifiers |= Modifiers::SHIFT,
            "cmd" | "super" | "win" | "meta" => modifiers |= Modifiers::SUPER,
            "" => {}
            other => code = Some(code_of(other)?),
        }
    }
    code.map(|c| HotKey::new(Some(modifiers), c))
}

/// 键名 → 键码。覆盖字母、数字、F1–F12，够录制任意常用组合。
fn code_of(token: &str) -> Option<Code> {
    Some(match token {
        "space" => Code::Space,
        "f1" => Code::F1,
        "f2" => Code::F2,
        "f3" => Code::F3,
        "f4" => Code::F4,
        "f5" => Code::F5,
        "f6" => Code::F6,
        "f7" => Code::F7,
        "f8" => Code::F8,
        "f9" => Code::F9,
        "f10" => Code::F10,
        "f11" => Code::F11,
        "f12" => Code::F12,
        "0" => Code::Digit0,
        "1" => Code::Digit1,
        "2" => Code::Digit2,
        "3" => Code::Digit3,
        "4" => Code::Digit4,
        "5" => Code::Digit5,
        "6" => Code::Digit6,
        "7" => Code::Digit7,
        "8" => Code::Digit8,
        "9" => Code::Digit9,
        "a" => Code::KeyA,
        "b" => Code::KeyB,
        "c" => Code::KeyC,
        "d" => Code::KeyD,
        "e" => Code::KeyE,
        "f" => Code::KeyF,
        "g" => Code::KeyG,
        "h" => Code::KeyH,
        "i" => Code::KeyI,
        "j" => Code::KeyJ,
        "k" => Code::KeyK,
        "l" => Code::KeyL,
        "m" => Code::KeyM,
        "n" => Code::KeyN,
        "o" => Code::KeyO,
        "p" => Code::KeyP,
        "q" => Code::KeyQ,
        "r" => Code::KeyR,
        "s" => Code::KeyS,
        "t" => Code::KeyT,
        "u" => Code::KeyU,
        "v" => Code::KeyV,
        "w" => Code::KeyW,
        "x" => Code::KeyX,
        "y" => Code::KeyY,
        "z" => Code::KeyZ,
        _ => return None,
    })
}

/// 录制时把物理键码转成规范的键名。
fn key_token(code: KeyCode) -> Option<&'static str> {
    Some(match code {
        KeyCode::Space => "space",
        KeyCode::F1 => "f1",
        KeyCode::F2 => "f2",
        KeyCode::F3 => "f3",
        KeyCode::F4 => "f4",
        KeyCode::F5 => "f5",
        KeyCode::F6 => "f6",
        KeyCode::F7 => "f7",
        KeyCode::F8 => "f8",
        KeyCode::F9 => "f9",
        KeyCode::F10 => "f10",
        KeyCode::F11 => "f11",
        KeyCode::F12 => "f12",
        KeyCode::Digit0 => "0",
        KeyCode::Digit1 => "1",
        KeyCode::Digit2 => "2",
        KeyCode::Digit3 => "3",
        KeyCode::Digit4 => "4",
        KeyCode::Digit5 => "5",
        KeyCode::Digit6 => "6",
        KeyCode::Digit7 => "7",
        KeyCode::Digit8 => "8",
        KeyCode::Digit9 => "9",
        KeyCode::KeyA => "a",
        KeyCode::KeyB => "b",
        KeyCode::KeyC => "c",
        KeyCode::KeyD => "d",
        KeyCode::KeyE => "e",
        KeyCode::KeyF => "f",
        KeyCode::KeyG => "g",
        KeyCode::KeyH => "h",
        KeyCode::KeyI => "i",
        KeyCode::KeyJ => "j",
        KeyCode::KeyK => "k",
        KeyCode::KeyL => "l",
        KeyCode::KeyM => "m",
        KeyCode::KeyN => "n",
        KeyCode::KeyO => "o",
        KeyCode::KeyP => "p",
        KeyCode::KeyQ => "q",
        KeyCode::KeyR => "r",
        KeyCode::KeyS => "s",
        KeyCode::KeyT => "t",
        KeyCode::KeyU => "u",
        KeyCode::KeyV => "v",
        KeyCode::KeyW => "w",
        KeyCode::KeyX => "x",
        KeyCode::KeyY => "y",
        KeyCode::KeyZ => "z",
        _ => return None,
    })
}

/// 把当前按下的修饰键 + 主键拼成配置里的写法。
/// 必须带至少一个修饰键，否则会把普通打字也吃成热键。
fn compose_spec(mods: ModifiersState, token: &str) -> Option<String> {
    let mut parts = Vec::new();
    if mods.control_key() {
        parts.push("ctrl");
    }
    if mods.alt_key() {
        parts.push("alt");
    }
    if mods.shift_key() {
        parts.push("shift");
    }
    if mods.super_key() {
        parts.push("cmd");
    }
    if parts.is_empty() {
        return None;
    }
    parts.push(token);
    Some(parts.join("+"))
}

/// 换一个全局热键：先注册新的，成功再注销旧的；失败则把旧的装回去。
fn apply_hotkey(
    manager: &mut Option<GlobalHotKeyManager>,
    current: &mut Option<HotKey>,
    hotkey_id: &mut Option<u32>,
    spec: &str,
) -> Result<(), String> {
    let Some(next) = parse_hotkey(spec) else {
        return Err(format!("不认识的组合键：{}", spec));
    };
    let Some(mgr) = manager.as_mut() else {
        return Err("热键管理器没有初始化".to_string());
    };
    if let Some(old) = current.as_ref() {
        if old.id() == next.id() {
            return Ok(());
        }
    }
    let previous = current.take();
    match mgr.register(next) {
        Ok(()) => {
            if let Some(old) = previous {
                let _ = mgr.unregister(old);
            }
            *current = Some(next);
            *hotkey_id = Some(next.id());
            Ok(())
        }
        Err(err) => {
            if let Some(old) = previous {
                let _ = mgr.register(old);
                *current = Some(old);
            }
            Err(format!("这个组合被系统或别的程序占用了：{}", err))
        }
    }
}

/// 把"本机现在占哪个口"同步给局域网广播。
///
/// 只有屏幕在本机时才读得到 VCP 0x60；读不到就保持上一次的值，
/// 对面仍然能看到"这台机器上次占的是哪个口"。
///
/// 返回本次读到的当前输入值（读不到返回 None）。不管有没有落盘，
/// 调用方都可以拿它判断"画面现在在哪"。
fn sync_own(switcher: &mut Switcher, mesh: &Mesh) -> Option<u16> {
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
    let sending = kvm_now().map_or(false, |h| h.state() == kvm::State::Sending);
    let Some(display) = switcher.pick_display(None) else {
        return None;
    };
    let Some(value) = switcher.backend().get_vcp(&display, VCP_INPUT_SOURCE) else {
        return None;
    };
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
fn inspect(switcher: &Switcher) -> (String, String) {
    let Some(display) = switcher.pick_display(None) else {
        return (String::new(), "屏幕不在本机，无法检测".to_string());
    };
    let caps = switcher.capabilities(&display);
    let report = crate::support::check(switcher.backend(), &display, caps);
    let verdict = report.verdict();

    let line = format!("{} {}", verdict.mark(), verdict.label());

    let mut detail = report.headline();
    if !verdict.advice().is_empty() {
        detail.push_str("。");
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

/// 把「对端没有回应」说到能照着动手的程度。
///
/// `link_alive` 只看**共用键鼠的心跳包**。对端程序开着、但它的共用键鼠
/// 关着的时候，心跳一个都不会来 —— 表现和对端关机、不在同一网段、
/// 防火墙拦了**完全一样**。announce 里带了 `kvm_on` 能力位，于是这两种
/// 情况现在能分开说：让人去对面把开关打开，而不是去查网线。
fn explain_silent_peer(mesh: &Mesh, node: Option<&str>, err: &str) -> String {
    let peers = mesh.live_peers();
    let hit = match node {
        Some(n) => peers.iter().find(|p| p.node == n),
        // 不知道是哪台（follow 链路里唯一那条）时：在线的那几台里
        // 只要有键鼠没开的，它就是最可疑的那个
        None => peers.iter().find(|p| !p.kvm_on),
    };
    match hit {
        Some(p) if !p.kvm_on => format!("{err}（{} 的程序在线，是它的共用键鼠没开）", p.node),
        _ => err.to_string(),
    }
}

/// 一条链路的方向（`follow_target` 用的是同一条判据：第一条 Follow 链路）。
fn follow_link_node(handle: &kvm::Kvm) -> Option<String> {
    handle
        .links()
        .iter()
        .find(|l| l.mode == kvm::Mode::Follow)
        .map(|l| l.node.clone())
}

/// 这次切屏给键鼠留下了什么结果 —— 成功文案的**唯一**依据。
///
/// 原来是 `handed / reclaimed / stranded` 三个互不相干的 bool：任何一个
/// 分支漏设一个，就会落到"（键鼠已跟过去）"这个默认结论上。实测暴露的
/// 三处谎报（Receiving 下 `follow()` 空转被当成功、`value == own` 时
/// `give_back()` 对 Receiving 是 no-op、纯 edge 拓扑根本没交接）全是
/// 同一个成因。改成枚举后，编译器逼着每个分支表态。
#[derive(PartialEq)]
enum Keys {
    /// 没动过，键鼠本来就在本机
    Home,
    /// 共用键鼠没开
    Off,
    /// 这次真的开始转发了
    Sent,
    /// 键鼠本来就在对端手上，这次没有重复交接
    AtPeer(String),
    /// 键鼠在对端，我们刚发了 Takeback —— 只能承诺"请了"，不能断言收回
    AskBack,
    /// 真的从转发里收回本机（`give_back` 生效）
    Back,
    /// 想交没交成
    Stranded(String),
}

/// 工作线程：独占一个 Switcher，串行处理 DDC 指令。
/// 执行一次切屏，并按结果决定键鼠跟不跟过去。
///
/// `follow = false` 是"只想换个画面"，比如自己接了两个显示口想切着看，
/// 或者故意让键鼠留在本机操作。这时候不能顺手把键鼠也带走。
/// 自动恢复失败后的等待时间（秒）：1 → 5 → 15 → 60 → 300，之后固定 300。
///
/// 为什么不是指数退避：副机开机时的失败几乎都是"网络还没起 / 对端还没上线"，
/// 几十秒内就会好；指数退避会让它几分钟都不再试，用户看到的就是"自启没生效"。
/// 也不能一秒一次：真失败（缺授权）会刷屏，还会反复弹系统设置页。
fn resume_backoff(failures: u32) -> u64 {
    match failures {
        0 | 1 => 1,
        2 => 5,
        3 => 15,
        4 => 60,
        _ => 300,
    }
}

fn do_switch(
    switcher: &mut Switcher,
    mesh: &Mesh,
    name: &str,
    value: u16,
    follow: bool,
) -> String {
    // 切走之前先把自己的口记下来，切完就读不到了
    let _ = sync_own(switcher, mesh);
    let own = switcher.config.own_value;

    // ---- 顺序很关键：**先交键鼠，再切画面** ----
    //
    // 反过来（先切画面、再交键鼠）是条死路：画面切走之后本机既读不到也
    // 写不了 DDC（实测 9/9 失败），而面板也跟着看不见了 —— 用户没有
    // 任何地方可以点"把键鼠交过去"。所以这一屏的键鼠必须在这里、在
    // 画面离开之前就交出去。
    //
    // 但键鼠交不出去时**不拒绝切换**：硬拒切的代价是"开了共用键鼠就连
    // 画面都切不了"（实测反馈）。降级放行，只是把真实结果说清楚。
    let mut keys = Keys::Home;
    if follow {
        match kvm_now() {
            // 共用键鼠没开：键鼠物理上在本机，但用户以为它会跟 —— 说清楚
            None => keys = Keys::Off,

            // 切回自己占的口（对端请我们切回去，或热键目标就是本机）：
            // 键鼠必须回本机，这跟「交给对端」是相反方向的同一个动作
            Some(handle) if value == own => match handle.state() {
                // 只有真的握着键鼠时 give_back 才动（release 只处理 Sending）
                kvm::State::Sending => {
                    keys = if handle.give_back() {
                        Keys::Back
                    } else {
                        Keys::Home
                    };
                }
                // 被控时 give_back() 是**空操作**，原来却照样说「已收回本机」。
                // 能要回来的只有 Takeback，而它是异步的：对端收到才松手，
                // 本机不能替它宣布已经收回。
                kvm::State::Receiving => {
                    keys = if handle.take_back_from_peer("画面切回本机") {
                        Keys::AskBack
                    } else {
                        Keys::Home
                    };
                }
                kvm::State::Idle => keys = Keys::Home,
            },

            // 切到对端，且配了"跟随画面"链路（共用一块屏的那台）
            Some(handle) if handle.has_follow_link() => {
                // **对端不在网上时，画面一步都不许动。**
                //
                // 切画面是单行道：走完之后本机既读不到也写不了 DDC（实测
                // 9/9 失败），画面又不在本机 —— 用户看不见光标、也没法操作
                // 对端，只能伸手去按显示器按钮。以前这里"降级放行"，结果是
                // 用户报「切过去键鼠用不了」（v0.6.62 实测：那台机器整机不在
                // 网上，ping 100% 丢、445 连不上，键鼠根本没处可交）。
                //
                // 对端真离线时切过去也看不到东西（它没有信号输出），所以
                // 硬拒不会挡住"我要看那台的屏幕"这种需求。
                if let Some(node) = follow_link_node(&handle) {
                    if handle.link_alive_by_node(&node) == Some(false) {
                        return format!(
                            "{node} 不在线，画面没有切 —— 切过去你就看不见本机了，\
                             键鼠也没处可交。先确认那台机器开着、和本机同一网段、\
                             防火墙没把程序拦了（Windows 网络位置若是「公用」会全拦）"
                        );
                    }
                }
                match handle.state() {
                    kvm::State::Idle => match handle.follow() {
                        Ok(true) => keys = Keys::Sent,
                        // 刚好被别的线程抢先建了会话：照实说键鼠在哪台
                        Ok(false) => {
                            keys = Keys::AtPeer(
                                handle.active_node().unwrap_or("对端").to_string(),
                            );
                        }
                        Err(err) => {
                            // 「没有回应」有两种截然不同的原因，说错就白查网线
                            let target = follow_link_node(&handle);
                            keys = Keys::Stranded(explain_silent_peer(
                                mesh,
                                target.as_deref(),
                                &err,
                            ));
                        }
                    },
                    // 已经在往外送：follow() 这时是空转，不能当"刚交过去"
                    kvm::State::Sending => {
                        keys = Keys::AtPeer(handle.active_node().unwrap_or("对端").to_string());
                    }
                    // 键鼠在对端手上（它在控制本机）。画面要切到对面去，得先
                    // 请它松手，否则它的按键还会继续打进本机。
                    kvm::State::Receiving => {
                        keys = if handle.take_back_from_peer("切到对端前先收回键鼠") {
                            Keys::AskBack
                        } else {
                            Keys::AtPeer("对端".to_string())
                        };
                    }
                }
            }

            // 没有"跟随画面"链路（多屏共享）：切画面跟键鼠无关，一律不动。
            // 但**不能**因此说"键鼠已跟过去" —— 原来是这里默认成的。
            Some(handle) => {
                keys = match handle.state() {
                    kvm::State::Idle => Keys::Home,
                    kvm::State::Sending | kvm::State::Receiving => {
                        Keys::AtPeer(handle.active_node().unwrap_or("对端").to_string())
                    }
                };
            }
        }
    }

    // 8 秒校验窗：实测这台显示器从收到 DDC 到离开本机拓扑要 2~6 秒不等，
    // 2 秒窗口会频繁误判失败，进而把刚交出去的键鼠收回来
    let outcome = switcher.switch_raw(name, value, Duration::from_secs(8));

    // 失败分两种，处理完全不同：
    //
    // 确凿失败（那个口没信号，显示器自己弹回来了）：画面明明在本机，
    // 键鼠必须立刻回本机。
    //
    // 模糊失败（校验窗超时 + VCP 读不到目标值）：这台显示器切到别的输入源
    // 后**并不离开 macOS 拓扑**（EDID 一直活着），VCP 读值也不更新——两个
    // 成功信号全部失效，但画面实际是切成功的（18:00 日志实锤：转发了
    // 1625 笔事件后误判失败、键鼠被抢收，用户看到「刚切过去能用，过一会
    // 就不能用」）。所以模糊失败时键鼠留在对端不动；真没切过去的话，
    // 用户按热键（Ctrl+Alt+Esc）收回兜底，转发中热键实测可用。
    let mut handed_ambiguous = false;
    if keys == Keys::Sent && !outcome.ok {
        if outcome.definite {
            // 确凿失败：画面明明在本机，键鼠必须立刻回本机
            if let Some(handle) = kvm_now() {
                if handle.give_back() {
                    keys = Keys::Back;
                }
            }
        } else {
            switcher.suppress_ddc(Duration::from_secs(4));
            handed_ambiguous = true;
        }
    }

    let hotkey = pretty_hotkey(&switcher.config.hotkey);
    if outcome.ok || handed_ambiguous {
        if handed_ambiguous {
            return format!(
                "键鼠已交给对端；显示器切换没能确认（{}）。\
                 若画面还在本机，按热键 {hotkey} 可收回键鼠",
                outcome.message
            );
        }
        match keys {
            Keys::Sent => format!("已切到 {name}（键鼠已跟过去）"),
            Keys::AtPeer(node) => {
                format!("已切到 {name}；键鼠已经在 {node}，这次没有重复交接")
            }
            Keys::AskBack => {
                format!("已切到 {name}；键鼠在对端，已请它交还 —— 对面收到后就能用")
            }
            // 只有画面回到本机口，才谈得上"切回"
            Keys::Back if value == own => format!("画面已切回 {name}（键鼠已收回本机）"),
            Keys::Back => format!("已切到 {name}（键鼠已收回本机）"),
            Keys::Stranded(err) => format!(
                "已切到 {name}，但键鼠留在本机（{err}）。按热键 {hotkey} 可把画面切回来"
            ),
            Keys::Off => format!("已切到 {name}（共用键鼠没开，键鼠留在本机）"),
            Keys::Home if value == own => format!("画面已切回 {name}（键鼠就在本机）"),
            Keys::Home if follow => format!("已切到 {name}（键鼠留在本机）"),
            Keys::Home => format!("已切到 {name}（只切画面，键鼠留在本机）"),
        }
    } else if keys == Keys::Back {
        // 切回本机时键鼠已经收回来了，画面切没切成是另一回事，别吓唬人
        format!("键鼠已收回本机；{}", outcome.message)
    } else if keys == Keys::AskBack {
        // Takeback 已发，但对端还没确认 —— 不能替它宣布"已收回"
        format!("已请对端交还键鼠；{}", outcome.message)
    } else {
        outcome.message
    }
}

fn engine_loop(
    mut switcher: Switcher,
    rx: Receiver<Command>,
    proxy: EventLoopProxy<UserEvent>,
    mesh: Mesh,
) {
    /// 只在状态真的变了才往 UI 线程丢事件。
    ///
    /// 5 秒一次的自动刷新绝大多数时候结论完全一样，无条件重发会让 UI 层
    /// 每 5 秒重建一次菜单，macOS 上就表现为"菜单点开几秒后自己消失"。
    fn push(
        proxy: &EventLoopProxy<UserEvent>,
        switcher: &Switcher,
        target: &str,
        text: String,
        support: &str,
        last: &mut String,
    ) {
        let key = profile_key_of(switcher);
        // 顺便刷新"本机有没有显示器"：面板要靠它区分"没接显示器"和"显示器不报口"
        let has_display = !switcher.displays().is_empty();
        let had_display = HAS_DISPLAY.swap(has_display, AtomicOrdering::SeqCst);
        // 画面刚切回本机（用户按了显示器面板按钮、或对面替我们切回来了）：
        // 键鼠必须立刻回到本机，否则就变成"看着本机的画面、操作着对面"。
        if has_display && !had_display {
            if let Some(handle) = kvm_now() {
                handle.give_back();
            }
        }
        let sig = format!("{key}\u{1f}{target}\u{1f}{text}\u{1f}{support}");
        if *last == sig {
            return;
        }
        *last = sig;
        let _ = proxy.send_event(UserEvent::Status {
            text,
            target: target.to_string(),
            profile_key: key,
            support: support.to_string(),
        });
    }

    let mut last_push = String::new();
    // 上一轮见到的显示器名集合，心跳里用来发现拓扑变化（零 I2C 开销）
    let mut last_topology: Vec<String> = Vec::new();
    // 拓扑刚变过 → 等 DDC 静默期过去再自动检测（见 Heartbeat）
    let mut pending_detect: Option<Instant> = None;
    let start_target = switcher.config.hotkey_target.clone();
    let _ = sync_own(&mut switcher, &mesh);
    let text = status_text(&mut switcher);
    // 只算一次：写探测要 1 秒，不能跟着状态刷新反复跑
    let mut support = inspect(&switcher).0;
    push(&proxy, &switcher, &start_target, text, &support, &mut last_push);

    while let Ok(command) = rx.recv() {
        match command {
            Command::SwitchRaw { name, value } => {
                let text = do_switch(&mut switcher, &mesh, &name, value, true);
                log_line(&text);
                let target = switcher.config.hotkey_target.clone();
                push(&proxy, &switcher, &target, text, &support, &mut last_push);
            }
            Command::SwitchOnly { name, value } => {
                let text = do_switch(&mut switcher, &mesh, &name, value, false);
                log_line(&text);
                let target = switcher.config.hotkey_target.clone();
                push(&proxy, &switcher, &target, text, &support, &mut last_push);
            }
            Command::SetTarget { name, value } => {
                // 只设热键目标，不往 profile 里写没验证过的值：
                // 否则"未确认"标记会被标准值污染，下次就看不出哪个口是真的试出来的
                let _ = value;
                switcher.config.hotkey_target = name.clone();
                let _ = switcher.config.save();
                log_line(&format!("热键目标设为 {}", name));
                let text = format!("热键目标已设为 {}", switcher.config.label_for(&name));
                push(&proxy, &switcher, &name, text, &support, &mut last_push);
            }
            Command::ClearTarget => {
                switcher.config.hotkey_target.clear();
                let _ = switcher.config.save();
                push(
                    &proxy,
                    &switcher,
                    "",
                    "已取消热键目标".to_string(),
                    &support,
                    &mut last_push,
                );
            }
            Command::Rescan => {
                // 这一步必须真的重读：缓存里存着上一次的失败结论，
                // 不清掉的话这个按钮点了等于没点（用户报的就是这个）
                let report = switcher.rescan();
                log_line(&format!("重新检测：{report}"));
                if let Ok(mut note) = PORT_NOTE.lock() {
                    *note = report.clone();
                }
                let _ = sync_own(&mut switcher, &mesh);
                // ---- 重新检测的收尾：把画面切回本机口 ----
                //
                // 重新检测本身是只读的，但有些显示器被一串 DDC 读折腾之后会
                // 掉到别的输入源上 —— 实测掉到一个**没接主机**的口，画面直接
                // 黑了，而且再也不会自己回来（用户 2026-10-22 报的就是这个）。
                //
                // 判据要卡两道：①本机此刻接着显示器（`HAS_DISPLAY`）——
                // 画面在对面那台时读到的 0x60 是**对端的口**，照它"切回本机"
                // 会把对端的画面抢走；②读到的值和 own_value 不同才动手。
                if HAS_DISPLAY.load(AtomicOrdering::SeqCst) {
                    let own = switcher.config.own_value;
                    if own > 0 {
                        if let Some(display) = switcher.pick_display(None) {
                            if let Some(now) = switcher.backend().get_vcp(&display, VCP_INPUT_SOURCE) {
                                if now != own {
                                    let name = switcher.config.label_for(&switcher.config.own_input.clone());
                                    let ok = switcher.backend().set_vcp(&display, VCP_INPUT_SOURCE, own);
                                    log_line(&format!(
                                        "重新检测把画面留在了 0x{now:02X}（本机口是 0x{own:02X} {name}），{}",
                                        if ok { "已切回" } else { "切回失败，请手动切一次" }
                                    ));
                                    if ok {
                                        switcher.suppress_ddc(Duration::from_millis(1500));
                                    }
                                }
                            }
                        }
                    }
                }
                let target = switcher.config.hotkey_target.clone();
                let mut text = status_text(&mut switcher);
                if text.is_empty() {
                    text = report;
                }
                push(&proxy, &switcher, &target, text, &support, &mut last_push);
            }
            Command::PasteIntent => {
                // 用户在对端屏幕上按下了 Ctrl+V，那个键已经被抓取层吞掉。
                // 这里起线程把文件推过去 —— 绝不能阻塞本循环，心跳和切屏
                // 命令都还在后面排队。
                if CLIP_BUSY.swap(true, AtomicOrdering::SeqCst) {
                    log_line("上一批剪贴板文件还在传，这次粘贴先忽略");
                    continue;
                }
                let files = CLIP_PENDING
                    .lock()
                    .map(|g| g.clone())
                    .unwrap_or_default();
                if files.is_empty() {
                    CLIP_BUSY.store(false, AtomicOrdering::SeqCst);
                    if let Some(h) = kvm_now() {
                        h.set_paste_ready(false);
                        h.send_key_to_peer(0x09); // V
                    }
                    log_line("剪贴板里没有待传文件，这次粘贴按原样转发给对端");
                    continue;
                }
                let peers = mesh.live_peers();
                let want = switcher.config.kvm.peer_node.clone();
                let peer = peers
                    .iter()
                    .find(|p| p.node == want)
                    .cloned()
                    .or_else(|| peers.first().cloned());
                let Some(peer) = peer else {
                    CLIP_BUSY.store(false, AtomicOrdering::SeqCst);
                    if let Some(h) = kvm_now() {
                        h.set_paste_ready(false);
                        h.send_key_to_peer(0x09);
                    }
                    log_line("剪贴板传输失败：没有在线对端，这次粘贴按原样转发");
                    continue;
                };
                let ip = peer.addr.ip();
                let port = switcher.config.transfer.port;
                let node = switcher.config.node.clone();
                let key = crate::pair::link_key(&switcher.config.pair.code);
                let count = files.len();
                let peer_name = peer.node.clone();
                let proxy_t = proxy.clone();
                let proxy_p = proxy.clone();
                log_line(&format!(
                    "按需传输开始：{count} 个条目 → {peer_name}（正在整理清单并校验）"
                ));
                thread::spawn(move || {
                    let _ = proxy_t.send_event(UserEvent::Transfer("正在准备文件…".to_string()));
                    let mut last_pct = u64::MAX;
                    let res = transfer::send(
                        ip,
                        port,
                        &node,
                        &files,
                        key,
                        true, // 这批是剪贴板按需传输：收端收完要写剪贴板
                        |sent, total| {
                            let pct = if total == 0 { 100 } else { sent * 100 / total };
                            if pct != last_pct {
                                last_pct = pct;
                                let _ = proxy_p.send_event(UserEvent::Transfer(format!(
                                    "剪贴板传输… {pct}%"
                                )));
                            }
                        },
                    );
                    let _ = proxy_t.send_event(UserEvent::PasteDone(res));
                });
            }
            Command::Heartbeat => {
                // 拓扑变化（显示器离开/回到本机）只靠枚举就能发现，零 I2C 开销。
                // 刚回来的头几秒链路还在重新协商，静默期把 DDC 读取全部挡掉。
                let displays_now: Vec<String> = switcher
                    .displays()
                    .into_iter()
                    .map(|d| d.name)
                    .collect();
                if displays_now != last_topology {
                    switcher.suppress_ddc(Duration::from_secs(4));
                    last_topology = displays_now;
                    // 拓扑变了有两种可能：画面切走又回来（同一台显示器），
                    // 或者真的**换了台显示器**（拔插 / 换 KVM 口）。
                    // 后者必须按新显示器自己的能力表重新建表 —— 这正是用户
                    // 心智里的"先检测显示器，再列它的输入源"。现在就做不行：
                    // 链路还在重新协商，读能力表会拿到空值并被缓存住。
                    pending_detect = Some(Instant::now());
                }
                // 静默期一过补一次自动检测：只在这台显示器还没建过档、
                // 或者建过但一个口都没读到时才真去读。已有的表（哪怕只有
                // 实测出来的几个口）一律不动，绝不因为一次拔插就重置结论。
                if let Some(t0) = pending_detect {
                    if t0.elapsed() >= Duration::from_secs(5) {
                        pending_detect = None;
                        let needs = switcher.displays().iter().any(|d| {
                            let key = switcher.identity(d);
                            switcher
                                .config
                                .profile_for(&key)
                                .map(|p| p.inputs.is_empty())
                                .unwrap_or(true)
                        });
                        if needs {
                            let report = switcher.rescan();
                            log_line(&format!("换了显示器？自动检测：{report}"));
                        }
                    }
                }
                let current = sync_own(&mut switcher, &mesh);
                // 自证收回安全网：VCP 当前值已经是本机的口，而键鼠还在往外
                // 转发 —— 画面已经回本机了（对面代切、用户按了显示器按钮、
                // 或对端消息在网络里丢了），键鼠必须立刻回来。
                // 「拓扑离开→回来」信号对从不离开本机拓扑的显示器永远不
                // 触发（实测），这条 VCP 判据是唯一可靠的兜底。
                // 注意要在 sync_own 之后比：刚才那次读值如果发现画面回本机，
                // 会顺手把 own 修正成正确值，陈旧的污染值不会误伤判断。
                // ⚠️ 只在跟随画面模式下有效：多屏共享（edge）模式显示器根本
                // 不切换输入源，VCP 永远等于自己的口 —— 不加这个守卫，转发
                // 最多 5 秒就会被误收回来（实测踩过）。
                // ⚠️ 模式要读 KVM 运行时实例的 mode()，不能读
                // switcher.config —— 菜单/面板改的是 UI 线程那份 config，
                // switcher 这份是启动时的旧副本，永远比对新（实测踩过）。
                if let Some(handle) = kvm_now() {
                    if handle.state() == kvm::State::Sending && !handle.active_is_edge() {
                        if let Some(v) = current {
                            if v > 0 && v == switcher.config.own_value {
                                handle.give_back();
                            }
                        }
                    }
                }
                let target = switcher.config.hotkey_target.clone();
                let text = status_text(&mut switcher);
                push(&proxy, &switcher, &target, text, &support, &mut last_push);
            }
            Command::AskPeerSwitch { value } => {
                // 本机没有接通的显示器，自己发不出 DDC；但对面那台现在握着画面，
                // 它的 DDC 是通的 —— 让它替我们把画面切到这个口。
                let peers = mesh.live_peers();
                let mut sent = 0usize;
                // 发不出去的原因要说出来。以前 send_command 只回 bool，"对端要码
                // 而本机没设"这种一定被丢的情况也算发出去了，界面就会谎报"几秒后
                // 屏幕会回来"——用户只能对着黑屏猜。
                let mut why: Vec<String> = Vec::new();
                for peer in &peers {
                    match mesh.send_command(
                        &peer.node,
                        "switch",
                        serde_json::json!({ "input": "", "value": value }),
                    ) {
                        Ok(()) => {
                            sent += 1;
                            log_line(&format!("已请 {} 把画面切到 0x{value:02X}", peer.node));
                        }
                        Err(err) => why.push(err),
                    }
                }
                let text = if sent == 0 {
                    if why.is_empty() {
                        "没有在线对端可以代切".to_string()
                    } else {
                        format!("切不了：{}", why.join("；"))
                    }
                } else {
                    format!("已请对端把画面切到 0x{value:02X}，几秒后屏幕会回来")
                };
                let target = switcher.config.hotkey_target.clone();
                push(&proxy, &switcher, &target, text, &support, &mut last_push);
            }
            Command::RemoteClipboard(text) => {
                // 写剪贴板得回主线程，这里只负责转交
                let _ = proxy.send_event(UserEvent::RemoteClipboard(text));
            }
            Command::CheckSupport => {
                let (line, detail) = inspect(&switcher);
                support = line;
                log_line(&format!("支持性检测：{}", detail));
                let target = switcher.config.hotkey_target.clone();
                push(&proxy, &switcher, &target, detail, &support, &mut last_push);
            }
            Command::ScanPorts => {
                let target = switcher.config.hotkey_target.clone();
                log_line("开始扫描输入口");

                // 物理试探有个硬前提：**切走之后得有谁把画面送回来**。
                // 本机一旦失去 DDC 就再也发不出指令，这个前提不成立时切过去就是单程票。
                let peers_now = mesh.live_peers();
                let local_paired = switcher.config.pair.is_set();
                let blocked: Vec<String> = peers_now
                    .iter()
                    .filter(|p| p.wants_pair && !local_paired)
                    .map(|p| p.node.clone())
                    .collect();
                let can_relay = !peers_now.is_empty() && blocked.is_empty();
                let skip_reason: Option<String> = if peers_now.is_empty() {
                    Some("局域网里没有别的机器在跑本程序，切过去就回不来".to_string())
                } else if !blocked.is_empty() {
                    Some(format!(
                        "{} 设了配对码而本机没设，它会把代切请求当成未签名的包丢掉 —— \
                         两边填同一个码再来",
                        blocked.join("、")
                    ))
                } else {
                    None
                };

                push(
                    &proxy,
                    &switcher,
                    &target,
                    if can_relay {
                        "正在扫描，屏幕会黑几次…".to_string()
                    } else {
                        "正在汇总已知信息（不做切屏试探）…".to_string()
                    },
                    &support,
                    &mut last_push,
                );

                // 对面自己报过的口直接采信，这些不用切屏试探
                let peer_ports: Vec<(String, u16)> = peers_now
                    .iter()
                    .filter(|p| p.knows_port())
                    .map(|p| (p.node.clone(), p.own_value))
                    .collect();
                if !peer_ports.is_empty() {
                    log_line(&format!("局域网里有 {} 台对端主动报了口", peer_ports.len()));
                }
                if let Some(reason) = &skip_reason {
                    log_line(&format!("跳过切屏试探：{reason}"));
                }

                let relay_mesh = mesh.clone();
                // 空切片 = 一个口都不物理试探，只汇总本机与对端已经报过的信息
                let no_probe: [u16; 0] = [];
                let only = if can_relay {
                    None
                } else {
                    Some(no_probe.as_slice())
                };
                let result = switcher.scan_presence(
                    &peer_ports,
                    Duration::from_secs(10),
                    true,
                    only,
                    |home| {
                        // 对面要是会丢弃未签名包，发也白发；直接说没送出去
                        if !can_relay {
                            return false;
                        }
                        let nodes: Vec<String> = relay_mesh
                            .live_peers()
                            .into_iter()
                            .map(|p| p.node)
                            .collect();
                        let mut sent = false;
                        for node in nodes {
                            if relay_mesh
                                .send_command(
                                    &node,
                                    "switch",
                                    serde_json::json!({ "input": "", "value": home }),
                                )
                                .is_ok()
                            {
                                sent = true;
                            }
                        }
                        sent
                    },
                    |step| log_line(step),
                );

                let text = match result {
                    Ok(report) => {
                        let occupied: Vec<String> = report
                            .ports
                            .iter()
                            .filter(|p| p.presence == Presence::Occupied)
                            .map(|p| p.name.clone())
                            .collect();
                        let idle = report
                            .ports
                            .iter()
                            .filter(|p| p.presence == Presence::Idle)
                            .count();
                        let mut text = if occupied.is_empty() {
                            "扫描完成：没找到有主机的口".to_string()
                        } else {
                            format!("扫描完成：{} 有主机", occupied.join("、"))
                        };
                        if idle > 0 {
                            text.push_str(&format!("，{} 个口无信号", idle));
                        }
                        if !report.back_home {
                            text.push_str(
                                "。画面没能自动切回本机 —— 要么在对面那台点一下切换，\
                                 要么用显示器 OSD 手动切回来",
                            );
                        }
                        if let Some(reason) = &skip_reason {
                            text.push_str(&format!("。这次没有切屏试探：{reason}"));
                        }
                        if let Some(reason) = &report.aborted {
                            log_line(reason);
                        }
                        text
                    }
                    Err(err) => format!("扫描失败：{}", err),
                };
                // 扫描刚结束时 DDC 常有一段静默期，这时重跑体检会误判成"通道不通"，
                // 所以只把自己的口同步一下，体检结论沿用原来的。
                let _ = sync_own(&mut switcher, &mesh);
                log_line(&text);
                push(&proxy, &switcher, &target, text, &support, &mut last_push);
            }
        }
    }
}

pub fn run() -> ! {
    // panic 一律先落日志再走原有流程：release 配置是 panic=abort，
    // 没有这条的话 abort 之后连 panic 内容都查不到（托盘菜单点击崩溃
    // 悬了三轮才靠符号化定位，有这条下次直接看日志）
    {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            log_line(&format!("PANIC: {info}"));
            default_hook(info);
        }));
    }

    let switcher = Switcher::new();
    let config_snapshot = switcher.config.clone();

    // 局域网：既是"对面那台替我做不了的事"，也是"哪台机器在哪个口"的信息来源
    let ddc_ok = switcher.pick_display(None).is_some();
    let mesh = Mesh::new(
        config_snapshot.node.clone(),
        config_snapshot.network.port,
        ddc_ok,
    );
    // 有配对码就立刻上签名：之后的广播和指令都要过 HMAC 校验
    mesh.set_key(crate::pair::link_key(&config_snapshot.pair.code));

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    // 热键必须在本线程注册，manager 必须活到进程结束；
    // 用户在菜单里改热键时要能注销旧的、注册新的，所以三个状态都得可变。
    let mut hotkey_id: Option<u32> = None;
    let mut current_hotkey: Option<HotKey> = None;
    let mut hotkey_manager: Option<GlobalHotKeyManager> = None;
    {
        if let Some(hotkey) = parse_hotkey(&switcher.config.hotkey) {
            if let Ok(created) = GlobalHotKeyManager::new() {
                if created.register(hotkey).is_ok() {
                    hotkey_id = Some(hotkey.id());
                    current_hotkey = Some(hotkey);
                }
                hotkey_manager = Some(created);
            }
        }
    }

    // 录制新热键用的小窗口：平时隐藏，点"按下新的组合键"时才弹出来抢焦点。
    // tao 不能只监听全局按键，必须有个窗口来收键盘事件。
    let record_window: Option<Window> = WindowBuilder::new()
        .with_title("按下新的组合键（Esc 取消）")
        .with_inner_size(LogicalSize::new(420.0, 120.0))
        .with_resizable(false)
        .with_always_on_top(true)
        .with_visible(false)
        .build(&event_loop)
        .ok();
    if record_window.is_some() {
        log_line("热键录制窗口已就绪");
    } else {
        log_line("警告：热键录制窗口创建失败，菜单里只能选预设组合");
    }

    let (tx, rx) = mpsc::channel::<Command>();

    if config_snapshot.network.enabled {
        let tx_relay = tx.clone();
        let started = mesh.start(Box::new(move |from, action, args| match action.as_str() {
            "switch" => {
                if let Some(input) = args.get("input").and_then(|v| v.as_str()) {
                    let value = args.get("value").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                    let _ = tx_relay.send(Command::SwitchRaw {
                        name: input.to_string(),
                        value,
                    });
                    log_line(&format!("收到 {from} 的切屏指令：{input}={value}"));
                }
            }
            "clipboard" => {
                if let Some(text) = args.get("text").and_then(|v| v.as_str()) {
                    let _ = tx_relay.send(Command::RemoteClipboard(text.to_string()));
                }
            }
            _ => {}
        }));
        if started.is_err() {
            log_line("局域网监听启动失败，仅本机模式");
        }
    }

    // 控制面板：浏览器就是 UI，跨平台一致，不用再引一套 GUI 框架
    let snapshot: Arc<Mutex<Snapshot>> = Arc::new(Mutex::new(Snapshot::default()));
    {
        let snapshot = snapshot.clone();
        let tx_panel = tx.clone();
        let proxy_panel = proxy.clone();
        let dispatch: panel::Dispatcher = Arc::new(move |action: Action| match action {
            Action::Switch { name, value, follow } => {
                // 只换画面时不带走键鼠：用户可能就是想在本机继续敲命令，
                // 顺手把键鼠也扔过去会让他连本机都操作不了
                let cmd = if follow {
                    Command::SwitchRaw { name, value }
                } else {
                    Command::SwitchOnly { name, value }
                };
                let _ = tx_panel.send(cmd);
            }
            Action::SetTarget { name, value } => {
                let _ = tx_panel.send(Command::SetTarget { name, value });
            }
            Action::ClearTarget => {
                let _ = tx_panel.send(Command::ClearTarget);
            }
            Action::Rescan => {
                let _ = tx_panel.send(Command::Rescan);
            }
            Action::ScanPorts => {
                let _ = tx_panel.send(Command::ScanPorts);
            }
            Action::CheckSupport => {
                let _ = tx_panel.send(Command::CheckSupport);
            }
            Action::AskPeer(value) => {
                let _ = tx_panel.send(Command::AskPeerSwitch { value });
            }
            // 其余的要在主线程做（开对话框、改配置、起浏览器）
            other => {
                let _ = proxy_panel.send_event(UserEvent::Panel(other));
            }
        });
        match panel::start(snapshot.clone(), dispatch, 47833) {
            Ok(port) => {
                PANEL_PORT.store(port, AtomicOrdering::SeqCst);
                log_line(&format!("控制面板：http://127.0.0.1:{port}/"));
            }
            Err(err) => log_line(&format!("控制面板启动失败：{err}")),
        }
    }

    // 文件接收服务
    let inbox_shared: Arc<Mutex<PathBuf>> =
        Arc::new(Mutex::new(config_snapshot.transfer.inbox_dir()));
    // 钥匙放在 `if` 外面声明：面板改配对码时要就地换掉它（见 `Action::Pair`）。
    // 文件服务没法像 KVM 那样"重启一下"——旧 TcpListener 还占着端口，
    // 重新 bind 必然 EADDRINUSE，所以只能让 key 变成共享可变。
    let transfer_key: Arc<Mutex<Option<[u8; 32]>>> =
        Arc::new(Mutex::new(crate::pair::link_key(&config_snapshot.pair.code)));
    if config_snapshot.transfer.enabled {
        let proxy_saved = proxy.clone();
        let inbox_for_log = config_snapshot.transfer.inbox_dir();
        match transfer::serve(
            inbox_shared.clone(),
            config_snapshot.transfer.port,
            transfer_key.clone(),
            |message| log_line(&format!("[传输] {message}")),
            move |saved, clip| {
                let _ = proxy_saved.send_event(UserEvent::Saved(saved, clip));
            },
        ) {
            Ok(()) => log_line(&format!("接收目录：{}", inbox_for_log.display())),
            Err(err) => log_line(&format!("文件接收服务启动失败：{err}")),
        }
    }

    {
        let proxy = proxy.clone();
        let mesh = mesh.clone();
        thread::spawn(move || engine_loop(switcher, rx, proxy, mesh));
    }
    log_line(&format!(
        "启动：热键={} 目标={} 节点={} 构建=v0.6.76",
        if hotkey_id.is_some() { "已注册" } else { "注册失败" },
        config_snapshot.hotkey_target,
        config_snapshot.node
    ));

    let mut tray: Option<TrayIcon> = None;
    let mut status = "正在检测显示器…".to_string();
    let mut target = config_snapshot.hotkey_target.clone();
    let mut config = config_snapshot;
    let mut profile_key = String::new();
    let mut support = String::new();
    // 上一次画出来的菜单长什么样。内容没变就不重建 —— 见 menu_signature 的注释。
    let mut menu_sig = String::new();
    let mut last_rescan = Instant::now();
    let mut recording = false;
    let mut record_mods = ModifiersState::empty();
    // 剪贴板：先取一次基线，否则启动瞬间会把用户手上已有的内容当成"新复制"
    let mut watcher = Watcher::default();
    let _ = watcher.poll();
    let mut transfer_note = String::new();
    // 控制面板：第一次点菜单才建窗口，之后关闭只是隐藏，再点秒开
    let mut panel_win: Option<Panel> = None;
    // 共用键鼠：开着的时候它会一直占着 CGEventTap / 低级钩子
    let mut kvm_handle: Option<Arc<kvm::Kvm>> = None;
    // 自动恢复的重试状态（v0.6.66）。
    //
    // 以前这里是 `kvm_restored: bool`，一次失败就永久放弃 —— 对没有键鼠的
    // 副机是死局：它开机比主机早、网络还没起、第一次授权没生效，任何一种
    // 都会让它永远停在空闲，而人没法去那台机器上点菜单。
    let mut resume_failures: u32 = 0;
    let mut resume_next: Option<Instant> = None;
    // 用户点了面板窗口的关闭按钮：等这轮事件处理完再真正释放
    let mut panel_stale = false;

    event_loop.run(move |event, elwt, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(400));

        match event {
            Event::NewEvents(StartCause::Init) => {
                let peers = mesh.live_peers();
                let menu = build_menu(
                    &status,
                    &config,
                    &peers,
                    own_value(&mesh),
                    &profile_key,
                    &support,
                );
                let mut builder = TrayIconBuilder::new()
                    .with_menu(Box::new(menu))
                    .with_tooltip("InputSwitch · 显示器输入源切换");
                if let Some(icon) = load_icon() {
                    builder = builder.with_icon(icon);
                } else {
                    log_line("警告：菜单栏图标解码失败");
                }
                #[cfg(target_os = "macos")]
                {
                    builder = builder.with_icon_as_template(true);
                }
                match builder.build() {
                    Ok(created) => {
                        tray = Some(created);
                        menu_sig = menu_signature(
                            &status,
                            &config,
                            &peers,
                            own_value(&mesh),
                            &profile_key,
                            &support,
                        );
                        log_line("托盘图标已创建");
                    }
                    Err(err) => log_line(&format!("托盘创建失败：{}", err)),
                }

                // 自测 / 需要直接进面板时用：INPUTSWITCH_OPEN_PANEL=1 启动即弹面板
                if std::env::var("INPUTSWITCH_OPEN_PANEL").is_ok() {
                    let port = PANEL_PORT.load(AtomicOrdering::SeqCst);
                    if port != 0 {
                        match Panel::open(elwt, port, "InputSwitch 控制面板") {
                            Ok(created) => {
                                created.show();
                                panel_win = Some(created);
                                log_line("控制面板窗口已自动打开");
                            }
                            Err(err) => log_line(&format!("控制面板窗口创建失败：{err}")),
                        }
                    }
                }
            }

            Event::UserEvent(UserEvent::Status {
                text,
                target: t,
                profile_key: key,
                support: s,
            }) => {
                status = text;
                target = t;
                config = Config::load();
                if !key.is_empty() {
                    profile_key = key;
                }
                if !s.is_empty() {
                    support = s;
                }
                let peers = mesh.live_peers();

                // ---- 链路表自动回填（v0.6.67）----
                //
                // 以前必须**两边都配**：A 的表里要有 B、B 的表里也要有 A ——
                // 因为收包白名单只认链路表里的 IP。只配一边的症状是"推过去
                // 键鼠消失、画面不切、日志安静"，极难自查。
                //
                // 现在：对端**已配对**（announce 签名验过）并且它自己的共用
                // 键鼠是开着的，就自动补一条"未摆位"链路，地址取自它的广播，
                // 本机不编造。用户只需要在**一边**把它拖到某个方向。
                let mut filled: Vec<String> = Vec::new();
                for p in &peers {
                    if !p.paired || !p.kvm_on {
                        continue;
                    }
                    if config.kvm.links.iter().any(|l| l.node == p.node) {
                        continue;
                    }
                    config.kvm.links.push(crate::config::LinkConfig {
                        node: p.node.clone(),
                        side: String::new(),
                        mode: String::new(),
                        input: String::new(),
                        addr: p.addr.ip().to_string(),
                    });
                    filled.push(p.node.clone());
                }
                if !filled.is_empty() {
                    let _ = config.save();
                    log_line(&format!(
                        "已自动补上链路：{}（在摆位图里拖到某个方向就能用；\
                         不用再让对方也配一遍）",
                        filled.join("、")
                    ));
                }

                // ---- 自动恢复（v0.6.66 改成可重试）----
                //
                // 语义只有一条：**自愈不改用户意图**。`config.kvm.enabled`
                // 是"用户想不想开"，这里只负责"想开但没开着"时补上。
                // 所以失败**绝不**写 enabled=false（v0.6.65 之前那条
                // `config.kvm.enabled = false` 会把副机永久钉死在关闭状态）。
                //
                // 三个条件：用户要开 + 开关允许自动恢复 + 现在确实没开。
                // 再加上"对端已上线"——开机瞬间网络/显示器都还没就绪，
                // 那时候硬起只会失败，白白消耗重试次数。
                let resume_due = resume_next.map(|t| Instant::now() >= t).unwrap_or(true);
                if config.kvm.enabled
                    && config.kvm.auto_resume
                    && kvm_handle.is_none()
                    && resume_due
                    // 缺授权时重试一百次也不会成功（TCC 结论缓存在进程里，
                    // 必须重启进程才认），别白刷日志、别把退避拉满。
                    && perms_ok()
                    && pick_peer(&peers, &config.kvm.peer_node).is_some()
                {
                    let log: Arc<dyn Fn(String) + Send + Sync> =
                        Arc::new(|text| log_line(&text));
                    match kvm_boot(&mut config, &peers, &tx, log) {
                        Ok((handle, text)) => {
                            kvm_handle = Some(handle.clone());
                            if let Ok(mut slot) = KVM.lock() {
                                *slot = Some(handle);
                            }
                            resume_failures = 0;
                            resume_next = None;
                            log_line(&format!("{text}（自动恢复上次的设置）"));
                        }
                        Err(err) => {
                            resume_failures = resume_failures.saturating_add(1);
                            let delay = resume_backoff(resume_failures);
                            resume_next = Some(Instant::now() + Duration::from_secs(delay));
                            // 不弹设置页：开机自启时弹窗会吓人。
                            // 但要写清"第几次、多久后重试"，否则用户以为它放弃了。
                            log_line(&format!(
                                "共用键鼠没能自动恢复（第 {} 次）：{err}；{delay} 秒后重试",
                                resume_failures
                            ));
                        }
                    }
                }

                refresh_menu(
                    &mut tray,
                    &mut menu_sig,
                    &status,
                    &config,
                    &peers,
                    own_value(&mesh),
                    &profile_key,
                    &support,
                );
            }

            Event::UserEvent(UserEvent::Panel(action)) => match action {
                Action::Pair(code) => {
                    let code = code.trim().to_string();
                    // 码没变就别重开共用键鼠：面板上点一次"保存"就把正在转发的那场
                    // 断掉，属于无缘无故的伤害。变了才需要换钥匙。
                    let changed = config.pair.code.trim() != code;
                    config.pair.code = code.clone();
                    let _ = config.save();
                    mesh.set_key(crate::pair::link_key(&code));
                    // 文件接收服务的 key 是共享可变的，就地换掉即可（不用重启监听器）
                    if let Ok(mut k) = transfer_key.lock() {
                        *k = crate::pair::link_key(&code);
                    }
                    let text = if code.is_empty() {
                        // 空码 = 取消配对：mesh 会真的把 key 置空（link_key 对空串返回 None），
                        // 之后本机不再签名也不再验签。这句必须把后果说全，不然用户不知道
                        // 自己刚把门打开了。
                        "已取消配对：本机不再校验签名，同一网段的任何设备都能发指令。\
                         对面还留着码的话，它会拒收你的指令，两边都要取消或重新填同一个码"
                            .to_string()
                    } else {
                        format!("已保存配对码 {}，对面填同一个码就能连上", mask_code(&code))
                    };
                    if changed {
                        // 共用键鼠的 `Sealer` 是**启动时快照**、且按设计绝不重建（nonce
                        // counter 不能从头算），所以换码只能换实例。不重开的话事件流还在
                        // 用旧钥匙 —— 界面说"双向消息已签名"，实际两边码不同却谁也发现不了；
                        // 取消配对更糟：KVM 仍在加密，对端没码就当垃圾丢掉，表现成"都开着就是不通"。
                        restart_kvm(&mut config, &mut kvm_handle, &mesh, &tx, &mut status, text);
                    } else {
                        status = text;
                        log_line(&status);
                    }
                }
                Action::GenerateCode => {
                    let code = crate::pair::random_code();
                    config.pair.code = code.clone();
                    let _ = config.save();
                    mesh.set_key(crate::pair::link_key(&code));
                    if let Ok(mut k) = transfer_key.lock() {
                        *k = crate::pair::link_key(&code);
                    }
                    let text = format!("新配对码 {code}，请在对面填同一个");
                    restart_kvm(&mut config, &mut kvm_handle, &mesh, &tx, &mut status, text);
                }
                Action::ToggleAutostart(on) => {
                    let result = if on {
                        // 守护开关跟着一起写进 LaunchAgent / 计划任务：
                        // 两件事在系统层是同一份配置，分开写必然不同步。
                        crate::autostart::enable(config.kvm.keep_alive)
                    } else {
                        crate::autostart::disable()
                    };
                    status = match result {
                        Ok(()) => {
                            if on {
                                "已开启开机自启".to_string()
                            } else {
                                "已关闭开机自启".to_string()
                            }
                        }
                        Err(err) => format!("开机自启设置失败：{err}"),
                    };
                }
                Action::ToggleClipboard(on) => {
                    config.clipboard.enabled = on;
                    let _ = config.save();
                    if on {
                        let _ = watcher.poll();
                    }
                    status = if on {
                        "剪贴板同步已开启".to_string()
                    } else {
                        "剪贴板同步已关闭".to_string()
                    };
                    log_line(&status);
                }
                Action::ChooseInbox => {
                    if let Some(dir) = pick_folder() {
                        config.transfer.inbox = dir.display().to_string();
                        let _ = config.save();
                        *inbox_shared.lock().unwrap() = dir.clone();
                        status = format!("接收目录已改为 {}", dir.display());
                        log_line(&status);
                    }
                }
                Action::OpenInbox => {
                    let dir = config.transfer.inbox_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    open_path(&dir);
                }
                Action::SendFiles(node) => start_send(&config, &mesh, &proxy, &mut status, node.as_deref()),
                Action::KvmPower(on) => {
                    if !on {
                        if let Some(handle) = kvm_handle.take() {
                            handle.stop();
                            if let Ok(mut slot) = KVM.lock() {
                                *slot = None;
                            }
                            config.kvm.enabled = false;
                            let _ = config.save();
                            status = "共用键鼠已关闭".to_string();
                        }
                    } else if kvm_handle.is_some() {
                        status = "共用键鼠已经在运行".to_string();
                    } else {
                        let peers = mesh.live_peers();
                        match pick_peer(&peers, &config.kvm.peer_node) {
                            None => {
                                status = "局域网里没找到对端，先让另一台开着本程序".to_string();
                            }
                            Some(_) => {
                                let log: Arc<dyn Fn(String) + Send + Sync> =
                                    Arc::new(|text| log_line(&text));
                                match kvm_boot(&mut config, &peers, &tx, log) {
                                    Ok((handle, text)) => {
                                        kvm_handle = Some(handle.clone());
                                        if let Ok(mut slot) = KVM.lock() {
                                            *slot = Some(handle);
                                        }
                                        config.kvm.enabled = true;
                                        let _ = config.save();
                                        status = text;
                                    }
                                    Err(err) => {
                                        // 缺权限时**别再说"勾上再点一次"** —— 勾上
                                        // 也不会生效：macOS 把 TCC 结论缓存在进程里，
                                        // 必须重启进程。这句话误导过用户两次。
                                        if perms_ok() {
                                            status = format!("共用键鼠开不了：{err}");
                                        } else {
                                            kvm::open_settings();
                                            status = "共用键鼠开不了：缺系统授权。设置页已打开，\
                                                      勾上本程序后点托盘里的「重启应用」\
                                                      —— 不重启这个进程不会认新授权"
                                                .to_string();
                                        }
                                        // **不改用户意图**：他点了「开启」，那就是
                                        // 想要开。以前这里写 enabled=false，等于
                                        // "失败一次就永远不再尝试" —— 副机（没有
                                        // 键鼠那台）碰上开机时网络没起就彻底废了，
                                        // 而人根本没法去那台机器上再点一次。
                                        config.kvm.enabled = true;
                                        let _ = config.save();
                                        if config.kvm.auto_resume && perms_ok() {
                                            status.push_str("；会自动重试");
                                        }
                                    }
                                }
                            }
                        }
                    }
                    log_line(&status);
                }
                Action::KvmMode(mode) => {
                    // ⚠️ 面板上**已经没有这个入口**（v0.6.58 删掉了「跟随画面 /
                    // 多屏共享」两个全局按钮）。留着只为 CLI / 老调用方兼容。
                    //
                    // 为什么删：`set_mode_all` 会把同一个 mode 写给**每一条**
                    // 链路，而一台机器完全可能"这台共用一块屏、那台并排共享"。
                    // 全局按钮一点，那种混拓扑就被压平成清一色 follow，
                    // 并排那台也跟着落进「没有门」—— 用户实测就是这么撞上的。
                    // 模式现在只由摆位决定：摆在格子里 = edge，放进「没有门」= follow。
                    //
                    // 两种模式的物理前提不同，切换必须重启实例：
                    // 边缘跨界要在「没抓取时」盯屏幕边缘，跟随画面则完全不动。
                    let next = if mode == "edge" { "edge" } else { "follow" };
                    let changed = config.kvm.mode != next;
                    // 写透到链路表：v3 起运行时只认 links，只改镜像等于没改
                    config.kvm.set_mode_all(next);
                    let _ = config.save();
                    if changed {
                        if let Some(handle) = kvm_handle.take() {
                            handle.stop();
                            if let Ok(mut slot) = KVM.lock() {
                                *slot = None;
                            }
                            config.kvm.enabled = false;
                            let _ = config.save();
                            status = if next == "edge" {
                                "已切到多屏共享（两台各有屏幕和键鼠）。共用键鼠需要重新打开"
                                    .to_string()
                            } else {
                                "已切到跟随画面（一套键鼠，跟屏幕走）。共用键鼠需要重新打开"
                                    .to_string()
                            };
                        } else {
                            status = if next == "edge" {
                                "已切到多屏共享：鼠标推到边缘越界".to_string()
                            } else {
                                "已切到跟随画面：画面切到哪台，键鼠跟到哪台".to_string()
                            };
                        }
                    } else {
                        status = format!("已经是{}模式", if next == "edge" { "多屏共享" } else { "跟随画面" });
                    }
                    log_line(&status);
                }
                Action::KvmRole(role) => {
                    let next = if role == "remote" { "remote" } else { "console" };
                    let changed = config.kvm.role != next;
                    config.kvm.role = next.to_string();
                    let _ = config.save();
                    if changed {
                        if let Some(handle) = kvm_handle.take() {
                            handle.stop();
                            if let Ok(mut slot) = KVM.lock() {
                                *slot = None;
                            }
                            config.kvm.enabled = false;
                            let _ = config.save();
                        }
                        status = if next == "console" {
                            "已设为键鼠端（本机插着键鼠）。共用键鼠需要重新打开".to_string()
                        } else {
                            "已设为被控端（键鼠在对面）。共用键鼠需要重新打开".to_string()
                        };
                    } else {
                        status = "角色没变".to_string();
                    }
                    log_line(&status);
                }
                Action::KvmScale(v) => {
                    // 手感旋钮：只影响**被控时**注入的位移，发送侧不动。
                    // 倍率在 KVM 启动时被快照进接收循环，所以改完要重建实例
                    // —— restart_kvm 会先把键鼠交还回去再停，不会留两头
                    // 都以为自己在转发的僵尸会话。
                    let v = v.clamp(0.5, 2.0);
                    let changed = (config.kvm.recv_scale - v).abs() > 0.001;
                    config.kvm.recv_scale = v;
                    let _ = config.save();
                    if changed && config.kvm.enabled {
                        restart_kvm(
                            &mut config,
                            &mut kvm_handle,
                            &mesh,
                            &tx,
                            &mut status,
                            format!("接收位移倍率已改为 x{v:.2}"),
                        );
                    } else {
                        status = if changed {
                            format!("接收位移倍率 x{v:.2}（下次开启共用键鼠生效）")
                        } else {
                            format!("接收位移倍率本来就是 x{v:.2}")
                        };
                        log_line(&status);
                    }
                }
                Action::KvmAutoResume(on) => {
                    config.kvm.auto_resume = on;
                    let _ = config.save();
                    status = if on {
                        "已开启：下次启动后，对端一上线就自动恢复共用键鼠".to_string()
                    } else {
                        "已关闭自动恢复：启动后不会自己去开共用键鼠".to_string()
                    };
                    log_line(&status);
                }
                Action::KvmKeepAlive(on) => {
                    config.kvm.keep_alive = on;
                    let _ = config.save();
                    // 守护是写在"开机自启"那份配置里的（plist / 计划任务），
                    // 自启没开时它无处可挂 —— 顺手把自启带上并说清楚。
                    let mut note = String::new();
                    if on && !crate::autostart::is_enabled() {
                        if crate::autostart::enable(true).is_ok() {
                            note = "（同时打开了开机自启 —— 守护要靠它才生效）".to_string();
                        }
                    } else if crate::autostart::is_enabled() {
                        // 自启已经开着：重写一次，把 KeepAlive / 计划任务加进去或去掉
                        let _ = crate::autostart::enable(on);
                    }
                    status = if on {
                        format!("已开启崩溃自动重启{note}")
                    } else {
                        "已关闭崩溃自动重启（下次退出后不会自己回来）".to_string()
                    };
                    log_line(&status);
                }
                Action::Submachine(on) => {
                    // 一键把"没有键鼠那台"该有的配置全打开：自启 + 自动恢复 +
                    // 守护 + 角色 remote + 共用键鼠开着。少一条它就是个哑终端。
                    config.kvm.auto_resume = on;
                    config.kvm.keep_alive = on;
                    if on {
                        config.kvm.role = "remote".to_string();
                        config.kvm.enabled = true;
                    }
                    let _ = config.save();
                    let auto = if on {
                        crate::autostart::enable(true)
                    } else {
                        crate::autostart::disable()
                    };
                    status = match auto {
                        Ok(()) if on => "副机模式已开启：开机自启 + 自动恢复 + 崩溃重启。\
                             记得给这台配静态 IP —— 共用键鼠的白名单只认地址，不认机器名"
                            .to_string(),
                        Ok(()) => "副机模式已关闭：不再自启、不再自动恢复、不再守护".to_string(),
                        Err(err) => format!("副机模式设置失败：{err}"),
                    };
                    log_line(&status);
                }
                Action::SetLabel { name, label } => {
                    // 空串 = 恢复默认名（删掉别名，让 label_for 按 MCCS 渲染）
                    let text = label.trim().to_string();
                    if text.is_empty() {
                        config.labels.remove(&name);
                        status = format!("已恢复默认名：{}", config.label_for(&name));
                    } else {
                        config.labels.insert(name.clone(), text.clone());
                        status = format!("已把这个口改名为「{text}」");
                    }
                    let _ = config.save();
                    log_line(&status);
                }
                Action::KvmHandOver(node) => {
                    // 不带 node 就是面板上那个全局按钮。多机时它靠 follow_target() 猜，
                    // 猜错的表现是"点了 A、键鼠跑到 B" —— 直接拒绝，让人点名。
                    let ambiguous = node.is_none() && mesh.live_peers().len() > 1;
                    status = if ambiguous {
                        "有多台对端在线，得点名：点下面某一行末尾的「交给它」".to_string()
                    } else {
                        match kvm_now() {
                            Some(handle) => {
                                // 点名交给哪台；不带 node 就是老行为（挑一条 Follow 链路）
                                let outcome = match node.as_deref() {
                                    Some(node) => handle.hand_over_to(node).map(|()| true),
                                    None => handle.follow(),
                                };
                                match outcome {
                                    // 这个按钮**不碰画面**（只抓键鼠 + 发 Enter）。
                                    // 所以不能说"画面这边黑着是正常的" —— 画面
                                    // 既不会黑也不会切，一直显示本机。
                                    Ok(true) => "键鼠已交给对端；画面还在这边。\
                                                 想连画面一起过去，请点上面的「切换」"
                                        .to_string(),
                                    Ok(false) => {
                                        let holder = kvm_now()
                                            .and_then(|h| h.active_node().map(|s| s.to_string()));
                                        match holder {
                                            Some(node) => format!(
                                                "键鼠已经在 {node} 手上，这次没有重复交接"
                                            ),
                                            None => "已经有会话在进行，先收回键鼠再交"
                                                .to_string(),
                                        }
                                    }
                                    Err(err) => {
                                        let target = match node.as_deref() {
                                            Some(n) => Some(n.to_string()),
                                            None => follow_link_node(&handle),
                                        };
                                        format!(
                                            "交不出去：{}",
                                            explain_silent_peer(&mesh, target.as_deref(), &err)
                                        )
                                    }
                                }
                            }
                            None => "共用键鼠没开，先在面板上打开它".to_string(),
                        }
                    };
                    log_line(&status);
                }
                Action::KvmLink { node, side, mode } => {
                    let mut changed = false;
                    let mut problem: Option<String> = None;
                    // 面板上可以把"已发现但还没配链路"的对端拖进格子 —— 链路表里没有它
                    // 时现建一条。不建的话第二台机器只能手改 config.json（五台的拓扑下
                    // 这就是硬阻塞），而且拖拽托盘里那类"未配链路"的卡片永远落不了地。
                    //
                    // 只认**在线**的对端：地址来自发现层广播，本机不编造。
                    if !config.kvm.links.iter().any(|l| l.node == node) {
                        if let Some(p) = mesh.live_peers().into_iter().find(|p| p.node == node) {
                            config.kvm.links.push(crate::config::LinkConfig {
                                node: node.clone(),
                                side: String::new(),
                                mode: String::new(),
                                input: String::new(),
                                addr: p.addr.ip().to_string(),
                            });
                            changed = true;
                            log_line(&format!("新建了一条到 {node} 的链路"));
                        }
                    }
                    if let Some(link) = config.kvm.links.iter_mut().find(|l| l.node == node) {
                        if let Some(side) = side {
                            let side = side.trim().to_string();
                            // 空串 = 去掉门（变成跟随画面链路），这是合法值
                            if side != "left"
                                && side != "right"
                                && side != "top"
                                && side != "bottom"
                                && !side.is_empty()
                            {
                                problem = Some(format!("方向只能是 left/right/top/bottom：{side}"));
                            } else {
                                if link.side != side {
                                    link.side = side;
                                    changed = true;
                                }
                                if link.side.is_empty()
                                    && link.mode_or(&config.kvm.mode) == "edge"
                                {
                                    // 无门的 Edge 链路是自相矛盾的组合：整屏都会被吞
                                    link.mode = "follow".to_string();
                                }
                            }
                        }
                        if problem.is_none() {
                            if let Some(mode) = mode {
                                let mode = if mode == "follow" { "follow" } else { "edge" };
                                if mode == "edge" && link.side.trim().is_empty() {
                                    problem = Some(
                                        "这条链路没有方向，不能设成多屏共享（先给它选个方向）"
                                            .to_string(),
                                    );
                                } else if link.mode != mode {
                                    link.mode = mode.to_string();
                                    changed = true;
                                }
                            }
                        }
                        // 有门就是多屏共享、没门就是跟随画面 —— 这两个字段是绑定的。
                        // 「有门却跟随」在摆位图上会画在格子里，实际却根本不占边，
                        // 是那种看着正常、推过去毫无反应的组合，顺手纠正掉。
                        if problem.is_none()
                            && !link.side.trim().is_empty()
                            && link.mode.trim() == "follow"
                        {
                            link.mode = "edge".to_string();
                            changed = true;
                        }
                    } else {
                        problem = Some(format!(
                            "没有叫 {node} 的链路，它现在也不在局域网里 —— 先让对面开机并开着本程序"
                        ));
                    }

                    if let Some(problem) = problem {
                        status = problem;
                    } else if changed {
                        // v3 起运行时只认链路表，改完要回写镜像让老版本也能读到
                        config.kvm.mirror_from_primary();
                        let _ = config.save();
                        // 掩码是按链路表算出来的，改了必须重启实例才生效。
                        // 交给 restart_kvm 自动重开，不再"关掉让你自己再点一次" ——
                        // 摆位图是拖的，一张图要摆好几台，每拖一次都重开一遍没人受得了。
                        // （托盘菜单里改方向走的是同一个函数，两条路行为一致）
                        restart_kvm(
                            &mut config,
                            &mut kvm_handle,
                            &mesh,
                            &tx,
                            &mut status,
                            "链路已改".to_string(),
                        );
                    } else {
                        status = "链路没变".to_string();
                    }
                    log_line(&status);
                }
                Action::RestartApp => {
                    // 先落日志再退出：重启之后用户/我都要能对上"是这一步干的"
                    log_line("收到重启请求（权限刚改过，旧进程里的 TCC 结论已过期）");
                    restart_app();
                }
                Action::KvmTakeBack => {
                    status = match kvm_now() {
                        Some(handle) => {
                            // 被控时本地没有任何东西可以停 —— 只能请对端交还。
                            // 键盘用户在面板上点这个按钮，是他不推鼠标时的唯一出路。
                            let before = handle.state();
                            if handle.take_back_from_peer("面板上点了收回键鼠") {
                                "已请对端交还键鼠".to_string()
                            } else if before == kvm::State::Sending {
                                handle.give_back();
                                "键鼠已收回本机".to_string()
                            } else {
                                "键鼠本来就在本机".to_string()
                            }
                        }
                        None => "共用键鼠没开".to_string(),
                    };
                    log_line(&status);
                }
                Action::RecordHotkey => {
                    // 面板上点「改键」→ 弹出这个常驻的原生小窗抢焦点收键盘事件。
                    // 网页里收不到全局组合键，只能这么绕。
                    if let Some(win) = record_window.as_ref() {
                        recording = true;
                        record_mods = ModifiersState::empty();
                        win.set_visible(true);
                        win.set_focus();
                        status = "请按下新的组合键（Esc 取消）…".to_string();
                    } else {
                        status = "这台机器打不开录制窗口，请直接改配置里的 hotkey".to_string();
                    }
                    log_line(&status);
                }
                Action::OpenConfigDir => open_config_dir(),
                _ => {}
            },

            Event::UserEvent(UserEvent::RemoteClipboard(text)) => {
                // NSPasteboard 只能在主线程碰，所以绕一圈回到这里
                if config.clipboard.enabled && clipboard::write_text(&text) {
                    watcher.mark_seen(&text);
                }
            }

            Event::UserEvent(UserEvent::Transfer(note)) => {
                transfer_note = note.clone();
                log_line(&note);
            }

            Event::UserEvent(UserEvent::Saved(paths, clip)) => {
                if clip {
                    // 剪贴板按需传输的落点：把收到的文件写进本机剪贴板，
                    // 用户按下粘贴键时就直接是本地文件了（那一刻是纯本地操作）。
                    if clipboard::write_files(&paths) {
                        watcher.mark_written();
                        status = format!("已接收 {} 个文件并放入剪贴板", paths.len());
                    } else {
                        status = format!("已接收 {} 个文件，但写入剪贴板失败", paths.len());
                    }
                } else if let Some(first) = paths.first() {
                    status = format!("接收了 {} 个文件 → {}", paths.len(), first.display());
                }
                transfer_note.clear();
                log_line(&status);
            }

            Event::UserEvent(UserEvent::PasteDone(result)) => {
                CLIP_BUSY.store(false, AtomicOrdering::SeqCst);
                let pending = CLIP_PENDING.lock().map(|g| g.len()).unwrap_or(0);
                if let Ok(mut slot) = CLIP_PENDING.lock() {
                    slot.clear();
                }
                match result {
                    Ok(msg) => {
                        if let Some(h) = kvm_now() {
                            h.set_paste_ready(false);
                            if config.clipboard.auto_paste {
                                // 用户按的那一下被我们吞掉了，这里替他把粘贴动作补完。
                                // 注入事件带 INJECT_TAG，不会被当成"又一次要传"。
                                h.replay_paste_to_peer();
                            }
                        }
                        status = if config.clipboard.auto_paste {
                            format!("已把 {pending} 个文件送到对端并替你按下粘贴（{msg}）")
                        } else {
                            format!("已把 {pending} 个文件送到对端：再按一次 Ctrl+V 即可")
                        };
                    }
                    Err(err) => {
                        if let Some(h) = kvm_now() {
                            h.set_paste_ready(false);
                            // 把用户原本那一下补回去：至少粘出剪贴板里原来的内容，
                            // 不能让他觉得"粘贴键坏了"。
                            h.send_key_to_peer(0x09); // V
                        }
                        status = format!("剪贴板文件传输失败：{err}");
                    }
                }
                transfer_note.clear();
                log_line(&status);
            }

            Event::MainEventsCleared => {
                // 面板窗口已经被用户关掉，这里才真正释放（WebView 会先于窗口 drop）
                if panel_stale {
                    panel_win = None;
                    panel_stale = false;
                    log_line("控制面板窗口已关闭");
                }
                // 面板状态 + 剪贴板轮询：都挂在 400ms 的 tick 上，不额外开线程
                {
                    let peers = mesh.live_peers();
                    update_snapshot(
                        &snapshot,
                        &status,
                        &support,
                        &target,
                        &config,
                        &profile_key,
                        &peers,
                        own_value(&mesh),
                        &mesh,
                        &transfer_note,
                    );
                    if config.clipboard.enabled {
                        match watcher.poll() {
                            Some(ClipItem::Text(text)) => {
                                // 复制的是文本：走老路，即时发。
                                // 同时清掉"待传文件"状态 —— 用户改主意了，
                                // 再按 Ctrl+V 不该还去传上一条文件。
                                CLIP_PENDING.lock().map(|mut g| g.clear()).ok();
                                if let Some(h) = kvm_now() {
                                    h.set_paste_ready(false);
                                }
                                // **发给谁**：有会话就只发"现在握着键鼠"的那台，
                                // 没有会话才广播。P7 之前是无条件广播 —— 一机四台时
                                // 复制一句话会同时改掉四台机器的剪贴板，既无意义也
                                // 会踩掉人家正在用的内容。
                                let active_node: Option<String> =
                                    kvm_now().and_then(|h| h.active_node().map(str::to_string));
                                let mut targets: Vec<&Peer> = match active_node.as_deref() {
                                    Some(node) => peers.iter().filter(|p| p.node == node).collect(),
                                    None => peers.iter().collect(),
                                };
                                // 会话那台刚好掉线（active 还指着它，但它已不在
                                // live_peers 里）：退回广播，别因为一台掉线整条同步哑掉
                                if targets.is_empty() {
                                    targets = peers.iter().collect();
                                }
                                let mut sent = 0;
                                for peer in targets {
                                    // 这里只关心"有没有发出去"：一次复制可能广播给
                                    // 好几台，逐条报错会刷屏（真正该解释的是代切那条路）
                                    if mesh
                                        .send_command(
                                            &peer.node,
                                            "clipboard",
                                            serde_json::json!({ "text": text }),
                                        )
                                        .is_ok()
                                    {
                                        sent += 1;
                                    }
                                }
                                if sent > 0 {
                                    log_line(&format!(
                                        "剪贴板已同步（{} 字）",
                                        text.chars().count()
                                    ));
                                }
                            }
                            Some(ClipItem::Files(paths)) => {
                                // 复制的是文件：**一个字节都不发**。
                                // 只在本机记住清单，把所有动作推迟到用户真的按下粘贴。
                                if config.clipboard.files {
                                    if !peers.iter().any(|p| p.clip_v >= 1) {
                                        log_line(
                                            "剪贴板里是文件，但对端版本不支持按需传输，已忽略",
                                        );
                                    } else {
                                        let n = paths.len();
                                        if let Ok(mut slot) = CLIP_PENDING.lock() {
                                            *slot = paths;
                                        }
                                        if let Some(h) = kvm_now() {
                                            h.set_paste_ready(true);
                                        }
                                        log_line(&format!(
                                            "剪贴板里有 {n} 个文件：推到对端按 Ctrl+V 时才传"
                                        ));
                                    }
                                }
                            }
                            None => {}
                        }
                    }

                    // 每轮同步一次"能不能拦粘贴键"：用户完全可能先复制文件、
                    // 后开共用键鼠（那时 poll 早就跑完了），只在 poll 里置位会漏。
                    // 一个原子写，400ms 一次，开销可忽略。
                    let pending_n = CLIP_PENDING.lock().map(|g| g.len()).unwrap_or(0);
                    if let Some(h) = kvm_now() {
                        h.set_paste_ready(pending_n > 0 && config.clipboard.files);
                    }

                    // 统一收口：内容真的变了才重画菜单（签名一样就不碰，
                    // 否则 macOS 会把用户正打开着的菜单关掉）
                    refresh_menu(
                        &mut tray,
                        &mut menu_sig,
                        &status,
                        &config,
                        &peers,
                        own_value(&mesh),
                        &profile_key,
                        &support,
                    );
                }

                while let Ok(event) = MenuEvent::receiver().try_recv() {
                    let id = event.id.0.as_str();
                    if id == MENU_QUIT {
                        if let Some(handle) = kvm_handle.take() {
                            handle.stop();
                        }
                        std::process::exit(0);
                    } else if id == MENU_RESTART {
                        log_line("菜单里点了「重启应用」");
                        restart_app();
                    } else if id == ID_PANEL {
                        let port = PANEL_PORT.load(AtomicOrdering::SeqCst);
                        // 已经有窗口就只是叫到前面来；没有才建，建不出来退回浏览器
                        if panel_win.is_none() && port != 0 {
                            match Panel::open(elwt, port, "InputSwitch 控制面板") {
                                Ok(created) => {
                                    log_line("控制面板窗口已创建");
                                    panel_win = Some(created);
                                }
                                Err(err) => log_line(&format!("控制面板窗口创建失败：{err}")),
                            }
                        }
                        match panel_win.as_ref() {
                            Some(win) => win.show(),
                            None => {
                                if !panel::open_in_browser(port) {
                                    status = "打不开控制面板，请手动访问 127.0.0.1 上的面板"
                                        .to_string();
                                }
                            }
                        }
                    } else if id == MENU_KVM {
                        if let Some(handle) = kvm_handle.take() {
                            handle.stop();
                            if let Ok(mut slot) = KVM.lock() {
                                *slot = None;
                            }
                            config.kvm.enabled = false;
                            let _ = config.save();
                            status = "共用键鼠已关闭".to_string();
                            log_line(&status);
                        } else {
                            let peers = mesh.live_peers();
                            match pick_peer(&peers, &config.kvm.peer_node) {
                                None => {
                                    status = "局域网里没找到对端，先让另一台开着本程序".to_string();
                                    log_line(&status);
                                }
                                Some(_) => {
                                    let log: Arc<dyn Fn(String) + Send + Sync> =
                                        Arc::new(|text| log_line(&text));
                                    match kvm_boot(&mut config, &peers, &tx, log) {
                                        Ok((handle, text)) => {
                                            kvm_handle = Some(handle.clone());
                                            if let Ok(mut slot) = KVM.lock() {
                                                *slot = Some(handle);
                                            }
                                            config.kvm.enabled = true;
                                            let _ = config.save();
                                            status = text;
                                        }
                                        Err(err) => {
                                            // 十有八九是没授权：直接把设置页推给用户
                                            kvm::open_settings();
                                            status = format!(
                                                "共用键鼠开不了：{err}（已经打开设置页，勾上本程序再点一次）"
                                            );
                                            config.kvm.enabled = false;
                                            let _ = config.save();
                                        }
                                    }
                                    log_line(&status);
                                }
                            }
                        }
                    } else if id == MENU_KVM_HOP {
                        match kvm_now() {
                            Some(handle) => {
                                status = match handle.retarget_next_edge() {
                                    Ok(()) => format!(
                                        "键鼠已转到 {}（画面没动）",
                                        handle.active_node().unwrap_or("对端")
                                    ),
                                    Err(err) => format!("转不过去：{err}"),
                                };
                                log_line(&status);
                            }
                            None => status = "共用键鼠没开".to_string(),
                        }
                    } else if id == MENU_KVM_MODE {
                        // 两种模式的物理前提不同，切换必须重启实例：
                        // 边缘跨界要在"没抓取时"盯屏幕边缘，跟随画面则完全不动。
                        let next = if config.kvm.mode == "edge" {
                            "follow".to_string()
                        } else {
                            "edge".to_string()
                        };
                        config.kvm.set_mode_all(&next);
                        let _ = config.save();
                        let was_on = kvm_handle.take();
                        if let Some(handle) = was_on.as_ref() {
                            handle.stop();
                        }
                        if let Ok(mut slot) = KVM.lock() {
                            *slot = None;
                        }
                        status = if next == "edge" {
                            "已切到多屏共享：两台各有屏幕和键鼠，鼠标推到边缘越界".to_string()
                        } else {
                            "已切到跟随画面：一套键鼠，画面切到哪台就跟到哪台".to_string()
                        };
                        if was_on.is_some() {
                            status.push_str("（共用键鼠已关闭，请重新打开）");
                            config.kvm.enabled = false;
                            let _ = config.save();
                        }
                        log_line(&status);
                    } else if let Some(side_value) = id.strip_prefix(MENU_KVM_SIDE) {
                        // 改越界方向。抓取钩子里记死了旧方向，KVM 开着就必须
                        // 重启实例；直接自动重开，别让用户再手动点一遍。
                        if config.kvm.side != side_value {
                            // 写透到主链路：v3 起掩码由链路表生成
                            config.kvm.set_primary_side(side_value);
                            let _ = config.save();
                            let text = format!("对端方向已改为{}侧", side_label(side_value));
                            restart_kvm(&mut config, &mut kvm_handle, &mesh, &tx, &mut status, text);
                        }
                    } else if let Some(rest) = id.strip_prefix(ID_PEER_SIDE) {
                        // `peer-side:<node>:<side|none>`。
                        // 空方向走 `none`：`peer-side:<node>:` 尾部空串在 rsplit 里
                        // 跟"根本没带值"分不开，两种错法表现不同、很难查。
                        if let Some((node, raw)) = rest.rsplit_once(':') {
                            let side = if raw == "none" { "" } else { raw };
                            let old = config
                                .kvm
                                .links
                                .iter()
                                .find(|l| l.node == node)
                                .map(|l| l.side.clone());
                            match old {
                                None => {
                                    status = format!(
                                        "链路表里没有 {node}（可能要重启一次程序让它重新扫描）"
                                    );
                                    log_line(&status);
                                }
                                Some(old) if old == side => {
                                    status = format!("{node} 的方向没变");
                                    log_line(&status);
                                }
                                Some(_) => {
                                    if let Some(link) =
                                        config.kvm.links.iter_mut().find(|l| l.node == node)
                                    {
                                        link.side = side.to_string();
                                    }
                                    // 镜像要跟着回写：老版本只读镜像里的 side，
                                    // 不回写的话两台程序看到的拓扑不一样
                                    config.kvm.mirror_from_primary();
                                    let _ = config.save();

                                    let where_is = if side.is_empty() {
                                        "共用一块屏".to_string()
                                    } else {
                                        format!("本机{}侧", side_label(side))
                                    };
                                    let mut text = format!("{node} 已设为在{where_is}");
                                    // 同一侧摆了两台：推过去落到谁那儿是不确定的。
                                    // 不拦（物理摆位是用户的事），但必须说出来
                                    if !side.is_empty() {
                                        let clash: Vec<&str> = config
                                            .kvm
                                            .links
                                            .iter()
                                            .filter(|l| l.node != node && l.side == side)
                                            .map(|l| l.node.as_str())
                                            .collect();
                                        if !clash.is_empty() {
                                            text.push_str(&format!(
                                                "。注意{}侧已经有 {}，推过去可能落到它那儿",
                                                side_label(side),
                                                clash.join("、")
                                            ));
                                        }
                                    }
                                    restart_kvm(
                                        &mut config,
                                        &mut kvm_handle,
                                        &mesh,
                                        &tx,
                                        &mut status,
                                        text,
                                    );
                                }
                            }
                        }
                    } else if let Some(rest) = id.strip_prefix(ID_PEER_HAND) {
                        // `peer-hand:<node>`
                        match kvm_now() {
                            Some(handle) => {
                                status = match handle.hand_over_to(rest) {
                                    Ok(()) => format!("键鼠已交给 {rest}"),
                                    Err(err) => format!(
                                        "交不出去：{}",
                                        explain_silent_peer(&mesh, Some(rest), &err)
                                    ),
                                };
                            }
                            None => status = "共用键鼠没开，面板上先打开它".to_string(),
                        }
                        log_line(&status);
                    } else if let Some(node) = id.strip_prefix(ID_PEER_FILE) {
                        // `peer-file:<node>` —— 点名发给这一台。菜单里点了谁就发给谁，
                        // 不再走"握着键鼠的那台"这种猜测
                        let node = node.to_string();
                        start_send(&config, &mesh, &proxy, &mut status, Some(&node));
                    } else if let Some(rest) = id.strip_prefix(ID_SWITCH) {
                        if let Some((name, value)) = parse_port(rest) {
                            status = format!("正在切换到 {}…", name);
                            let _ = tx.send(Command::SwitchRaw { name, value });
                        }
                    } else if let Some(rest) = id.strip_prefix(ID_PEER) {
                        // 直接切到对端广播的那个口，不需要用户知道它叫 dp1 还是 usbc
                        if let Some((node, value)) = rest.rsplit_once(':') {
                            if let Ok(value) = value.parse::<u16>() {
                                // **本机口径校验**：对端报的是它**自己那块屏**的端口
                                // 编号，两块屏的 0x0F/0x12 各算各的 —— 拿别人的编号
                                // 切自己的屏，按下去就是黑屏（v0.6.35 幽灵口同源）。
                                // 面板早就这么校验了，托盘这条一直没校验：两台同款
                                // 显示器时看不出来，5 台配不同显示器必踩。
                                let known = config
                                    .profile_for(&profile_key)
                                    .map(|p| p.inputs.iter().any(|s| s.value == value))
                                    .unwrap_or(false);
                                if !known {
                                    status = format!(
                                        "本机的口里没有 0x{value:02X}（{node} 报的是它自己\
                                         那块屏的编号）。这条只在两台接同一块屏时成立，\
                                         已拒绝切换"
                                    );
                                } else {
                                    let name = mesh
                                        .live_peers()
                                        .into_iter()
                                        .find(|p| p.node == node)
                                        .map(|p| p.own_input)
                                        .unwrap_or_default();
                                    status = format!("正在切到 {}…", node);
                                    let _ = tx.send(Command::SwitchRaw { name, value });
                                }
                            }
                        }
                    }
                    let peers = mesh.live_peers();
                    refresh_menu(
                        &mut tray,
                        &mut menu_sig,
                        &status,
                        &config,
                        &peers,
                        own_value(&mesh),
                        &profile_key,
                        &support,
                    );
                }

                while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
                    if Some(event.id) != hotkey_id {
                        continue;
                    }
                    if event.state != global_hotkey::HotKeyState::Pressed {
                        continue;
                    }
                    if target.is_empty() {
                        status = "热键目标未设置：控制面板 → 输入源 → 设为目标".to_string();
                    } else if let Some(value) = configured_value(&config, &profile_key, &target) {
                        status = format!("正在切换到 {}…", target);
                        let _ = tx.send(Command::SwitchRaw {
                            name: target.clone(),
                            value,
                        });
                    } else {
                        // 宁可不切，也不要拿标准值瞎猜 —— 猜错会把屏幕切到空口上
                        status = format!("热键目标 {} 没有对应的口，请在菜单里重新选", target);
                    }
                    let peers = mesh.live_peers();
                    refresh_menu(
                        &mut tray,
                        &mut menu_sig,
                        &status,
                        &config,
                        &peers,
                        own_value(&mesh),
                        &profile_key,
                        &support,
                    );
                }

                // 每 5 秒一次轻量心跳：只同步广播和状态。全量重扫（EDID+能力表，
                // 十几笔 I2C）不能挂在这——显示器刚切回来时撞上这波读取，
                // WindowServer 会卡进看门狗超时（实测整机关机）。重活只留给
                // 用户主动点的「重新检测显示器」。
                if last_rescan.elapsed() > Duration::from_secs(5) {
                    let _ = tx.send(Command::Heartbeat);
                    last_rescan = Instant::now();
                }
            }

            // 录制窗口的键盘事件；面板窗口点关闭只是收起来，不销毁
            Event::WindowEvent { event, window_id, .. } => match event {
                // 面板窗口关掉：tao 没有 prevent_close，窗口会被系统销毁，
                // 这里只做标记，等这一轮事件处理完再释放，避免在窗口回调里自毁。
                WindowEvent::CloseRequested => {
                    if panel_win.as_ref().is_some_and(|win| win.owns(window_id)) {
                        panel_stale = true;
                    } else {
                        recording = false;
                        if let Some(win) = record_window.as_ref() {
                            win.set_visible(false);
                        }
                    }
                }
                // WebView 是子视图，窗口变大变小要手动跟
                WindowEvent::Resized(_) => {
                    if let Some(win) = panel_win.as_ref() {
                        if win.owns(window_id) {
                            win.fit();
                        }
                    }
                }
                WindowEvent::ModifiersChanged(mods) => record_mods = mods,
                WindowEvent::KeyboardInput { event, .. } => {
                    let from_record = record_window
                        .as_ref()
                        .is_some_and(|win| win.id() == window_id);
                    if !recording || !from_record || event.state != ElementState::Pressed {
                        return;
                    }
                    if event.physical_key == KeyCode::Escape {
                        recording = false;
                        if let Some(win) = record_window.as_ref() {
                            win.set_visible(false);
                        }
                        status = "已取消录制".to_string();
                        return;
                    }
                    let Some(token) = key_token(event.physical_key) else {
                        return;
                    };
                    let Some(spec) = compose_spec(record_mods, token) else {
                        // 只按住修饰键或只按普通键：继续等，不要吞掉
                        status = "再带一个 Ctrl / Alt / Shift / Cmd 一起按".to_string();
                        return;
                    };
                    match apply_hotkey(
                        &mut hotkey_manager,
                        &mut current_hotkey,
                        &mut hotkey_id,
                        &spec,
                    ) {
                        Ok(()) => {
                            config.hotkey = spec.clone();
                            let _ = config.save();
                            status = format!("热键已改为 {}", pretty_hotkey(&spec));
                            log_line(&status);
                        }
                        Err(err) => {
                            status = err;
                            log_line(&status);
                        }
                    }
                    recording = false;
                    if let Some(win) = record_window.as_ref() {
                        win.set_visible(false);
                    }
                }
                _ => {}
            },

            _ => {}
        }
    })
}

fn parse_port(rest: &str) -> Option<(String, u16)> {
    let (name, value) = rest.split_once(':')?;
    Some((name.to_string(), value.parse().ok()?))
}

/// 方向的中文名，菜单和状态文字都用它
fn side_label(side: &str) -> &'static str {
    match kvm::event::Side::parse(side) {
        Some(kvm::event::Side::Left) => "左",
        Some(kvm::event::Side::Top) => "上",
        Some(kvm::event::Side::Bottom) => "下",
        _ => "右",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 自动恢复的退避：1 → 5 → 15 → 60 → 300，之后一直 300。
    ///
    /// 这里锁的是"别让它放弃，也别让它刷屏"：第一档必须是 1 秒（开机时
    /// 网络没起，几秒内就好），最后一档必须封顶（真失败别把日志刷爆）。
    #[test]
    fn resume_backoff_ramps_and_caps() {
        assert_eq!(resume_backoff(0), 1);
        assert_eq!(resume_backoff(1), 1);
        assert_eq!(resume_backoff(2), 5);
        assert_eq!(resume_backoff(3), 15);
        assert_eq!(resume_backoff(4), 60);
        assert_eq!(resume_backoff(5), 300);
        assert_eq!(resume_backoff(1000), 300);
        // 单调不减：任何一次失败都不该比上一次等得更短
        let mut last = 0;
        for n in 0..40 {
            let d = resume_backoff(n);
            assert!(d >= last, "第 {n} 次退避比上一次短：{d} < {last}");
            last = d;
        }
    }
}
