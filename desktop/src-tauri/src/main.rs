//! InputSwitch 的 Tauri 2 壳（P2 第二批：引擎真接线）。
//!
//! 架构不变式（细案 D1/D2）：面板数据与控制全部走 loopback HTTP——
//! 根 lib 的 `panel::start` 出 `/`（旧面板）与 `/app/`（React 前端），本窗口
//! 只是把 `http://127.0.0.1:{port}/app/` 装进 WebviewWindow。零 IPC。
//!
//! 本批接线范围：
//! - 引擎线程（engine_loop）+ Mesh 网络发现 + 400ms 快照 tick —— 新面板的
//!   读全真（状态/输入口/对端），切屏/检测/扫描/热键目标/配置类写操作全真。
//! - KVM 生命周期（kvm_boot/restart_kvm）与对话框类暂缓：前者牵 tray::Ui
//!   状态机平移，后者要 Tauri dialog 插件评估，都放下一批。对应 Action 落
//!   日志，绝不静默假装成功（家规：失败要说清）。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Manager,
    WebviewWindowBuilder,
};

use inputswitch::config::Config;
use inputswitch::panel::{self, Action, Snapshot};
use inputswitch::peer::Mesh;
use inputswitch::kvm;
use inputswitch::tray::engine::{engine_loop, Command, UserEvent};
use inputswitch::tray::{
    kvm_boot, log_line, open_config_dir, open_path, pick_files, pick_folder, resolve_peer,
    restart_kvm, KVM,
};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tauri::menu::{CheckMenuItem, PredefinedMenuItem, Submenu};
use inputswitch::transfer;
use inputswitch::Switcher;

