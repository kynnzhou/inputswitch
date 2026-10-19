//! 判断"这台显示器能不能被软件切换"。
//!
//! 用户点了菜单没反应，原因通常不是代码错，而是下面几种之一：
//!
//! 1. **DDC/CI 在显示器 OSD 里被关掉了** —— 出厂一般开着，但有人会关；
//!    某些型号在"省电模式 / DP 1.2 兼容 / USB-C 优先"档位下也会关掉。
//! 2. **这条链路不转发 DDC** —— 转接头、KVM、部分扩展坞、老 HDMI 线。
//! 3. **只能写不能读** —— 苹果 Silicon 配某些屏很常见，切换能用但确认不了结果。
//! 4. **固件根本不支持输入源切换** —— 低端屏、投影仪、电视。
//!
//! 这个模块把四种情况分开，每种给一句能直接照做的话。全程**只读**：
//! 唯一一次写操作是把当前值原样写回去（画面不会变），用来确认写通道是否真的通。

use std::thread::sleep;
use std::time::Duration;

use crate::caps::Caps;
use crate::ddc::Backend;
use crate::models::{Display, VCP_BRIGHTNESS, VCP_INPUT_SOURCE};

/// 综合结论。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    /// 能读能写，且能力表声明了 0x60：完整可用
    Full,
    /// 能读能写，但能力表没读到或没声明 0x60：能用，只是信息不全
    Partial,
    /// 只能写：能切，但无法确认结果，也无法自动识别口
    WriteOnly,
    /// 只能读：完全没法切换
    ReadOnly,
    /// DDC 通道不通
    Blocked,
    /// 当前没有可操作的显示器（屏幕在对面那台）
    NoDisplay,
}

impl Support {
    pub fn mark(self) -> &'static str {
        match self {
            Support::Full => "✓",
            Support::Partial => "△",
            Support::WriteOnly => "△",
            Support::ReadOnly => "✗",
            Support::Blocked => "✗",
            Support::NoDisplay => "–",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Support::Full => "完整支持软件切换",
            Support::Partial => "可以使用（能力表信息不全）",
            Support::WriteOnly => "只能发送切换指令，无法确认结果",
            Support::ReadOnly => "只能读取，不能切换（写通道被拒）",
            Support::Blocked => "DDC 通道不通，软件无法切换",
            Support::NoDisplay => "屏幕不在这台机器上",
        }
    }

    /// 一句可以直接照做的建议。空字符串表示不需要做什么。
    pub fn advice(self) -> &'static str {
        match self {
            Support::Full => "",
            Support::Partial => "能用。想看到每个口有没有主机，运行一次「扫描输入口」即可。",
            Support::WriteOnly => {
                "切换指令发得出去，但读不到显示器的回答，所以没法确认有没有切成功。\
                 如果点了没反应，检查显示器 OSD 里的 DDC/CI 开关。"
            }
            Support::ReadOnly => {
                "显示器接受读取但拒绝写入。多半是 OSD 里的 DDC/CI 只开了一半，\
                 或者固件把输入源切换锁定了（部分型号叫「自动信号源」需要先关掉）。"
            }
            Support::Blocked => {
                "完全读不到显示器的 DDC 应答。按顺序排查：\
                 ① 显示器 OSD → 其他设置 → DDC/CI 是否打开；\
                 ② 换一根线直连，不要经过扩展坞 / KVM / 转接头；\
                 ③ 部分型号的「DP 1.2 / 1.4 兼容模式」「省电模式」会关掉 DDC。"
            }
            Support::NoDisplay => "屏幕已经被切到对面那台了，需在那边发指令切回来。",
        }
    }
}

/// 一次完整体检的结果。
#[derive(Clone, Debug)]
pub struct Check {
    pub display: String,
    pub platform: String,
    /// 传输层探活：任意 VCP 能否读到（用亮度）
    pub transport: bool,
    /// 读到的当前输入源取值
    pub current: Option<u16>,
    /// 写通道是否真的通：`None` 表示无法在不切屏的前提下判断
    pub writable: Option<bool>,
    /// 能力表是否读到
    pub caps_seen: bool,
    /// 能力表是否声明了 0x60
    pub caps_declares_input: bool,
    /// 能力表给出的可切换口
    pub inputs: Vec<u16>,
    /// 能读到的 VCP 代码数量，用作"DDC 通道质量"的粗略指标
    pub readable_vcps: usize,
    pub notes: Vec<String>,
}

