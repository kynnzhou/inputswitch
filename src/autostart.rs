//! 开机自启。
//!
//! macOS 写一个 LaunchAgent plist（用户级，不需要管理员权限）；
//! Windows 写 HKCU 的 Run 键。

use std::path::PathBuf;
#[cfg(windows)]
use std::sync::Mutex;

#[cfg(target_os = "macos")]
const LABEL: &str = "cn.zhouke.inputswitch";
#[cfg(windows)]
const RUN_KEY: &str = "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run";
#[cfg(windows)]
const VALUE_NAME: &str = "InputSwitch";

/// Windows 下查自启状态要开子进程（reg.exe），而托盘菜单每隔几秒就要重建一次。
/// 每重建一次就 spawn 一次，屏幕上就闪一个黑窗。所以只在第一次真去查，
/// 之后用内存里的值；enable / disable 会主动更新缓存。
#[cfg(windows)]
static CACHE: Mutex<Option<bool>> = Mutex::new(None);

fn exe_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("InputSwitch"))
}

#[cfg(target_os = "macos")]
fn plist_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{}.plist", LABEL))
}

/// reg.exe 的绝对路径，避免 PATH 被改过时找不到或被同名程序劫持。
#[cfg(windows)]
fn reg_exe() -> PathBuf {
    std::env::var("SystemRoot")
        .map(|root| PathBuf::from(root).join("System32").join("reg.exe"))
        .unwrap_or_else(|_| PathBuf::from("reg"))
}

#[cfg(windows)]
fn remember(value: bool) {
    if let Ok(mut guard) = CACHE.lock() {
        *guard = Some(value);
    }
}

/// 真正去注册表里查一次。
#[cfg(windows)]
fn query_run_key() -> bool {
    let mut cmd = std::process::Command::new(reg_exe());
    cmd.args(["query", RUN_KEY, "/v", VALUE_NAME])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    crate::hidden::quiet(&mut cmd)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 当前是否已开启
pub fn is_enabled() -> bool {
    #[cfg(windows)]
    {
        if let Ok(guard) = CACHE.lock() {
            if let Some(value) = *guard {
                return value;
            }
        }
        let value = query_run_key();
        remember(value);
        value
    }
    #[cfg(target_os = "macos")]
    {
        plist_path().exists()
    }
    #[cfg(target_os = "linux")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home)
            .join(".config/autostart/inputswitch.desktop")
            .exists()
    }
}

pub fn enable() -> Result<(), String> {
    let exe = exe_path();
    #[cfg(target_os = "macos")]
    {
        let path = plist_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("建目录失败: {}", e))?;
        }
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{}</string>
    <key>ProgramArguments</key>
    <array><string>{}</string></array>
    <key>RunAtLoad</key><true/>
    <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
"#,
            LABEL,
            exe.display()
        );
        std::fs::write(&path, xml).map_err(|e| format!("写 plist 失败: {}", e))?;
        // 不调 launchctl：~/Library/LaunchAgents 下的 plist 下次登录会自动加载。
        // 主动 load 需要额外权限，而且在部分沙箱环境会卡住，得不偿失。
        Ok(())
    }
    #[cfg(windows)]
    {
        let mut cmd = std::process::Command::new(reg_exe());
        cmd.args([
            "add",
            RUN_KEY,
            "/v",
            VALUE_NAME,
            "/t",
            "REG_SZ",
            "/d",
            &format!("\"{}\"", exe.display()),
            "/f",
        ]);
        let status = crate::hidden::quiet(&mut cmd)
            .status()
            .map_err(|e| format!("调用 reg 失败: {}", e))?;
        if status.success() {
            remember(true);
            Ok(())
        } else {
            Err("写入注册表失败".into())
        }
    }
    #[cfg(target_os = "linux")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        let path = PathBuf::from(home).join(".config/autostart/inputswitch.desktop");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("建目录失败: {}", e))?;
        }
        let body = format!(
            "[Desktop Entry]\nType=Application\nName=InputSwitch\nExec={}\nX-GNOME-Autostart-enabled=true\n",
            exe.display()
        );
        std::fs::write(&path, body).map_err(|e| format!("写 desktop 文件失败: {}", e))
    }
}

pub fn disable() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let path = plist_path();
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("删除 plist 失败: {}", e))?;
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        let mut cmd = std::process::Command::new(reg_exe());
        cmd.args(["delete", RUN_KEY, "/v", VALUE_NAME, "/f"]);
        let status = crate::hidden::quiet(&mut cmd)
            .status()
            .map_err(|e| format!("调用 reg 失败: {}", e))?;
        if status.success() {
            remember(false);
            Ok(())
        } else {
            Err("删除注册表项失败".into())
        }
    }
    #[cfg(target_os = "linux")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        let path = PathBuf::from(home).join(".config/autostart/inputswitch.desktop");
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("删除失败: {}", e))?;
        }
        Ok(())
    }
}
