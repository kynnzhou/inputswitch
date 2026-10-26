//! 共用键鼠：kvm 直接在本机跑一套，kvm-probe / inject-probe 是它的两个自检。
//!
//! 都只在 macOS / Windows 上有后端（其余平台走 else 分支直接说明），所以
//! 平台相关的导入都写在函数体内，文件顶部只剩一条 Config。

use inputswitch::config::Config;

/// 注入自检：不依赖网络，直接往本机注入一次鼠标移动并读回光标位置。
/// 专门用来分辨"键鼠不工作"到底卡在传输还是卡在注入。
pub fn cmd_inject_probe() -> i32 {
    use inputswitch::kvm::event::Event;
    use inputswitch::kvm::Inject;
    let inject = Inject::new();
    let Some((x, y)) = inject.cursor() else {
        println!("✗ 连光标位置都读不到（GetCursorPos/CGEvent 失败）");
        return 1;
    };
    let (w, h) = inject.screen();
    println!("光标：({x:.0},{y:.0})，屏幕：{w:.0}x{h:.0}");
    let dx = (w / 2.0) as i32 - x as i32;
    let dy = (h / 2.0) as i32 - y as i32;
    inject.apply(&Event::Motion { dx, dy });
    std::thread::sleep(std::time::Duration::from_millis(200));
    match inject.cursor() {
        Some((nx, ny)) => {
            let moved = ((nx - x) * (nx - x) + (ny - y) * (ny - y)).sqrt();
            if moved > 50.0 {
                println!("✓ 注入成功：光标从 ({x:.0},{y:.0}) 移动到了 ({nx:.0},{ny:.0})");
                println!("  说明本机注入环节没问题，键鼠不工作要往传输/对端抓取方向查");
                0
            } else {
                println!("✗ 注入失败：光标几乎没动（移动了 {moved:.1} 点）");
                #[cfg(windows)]
                println!("  常见原因：前台窗口以管理员权限运行，而本程序没有 —— 以管理员身份重跑本命令再试");
                #[cfg(not(windows))]
                println!("  常见原因：辅助功能权限没有真正生效（重勾一次，重启程序）");
                1
            }
        }
        None => {
            println!("✗ 注入后读不到光标位置");
            1
        }
    }
}

/// 共用键鼠的自检：权限、屏幕、当前状态。
pub fn cmd_kvm_probe() -> i32 {
    #[cfg(any(target_os = "macos", windows))]
    {
        let trusted = inputswitch::kvm::trusted();
        let post = inputswitch::kvm::can_post();
        let (w, h) = inputswitch::kvm::screen_size();
        println!("抓取权限（辅助功能）：{}", if trusted { "✓ 有" } else { "✗ 没有" });
        println!("注入权限（辅助功能/PostEvent）：{}", if post { "✓ 有" } else { "✗ 没有" });
        println!("主屏尺寸：{w} x {h} 点");
        let config = Config::load();
        if config.pair.is_set() {
            println!("事件流：已加密（ChaCha20-Poly1305，密钥由配对码派生）");
        } else {
            println!("事件流：明文 —— 同一网段任何人都能嗅探你敲的键");
            println!("  先跑 inputswitch-cli pair --generate，两台填同一个码");
        }
        println!("心跳：每 800ms，2.5 秒收不到对端就自动交还");
        let mode = match config.kvm.mode.as_str() {
            "edge" => "多屏共享（两台各有屏幕和键鼠，鼠标推到边缘越界）",
            _ => "跟随画面（一套键鼠，画面切到哪台键鼠跟到哪台）",
        };
        let role = if config.kvm.role == "remote" {
            "被控端（键鼠在对面，本机只注入）"
        } else {
            "键鼠端（本机抓取并转发）"
        };
        println!("当前模式：{mode}");
        println!("当前角色：{role}");
        println!("  一套键鼠、一块屏 → 模式选「跟随画面」，键鼠那台选「键鼠端」");
        println!("  两台各有屏幕     → 模式选「多屏共享」，两边角色随意");
        if !trusted {
            println!();
            println!("去这里勾上本程序：");
            println!("  系统设置 → 隐私与安全性 → 辅助功能");
            println!("  系统设置 → 隐私与安全性 → 输入监控（抓键盘要这个）");
            println!();
            println!("注意：debug / release 重新编译后签名会变，需要重新勾一次。");
            return 1;
        }
        0
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        eprintln!("这个平台还没有共用键鼠的后端");
        1
    }
}

/// 直接在本机跑共用键鼠（不经过托盘），用来验证抓取和注入。
pub fn cmd_kvm(peer: &str, mode: &str, role: &str, side: &str, seconds: u64) -> i32 {
    #[cfg(any(target_os = "macos", windows))]
    {
        use std::sync::Arc;

        let Ok(ip) = peer.parse::<std::net::IpAddr>() else {
            eprintln!("对端地址看不懂：{peer}");
            return 2;
        };
        let Some(mode) = inputswitch::kvm::Mode::parse(mode) else {
            eprintln!("mode 只能是 follow / edge");
            return 2;
        };
        let Some(role) = inputswitch::kvm::Role::parse(role) else {
            eprintln!("role 只能是 console / remote");
            return 2;
        };
        let Some(side) = inputswitch::kvm::event::Side::parse(side) else {
            eprintln!("side 只能是 right / left / top / bottom");
            return 2;
        };
        let config = Config::load();
        let log = Arc::new(|text: String| println!("[kvm] {text}"));
        // CLI 只有一台对端，也没有发现层可查节点名 —— 名字就用地址，
        // 日志里至少一眼能看出是跟谁在开会话。
        let opts = inputswitch::kvm::Options {
            links: vec![inputswitch::kvm::Link {
                node: ip.to_string(),
                ip,
                side: if mode == inputswitch::kvm::Mode::Edge {
                    Some(side)
                } else {
                    None
                },
                mode,
            }],
            mode,
            role,
            base_port: config.network.port,
            key: inputswitch::pair::link_key(&config.pair.code),
            on_panic: None,
            log,
            on_paste: None,
            push_hop: config.kvm.push_hop,
            recv_scale: config.kvm.recv_scale,
        };
        let kvm = match inputswitch::kvm::start(opts) {
            Ok(kvm) => kvm,
            Err(err) => {
                eprintln!("启动失败：{err}");
                return 1;
            }
        };
        match mode {
            inputswitch::kvm::Mode::Follow if role == inputswitch::kvm::Role::Console => {
                println!("跟随画面模式：本机是键鼠端。托盘里切屏成功后键鼠会自动跟到 {ip}；");
                println!("命令行下可以用 Ctrl+Alt+Esc 紧急收回（这条路径不会切回画面）。");
            }
            inputswitch::kvm::Mode::Follow => {
                println!("跟随画面模式：本机是被控端，不抓键鼠，等 {ip} 那边接管。");
            }
            inputswitch::kvm::Mode::Edge => {
                println!(
                    "多屏共享模式：鼠标推到屏幕{}侧越界到 {ip}",
                    match side {
                        inputswitch::kvm::event::Side::Right => "右",
                        inputswitch::kvm::event::Side::Left => "左",
                        inputswitch::kvm::event::Side::Top => "上",
                        inputswitch::kvm::event::Side::Bottom => "下",
                    }
                );
            }
        }
        if seconds == 0 {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        } else {
            std::thread::sleep(std::time::Duration::from_secs(seconds));
            kvm.stop();
            println!("已停止");
        }
        0
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (peer, side, seconds);
        eprintln!("这个平台还没有共用键鼠的后端");
        1
    }
}
