//! 系统开关与杂项：自启 / 副机模式 / 剪贴板 / 接收目录 / 发文件 /
//! 别名 / 录制窗 / 重启 / 开配置目录。
//!
//! 共同点是都不碰 KVM 链路，改一件事只动一个方法。副机模式（Submachine）
//! 是这堆开关的组合拳：自启 + 自动恢复 + 守护 + 角色 remote + 共用键鼠，
//! 少一条它就是个哑终端。

use tao::keyboard::ModifiersState;

use crate::tray::log_line;
use crate::tray::send::{pick_folder, retry_send, start_send, start_send_folder, start_send_test};
use crate::tray::sys::open_path;
use crate::tray::Ui;
use crate::transfer::tasks;

impl Ui {
    pub(super) fn on_toggle_autostart(&mut self, on: bool) {
        let result = if on {
            // 守护开关跟着一起写进 LaunchAgent / 计划任务：
            // 两件事在系统层是同一份配置，分开写必然不同步。
            crate::autostart::enable(self.config.kvm.keep_alive)
        } else {
            crate::autostart::disable()
        };
        self.status = match result {
            Ok(()) => {
                if on {
                    "已开启开机自启".to_string()
                } else {
                    "已关闭开机自启".to_string()
                }
            }
            Err(err) => format!("开机自启设置失败：{err}"),
        };
    }

    pub(super) fn on_toggle_clipboard(&mut self, on: bool) {
        self.config.clipboard.enabled = on;
        let _ = self.config.save();
        if on {
            let _ = self.watcher.poll();
        }
        self.status = if on {
            "剪贴板同步已开启".to_string()
        } else {
            "剪贴板同步已关闭".to_string()
        };
        log_line(&self.status);
    }

    pub(super) fn on_choose_inbox(&mut self) {
        if let Some(dir) = pick_folder() {
            self.config.transfer.inbox = dir.display().to_string();
            let _ = self.config.save();
            *self.inbox_shared.lock().unwrap() = dir.clone();
            self.status = format!("接收目录已改为 {}", dir.display());
            log_line(&self.status);
        }
    }

    pub(super) fn on_open_inbox(&mut self) {
        let dir = self.config.transfer.inbox_dir();
        let _ = std::fs::create_dir_all(&dir);
        open_path(&dir);
    }

    pub(super) fn on_send_files(&mut self, node: Option<String>) {
        start_send(
            &self.config,
            &self.mesh,
            &self.proxy,
            &mut self.status,
            node.as_deref(),
        );
    }

    pub(super) fn on_send_folder(&mut self, node: Option<String>) {
        start_send_folder(
            &self.config,
            &self.mesh,
            &self.proxy,
            &mut self.status,
            node.as_deref(),
        );
    }

    pub(super) fn on_send_test(&mut self, node: Option<String>) {
        start_send_test(
            &self.config,
            &self.mesh,
            &self.proxy,
            &mut self.status,
            node.as_deref(),
        );
    }

    /// 传输列表四件套。合法性都由注册表答（任务在不在、什么阶段），
    /// 这里只把结论翻译成状态行 —— 和面板上"点了有反应"是同一份文案。
    pub(super) fn on_transfer_pause(&mut self, id: u64) {
        self.status = if tasks::set_paused(id, true) {
            format!("已暂停传输 #{id}")
        } else {
            "这条传输现在不能暂停".to_string()
        };
        log_line(&self.status);
    }

    pub(super) fn on_transfer_resume(&mut self, id: u64) {
        self.status = if tasks::set_paused(id, false) {
            format!("已继续传输 #{id}")
        } else {
            "这条传输现在不能继续".to_string()
        };
        log_line(&self.status);
    }

    pub(super) fn on_transfer_cancel(&mut self, id: u64) {
        // 只置标志：阶段等传输线程落。它每个块之间都看，最多一个块的延迟
        self.status = if tasks::cancel(id) {
            format!("已取消传输 #{id}")
        } else {
            "这条传输现在不能取消".to_string()
        };
        log_line(&self.status);
    }

    pub(super) fn on_transfer_retry(&mut self, id: u64) {
        retry_send(
            &self.config,
            &self.mesh,
            &self.proxy,
            &mut self.status,
            id,
        );
        log_line(&self.status);
    }

    pub(super) fn on_submachine(&mut self, on: bool) {
        // 一键把"没有键鼠那台"该有的配置全打开：自启 + 自动恢复 +
        // 守护 + 角色 remote + 共用键鼠开着。少一条它就是个哑终端。
        self.config.kvm.auto_resume = on;
        self.config.kvm.keep_alive = on;
        if on {
            self.config.kvm.role = "remote".to_string();
            self.config.kvm.enabled = true;
        }
        let _ = self.config.save();
        let auto = if on {
            crate::autostart::enable(true)
        } else {
            crate::autostart::disable()
        };
        self.status = match auto {
            Ok(()) if on => "副机模式已开启：开机自启 + 自动恢复 + 崩溃重启。\
                 记得给这台配固定 IP —— 共用键鼠按 IP 认机器，不认机器名"
                .to_string(),
            Ok(()) => "副机模式已关闭：不再自启、不再自动恢复、不再守护".to_string(),
            Err(err) => format!("副机模式设置失败：{err}"),
        };
        log_line(&self.status);
    }

    pub(super) fn on_set_label(&mut self, name: String, label: String) {
        // 空串 = 恢复默认名（删掉别名，让 label_for 按 MCCS 渲染）
        let text = label.trim().to_string();
        if text.is_empty() {
            self.config.labels.remove(&name);
            self.status = format!("已恢复默认名：{}", self.config.label_for(&name));
        } else {
            self.config.labels.insert(name.clone(), text.clone());
            self.status = format!("已把这个口改名为「{text}」");
        }
        let _ = self.config.save();
        log_line(&self.status);
    }

    pub(super) fn on_record_hotkey(&mut self) {
        // 面板上点「改键」→ 弹出这个常驻的原生小窗抢焦点收键盘事件。
        // 网页里收不到全局组合键，只能这么绕。
        if let Some(win) = self.record_window.as_ref() {
            self.recording = true;
            self.record_mods = ModifiersState::empty();
            win.set_visible(true);
            win.set_focus();
            self.status = "请按下新的组合键（Esc 取消）…".to_string();
        } else {
            self.status = "这台机器打不开录制窗口，请直接改配置里的 hotkey".to_string();
        }
        log_line(&self.status);
    }

    pub(super) fn on_restart_app(&mut self) {
        // 先落日志再退出：重启之后用户/我都要能对上"是这一步干的"
        log_line("收到重启请求（权限刚改过，旧进程里的 TCC 结论已过期）");
        crate::tray::sys::restart_app();
    }

    pub(super) fn on_open_config_dir(&mut self) {
        crate::tray::sys::open_config_dir();
    }
}
