//! 剪贴板读取与写入：文本 + 文件列表。
//!
//! 两端的取数方式不同但语义一致：一个"变更计数" + 按需取内容。
//! - macOS：`NSPasteboard.changeCount` + `readObjectsForClasses:`
//! - Windows：`GetClipboardSequenceNumber` + `CF_HDROP` / `CF_UNICODETEXT`
//!
//! 为什么不监听事件而是轮询：事件监听在 macOS 要跑一个 runloop 源，
//! 在 Windows 要一个 `HWND` 接 `WM_CLIPBOARDUPDATE`，两套都得跟 tao 的主循环
//! 打交道。400ms 轮询一次计数器几乎不花 CPU，代码少一半，够用。
//!
//! ⚠️ macOS 的 NSPasteboard 必须在主线程碰。所以这个模块只在 UI 线程调用，
//! 后台线程要写剪贴板时通过事件绕回主线程。
//!
//! 文件列表只列"有哪些路径"，不读内容 —— 内容什么时候传由上层决定
//! （v0.6.39 起：用户真的按下粘贴键时才传）。

use std::hash::{Hash, Hasher};
use std::path::PathBuf;

/// 剪贴板里的一次内容。文本走即时同步，文件列表要等用户按粘贴才传。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipItem {
    Text(String),
    Files(Vec<PathBuf>),
}

impl ClipItem {
    pub fn is_empty(&self) -> bool {
        match self {
            ClipItem::Text(t) => t.trim().is_empty(),
            ClipItem::Files(v) => v.is_empty(),
        }
    }

    /// 给日志/气泡用的一句话摘要
    pub fn summary(&self) -> String {
        match self {
            ClipItem::Text(t) => format!("{} 字", t.chars().count()),
            ClipItem::Files(v) => format!("{} 个文件", v.len()),
        }
    }

    fn hash(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        match self {
            ClipItem::Text(t) => {
                0u8.hash(&mut hasher);
                t.hash(&mut hasher);
            }
            ClipItem::Files(v) => {
                1u8.hash(&mut hasher);
                for p in v {
                    p.hash(&mut hasher);
                }
            }
        }
        hasher.finish()
    }
}

/// 记录"上一次看到的内容"，用来判断是不是真的有新东西。
#[derive(Default)]
pub struct Watcher {
    seq: u64,
    hash: u64,
}

impl Watcher {
    /// 本机剪贴板变了就返回新内容。第一次调用只做基线，返回 None。
    pub fn poll(&mut self) -> Option<ClipItem> {
        let seq = sequence();
        if seq == self.seq {
            return None;
        }
        self.seq = seq;
        let item = read_item()?;
        let hash = item.hash();
        if hash == self.hash {
            return None;
        }
        self.hash = hash;
        if item.is_empty() {
            return None;
        }
        Some(item)
    }

    /// 刚把对端的内容写进本机剪贴板，记下来，别又当成"本机新内容"回传。
    pub fn mark_seen(&mut self, text: &str) {
        self.hash = ClipItem::Text(text.to_string()).hash();
        self.seq = sequence();
    }

    /// 刚写入了非文本内容（文件列表 / 图片）后的防回环。
    /// 只记 seq 就够：下一次 poll 看到 seq 没变就不会读。
    pub fn mark_written(&mut self) {
        self.seq = sequence();
        self.hash = 0;
    }
}

