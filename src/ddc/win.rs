//! Windows DDC/CI 后端。
//!
//! 走 dxva2.dll 的 GetVCPFeatureAndVCPFeatureReply / SetVCPFeature，
//! 这是微软文档化的公开 API，绝大多数显卡驱动都会正确转发到显示器的 I2C 总线。
//!
//! 唯一高频坑：物理显示器句柄必须来自 GetPhysicalMonitorsFromHMONITOR，
//! **不能**把 HMONITOR 直接传进去 —— 这是新手写不出效果的头号原因。
//!
//! 所有 Win32 结构体与函数在此自行声明，不依赖任何绑定 crate 的版本。

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;

use super::Backend;
use crate::models::{Capability, Display};

type Bool = i32;
type Hmonitor = isize;
type Hdc = isize;
type Lparam = isize;

const MONITORINFOF_PRIMARY: u32 = 0x1;

#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct MonitorInfoExW {
    cb_size: u32,
    rc_monitor: Rect,
    rc_work: Rect,
    dw_flags: u32,
    sz_device: [u16; 32],
}

#[repr(C)]
struct PhysicalMonitor {
    handle: isize,
    description: [u16; 128],
}

#[repr(C)]
struct DisplayDeviceW {
    cb: u32,
    device_name: [u16; 32],
    device_string: [u16; 128],
    state_flags: u32,
    device_id: [u16; 128],
    device_key: [u16; 128],
}

type MonitorEnumProc =
    unsafe extern "system" fn(Hmonitor, Hdc, *mut Rect, Lparam) -> Bool;

#[link(name = "user32")]
extern "system" {
    fn EnumDisplayMonitors(
        hdc: Hdc,
        clip: *const Rect,
        callback: MonitorEnumProc,
        data: Lparam,
    ) -> Bool;
    fn GetMonitorInfoW(monitor: Hmonitor, info: *mut MonitorInfoExW) -> Bool;
    fn EnumDisplayDevicesW(
        device: *const u16,
        index: u32,
        info: *mut DisplayDeviceW,
        flags: u32,
    ) -> Bool;
}

#[link(name = "dxva2")]
extern "system" {
    fn GetNumberOfPhysicalMonitorsFromHMONITOR(monitor: Hmonitor, count: *mut u32) -> Bool;
    fn GetPhysicalMonitorsFromHMONITOR(
        monitor: Hmonitor,
        count: u32,
        array: *mut PhysicalMonitor,
    ) -> Bool;
    fn DestroyPhysicalMonitor(handle: isize) -> Bool;
    fn SetVCPFeature(handle: isize, code: u8, value: u32) -> Bool;
    fn GetVCPFeatureAndVCPFeatureReply(
        handle: isize,
        code: u8,
        vcp_type: *mut u32,
        current: *mut u32,
        maximum: *mut u32,
    ) -> Bool;
    // 显示器自报能力表：这是"通用化"的关键，能直接得到它支持哪些输入口
    fn GetCapabilitiesStringLength(handle: isize, length: *mut u32) -> Bool;
    fn CapabilitiesRequestAndCapabilitiesReply(
        handle: isize,
        out: *mut u8,
        length: u32,
    ) -> Bool;
}

#[link(name = "advapi32")]
extern "system" {
    fn RegOpenKeyExW(
        hkey: isize,
        sub_key: *const u16,
        options: u32,
        sam: u32,
        result: *mut isize,
    ) -> i32;
    fn RegQueryValueExW(
        hkey: isize,
        value_name: *const u16,
        reserved: *mut u32,
        kind: *mut u32,
        data: *mut u8,
        length: *mut u32,
    ) -> i32;
    fn RegCloseKey(hkey: isize) -> i32;
}

const HKEY_LOCAL_MACHINE: isize = 0x8000_0002u32 as isize;
const KEY_READ: u32 = 0x2_0019;

struct MonitorEntry {
    hmonitor: Hmonitor,
    device: String,
    /// 显示器设备实例 ID，形如 `\DISPLAY\LHC907D\5&xxxx&0&UID4352`
    device_id: String,
    primary: bool,
}

