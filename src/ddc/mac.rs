//! macOS DDC/CI 后端。
//!
//! Apple Silicon 上 DDC 不走 IOFramebuffer，而是走 Display Coprocessor 暴露的
//! `DCPAVServiceProxy` 服务，用 `IOAVServiceWriteI2C` / `IOAVServiceReadI2C`
//! 访问 I2C 上地址 0x37 的 DDC/CI 设备。这些符号没有公开头文件，
//! 这里直接声明 FFI，不引入任何第三方绑定。
//!
//! 帧格式对齐 m1ddc（macOS 上唯一被大规模验证过的实现）：
//! - 写：`IOAVServiceWriteI2C(chip, 0x51, [0x84, 0x03, code, hi, lo, ck], 6)`
//! - 读：`IOAVServiceReadI2C (chip, 0x51, buf, 12)` —— 读不需要先发请求帧，
//!   偏移必须是 0x51 而不是 0，长度必须是 12 而不是 11。
//! 这两点写错的表现就是每次读回来的都是同一段未初始化内存（`6e 80 be ...`），
//! 写则静默失败：DCP 接受事务但显示器根本收不到指令。
//!
//! 实测要点（M4 + INNOCN P275MV PLUS）：
//! 1. 写返回 0 只代表 DCP 接受了 I2C 事务，不代表显示器执行了 ——
//!    切换是否生效仍然只能靠"显示器从拓扑里消失"来判断。
//! 2. 显示器被切走后本机 DDC 立即失效，切回来必须由对面主机发起。
//! 3. 部分机型的 DDC 挂在 0x50（MCDP29XX），需要回退重试。

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

use super::Backend;
use crate::models::Display;

type IoReturn = c_int;
type IoObject = c_uint;
type IoIterator = c_uint;

/// DDC/CI 芯片地址：绝大多数机型是 0x37，MCDP29xx 是 0x50。
const CHIP_ADDR: u32 = 0x37;
const CHIP_ADDR_MCDP: u32 = 0x50;
/// I2C 上 DDC/CI 的寄存器偏移，等价于 DDC 帧的源地址。
const INPUT_ADDR: u8 = 0x51;
const INPUT_ADDR_ALT: u8 = 0x50;
/// EDID EEPROM 的 I2C 地址。读它不经过 DDC/CI 协议，offset 就是字节偏移。
const EDID_ADDR: u32 = 0x50;
/// DDC/CI 应答固定 12 字节（含目的地址、源地址、opcode、数据、校验和）。
const REPLY_LEN: usize = 12;
const DDC_WAIT: Duration = Duration::from_millis(12);
const CF_UTF8: u32 = 0x0800_0100;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(
        main_port: c_uint,
        matching: *mut c_void,
        existing: *mut IoIterator,
    ) -> IoReturn;
    fn IOIteratorNext(iterator: IoIterator) -> IoObject;
    fn IOObjectRelease(object: IoObject) -> IoReturn;
    fn IORegistryEntryCreateCFProperty(
        entry: IoObject,
        key: *const c_void,
        allocator: *const c_void,
        options: c_uint,
    ) -> *mut c_void;
    fn IORegistryEntryGetChildIterator(
        entry: IoObject,
        plane: *const c_char,
        iterator: *mut IoIterator,
    ) -> IoReturn;
    fn IORegistryEntryGetRegistryEntryID(entry: IoObject, id: *mut u64) -> IoReturn;
    fn IOAVServiceCreateWithService(allocator: *const c_void, service: IoObject) -> *mut c_void;
    fn IOAVServiceReadI2C(
        service: *mut c_void,
        chip: u32,
        offset: u32,
        buffer: *mut c_void,
        length: u32,
    ) -> IoReturn;
    fn IOAVServiceWriteI2C(
        service: *mut c_void,
        chip: u32,
        offset: u32,
        buffer: *mut c_void,
        length: u32,
    ) -> IoReturn;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(
        alloc: *const c_void,
        s: *const c_char,
        encoding: u32,
    ) -> *const c_void;
    fn CFStringGetCString(
        s: *const c_void,
        buffer: *mut c_char,
        size: isize,
        encoding: u32,
    ) -> bool;
    fn CFGetTypeID(cf: *const c_void) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFRelease(cf: *const c_void);
}

