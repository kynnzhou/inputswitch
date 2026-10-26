//! 键鼠事件的定义与线格式。
//!
//! 两种模式共用这一套事件：
//!
//! - **跟随画面（软件 KVM）**：一套物理键鼠，画面切到哪台键鼠跟到哪台。
//!   触发是切屏动作本身，不需要"边缘"。
//! - **边缘跨界（多屏共享）**：两台各有屏幕各有键鼠，光标推到屏幕边缘就越界。
//!   由 `Side` + `hit_edge` 判定。
//!
//! 设计参照 lan-mouse（GPL-3.0）与 Deskflow（GPL-2.0）的公开架构说明，
//! 代码是自己写的：鼠标走**相对位移**而不是绝对坐标 —— 两台机器分辨率、
//! 缩放都不一样，绝对坐标要维护一套屏幕布局；相对位移由接收端累加并
//! 夹在自己的屏幕范围内，天然跨分辨率且不会漂。
//!
//! 线格式刻意做得很紧（一个事件一个 UDP 包，不合并、不确认）：
//!
//! ```text
//! [0xA7][kind][seq_lo][seq_hi][payload...]
//! ```
//!
//! 丢包就丢一个鼠标移动，局域网里几乎不会发生；用 TCP 反而会把
//! 鼠标粘住（Nagle + 重传）。lan-mouse 也是这么取舍的。
//!
//! 键码统一用 **macOS virtual keycode**：Mac 端收发都是原生值，
//! 只有 Windows 端需要维护一张映射表（见 kvm/win.rs）。
//!
//! ---- 切分轴（2026-10-26，原 832 行一个文件）----
//! 按「改一件事要动几个文件」切，不按层切：
//!   codec   事件枚举与编解码：Event + encode/decode + 明包测试
//!   edge    Side 与边界几何：撞边判定、角落死区、撤防圈 + 几何测试
//!   seal    加密：Sealer / Opener（ChaCha20-Poly1305）+ 加解密测试
//! 协议常量 MAGIC / PEER_UNKNOWN 留在本文件：编解码与加密两层都用它。
//! 子模块是私有 `mod`，公开面一律 `pub use` 收口在本文件 —— 拆分前后
//! `crate::kvm::event::Event` 这些路径一个都没变。

mod codec;
mod edge;
mod seal;

pub use codec::Event;
pub use edge::{hit_edge, hit_edge_in, within_edge, Side, REARM_DIST};
pub use seal::{Opener, Sealer};

/// 包首字节。用来快速丢弃不是本协议的 UDP 包（会和发现层共用同一张网卡）。
pub const MAGIC: u8 = 0xA7;

/// Pong 里对端状态"未知"的标记。旧版本对端的 Pong 没有状态字节，
/// 解码时填这个而不是 Idle —— 状态自愈看守看到"未知"必须跳过判定
/// （没法判断就别动手），否则会把混版本当成状态错位，每 2.5 秒
/// 误杀一次会话（v0.6.11 首日实测踩过）。
pub const PEER_UNKNOWN: u8 = 0xFF;
