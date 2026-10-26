//! 切屏执行：SwitchRaw / SwitchOnly / SetTarget / ClearTarget。
//!
//! 四个都是"用户明确要一个结果"的命令：切屏（要不要动键鼠由 `do_switch`
//! 的 Keys 枚举决定，这里不碰结论）、设/清热键目标。共同收尾都是
//! `self.push`：把新状态推给 UI 线程（去重后）。

use crate::tray::switch::do_switch;

use super::Ctx;
use crate::tray::log_line;

impl Ctx {
    pub(super) fn on_switch_raw(&mut self, name: String, value: u16) {
        let text = do_switch(&mut self.switcher, &self.mesh, &name, value, true);
        log_line(&text);
        let target = self.switcher.config.hotkey_target.clone();
        self.push(&target, text);
    }

    pub(super) fn on_switch_only(&mut self, name: String, value: u16) {
        let text = do_switch(&mut self.switcher, &self.mesh, &name, value, false);
        log_line(&text);
        let target = self.switcher.config.hotkey_target.clone();
        self.push(&target, text);
    }

    pub(super) fn on_set_target(&mut self, name: String, value: u16) {
        // 只设热键目标，不往 profile 里写没验证过的值：
        // 否则"未确认"标记会被标准值污染，下次就看不出哪个口是真的试出来的
        let _ = value;
        self.switcher.config.hotkey_target = name.clone();
        let _ = self.switcher.config.save();
        log_line(&format!("热键目标设为 {}", name));
        let text = format!("热键目标已设为 {}", self.switcher.config.label_for(&name));
        self.push(&name, text);
    }

    pub(super) fn on_clear_target(&mut self) {
        self.switcher.config.hotkey_target.clear();
        let _ = self.switcher.config.save();
        self.push("", "已取消热键目标".to_string());
    }
}
