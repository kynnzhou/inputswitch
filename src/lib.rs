//! 跨平台显示器输入源切换。
//!
//! 通过 VESA DDC/CI 的 VCP 0x60（Input Source）指令切换显示器输入源，
//! 不需要任何硬件改造。

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

#[cfg(feature = "tray")]
pub mod panelwin;
#[cfg(feature = "tray")]
pub mod tray;

pub use models::{Capability, Display};
pub use switcher::{ProbeSource, PortProbe, ScanReport, SwitchOutcome, Switcher};
pub use support::{Check, Support};
