//! 配置怎么落盘：load / save / migrate / monitors 的增删查。
//!
//! 两个不变量锁在本文件，都有实测回归背景：
//!   clear_unverified_occupancy 每次加载都跑，不只在版本迁移里 ——
//!     扫描会把对端自报的「有主机」写回来，一次性迁移拦不住它；
//!   migrate 是链式的，从 v1 直接升 v3 时两段都要跑。

use std::fs;

use crate::models::{normalize_model, Capability, DisplayProfile, Presence};

use super::paths::default_node;
use super::schema::{Config, CONFIG_VERSION};

impl Config {
    pub fn load() -> Self {
        let path = Self::path();
        let mut cfg = match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Config {
                node: default_node(),
                ..Default::default()
            },
        };
        cfg.migrate();
        // 每次加载都守一道，不只在版本迁移里（见 clear_unverified_occupancy 的注释）：
        // 扫描/重新检测会把"对端自报占用"再写回来，一次性的迁移拦不住它。
        let renamed = cfg.rename_anonymous_inputs();
        let ghost = cfg.clear_unverified_occupancy();
        if renamed > 0 {
            #[cfg(feature = "tray")]
            crate::tray::log_line(&format!(
                "已把 {renamed} 个匿名输入口按 MCCS 标准改名（0x12 → HDMI-2 这种）"
            ));
            #[cfg(not(feature = "tray"))]
            eprintln!("已把 {renamed} 个匿名输入口按 MCCS 标准改名");
        }
        if ghost > 0 {
            #[cfg(feature = "tray")]
            crate::tray::log_line(&format!(
                "已清掉 {ghost} 条没有本机验证的「有主机」记录\
                 （对端自报的口编号不能当本机的输入源）"
            ));
            #[cfg(not(feature = "tray"))]
            eprintln!("已清掉 {ghost} 条没有本机验证的「有主机」记录");
        }
        // **改名也要落盘**：以前 save 只写在 ghost 分支里，改名只改了内存，
        // 下次加载又从头改一遍（日志里能看见同一条改名消息刷两次）。
        if renamed > 0 || ghost > 0 {
            let _ = cfg.save();
        }
        cfg
    }

    /// 老配置的一次性清理。**链式**：每一段只看自己那条版本线，
    /// 从 v1 直接升到 v3 时两段都要跑。
    ///
    /// 早先是"版本不到位就整段跑一遍然后直接把 version 写成最新"，
    /// 那样把版本提到 3 会**跳过** v2 那段清理 —— 幽灵输入口就留在表里了。
    ///
    /// v1→v2：旧版会把对端广播的「我挂在 0x12」写进本机显示器的口表，
    /// 但每台显示器的口编号是各自固件定义的，跨机器搬过来就是错的 ——
    /// 表现为一个绿点幽灵口，点「切换」直接黑屏。
    ///
    /// 识别依据：`presence=Occupied` 且 `verified=false`。真正切过去过的口
    /// 一定同时被标成已验证（`record_success` 两个字段一起写），所以这条
    /// 判据只命中"从来没在本机验证过、却声称有主机"的条目。
    /// 代价：只跑过扫描、从没手动切过去的口也会回到"未检测"，重扫一次即可。
    ///
    /// v2→v3：`kvm` 从"一条链路"改成链路表。老字段升成 links[0]。
    /// 清掉"对端自报占用"留下的假证据，返回清掉几条。
    ///
    /// 判据：`presence == Occupied && verified == false`。
    ///
    /// 为什么这条判据是对的：真正切过去过、画面留住了的口，`record_success`
    /// 会把 `verified` 和 `presence` 一起写（见 `mark_verified`）。所以
    /// "声称有主机却从没验证过"只能是**别人替它说的** —— 最常见的就是对端
    /// 广播"我挂在 0x12"，扫描直接把 0x12 标成 Occupied。可每台显示器的口
    /// 编号是各自固件定义的，跨机器搬过来就是错的：面板上会多出一条
    /// `输入源 0x12`（连名字都没有），点「切换」直接黑屏。
    ///
    /// **不能只在版本迁移里跑一次**：v0.6.71 之前它是 v1→v2 的一次性清理，
    /// 配置升到 v3 之后就不再执行，于是扫描再写一次、这条幽灵口就复活了
    /// （2026-10-22 用户实测：重新检测后第四行又冒出 `输入源 0x12`）。
    /// 现在由 `load()` 每次加载都守一道。
    ///
    /// 代价：只跑过扫描、从没手动切过去的口会回到"未检测"，重扫一次即可。
    fn clear_unverified_occupancy(&mut self) -> usize {
        let mut cleared = 0usize;
        for profile in self.monitors.values_mut() {
            for src in profile.inputs.iter_mut() {
                if src.presence == Presence::Occupied && !src.verified {
                    src.presence = Presence::Unknown;
                    src.checked_at = 0;
                    cleared += 1;
                }
            }
        }
        cleared
    }

    /// 把 `输入源 0x12` 这种匿名口按 MCCS 标准改名（0x12 → hdmi2）。
    ///
    /// 只改"名字是程序自动起的匿名名"的条目，用户自己命名过的一律不动；
    /// 顺手把 `labels` 里挂在旧名上的自定义标签搬到新名上，否则用户设的
    /// 显示名会突然失效。
    pub fn rename_anonymous_inputs(&mut self) -> usize {
        let mut renames: Vec<(String, String)> = Vec::new();
        for profile in self.monitors.values_mut() {
            for src in profile.inputs.iter_mut() {
                if !src.name.starts_with("输入源 0x") {
                    continue;
                }
                let Some(short) = crate::models::mccs_input_name(src.value) else {
                    continue; // 标准里没有的取值：留着 0x..，比乱猜强
                };
                renames.push((src.name.clone(), short.to_string()));
                src.name = short.to_string();
            }
        }
        for (old, new) in &renames {
            if let Some(label) = self.labels.remove(old) {
                self.labels.entry(new.clone()).or_insert(label);
            }
        }
        renames.len()
    }

    fn migrate(&mut self) {
        if self.version >= CONFIG_VERSION {
            return;
        }
        let mut dirty = false;
        if self.version < 2 {
            let cleared = self.clear_unverified_occupancy();
            if cleared > 0 {
                // 注意：这个 crate 也会以 `--no-default-features` 编译成纯 CLI
                // 诊断程序，那时 `tray` 模块整个被 cfg 掉，直接引用会 E0433。
                // 托盘版写日志文件，CLI 版打到 stderr。
                #[cfg(feature = "tray")]
                crate::tray::log_line(&format!(
                    "已清理 {cleared} 条来自对端的输入口状态：每台显示器的口编号互不相通，\
                     跨机器抄来的「有主机」是错的"
                ));
                #[cfg(not(feature = "tray"))]
                eprintln!("已清理 {cleared} 条来自对端的输入口状态（跨机器抄来的「有主机」是错的）");
            }
            dirty = true;
        }
        if self.version < 3 {
            // 老字段 → 链路表。已经手写过 links 的（P4 的手工验收配置）
            // 保持不动，只补镜像。
            self.kvm.normalize();
            dirty = true;
        }
        if dirty {
            self.version = CONFIG_VERSION;
            let _ = self.save();
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        // 同目录写临时文件再原子替换，避免半截文件
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &path)
    }

    /// 按 EDID 派生出的身份键取 profile。
    ///
    /// 键不再是显示器型号名：同一个型号在不同系统上会写成不同字符串
    /// （Windows 给 "Generic PnP Monitor"，macOS 给 "P275MV PLUS"），
    /// 拿名字当键会让配置在两台机器之间对不上。EDID 的厂商+产品码+序列号
    /// 才是显示器的真实身份，跨机器、跨重启都稳定。
    pub fn profile_for(&self, key: &str) -> Option<&DisplayProfile> {
        self.monitors.get(key)
    }

    pub fn ensure_profile(&mut self, key: &str, name: &str) -> &mut DisplayProfile {
        self.monitors
            .entry(key.to_string())
            .or_insert_with(|| DisplayProfile::new(key, name))
    }

    pub fn set_input(&mut self, key: &str, name: &str, value: u16, verified: bool) {
        let profile = self.ensure_profile(key, name);
        // 按 VCP 取值匹配：同一个输入口只有一个合法取值，名字可以变
        if let Some(src) = profile.inputs.iter_mut().find(|s| s.value == value) {
            src.name = name.to_string();
            src.verified = verified;
        } else {
            profile.inputs.push(crate::models::InputSource {
                name: name.to_string(),
                value,
                verified,
                presence: crate::models::Presence::Unknown,
                checked_at: 0,
            });
            profile.inputs.sort_by_key(|s| s.value);
        }
        let _ = self.save();
    }

    /// 给某个输入口改名（用户点菜单里的"命名"时走这里）。
    ///
    /// 顺手同步 hotkey_target：热键目标存的是名字，口一改名就对不上了，
    /// 会让热键突然"失灵"。
    pub fn rename_input(&mut self, key: &str, value: u16, new_name: &str) -> bool {
        let Some(profile) = self.monitors.get_mut(key) else {
            return false;
        };
        let Some(src) = profile.inputs.iter_mut().find(|s| s.value == value) else {
            return false;
        };
        let old_name = src.name.clone();
        src.name = new_name.to_string();
        if self.hotkey_target.eq_ignore_ascii_case(&old_name) {
            self.hotkey_target = new_name.to_string();
        }
        let _ = self.save();
        true
    }

    pub fn set_capability(&mut self, key: &str, name: &str, cap: Capability) {
        let profile = self.ensure_profile(key, name);
        if profile.capability != cap {
            profile.capability = cap;
            let _ = self.save();
        }
    }

    /// 按显示器自报的能力表铺满输入源列表。只补不覆盖：
    /// 用户已经命名或验证过的口保持原样。
    ///
    /// 这是"通用"的关键一步 —— 不再需要任何厂商硬编码，
    /// 显示器说自己支持哪几个口，菜单就列哪几个。
    pub fn seed_inputs(&mut self, key: &str, name: &str, values: &[u16]) -> usize {
        let profile = self.ensure_profile(key, name);
        let mut added = 0;
        for value in values {
            if profile.inputs.iter().any(|i| i.value == *value) {
                continue;
            }
            // 能力表只给数值时按 MCCS 标准起名（0x12 → hdmi2）；
            // 标准里没有的才退回 `输入源 0x..`。
            let name = crate::models::mccs_input_name(*value)
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("输入源 0x{:02X}", value));
            profile.inputs.push(crate::models::InputSource {
                name,
                value: *value,
                verified: false,
                presence: crate::models::Presence::Unknown,
                checked_at: 0,
            });
            added += 1;
        }
        if added > 0 {
            profile.inputs.sort_by_key(|s| s.value);
            let _ = self.save();
        }
        added
    }

    /// 老版本拿型号名当键，升级后把旧配置合并到 EDID 键上，
    /// 用户实测出来的映射不会丢。
    pub fn migrate_legacy(&mut self, display_name: &str, new_key: &str) -> bool {
        let legacy = normalize_model(display_name);
        if legacy == new_key {
            return false;
        }
        let Some(old) = self.monitors.remove(&legacy) else {
            return false;
        };
        let old_cap = old.capability;
        let old_inputs = old.inputs;
        let entry = self
            .monitors
            .entry(new_key.to_string())
            .or_insert_with(|| DisplayProfile::new(new_key, display_name.to_string()));
        for input in old_inputs {
            match entry.inputs.iter_mut().find(|i| i.value == input.value) {
                Some(existing) => {
                    // 旧记录来自实测，比自动生成的更可信
                    if !existing.verified && input.verified {
                        *existing = input;
                    }
                }
                None => entry.inputs.push(input),
            }
        }
        entry.inputs.sort_by_key(|s| s.value);
        if entry.capability == Capability::ReadWrite {
            entry.capability = old_cap;
        }
        let _ = self.save();
        true
    }

    pub fn label_for(&self, input_name: &str) -> String {
        self.labels
            .get(input_name)
            .cloned()
            // 没自定义过就按 MCCS 短名渲染成 HDMI-2 / DP-1 这种，
            // 而不是把 `hdmi2` 原样丢给用户看
            .unwrap_or_else(|| crate::models::pretty_input_name(input_name))
    }

    /// 记住"本机接在哪个口"。只在真的读到值时调用 ——
    /// 屏幕不在本机时读不到 DDC，这时候必须保留上一次的值。
    pub fn remember_own(&mut self, name: &str, value: u16) -> bool {
        if value == 0 || (self.own_value == value && self.own_input == name) {
            return false;
        }
        self.own_value = value;
        self.own_input = name.to_string();
        let _ = self.save();
        true
    }

    /// 反查：这个 VCP 取值对应哪个口名（限定在某台显示器内）。
    pub fn input_name_of(&self, key: &str, value: u16) -> Option<String> {
        self.monitors
            .get(key)?
            .inputs
            .iter()
            .find(|i| i.value == value)
            .map(|i| i.name.clone())
    }

    /// 把某个口标记成"已验证"：切换真的生效过，说明这个值是对的。
    pub fn mark_verified(&mut self, key: &str, value: u16) {
        if let Some(profile) = self.monitors.get_mut(key) {
            if let Some(src) = profile.inputs.iter_mut().find(|s| s.value == value) {
                if !src.verified {
                    src.verified = true;
                    let _ = self.save();
                }
            }
        }
    }

    /// 记录"这个口上有没有主机"。扫描一次就把整张表写下来。
    ///
    /// 批量写是为了只落盘一次：每个口存一次太费，而且扫描本身已经够慢。
    pub fn set_presence(&mut self, key: &str, items: &[(u16, Presence)]) -> bool {
        let Some(profile) = self.monitors.get_mut(key) else {
            return false;
        };
        let stamp = now_secs();
        let mut changed = false;
        for (value, presence) in items {
            if let Some(src) = profile.inputs.iter_mut().find(|s| s.value == *value) {
                // 只在结论真的变了才写盘：这个方法会被状态刷新每 5 秒调一次
                if src.presence != *presence {
                    src.presence = *presence;
                    src.checked_at = stamp;
                    changed = true;
                }
            }
        }
        if changed {
            let _ = self.save();
        }
        changed
    }

    pub fn presence_of(&self, key: &str, value: u16) -> Presence {
        self.monitors
            .get(key)
            .and_then(|p| p.inputs.iter().find(|s| s.value == value))
            .map(|s| s.presence)
            .unwrap_or(Presence::Unknown)
    }

    /// 已知有主机的口，按取值升序。热键与菜单都用它来避开空口。
    pub fn occupied_inputs(&self, key: &str) -> Vec<u16> {
        let Some(profile) = self.monitors.get(key) else {
            return Vec::new();
        };
        let mut out: Vec<u16> = profile
            .inputs
            .iter()
            .filter(|s| s.presence == Presence::Occupied)
            .map(|s| s.value)
            .collect();
        out.sort_unstable();
        out
    }

    /// 清掉所有"不存在"的口：固件不接受的取值留在菜单里只会误导人。
    pub fn drop_invalid(&mut self, key: &str) -> usize {
        let Some(profile) = self.monitors.get_mut(key) else {
            return 0;
        };
        let before = profile.inputs.len();
        profile
            .inputs
            .retain(|s| s.presence != Presence::Invalid);
        let removed = before - profile.inputs.len();
        if removed > 0 {
            let _ = self.save();
        }
        removed
    }
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod ghost_input_tests {
    use super::*;

    /// 对端自报"我挂在 0x12"留下的 Occupied 必须被清掉 —— 它不是本机证据。
    ///
    /// 这条锁的是 2026-10-22 那个回归：清理只写在 v1→v2 迁移里，配置升到
    /// v3 之后不再执行，扫描再写一次 `输入源 0x12` 就复活了。
    #[test]
    fn unverified_occupancy_is_cleared() {
        let mut cfg: Config = serde_json::from_str(
            r#"{
              "monitors": {
                "X": {
                  "uid": "X",
                  "name": "P275MV",
                  "capability": "read_write",
                  "inputs": [
                    {"name": "输入源 0x12", "value": 18, "verified": false, "presence": "occupied"},
                    {"name": "usbc", "value": 15, "verified": true, "presence": "occupied"}
                  ]
                }
              }
            }"#,
        )
        .expect("配置要能反序列化");
        assert_eq!(cfg.monitors.get("X").unwrap().inputs.len(), 2);

        assert_eq!(cfg.clear_unverified_occupancy(), 1);
        let profile = cfg.monitors.get("X").unwrap();
        assert_eq!(
            profile.inputs[0].presence,
            Presence::Unknown,
            "未验证的「有主机」必须清掉"
        );
        assert_eq!(
            profile.inputs[1].presence,
            Presence::Occupied,
            "真的切过去验证过的口不能动"
        );

        // 幂等：再跑一次不该有东西可清（否则每次加载都要写盘）
        assert_eq!(cfg.clear_unverified_occupancy(), 0);
    }
}