fn xor_all(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |acc, b| acc ^ b)
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// DDC/CI "Set VCP Feature" 帧：`84 03 <code> <hi> <lo> <ck>`。
fn write_frame(code: u8, value: u16, addr: u8) -> [u8; 6] {
    let mut f = [
        0x84,
        0x03,
        code,
        (value >> 8) as u8,
        (value & 0xFF) as u8,
        0,
    ];
    f[5] = xor_all(&[0x6E ^ addr, f[0], f[1], f[2], f[3], f[4]]);
    f
}

/// DDC/CI "Get VCP Feature" 请求帧，仅在 ReadI2C 不带请求语义时需要显式写一次。
fn read_frame(code: u8, addr: u8) -> [u8; 4] {
    let mut f = [0x82, 0x01, code, 0];
    f[3] = xor_all(&[0x6E ^ addr, f[0], f[1], f[2]]);
    f
}

/// 读取 IOKit 节点上的字符串属性。
fn prop_string(entry: IoObject, key: &str) -> Option<String> {
    let cf_key = {
        let c = CString::new(key).ok()?;
        unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), CF_UTF8) }
    };
    if cf_key.is_null() {
        return None;
    }
    let value = unsafe { IORegistryEntryCreateCFProperty(entry, cf_key, std::ptr::null(), 0) };
    unsafe { CFRelease(cf_key) };
    if value.is_null() {
        return None;
    }
    let mut out: Option<String> = None;
    unsafe {
        if CFGetTypeID(value) == CFStringGetTypeID() {
            let mut buf = [0i8; 256];
            if CFStringGetCString(value, buf.as_mut_ptr(), buf.len() as isize, CF_UTF8) {
                let cstr = std::ffi::CStr::from_ptr(buf.as_ptr());
                out = cstr.to_str().ok().map(|s| s.to_string());
            }
        }
        CFRelease(value);
    }
    out
}

fn children(entry: IoObject) -> Vec<IoObject> {
    let mut iter: IoIterator = 0;
    let plane = CString::new("IOService").unwrap();
    if unsafe { IORegistryEntryGetChildIterator(entry, plane.as_ptr(), &mut iter) } != 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    loop {
        let child = unsafe { IOIteratorNext(iter) };
        if child == 0 {
            break;
        }
        out.push(child);
    }
    unsafe { IOObjectRelease(iter) };
    out
}

fn registry_id(entry: IoObject) -> u64 {
    let mut id: u64 = 0;
    unsafe { IORegistryEntryGetRegistryEntryID(entry, &mut id) };
    id
}

fn collect_external(entry: IoObject, depth: usize, out: &mut Vec<IoObject>) {
    if prop_string(entry, "Location").as_deref() == Some("External") {
        let service = unsafe { IOAVServiceCreateWithService(std::ptr::null(), entry) };
        if !service.is_null() {
            unsafe { CFRelease(service) };
            out.push(entry);
            return;
        }
    }
    if depth >= 2 {
        return;
    }
    for child in children(entry) {
        collect_external(child, depth + 1, out);
    }
}

