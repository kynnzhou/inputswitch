//! Linux DDC/CI 后端（通过 ddcutil 子进程）。
//!
//! ddcutil 是 Linux 上最可靠的 DDC 实现，直接操作 /dev/i2c-*。
//! 为了避免重复实现 I2C 协议和权限处理，这里走子进程调用：
//!
//! ```text
//! sudo apt install ddcutil
//! sudo usermod -aG i2c $USER   # 免 sudo 访问
//! ```

use std::process::Command;

use super::Backend;
use crate::models::{Capability, Display};

fn run(args: &[&str]) -> String {
    match Command::new("ddcutil").args(args).output() {
        Ok(out) => {
            let mut text = String::from_utf8_lossy(&out.stdout).to_string();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            text
        }
        Err(err) => format!("error: {err}"),
    }
}

fn find_value(text: &str, key: &str) -> Option<String> {
    text.lines()
        .find(|line| line.contains(key))
        .and_then(|line| line.split('=').nth(1))
        .map(|v| v.trim().to_string())
}

pub struct LinuxBackend;

impl LinuxBackend {
    pub fn new() -> Self {
        Self
    }

    fn index_of(&self, display: &Display) -> String {
        // uid 形如 linux:i2c-3，取最后一段
        display
            .uid
            .rsplit('-')
            .next()
            .unwrap_or("1")
            .to_string()
    }
}

impl Backend for LinuxBackend {
    fn platform(&self) -> &'static str {
        "linux"
    }

    fn list_displays(&self) -> Vec<Display> {
        let text = run(&["detect"]);
        let mut out: Vec<Display> = Vec::new();
        let mut index: Option<String> = None;
        let mut bus: Option<String> = None;
        let mut name: Option<String> = None;

        let mut flush = |index: &Option<String>, bus: &Option<String>, name: &Option<String>| {
            if let Some(idx) = index {
                let bus_id = bus.clone().unwrap_or_else(|| "x".into());
                out.push(Display {
                    uid: format!("linux:i2c-{bus_id}"),
                    name: name
                        .clone()
                        .unwrap_or_else(|| format!("I2C-{bus_id}")),
                    platform: "linux",
                    is_main: out.is_empty(),
                    handle: idx.parse().unwrap_or(0),
                });
            }
        };

        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("Display ") && trimmed.ends_with(|c: char| c.is_ascii_digit()) {
                flush(&index, &bus, &name);
                index = trimmed.split_whitespace().last().map(|s| s.to_string());
                bus = None;
                name = None;
            } else if let Some(rest) = trimmed.strip_prefix("I2C bus:") {
                bus = rest.trim().rsplit('/').next().map(|s| s.replace("i2c-", ""));
            } else if let Some(rest) = trimmed.strip_prefix("Monitor:") {
                name = Some(rest.trim().to_string());
            }
        }
        flush(&index, &bus, &name);
        drop(flush);
        out
    }

    fn get_vcp(&self, display: &Display, code: u8) -> Option<u16> {
        let index = self.index_of(display);
        let text = run(&["--display", &index, "getvcp", &format!("0x{code:02x}")]);
        find_value(&text, "current value").and_then(|v| v.parse().ok())
    }

    fn set_vcp(&self, display: &Display, code: u8, value: u16) -> bool {
        let index = self.index_of(display);
        let text = run(&[
            "--display",
            &index,
            "setvcp",
            &format!("0x{code:02x}"),
            &value.to_string(),
        ]);
        let lower = text.to_lowercase();
        !lower.contains("failed") && !lower.contains("error")
    }

    fn transport_ok(&self, display: &Display) -> bool {
        self.get_vcp(display, crate::models::VCP_BRIGHTNESS).is_some()
    }
}

impl Default for LinuxBackend {
    fn default() -> Self {
        Self::new()
    }
}

pub fn quick_capability(backend: &LinuxBackend, display: &Display) -> Capability {
    if backend.get_vcp(display, crate::models::VCP_BRIGHTNESS).is_some() {
        Capability::ReadWrite
    } else {
        Capability::Unsupported
    }
}
