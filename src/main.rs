//! 命令行入口。
//!
//! 只放接口：clap 定义 + 分发表。20 个子命令的实现按「跑下去会不会真的
//! 改动屏幕」分在 `cli/` 下五个文件，切分轴见 `cli/mod.rs` 顶部说明。

mod cli;

use clap::{Parser, Subcommand};

use cli::{act, display, kvm, net, service, update};

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
    /// 检查更新（可顺带下载校验、安装重启，不依赖图形界面）
    Update {
        /// 有新版时接着下载并校验 SHA-256（只下不装）
        #[arg(long)]
        download: bool,
        /// 下载并校验后接着静默安装、重启应用（隐含 --download）
        #[arg(long)]
        install: bool,
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
        None | Some(Command::Daemon) => service::cmd_daemon(),
        Some(Command::Probe) => display::cmd_probe(),
        Some(Command::Selftest) => display::cmd_selftest(),
        Some(Command::Switch {
            input,
            model,
            no_verify,
            relay,
        }) => act::cmd_switch(&input, model.as_deref(), !no_verify, relay),
        Some(Command::Wizard {
            own,
            other,
            model,
            max_tries,
            yes,
        }) => act::cmd_wizard(&own, &other, model.as_deref(), max_tries, yes),
        Some(Command::Raw { code }) => display::cmd_raw(&code),
        Some(Command::Write { code, value }) => act::cmd_write(&code, &value),
        Some(Command::Status) => net::cmd_status(),
        Some(Command::Autostart { on, off }) => service::cmd_autostart(on, off),
        Some(Command::Info) => display::cmd_info(),
        Some(Command::Rename { value, name }) => act::cmd_rename(&value, &name),
        Some(Command::Menu) => display::cmd_menu(),
        Some(Command::Check) => display::cmd_check(),
        Some(Command::Pair {
            code,
            generate,
            clear,
        }) => net::cmd_pair(code.as_deref(), generate, clear),
        Some(Command::Send { paths, wait }) => net::cmd_send(&paths, wait),
        Some(Command::Clip { text }) => service::cmd_clip(text.as_deref()),
        Some(Command::Update { download, install }) => update::cmd_update(download, install),
        Some(Command::KvmProbe) => kvm::cmd_kvm_probe(),
        Some(Command::InjectProbe) => kvm::cmd_inject_probe(),
        Some(Command::Kvm {
            peer,
            mode,
            role,
            side,
            seconds,
        }) => kvm::cmd_kvm(&peer, &mode, &role, &side, seconds),
        Some(Command::Scan {
            all,
            yes,
            idle_window,
            only,
        }) => act::cmd_scan(all, yes, idle_window, only.as_deref()),
    };
    std::process::exit(code);
}
