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
use inputswitch::tray::engine::{engine_loop, Command, UserEvent};
use inputswitch::tray::log_line;
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
    let dispatch: panel::Dispatcher = {
        let cfg_state = cfg_state.clone();
        let mesh = mesh.clone();
        let tx_cmd = tx_cmd.clone();
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