unsafe extern "system" fn enum_callback(
    hmonitor: Hmonitor,
    _hdc: Hdc,
    _rect: *mut Rect,
    data: Lparam,
) -> Bool {
    let entries = &mut *(data as *mut Vec<MonitorEntry>);
    let mut info: MonitorInfoExW = std::mem::zeroed();
    info.cb_size = std::mem::size_of::<MonitorInfoExW>() as u32;
    if GetMonitorInfoW(hmonitor, &mut info) != 0 {
        let len = info
            .sz_device
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(info.sz_device.len());
        let device = OsString::from_wide(&info.sz_device[..len])
            .to_string_lossy()
            .to_string();
        let device_id = device_id_of(&device).unwrap_or_default();
        entries.push(MonitorEntry {
            hmonitor,
            device,
            device_id,
            primary: info.dw_flags & MONITORINFOF_PRIMARY != 0,
        });
    }
    1
}

fn to_wide(text: &str) -> Vec<u16> {
    let mut buf: Vec<u16> = text.encode_utf16().collect();
    buf.push(0);
    buf
}

/// `\\.\DISPLAY1` → `\DISPLAY\LHC907D\5&xxxx&0&UID4352`
///
/// EnumDisplayDevices 要调两级：第一次拿适配器，第二次拿接在它上面的显示器。
fn device_id_of(device: &str) -> Option<String> {
    let wide = to_wide(device);
    let mut info: DisplayDeviceW = unsafe { std::mem::zeroed() };
    info.cb = std::mem::size_of::<DisplayDeviceW>() as u32;
    if unsafe { EnumDisplayDevicesW(wide.as_ptr(), 0, &mut info, 0) } == 0 {
        return None;
    }
    let id = wide_to_string(&info.device_id);
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// 从注册表读 128 字节 EDID。
///
/// 路径固定：`HKLM\SYSTEM\CurrentControlSet\Enum\<设备实例>\Device Parameters`
/// 下的 `EDID` 值。这是 Windows 上唯一不需要额外权限的 EDID 来源。
fn edid_from_registry(device_id: &str) -> Option<Vec<u8>> {
    let tail = device_id.trim_start_matches('\\');
    if tail.is_empty() {
        return None;
    }
    let sub = to_wide(&format!(
        "SYSTEM\\CurrentControlSet\\Enum\\{}\\Device Parameters",
        tail
    ));
    let value_name = to_wide("EDID");
    unsafe {
        let mut hkey: isize = 0;
        if RegOpenKeyExW(HKEY_LOCAL_MACHINE, sub.as_ptr(), 0, KEY_READ, &mut hkey) != 0 {
            return None;
        }
        let mut kind: u32 = 0;
        let mut length: u32 = 0;
        let rc = RegQueryValueExW(
            hkey,
            value_name.as_ptr(),
            std::ptr::null_mut(),
            &mut kind,
            std::ptr::null_mut(),
            &mut length,
        );
        if rc != 0 || length == 0 || length > 4096 {
            RegCloseKey(hkey);
            return None;
        }
        let mut data = vec![0u8; length as usize];
        let rc = RegQueryValueExW(
            hkey,
            value_name.as_ptr(),
            std::ptr::null_mut(),
            &mut kind,
            data.as_mut_ptr(),
            &mut length,
        );
        RegCloseKey(hkey);
        if rc != 0 || (length as usize) < data.len() {
            return None;
        }
        if data.len() >= 128 {
            Some(data)
        } else {
            None
        }
    }
}

fn wide_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    OsString::from_wide(&buf[..len])
        .to_string_lossy()
        .trim()
        .to_string()
}

fn enumerate() -> Vec<MonitorEntry> {
    let mut entries: Vec<MonitorEntry> = Vec::new();
    let data = &mut entries as *mut Vec<MonitorEntry> as Lparam;
    unsafe {
        EnumDisplayMonitors(0, std::ptr::null(), enum_callback, data);
    }
    entries
}

