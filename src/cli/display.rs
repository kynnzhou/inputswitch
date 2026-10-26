//! 只读诊断：这些子命令不改动屏幕，排查时放心跑。
//!
//! 另一面在 `act.rs` —— 那边的命令会把输入源切走、让屏幕黑几秒。

use inputswitch::models::{Presence, VCP_BRIGHTNESS, VCP_INPUT_SOURCE};
use inputswitch::Switcher;

use super::print_advice;

pub fn cmd_probe() -> i32 {
    let sw = Switcher::new();
    let displays = sw.displays();
    if displays.is_empty() {
        println!("没有发现可发送 DDC 的显示器。");
        return 1;
    }
    for display in displays {
        println!("\n■ {}", display.name);
        println!("  uid      : {}", display.uid);
        println!("  平台     : {}   主屏: {}", display.platform, display.is_main);
        let cap = sw.capability(&display);
        println!("  DDC 能力 : {} {}", cap.mark(), cap.label());
        let input = sw.backend().get_vcp(&display, VCP_INPUT_SOURCE);
        let brightness = sw.backend().get_vcp(&display, VCP_BRIGHTNESS);
        println!(
            "  当前输入源: {}",
            input.map(|v| v.to_string()).unwrap_or_else(|| "读取不到".into())
        );
        println!(
            "  当前亮度  : {}",
            brightness.map(|v| v.to_string()).unwrap_or_else(|| "读取不到".into())
        );
        let note = sw.backend().last_note();
        if !note.is_empty() {
            println!("  读诊断   : {}", note);
        }
        match sw.config.profile_for(&display.name) {
            Some(profile) if !profile.inputs.is_empty() => {
                let known = profile
                    .inputs
                    .iter()
                    .map(|s| format!("{}={}", s.name, s.value))
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("  已配置   : {}", known);
            }
            _ => println!("  已配置   : 无（运行 inputswitch wizard）"),
        }
    }
    println!("\n■ 通路诊断");
    println!("  {}", inputswitch::ddc::diagnose());
    println!("\n配置文件: {}", inputswitch::config::Config::path().display());
    0
}

pub fn cmd_selftest() -> i32 {
    println!("配置文件: {}", inputswitch::config::Config::path().display());
    let sw = Switcher::new();
    let displays = sw.displays();
    println!("显示器数量: {}", displays.len());
    if displays.is_empty() {
        println!("\n没有枚举到任何可发 DDC 的显示器。");
        print_advice();
        return 1;
    }
    let mut problems = 0;
    for display in &displays {
        let cap = sw.capability(display);
        let input = sw.backend().get_vcp(display, VCP_INPUT_SOURCE);
        println!(
            "  - {}: {} {}, 输入源={}",
            display.name,
            cap.mark(),
            cap.label(),
            input.map(|v| v.to_string()).unwrap_or_else(|| "不可读".into())
        );
        let note = sw.backend().last_note();
        if !note.is_empty() {
            println!("    读诊断: {}", note);
        }
        match cap {
            inputswitch::Capability::Unsupported => problems += 1,
            inputswitch::Capability::WriteOnly => {
                println!("    注: 只能写不能读，切换是否生效靠显示器拓扑变化判断");
            }
            _ => {}
        }
    }
    if problems > 0 {
        print_advice();
        return 1;
    }
    println!("\nDDC 通道正常。");
    0
}

