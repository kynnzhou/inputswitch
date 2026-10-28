//! 控制面板转发过来的动作。
//!
//! 面板里凡是"只能主线程做"的事（开系统文件选择器、开浏览器、改配置、
//! 重启进程）都走 `UserEvent::Panel` 回到这里。纯粹的切屏/重扫不需要
//! 主线程，在 `run()` 的 dispatcher 里就转成 `Command` 发出去了。
//!
//! 这一层的方法有一个共同点：**都会改 `self.status`**。所以每个分支末尾
//! 都有 `log_line(&self.status)` —— 菜单第一行和面板顶部显示的是同一句。
//!
//! ---- 切分轴（2026-10-26，原 556 行一个文件）----
//! `on_panel` 只剩分发骨架，21 个 `Action` 分支按"改一件事要动几个文件"
//! 提成 `on_*` 方法分驻四文件：
//!   pair    配对码：Pair / GenerateCode（换钥匙必须重启 KVM 实例）
//!   kvm     共用键鼠总控：Power / Mode / Role / Scale / AutoResume /
//!           KeepAlive / TakeBack / HandOver
//!   link    链路摆位：KvmLink（拖格子改链路，原文件里最大的一坨）
//!   system  系统开关与杂项：自启 / 剪贴板 / 接收目录 / 发文件 / 副机模式 /
//!           别名 / 录制窗 / 重启 / 开目录 —— 共同点是都不碰 KVM 链路
//! `_ => {}` 兜底的是 Switch / Rescan 那几个"不用回主线程"的变体 ——
//! 它们在 tray/mod.rs 的 dispatcher 里就转成 Command 发出去了，走不到这。

use crate::panel::Action;

use super::Ui;

mod kvm;
mod link;
mod pair;
mod system;

impl Ui {
    /// 面板动作分发。`Action` 的定义在 `crate::panel`，加一个动作要同时
    /// 看那边和这里。
    pub(super) fn on_panel(&mut self, action: Action) {
        match action {
            Action::Pair(code) => self.on_pair(code),
            Action::GenerateCode => self.on_generate_code(),
            Action::ToggleAutostart(on) => self.on_toggle_autostart(on),
            Action::ToggleClipboard(on) => self.on_toggle_clipboard(on),
            Action::ChooseInbox => self.on_choose_inbox(),
            Action::OpenInbox => self.on_open_inbox(),
            Action::SendFiles(node) => self.on_send_files(node),
            Action::SendFolder(node) => self.on_send_folder(node),
            Action::SendTest(node) => self.on_send_test(node),
            Action::TransferPause(id) => self.on_transfer_pause(id),
            Action::TransferResume(id) => self.on_transfer_resume(id),
            Action::TransferCancel(id) => self.on_transfer_cancel(id),
            Action::TransferRetry(id) => self.on_transfer_retry(id),
            Action::KvmPower(on) => self.on_kvm_power(on),
            Action::KvmMode(mode) => self.on_kvm_mode(mode),
            Action::KvmRole(role) => self.on_kvm_role(role),
            Action::KvmScale(v) => self.on_kvm_scale(v),
            Action::KvmAutoResume(on) => self.on_kvm_auto_resume(on),
            Action::KvmKeepAlive(on) => self.on_kvm_keep_alive(on),
            Action::Submachine(on) => self.on_submachine(on),
            Action::SetLabel { name, label } => self.on_set_label(name, label),
            Action::KvmHandOver(node) => self.on_kvm_hand_over(node),
            Action::KvmLink { node, side, mode } => self.on_kvm_link(node, side, mode),
            Action::RestartApp => self.on_restart_app(),
            Action::KvmTakeBack => self.on_kvm_take_back(),
            Action::RecordHotkey => self.on_record_hotkey(),
            Action::ResetHotkey => self.on_reset_hotkey(),
            Action::OpenConfigDir => self.on_open_config_dir(),
            _ => {}
        }
    }
}