fn physical_handles(hmonitor: Hmonitor) -> Vec<(isize, String)> {
    let mut count: u32 = 0;
    unsafe {
        if GetNumberOfPhysicalMonitorsFromHMONITOR(hmonitor, &mut count) == 0 || count == 0 {
            return Vec::new();
        }
        let mut array: Vec<PhysicalMonitor> = (0..count)
            .map(|_| PhysicalMonitor {
                handle: 0,
                description: [0u16; 128],
            })
            .collect();
        if GetPhysicalMonitorsFromHMONITOR(hmonitor, count, array.as_mut_ptr()) == 0 {
            return Vec::new();
        }
        array
            .iter()
            .map(|m| (m.handle, wide_to_string(&m.description)))
            .collect()
    }
}

fn release_handles(handles: &[(isize, String)]) {
    for (handle, _) in handles {
        unsafe {
            DestroyPhysicalMonitor(*handle);
        }
    }
}

pub struct WinBackend;

impl WinBackend {
    pub fn new() -> Self {
        Self
    }

    fn entry_for(&self, display: &Display) -> Option<MonitorEntry> {
        let wanted = display.uid.strip_prefix("win:").unwrap_or(&display.uid);
        let wanted_device = wanted.split('#').next().unwrap_or(wanted);
        enumerate().into_iter().find(|e| e.device == wanted_device)
    }

    fn hmonitor_for(&self, display: &Display) -> Option<Hmonitor> {
        self.entry_for(display).map(|e| e.hmonitor)
    }
}

impl Backend for WinBackend {
    fn platform(&self) -> &'static str {
        "win32"
    }

    fn list_displays(&self) -> Vec<Display> {
        let mut out = Vec::new();
        for entry in enumerate() {
            let handles = physical_handles(entry.hmonitor);
            if handles.is_empty() {
                continue;
            }
            for (index, (_handle, description)) in handles.iter().enumerate() {
                let uid = if handles.len() == 1 {
                    format!("win:{}", entry.device)
                } else {
                    format!("win:{}#{}", entry.device, index)
                };
                out.push(Display {
                    uid,
                    name: if description.is_empty() {
                        entry.device.clone()
                    } else {
                        description.clone()
                    },
                    platform: "win32",
                    is_main: entry.primary,
                    handle: entry.hmonitor as u64,
                });
            }
            release_handles(&handles);
        }
        out
    }

    fn get_vcp(&self, display: &Display, code: u8) -> Option<u16> {
        let hmonitor = self.hmonitor_for(display)?;
        let handles = physical_handles(hmonitor);
        let handle = handles.first()?.0;
        let mut vcp_type: u32 = 0;
        let mut current: u32 = 0;
        let mut maximum: u32 = 0;
        let ok = unsafe {
            GetVCPFeatureAndVCPFeatureReply(
                handle,
                code,
                &mut vcp_type,
                &mut current,
                &mut maximum,
            )
        };
        release_handles(&handles);
        if ok != 0 {
            Some(current as u16)
        } else {
            None
        }
    }

    fn set_vcp(&self, display: &Display, code: u8, value: u16) -> bool {
        let Some(hmonitor) = self.hmonitor_for(display) else {
            return false;
        };
        let handles = physical_handles(hmonitor);
        let Some(handle) = handles.first().map(|(h, _)| *h) else {
            return false;
        };
        // 零副作用写入校验：读回来的值原样写回，能验证写通道是否通
        let ok = unsafe { SetVCPFeature(handle, code, value as u32) };
        release_handles(&handles);
        ok != 0
    }

    /// EDID 走注册表：dxva2 不提供 EDID 读取，而设备实例键下就有一份原样副本。
    fn read_edid(&self, display: &Display) -> Option<Vec<u8>> {
        let entry = self.entry_for(display)?;
        edid_from_registry(&entry.device_id)
    }

    /// 显示器自报的能力表。dxva2 内部会把 DDC 分页读全，一次拿到完整字符串。
    fn capabilities(&self, display: &Display) -> Option<crate::caps::Caps> {
        let hmonitor = self.hmonitor_for(display)?;
        let handles = physical_handles(hmonitor);
        let handle = handles.first()?.0;

        let mut length: u32 = 0;
        if unsafe { GetCapabilitiesStringLength(handle, &mut length) } == 0
            || length == 0
            || length > 64 * 1024
        {
            release_handles(&handles);
            return None;
        }
        let mut buf = vec![0u8; length as usize];
        let ok =
            unsafe { CapabilitiesRequestAndCapabilitiesReply(handle, buf.as_mut_ptr(), length) };
        release_handles(&handles);
        if ok == 0 {
            return None;
        }
        let text = String::from_utf8_lossy(&buf)
            .trim_end_matches('\0')
            .to_string();
        if text.trim().is_empty() {
            return None;
        }
        Some(crate::caps::parse(&text))
    }

    fn transport_ok(&self, display: &Display) -> bool {
        let Some(hmonitor) = self.hmonitor_for(display) else {
            return false;
        };
        let handles = physical_handles(hmonitor);
        let Some(handle) = handles.first().map(|(h, _)| *h) else {
            return false;
        };
        let mut vcp_type: u32 = 0;
        let mut current: u32 = 0;
        let mut maximum: u32 = 0;
        let ok = unsafe {
            GetVCPFeatureAndVCPFeatureReply(
                handle,
                crate::models::VCP_BRIGHTNESS,
                &mut vcp_type,
                &mut current,
                &mut maximum,
            )
        };
        release_handles(&handles);
        ok != 0
    }
}

