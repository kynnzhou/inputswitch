//! 常驻与服务项：daemon 把程序跑起来，autostart 管开机自启，clip 是剪贴板自检。
//!
//! 共同点是不针对某一台显示器 —— daemon 背后是托盘（`tray::run`），无托盘
//! 构建时退化成 headless 轮询。这个文件的命令都是对 lib 的薄封装，路径全部
//! 写全限定，所以顶部一条 import 都没有。

pub fn cmd_autostart(on: bool, off: bool) -> i32 {
    if on {
        // CLI 里不单独给守护开关：按配置里那一份写，命令行和面板不会打架
        let keep = inputswitch::config::Config::load().kvm.keep_alive;
        match inputswitch::autostart::enable(keep) {
            Ok(()) => println!("已开启开机自启"),
            Err(err) => {
                println!("开启失败: {}", err);
                return 1;
            }
        }
    } else if off {
        match inputswitch::autostart::disable() {
            Ok(()) => println!("已关闭开机自启"),
            Err(err) => {
                println!("关闭失败: {}", err);
                return 1;
            }
        }
    } else {
        println!(
            "开机自启: {}",
            if inputswitch::autostart::is_enabled() {
                "已开启"
            } else {
                "未开启"
            }
        );
    }
    0
}

pub fn cmd_daemon() -> i32 {
    #[cfg(feature = "tray")]
    {
        inputswitch::tray::run()
    }

    #[cfg(not(feature = "tray"))]
    {
        cmd_daemon_headless()
    }
}

#[cfg(not(feature = "tray"))]
pub fn cmd_daemon_headless() -> i32 {
    let sw = inputswitch::Switcher::new();
    let ddc_ok = sw
        .pick_display(None)
        .map(|d| sw.capability(&d) != inputswitch::Capability::Unsupported)
        .unwrap_or(false);
    let port = sw.config.network.port;
    let node = sw.config.node.clone();

    let mesh = inputswitch::peer::Mesh::new(node.clone(), port, ddc_ok);
    let started = mesh.start(Box::new(move |from, action, args| {
        if action == "switch" {
            if let Some(target) = args.get("input").and_then(|v| v.as_str()) {
                let mut inner = inputswitch::Switcher::new();
                let outcome = inner.switch(target, None, true, std::time::Duration::from_secs(3));
                eprintln!("[relay] {from} → {}", outcome.message);
            }
        }
    }));

    println!("inputswitch daemon（无托盘构建）");
    println!("  节点: {}", node);
    println!("  DDC : {}", if ddc_ok { "可用" } else { "不可用" });
    if started.is_err() {
        eprintln!("局域网监听启动失败，仅本地模式");
    }
    println!("  Ctrl-C 退出");

    loop {
        std::thread::sleep(std::time::Duration::from_secs(3));
        let displays = sw.displays();
        if displays.is_empty() {
            continue;
        }
        if let Some(display) = displays.first() {
            if let Some(name) = sw.current_input_name(display) {
                eprint!("\r状态: {} · {}", display.name, name);
            }
        }
    }
}

/// 剪贴板自检：不带参数读，带参数写。
pub fn cmd_clip(text: Option<&str>) -> i32 {
    match text {
        Some(text) => {
            if inputswitch::clipboard::write_text(text) {
                println!("已写入剪贴板（{} 字）", text.chars().count());
                0
            } else {
                eprintln!("写剪贴板失败");
                1
            }
        }
        None => match inputswitch::clipboard::read_item() {
            Some(inputswitch::clipboard::ClipItem::Text(t)) => {
                println!("文本：{}", t.chars().count());
                println!("{t}");
                0
            }
            Some(inputswitch::clipboard::ClipItem::Files(v)) => {
                println!("文件：{} 个", v.len());
                for p in v {
                    println!("{}", p.display());
                }
                0
            }
            None => {
                println!("（剪贴板里没有能识别的内容）");
                0
            }
        },
    }
}