pub fn cmd_raw(code: &str) -> i32 {
    let parsed = u8::from_str_radix(code.trim_start_matches("0x").trim_start_matches("0X"), 16);
    let Ok(code) = parsed else {
        println!("VCP 代码格式不对：{}", code);
        return 1;
    };
    let sw = Switcher::new();
    let Some(display) = sw.pick_display(None) else {
        println!("没有找到可操作的显示器（屏幕可能已经在对面那台）。");
        return 1;
    };
    println!("显示器: {} ({})", display.name, display.uid);
    println!("VCP 0x{:02x}", code);

    #[cfg(target_os = "macos")]
    {
        let backend = inputswitch::ddc::mac::MacBackend::new();
        let replies = backend.raw_read(&display, code);
        if replies.is_empty() {
            println!("  打不开 DCP AV 服务，无法读 I2C。");
            return 1;
        }
        for (idx, r) in replies.iter().enumerate() {
            let body = r
                .bytes
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            println!(
                "  #{} chip=0x{:02x} addr=0x{:02x} rc={:<3} {}",
                idx + 1,
                r.chip,
                r.addr,
                r.rc,
                body
            );
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        println!("  原始帧抓取只在 macOS 上实现。");
        println!(
            "  当前值: {:?}",
            sw.backend().get_vcp(&display, code)
        );
    }
    0
}

pub fn cmd_info() -> i32 {
    let sw = Switcher::new();
    let Some(display) = sw.pick_display(None) else {
        println!("没有找到可操作的显示器（屏幕可能已经在对面那台）。");
        return 1;
    };
    println!("■ {}", display.name);

    // EDID：身份信息的唯一可靠来源
    match sw.backend().read_edid(&display) {
        Some(bytes) => match inputswitch::edid::parse(&bytes) {
            Some(edid) => {
                println!("  配置键    : {}", edid.identity());
                println!(
                    "  厂商/型号 : {} / {:04X}",
                    if edid.vendor.is_empty() {
                        "?"
                    } else {
                        &edid.vendor
                    },
                    edid.product
                );
                if let Some(text) = &edid.serial_text {
                    println!("  序列号    : {}（数字 {}）", text, edid.serial);
                } else {
                    println!("  序列号    : {}", edid.serial);
                }
                if let Some(date) = edid.manufactured() {
                    println!("  生产日期  : {}", date);
                }
                if let Some(inch) = edid.diagonal_inch() {
                    let (w, h) = edid.size_cm.unwrap_or((0, 0));
                    println!("  尺寸      : {} x {} cm（{:.1} 英寸）", w, h, inch);
                }
                println!(
                    "  输入类型  : {}",
                    if edid.digital { "数字" } else { "模拟" }
                );
            }
            None => println!("  EDID 解析失败（{} 字节）", bytes.len()),
        },
        None => println!("  EDID 读不到（该接口不转发 I2C EEPROM）"),
    }

    // 能力表：这台显示器支持哪些输入源
    match sw.backend().capabilities(&display) {
        Some(caps) => {
            if caps.has_inputs() {
                let list: Vec<String> = caps
                    .inputs
                    .iter()
                    .map(|v| match inputswitch::caps::standard_hint(*v) {
                        Some(hint) => format!("0x{:02X}({})", v, hint),
                        None => format!("0x{:02X}", v),
                    })
                    .collect();
                println!("  支持的输入源: {}", list.join("  "));
            } else {
                println!("  支持的输入源: 能力表里没有声明");
            }
            if let Some(model) = &caps.model {
                println!("  能力表型号: {}", model);
            }
            if let Some(ver) = &caps.mccs_version {
                println!("  MCCS 版本 : {}", ver);
            }
            let raw: String = caps
                .raw
                .chars()
                .map(|c| if c.is_ascii_graphic() || c == ' ' { c } else { ' ' })
                .collect();
            println!("  能力表原文: {}", raw.trim());
        }
        None => println!("  能力表读不到（显示器未实现 VCP 0xDF）"),
    }

    let cap = sw.capability(&display);
    println!("  DDC 通道  : {} {}", cap.mark(), cap.label());
    match sw.backend().get_vcp(&display, VCP_INPUT_SOURCE) {
        Some(value) => println!("  当前输入源: 0x{:02X} ({})", value, value),
        None => println!("  当前输入源: {}（读不到）", sw.backend().last_note()),
    }
    0
}

/// 把托盘菜单里的输入源列表按文本打印一遍，方便不开图形界面也能核对。
pub fn cmd_menu() -> i32 {
    let sw = Switcher::new();
    let Some(display) = sw.pick_display(None) else {
        println!("没有找到可操作的显示器（屏幕可能已经在对面那台）。");
        return 1;
    };
    let key = sw.identity(&display);
    let current = sw.backend().get_vcp(&display, VCP_INPUT_SOURCE);
    println!("■ {}（配置键 {}）", display.name, key);
    if let Some(value) = current {
        println!("  本机当前: 0x{:02X}", value);
    }
    let caps = sw.capabilities(&display);
    let report = inputswitch::support::check(sw.backend(), &display, caps);
    println!("  支持情况: {}", report.headline());
    let Some(profile) = sw.config.profile_for(&key) else {
        println!("  这个显示器还没登记任何输入口");
        return 1;
    };
    println!("  切换输入源");
    for src in &profile.inputs {
        let mark = if current == Some(src.value) { "✓" } else { "　" };
        let mut tail = String::new();
        if current == Some(src.value) {
            tail.push_str(" · 本机");
        }
        if src.presence != Presence::Occupied {
            tail.push_str(src.presence.mark());
        } else if current != Some(src.value) {
            tail.push_str(" · 有主机");
        }
        if !src.verified && src.presence == Presence::Unknown {
            tail.push_str(" · 待确认");
        }
        println!(
            "    {}{}  (0x{:02X}){}",
            mark,
            sw.config.label_for(&src.name),
            src.value,
            tail
        );
    }
    println!(
        "  热键: {}   热键目标: {}",
        sw.config.hotkey,
        if sw.config.hotkey_target.is_empty() {
            "未设置".to_string()
        } else {
            sw.config.label_for(&sw.config.hotkey_target)
        }
    );
    0
}

pub fn cmd_check() -> i32 {
    let sw = Switcher::new();
    let Some(display) = sw.pick_display(None) else {
        println!("屏幕不在这台机器上，无法检测。");
        println!("（只有持有屏幕的那台才读得到显示器的 DDC 应答）");
        return 1;
    };
    println!("■ {} ({})", display.name, display.platform);
    let caps = sw.capabilities(&display);
    let report = inputswitch::support::check(sw.backend(), &display, caps);
    let verdict = report.verdict();

    println!("{}", report.headline());
    println!();
    println!(
        "  DDC 传输层 : {}",
        if report.transport { "通" } else { "不通" }
    );
    match report.current {
        Some(v) => println!("  读输入源   : 0x{:02X}", v),
        None => println!("  读输入源   : 读不到"),
    }
    println!(
        "  写输入源   : {}",
        match report.writable {
            Some(true) => "通（写入被接受）",
            Some(false) => "被拒绝",
            None => "无法确认",
        }
    );
    println!(
        "  能力表     : {}",
        if !report.caps_seen {
            "读不到".to_string()
        } else if report.caps_declares_input {
            "读到，声明支持输入源切换".to_string()
        } else {
            "读到，但没声明 0x60".to_string()
        }
    );
    if !report.inputs.is_empty() {
        let list: Vec<String> = report
            .inputs
            .iter()
            .map(|v| format!("0x{:02X}", v))
            .collect();
        println!("  可切换的口 : {}", list.join("  "));
    }

    if !report.notes.is_empty() {
        println!();
        for note in &report.notes {
            println!("  · {}", note);
        }
    }

    let advice = verdict.advice();
    if !advice.is_empty() {
        println!();
        println!("结论：{}", advice);
    }
    0
}