/// 用 system_profiler 补齐型号名。现代 macOS 已不再把 EDID 暴露到 IORegistry，
/// 这是唯一还能拿到显示器官名的公开途径。
fn profiler_names() -> Vec<String> {
    let output = Command::new("system_profiler")
        .args(["SPDisplaysDataType", "-json"])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    if let Some(gpus) = json.get("SPDisplaysDataType").and_then(|v| v.as_array()) {
        for gpu in gpus {
            if let Some(displays) = gpu.get("spdisplays_ndrvs").and_then(|v| v.as_array()) {
                for disp in displays {
                    if disp.get("spdisplays_builtin").and_then(|v| v.as_str())
                        == Some("spdisplays_yes")
                    {
                        continue;
                    }
                    if let Some(name) = disp.get("_name").and_then(|v| v.as_str()) {
                        let name = name.trim();
                        if !name.is_empty() {
                            names.push(name.to_string());
                        }
                    }
                }
            }
        }
    }
    names
}

pub struct MacBackend {
    note: std::cell::RefCell<String>,
}

/// 一次 I2C 读的结果：原始字节 + 传输层返回码。
pub struct RawReply {
    pub bytes: Vec<u8>,
    pub rc: i32,
    pub chip: u32,
    pub addr: u8,
}

impl MacBackend {
    pub fn new() -> Self {
        Self {
            note: std::cell::RefCell::new(String::new()),
        }
    }

    fn av_service(&self, display: &Display) -> Option<*mut c_void> {
        let service =
            unsafe { IOAVServiceCreateWithService(std::ptr::null(), display.handle as IoObject) };
        if service.is_null() {
            None
        } else {
            Some(service)
        }
    }

    fn write_i2c(&self, service: *mut c_void, chip: u32, offset: u32, payload: &[u8]) -> i32 {
        let mut buf = payload.to_vec();
        // m1ddc 同样写两遍：某些显示器第一帧会被吞掉。
        let mut rc = -1;
        for _ in 0..2 {
            sleep(DDC_WAIT);
            rc = unsafe {
                IOAVServiceWriteI2C(
                    service,
                    chip,
                    offset,
                    buf.as_mut_ptr() as *mut c_void,
                    buf.len() as u32,
                )
            };
            if rc == 0 {
                break;
            }
        }
        rc
    }

    fn read_i2c(&self, service: *mut c_void, chip: u32, offset: u32) -> RawReply {
        self.read_i2c_len(service, chip, offset, REPLY_LEN)
    }

    /// 变长 I2C 读。读 CAPS 字符串时必须一次读完整段（12 字节头 + 32 字节数据），
    /// 分两次读会拿到重复的流式数据。
    fn read_i2c_len(
        &self,
        service: *mut c_void,
        chip: u32,
        offset: u32,
        len: usize,
    ) -> RawReply {
        let mut buf = vec![0u8; len];
        sleep(DDC_WAIT);
        let rc = unsafe {
            IOAVServiceReadI2C(
                service,
                chip,
                offset,
                buf.as_mut_ptr() as *mut c_void,
                len as u32,
            )
        };
        RawReply {
            bytes: buf,
            rc,
            chip,
            addr: offset as u8,
        }
    }

    /// 读一次「显示器能力表」（DDC/CI Get Capabilities, opcode 0xF3）。
    ///
    /// 这是让程序变通用的关键：显示器会自报支持哪些 VCP，其中
    /// `vcp(60(0F 10 11 12))` 直接列出输入源所有合法取值，不用猜。
    /// 协议：请求帧 5 字节 `83 F3 <off_hi> <off_lo> <ck>`，
    /// 应答 `6E 88 E3 <result> <off_hi> <off_lo> <rem_hi> <rem_lo> <data...> <ck>`。
    pub fn caps_raw(&self, display: &Display, offset: u16) -> Vec<RawReply> {
        let Some(service) = self.av_service(display) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (chip, addr) in [
            (CHIP_ADDR, INPUT_ADDR_ALT),
            (CHIP_ADDR, INPUT_ADDR),
            (CHIP_ADDR_MCDP, INPUT_ADDR),
        ] {
            // 32 字节数据 + 12 字节报文头，一次读完
            let len = REPLY_LEN + 32;
            let mut frame = [0x83u8, 0xF3, (offset >> 8) as u8, (offset & 0xFF) as u8, 0];
            frame[4] = xor_all(&[0x6E ^ addr, frame[0], frame[1], frame[2], frame[3]]);
            let _ = self.write_i2c(service, chip, addr as u32, &frame);
            out.push(self.read_i2c_len(service, chip, addr as u32, len));
        }
        unsafe { CFRelease(service) };
        out
    }

