//! 配置与运行状态持久化。
//!
//! 关键设计：profile 按**显示器型号**索引，不按主机。
//! VCP 0x60 的取值由显示器固件决定，Mac 和 Windows 问同一台显示器得到的是
//! 同一套值，所以在一台机器上试出来的映射另一台可以直接复用。
//!
//! ---- 切分轴（2026-10-25，原 1117 行一个文件）----
//! 按「改一件事要动几个文件」切，不按层切：
//!   schema   配置长什么样：结构体、默认值、Default（纯数据）
//!   paths    配置住在哪：配置目录、默认节点名（三平台 cfg 分支都在这）
//!   kvm      配置里的共用键鼠：`Kvm` 的链路表行为 + 它的测试
//!   store    配置怎么落盘：load / save / migrate / monitors 增删查 + 幽灵口与改名测试
//!   state    运行状态：上次把屏幕给了哪个口（state.json，与 config.json 分开存）
//! 子模块是私有 `mod`，公开面一律 `pub use` 收口在本文件 —— 拆分前后
//! `crate::config::Config` 这些路径一个都没变。

mod kvm;
mod paths;
mod schema;
mod state;
mod store;

pub use paths::{config_dir, default_node};
pub use schema::{
    default_hotkey, Clipboard, Config, Kvm, LinkConfig, Network, Pair, Transfer, CONFIG_VERSION,
};
pub use state::RuntimeState;
pub use store::now_secs;
