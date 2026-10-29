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
                Action::RecordHotkey | Action::RestartApp => {
                    log_line("[Tauri 壳] 改键窗口/重启：下一批接线");
                }
                other => {
                    log_line(&format!("[Tauri 壳] 该动作下一批接线：{other:?}"));
                }
            }
        })
    };

    let port = panel::start(state.clone(), dispatch, 47833).expect("面板服务起不来");

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
            let open = MenuItem::with_id(app, "open", "打开控制面板…", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;
            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .icon_as_template(true)
                .menu(&menu)
                .show_menu_on_left_click(true)
                .on_menu_event(move |app, event| {
                    if event.id() == open.id() {
                        open_panel(app, port);
                    } else if event.id() == quit.id() {
                        app.exit(0);
                    }
                })
                .build(app)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Tauri 壳启动失败");
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
