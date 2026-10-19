//! 配置与运行状态持久化。
//!
//! 关键设计：profile 按**显示器型号**索引，不按主机。
//! VCP 0x60 的取值由显示器固件决定，Mac 和 Windows 问同一台显示器得到的是
//! 同一套值，所以在一台机器上试出来的映射另一台可以直接复用。

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::models::{normalize_model, Capability, DisplayProfile, Presence};

/// 配置结构版本。
///
/// 1 → 2：清掉旧版从对端广播里抄来的「有主机」标记（见 `Config::migrate`）。
pub const CONFIG_VERSION: u32 = 2;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Network {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_true() -> bool {
    true
}
fn default_port() -> u16 {
    47831
}

impl Default for Network {
    fn default() -> Self {
        Self {
            enabled: true,
            port: default_port(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub node: String,
    #[serde(default = "default_hotkey")]
    pub hotkey: String,
    #[serde(default)]
    pub switch_order: Vec<String>,
    #[serde(default)]
    pub labels: HashMap<String, String>,
    /// 本机上次读到的、自己接在哪个口上。屏幕不在本机时读不到 DDC，
    /// 这个值就是唯一还记得的线索 —— 靠它才能请对端把画面切回来。
    #[serde(default)]
    pub own_input: String,
    #[serde(default)]
    pub own_value: u16,
    /// 热键要推到哪个口（一般是"对面那台机器"接的口）
    #[serde(default)]
    pub hotkey_target: String,
    #[serde(default)]
    pub network: Network,
    #[serde(default)]
    pub pair: Pair,
    #[serde(default)]
    pub transfer: Transfer,
    #[serde(default)]
    pub clipboard: Clipboard,
    #[serde(default)]
    pub kvm: Kvm,
    #[serde(default)]
    pub monitors: HashMap<String, DisplayProfile>,
}

/// 配对：两台机器填同一个码，之后所有局域网消息都带签名。
///
/// 码本身只是"门禁"不是加密 —— 6 位数字只有 20 bit 熵，抓包的人可以离线穷举。
/// 所以还给了长口令的余地：填 16 位以上时熵就够当真正的密钥用了。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Pair {
    #[serde(default)]
    pub code: String,
}

impl Pair {
    /// 已经配对（码够长到能当密钥材料）
    pub fn is_set(&self) -> bool {
        !self.code.trim().is_empty()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transfer {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_transfer_port")]
    pub port: u16,
    /// 接收目录。空 = 默认目录
    #[serde(default)]
    pub inbox: String,
}

fn default_transfer_port() -> u16 {
    47832
}

impl Default for Transfer {
    fn default() -> Self {
        Self {
            enabled: true,
            port: default_transfer_port(),
            inbox: String::new(),
        }
    }
}

impl Transfer {
    pub fn inbox_dir(&self) -> PathBuf {
        if self.inbox.trim().is_empty() {
            home().join("Downloads").join("InputSwitch")
        } else {
            PathBuf::from(self.inbox.trim())
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Clipboard {
    /// 文本同步总开关（老行为，即时发）
    #[serde(default)]
    pub enabled: bool,
    /// 文件/图片按需传输：复制时只记清单，对方按粘贴时才传。
    /// 默认关，因为要两端都升到 v0.6.39 才有意义。
    #[serde(default)]
    pub files: bool,
    /// 传完之后自动补按一次粘贴键（用户只按一次）。
    /// 关掉的话，传完只提示"已就绪，请再按一次"。
    #[serde(default = "default_true")]
    pub auto_paste: bool,
}

/// 共用键鼠。两种模式，见 `kvm::Mode`。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Kvm {
    /// 是否启动。默认关：它会接管键盘鼠标，不能一装就生效。
    #[serde(default)]
    pub enabled: bool,
    /// `follow` = 跟随画面（软件 KVM，一套键鼠）；
    /// `edge`   = 边缘跨界（多屏共享，两台各有键鼠）。
    #[serde(default = "default_kvm_mode")]
    pub mode: String,
    /// `console` = 键鼠插在本机；`remote` = 键鼠在对面。只在 follow 模式用。
    #[serde(default = "default_kvm_role")]
    pub role: String,
    /// 边缘跨界模式下从哪一侧出去
    #[serde(default = "default_side")]
    pub side: String,
    /// 上次共用键鼠实际在用的对端节点名。启动/重启 KVM 时优先找它，
    /// 找不到才退回第一个在线节点 —— 局域网里有第三台装过本程序的
    /// 机器时（旧笔记本自启没关很常见），peers.first() 会稳定选错人，
    /// 两边各自对着错的机器发心跳，表现就是"两边都空闲、对端没回应"
    /// （2026-10-18 实测踩过：网上同时有 CHINAMI-0DFF1JU 和
    /// CHINAMI-MC5K9SJ 两台在通告）。
    #[serde(default)]
    pub peer_node: String,
}

fn default_side() -> String {
    "right".to_string()
}

fn default_kvm_mode() -> String {
    // 默认跟随画面：装这套软件的人，绝大多数是"一套键鼠、一块屏、两台机器"
    "follow".to_string()
}

fn default_kvm_role() -> String {
    "console".to_string()
}

impl Default for Kvm {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: default_kvm_mode(),
            role: default_kvm_role(),
            side: default_side(),
            peer_node: String::new(),
        }
    }
}

impl Default for Clipboard {
    fn default() -> Self {
        Self {
            enabled: false,
            files: false,
            auto_paste: true,
        }
    }
}

fn default_version() -> u32 {
    CONFIG_VERSION
}
fn default_hotkey() -> String {
    "ctrl+alt+s".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            node: default_node(),
            hotkey: default_hotkey(),
            switch_order: Vec::new(),
            labels: HashMap::new(),
            own_input: String::new(),
            own_value: 0,
            hotkey_target: String::new(),
            network: Network::default(),
            pair: Pair::default(),
            transfer: Transfer::default(),
            clipboard: Clipboard::default(),
            kvm: Kvm::default(),
            monitors: HashMap::new(),
        }
    }
}

pub fn default_node() -> String {
    std::env::var("COMPUTERNAME")
        .ok()
        .or_else(|| {
            let mut cmd = std::process::Command::new("hostname");
            crate::hidden::quiet(&mut cmd)
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| "node".to_string())
        .trim_end_matches(".local")
        .to_string()
}

pub fn config_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home().join("Library/Application Support/inputswitch")
    }
    #[cfg(windows)]
    {
        std::env::var("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home().join("AppData/Roaming"))
            .join("inputswitch")
    }
    #[cfg(target_os = "linux")]
    {
        std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home().join(".config"))
            .join("inputswitch")
    }
}

fn home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

impl Config {
    pub fn path() -> PathBuf {
        config_dir().join("config.json")
    }

    pub fn load() -> Self {
        let path = Self::path();
        let mut cfg = match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => {
                let mut cfg = Config::default();
                cfg.node = default_node();
                cfg
            }
        };
        cfg.migrate();
        cfg
    }

    /// 老配置的一次性清理。只跑一次（靠 `version` 门控），幂等。
    ///
    /// v1→v2：旧版会把对端广播的「我挂在 0x12」写进本机显示器的口表，
    /// 但每台显示器的口编号是各自固件定义的，跨机器搬过来就是错的 ——
    /// 表现为一个绿点幽灵口，点「切换」直接黑屏。
    ///
    /// 识别依据：`presence=Occupied` 且 `verified=false`。真正切过去过的口
    /// 一定同时被标成已验证（`record_success` 两个字段一起写），所以这条
    /// 判据只命中"从来没在本机验证过、却声称有主机"的条目。
    /// 代价：只跑过扫描、从没手动切过去的口也会回到"未检测"，重扫一次即可。
    fn migrate(&mut self) {
        if self.version >= CONFIG_VERSION {
            return;
        }
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
        self.version = CONFIG_VERSION;
        let _ = self.save();
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
            profile.inputs.push(crate::models::InputSource {
                name: format!("输入源 0x{:02X}", value),
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
            .unwrap_or_else(|| input_name.to_string())
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
