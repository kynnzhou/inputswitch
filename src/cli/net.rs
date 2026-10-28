//! 局域网与配对：status 看得到对端，pair 管签名密钥，send 直传文件。
//!
//! 三个都走 `peer::Mesh`（UDP 广播发现 + 中继指令），不碰显示器。

use std::thread::sleep;
use std::time::Duration;

use inputswitch::config::Config;
use inputswitch::models::VCP_INPUT_SOURCE;
use inputswitch::peer::Mesh;
use inputswitch::Switcher;

pub fn cmd_status() -> i32 {
    let sw = Switcher::new();
    println!("节点: {}", sw.config.node);
    let displays = sw.displays();
    if displays.is_empty() {
        println!("  当前没有显示器挂在本机（屏幕可能已经被切到对面）");
    }
    for display in &displays {
        let input = sw.backend().get_vcp(display, VCP_INPUT_SOURCE);
        let name = sw
            .current_input_name(display)
            .unwrap_or_else(|| "?".into());
        println!(
            "  {}: 当前={} (值={})",
            display.name,
            name,
            input.map(|v| v.to_string()).unwrap_or_else(|| "不可读".into())
        );
    }
    println!("热键: {}", sw.config.hotkey);
    if !sw.config.switch_order.is_empty() {
        println!("轮转顺序: {}", sw.config.switch_order.join(" → "));
    }

    if sw.config.network.enabled {
        let mesh = Mesh::new(sw.config.node.clone(), sw.config.network.port, false);
        if mesh.start(Box::new(|_, _, _| {})).is_ok() {
            sleep(Duration::from_millis(2500));
            let peers = mesh.live_peers();
            println!("\n局域网对端: {} 台", peers.len());
            for peer in &peers {
                println!(
                    "  {}  DDC={}  占用的口={}  地址={}",
                    peer.node,
                    if peer.ddc_ok { "可用" } else { "不可用" },
                    if peer.knows_port() {
                        format!("0x{:02X}", peer.own_value)
                    } else {
                        "未知".to_string()
                    },
                    peer.addr.ip()
                );
            }
            if peers.is_empty() {
                println!("  （没有发现对端，确认另一台也在跑 daemon 且同一网段）");
            }
            mesh.stop();
        }
    }
    0
}

/// 查看或设置配对码。填了就立刻写盘，GUI 下次启动（或面板里改）生效。
pub fn cmd_pair(code: Option<&str>, generate: bool, clear: bool) -> i32 {
    let mut config = Config::load();
    if clear {
        config.pair.code.clear();
        if let Err(err) = config.save() {
            eprintln!("保存失败：{err}");
            return 1;
        }
        println!("已取消配对：本机不再签名，也不再验签");
        println!("  同一网段的任何设备都能给你的显示器发指令。");
        println!("  对面那台如果还留着码，它会拒收你的指令 —— 要么也取消，要么两边重新填同一个码。");
        return 0;
    }
    if generate {
        let fresh = inputswitch::pair::random_code();
        config.pair.code = fresh.clone();
        if let Err(err) = config.save() {
            eprintln!("保存失败：{err}");
            return 1;
        }
        println!("新配对码：{fresh}");
        println!("在另一台上执行 inputswitch-cli pair {fresh}（或在面板里填同一个）");
        return 0;
    }
    if let Some(code) = code {
        let trimmed = code.trim().to_string();
        if trimmed.is_empty() {
            // 空字符串容易被 shell 吃掉参数，语义也不明确，直接让用户说清楚
            println!("传入的是空码。要取消配对请用：inputswitch-cli pair --clear");
            return 1;
        }
        config.pair.code = trimmed.clone();
        if let Err(err) = config.save() {
            eprintln!("保存失败：{err}");
            return 1;
        }
        println!("配对码已保存：{}", mask_fingerprint(&trimmed));
        return 0;
    }

    if config.pair.is_set() {
        let key = inputswitch::pair::link_key(&config.pair.code).unwrap_or([0u8; 32]);
        println!("本机节点：{}", config.node);
        println!("配对码：{}", mask_fingerprint(&config.pair.code));
        println!("密钥指纹：{}", inputswitch::pair::sign(&key, "inputswitch-fingerprint"));
        println!("状态：已启用签名，未签名的指令一律丢弃");
    } else {
        println!("本机节点：{}", config.node);
        println!("状态：未配对 —— 同一网段的任何设备都能给你的显示器发指令");
        println!("建议：inputswitch-cli pair --generate，然后在另一台填同一个码");
    }
    0
}

fn mask_fingerprint(code: &str) -> String {
    let chars: Vec<char> = code.trim().chars().collect();
    if chars.len() <= 4 {
        return "*".repeat(chars.len());
    }
    let head: String = chars.iter().take(2).collect();
    let tail: String = chars.iter().skip(chars.len() - 2).collect();
    format!("{head}**{tail}")
}

/// 直接把文件推给局域网里的对端，用来验证传输通道（不经过托盘）。
pub fn cmd_send(paths: &[String], wait_secs: u64) -> i32 {
    if paths.is_empty() {
        eprintln!("要给至少一个文件或目录");
        return 2;
    }
    let config = Config::load();
    let mesh = Mesh::new(config.node.clone(), config.network.port, false);
    mesh.set_key(inputswitch::pair::link_key(&config.pair.code));
    if let Err(err) = mesh.start(Box::new(|_, _, _| {})) {
        eprintln!("局域网监听失败：{err}");
        return 1;
    }

    print!("等待对端出现");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait_secs);
    let mut peer = None;
    while std::time::Instant::now() < deadline {
        if let Some(found) = mesh.live_peers().into_iter().next() {
            peer = Some(found);
            break;
        }
        print!(".");
        use std::io::Write;
        let _ = std::io::stdout().flush();
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    println!();
    let Some(peer) = peer else {
        eprintln!("{} 秒内没发现对端。确认对面开着本程序、同一个 Wi-Fi/交换机", wait_secs);
        return 1;
    };

    println!(
        "→ {}（{}，{}）",
        peer.node,
        peer.addr.ip(),
        if peer.paired { "已配对" } else { "未配对" }
    );
    let files: Vec<std::path::PathBuf> = paths.iter().map(std::path::PathBuf::from).collect();
    let key = inputswitch::pair::link_key(&config.pair.code);
    let mut last = 0u64;
    let result = inputswitch::transfer::send(peer.addr, &config.node, &files, key, false, None, |sent, total| {
        // total 为 0（空目录）算作 100%。用 checked_div 而不是
        // `if total == 0 { .. } else { .. / total }` —— 后者是
        // clippy::manual_checked_ops 盯的写法，除零与溢出一起防。
        let pct = sent
            .checked_mul(100)
            .and_then(|num| num.checked_div(total))
            .unwrap_or(100);
        if pct / 10 != last / 10 {
            last = pct;
            print!("\r{pct}%");
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
    });
    println!();
    match result {
        Ok(message) => {
            println!("✓ {message}");
            0
        }
        Err(err) => {
            eprintln!("✗ {err}");
            1
        }
    }
}
