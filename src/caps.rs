//! 显示器能力表（DDC/CI Get Capabilities）解析。
//!
//! 显示器通过 VCP 0xDF 自报一串 ASCII 文本，例如：
//!
//! ```text
//! (prot(monitor)type(lcd)model(P275MV)cmds(01 02 03 07 0C E3 F3)
//!  vcp(02 04 10 12 60(0F 10 11 12) 62 AC))
//! ```
//!
//! `vcp(60(...))` 里的括号内容就是**这台显示器支持的输入源取值**。
//! 有了它就不用再"逐个试值、试到黑屏"了 —— 这是程序能通用的关键。
//!
//! 现实里的字符串经常残缺：厂商固件会漏字符、分页边界会切开单词
//! （实测 P275MV PLUS 会返回 `MStarcmds0e(lcd)` 这种错位）。所以解析器
//! 必须宽松：找不到 `model(...)` 就算了，只认 `60(...)` 这一个片段。

/// 一次能力表读取的结果。
#[derive(Clone, Debug, Default)]
pub struct Caps {
    /// 原始文本，诊断时原样打印
    pub raw: String,
    /// VCP 0x60 支持的取值，升序去重
    pub inputs: Vec<u16>,
    /// 厂商自报的型号
    pub model: Option<String>,
    /// MCCS 版本，例如 "2.1"
    pub mccs_version: Option<String>,
    /// `vcp(...)` 段里声明的全部 VCP 代码。
    ///
    /// 这是判断"显示器愿不愿意被软件切输入源"的官方依据：
    /// 声明了 0x60 才代表它提供输入源切换能力。
    pub vcp_codes: Vec<u16>,
}

impl Caps {
    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty() && self.model.is_none() && self.raw.is_empty()
    }

    /// 有没有拿到输入源列表 —— 决定菜单能不能自动生成。
    pub fn has_inputs(&self) -> bool {
        !self.inputs.is_empty()
    }

    /// 能力表里明确声明了 VCP 0x60（Input Source）。
    ///
    /// 两个判据都算：显式声明了 `60` 这个代码，或给出了 `60(...)` 的取值列表。
    /// macOS 上分页拼接会错位，单页解析能拿到取值列表却未必拿到完整的 `vcp(...)` 段。
    pub fn declares_input_source(&self) -> bool {
        self.vcp_codes.contains(&0x60) || !self.inputs.is_empty()
    }
}

/// 解析能力表文本。解析不出输入源时 `inputs` 为空，调用方回退到其它策略。
pub fn parse(text: &str) -> Caps {
    // 去掉换行和多余空白，厂商常在分页处塞 \r\n
    let flat: String = text
        .chars()
        .map(|c| if c.is_ascii_graphic() || c == ' ' { c } else { ' ' })
        .collect();

    let mut caps = Caps {
        raw: text.trim().to_string(),
        inputs: find_input_values(&flat),
        model: find_field(&flat, "model"),
        mccs_version: find_field(&flat, "mccs_ver"),
        vcp_codes: find_vcp_codes(&flat),
    };
    caps.inputs.sort_unstable();
    caps.inputs.dedup();
    caps.vcp_codes.sort_unstable();
    caps.vcp_codes.dedup();
    caps
}

/// 扫 `vcp(...)` 段，列出里面声明的全部代码。
///
/// 两种写法都要认：`10` 这种裸值，和 `60(0F 10)` 这种带参数的。
/// 带参数的那个括号体要跳过，否则里面的取值会被误当成 VCP 代码。
fn find_vcp_codes(text: &str) -> Vec<u16> {
    let Some(pos) = text.find("vcp(") else {
        return Vec::new();
    };
    let mut out: Vec<u16> = Vec::new();
    let mut token = String::new();
    let mut depth = 1i32;

    let flush = |token: &mut String, out: &mut Vec<u16>| {
        let trimmed = token.trim();
        if !trimmed.is_empty() {
            if let Ok(value) = u16::from_str_radix(trimmed, 16) {
                if (1..=0xFF).contains(&value) {
                    out.push(value);
                }
            }
        }
        token.clear();
    };

    for ch in text[pos + 4..].chars() {
        match ch {
            '(' => {
                // 紧挨着的 token 就是带参数的 VCP 代码
                if depth == 1 {
                    flush(&mut token, &mut out);
                }
                depth += 1;
            }
            ')' => {
                depth -= 1;
                if depth <= 0 {
                    break;
                }
                if depth == 1 {
                    flush(&mut token, &mut out);
                }
            }
            c if depth == 1 => {
                if c.is_whitespace() {
                    flush(&mut token, &mut out);
                } else {
                    token.push(c);
                }
            }
            // 带参数 VCP 的括号体内部：整段丢弃
            _ => {}
        }
    }
    out
}

/// 找 `vcp(... 60(<值列表>) ...)` 里 0x60 的取值列表。
///
/// 不要求括号配对完美：从 `60(` 往后扫到最近的 `)`，中间按空白切词，
/// 只收纯十六进制且落在 1..=255 的词。错位的垃圾（如 `0DB`）会被
/// `from_str_radix` 直接挡掉。
fn find_input_values(text: &str) -> Vec<u16> {
    let bytes = text.as_bytes();
    let mut out: Vec<u16> = Vec::new();
    let mut i = 0;
    while i + 2 < bytes.len() {
        if bytes[i] == b'6' && bytes[i + 1] == b'0' && bytes[i + 2] == b'(' {
            let rest = &text[i + 3..];
            let end = rest.find(')').unwrap_or(rest.len());
            for token in rest[..end].split_whitespace() {
                let token = token.trim();
                if token.is_empty() || token.len() > 2 {
                    continue;
                }
                if let Ok(value) = u16::from_str_radix(token, 16) {
                    if (1..=0xFF).contains(&value) {
                        out.push(value);
                    }
                }
            }
        }
        i += 1;
    }
    out
}

/// 取 `key(内容)` 里的内容，支持中途被空白打断。
fn find_field(text: &str, key: &str) -> Option<String> {
    let pattern = format!("{}(", key);
    let start = text.find(&pattern)? + pattern.len();
    let rest = &text[start..];
    let end = rest.find(')')?;
    let value: String = rest[..end].trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// 把取值换算成 MCCS 标准里的常见含义，仅用于给菜单一个**可读的猜测**。
///
/// 厂商普遍不按标准来（实测 P275MV PLUS 的 0x0F 是 USB-C、0x10 是 DP），
/// 所以这只能当提示，不能当结论 —— 真正准确的来源是：
/// 1. 本机自己读到的当前值（就是本机所在口）
/// 2. 对端机器广播的值
pub fn standard_hint(value: u16) -> Option<&'static str> {
    match value {
        0x01 => Some("VGA"),
        0x02 => Some("VGA-2"),
        0x03 => Some("DVI"),
        0x04 => Some("DVI-2"),
        0x0F => Some("DP"),
        0x10 => Some("DP-2"),
        0x11 => Some("HDMI"),
        0x12 => Some("HDMI-2"),
        0x1B => Some("USB-C"),
        _ => None,
    }
}
