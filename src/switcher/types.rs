//! 报告与结论的名词：一次探测 / 扫描 / 切换长什么样。
//!
//! 动词在各自的文件里：scan.rs 产出 ScanReport，switch.rs 产出 SwitchOutcome。
//! 这里只放数据结构 —— 改"报告里有哪些字段"只看这一个文件。

use crate::models::Presence;

/// 一个口的探测结果是怎么来的。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeSource {
    /// 本机自己就挂在这个口上
    SelfHost,
    /// 对端机器通过局域网报的
    PeerReport,
    /// 物理切过去试出来的
    Physical,
    /// 从历史记录里带过来的
    Cached,
}

impl ProbeSource {
    pub fn label(self) -> &'static str {
        match self {
            ProbeSource::SelfHost => "本机",
            ProbeSource::PeerReport => "对端报告",
            ProbeSource::Physical => "实测",
            ProbeSource::Cached => "上次结果",
        }
    }
}

/// 一个口的最终判定。
#[derive(Clone, Debug)]
pub struct PortProbe {
    pub value: u16,
    pub name: String,
    pub presence: Presence,
    pub via: ProbeSource,
}

/// 一轮扫描的完整结果。
#[derive(Clone, Debug)]
pub struct ScanReport {
    pub ports: Vec<PortProbe>,
    /// 扫描前本机占用哪个值
    pub original: Option<u16>,
    /// 扫描结束时屏幕是否回到了本机
    pub back_home: bool,
    /// 中途放弃的原因
    pub aborted: Option<String>,
    /// 逐条过程记录
    pub steps: Vec<String>,
}

impl ScanReport {
    /// 有没有真的发过切换指令（决定要不要提示"这次动作可能改了你的屏幕"）
    pub fn touched_screen(&self) -> bool {
        self.ports.iter().any(|p| p.via == ProbeSource::Physical)
    }

    pub fn occupied(&self) -> Vec<u16> {
        self.ports
            .iter()
            .filter(|p| p.presence == Presence::Occupied)
            .map(|p| p.value)
            .collect()
    }
}

pub struct SwitchOutcome {
    pub ok: bool,
    pub message: String,
    pub input_name: String,
    /// 失败是否有确凿反证。「没有生效」这类**拿不到证据**的失败不算——
    /// 有的显示器切到别的输入源后仍挂在 macOS 拓扑里（EDID 一直活着）、
    /// VCP 读值也不更新，两个成功信号全部失效，但画面实际是切成功的。
    /// 调用方（跟随模式）对确凿失败要立刻收回键鼠，对模糊失败则不能抢收。
    pub definite: bool,
}

impl SwitchOutcome {
    pub(super) fn ok(input: &str, message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: message.into(),
            input_name: input.to_string(),
            definite: true,
        }
    }

    pub(super) fn fail(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
            input_name: String::new(),
            definite: true,
        }
    }

    /// 证据不足的失败：可能成功、可能失败，分不清。
    pub(super) fn fail_ambiguous(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
            input_name: String::new(),
            definite: false,
        }
    }
}
