//! EDID 解析：显示器的身份信息。
//!
//! EDID 是显示器通过 I2C 地址 0x50 自报的 128 字节结构，包含厂商、型号、
//! 序列号、生产日期、物理尺寸。它是"这台显示器是谁"的唯一可靠来源 ——
//! 型号名字符串在不同系统上写法不一致（Windows 给 "Generic PnP Monitor"，
//! macOS 给 "P275MV PLUS"），拿它当配置键早晚会串。
//!
//! 这里只做纯解析，不碰 IO。取字节的工作交给各平台后端。

/// 从 EDID 读出的显示器身份。
#[derive(Clone, Debug, Default)]
pub struct Edid {
    /// 三字母厂商码，例如 "INN"
    pub vendor: String,
    /// 厂商内部产品码
    pub product: u16,
    /// 数字序列号（很多机型填 0）
    pub serial: u32,
    /// 生产周（0 表示未指定）
    pub week: u8,
    pub year: u16,
    /// descriptor 0xFC：显示器名字
    pub name: Option<String>,
    /// descriptor 0xFF：序列号文本
    pub serial_text: Option<String>,
    /// 屏幕物理尺寸（厘米），宽 x 高
    pub size_cm: Option<(u8, u8)>,
    /// 是否数字输入
    pub digital: bool,
    /// 原始字节，诊断时打印用
    pub raw: Vec<u8>,
}

impl Edid {
    /// 跨机器、跨重启都稳定的配置键。
    ///
    /// 优先用数字序列号；序列号为 0 时退到序列号文本；
    /// 两者都没有时才只靠厂商 + 产品码（同型号多台会撞，但总比型号名稳）。
    pub fn identity(&self) -> String {
        let vendor = if self.vendor.is_empty() {
            "UNK".to_string()
        } else {
            self.vendor.clone()
        };
        if self.serial != 0 {
            format!("{}-{:04X}-{:08X}", vendor, self.product, self.serial)
        } else if let Some(text) = self.serial_text.as_deref().map(str::trim) {
            let cleaned: String = text
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect();
            if cleaned.is_empty() || cleaned.chars().all(|c| c == '0') {
                format!("{}-{:04X}", vendor, self.product)
            } else {
                format!("{}-{:04X}-{}", vendor, self.product, cleaned)
            }
        } else {
            format!("{}-{:04X}", vendor, self.product)
        }
    }

    /// 给人看的名字：型号名 > 厂商 + 产品码。
    pub fn display_name(&self) -> String {
        if let Some(name) = self.name.as_deref().map(str::trim) {
            if !name.is_empty() {
                return name.to_string();
            }
        }
        if self.vendor.is_empty() {
            "未知显示器".to_string()
        } else {
            format!("{} {:04X}", self.vendor, self.product)
        }
    }

    /// "2026 年第 12 周" 这样的一行说明。
    pub fn manufactured(&self) -> Option<String> {
        if self.year == 0 {
            return None;
        }
        if self.week == 0 {
            Some(format!("{} 年", self.year))
        } else {
            Some(format!("{} 年第 {} 周", self.year, self.week))
        }
    }

    /// 对角线英寸，由厘米尺寸换算。
    pub fn diagonal_inch(&self) -> Option<f32> {
        let (w, h) = self.size_cm?;
        if w == 0 || h == 0 {
            return None;
        }
        let cm = ((w as f32).powi(2) + (h as f32).powi(2)).sqrt();
        Some(cm / 2.54)
    }
}

/// 解析 128 字节基础 EDID。数据不合法时返回 None。
pub fn parse(bytes: &[u8]) -> Option<Edid> {
    if bytes.len() < 128 {
        return None;
    }
    if bytes[0..8] != [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00] {
        return None;
    }

    // 厂商码：两个字节里的 15 位，每 5 位一个字母，1 = 'A'
    let packed = ((bytes[8] as u16) << 8) | bytes[9] as u16;
    let decode = |v: u16| -> char {
        let c = (v & 0x1F) as u8;
        if (1..=26).contains(&c) {
            (b'A' + c - 1) as char
        } else {
            '?'
        }
    };
    let vendor = format!(
        "{}{}{}",
        decode(packed >> 10),
        decode(packed >> 5),
        decode(packed)
    );
    let vendor = if vendor.contains('?') {
        String::new()
    } else {
        vendor
    };

    let product = (bytes[10] as u16) | ((bytes[11] as u16) << 8);
    let serial = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    let week = bytes[16];
    let year = 1990 + bytes[17] as u16;

    let mut edid = Edid {
        vendor,
        product,
        serial,
        week,
        year,
        name: None,
        serial_text: None,
        size_cm: None,
        digital: bytes[20] & 0x80 != 0,
        raw: bytes[..128].to_vec(),
    };

    // 屏幕尺寸：字节 21/22 分别是宽高厘米；为 0 表示用后面的比例描述
    if bytes[21] != 0 && bytes[22] != 0 {
        edid.size_cm = Some((bytes[21], bytes[22]));
    }

    // 四个 18 字节 descriptor，起点 54
    for i in 0..4 {
        let start = 54 + i * 18;
        if start + 18 > 128 {
            break;
        }
        let d = &bytes[start..start + 18];
        // 前 5 字节这样的才是 descriptor（其余是 timing descriptor）
        if d[0] != 0 || d[1] != 0 {
            continue;
        }
        let text = |slice: &[u8]| -> Option<String> {
            let s: String = slice
                .iter()
                .take_while(|b| **b != 0x0A && **b != 0)
                .map(|b| *b as char)
                .collect();
            let trimmed = s.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        };
        match d[3] {
            0xFC => edid.name = text(&d[5..18]),
            0xFF => edid.serial_text = text(&d[5..18]),
            _ => {}
        }
    }

    Some(edid)
}
