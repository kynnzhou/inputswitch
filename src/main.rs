//! 命令行入口。

use std::io::Write;
use std::thread::sleep;
use std::time::Duration;

use clap::{Parser, Subcommand};

use inputswitch::config::Config;
use inputswitch::models::{Presence, VCP_BRIGHTNESS, VCP_INPUT_SOURCE};
use inputswitch::peer::Mesh;
use inputswitch::Switcher;

#[derive(Parser)]
#[command(name = "inputswitch", version, about = "跨平台显示器输入源切换")]
struct Cli {
    /// 不带子命令时直接进入托盘模式（双击 .app 走的就是这条路）
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// 只读检查显示器与 DDC 能力（不改动屏幕）
    Probe,
    /// 诊断 DDC 通道并给出排查建议
    Selftest,
    /// 切换输入源
    Switch {
        /// 目标输入源名称，例如 usbc / dp1
        input: String,
        /// 显示器型号（多显示器时指定）
        #[arg(long)]
        model: Option<String>,
        /// 不等拓扑变化确认
        #[arg(long)]
        no_verify: bool,
        /// 本机失败时请对端代切
        #[arg(long)]
        relay: bool,
    },
    /// 发现输入源取值（会真的切换屏幕）
    Wizard {
        /// 本机接的口，例如 usbc / dp1
        #[arg(long)]
        own: String,
        /// 要切过去的口，例如 dp1 / usbc
        #[arg(long)]
        other: String,
        #[arg(long)]
        model: Option<String>,
        /// 最多试几个候选值
        #[arg(long, default_value_t = 8)]
        max_tries: usize,
        /// 跳过确认
        #[arg(short, long)]
        yes: bool,
    },
    /// 查看当前状态与局域网对端
    Status,
    /// 开机自启开关
    Autostart {
        #[arg(long, conflicts_with = "off")]
        on: bool,
        #[arg(long)]
        off: bool,
    },
    /// 抓 DDC 原始应答帧（诊断用）
    Raw {
        /// VCP 代码，十六进制，例如 0x60 输入源 / 0x10 亮度
        #[arg(long, default_value = "0x60")]
        code: String,
    },
    /// 写一次 VCP 并读回校验（会真的改显示器）
    Write {
        /// VCP 代码，十六进制
        code: String,
        /// 要写入的值，十进制或 0x 前缀十六进制
        value: String,
    },
    /// 常驻：热键 + 双机协同
    Daemon,
    /// 读显示器身份与能力（型号、序列号、支持哪些输入源）
    Info,
    /// 给某个输入口改名，例如把 0x10 标成 DP
    Rename {
        /// VCP 0x60 取值，例如 0x10
        value: String,
        /// 新名字，例如 DP
        name: String,
    },
    /// 按托盘菜单的样子打印一遍输入源列表（不依赖图形界面）
    Menu,
    /// 检测这台显示器能不能被软件切换（只读，不动画面）
    Check,
    /// 扫描每个输入口上有没有主机（会真的切换屏幕）
    Scan {
        /// 扫完所有口，不在第一个有主机的口停下
        #[arg(long)]
        all: bool,
        /// 跳过确认
        #[arg(short, long)]
        yes: bool,
        /// 切过去后等几秒判断"显示器会不会自己退回来"
        #[arg(long, default_value_t = 10)]
        idle_window: u64,
        /// 只扫这几个口（十六进制，逗号分隔），例如 --only 0x11
        #[arg(long)]
        only: Option<String>,
    },
    /// 查看或设置配对码（两台填同一个）
    Pair {
        /// 不填就只打印当前码和指纹
        code: Option<String>,
        /// 生成一个随机 6 位码并保存
        #[arg(long)]
        generate: bool,
        /// 取消配对：清掉本机的码，之后不再签名也不再验签
        #[arg(long)]
        clear: bool,
    },
    /// 把文件发给局域网里的对端（会真的传）
    Send {
        /// 文件或目录，可以给多个
        paths: Vec<String>,
        /// 等对端出现的最长秒数
        #[arg(long, default_value_t = 10)]
        wait: u64,
    },
    /// 读或写本机剪贴板（自检用）
    Clip {
        /// 不填就打印当前剪贴板文本
        text: Option<String>,
    },
    /// 检查共用键鼠需要的权限和屏幕尺寸
    KvmProbe,
    /// 注入自检：把光标挪到屏幕中央再读回位置（不依赖网络）
    InjectProbe,
    /// 启动共用键鼠
    Kvm {
        /// 对端 IP，例如 192.168.0.40
        #[arg(long)]
        peer: String,
        /// follow = 跟随画面（一套键鼠）；edge = 多屏共享（两台各一套）
        #[arg(long, default_value = "follow")]
        mode: String,
        /// console = 键鼠插在本机；remote = 键鼠在对面。只在 follow 模式用
        #[arg(long, default_value = "console")]
        role: String,
        /// 多屏共享模式下从哪一侧越界：right / left / top / bottom
        #[arg(long, default_value = "right")]
        side: String,
        /// 跑多少秒后自动退出（0 = 一直跑到 Ctrl+C）
        #[arg(long, default_value_t = 0)]
        seconds: u64,
    },
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.command {
        None | Some(Command::Daemon) => cmd_daemon(),
        Some(Command::Probe) => cmd_probe(),
        Some(Command::Selftest) => cmd_selftest(),
        Some(Command::Switch {
            input,
            model,
            no_verify,
            relay,
        }) => cmd_switch(&input, model.as_deref(), !no_verify, relay),
        Some(Command::Wizard {
            own,
            other,
            model,
            max_tries,
            yes,
        }) => cmd_wizard(&own, &other, model.as_deref(), max_tries, yes),
        Some(Command::Raw { code }) => cmd_raw(&code),
        Some(Command::Write { code, value }) => cmd_write(&code, &value),
        Some(Command::Status) => cmd_status(),
        Some(Command::Autostart { on, off }) => cmd_autostart(on, off),
        Some(Command::Info) => cmd_info(),
        Some(Command::Rename { value, name }) => cmd_rename(&value, &name),
        Some(Command::Menu) => cmd_menu(),
        Some(Command::Check) => cmd_check(),
        Some(Command::Pair {
            code,
            generate,
            clear,
        }) => cmd_pair(code.as_deref(), generate, clear),
        Some(Command::Send { paths, wait }) => cmd_send(&paths, wait),
        Some(Command::Clip { text }) => cmd_clip(text.as_deref()),
        Some(Command::KvmProbe) => cmd_kvm_probe(),
        Some(Command::InjectProbe) => cmd_inject_probe(),
        Some(Command::Kvm {
            peer,
            mode,
            role,
            side,
            seconds,
        }) => cmd_kvm(&peer, &mode, &role, &side, seconds),
        Some(Command::Scan {
            all,
            yes,
            idle_window,
            only,
        }) => cmd_scan(all, yes, idle_window, only.as_deref()),
    };
    std::process::exit(code);
}