pub fn hash_of(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

#[cfg(target_os = "macos")]
mod imp {
    use super::ClipItem;
    use std::ffi::{c_char, c_void, CStr, CString};
    use std::mem::transmute;
    use std::path::PathBuf;

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
    type MsgBool1 = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> i8;
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

    /// 用 `readObjectsForClasses:options:` 取指定类的一组对象再逐个转字符串。
    ///
    /// 别用 `stringForType:` —— 剪贴板被并发改写时它会在 AppKit 内部
    /// （`_updateTypeCacheIfNeeded`）直接崩（实测 SIGSEGV）。
    unsafe fn objects_of_class(
        pb: *mut c_void,
        cls_name: &str,
        opts: *mut c_void,
    ) -> Vec<*mut c_void> {
        let classes: *mut c_void = {
            let f: MsgId1 = transmute(objc_msgSend as unsafe extern "C" fn());
            f(class("NSArray"), sel("arrayWithObject:"), class(cls_name))
        };
        if classes.is_null() {
            return Vec::new();
        }
        let f: MsgId2 = transmute(objc_msgSend as unsafe extern "C" fn());
        let arr = f(pb, sel("readObjectsForClasses:options:"), classes, opts);
        if arr.is_null() {
            return Vec::new();
        }
        let f: MsgInt = transmute(objc_msgSend as unsafe extern "C" fn());
        let n = f(arr, sel("count"));
        if n <= 0 {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(n as usize);
        let f: MsgIdInt = transmute(objc_msgSend as unsafe extern "C" fn());
        for i in 0..n {
            let o = f(arr, sel("objectAtIndex:"), i);
            if !o.is_null() {
                out.push(o);
            }
        }
        out
    }

    /// `[obj UTF8String]` —— obj 本身必须是 NSString
    unsafe fn str_of(obj: *mut c_void) -> Option<String> {
        let f: MsgId = transmute(objc_msgSend as unsafe extern "C" fn());
        let raw = f(obj, sel("UTF8String")) as *const c_char;
        if raw.is_null() {
            return None;
        }
        Some(CStr::from_ptr(raw).to_string_lossy().into_owned())
    }

    /// 先用 `sel_name` 取一个属性（返回 NSString），再取它的 UTF8String。
    ///
    /// 两步不能省：`[url path]` 返回的是 NSString 对象指针，直接当 char*
    /// 读会打印出一串乱码（实测踩过）。
    unsafe fn prop_str(obj: *mut c_void, sel_name: &str) -> Option<String> {
        let f: MsgId = transmute(objc_msgSend as unsafe extern "C" fn());
        let s = f(obj, sel(sel_name));
        if s.is_null() {
            return None;
        }
        str_of(s)
    }

    /// 剪贴板里如果是"文件"（Finder 复制），返回那一组路径。
    unsafe fn read_files(pb: *mut c_void) -> Vec<PathBuf> {
        // options = @{ NSPasteboardURLReadingFileURLsOnly : @YES }
        let yes: *mut c_void = {
            let f: MsgIdInt = transmute(objc_msgSend as unsafe extern "C" fn());
            f(class("NSNumber"), sel("numberWithBool:"), 1)
        };
        let key = nsstring("NSPasteboardURLReadingFileURLsOnly");
        let opts: *mut c_void = {
            let f: MsgId2 = transmute(objc_msgSend as unsafe extern "C" fn());
            f(
                class("NSDictionary"),
                sel("dictionaryWithObject:forKey:"),
                yes,
                key,
            )
        };
        objects_of_class(pb, "NSURL", opts)
            .into_iter()
            .filter_map(|u| prop_str(u, "path"))
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .collect()
    }

    pub fn read_item() -> Option<ClipItem> {
        unsafe {
            let pb = pasteboard();
            if pb.is_null() {
                return None;
            }
            // 先看文件：Finder 复制出来的剪贴板里通常同时带文件名文本，
            // 但"文件"这个信息更完整，优先按文件处理。
            let files = read_files(pb);
            if !files.is_empty() {
                return Some(ClipItem::Files(files));
            }
            let objs = objects_of_class(pb, "NSString", std::ptr::null_mut());
            let text = objs.into_iter().next().and_then(|o| str_of(o))?;
            Some(ClipItem::Text(text))
        }
    }

    pub fn read_text() -> Option<String> {
        match read_item() {
            Some(ClipItem::Text(t)) => Some(t),
            _ => None,
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

    /// 把一组本地路径写成"文件"剪贴板，并附上文件名文本。
    ///
    /// 附文本是为了让粘贴到文本框时也能得到点东西（而不是毫无反应）。
    pub fn write_files(paths: &[PathBuf]) -> bool {
        if paths.is_empty() {
            return false;
        }
        unsafe {
            let pb = pasteboard();
            let f: MsgInt = transmute(objc_msgSend as unsafe extern "C" fn());
            f(pb, sel("clearContents"));

            let arr: *mut c_void = {
                let f: MsgId = transmute(objc_msgSend as unsafe extern "C" fn());
                f(class("NSMutableArray"), sel("array"))
            };
            if arr.is_null() {
                return false;
            }
            let add: MsgId1 = transmute(objc_msgSend as unsafe extern "C" fn());
            for p in paths {
                let s = nsstring(&p.to_string_lossy());
                if s.is_null() {
                    continue;
                }
                let url: *mut c_void = {
                    let f: MsgId1 = transmute(objc_msgSend as unsafe extern "C" fn());
                    f(class("NSURL"), sel("fileURLWithPath:"), s)
                };
                if !url.is_null() {
                    add(arr, sel("addObject:"), url);
                }
            }
            let w: MsgBool1 = transmute(objc_msgSend as unsafe extern "C" fn());
            let ok = w(pb, sel("writeObjects:"), arr) != 0;
            // 不再额外调 setString:forType: 追加文件名文本：
            // 它只作用于"第一个 item"，在 writeObjects 之后调用会把
            // 文件那条唯一的内容替换成纯文本（实测语义不确定），
            // 得不偿失。需要"粘贴到输入框也能出字"的话，P2 用
            // NSPasteboardItem 一次性把两种类型塞进同一个 item。
            ok
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::ClipItem;
    use std::path::PathBuf;

    const CF_UNICODETEXT: u32 = 13;
    const CF_HDROP: u32 = 15;
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

    #[link(name = "shell32")]
    extern "system" {
        fn DragQueryFileW(hdrop: isize, ifile: u32, buf: *mut u16, cch: u32) -> u32;
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

    /// 取出 CF_HDROP 里的路径列表。调用方保证剪贴板已经打开。
    unsafe fn files_locked() -> Vec<PathBuf> {
        let h = GetClipboardData(CF_HDROP);
        if h == 0 {
            return Vec::new();
        }
        let n = DragQueryFileW(h, 0xFFFF_FFFF, std::ptr::null_mut(), 0);
        if n == 0 || n > 100_000 {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            let len = DragQueryFileW(h, i, std::ptr::null_mut(), 0);
            if len == 0 {
                continue;
            }
            let mut buf = vec![0u16; len as usize + 1];
            let got = DragQueryFileW(h, i, buf.as_mut_ptr(), len + 1);
            if got == 0 {
                continue;
            }
            let s = String::from_utf16_lossy(&buf[..got as usize]);
            if !s.is_empty() {
                out.push(PathBuf::from(s));
            }
        }
        out
    }

    /// 取 CF_UNICODETEXT。调用方保证剪贴板已经打开。
    unsafe fn text_locked() -> Option<String> {
        let handle = GetClipboardData(CF_UNICODETEXT);
        if handle == 0 {
            return None;
        }
        let ptr = GlobalLock(handle) as *const u16;
        if ptr.is_null() {
            return None;
        }
        let mut len = 0usize;
        // 上限防呆：真遇到坏数据也不至于把内存读穿
        while len < 1 << 24 && *ptr.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(ptr, len);
        let text = String::from_utf16_lossy(slice);
        GlobalUnlock(handle);
        Some(text)
    }

    pub fn read_item() -> Option<ClipItem> {
        if !open() {
            return None;
        }
        let files = unsafe { files_locked() };
        let item = if !files.is_empty() {
            Some(ClipItem::Files(files))
        } else {
            unsafe { text_locked() }.map(ClipItem::Text)
        };
        unsafe { CloseClipboard() };
        item
    }

    pub fn read_text() -> Option<String> {
        match read_item() {
            Some(ClipItem::Text(t)) => Some(t),
            _ => None,
        }
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

    /// 在一段已经拿到的 `GlobalLock` 内存里写入宽字符串（带结尾 NUL）。
    unsafe fn put_utf16(base: *mut u8, offset: usize, text: &str) {
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        wide.push(0);
        std::ptr::copy_nonoverlapping(
            wide.as_ptr() as *const u8,
            base.add(offset),
            wide.len() * 2,
        );
    }

    /// 把一组本地路径写成"文件"剪贴板（CF_HDROP），并附上文件名文本。
    ///
    /// CF_HDROP 的实际内存布局是 `DROPFILES` 头 + 一串以 NUL 结尾的宽字符路径
    /// + 结尾再多一个 NUL（双 NUL 表示列表结束）。`fWide = TRUE` 表示宽字符。
    pub fn write_files(paths: &[PathBuf]) -> bool {
        if paths.is_empty() {
            return false;
        }
        let mut wide_len = 0usize;
        for p in paths {
            wide_len += p.to_string_lossy().encode_utf16().count() + 1;
        }
        wide_len += 1; // 结尾双 NUL

        let head = 20usize; // sizeof(DROPFILES) on 64-bit: 4 + 8 + 4 + 4
        let bytes = head + wide_len * 2;

        unsafe {
            let handle = GlobalAlloc(GMEM_MOVEABLE, bytes);
            if handle == 0 {
                return false;
            }
            let base = GlobalLock(handle) as *mut u8;
            if base.is_null() {
                GlobalFree(handle);
                return false;
            }
            // DROPFILES { DWORD pFiles; POINT pt; BOOL fNC; BOOL fWide; }
            let head_words = base as *mut u32;
            *head_words = head as u32; // pFiles：文件列表相对结构体起点的偏移
            *head_words.add(1) = 0; // pt.x
            *head_words.add(2) = 0; // pt.y
            *head_words.add(3) = 0; // fNC
            *head_words.add(4) = 1; // fWide = TRUE

            let mut off = head;
            for p in paths {
                let s = p.to_string_lossy();
                put_utf16(base, off, &s);
                off += (s.encode_utf16().count() + 1) * 2;
            }
            *base.add(off) = 0;
            *base.add(off + 1) = 0;
            GlobalUnlock(handle);

            if !open() {
                GlobalFree(handle);
                return false;
            }
            EmptyClipboard();
            let ok = SetClipboardData(CF_HDROP, handle) != 0;
            if !ok {
                GlobalFree(handle);
            }

            // 追加文件名文本兜底
            let names: Vec<String> = paths
                .iter()
                .map(|p| {
                    p.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| p.to_string_lossy().into_owned())
                })
                .collect();
            let joined = names.join("\r\n");
            let utf16: Vec<u16> = joined.encode_utf16().chain(std::iter::once(0)).collect();
            let h2 = GlobalAlloc(GMEM_MOVEABLE, utf16.len() * 2);
            if h2 != 0 {
                let dst = GlobalLock(h2) as *mut u16;
                if dst.is_null() {
                    GlobalFree(h2);
                } else {
                    std::ptr::copy_nonoverlapping(utf16.as_ptr(), dst, utf16.len());
                    GlobalUnlock(h2);
                    if SetClipboardData(CF_UNICODETEXT, h2) == 0 {
                        GlobalFree(h2);
                    }
                }
            }
            CloseClipboard();
            ok
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::ClipItem;

    pub fn sequence() -> u64 {
        0
    }
    pub fn read_item() -> Option<ClipItem> {
        read_text().map(ClipItem::Text)
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
    pub fn write_files(_paths: &[std::path::PathBuf]) -> bool {
        false
    }
}

pub use imp::{read_item, read_text, sequence, write_files, write_text};

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
        match watcher.poll() {
            Some(ClipItem::Text(t)) => assert_eq!(t, probe),
            other => panic!("期望文本，得到 {other:?}"),
        }
        assert!(watcher.poll().is_none(), "同一内容不该报两次");
    }
}