fn main() {
    let switcher = Switcher::new();
    let config0 = switcher.config.clone();
    let mesh = Mesh::new(
        config0.node.clone(),
        config0.network.port,
        switcher.pick_display(None).is_some(),
    );
    mesh.set_key(inputswitch::pair::link_key(&config0.pair.code));

    let state: panel::State = Arc::new(Mutex::new(Snapshot::default()));
    let (tx, rx) = std::sync::mpsc::channel::<Command>();

    if config0.network.enabled {
        let tx_relay = tx.clone();
        let started = mesh.start(Box::new(move |from, action, args| {
            log_line(&format!("收到 {from} 的指令：{action}"));
            match action.as_str() {
                "switch" => {
                    let value = args.get("value").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                    let name = args
                        .get("input")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let _ = tx_relay.send(Command::SwitchRaw { name, value });
                }
                "clipboard" => {
                    if let Some(text) = args.get("text").and_then(|v| v.as_str()) {
                        let _ = tx_relay.send(Command::RemoteClipboard(text.to_string()));
                    }
                }
                _ => {}
            }
        }));
        if started.is_err() {
            log_line("局域网监听启动失败，仅本机模式");
        }
    }

    let tx_cmd = tx.clone();
    let cfg_state: Arc<Mutex<Config>> = Arc::new(Mutex::new(config0.clone()));
    // KVM 实例句柄：等价旧壳 Ui.kvm_handle（KVM 静态供引擎侧 kvm_now() 用）
    let kvm_handle: Arc<Mutex<Option<Arc<kvm::Kvm>>>> = Arc::new(Mutex::new(None));
    let tx_for_kvm = tx.clone();
    // 面板真实端口（panel::start 顺延后写回；dispatcher 生成 URL 要用）
    let port_shared: Arc<std::sync::atomic::AtomicU16> = Arc::new(std::sync::atomic::AtomicU16::new(0));
    // UI 请求通道：dispatcher（HTTP 线程）→ 持 AppHandle 的消费线程开窗
    let (ui_tx, ui_rx) = std::sync::mpsc::channel::<UiRequest>();
    // 热键管理器：进程级单例（manager 必须活到退出）
    let hotkey_mgr: Arc<Mutex<Option<GlobalHotKeyManager>>> = Arc::new(Mutex::new(None));
    let menu_sig: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    // 文件接收服务（对话框类接线需要它）
    let inbox_shared: Arc<Mutex<std::path::PathBuf>> =
        Arc::new(Mutex::new(config0.transfer.inbox_dir()));
    let transfer_key: Arc<Mutex<Option<[u8; 32]>>> =
        Arc::new(Mutex::new(inputswitch::pair::link_key(&config0.pair.code)));
    if config0.transfer.enabled {
        let inbox = inbox_shared.clone();
        let key = transfer_key.clone();
        let dir_for_log = config0.transfer.inbox_dir();
        let _ = transfer::serve(
            inbox,
            config0.transfer.port,
            key,
            |m| log_line(&format!("[传输] {m}")),
            |saved, _clip| {
                log_line(&format!("已接收 {} 个文件", saved.len()));
            },
        );
        log_line(&format!("接收目录：{}", dir_for_log.display()));
    }
    let state_tx: panel::State = state.clone();

    let dispatch: panel::Dispatcher = {
        let cfg_state = cfg_state.clone();
        let mesh = mesh.clone();
        let tx_cmd = tx_cmd.clone();
        let kvm_handle = kvm_handle.clone();
        let state_tx = state_tx.clone();
        let inbox_shared = inbox_shared.clone();
        let tx_for_kvm = tx_for_kvm.clone();
        let port_tx = port_shared.clone();
        let app_tx = ui_tx.clone();
        let hotkey_mgr = hotkey_mgr.clone();
        Arc::new(move |action: Action| {
            let mut cfg = cfg_state.lock().unwrap();
            match action {
                Action::Switch { name, value, follow } => {
                    let cmd = if follow {
                        Command::SwitchRaw { name, value }
                    } else {
                        Command::SwitchOnly { name, value }
                    };
                    let _ = tx_cmd.send(cmd);
                }
                Action::SetTarget { name, value } => {
                    let _ = tx_cmd.send(Command::SetTarget { name, value });
                }
                Action::ClearTarget => {
                    let _ = tx_cmd.send(Command::ClearTarget);
                }
                Action::Rescan => {
                    let _ = tx_cmd.send(Command::Rescan);
                }
                Action::ScanPorts => {
                    let _ = tx_cmd.send(Command::ScanPorts);
                }
                Action::CheckSupport => {
                    let _ = tx_cmd.send(Command::CheckSupport);
                }
                Action::AskPeer(value) => {
                    let _ = tx_cmd.send(Command::AskPeerSwitch { value });
                }
                Action::Pair(code) => {
                    let code = code.trim().to_string();
                    cfg.pair.code = code.clone();
                    let _ = cfg.save();
                    mesh.set_key(inputswitch::pair::link_key(&code));
                    log_line(if code.is_empty() { "已取消配对" } else { "配对码已保存" });
                }
                Action::GenerateCode => {
                    let code = inputswitch::pair::random_code();
                    cfg.pair.code = code.clone();
                    let _ = cfg.save();
                    mesh.set_key(inputswitch::pair::link_key(&code));
                    log_line(&format!("新配对码 {code}"));
                }
                Action::ToggleClipboard(on) => {
                    cfg.clipboard.enabled = on;
                    let _ = cfg.save();
                }
                Action::KvmAutoResume(on) => {
                    cfg.kvm.auto_resume = on;
                    let _ = cfg.save();
                }
                Action::KvmKeepAlive(on) => {
                    cfg.kvm.keep_alive = on;
                    let _ = cfg.save();
                }
                Action::SetLabel { name, label } => {
                    let label = label.trim().to_string();
                    if label.is_empty() {
                        cfg.labels.remove(&name);
                    } else {
                        cfg.labels.insert(name, label);
                    }
                    let _ = cfg.save();
                }
                // ---- KVM 生命周期：与旧壳同一份 kvm_boot/restart_kvm ----
                Action::KvmPower(on) => {
                    if on {
                        let peers = mesh.live_peers();
                        if inputswitch::tray::pick_peer(&peers, &cfg.kvm.peer_node).is_none() {
                            log_line("局域网里没找到对端，先让另一台开着本程序");
                        } else {
                            let log: Arc<dyn Fn(String) + Send + Sync> = Arc::new(|t| log_line(&t));
                            match kvm_boot(&mut cfg, &peers, &tx_for_kvm, log) {
                                Ok((handle, text)) => {
                                    *kvm_handle.lock().unwrap() = Some(handle);
                                    cfg.kvm.enabled = true;
                                    let _ = cfg.save();
                                    log_line(&text);
                                }
                                Err(err) => {
                                    cfg.kvm.enabled = true;
                                    let _ = cfg.save();
                                    log_line(&format!("共用键鼠开不了：{err}"));
                                }
                            }
                        }
                    } else if let Some(handle) = kvm_handle.lock().unwrap().take() {
                        handle.stop();
                        if let Ok(mut slot) = KVM.lock() {
                            *slot = None;
                        }
                        cfg.kvm.enabled = false;
                        let _ = cfg.save();
                        log_line("共用键鼠已关闭");
                    }
                }
                Action::KvmLink { node, side, mode } => {
                    let default_mode = cfg.kvm.mode.clone();
                    if let Some(link) = cfg.kvm.links.iter_mut().find(|l| l.node == node) {
                        if let Some(side) = &side {
                            let s = side.trim().to_string();
                            let valid = s.is_empty()
                                || ["left", "right", "top", "bottom"].contains(&s.as_str());
                            if !valid {
                                log_line(&format!("方向只能是 left/right/top/bottom：{s}"));
                                return;
                            }
                            link.side = s;
                        }
                        if let Some(m) = mode {
                            link.mode = m;
                        }
                        if !link.side.trim().is_empty() && link.mode.trim() == "follow" {
                            link.mode = "edge".into();
                        }
                        if link.side.trim().is_empty() && link.mode_or(&default_mode) == "edge" {
                            link.mode = "follow".into();
                        }
                        cfg.kvm.mirror_from_primary();
                        let _ = cfg.save();
                        let mut status = String::new();
                        restart_kvm(&mut cfg, &mut kvm_handle.lock().unwrap(), &mesh, &tx_for_kvm, &mut status, format!("{node} 链路已改"));
                        log_line(&status);
                    } else if let Some(p) = mesh.live_peers().into_iter().find(|p| p.node == node) {
                        cfg.kvm.links.push(inputswitch::config::LinkConfig {
                            node: node.clone(),
                            side: side.clone().unwrap_or_default().trim().to_string(),
                            mode: mode.clone().unwrap_or_default(),
                            input: String::new(),
                            addr: p.addr.ip().to_string(),
                        });
                        let _ = cfg.save();
                        let mut status = String::new();
                        restart_kvm(&mut cfg, &mut kvm_handle.lock().unwrap(), &mesh, &tx_for_kvm, &mut status, format!("新建了一条到 {node} 的链路"));
                        log_line(&status);
                    }
                }
                Action::KvmScale(v) => {
                    let v = v.clamp(0.5, 2.0);
                    let changed = (cfg.kvm.recv_scale - v).abs() > 0.001;
                    cfg.kvm.recv_scale = v;
                    let _ = cfg.save();
                    if changed && kvm_handle.lock().unwrap().is_some() {
                        let mut status = String::new();
                        restart_kvm(&mut cfg, &mut kvm_handle.lock().unwrap(), &mesh, &tx_for_kvm, &mut status, format!("接收位移倍率已改为 x{v:.2}"));
                        log_line(&status);
                    }
                }
                Action::KvmHandOver(node) => {
                    let h = inputswitch::tray::kvm_now();
                    match h {
                        Some(h) => {
                            let r = match &node {
                                Some(n) => h.hand_over_to(n).map(|()| true),
                                None => h.follow(),
                            };
                            log_line(&if r.is_ok() { "键鼠已交给对端".into() } else { format!("交不出去：{:?}", r.err()) });
                        }
                        None => log_line("共用键鼠没开，面板上先打开它"),
                    }
                }
                Action::KvmTakeBack => {
                    if let Some(h) = inputswitch::tray::kvm_now() {
                        if !h.take_back_from_peer("面板点了收回") {
                            h.give_back();
                            log_line("键鼠已收回本机");
                        } else {
                            log_line("已请对端交还键鼠");
                        }
                    }
                }
                Action::Submachine(on) => {
                    cfg.kvm.auto_resume = on;
                    cfg.kvm.keep_alive = on;
                    if on {
                        cfg.kvm.role = "remote".into();
                        cfg.kvm.enabled = true;
                    }
                    let _ = cfg.save();
                    let r = if on {
                        inputswitch::autostart::enable(true)
                    } else {
                        inputswitch::autostart::disable()
                    };
                    log_line(&format!("副机模式：{r:?}"));
                }
                Action::KvmMode(mode) => {
                    cfg.kvm.set_mode_all(if mode == "edge" { "edge" } else { "follow" });
                    let _ = cfg.save();
                    if let Some(h) = kvm_handle.lock().unwrap().take() {
                        h.stop();
                        if let Ok(mut slot) = KVM.lock() {
                            *slot = None;
                        }
                    }
                    log_line("模式已改，重新打开共用键鼠生效");
                }
                Action::KvmRole(role) => {
                    cfg.kvm.role = if role == "remote" { "remote".into() } else { "console".into() };
                    let _ = cfg.save();
                    log_line("角色已改（仅影响状态文案）");
                }
                // ---- 对话框类：系统选择器是阻塞的，spawn 线程别挂住面板服务 ----
                Action::SendFiles(node) => {
                    let mesh = mesh.clone();
                    let cfg = cfg.clone();
                    let node = node.clone();
                    let state_tx = state_tx.clone();
                    thread::spawn(move || {
                        let peer = match resolve_peer(&mesh, node.as_deref()) {
                            Ok(p) => p,
                            Err(err) => {
                                log_line(&err);
                                if let Ok(mut s) = state_tx.lock() {
                                    s.transfer = err;
                                }
                                return;
                            }
                        };
                        let files = pick_files();
                        if files.is_empty() {
                            return;
                        }
                        if let Ok(mut s) = state_tx.lock() {
                            s.transfer = format!("正在发送 {} 个文件…", files.len());
                        }
                        let key = inputswitch::pair::link_key(&cfg.pair.code);
                        let addr = std::net::SocketAddr::new(peer.addr.ip(), cfg.transfer.port);
                        let r = transfer::send(addr, &cfg.node, &files, key, false, None, |_, _| {});
                        let note = match r {
                            Ok(m) => format!("{m} → {}", peer.node),
                            Err(e) => format!("发送失败：{e}"),
                        };
                        log_line(&note);
                        if let Ok(mut s) = state_tx.lock() {
                            s.transfer = note;
                        }
                    });
                }
                Action::ChooseInbox => {
                    let cfg_state = cfg_state.clone();
                    let inbox_shared = inbox_shared.clone();
                    thread::spawn(move || {
                        if let Some(dir) = pick_folder() {
                            if let Ok(mut cfg) = cfg_state.lock() {
                                cfg.transfer.inbox = dir.display().to_string();
                                let _ = cfg.save();
                            }
                            *inbox_shared.lock().unwrap() = dir.clone();
                            log_line(&format!("接收目录已改为 {}", dir.display()));
                        }
                    });
                }
                Action::OpenInbox => {
                    let dir = cfg.transfer.inbox_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    open_path(&dir);
                }
                Action::OpenConfigDir => open_config_dir(),
                Action::RecordHotkey => {
                    // 录制页：收完组合键 POST /api/hotkey-set 回来（下一个分支）
                    open_window(&app_tx, &port_tx, "recorder", "/hotkey-recorder".to_string(), 420.0, 120.0, "按下新的组合键");
                }
                Action::SetHotkey(combo) => {
                    cfg.hotkey = combo.clone();
                    let _ = cfg.save();
                    // 立刻换注册（换不动会在热键线程下次刷新时重试）
                    if hotkey::apply(&hotkey_mgr, &combo) {
                        log_line(&format!("热键已改为 {combo}"));
                    } else {
                        log_line("热键换注册失败，旧热键仍在生效");
                    }
                }
                Action::RestartApp => {
                    log_line("收到重启请求");
                    restart_app();
                }
                other => {
                    log_line(&format!("[Tauri 壳] 该动作下一批接线：{other:?}"));
                }
            }
        })
    };

    let port = panel::start(state.clone(), dispatch, 47833).expect("面板服务起不来");
    port_shared.store(port, std::sync::atomic::Ordering::SeqCst);

    {
        let state = state.clone();
        let cfg_state = cfg_state.clone();
        let sink = Arc::new(move |ev: UserEvent| match ev {
            UserEvent::Status { text, target, profile_key, support } => {
                if let Ok(mut s) = state.lock() {
                    s.status = text;
                    s.target = target;
                    if !profile_key.is_empty() {
                        s.identity = profile_key.clone();
                    }
                    if !support.is_empty() {
                        s.support = support;
                    }
                }
                if let Ok(mut cfg) = cfg_state.lock() {
                    *cfg = Config::load();
                }
            }
            UserEvent::Transfer(note) => {
                if let Ok(mut s) = state.lock() {
                    s.transfer = note.clone();
                }
                log_line(&note);
            }
            other => {
                log_line(&format!("[引擎事件] {other:?}"));
            }
        });
        let mesh_for_engine = mesh.clone();
        thread::spawn(move || engine_loop(switcher, rx, sink, mesh_for_engine));
    }

    {
        let state = state.clone();
        let cfg_state = cfg_state.clone();
        let mesh = mesh.clone();
        let tx_beat = tx.clone();
        let mut last_beat = Instant::now();
        thread::spawn(move || loop {
            thread::sleep(Duration::from_millis(400));
            // 引擎心跳（对照旧壳 400ms tick 里的 5 秒节拍）：拓扑变化发现、
            // own_value 同步、状态文案刷新全靠它
            if last_beat.elapsed() >= Duration::from_secs(5) {
                let _ = tx_beat.send(Command::Heartbeat);
                last_beat = Instant::now();
            }
            let cfg = cfg_state.lock().unwrap().clone();
            let peers = mesh.live_peers();
            let mut s = state.lock().unwrap();
            s.node = cfg.node.clone();
            s.paired = cfg.pair.is_set();
            s.pair_code = cfg.pair.code.clone();
            s.autostart = inputswitch::autostart::is_enabled();
            s.clipboard = cfg.clipboard.enabled;
            s.inbox = cfg.transfer.inbox_dir().display().to_string();
            s.own_value = cfg.own_value;
            s.peers = peers
                .iter()
                .map(|p| panel::PeerView {
                    node: p.node.clone(),
                    addr: p.addr.ip().to_string(),
                    input: p.own_input.clone(),
                    label: if p.own_input.is_empty() {
                        String::new()
                    } else {
                        cfg.label_for(&p.own_input)
                    },
                    value: p.own_value,
                    paired: p.paired,
                    knows_port: p.knows_port(),
                    needs_pair: p.wants_pair && !cfg.pair.is_set(),
                    kvm_on: p.kvm_on,
                })
                .collect();
            s.peer_verified = peers.iter().filter(|p| p.paired).count();
            // KVM 视图（对照旧壳 update_snapshot 的映射；没开时按配置画链路）
            s.kvm = match inputswitch::tray::kvm_now() {
                Some(h) => {
                    let active_idx = h.active_index();
                    panel::KvmView {
                        on: true,
                        mode: h.mode().as_str().to_string(),
                        role: h.role().as_str().to_string(),
                        state: h.state().label().to_string(),
                        peer_alive: h.peer_alive(),
                        active: h.active_node().unwrap_or("").to_string(),
                        follow_link: h.has_follow_link(),
                        will_follow: h.has_follow_link() && h.can_capture(),
                        root: if h.at_root() { String::new() } else { h.root_node().unwrap_or("").to_string() },
                        links: h
                            .links()
                            .iter()
                            .enumerate()
                            .map(|(i, l)| panel::LinkView {
                                node: l.node.clone(),
                                addr: l.ip.to_string(),
                                side: l.side.map(|x| x.as_str().to_string()).unwrap_or_default(),
                                mode: l.mode.as_str().to_string(),
                                alive: h.alive_at(i),
                                active: active_idx == Some(i),
                                can_retarget: h.state() == kvm::State::Sending && active_idx != Some(i) && h.alive_at(i),
                            })
                            .collect(),
                        recv_scale: cfg.kvm.recv_scale,
                        auto_resume: cfg.kvm.auto_resume,
                        keep_alive: cfg.kvm.keep_alive,
                    }
                }
                None => panel::KvmView {
                    on: false,
                    mode: cfg.kvm.mode.clone(),
                    role: cfg.kvm.role.clone(),
                    state: "未启动".to_string(),
                    links: cfg
                        .kvm
                        .links
                        .iter()
                        .map(|l| panel::LinkView {
                            node: l.node.clone(),
                            addr: l.addr.clone(),
                            side: l.side.clone(),
                            mode: l.mode_or(&cfg.kvm.mode),
                            alive: false,
                            active: false,
                            can_retarget: false,
                        })
                        .collect(),
                    follow_link: cfg.kvm.links.iter().any(|l| l.mode_or(&cfg.kvm.mode) == "follow"),
                    recv_scale: cfg.kvm.recv_scale,
                    auto_resume: cfg.kvm.auto_resume,
                    keep_alive: cfg.kvm.keep_alive,
                    ..Default::default()
                },
            };
            let key = s.identity.clone();
            let cur = s.inputs.iter().find(|i| i.current).map(|i| i.value);
            let rows = inputswitch::tray::port_rows_for_snapshot(&cfg, &key, cur);
            s.inputs = rows
                .iter()
                .map(|r| panel::InputView {
                    name: r.name.clone(),
                    label: r.label.clone(),
                    value: r.value,
                    current: r.current,
                    presence: format!("{:?}", r.presence).to_lowercase(),
                    verified: r.verified,
                })
                .collect();
        });
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(move |app| {
            let app_handle = app.handle().clone();

            // UI 请求消费：dispatcher（HTTP 线程）→ 开窗（必须持 AppHandle）
            {
                let app_handle = app_handle.clone();
                thread::spawn(move || {
                    while let Ok(req) = ui_rx.recv() {
                        match req {
                            UiRequest::OpenWindow { id, url, w, h, title } => {
                                handle_open_window(&app_handle, &id, &url, w, h, &title);
                            }
                        }
                    }
                });
            }

            // 热键：初始注册 + 事件轮询（对照旧壳：按下 → 查 target 值 → 切屏）
            {
                let hotkey_mgr = hotkey_mgr.clone();
                let cfg_state = cfg_state.clone();
                let tx_hotkey = tx.clone();
                let state_hot = state.clone();
                let first = cfg_state.lock().unwrap().hotkey.clone();
                if !first.is_empty() {
                    hotkey::apply(&hotkey_mgr, &first);
                }
                thread::spawn(move || loop {
                    while let Ok(ev) = GlobalHotKeyEvent::receiver().try_recv() {
                        if ev.state != HotKeyState::Pressed {
                            continue;
                        }
                        let cfg = cfg_state.lock().unwrap().clone();
                        let snap = state_hot.lock().unwrap();
                        let target = &cfg.hotkey_target;
                        if target.is_empty() {
                            log_line("热键还没指定目标：到控制面板的输入源列表点「设为热键」");
                            continue;
                        }
                        let hit = snap
                            .inputs
                            .iter()
                            .find(|i| i.name.eq_ignore_ascii_case(target) || i.label.eq_ignore_ascii_case(target));
                        match hit {
                            Some(i) => {
                                let name = i.name.clone();
                                let value = i.value;
                                drop(snap);
                                log_line(&format!("热键触发：切到 {name}"));
                                let _ = tx_hotkey.send(Command::SwitchRaw { name, value });
                            }
                            None => {
                                drop(snap);
                                log_line(&format!("热键目标 {target} 没有对应的口"));
                            }
                        }
                    }
                    thread::sleep(Duration::from_millis(120));
                });
            }

            // 托盘 + 全量菜单：每 2.5s 按快照/配置重建（签名一致就不换 ——
            // 换菜单会收起已展开的菜单，旧壳 menu_signature 同款坑）
            let tray = {
                let menu = build_menu(app.handle(), &cfg_state.lock().unwrap(), &state.lock().unwrap())?;
                *menu_sig.lock().unwrap() = menu_signature(&cfg_state.lock().unwrap(), &state.lock().unwrap());
                TrayIconBuilder::new()
                    .icon(app.default_window_icon().unwrap().clone())
                    .icon_as_template(true)
                    .menu(&menu)
                    .show_menu_on_left_click(true)
                    .build(app)?
            };
            let tray = Arc::new(tray);
            {
                let tray = tray.clone();
                let app_handle = app_handle.clone();
                let cfg_state = cfg_state.clone();
                let state = state.clone();
                let menu_sig = menu_sig.clone();
                thread::spawn(move || loop {
                    thread::sleep(Duration::from_millis(2500));
                    let cfg = cfg_state.lock().unwrap().clone();
                    let snap = state.lock().unwrap().clone();
                    let sig = menu_signature(&cfg, &snap);
                    if sig == *menu_sig.lock().unwrap() {
                        continue;
                    }
                    match build_menu(&app_handle, &cfg, &snap) {
                        Ok(menu) => {
                            let _ = tray.set_menu(Some(menu));
                            *menu_sig.lock().unwrap() = sig;
                        }
                        Err(e) => log_line(&format!("托盘菜单重建失败：{e}")),
                    }
                });
            }

            // 菜单事件分发：沿用旧壳菜单 id 协议（switch:/peer:/peer-hand:/
            // peer-side:/toggle-kvm/ask-back/restart-app/quit/open-panel）
            let tx_menu = tx.clone();
            let cfg_state_menu = cfg_state.clone();
            let kvm_handle_menu = kvm_handle.clone();
            let mesh_menu = mesh.clone();
            let port_menu = port;
            tray.on_menu_event(move |app, event| {
                let id = event.id().0.as_str().to_string();
                let tx = tx_menu.clone();
                let mut cfg = cfg_state_menu.lock().unwrap();
                let _ = (&kvm_handle_menu, &mesh_menu);
                if id == "quit" {
                    app.exit(0);
                } else if id == "open-panel" {
                    open_panel(app, port_menu);
                } else if id == "toggle-kvm" {
                    let on = inputswitch::tray::kvm_now().is_none();
                    if on {
                        let peers = mesh_menu.live_peers();
                        let log: Arc<dyn Fn(String) + Send + Sync> = Arc::new(|t| log_line(&t));
                        match kvm_boot(&mut cfg, &peers, &tx, log) {
                            Ok((h, text)) => {
                                *kvm_handle_menu.lock().unwrap() = Some(h);
                                cfg.kvm.enabled = true;
                                let _ = cfg.save();
                                log_line(&text);
                            }
                            Err(e) => log_line(&format!("共用键鼠开不了：{e}")),
                        }
                    } else if let Some(h) = kvm_handle_menu.lock().unwrap().take() {
                        h.stop();
                        if let Ok(mut slot) = KVM.lock() {
                            *slot = None;
                        }
                        cfg.kvm.enabled = false;
                        let _ = cfg.save();
                        log_line("共用键鼠已关闭");
                    }
                } else if id == "ask-back" {
                    let _ = tx.send(Command::AskPeerSwitch { value: cfg.own_value });
                } else if id == "restart-app" {
                    restart_app();
                } else if let Some(rest) = id.strip_prefix("switch:") {
                    if let Some((name, value)) = rest.rsplit_once(':') {
                        if let Ok(value) = value.parse() {
                            let _ = tx.send(Command::SwitchRaw { name: name.to_string(), value });
                        }
                    }
                } else if let Some(rest) = id.strip_prefix("peer:") {
                    if let Some((_node, value)) = rest.rsplit_once(':') {
                        if let Ok(value) = value.parse() {
                            let _ = tx.send(Command::SwitchRaw { name: String::new(), value });
                        }
                    }
                } else if let Some(node) = id.strip_prefix("peer-hand:") {
                    if let Some(h) = inputswitch::tray::kvm_now() {
                        let _ = h.hand_over_to(node);
                    }
                } else if let Some(rest) = id.strip_prefix("peer-side:") {
                    if let Some((node, side)) = rest.rsplit_once(':') {
                        let side = if side == "none" { "" } else { side };
                        if let Some(link) = cfg.kvm.links.iter_mut().find(|l| &l.node == node) {
                            link.side = side.to_string();
                            link.mode = if side.is_empty() { "follow".into() } else { "edge".into() };
                        }
                        cfg.kvm.mirror_from_primary();
                        let _ = cfg.save();
                        log_line(&format!("{node} 已设在{}", if side.is_empty() { "共用一块屏".to_string() } else { side.to_string() }));
                    }
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Tauri 壳启动失败");
}

/// 托盘全量菜单（对照旧壳 menu.rs build_menu 的精简版：状态行 + 输入源
/// 切换 + 对端子菜单 + KVM 开关 + 画面切回 + 面板 + 退出）。
fn build_menu(
    app: &tauri::AppHandle,
    cfg: &Config,
    snap: &Snapshot,
) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::new(app)?;
    menu.append(&MenuItem::with_id(app, "status-line", &snap.status, false, None::<&str>)?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;

    if snap.has_display {
        let sub = Submenu::with_id(app, "switch-sub", "切换输入源", true)?;
        let rows = inputswitch::tray::port_rows_for_snapshot(cfg, &snap.identity, Some(snap.current));
        if rows.is_empty() {
            sub.append(&MenuItem::with_id(app, "no-ports", "没读到输入口 · 到面板重新检测", false, None::<&str>)?)?;
        }
        for r in rows {
            let dead = r.presence == inputswitch::models::Presence::Invalid;
            sub.append(&CheckMenuItem::with_id(
                app,
                format!("switch:{}:{}", r.name, r.value),
                if r.label.trim().is_empty() { format!("0x{:02X}", r.value) } else { r.label.clone() },
                !dead,
                r.current,
                None::<&str>,
            )?)?;
        }
        menu.append(&sub)?;
    } else if cfg.own_value > 0 {
        menu.append(&MenuItem::with_id(app, "ask-back", "请对面把画面切回本机", true, None::<&str>)?)?;
    }

    for link in &cfg.kvm.links {
        let head = format!("{link}{mark}", link = link.node,
            mark = if link.side.is_empty() { " · 共用一块屏" } else { "" });
        let sub = Submenu::with_id(app, format!("peer-sub-{}", link.node), head, true)?;
        sub.append(&MenuItem::with_id(app, format!("peer-hand:{}", link.node), "把键鼠交给它", true, None::<&str>)?)?;
        for (v, label) in [("right", "右侧"), ("left", "左侧"), ("top", "上方"), ("bottom", "下方"), ("", "共用一块屏")] {
            sub.append(&CheckMenuItem::with_id(
                app,
                if v.is_empty() { format!("peer-side:{}:none", link.node) } else { format!("peer-side:{}:{v}", link.node) },
                label,
                true,
                link.side == v,
                None::<&str>,
            )?)?;
        }
        menu.append(&sub)?;
    }

    menu.append(&PredefinedMenuItem::separator(app)?)?;
    let kvm_text = if snap.kvm.on { "共用键鼠 · 已开启 · 点此关闭" } else { "共用键鼠 · 已关闭 · 点此开启" };
    menu.append(&MenuItem::with_id(app, "toggle-kvm", kvm_text, true, None::<&str>)?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "open-panel", "打开控制面板…", true, None::<&str>)?)?;
    menu.append(&MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?)?;
    Ok(menu)
}

/// 菜单签名（对照旧壳：内容没变不重建，macOS 换菜单会收起展开中的菜单）
fn menu_signature(cfg: &Config, snap: &Snapshot) -> String {
    use std::fmt::Write;
    let mut sig = String::new();
    let _ = write!(sig, "{status}|{display}|{current}|{own}", status = snap.status, display = snap.has_display, current = snap.current, own = cfg.own_value);
    for l in &cfg.kvm.links {
        let _ = write!(sig, ";{node}={side}", node = l.node, side = l.side);
    }
    let _ = write!(sig, ";kvm={}", snap.kvm.on);
    sig
}

fn open_panel(app: &tauri::AppHandle, port: u16) {
    let url = format!("http://127.0.0.1:{port}/app/");
    if let Some(win) = app.get_webview_window("panel") {
        let _ = win.show();
        let _ = win.set_focus();
        return;
    }
    let _ = WebviewWindowBuilder::new(app, "panel", tauri::WebviewUrl::External(url.parse().unwrap()))
        .title("InputSwitch · 切屏控制台")
        .inner_size(820.0, 840.0)
        .min_inner_size(600.0, 520.0)
        .build();
}


// ===================== P2 尾批支撑 =====================

/// dispatcher → 主线程的 UI 请求（开窗必须持 AppHandle 的线程做）
enum UiRequest {
    OpenWindow { id: String, url: String, w: f64, h: f64, title: String },
}

/// 开（或唤起）一个 Tauri 窗口。对照旧 panelwin：窗口对象常驻、关闭只是
/// 隐藏；面板 show 时把滚动位置重置（WKWebView 保留旧滚动位置的怪癖）。
fn open_window(
    ui_tx: &std::sync::mpsc::Sender<UiRequest>,
    port: &std::sync::atomic::AtomicU16,
    id: &str,
    path_or_url: String,
    w: f64,
    h: f64,
    title: &str,
) {
    let port = port.load(std::sync::atomic::Ordering::SeqCst);
    let url = if path_or_url.starts_with("http") {
        path_or_url
    } else {
        format!("http://127.0.0.1:{port}{path_or_url}")
    };
    let _ = ui_tx.send(UiRequest::OpenWindow {
        id: id.to_string(),
        url,
        w,
        h,
        title: title.to_string(),
    });
}

fn handle_open_window(app: &tauri::AppHandle, id: &str, url: &str, w: f64, h: f64, title: &str) {
    use tauri::Manager;
    if let Some(win) = app.get_webview_window(id) {
        let _ = win.show();
        let _ = win.set_focus();
        // 滚动重置：WebView 保留上次滚动位置（panelwin 三 hack 之一）
        let _ = win.eval("window.scrollTo(0,0)");
        return;
    }
    let _ = WebviewWindowBuilder::new(
        app,
        id,
        tauri::WebviewUrl::External(url.parse().unwrap()),
    )
    .title(title)
    .inner_size(w, h)
    .build();
}

/// 热键注册（对照旧壳 hotkey.rs 的 apply_hotkey：先注新、成功再销旧）。
/// manager 是进程级单例，第一次调用时创建。
mod hotkey {
    use super::*;

    pub fn apply(mgr_slot: &Arc<Mutex<Option<GlobalHotKeyManager>>>, spec: &str) -> bool {
        let Some(next) = parse(spec) else { return false };
        let mut slot = mgr_slot.lock().unwrap();
        if slot.is_none() {
            match GlobalHotKeyManager::new() {
                Ok(m) => *slot = Some(m),
                Err(e) => {
                    log_line(&format!("热键管理器创建失败：{e}"));
                    return false;
                }
            }
        }
        let mgr = slot.as_ref().unwrap();
        if let Err(e) = mgr.register(next) {
            log_line(&format!("热键 {spec} 注册失败（被占用？）：{e}"));
            return false;
        }
        log_line(&format!("热键已注册：{spec}（id={}）", next.id()));
        true
    }

    /// "ctrl+alt+s" → HotKey。键表对照旧壳 code_of（字母/数字/F1-F12 够用）。
    fn parse(spec: &str) -> Option<HotKey> {
        let mut mods = Modifiers::empty();
        let mut code = None;
        for tok in spec.to_lowercase().split('+') {
            match tok.trim() {
                "ctrl" | "control" => mods |= Modifiers::CONTROL,
                "alt" | "option" => mods |= Modifiers::ALT,
                "shift" => mods |= Modifiers::SHIFT,
                "cmd" | "super" | "win" | "meta" => mods |= Modifiers::SUPER,
                "" => {}
                other => code = Some(code_of(other)?),
            }
        }
        code.map(|c| HotKey::new(Some(mods), c))
    }

    fn code_of(tok: &str) -> Option<Code> {
        let c = match tok {
            "space" => Code::Space,
            "0" => Code::Digit0, "1" => Code::Digit1, "2" => Code::Digit2, "3" => Code::Digit3,
            "4" => Code::Digit4, "5" => Code::Digit5, "6" => Code::Digit6, "7" => Code::Digit7,
            "8" => Code::Digit8, "9" => Code::Digit9,
            "a" => Code::KeyA, "b" => Code::KeyB, "c" => Code::KeyC, "d" => Code::KeyD,
            "e" => Code::KeyE, "f" => Code::KeyF, "g" => Code::KeyG, "h" => Code::KeyH,
            "i" => Code::KeyI, "j" => Code::KeyJ, "k" => Code::KeyK, "l" => Code::KeyL,
            "m" => Code::KeyM, "n" => Code::KeyN, "o" => Code::KeyO, "p" => Code::KeyP,
            "q" => Code::KeyQ, "r" => Code::KeyR, "s" => Code::KeyS, "t" => Code::KeyT,
            "u" => Code::KeyU, "v" => Code::KeyV, "w" => Code::KeyW, "x" => Code::KeyX,
            "y" => Code::KeyY, "z" => Code::KeyZ,
            "f1" => Code::F1, "f2" => Code::F2, "f3" => Code::F3, "f4" => Code::F4,
            "f5" => Code::F5, "f6" => Code::F6, "f7" => Code::F7, "f8" => Code::F8,
            "f9" => Code::F9, "f10" => Code::F10, "f11" => Code::F11, "f12" => Code::F12,
            _ => return None,
        };
        Some(c)
    }
}

/// 重启本程序（对照旧壳 sys.rs）：延迟 1 秒等本进程退出释放端口/托盘。
fn restart_app() {
    #[cfg(target_os = "macos")]
    {
        let exe = std::env::current_exe().unwrap_or_default();
        let script = format!("sleep 1; open -n '{}'", exe.display());
        let _ = std::process::Command::new("sh").arg("-c").arg(script).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(exe) = std::env::current_exe() {
            let cmd = format!("timeout /t 1 >nul & start \"\" \"{}\"", exe.display());
            let mut child = std::process::Command::new("cmd");
            child.arg("/C").arg(cmd);
            let _ = child.spawn();
        }
    }
    std::thread::sleep(Duration::from_millis(150));
    std::process::exit(0);
}
