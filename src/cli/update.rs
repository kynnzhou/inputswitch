//! `update` 子命令：检查更新。
//!
//! 单独一个文件而不是塞进 `service.rs`：`service` 那组是「常驻/服务项」
//! （daemon / autostart / clip），而 update 是唯一**要联网**的命令，
//! 排查时最需要单独看清它到底访问了哪个源、卡在哪一步。
//!
//! P0 只做检查（不下载、不安装）。这样做的好处是**无界面也能验证整条
//! 链路**：`inputswitch-cli update` 跑一遍就知道清单能不能拉到、版本号
//! 比得对不对，不用先开托盘、再点面板。

use inputswitch::update::{self, Phase};

/// 返回进程退出码：0 = 检查完成（有无新版都算完成），1 = 检查失败。
pub fn cmd_update() -> i32 {
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
            println!();
            println!("（当前版本还没有自动安装能力，请手动下载上面的安装包）");
            0
        }
        Phase::UpToDate => {
            println!("已是最新（v{}）", s.current);
            0
        }
        Phase::Error => {
            eprintln!("检查失败：{}", s.error);
            if !s.code.is_empty() {
                eprintln!("  错误分类：{}", s.code);
            }
            eprintln!();
            eprintln!("排查建议：");
            eprintln!("  1. 用浏览器打开上面任一个候选源，确认网络能到");
            eprintln!("  2. 如果配了代理（HTTPS_PROXY）但代理没开，清掉那个环境变量再试");
            eprintln!("  3. 如果所有源都返回 404，说明这个 release 里没有 updater.json");
            1
        }
        other => {
            // Checking 不该出现在这里（check_blocking 是阻塞的，返回时已经出结果）
            println!("状态：{other:?}");
            0
        }
    }
}
