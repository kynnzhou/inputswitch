//! DDC/CI 后端抽象与平台派发。
//!
//! 三个平台的实现差异很大，但对外只有四个动作：
//! 列显示器、读 VCP、写 VCP、判断显示器是否还在。

use crate::models::Display;

#[cfg(target_os = "macos")]
pub mod mac;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(windows)]
pub mod win;

pub trait Backend: Send {
    fn platform(&self) -> &'static str;

    fn list_displays(&self) -> Vec<Display>;

    /// 读取 VCP 当前值。`None` 表示没有拿到**合法**应答，绝不等价于 0。
    fn get_vcp(&self, display: &Display, code: u8) -> Option<u16>;

    /// 写入 VCP。返回值只说明传输层接受与否，不代表显示器真的执行了。
    fn set_vcp(&self, display: &Display, code: u8, value: u16) -> bool;

    /// 最近一次读取失败的原因，用于诊断输出。
    fn last_note(&self) -> String {
        String::new()
    }

    /// 传输层是否通。用于区分"能写但读不到"和"这个口根本不转发 DDC"。
    fn transport_ok(&self, display: &Display) -> bool {
        self.get_vcp(display, crate::models::VCP_BRIGHTNESS).is_some()
    }

    /// 读显示器的 EDID（128 字节身份信息）。不支持的平台返回 `None`。
    fn read_edid(&self, _display: &Display) -> Option<Vec<u8>> {
        None
    }

    /// 读显示器自报的能力表。返回 `None` 表示该显示器/该平台不支持。
    ///
    /// 有了它就能知道这台显示器支持哪些输入源取值，不用逐个试。
    fn capabilities(&self, _display: &Display) -> Option<crate::caps::Caps> {
        None
    }

    /// 显示器是否仍在当前拓扑里。这是判断切换成功的唯一可靠信号。
    fn display_present(&self, display: &Display) -> bool {
        self.list_displays().iter().any(|d| d.uid == display.uid)
    }
}

pub fn new_backend() -> Box<dyn Backend> {
    #[cfg(target_os = "macos")]
    {
        Box::new(mac::MacBackend::new())
    }
    #[cfg(windows)]
    {
        Box::new(win::WinBackend::new())
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(linux::LinuxBackend::new())
    }
}

/// 读不到输入源时，逐层指出卡在哪一步。
///
/// 面板的「重新检测显示器」会把结论直接显示出来，
/// `inputswitch-cli probe` 也会打印，用户不用猜。
pub fn diagnose() -> String {
    #[cfg(windows)]
    {
        win::diagnose()
    }
    #[cfg(not(windows))]
    {
        let backend = new_backend();
        let displays = backend.list_displays();
        if displays.is_empty() {
            return "没枚举到可发 DDC 的显示器（画面可能正显示另一台机器）".to_string();
        }
        let mut lines: Vec<String> = Vec::new();
        for display in &displays {
            let brightness = backend.get_vcp(display, crate::models::VCP_BRIGHTNESS);
            let caps = backend.capabilities(display);
            let mut line = format!("{}：", display.name);
            match brightness {
                Some(value) => line.push_str(&format!("DDC 通（亮度 {value}）")),
                None => line.push_str("DDC 无应答（亮度也读不到）"),
            }
            match caps {
                Some(caps) if caps.has_inputs() => {
                    line.push_str(&format!("，能力表报 {} 个输入口", caps.inputs.len()))
                }
                Some(_) => line.push_str("，能力表里没写输入口"),
                None => line.push_str("，读不到能力表（VCP 0xDF）"),
            }
            lines.push(line);
        }
        lines.join("；")
    }
}
