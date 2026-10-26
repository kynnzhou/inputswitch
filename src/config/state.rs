//! 运行状态：上一次把屏幕给了哪个输入。
//!
//! 读不到 VCP 时（屏幕不在本机）靠它推断当前状态。跟 config.json 分开存：
//! 它每次切换都写，是热数据；配置用户改一次才写一次，是冷数据。

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::paths::config_dir;

/// 记录"上一次把屏幕给了哪个输入"，用于读不到 VCP 时推断当前状态。
#[derive(Default, Debug, Serialize, Deserialize)]
pub struct RuntimeState {
    pub active_input: HashMap<String, String>,
}

impl RuntimeState {
    pub fn path() -> PathBuf {
        config_dir().join("state.json")
    }

    pub fn load() -> Self {
        match fs::read_to_string(Self::path()) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn get(&self, model_key: &str) -> Option<String> {
        self.active_input.get(model_key).cloned()
    }

    pub fn set(&mut self, model_key: &str, input_name: &str) {
        self.active_input
            .insert(model_key.to_string(), input_name.to_string());
        if let Some(parent) = Self::path().parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = fs::write(Self::path(), text);
        }
    }
}