impl Check {
    pub fn verdict(&self) -> Support {
        if !self.transport && self.current.is_none() && !self.caps_seen {
            return Support::Blocked;
        }
        match (self.current.is_some(), self.writable) {
            (true, Some(true)) => {
                if self.caps_declares_input {
                    Support::Full
                } else {
                    Support::Partial
                }
            }
            // 读得到、写被拒：多半 OSD 里 DDC/CI 只开了一半
            (true, Some(false)) => Support::ReadOnly,
            // 写通道通但读不到：苹果 Silicon 上的常见状态
            (false, Some(true)) => Support::WriteOnly,
            // 读不到也判断不了写：先按"只能写"归类，让用户试着点一下
            (false, None) => {
                if self.transport {
                    Support::WriteOnly
                } else {
                    Support::Blocked
                }
            }
            // 读得到、写未知
            (true, None) => Support::Partial,
            // 理论到不了这里：读不到当前值时根本不会去写探测
            (false, Some(_)) => Support::Blocked,
        }
    }

    /// 一行结论，托盘状态栏和 CLI 都用它。
    pub fn headline(&self) -> String {
        let verdict = self.verdict();
        format!("{} {}", verdict.mark(), verdict.label())
    }
}

/// 对一台显示器做只读体检。
pub fn check(backend: &dyn Backend, display: &Display, caps: Option<Caps>) -> Check {
    let mut notes: Vec<String> = Vec::new();

    // 先把当前输入源读出来：这是后面写探测的前提（要写回原值）
    let current = backend.get_vcp(display, VCP_INPUT_SOURCE);
    if current.is_none() {
        notes.push("读不到当前输入源（VCP 0x60）".into());
    }

    // 传输层探活：亮度是几乎所有显示器都实现的最基础 VCP
    let brightness = backend.get_vcp(display, VCP_BRIGHTNESS);
    let transport = brightness.is_some() || current.is_some();

    match brightness {
        Some(v) => notes.push(format!("亮度可读（当前 {}）", v)),
        None if transport => {}
        None => {
            let note = backend.last_note();
            if !note.is_empty() {
                notes.push(format!("读亮度失败：{}", note));
            }
        }
    }

    // 读一批常用 VCP，粗略衡量通道质量：一个都读不到说明 DDC 基本不可用
    let probes: [(u8, &str); 4] = [
        (0x10, "亮度"),
        (0x60, "输入源"),
        (0x62, "音量"),
        (0xDF, "能力表"),
    ];
    let readable_vcps = probes
        .iter()
        .filter(|(code, _)| backend.get_vcp(display, *code).is_some())
        .count();

    let caps_seen = caps.is_some();
    let caps_declares_input = caps.as_ref().map(|c| c.declares_input_source()).unwrap_or(false);
    let inputs = caps
        .as_ref()
        .map(|c| c.inputs.clone())
        .unwrap_or_default();

    if let Some(caps) = caps.as_ref() {
        if !caps.has_inputs() {
            notes.push(
                "能力表能读，但里面没有 vcp(60(...)) 段 —— 这台显示器可能不支持切换输入源".into(),
            );
        }
    } else if transport {
        notes.push("读不到能力表（VCP 0xDF），但 DDC 通道是通的".into());
    }

    // 写探测：把当前值原样写回去。画面不会变，但能确认写通道是否被接受。
    let writable = probe_write(backend, display, current, &mut notes);

    Check {
        display: display.name.clone(),
        platform: display.platform.to_string(),
        transport,
        current,
        writable,
        caps_seen,
        caps_declares_input,
        inputs,
        readable_vcps,
        notes,
    }
}

/// 写回原值来判断写通道。
///
/// 之所以敢写：写的是显示器**当前已经在用的那个值**，画面不会跳。
/// 读不到当前值时不写 —— 盲写很可能把屏幕切到空口上，那才是真正的事故。
fn probe_write(
    backend: &dyn Backend,
    display: &Display,
    current: Option<u16>,
    notes: &mut Vec<String>,
) -> Option<bool> {
    let Some(value) = current else {
        notes.push("读不到当前输入源，跳过写探测（避免盲写把屏幕切到空口）".into());
        return None;
    };
    if !backend.set_vcp(display, VCP_INPUT_SOURCE, value) {
        notes.push("写探测被拒绝：传输层直接返回失败".into());
        return Some(false);
    }
    // 厂商固件对写操作的回显时间不一致，读三次取任意一次成功
    for _ in 0..3 {
        sleep(Duration::from_millis(350));
        match backend.get_vcp(display, VCP_INPUT_SOURCE) {
            Some(v) if v == value => {
                notes.push(format!("写通道正常（0x60 写回 0x{:02X} 后读回一致）", value));
                return Some(true);
            }
            Some(_) => return Some(false),
            None => continue,
        }
    }
    notes.push("写入后读通道静默，无法确认写通道是否被接受".into());
    None
}
