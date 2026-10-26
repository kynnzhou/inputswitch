//! 配置住在哪：配置目录、默认节点名。
//!
//! 三平台的 cfg 分支收在这一处（APPDATA / Library/Application Support /
//! XDG_CONFIG_HOME），别散到各文件 —— 散开就没人说得全配置装在哪个目录。

use std::path::PathBuf;

pub fn default_node() -> String {
    std::env::var("COMPUTERNAME")
        .ok()
        .or_else(|| {
            let mut cmd = std::process::Command::new("hostname");
            crate::hidden::quiet(&mut cmd)
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| "node".to_string())
        .trim_end_matches(".local")
        .to_string()
}

pub fn config_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home().join("Library/Application Support/inputswitch")
    }
    #[cfg(windows)]
    {
        std::env::var("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home().join("AppData/Roaming"))
            .join("inputswitch")
    }
    #[cfg(target_os = "linux")]
    {
        std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home().join(".config"))
            .join("inputswitch")
    }
}

pub(super) fn home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}