    /// 写一次 VCP，返回 DCP 是否接受事务（不代表显示器执行）。
    pub fn write_vcp(&self, display: &Display, code: u8, value: u16) -> bool {
        let Some(service) = self.av_service(display) else {
            return false;
        };
        let mut ok = false;
        for (addr, chip) in [(INPUT_ADDR, CHIP_ADDR), (INPUT_ADDR, CHIP_ADDR_MCDP)] {
            let frame = write_frame(code, value, addr);
            if self.write_i2c(service, chip, addr as u32, &frame) == 0 {
                ok = true;
                break;
            }
        }
        unsafe { CFRelease(service) };
        ok
    }

    /// 抓一次原始应答，给诊断命令用。
    pub fn raw_read(&self, display: &Display, code: u8) -> Vec<RawReply> {
        let Some(service) = self.av_service(display) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (chip, addr) in [
            (CHIP_ADDR, INPUT_ADDR),
            (CHIP_ADDR, INPUT_ADDR_ALT),
            (CHIP_ADDR_MCDP, INPUT_ADDR),
        ] {
            // 先显式发一次读请求，再读应答：部分 DCP 固件需要这样触发。
            let _ = self.write_i2c(service, chip, addr as u32, &read_frame(code, addr));
            out.push(self.read_i2c(service, chip, addr as u32));
            // 再试一次"只读不请求"的路径，m1ddc 用的就是这一条。
            out.push(self.read_i2c(service, chip, addr as u32));
        }
        unsafe { CFRelease(service) };
        out
    }

    /// 合法应答的判据。
    ///
    /// 不能用 DDC/CI 规范的 XOR 校验和：实测这台 P275MV PLUS 回的
    /// `6e 88 02 00 60 00 00 12 00 0f c9 3b` 里 checksum 与任何标准算法都对不上，
    /// 但结构本身是自洽的。真正可靠的三条：目的地址是显示器、opcode 是
    /// Get VCP Feature Reply、回显的 VCP 码就是请求的那个。
    ///
    /// 偏移说明：`[6..7]` 是最大值、`[8..9]` 是当前值（大端），这与 m1ddc 一致。
    fn parse_reply(reply: &RawReply, code: u8) -> Option<u16> {
        let b = &reply.bytes;
        if b.len() < REPLY_LEN || b.iter().all(|x| *x == 0) {
            return None;
        }
        if b[0] != 0x6E || b[2] != 0x02 || b[4] != code {
            return None;
        }
        Some(((b[8] as u16) << 8) | b[9] as u16)
    }
    /// 读一页能力表，返回其中的 32 字节正文。
    ///
    /// 应答头是 `6E A3 E3 <off_hi> <off_lo>`，正文从第 5 字节起。
    /// 注意这台显示器的固件每页实际吐 38 字节而 offset 只按 32 递增，
    /// 直接首尾相接会错位 6 字节 —— 所以调用方按页独立解析、合并结果，
    /// 不做字符串拼接。
    fn caps_page(&self, service: *mut c_void, offset: u16) -> Option<Vec<u8>> {
        for (chip, addr) in [
            (CHIP_ADDR, INPUT_ADDR_ALT),
            (CHIP_ADDR, INPUT_ADDR),
            (CHIP_ADDR_MCDP, INPUT_ADDR),
        ] {
            let mut frame = [0x83u8, 0xF3, (offset >> 8) as u8, (offset & 0xFF) as u8, 0];
            frame[4] = xor_all(&[0x6E ^ addr, frame[0], frame[1], frame[2], frame[3]]);
            let _ = self.write_i2c(service, chip, addr as u32, &frame);
            let reply = self.read_i2c_len(service, chip, addr as u32, REPLY_LEN + 32);
            let b = &reply.bytes;
            if reply.rc == 0 && b.len() >= 37 && b[0] == 0x6E && b[2] == 0xE3 {
                return Some(b[5..37].to_vec());
            }
        }
        None
    }

