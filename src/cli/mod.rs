//! 子命令实现层。`main.rs` 只留接口（clap 定义 + 分发表），20 个命令的实现在这里。
//!
//! 按「跑下去会不会真的改动屏幕」切，不按层切：诊断 CLI 的用法是排查时
//! 随手跑，`probe`/`info`/`check` 只读可以放心跑，`switch`/`wizard`/`scan`
//! 会把输入源切走、让屏幕黑几秒，混在一个文件里滚动时容易看漏。分工：
//!   display   只读诊断：probe / selftest / raw / info / menu / check
//!   act       会改动屏幕：switch / wizard / rename / scan / write
//!   net       局域网与配对：status / pair / send
//!   kvm       共用键鼠：kvm / kvm-probe / inject-probe
//!   service   常驻与服务项：daemon / autostart / clip
//! 子模块声明成 pub：`mod cli` 本身是 main.rs 私有的，pub 出不了这个 bin
//! crate，只是为了 main.rs 的分发表能写 `display::cmd_probe()` 这样自带
//! 文件名的路径。

pub mod act;
pub mod display;
pub mod kvm;
pub mod net;
pub mod service;

/// DDC 排查建议。selftest 和 wizard 失败时都要打印，放父模块当共享 helper，
/// 省得 display 和 act 两个兄弟模块互相 import。
pub(super) fn print_advice() {
    println!("\n按顺序排查（命中率从高到低）：");
    println!("  1. 显示器 OSD 里把 DDC/CI 打开 —— 很多机器出厂是关的");
    println!("  2. 不要经过扩展坞/转接头，用线直连，DDC 在多数 hub 上会被吃掉");
    println!("  3. 有的显示器只在 DP/HDMI 口转发 DDC，USB-C 口不转发");
    println!("  4. 虚拟机/远程桌面里拿不到 DDC");
}
