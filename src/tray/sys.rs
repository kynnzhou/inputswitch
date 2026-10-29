//! 与操作系统打交道的一揽子：图标、打开文件夹、重启进程、配对码掩码。
//!
//! 四个函数都是"平台三分支 + 只 spawn 不等待"的同构写法，合在一起才看得出
//! 一致性（哪个平台漏了 hidden::quiet 一眼就能发现）。

use std::thread;
use std::time::Duration;

use tray_icon::Icon;

use super::log_line;

/// 菜单栏 / 通知区域的图标。
///
/// 两个平台要的不是同一张图：
/// * macOS 用模板图（只有 alpha，系统按菜单栏明暗自动反色），22×22 那张的 @2x。
///   `with_icon_as_template(true)` 让系统把它当模板处理，图标里的颜色会被丢掉 ——
///   所以那张图里的黑只是占位，实际显示成什么颜色由系统定。
/// * Windows 没有模板图，通知区域是彩色小图，用单独画的那张：字形更粗、
///   不带深色底板。带底板在浅色任务栏上会变成一块突兀的深色方块，
///   比旁边的系统图标大一圈。
pub(super) fn load_icon() -> Option<Icon> {
    // include_bytes! 以**本文件**为基准解析相对路径（现在是 src/tray/ 下，
    // 比原来深了一层），所以assets 要往上走两级。
    #[cfg(target_os = "windows")]
    let bytes = include_bytes!("../../assets/tray-win-32.png");
    #[cfg(not(target_os = "windows"))]
    let bytes = include_bytes!("../../assets/menubar@2x.png");

    let image = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (width, height) = image.dimensions();
    Icon::from_rgba(image.into_raw(), width, height).ok()
}

///
/// 存在的理由只有一个：**macOS 把 TCC 授权结论缓存在进程里**。用户在设置里
/// 勾上以后，正在跑的这个进程照样报"没权限"，点「开启共用键鼠」只会再弹一次
/// 设置页 —— 看起来就是"勾选无效 / 装错版本"（2026-10-20 连踩两次）。
/// 唯一解法是重启进程，所以把它做成一个按钮。
///
/// 延迟 1 秒再拉起新实例：本进程要先退出，否则托盘图标和面板端口还被占着，
/// 新实例起来抢不到，表现为"重启之后更没反应了"。
pub(super) fn restart_app() {
    #[cfg(target_os = "macos")]
    {
        // 从 .../X.app/Contents/MacOS/X 反推 bundle 路径，装在别处也能重启
        let bundle = std::env::current_exe().ok().and_then(|exe| {
            exe.ancestors()
                .find(|p| p.extension().map(|e| e == "app").unwrap_or(false))
                .map(|p| p.to_path_buf())
        });
        let script = match bundle {
            Some(path) => format!("sleep 1; open -n '{}'", path.display()),
            None => "sleep 1; open -n -a InputSwitch".to_string(),
        };
        let _ = std::process::Command::new("sh").arg("-c").arg(script).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(exe) = std::env::current_exe() {
            let cmd = format!("timeout /t 1 >nul & start \"\" \"{}\"", exe.display());
            let mut child = std::process::Command::new("cmd");
            crate::hidden::quiet(&mut child);
            let _ = child.arg("/C").arg(cmd).spawn();
        }
    }
    log_line("重启中…（1 秒后自动起来）");
    // 日志是追加写的，退出前给文件系统一点时间把它落盘
    thread::sleep(Duration::from_millis(150));
    std::process::exit(0);
}

pub fn open_config_dir() {
    let dir = crate::config::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(target_os = "macos")]
    {
        let mut cmd = std::process::Command::new("open");
        let _ = crate::hidden::quiet(&mut cmd).arg(&dir).spawn();
    }
    #[cfg(windows)]
    {
        let mut cmd = std::process::Command::new("explorer");
        let _ = crate::hidden::quiet(&mut cmd).arg(&dir).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let mut cmd = std::process::Command::new("xdg-open");
        let _ = crate::hidden::quiet(&mut cmd).arg(&dir).spawn();
    }
}

/// 配对码显示在菜单里时只露头尾：菜单截图、录屏都可能把它带出去。
pub(super) fn mask_code(code: &str) -> String {
    let code = code.trim();
    if code.len() <= 4 {
        return "*".repeat(code.chars().count());
    }
    let chars: Vec<char> = code.chars().collect();
    let head: String = chars.iter().take(2).collect();
    let tail: String = chars.iter().skip(chars.len() - 2).collect();
    format!("{head}**{tail}")
}

pub fn open_path(path: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(path);
        c
    };
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("explorer");
        c.arg(path);
        c
    };
    #[cfg(target_os = "linux")]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(path);
        c
    };
    let _ = crate::hidden::quiet(&mut cmd).spawn();
}
