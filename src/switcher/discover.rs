//! 向导：试出"另一个口"的 0x60 取值。
//!
//! 先把当前值读出来（这就是 own 的取值），再逐个写候选值，哪个让显示器从
//! 拓扑里消失，哪个就是 other 的取值。副作用提醒：一旦命中，本机就失去
//! DDC 通道了，恢复要靠对面主机或 OSD。

use std::time::Duration;

use crate::models::{CANDIDATE_INPUT_VALUES, VCP_INPUT_SOURCE};

use super::Switcher;

impl Switcher {
    /// 找出另一个输入口的 0x60 取值。
    ///
    /// 先把当前值读出来（这就是 own 的取值），再逐个写候选值，
    /// 哪个让显示器从拓扑里消失，哪个就是 other 的取值。
    ///
    /// 副作用提醒：一旦命中，本机就失去 DDC 通道了，恢复要靠对面主机或 OSD。
    pub fn discover(
        &mut self,
        own_input: &str,
        other_input: &str,
        model: Option<&str>,
        max_tries: usize,
        mut on_try: impl FnMut(u8),
    ) -> Result<u16, String> {
        let Some(display) = self.pick_display(model) else {
            return Err("没有找到可操作的显示器".into());
        };

        let own_value = self.backend.get_vcp(&display, VCP_INPUT_SOURCE);
        let key = self.identity(&display);
        if let Some(v) = own_value {
            self.config.set_input(&key, own_input, v, true);
        }

        let candidates: Vec<u16> = CANDIDATE_INPUT_VALUES
            .iter()
            .map(|v| *v as u16)
            .filter(|v| Some(*v) != own_value)
            .take(max_tries)
            .collect();

        for value in candidates {
            on_try(value as u8);
            if !self.backend.set_vcp(&display, VCP_INPUT_SOURCE, value) {
                continue;
            }
            if self.wait_until_gone(&display, Duration::from_secs(2)) {
                self.config
                    .set_input(&key, other_input, value, true);
                return Ok(value);
            }
        }
        Err(format!(
            "试完 {} 个候选值都没有命中",
            CANDIDATE_INPUT_VALUES.len().min(max_tries)
        ))
    }
}
