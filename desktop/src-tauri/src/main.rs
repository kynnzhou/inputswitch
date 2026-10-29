//! InputSwitch 的 Tauri 2 壳（P2 骨架）。
//!
//! 架构不变式（细案 D1/D2）：**面板数据与控制全部走 loopback HTTP**——
//! 根 lib 的 `panel::start` 仍然出 `/`（旧面板）与 `/app/`（React 前端），
//! 本窗口只是把 `http://127.0.0.1:{port}/app/` 装进 Tauri 的 WebviewWindow。
//! 零 IPC、零 tauri commands：CLI/脚本/浏览器退路继续复用同一套接口。
//!
//! P2 骨架范围：托盘（打开面板 / 退出）+ 面板窗口 + HTTP 服务接线。
//! 完整平移（热键注册、KVM 生命周期、菜单全量、panelwin 三处平台 hack）
//! 在后续批次做 —— `tray/ui.rs` 的事件模型是围绕 tao EventLoop 写的，
//! 搬家要连同引擎线程/快照循环一起动，一次只挪一块。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};

use tauri::{
    Manager,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    WebviewWindowBuilder,
};

use inputswitch::panel::{self, Action, Snapshot};

fn main() {
    // 与旧壳同一份快照/分发模型：Snapshot 由 400ms tick 刷新（P2 后续批次
    // 接引擎真实状态；骨架期是静态默认值，页面骨架能渲染）。
    let state: panel::State = Arc::new(Mutex::new(Snapshot::default()));
    let dispatch: panel::Dispatcher = Arc::new(|action: Action| {
        // P2 后续批次：翻译成 Command 发引擎线程 / 主线程事件。
        // 骨架期先落日志 —— 面板只读部分（状态/输入口/对端）不经过这里，
        // 页面仍然可用；写操作（切换/配对/开关）到下一批接通。
        let _ = action;
        println!("[dispatch] {:?}", action);
    });

    let port = panel::start(state, dispatch, 47833).expect("面板服务起不来");

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

/// 打开（或唤起）面板窗口。对照旧 panelwin：窗口对象常驻、关闭只是隐藏。
/// macOS 的 activateIgnoringOtherApps（LSUIElement 抢不到前台）平移到
/// set_focus 调用点，真机验证在后续批次（细案 D4 三处 hack 之一）。
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