impl Default for WinBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// 供诊断使用的能力判断辅助。
pub fn quick_capability(backend: &WinBackend, display: &Display) -> Capability {
    if backend.get_vcp(display, crate::models::VCP_BRIGHTNESS).is_some() {
        Capability::ReadWrite
    } else if backend.transport_ok(display) {
        Capability::WriteOnly
    } else {
        Capability::Unsupported
    }
}

/// 把 DDC 读取的每一步都走一遍，指出卡在哪一步。
///
/// Windows 上读不到输入源的三种典型原因差别很大，光看"没读到"分不清：
///   1. 系统层面就没枚举到显示器（画面在别的输入口）
///   2. 枚举到了但拿不到物理显示器句柄
///   3. 句柄有了但显示器不回 DDC（扩展坞/切换器/虚拟机）
pub fn diagnose() -> String {
    let entries = enumerate();
    if entries.is_empty() {
        return "系统没枚举到任何显示器（EnumDisplayMonitors 返回 0 台）".to_string();
    }

    let mut lines: Vec<String> = Vec::new();
    for entry in &entries {
        let handles = physical_handles(entry.hmonitor);
        if handles.is_empty() {
            lines.push(format!(
                "{}：拿不到物理显示器句柄（GetPhysicalMonitorsFromHMONITOR 失败）",
                entry.device
            ));
            continue;
        }
        let (handle, description) = handles[0].clone();

        let mut vcp_type: u32 = 0;
        let mut current: u32 = 0;
        let mut maximum: u32 = 0;
        let brightness = unsafe {
            GetVCPFeatureAndVCPFeatureReply(
                handle,
                crate::models::VCP_BRIGHTNESS,
                &mut vcp_type,
                &mut current,
                &mut maximum,
            )
        };
        let mut caps_len: u32 = 0;
        let caps = unsafe { GetCapabilitiesStringLength(handle, &mut caps_len) };

        let name = if description.is_empty() {
            entry.device.clone()
        } else {
            description
        };
        let mut line = format!("{}（{}）：", name, entry.device);
        if brightness == 0 {
            line.push_str("DDC 完全没有应答（亮度也读不到）");
            line.push_str(" —— 多为这一路输入没在显示，或中间串了扩展坞/切换器/虚拟机");
        } else {
            line.push_str(&format!("DDC 通（亮度 {current}）"));
            if caps == 0 || caps_len == 0 {
                line.push_str("，但显示器不回能力表（VCP 0xDF），只能手工指定输入口");
            } else {
                line.push_str(&format!("，能力表 {caps_len} 字节"));
            }
        }
        lines.push(line);
        release_handles(&handles);
    }
    lines.join("；")
}
