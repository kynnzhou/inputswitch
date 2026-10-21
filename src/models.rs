//! 数据模型与 VCP 常量。

use serde::{Deserialize, Serialize};

/// MCCS: Input Source
pub const VCP_INPUT_SOURCE: u8 = 0x60;
/// MCCS: Luminance（亮度），常用来做通道探活
pub const VCP_BRIGHTNESS: u8 = 0x10;

/// MCCS 标准输入源取值。绝大多数显示器**不遵守**这套标准值，
/// 尤其 USB-C 和第二个 HDMI 口基本是厂商私有码，真实取值必须实测。
pub const STANDARD_INPUT_VALUES: &[(&str, u8)] = &[
    ("vga1", 0x01),
    ("vga2", 0x02),
    ("dvi1", 0x03),
    ("dvi2", 0x04),
    ("dp1", 0x0F),
    ("dp2", 0x10),
    ("hdmi1", 0x11),
    ("hdmi2", 0x12),
    ("usbc", 0x1B),
];

/// 试探 0x60 时的候选值顺序：先常见值，再扩大范围。
pub const CANDIDATE_INPUT_VALUES: &[u8] = &[
    0x0F, 0x11, 0x12, 0x10, 0x1B, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x01, 0x02, 0x13, 0x14,
    0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1C, 0x1D, 0x1E, 0x1F, 0x20, 0x21, 0x22, 0x0A, 0x0B,
    0x0C, 0x0D, 0x0E,
];

/// 实测过的显示器型号 → 输入源真实取值。
///
/// 厂商普遍不按 MCCS 标准值实现，所以这些值只能实测得出。已经测出来的直接内置，
/// 用户第一次运行就不用再走一遍"逐个试值"的黑屏探测。
///
/// P275MV PLUS（INNOCN，2×HDMI2.1 + DP1.4 + USB-C）实测，按厂家自己的顺序编号：
/// - USB-C = 15
/// - DP    = 16
/// - HDMI-1 = 17
///
/// 坑：18 也没验证。另外这台机器上"能切过去但几秒后自己退回"≠ 该口是空的 ——
/// 如果那个口上的主机处于 DPMS 休眠，同样会看到无信号回退，
/// 第一次判 DP 就栽在这里。
pub const KNOWN_MODELS: &[(&str, &[(&str, u16)])] = &[(
    "P275MV PLUS",
    &[("usbc", 15), ("dp1", 16), ("hdmi1", 17)],
)];

/// 一台物理显示器在当前平台的句柄。
#[derive(Clone, Debug)]
pub struct Display {
    /// 平台内稳定标识（同一启动周期内唯一）
    pub uid: String,
    /// 人类可读名称，同时作为 profile 的索引键
    pub name: String,
    pub platform: &'static str,
    pub is_main: bool,
    /// 平台私有句柄：macOS 存 IOKit service id
    pub handle: u64,
}

/// DDC 通道的实测能力。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// 读写都通
    ReadWrite,
    /// 能写但读不到有效应答（Apple Silicon 上很常见）
    WriteOnly,
    /// 该口不转发 DDC
    Unsupported,
}

impl Capability {
    pub fn label(self) -> &'static str {
        match self {
            Capability::ReadWrite => "可读可写",
            Capability::WriteOnly => "仅可写（读应答无效）",
            Capability::Unsupported => "不可用",
        }
    }

    pub fn mark(self) -> &'static str {
        match self {
            Capability::ReadWrite => "✓",
            Capability::WriteOnly => "△",
            Capability::Unsupported => "✗",
        }
    }
}

/// 某个输入口上有没有主机在输出信号。
///
/// DDC 协议本身**没有**"查询某个口是否接了设备"的命令：显示器只对当前
/// 激活的那条链路转发 DDC。所以这个状态只能靠三种侧写得到：
/// 1. 局域网广播（对面机器自己报告占用的口）—— 零副作用，最可靠
/// 2. 物理试探（切过去看显示器会不会自己退回来）—— 会短暂黑屏
/// 3. 本机自己的当前值 —— 一定是 Occupied
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    /// 还没检测过
    #[default]
    Unknown,
    /// 有主机在输出，切过去能留住画面
    Occupied,
    /// 没有活动信号：空口，或者那台主机处于休眠/关屏
    Idle,
    /// 固件不接受这个取值，这个口实际上不存在
    Invalid,
}

