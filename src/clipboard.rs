//! 剪贴板同步（P0，仅文本）。
//!
//! 两端的取数方式不同但语义一致：一个"变更计数" + 按需取文本。
//! - macOS：`NSPasteboard.changeCount` + `stringForType:@"public.utf8-plain-text"`
//! - Windows：`GetClipboardSequenceNumber` + `CF_UNICODETEXT`
//!
//! 为什么不监听事件而是轮询：事件监听在 macOS 要跑一个 runloop 源，
//! 在 Windows 要一个 `HWND` 接 `WM_CLIPBOARDUPDATE`，两套都得跟 tao 的主循环
//! 打交道。500ms 轮询一次计数器几乎不花 CPU，代码少一半，够用。
//!
//! ⚠️ macOS 的 NSPasteboard 必须在主线程碰。所以这个模块只在 UI 线程调用，
//! 后台线程要写剪贴板时通过事件绕回主线程。

use std::hash::{Hash, Hasher};

/// 记录"上一次看到的内容"，用来判断是不是真的有新东西。
#[derive(Default)]
pub struct Watcher {
    seq: u64,
    hash: u64,
}

impl Watcher {
    /// 本机剪贴板变了就返回新文本。第一次调用只做基线，返回 None。
    pub fn poll(&mut self) -> Option<String> {
        let seq = sequence();
        if seq == self.seq {
            return None;
        }
        self.seq = seq;
        let text = read_text()?;
        let hash = hash_of(&text);
        if hash == self.hash {
            return None;
        }
        self.hash = hash;
        if text.trim().is_empty() {
            return None;
        }
        Some(text)
    }

    /// 刚把对端的内容写进本机剪贴板，记下来，别又当成"本机新内容"回传。
    pub fn mark_seen(&mut self, text: &str) {
        self.hash = hash_of(text);
        self.seq = sequence();
    }
}

pub fn hash_of(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{c_char, c_void, CStr, CString};
    use std::mem::transmute;

    extern "C" {
        fn objc_getClass(name: *const c_char) -> *mut c_void;
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        fn objc_msgSend();
    }

    type MsgId = unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void;
    type MsgInt = unsafe extern "C" fn(*mut c_void, *mut c_void) -> isize;
    type MsgId1 = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void;
    type MsgId2 =
        unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> *mut c_void;
    type MsgIdInt = unsafe extern "C" fn(*mut c_void, *mut c_void, isize) -> *mut c_void;
    type MsgBool2 =
        unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> i8;
    type MsgIdPtr = unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_char) -> *mut c_void;

    unsafe fn class(name: &str) -> *mut c_void {
        let c = CString::new(name).expect("类名不含 NUL");
        objc_getClass(c.as_ptr())
    }

    unsafe fn sel(name: &str) -> *mut c_void {
        let c = CString::new(name).expect("选择器不含 NUL");
        sel_registerName(c.as_ptr())
    }

    unsafe fn nsstring(text: &str) -> *mut c_void {
        // 必须过 CString：&str 不保证结尾有 NUL，直接传指针会读到别的内存里去
        let c = CString::new(text).unwrap_or_else(|_| CString::new("").expect("空串一定合法"));
        let cls = class("NSString");
        let f: MsgIdPtr = transmute(objc_msgSend as unsafe extern "C" fn());
        f(cls, sel("stringWithUTF8String:"), c.as_ptr())
    }

    unsafe fn pasteboard() -> *mut c_void {
        let f: MsgId = transmute(objc_msgSend as unsafe extern "C" fn());
        f(class("NSPasteboard"), sel("generalPasteboard"))
    }

    pub fn sequence() -> u64 {
        unsafe {
            let f: MsgInt = transmute(objc_msgSend as unsafe extern "C" fn());
            f(pasteboard(), sel("changeCount")) as u64
        }
    }

    pub fn read_text() -> Option<String> {
        unsafe {
            let pb = pasteboard();
            if pb.is_null() {
                return None;
            }
            // 别用 stringForType: —— 剪贴板被并发改写时它会在 AppKit 内部
            // （_updateTypeCacheIfNeeded）直接崩（实测 SIGSEGV）。
            // readObjectsForClasses:options: 是官方推荐路径，内部自己处理竞态。
            let classes: *mut c_void = {
                let f: MsgId1 = transmute(objc_msgSend as unsafe extern "C" fn());
                f(class("NSArray"), sel("arrayWithObject:"), class("NSString"))
            };
            if classes.is_null() {
                return None;
            }
            let f: MsgId2 = transmute(objc_msgSend as unsafe extern "C" fn());
            let arr = f(
                pb,
                sel("readObjectsForClasses:options:"),
                classes,
                std::ptr::null_mut(),
            );
            if arr.is_null() {
                return None;
            }
            let f: MsgInt = transmute(objc_msgSend as unsafe extern "C" fn());
            if f(arr, sel("count")) < 1 {
                return None;
            }
            let f: MsgIdInt = transmute(objc_msgSend as unsafe extern "C" fn());
            let value = f(arr, sel("objectAtIndex:"), 0);
            if value.is_null() {
                return None;
            }
            let f: MsgId = transmute(objc_msgSend as unsafe extern "C" fn());
            let raw = f(value, sel("UTF8String")) as *const c_char;
            if raw.is_null() {
                return None;
            }
            Some(CStr::from_ptr(raw).to_string_lossy().into_owned())
        }
    }

    pub fn write_text(text: &str) -> bool {
        unsafe {
            let pb = pasteboard();
            let f: MsgInt = transmute(objc_msgSend as unsafe extern "C" fn());
            f(pb, sel("clearContents"));
            let kind = nsstring("public.utf8-plain-text");
            let value = nsstring(text);
            let f: MsgBool2 = transmute(objc_msgSend as unsafe extern "C" fn());
            f(pb, sel("setString:forType:"), value, kind) != 0
        }
    }
}

