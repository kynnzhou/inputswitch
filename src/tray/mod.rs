//! 托盘 UI + 全局热键。
//!
//! 设计目标：**不打开终端就能用**。菜单里直接列出各输入口，点一下就切；
//! 切成功后自动把「名称→VCP 取值」记进 profile，并记下热键目标。
//!
//! 所有 DDC 操作都放到工作线程：一次切换要等 2–3 秒看显示器有没有离开本机，
//! 放在 UI 线程会卡住菜单。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering as AtomicOrdering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use global_hotkey::hotkey::HotKey;
use global_hotkey::GlobalHotKeyManager;
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::keyboard::ModifiersState;
use tao::window::{Window, WindowBuilder};

use crate::clipboard::Watcher;
use crate::kvm;
use crate::panel::{self, Action, Snapshot};
use crate::peer::Mesh;
use crate::{transfer, Switcher};

// ---- 子模块 ----
// 按"改一件事要动几个文件"切，不按层切：原来是 4202 行一个文件，改一行菜单文案
// 要滚过 kvm_boot、engine_loop 两个几百行的函数；现在 22 个文件、最大 menu.rs 575
// 行（engine / actions 各拆成 6 / 5 个文件，门禁见 tools/check-size.js）。分工：
//   ui / actions / events   主线程：tao Event → Ui 方法 → 面板动作、菜单与热键轮询
//   engine / switch         工作线程：吃 Command 队列，真正下发切屏指令
//   menu / snapshot         菜单构建与签名去重、面板快照
//   kvmctl                  共用键鼠的启动 / 重启 / 挑选对端
//   hotkey / send / sys / log   热键、发文件、平台相关（图标 / 重启 / 开目录）、日志
// 可见性一律 `pub(super)`：只给 `crate::tray` 这层看，不泄漏到 crate 外
// （对外仍然只有 `tray::run` 和 `tray::log_line` 两个入口）。
mod actions;
mod engine;
mod events;
mod hotkey;
mod kvmctl;
mod log;
mod menu;
mod send;
mod snapshot;
mod switch;
mod sys;
mod ui;

pub use log::log_line;

use engine::engine_loop;
use hotkey::parse_hotkey;
use ui::Ui;

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
/// 画面在对面时，请对端替我们把画面切回来。值 = 本机记忆里的口。
const MENU_ASK_BACK: &str = "ask-back";
/// 面板实际监听的端口。build_menu 要拿它拼菜单项，所以放静态里。
pub(super) static PANEL_PORT: AtomicU16 = AtomicU16::new(0);

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

/// 「重新检测显示器」的结论。工作线程写，UI 线程压快照时取走。
/// 用静态是因为这条结论要横跨两个线程、又不值得为它加一条事件通道。
pub(super) static PORT_NOTE: Mutex<String> = Mutex::new(String::new());

/// 本机此刻有没有接通的显示器。
///
/// 画面在对面那台时，本机显卡这边就是"没接显示器"，枚举结果为 0 ——
/// 这不是故障，但必须让面板说清楚，否则用户只会看到一句"没报输入源"。
pub(super) static HAS_DISPLAY: AtomicBool = AtomicBool::new(false);

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
/// 启动托盘 + 事件循环。这个函数不返回 —— 菜单里点「退出」直接走
/// `std::process::exit`，所以签名是 `-> !`。
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
        "启动：热键={} 目标={} 节点={} 构建=v{}",
        if hotkey_id.is_some() { "已注册" } else { "注册失败" },
        config_snapshot.hotkey_target,
        config_snapshot.node, crate::VERSION
    ));

    // ---- 主线程状态收口 ----
    //
    // 以前这些都是 event_loop.run 闭包的捕获变量（那个闭包 1259 行，改一个
    // 字段要在几百行里搜它出现在哪些分支）。现在收进 Ui，闭包里只剩分发；
    // 每个 Event 的处理在 tray::ui / tray::actions / tray::events 三个文件里。
    let mut ui = Ui {
        hotkey_manager,
        current_hotkey,
        hotkey_id,
        record_window,
        tx,
        mesh,
        proxy,
        snapshot,
        inbox_shared,
        transfer_key,
        tray: None,
        status: "正在检测显示器…".to_string(),
        target: config_snapshot.hotkey_target.clone(),
        config: config_snapshot,
        profile_key: String::new(),
        support: String::new(),
        // 上一次画出来的菜单长什么样。内容没变就不重建 —— 见 menu_signature 的注释。
        menu_sig: String::new(),
        last_rescan: Instant::now(),
        recording: false,
        record_mods: ModifiersState::empty(),
        // 剪贴板：先取一次基线，否则启动瞬间会把用户手上已有的内容当成"新复制"
        watcher: Watcher::default(),
        transfer_note: String::new(),
        // 控制面板：第一次点菜单才建窗口，之后关闭只是隐藏，再点秒开
        panel_win: None,
        // 共用键鼠：开着的时候它会一直占着 CGEventTap / 低级钩子
        kvm_handle: None,
        // 自动恢复的重试状态（v0.6.66）。
        //
        // 以前这里是 `kvm_restored: bool`，一次失败就永久放弃 —— 对没有键鼠的
        // 副机是死局：它开机比主机早、网络还没起、第一次授权没生效，任何一种
        // 都会让它永远停在空闲，而人没法去那台机器上点菜单。
        resume_failures: 0,
        resume_next: None,
        // 用户点了面板窗口的关闭按钮：等这轮事件处理完再真正释放
        panel_stale: false,
    };
    // 基线要在事件循环开始之前取：启动瞬间用户手上可能已经有内容了
    let _ = ui.watcher.poll();

    event_loop.run(move |event, elwt, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(400));

        match event {
            Event::NewEvents(StartCause::Init) => ui.on_init(elwt),
            Event::UserEvent(UserEvent::Status {
                text,
                target,
                profile_key,
                support,
            }) => ui.on_status(text, target, profile_key, support),
            Event::UserEvent(UserEvent::Panel(action)) => ui.on_panel(action),
            Event::UserEvent(UserEvent::RemoteClipboard(text)) => ui.on_remote_clipboard(text),
            Event::UserEvent(UserEvent::Transfer(note)) => ui.on_transfer(note),
            Event::UserEvent(UserEvent::Saved(paths, clip)) => ui.on_saved(paths, clip),
            Event::UserEvent(UserEvent::PasteDone(result)) => ui.on_paste_done(result),
            Event::MainEventsCleared => ui.on_main_events_cleared(elwt),
            // 录制窗口的键盘事件；面板窗口点关闭只是收起来，不销毁
            Event::WindowEvent { event, window_id, .. } => ui.on_window_event(event, window_id),
            _ => {}
        }
    })
}

fn parse_port(rest: &str) -> Option<(String, u16)> {
    let (name, value) = rest.split_once(':')?;
    Some((name.to_string(), value.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::kvmctl::resume_backoff;

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