impl Presence {
    /// 菜单里挂在口名后面的短标记。空串表示"不显示"。
    pub fn mark(self) -> &'static str {
        match self {
            Presence::Unknown => "",
            Presence::Occupied => " · 有主机",
            Presence::Idle => " · 无信号",
            Presence::Invalid => " · 该口不存在",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Presence::Unknown => "未检测",
            Presence::Occupied => "有主机",
            Presence::Idle => "无信号",
            Presence::Invalid => "不存在",
        }
    }
}

/// profile 里记录的一个输入源。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InputSource {
    pub name: String,
    pub value: u16,
    #[serde(default)]
    pub verified: bool,
    /// 这个口上有没有主机（扫描/广播得到，跨启动保留）
    #[serde(default)]
    pub presence: Presence,
    /// 上一次判定这个状态的时间（unix 秒）。0 表示从没测过。
    #[serde(default)]
    pub checked_at: u64,
}

/// VCP 0x60（Input Source）的标准取值 → 短名。
///
/// 显示器自己的能力表经常**只给数值不给名字**（`vcp(60(0F 10 11 12))`），
/// 以前就退化成 `输入源 0x12` —— 用户没法判断那到底是哪个口。MCCS 对这组
/// 取值有标准定义，按标准起名比"0x12"有用得多（2026-10-22 用户要求）。
///
/// 只收录 0x01~0x12 这一段**没有歧义**的：0x13 往上各家实现不一致
/// （有的当 DVI、有的当 USB-C），猜错比不猜更糟，留给调用方回退成 0x..。
///
/// 名字刻意用短名（`hdmi2` 而不是 `HDMI-2`）：它是配置里的稳定键，
/// 显示用的漂亮格式由 `Config::label_for` 负责。
pub fn mccs_input_name(value: u16) -> Option<&'static str> {
    Some(match value {
        0x01 => "analog1",
        0x02 => "analog2",
        0x03 => "dvi1",
        0x04 => "dvi2",
        0x05 => "composite1",
        0x06 => "composite2",
        0x07 => "svideo1",
        0x08 => "svideo2",
        0x09 => "tuner1",
        0x0A => "tuner2",
        0x0B => "tuner3",
        0x0C => "component1",
        0x0D => "component2",
        0x0E => "component3",
        0x0F => "dp1",
        0x10 => "dp2",
        0x11 => "hdmi1",
        0x12 => "hdmi2",
        _ => return None,
    })
}

/// 把短名渲染成人看的样子：`hdmi2` → `HDMI-2`，`usbc` → `USB-C`。
///
/// 认不出来就原样返回（用户自己起的名字不该被改写）。
pub fn pretty_input_name(name: &str) -> String {
    let lower = name.trim().to_ascii_lowercase();
    let with_dash = |prefix: &str| -> Option<String> {
        let rest = lower.strip_prefix(prefix)?;
        if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        Some(format!("{}-{}", prefix.to_ascii_uppercase(), rest))
    };
    for prefix in ["dp", "hdmi", "dvi", "analog", "component", "composite", "svideo", "tuner"] {
        if let Some(pretty) = with_dash(prefix) {
            return pretty;
        }
    }
    match lower.as_str() {
        "usbc" | "usb-c" | "type-c" => "USB-C".to_string(),
        "vga" => "VGA".to_string(),
        _ => name.to_string(),
    }
}

/// 一台显示器型号的输入源映射表。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DisplayProfile {
    pub uid: String,
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_capability")]
    pub capability: Capability,
    #[serde(default)]
    pub inputs: Vec<InputSource>,
}

fn default_capability() -> Capability {
    Capability::ReadWrite
}

impl DisplayProfile {
    pub fn new(uid: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            uid: uid.into(),
            name: name.into(),
            capability: Capability::ReadWrite,
            inputs: Vec::new(),
        }
    }

    pub fn find(&self, name: &str) -> Option<&InputSource> {
        let key = name.trim().to_lowercase();
        self.inputs.iter().find(|s| s.name.to_lowercase() == key)
    }
}

/// 把显示器型号归一化成 profile key。
pub fn normalize_model(name: &str) -> String {
    let collapsed = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let upper = collapsed.to_uppercase();
    if upper.starts_with("外接显示器") && upper.contains('#') {
        return "UNKNOWN".to_string();
    }
    if upper.is_empty() {
        "UNKNOWN".to_string()
    } else {
        upper
    }
}
