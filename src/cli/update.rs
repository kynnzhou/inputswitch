//! `update` 子命令：检查更新，`--download` 时顺带下载并校验。
//!
//! 单独一个文件而不是塞进 `service.rs`：`service` 那组是「常驻/服务项」
//! （daemon / autostart / clip），而 update 是唯一**要联网**的命令，
//! 排查时最需要单独看清它到底访问了哪个源、卡在哪一步。
//!
//! 这里也是**无界面验证整条链路**的入口：`inputswitch-cli update --download`
//! 跑一遍就知道清单能不能拉、版本比得对不对、包能不能下、摘要对不对，
//! 不用先开托盘再点面板。P2 的安装能力还没接，所以下完只报路径。

use std::io::Write;
use std::time::Duration;

use inputswitch::update::{self, Phase};

/// 返回进程退出码：0 = 完成（有无新版都算完成），1 = 失败。
pub fn cmd_update(download: bool) -> i32 {
    println!("当前版本：v{}", inputswitch::VERSION);
    let urls = update::source::manifest_urls();
    for (i, u) in urls.iter().enumerate() {
        println!("  候选源 {}：{u}", i + 1);
    }
    println!("正在检查…");

    let s = update::check_blocking();
    match s.phase {
        Phase::Available => {
            println!();
            println!("有新版本：v{} → v{}", s.current, s.latest);
            if !s.pub_date.is_empty() {
                println!("发布时间：{}", s.pub_date);
            }
            if !s.notes.is_empty() {
                println!("更新说明：");
                for line in s.notes.lines() {
                    println!("  {line}");
                }
            }
            println!("安装包：{}", s.url);
            println!("  SHA-256：{}", s.checksum);
            if !download {
                println!();
                println!("（加 --download 可下载并校验；安装能力在下一期接）");
                return 0;
            }
            download_and_report()
        }
        Phase::UpToDate => {
            println!("已是最新（v{}）", s.current);
            0
        }
        Phase::Error => report_error(&s.error, &s.code),
        other => {
            // Checking 不该出现在这里（check_blocking 是阻塞的，返回时已经出结果）
            println!("状态：{other:?}");
            0
        }
    }
}

/// 下载 + 校验。进度靠轮询全局状态：`start_blocking` 在别的线程跑，
/// 主线程每 250ms 看一眼 —— 和面板 P3 要做的完全是同一件事，
/// 这里先把这条路趟通。
fn download_and_report() -> i32 {
    println!();
    println!("开始下载…");
    let worker = std::thread::spawn(update::start_blocking);

    let mut last_line = u64::MAX;
    loop {
        if worker.is_finished() {
            break;
        }
        let s = update::snapshot();
        if s.phase == Phase::Downloading && s.downloaded != last_line {
            last_line = s.downloaded;
            // 服务器没给 Content-Length 时 total 是 0，除不了就说「?%」
            let pct = match s
                .downloaded
                .checked_mul(100)
                .and_then(|n| n.checked_div(s.total))
            {
                Some(p) => format!("{p:>3}%"),
                None => "  ?%".to_string(),
            };
            let total = if s.total > 0 {
                human(s.total)
            } else {
                "?".to_string()
            };
            print!(
                "\r  {pct}  {} / {total}  {}/s   ",
                human(s.downloaded),
                human(s.bytes_per_sec)
            );
            let _ = std::io::stdout().flush();
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    println!();

    let s = worker.join().unwrap_or_else(|_| update::snapshot());
    match s.phase {
        Phase::Ready => {
            println!("下载完成，摘要校验通过（{}）", human(s.downloaded));
            match update::pending_installer() {
                Some(p) => println!("安装包已就绪：{}", p.display()),
                None => println!("安装包路径未知（内部状态被清空）"),
            }
            println!();
            println!("（当前版本还没有自动安装能力，P2 会接上静默安装与重启）");
            0
        }
        Phase::Error => report_error(&s.error, &s.code),
        other => {
            println!("下载结束，状态：{other:?}");
            0
        }
    }
}

fn report_error(message: &str, code: &str) -> i32 {
    eprintln!("失败：{message}");
    if !code.is_empty() {
        eprintln!("  错误分类：{code}");
    }
    eprintln!();
    eprintln!("排查建议：");
    eprintln!("  1. 用浏览器打开上面任一个候选源，确认网络能到");
    eprintln!("  2. 如果配了代理（HTTPS_PROXY）但代理没开，清掉那个环境变量再试");
    eprintln!("  3. 如果所有源都返回 404，说明这个 release 里没有 updater.json");
    if code == "checksum" {
        eprintln!("  4. 摘要不匹配通常是镜像缓存了坏包，删掉临时目录再试一次");
    }
    1
}

/// 字节数 → 「12.3 MB」。只用于给人看，不追求精确。
fn human(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}