    /// 读 128 字节 EDID。I2C 地址 0x50 是 EDID EEPROM，offset 直接是字节偏移。
    /// 分 4 段读：一次读 128 字节有的 DCP 固件会拒绝。
    pub fn read_edid_raw(&self, display: &Display) -> Option<Vec<u8>> {
        let service = self.av_service(display)?;
        let mut out = Vec::with_capacity(128);
        for block in 0..4u32 {
            let reply = self.read_i2c_len(service, EDID_ADDR, block * 32, 32);
            if reply.rc != 0 || reply.bytes.iter().all(|b| *b == 0) {
                break;
            }
            out.extend_from_slice(&reply.bytes);
        }
        unsafe { CFRelease(service) };
        if out.len() >= 128 {
            Some(out)
        } else {
            None
        }
    }
}

impl Backend for MacBackend {
    fn platform(&self) -> &'static str {
        "darwin"
    }

    fn list_displays(&self) -> Vec<Display> {
        let match_ptr =
            unsafe { IOServiceMatching(b"DCPAVServiceProxy\0".as_ptr() as *const c_char) };
        let mut iter: IoIterator = 0;
        let mut services = Vec::new();
        unsafe {
            if IOServiceGetMatchingServices(0, match_ptr, &mut iter) == 0 {
                loop {
                    let svc = IOIteratorNext(iter);
                    if svc == 0 {
                        break;
                    }
                    services.push(svc);
                }
                IOObjectRelease(iter);
            }
        }

        let mut candidates = Vec::new();
        for root in services {
            collect_external(root, 0, &mut candidates);
        }

        let names = profiler_names();
        candidates
            .iter()
            .enumerate()
            .map(|(idx, &entry)| Display {
                uid: format!("iokit:{:x}", registry_id(entry)),
                name: names
                    .get(idx)
                    .cloned()
                    .unwrap_or_else(|| format!("外接显示器 #{}", idx + 1)),
                platform: "darwin",
                is_main: candidates.len() == 1,
                handle: entry as u64,
            })
            .collect()
    }

    fn get_vcp(&self, display: &Display, code: u8) -> Option<u16> {
        let Some(service) = self.av_service(display) else {
            return None;
        };
        let mut last: Option<RawReply> = None;
        let mut result = None;

        // 0x50 优先：这台机器的 DCP 在 0x50 上回的帧是干净的，
        // 0x51 会把写请求的前几个字节混进应答头部。
        for (chip, addr) in [
            (CHIP_ADDR, INPUT_ADDR_ALT),
            (CHIP_ADDR, INPUT_ADDR),
            (CHIP_ADDR_MCDP, INPUT_ADDR),
        ] {
            // 必须显式发一次 Get VCP Feature 请求帧：本机的 DCP 不会替我们发。
            let _ = self.write_i2c(service, chip, addr as u32, &read_frame(code, addr));
            let reply = self.read_i2c(service, chip, addr as u32);
            if let Some(cur) = Self::parse_reply(&reply, code) {
                result = Some(cur);
                last = Some(reply);
                break;
            }
            last = Some(reply);
        }

        unsafe { CFRelease(service) };

        if result.is_none() {
            let note = match &last {
                Some(r) => {
                    if r.bytes.iter().all(|b| *b == 0) {
                        format!("DDC 读无应答（chip 0x{:02x} addr 0x{:02x} 返回空缓冲）", r.chip, r.addr)
                    } else {
                        format!(
                            "DDC 应答校验未通过（chip 0x{:02x} addr 0x{:02x} rc={}）：{}",
                            r.chip,
                            r.addr,
                            r.rc,
                            hex(&r.bytes)
                        )
                    }
                }
                None => "无法打开 DCP AV 服务".to_string(),
            };
            *self.note.borrow_mut() = note;
        } else {
            self.note.borrow_mut().clear();
        }
        result
    }

    fn set_vcp(&self, display: &Display, code: u8, value: u16) -> bool {
        self.write_vcp(display, code, value)
    }

    fn last_note(&self) -> String {
        self.note.borrow().clone()
    }

    /// 能读到亮度就说明 DDC 双向通道都通；读不到就退回"写一次看 DCP 是否接受"。
    fn transport_ok(&self, display: &Display) -> bool {
        if self.get_vcp(display, crate::models::VCP_BRIGHTNESS).is_some() {
            return true;
        }
        let Some(service) = self.av_service(display) else {
            return false;
        };
        let frame = write_frame(crate::models::VCP_BRIGHTNESS, 50, INPUT_ADDR);
        let ok = self.write_i2c(service, CHIP_ADDR, INPUT_ADDR as u32, &frame) == 0;
        unsafe { CFRelease(service) };
        ok
    }

    fn read_edid(&self, display: &Display) -> Option<Vec<u8>> {
        self.read_edid_raw(display)
    }

    /// 逐页读能力表，每页独立解析后合并。
    ///
    /// 不做字符串首尾拼接：厂商固件每页实际吐的字节数和 offset 步进对不上，
    /// 拼起来会错位。只要某一页完整包含 `60(...)` 就能拿到输入源列表。
    fn capabilities(&self, display: &Display) -> Option<crate::caps::Caps> {
        let service = self.av_service(display)?;
        let mut text = String::new();
        let mut inputs: Vec<u16> = Vec::new();
        let mut vcp_codes: Vec<u16> = Vec::new();
        let mut model: Option<String> = None;
        let mut mccs: Option<String> = None;
        let mut pages = 0;

        for page in 0..8u16 {
            let offset = page * 32;
            if offset > 0xFF {
                break;
            }
            let Some(data) = self.caps_page(service, offset) else {
                break;
            };
            if data.iter().all(|b| *b == 0) {
                break;
            }
            pages += 1;
            let chunk = String::from_utf8_lossy(&data).to_string();
            text.push_str(&chunk);
            let parsed = crate::caps::parse(&chunk);
            inputs.extend(parsed.inputs);
            vcp_codes.extend(parsed.vcp_codes);
            if model.is_none() {
                model = parsed.model;
            }
            if mccs.is_none() {
                mccs = parsed.mccs_version;
            }
        }
        unsafe { CFRelease(service) };

        if pages == 0 {
            return None;
        }
        inputs.sort_unstable();
        inputs.dedup();
        vcp_codes.sort_unstable();
        vcp_codes.dedup();
        Some(crate::caps::Caps {
            raw: text.trim().to_string(),
            inputs,
            model,
            mccs_version: mccs,
            vcp_codes,
        })
    }

    fn display_present(&self, display: &Display) -> bool {
        let match_ptr =
            unsafe { IOServiceMatching(b"DCPAVServiceProxy\0".as_ptr() as *const c_char) };
        let mut iter: IoIterator = 0;
        let mut services = Vec::new();
        unsafe {
            if IOServiceGetMatchingServices(0, match_ptr, &mut iter) != 0 {
                return false;
            }
            loop {
                let svc = IOIteratorNext(iter);
                if svc == 0 {
                    break;
                }
                services.push(svc);
            }
            IOObjectRelease(iter);
        }
        let mut candidates = Vec::new();
        for root in services {
            collect_external(root, 0, &mut candidates);
        }
        candidates
            .iter()
            .any(|&e| format!("iokit:{:x}", registry_id(e)) == display.uid)
    }
}
