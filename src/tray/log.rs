//! 日志落盘 + 本地时间戳。
//!
//! 单独成文件的理由：日志是**跨模块**的（config、kvm、engine、菜单都要写），
//! 而它的四个实现细节（排他写、毫秒、时区、公历换算）没有第二处关心。
//! 混在 tray/mod.rs 里时，每次只想加一行日志都得翻过 600 行 UI 代码。


/// 追加一行日志。托盘应用没有控制台，出问题时用户能把这个文件发给我。
pub fn log_line(message: &str) {
    use std::io::Write;
    // 排他写：钩子线程、收包线程、UI 线程都会打日志，各自 open+append
    // 时两次写会交错成 "[时间[] 时间...文字]" 这种半截行 —— 排障时最
    // 需要的恰恰是时序，混一行就可能读反因果。
    static LOG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = LOG_LOCK.lock().ok();
    let dir = crate::config::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("app.log"))
    {
        let _ = writeln!(file, "[{}] {}", local_stamp(), message);
    }
}

/// 本地时间戳 YYYY-MM-DD HH:MM:SS.mmm，不引第三方日期库。
/// 毫秒是排障刚需：交还/进门这类"差几百毫秒"的手感问题，秒级时间戳
/// 根本看不出谁慢（v0.6.32 起统一带毫秒）。
fn local_stamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64 + local_offset_secs();
    let millis = now.subsec_millis();
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        y,
        m,
        d,
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60,
        millis
    )
}

/// UTC 与本地时区差（秒）。用系统 API 取，避免硬编码夏令时。
fn local_offset_secs() -> i64 {
    #[cfg(unix)]
    {
        // 用 libc 的 localtime_r 读 tm_gmtoff
        #[repr(C)]
        struct Tm {
            tm_sec: i32,
            tm_min: i32,
            tm_hour: i32,
            tm_mday: i32,
            tm_mon: i32,
            tm_year: i32,
            tm_wday: i32,
            tm_yday: i32,
            tm_isdst: i32,
            tm_gmtoff: i64,
            tm_zone: *const std::os::raw::c_char,
        }
        extern "C" {
            fn localtime_r(time: *const i64, result: *mut Tm) -> *mut Tm;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let mut tm = Tm {
            tm_sec: 0,
            tm_min: 0,
            tm_hour: 0,
            tm_mday: 1,
            tm_mon: 0,
            tm_year: 70,
            tm_wday: 0,
            tm_yday: 0,
            tm_isdst: 0,
            tm_gmtoff: 0,
            tm_zone: std::ptr::null(),
        };
        unsafe {
            localtime_r(&now, &mut tm);
        }
        tm.tm_gmtoff
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// Howard Hinnant 的 civil_from_days：把「1970-01-01 起的天数」转成公历。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

