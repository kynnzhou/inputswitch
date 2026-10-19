//! 托盘程序入口（macOS 的 .app / Windows 的 exe 都指向这个）。
//!
//! 和命令行版分开编，原因是 Windows 上托盘程序必须用 windows 子系统，
//! 不然双击会弹一个黑色控制台窗口；而命令行诊断又必须保留控制台。
//! 一个二进制没法同时满足，所以拆成两个。
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(feature = "tray")]
fn main() {
    inputswitch::tray::run()
}

#[cfg(not(feature = "tray"))]
fn main() {
    eprintln!("这份构建没有包含托盘功能（需要 --features tray）");
    std::process::exit(1)
}