fn cmd_probe() -> i32 {
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

fn cmd_selftest() -> i32 {
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

fn print_advice() {
    println!("\n按顺序排查（命中率从高到低）：");
    println!("  1. 显示器 OSD 里把 DDC/CI 打开 —— 很多机器出厂是关的");
    println!("  2. 不要经过扩展坞/转接头，用线直连，DDC 在多数 hub 上会被吃掉");
    println!("  3. 有的显示器只在 DP/HDMI 口转发 DDC，USB-C 口不转发");
    println!("  4. 虚拟机/远程桌面里拿不到 DDC");
}

fn cmd_switch(input: &str, model: Option<&str>, verify: bool, relay: bool) -> i32 {
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

fn cmd_wizard(
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

fn cmd_autostart(on: bool, off: bool) -> i32 {
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

fn cmd_status() -> i32 {
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

fn cmd_raw(code: &str) -> i32 {
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

fn cmd_info() -> i32 {
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

fn cmd_rename(value: &str, name: &str) -> i32 {
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

/// 把托盘菜单里的输入源列表按文本打印一遍，方便不开图形界面也能核对。
fn cmd_menu() -> i32 {
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

fn cmd_check() -> i32 {
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

fn cmd_scan(all: bool, yes: bool, idle_window: u64, only: Option<&str>) -> i32 {
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

fn cmd_write(code: &str, value: &str) -> i32 {
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

fn cmd_daemon() -> i32 {
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
fn cmd_daemon_headless() -> i32 {
    let mut sw = Switcher::new();
    let ddc_ok = sw
        .pick_display(None)
        .map(|d| sw.capability(&d) != inputswitch::Capability::Unsupported)
        .unwrap_or(false);
    let port = sw.config.network.port;
    let node = sw.config.node.clone();

    let mesh = Mesh::new(node.clone(), port, ddc_ok);
    let started = mesh.start(Box::new(move |from, action, args| {
        if action == "switch" {
            if let Some(target) = args.get("input").and_then(|v| v.as_str()) {
                let mut inner = Switcher::new();
                let outcome = inner.switch(target, None, true, Duration::from_secs(3));
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
        sleep(Duration::from_secs(3));
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

/// 查看或设置配对码。填了就立刻写盘，GUI 下次启动（或面板里改）生效。
fn cmd_pair(code: Option<&str>, generate: bool, clear: bool) -> i32 {
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
fn cmd_send(paths: &[String], wait_secs: u64) -> i32 {
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
    let result = inputswitch::transfer::send(peer.addr.ip(), config.transfer.port, &config.node, &files, key, false, |sent, total| {
        let pct = if total == 0 { 100 } else { sent * 100 / total };
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

/// 剪贴板自检：不带参数读，带参数写。
fn cmd_clip(text: Option<&str>) -> i32 {
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

/// 注入自检：不依赖网络，直接往本机注入一次鼠标移动并读回光标位置。
/// 专门用来分辨"键鼠不工作"到底卡在传输还是卡在注入。
fn cmd_inject_probe() -> i32 {
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
fn cmd_kvm_probe() -> i32 {
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
fn cmd_kvm(peer: &str, mode: &str, role: &str, side: &str, seconds: u64) -> i32 {
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