#[cfg(test)]
mod input_naming_tests {
    use super::*;

    /// MCCS 标准取值 → 短名；没把握的区间必须返回 None（宁可显示 0x..，别乱猜）
    #[test]
    fn mccs_names_cover_the_standard_range() {
        assert_eq!(crate::models::mccs_input_name(0x0F), Some("dp1"));
        assert_eq!(crate::models::mccs_input_name(0x10), Some("dp2"));
        assert_eq!(crate::models::mccs_input_name(0x11), Some("hdmi1"));
        assert_eq!(crate::models::mccs_input_name(0x12), Some("hdmi2"));
        assert_eq!(crate::models::mccs_input_name(0x13), None);
        assert_eq!(crate::models::mccs_input_name(0xFF), None);
    }

    /// 短名渲染：认识的按厂商写法，不认识的原样返回（用户起的名字不能改）
    #[test]
    fn pretty_names_read_like_a_monitor_osd() {
        assert_eq!(crate::models::pretty_input_name("hdmi2"), "HDMI-2");
        assert_eq!(crate::models::pretty_input_name("dp1"), "DP-1");
        assert_eq!(crate::models::pretty_input_name("usbc"), "USB-C");
        assert_eq!(crate::models::pretty_input_name("DP（Windows 那台）"), "DP（Windows 那台）");
    }

    /// 匿名口按 MCCS 改名，用户给它设过的显示标签要跟着搬到新名字上
    #[test]
    fn anonymous_inputs_are_renamed_and_labels_follow() {
        let mut cfg: Config = serde_json::from_str(
            r#"{
              "labels": { "输入源 0x12": "客厅那台" },
              "monitors": {
                "X": {
                  "uid": "X", "name": "P275MV", "capability": "read_write",
                  "inputs": [
                    {"name": "输入源 0x12", "value": 18, "verified": false, "presence": "unknown"},
                    {"name": "hdmi1", "value": 17, "verified": true, "presence": "occupied"}
                  ]
                }
              }
            }"#,
        )
        .unwrap();

        assert_eq!(cfg.rename_anonymous_inputs(), 1);
        let profile = cfg.monitors.get("X").unwrap();
        assert_eq!(profile.inputs[0].name, "hdmi2", "0x12 应按 MCCS 叫 hdmi2");
        assert_eq!(profile.inputs[1].name, "hdmi1", "有名字的口不许动");
        assert_eq!(cfg.label_for("hdmi2"), "客厅那台", "自定义标签要跟着搬");
        // 幂等
        assert_eq!(cfg.rename_anonymous_inputs(), 0);
    }
}
