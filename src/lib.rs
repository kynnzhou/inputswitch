//! 跨平台显示器输入源切换。
//!
//! 通过 VESA DDC/CI 的 VCP 0x60（Input Source）指令切换显示器输入源，
//! 不需要任何硬件改造。

/// 构建版本号。**唯一真源是 `Cargo.toml` 的 `version`**，别处一律引用这里。
///
/// 存在的理由：以前启动日志里手写着 `构建=v0.6.89`，改版本要同时改两处，
/// 漏一处就会出现「面板说 0.6.90、日志说 0.6.89」这种没法排查的错位。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod autostart;
pub mod caps;
pub mod clipboard;
pub mod config;
pub mod ddc;
pub mod edid;
pub mod hidden;
/// 共用键鼠（软件 KVM）。只有 macOS / Windows 有抓取后端。
#[cfg(any(target_os = "macos", windows))]
pub mod kvm;
pub mod models;
pub mod pair;
pub mod panel;
pub mod peer;
pub mod support;
pub mod transfer;
pub mod switcher;
pub mod update;

#[cfg(feature = "tray")]
pub mod panelwin;
#[cfg(feature = "tray")]
pub mod tray;

pub use models::{Capability, Display};
pub use switcher::{ProbeSource, PortProbe, ScanReport, SwitchOutcome, Switcher};
pub use support::{Check, Support};
