//! 会真的改动屏幕的子命令：切输入源、写 VCP、扫输入口。
//!
//! 共同点：跑下去本机画面可能被切走或黑几秒，所以每个都在动手前打印后果
//! 并要确认（`--yes` 之类的开关跳过确认）。只读的那半在 `display.rs`。

use std::io::Write;
use std::thread::sleep;
use std::time::Duration;

use inputswitch::models::{Presence, VCP_INPUT_SOURCE};
use inputswitch::peer::Mesh;
use inputswitch::Switcher;

use super::print_advice;

pub fn cmd_switch(input: &str, model: Option<&str>, verify: bool, relay: bool) -> i32 {
    let mut sw = Switcher::new();
    let outcome = sw.switch(input, model, verify, Duration::from_secs(3));
    println!("{}", outcome.message);
    if outcome.ok {
        return 0;
    }
    if relay {
        println!("本机切换失败，尝试请对端代为切换……");
        let sw2 = Switcher::new();
        let ddc_ok = sw2
            .pick_display(None)
            .map(|d| sw2.capability(&d) != inputswitch::Capability::Unsupported)
            .unwrap_or(false);
        let mesh = Mesh::new(sw.config.node.clone(), sw.config.network.port, ddc_ok);
        // 对端设了配对码时，不签名的指令会被它当陌生人丢掉。以前这里从不装钥匙，
        // 于是 `--relay` 在配对过的机器上 100% 无效，而界面还报"已请对端执行切换"。
        mesh.set_key(inputswitch::pair::link_key(&sw.config.pair.code));
        if mesh.start(Box::new(|_, _, _| {})).is_err() {
            println!("无法启动局域网监听");
            return 2;
        }
        sleep(Duration::from_secs(3));
        match mesh.find_ddc_capable() {
            Some(peer) => {
                let args = serde_json::json!({ "input": input, "model": model });
                match mesh.send_command(&peer.node, "switch", args) {
                    Ok(()) => println!("已请 {} 执行切换", peer.node),
                    Err(err) => println!("发不出去：{err}"),
                }
            }
            None => println!("没有找到具备 DDC 能力的对端（确认两边都开着 daemon）"),
        }
        mesh.stop();
    }
    2
}

pub fn cmd_wizard(
    own: &str,
    other: &str,
    model: Option<&str>,
    max_tries: usize,
    yes: bool,
) -> i32 {
    let mut sw = Switcher::new();
    let Some(display) = sw.pick_display(model) else {
        println!("没有找到可操作的显示器。");
        return 1;
    };
    let cap = sw.capability(&display);
    println!("目标显示器: {}", display.name);
    println!("DDC 能力  : {} {}", cap.mark(), cap.label());
    if cap == inputswitch::Capability::Unsupported {
        println!("\n这台显示器在当前连接方式下不支持 DDC/CI，无法继续。");
        print_advice();
        return 1;
    }
    println!("\n本机当前接的口: {}", own);
    println!("要找的口      : {}", other);
    println!("\n即将逐个写候选值试探。一旦命中，本机屏幕会立刻黑掉、DDC 通道失效。");
    println!("恢复方式：在对面那台机器上按热键切回来，或用显示器 OSD 手动切回。");

    if !yes {
        print!("\n继续？[y/N] ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return 1;
        }
        if !matches!(line.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("已取消。");
            return 1;
        }
    }

    let result = sw.discover(own, other, model, max_tries, |value| {
        print!("  试探 0x{:02x} ({}) ... ", value, value);
        let _ = std::io::stdout().flush();
    });
    println!();
    match result {
        Ok(value) => {
            println!("\n命中：{} = 0x{:02x} ({})", other, value, value);
            println!("已写入配置，另一台机器可直接复用（或两边都开 daemon 自动同步）。");
            0
        }
        Err(message) => {
            println!("\n{}", message);
            println!("可能原因：这个口不转发 DDC，或显示器不支持运行中切该口。");
            1
        }
    }
}

pub fn cmd_rename(value: &str, name: &str) -> i32 {
    let Some(value) = parse_num(value).filter(|v| *v <= 0xFFFF).map(|v| v as u16) else {
        println!("取值格式不对：{}", value);
        return 1;
    };
    let mut sw = Switcher::new();
    let Some(display) = sw.pick_display(None) else {
        println!("没有找到可操作的显示器（屏幕可能已经在对面那台）。");
        return 1;
    };
    let key = sw.identity(&display);
    if sw.config.rename_input(&key, value, name) {
        println!(
            "已把 {} 的 0x{:02X} 命名为「{}」，托盘菜单里会显示这个名字。",
            display.name, value, name
        );
        0
    } else {
        println!(
            "{} 的配置里没有 0x{:02X} 这个口。先用 info 看它支持哪些取值。",
            display.name, value
        );
        1
    }
}

