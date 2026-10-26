//! 这台显示器是谁、能干什么：身份键、EDID 与能力表缓存、挑哪台操作。
//!
//! 两块缓存（edid / caps）和刷新入口（refresh / rescan）都收在本文件：
//! 身份查询在菜单刷新里会被反复调用，能力表读一次要 5~8 次 I2C，不缓存
//! 面板会卡；而"怎么识别一台显示器"的改动也只该动这一个文件。

use crate::caps::Caps;
use crate::ddc::Backend;
use crate::edid::{self, Edid};
use crate::models::{normalize_model, Capability, Display, VCP_BRIGHTNESS, VCP_INPUT_SOURCE};

use super::Switcher;

impl Switcher {
    /// 重新识别所有在线显示器：读 EDID 定身份、读能力表补输入源。
    ///
    /// 这两件事都只在"显示器还有信号"的时候能做，所以必须早点做。
    pub fn refresh(&mut self) {
        for display in self.backend.list_displays() {
            self.sync_profile(&display);
        }
    }

    /// 丢掉缓存重新探测一遍，返回一句给人看的结论。
    ///
    /// 面板上的「重新检测显示器」走这里。必须**先清缓存**：能力表读失败会被
    /// 记成 `None` 缓存下来，不清掉的话再点一百次也还是那份空列表 ——
    /// 用户看到的就是"点了没任何效果"。
    pub fn rescan(&mut self) -> String {
        self.edid_cache.borrow_mut().clear();
        self.caps_cache.borrow_mut().clear();

        let displays = self.backend.list_displays();
        if displays.is_empty() {
            // 这个分支很常见，而且不是故障：显示器此刻正显示另一台机器，
            // 本机的显卡那边就是"没接显示器"，DDC 通道自然也不存在。
            return "本机现在没有接通的显示器（画面正显示另一台机器）".to_string();
        }

        let mut notes: Vec<String> = Vec::new();
        let mut any_inputs = false;
        for display in &displays {
            let key = self.sync_profile(display);
            let count = self
                .config
                .profile_for(&key)
                .map(|p| p.inputs.len())
                .unwrap_or(0);
            let current = self.backend.get_vcp(display, VCP_INPUT_SOURCE);
            let caps = self.capabilities(display).is_some();

            let mut note = format!("{}：{} 个输入口", display.name, count);
            match current {
                Some(value) => note.push_str(&format!("，当前 0x{value:02X}")),
                None => note.push_str("，当前输入源读不到"),
            }
            if !caps {
                note.push_str("（显示器没回能力表）");
            }
            if count > 0 {
                any_inputs = true;
            }
            notes.push(note);
        }

        let mut text = notes.join("；");
        if !any_inputs {
            text.push('。');
            text.push_str(&crate::ddc::diagnose());
        }
        text
    }

    /// 把一台显示器同步进配置，返回它的身份键。
    pub fn sync_profile(&mut self, display: &Display) -> String {
        let key = self.identity(display);
        // 老版本用型号名当键，先合并过来，别让用户实测的结果丢掉
        self.config.migrate_legacy(&display.name, &key);
        if let Some(caps) = self.capabilities(display) {
            if caps.has_inputs() {
                self.config.seed_inputs(&key, &display.name, &caps.inputs);
            }
        }
        let capability = self.capability(display);
        self.config.set_capability(&key, &display.name, capability);
        key
    }

    /// 显示器身份键：优先 EDID，读不到才退回型号名。
    pub fn identity(&self, display: &Display) -> String {
        self.edid(display)
            .map(|e| e.identity())
            .unwrap_or_else(|| normalize_model(&display.name))
    }

    pub fn edid(&self, display: &Display) -> Option<Edid> {
        if let Some(cached) = self.edid_cache.borrow().get(&display.uid) {
            return cached.clone();
        }
        let parsed = self.backend.read_edid(display).and_then(|b| edid::parse(&b));
        self.edid_cache
            .borrow_mut()
            .insert(display.uid.clone(), parsed.clone());
        parsed
    }

    pub fn capabilities(&self, display: &Display) -> Option<Caps> {
        if let Some(cached) = self.caps_cache.borrow().get(&display.uid) {
            return cached.clone();
        }
        let caps = self.backend.capabilities(display);
        self.caps_cache
            .borrow_mut()
            .insert(display.uid.clone(), caps.clone());
        caps
    }

    /// 这台显示器当前支持的输入源取值。能力表没有时返回空。
    pub fn input_values(&self, display: &Display) -> Vec<u16> {
        self.capabilities(display)
            .map(|c| c.inputs)
            .unwrap_or_default()
    }

    pub fn backend(&self) -> &dyn Backend {
        self.backend.as_ref()
    }

    pub fn displays(&self) -> Vec<Display> {
        self.backend.list_displays()
    }

    pub fn pick_display(&self, model: Option<&str>) -> Option<Display> {
        let all = self.displays();
        let filtered: Vec<Display> = match model {
            Some(m) => {
                let key = normalize_model(m);
                all.into_iter()
                    .filter(|d| normalize_model(&d.name) == key)
                    .collect()
            }
            None => all,
        };
        filtered
            .iter()
            .find(|d| !d.is_main)
            .or_else(|| filtered.first())
            .cloned()
    }

    pub fn capability(&self, display: &Display) -> Capability {
        if self.backend.get_vcp(display, VCP_BRIGHTNESS).is_some() {
            return Capability::ReadWrite;
        }
        if self.backend.transport_ok(display) {
            Capability::WriteOnly
        } else {
            Capability::Unsupported
        }
    }

    pub fn current_input_name(&self, display: &Display) -> Option<String> {
        let value = self.backend.get_vcp(display, VCP_INPUT_SOURCE)?;
        let key = self.identity(display);
        self.config
            .input_name_of(&key, value)
            .or_else(|| Some(format!("输入源 0x{:02X}", value)))
    }
}
