//! 工作线程：切屏 + 状态推送 + 键鼠跟随决策。
//!
//! 独占一个 Switcher（DDC 不能并发），串行处理 Command。UI 线程只发命令、
//! 收 UserEvent，两边不共享任何可变状态。
//!
//! `do_switch` 的键鼠结论是整个文件里最容易说谎的地方（七个分支，说错一句
//! 用户就要去按显示器按钮），所以它单独一个函数、单独一个 Keys 枚举，
//! 不让任何调用方自己拼文案。
//!
//! ---- 切分轴（2026-10-26，原 619 行一个文件）----
//! `engine_loop` 的 4 个参数加 4 个跨分支局部状态收进 `Ctx` 结构体，11 个
//! `Command` 分支按性质提成 `on_*` 方法分驻四文件，`run()` 只留收命令 →
//! 分发骨架（分支中间的 `continue` 搬进方法后变成 `return`）：
//!   status   4 个 pub(super) 查询助手（身份键 / 状态文案 / 占口同步 / 体检），
//!            原样搬运 —— `crate::tray::switch` 也要用 sync_own，方法化反而
//!            要把 Ctx 暴露过去
//!   switch   切屏执行：SwitchRaw / SwitchOnly / SetTarget / ClearTarget
//!   probe    探测：Rescan / CheckSupport / ScanPorts
//!   clip     剪贴板按需传输：PasteIntent / RemoteClipboard
//!   beat     心跳与对端协作：Heartbeat / AskPeerSwitch
//! `push` 也从嵌套函数改成 `Ctx` 方法：原 6 个参数里 proxy / switcher /
//! support / last 四个都来自 `self`，方法化后只留 `(target, text)`。

use std::sync::atomic::Ordering as AtomicOrdering;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use crate::peer::Mesh;
use crate::Switcher;
use tao::event_loop::EventLoopProxy;

use self::status::{inspect, profile_key_of, status_text};
use super::kvm_now;
use super::{Command, UserEvent, HAS_DISPLAY};

mod beat;
mod clip;
mod probe;
mod status;
mod switch;

/// 工作线程的全部可变状态：`engine_loop` 的 4 个参数一个字段一个，原函数体里
/// 跨分支存活的 4 个局部状态（last_push / last_topology / pending_detect）与
/// 只算一次的体检结论 support 也是字段 —— 分支处理方法按 `&mut self` 拿它们。
/// 字段私有即可：行为文件是本模块的后代，看得见。
struct Ctx {
    switcher: Switcher,
    rx: Receiver<Command>,
    proxy: EventLoopProxy<UserEvent>,
    mesh: Mesh,
    /// 上一轮推送的签名，只在状态真的变了才往 UI 线程丢事件
    last_push: String,
    /// 上一轮见到的显示器名集合，心跳里用来发现拓扑变化（零 I2C 开销）
    last_topology: Vec<String>,
    /// 拓扑刚变过 → 等 DDC 静默期过去再自动检测（见 Heartbeat）
    pending_detect: Option<Instant>,
    /// 体检结论第二行。写探测要 1 秒，全程只算一次
    support: String,
}

impl Ctx {
    /// 只在状态真的变了才往 UI 线程丢事件。
    ///
    /// 5 秒一次的自动刷新绝大多数时候结论完全一样，无条件重发会让 UI 层
    /// 每 5 秒重建一次菜单，macOS 上就表现为"菜单点开几秒后自己消失"。
    fn push(&mut self, target: &str, text: String) {
        let key = profile_key_of(&self.switcher);
        // 顺便刷新"本机有没有显示器"：面板要靠它区分"没接显示器"和"显示器不报口"
        let has_display = !self.switcher.displays().is_empty();
        let had_display = HAS_DISPLAY.swap(has_display, AtomicOrdering::SeqCst);
        // 画面刚切回本机（用户按了显示器面板按钮、或对面替我们切回来了）：
        // 键鼠必须立刻回到本机，否则就变成"看着本机的画面、操作着对面"。
        if has_display && !had_display {
            if let Some(handle) = kvm_now() {
                handle.give_back();
            }
        }
        let support = self.support.clone();
        let sig = format!("{key}\u{1f}{target}\u{1f}{text}\u{1f}{support}");
        if self.last_push == sig {
            return;
        }
        self.last_push = sig;
        let _ = self.proxy.send_event(UserEvent::Status {
            text,
            target: target.to_string(),
            profile_key: key,
            support: self.support.to_string(),
        });
    }
}

/// 转发到 `status::sync_own`。
///
/// `crate::tray::switch` 的 `do_switch` 要用它，而 `status` 是 `engine` 的
/// 私有子模块、外面看不见；`pub(super)` 项又不能被 re-export 到更宽的范围
/// （E0364），所以在这层薄转发一次。逻辑仍在 `status.rs`，一行未改。
pub(super) fn sync_own(switcher: &mut Switcher, mesh: &Mesh) -> Option<u16> {
    status::sync_own(switcher, mesh)
}

/// 启动工作线程。参数清单 = 原 `engine_loop` 的签名，语义不变。
pub(super) fn engine_loop(
    switcher: Switcher,
    rx: Receiver<Command>,
    proxy: EventLoopProxy<UserEvent>,
    mesh: Mesh,
) {
    let mut ctx = Ctx {
        switcher,
        rx,
        proxy,
        mesh,
        last_push: String::new(),
        last_topology: Vec::new(),
        pending_detect: None,
        support: String::new(),
    };
    ctx.run()
}

impl Ctx {
    fn run(&mut self) {
        let start_target = self.switcher.config.hotkey_target.clone();
        let _ = sync_own(&mut self.switcher, &self.mesh);
        let text = status_text(&mut self.switcher);
        // 只算一次：写探测要 1 秒，不能跟着状态刷新反复跑
        self.support = inspect(&self.switcher).0;
        self.push(&start_target, text);

        while let Ok(command) = self.rx.recv() {
            match command {
                Command::SwitchRaw { name, value } => {
                    self.on_switch_raw(name, value);
                }
                Command::SwitchOnly { name, value } => {
                    self.on_switch_only(name, value);
                }
                Command::SetTarget { name, value } => {
                    self.on_set_target(name, value);
                }
                Command::ClearTarget => {
                    self.on_clear_target();
                }
                Command::Rescan => {
                    self.on_rescan();
                }
                Command::PasteIntent => {
                    self.on_paste_intent();
                }
                Command::Heartbeat => {
                    self.on_heartbeat();
                }
                Command::AskPeerSwitch { value } => {
                    self.on_ask_peer_switch(value);
                }
                Command::RemoteClipboard(text) => {
                    self.on_remote_clipboard(text);
                }
                Command::CheckSupport => {
                    self.on_check_support();
                }
                Command::ScanPorts => {
                    self.on_scan_ports();
                }
            }
        }
    }
}