#[cfg(windows)]
mod imp {
    const CF_UNICODETEXT: u32 = 13;
    const GMEM_MOVEABLE: u32 = 0x0002;

    extern "system" {
        fn OpenClipboard(owner: isize) -> i32;
        fn CloseClipboard() -> i32;
        fn EmptyClipboard() -> i32;
        fn GetClipboardData(format: u32) -> isize;
        fn SetClipboardData(format: u32, mem: isize) -> isize;
        fn GetClipboardSequenceNumber() -> u32;
        fn GlobalAlloc(flags: u32, bytes: usize) -> isize;
        fn GlobalLock(mem: isize) -> *mut u8;
        fn GlobalUnlock(mem: isize) -> i32;
        fn GlobalFree(mem: isize) -> isize;
    }

    /// 剪贴板是全局独占资源，别的程序正好开着就会失败。
    /// 不重试的话会漏掉事件，重试 5 次（50ms）能覆盖绝大多数冲突。
    fn open() -> bool {
        for _ in 0..5 {
            if unsafe { OpenClipboard(0) } != 0 {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    pub fn sequence() -> u64 {
        unsafe { GetClipboardSequenceNumber() as u64 }
    }

    pub fn read_text() -> Option<String> {
        if !open() {
            return None;
        }
        let handle = unsafe { GetClipboardData(CF_UNICODETEXT) };
        if handle == 0 {
            unsafe { CloseClipboard() };
            return None;
        }
        let ptr = unsafe { GlobalLock(handle) } as *const u16;
        if ptr.is_null() {
            unsafe { CloseClipboard() };
            return None;
        }
        let mut len = 0usize;
        // 上限防呆：真遇到坏数据也不至于把内存读穿
        while len < 1 << 24 && unsafe { *ptr.add(len) } != 0 {
            len += 1;
        }
        let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
        let text = String::from_utf16_lossy(slice);
        unsafe {
            GlobalUnlock(handle);
            CloseClipboard();
        }
        Some(text)
    }

    pub fn write_text(text: &str) -> bool {
        let utf16: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = utf16.len() * 2;
        unsafe {
            let handle = GlobalAlloc(GMEM_MOVEABLE, bytes);
            if handle == 0 {
                return false;
            }
            let dst = GlobalLock(handle) as *mut u16;
            if dst.is_null() {
                GlobalFree(handle);
                return false;
            }
            std::ptr::copy_nonoverlapping(utf16.as_ptr(), dst, utf16.len());
            GlobalUnlock(handle);

            if !open() {
                GlobalFree(handle);
                return false;
            }
            EmptyClipboard();
            // 成功之后内存归系统所有，不能再 free
            let ok = SetClipboardData(CF_UNICODETEXT, handle) != 0;
            CloseClipboard();
            if !ok {
                GlobalFree(handle);
            }
            ok
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    pub fn sequence() -> u64 {
        0
    }
    pub fn read_text() -> Option<String> {
        // Linux 上先用 xclip 兜底，装了就有，没装就静默降级
        let out = std::process::Command::new("xclip")
            .args(["-selection", "clipboard", "-o"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8(out.stdout).ok()
    }
    pub fn write_text(text: &str) -> bool {
        use std::io::Write;
        let Ok(mut child) = std::process::Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(std::process::Stdio::piped())
            .spawn()
        else {
            return false;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        child.wait().map(|s| s.success()).unwrap_or(false)
    }
}

pub use imp::{read_text, sequence, write_text};

#[cfg(test)]
mod tests {
    use super::*;

    /// 真机跑一遍：手写 objc_msgSend 的类型签名错一个字节就是崩溃或乱码，
    /// 所以这个测试的价值不在断言，而在于"跑完没崩"。
    #[test]
    fn text_roundtrip_keeps_original() {
        let original = read_text();
        let probe = format!("inputswitch-clip-{}", std::process::id());
        assert!(write_text(&probe), "写剪贴板失败");
        assert_eq!(read_text().as_deref(), Some(probe.as_str()));
        match original {
            Some(text) => {
                write_text(&text);
            }
            None => {
                write_text("");
            }
        }
    }

    #[test]
    fn watcher_reports_change_once() {
        let mut watcher = Watcher::default();
        let _ = watcher.poll(); // 基线
        let probe = format!("isw-watcher-{}", std::process::id());
        write_text(&probe);
        assert_eq!(watcher.poll().as_deref(), Some(probe.as_str()));
        assert!(watcher.poll().is_none(), "同一内容不该报两次");
    }
}