pub fn cmd_scan(all: bool, yes: bool, idle_window: u64, only: Option<&str>) -> i32 {
    let mut sw = Switcher::new();
    let Some(display) = sw.pick_display(None) else {
        println!("屏幕不在这台机器上，无法扫描。");
        println!("扫描必须由持有屏幕的那台发起。");
        return 1;
    };
    let key = sw.identity(&display);
    let current = sw.backend().get_vcp(&display, VCP_INPUT_SOURCE);
    let total = sw
        .config
        .profile_for(&key)
        .map(|p| p.inputs.len())
        .unwrap_or(0);
    if total == 0 {
        println!("还没读到任何输入口。先运行 `inputswitch-cli info`，或在面板里点「重新检测」。");
        return 1;
    }

    println!("■ {}（{} 个输入口）", display.name, total);
    println!(
        "  本机当前: {}",
        current
            .map(|v| format!("0x{:02X}", v))
            .unwrap_or_else(|| "读不到".into())
    );
    println!();
    println!("这个操作会真的切换显示器输入源：");
    println!("  · 切到空口：显示器自己退回，屏幕黑 8 秒左右，然后恢复");
    println!("  · 切到有主机的口：会停在那台机器上，需要对面那台帮忙切回");
    println!("  · 对面没装本程序：最后会停在那边，只能手动切回");
    if !yes {
        print!("\n继续请输 yes：");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() || line.trim() != "yes" {
            println!("已取消。");
            return 1;
        }
    }

    // 先听一下局域网：对面如果在自己报口，这些口就不用切屏试探
    let mesh = Mesh::new(sw.config.node.clone(), sw.config.network.port, true);
    mesh.set_key(inputswitch::pair::link_key(&sw.config.pair.code));
    let started = mesh.start(Box::new(|_, _, _| {})).is_ok();
    let mut peer_ports: Vec<(String, u16)> = Vec::new();
    if started {
        print!("正在寻找局域网里的另一台");
        let _ = std::io::stdout().flush();
        for _ in 0..10 {
            sleep(Duration::from_millis(500));
            print!(".");
            let _ = std::io::stdout().flush();
        }
        println!();
        peer_ports = mesh
            .live_peers()
            .into_iter()
            .filter(|p| p.knows_port())
            .map(|p| (p.node.clone(), p.own_value))
            .collect();
        for (node, value) in &peer_ports {
            println!("  {} 报告自己挂在 0x{:02X}", node, value);
        }
        if peer_ports.is_empty() {
            println!("  没发现对端（对面没开程序 / 不同网段 / 防火墙拦了广播）");
            println!("  ! 没有对端代切：一旦切到有主机的口，本机就发不出指令，只能手动切回来");
        }
    } else {
        println!("局域网监听没起来（端口被托盘程序占着），跳过对端信息直接扫。");
    }

    println!();
    let only_values: Option<Vec<u16>> = only.map(|text| {
        text.split(',')
            .filter_map(|token| {
                let token = token.trim().trim_start_matches("0x").trim_start_matches("0X");
                u16::from_str_radix(token, 16).ok()
            })
            .collect()
    });
    if let Some(list) = &only_values {
        if list.is_empty() {
            println!("--only 里没有解析出任何取值（要写成 0x11 这种十六进制）");
            return 1;
        }
        let names: Vec<String> = list.iter().map(|v| format!("0x{:02X}", v)).collect();
        println!("只扫这几个口：{}", names.join(" "));
    }

    let report = match sw.scan_presence(
        &peer_ports,
        Duration::from_secs(idle_window),
        !all,
        only_values.as_deref(),
        |home| {
            // 请对面代切回原位：对面收到后会用同一套逻辑写 0x60
            let mut sent = false;
            for peer in mesh.live_peers() {
                if mesh
                    .send_command(
                        &peer.node,
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
        |step| {
            println!("  {}", step);
            let _ = std::io::stdout().flush();
        },
    ) {
        Ok(report) => report,
        Err(err) => {
            println!("扫描失败：{}", err);
            return 1;
        }
    };

    println!();
    println!("结果");
    for port in &report.ports {
        let mark = match port.presence {
            Presence::Occupied => "✓",
            Presence::Idle => "·",
            Presence::Invalid => "✗",
            Presence::Unknown => "?",
        };
        println!(
            "  {} {:<16} (0x{:02X})  {:<6}  ← {}",
            mark,
            port.name,
            port.value,
            port.presence.label(),
            port.via.label()
        );
    }

    if let Some(reason) = &report.aborted {
        println!();
        println!("注意：{}", reason);
    }
    println!();
    if report.back_home {
        println!("屏幕已回到本机。结论已存进配置，菜单和热键以后直接按这个来。");
    } else {
        println!("屏幕现在不在本机，需要手动切回（或在那台机器上点一下）。");
    }
    0
}

fn parse_num(text: &str) -> Option<u32> {
    let t = text.trim();
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()
    } else {
        t.parse::<u32>().ok()
    }
}

pub fn cmd_write(code: &str, value: &str) -> i32 {
    let Some(code) = parse_num(code).filter(|c| *c <= 0xFF).map(|c| c as u8) else {
        println!("VCP 代码格式不对：{}", code);
        return 1;
    };
    let Some(value) = parse_num(value).filter(|v| *v <= 0xFFFF).map(|v| v as u16) else {
        println!("值格式不对：{}", value);
        return 1;
    };

    let sw = Switcher::new();
    let Some(display) = sw.pick_display(None) else {
        println!("没有找到可操作的显示器。");
        return 1;
    };
    let before = sw.backend().get_vcp(&display, code);
    println!(
        "{} · VCP 0x{:02x} 当前={}",
        display.name,
        code,
        before
            .map(|v| v.to_string())
            .unwrap_or_else(|| "不可读".into())
    );

    let sent = sw.backend().set_vcp(&display, code, value);
    println!("写入 0x{:02x}=({}) 传输层: {}", code, value, if sent { "接受" } else { "拒绝" });

    sleep(Duration::from_millis(400));
    let after = sw.backend().get_vcp(&display, code);
    match (before, after) {
        (Some(_), Some(a)) if a == value => {
            println!("读回 {} —— 写入确实生效了", a);
            0
        }
        (_, Some(a)) => {
            println!("读回 {} —— 与写入值不一致，显示器没接受这次写入", a);
            1
        }
        _ => {
            println!("读不回来了（屏幕可能已被切走，或该口 DDC 失效）");
            2
        }
    }
}
