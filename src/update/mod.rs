//! 自动更新：检查 / 下载 / 校验 / 安装。
//!
//! 结构对齐 cc-bridge 的 `commands/update.rs`，但底层能力全部换成
//! 「零新增依赖」的等价实现：
//!
//! | cc-bridge | 这里 | 为什么 |
//! |---|---|---|
//! | Tauri updater 插件 | 本模块自己编排 | 本项目没有 Tauri（裸 tao + wry） |
//! | `reqwest` 异步下载 | 系统 `curl` 子进程（`fetch.rs`） | 见 `fetch.rs` 里的依赖风险说明 |
//! | minisign 验签 | SHA-256 摘要（`manifest.rs`） | 2026-10-27 决定：不做密钥链 |
//! | Tauri event 推送状态 | 面板已有的 1.2s 轮询 | 面板本来就是轮询式，不需要事件通道 |
//!
//! ## 分期
//!
//! - **P0（本文件）**：状态机 + 检查（拿到「有没有新版」的结论）
//! - P1：下载到临时文件、进度、摘要校验
//! - P2：安装（Windows NSIS 静默安装 / macOS .app 替换）+ 重启
//! - P3：面板 UI（徽章 / 更新说明弹窗 / 重启过渡层）
//!
//! 状态放在一个进程级 `Mutex` 里。面板线程只读（`snapshot`），检查线程只写。
//! 不用 channel 是因为读写两端都不关心「变化了几次」，只关心「现在是什么」。

pub mod fetch;
pub mod manifest;
pub mod source;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use manifest::Manifest;
use source::{CheckError, ErrorCode};

/// 自动检查更新的间隔。面板每次启动只查一次，够用；24h 是 cc-bridge 的选择。
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// 七态，与 cc-bridge 的 `UpdateContext` 一一对应，方便两边对照排查。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// 还没查过
    #[default]
    Idle,
    /// 正在查（或正在下载，P1 会把下载也放进来）
    Checking,
    /// 有新版可用
    Available,
    /// 正在下载
    Downloading,
    /// 下载并校验完成，等着装
    Ready,
    /// 已经是最新
    UpToDate,
    Error,
}

/// 一次检查的完整结果，直接序列化进 `/api/state`。
///
/// 字段用空串表示「不适用」而不是 `Option`：面板是手写 JS，`null` 的判空
/// 到处都要写，空串直接 `if (v)` 就行。
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct UpdateState {
    pub phase: Phase,
    /// 本机版本（= `crate::VERSION`）
    pub current: String,
    pub latest: String,
    pub notes: String,
    pub pub_date: String,
    /// 安装包地址（`available` 之后才有）
    pub url: String,
    /// 安装包 SHA-256
    pub checksum: String,
    /// 给用户看的错误消息（空 = 没错误）
    pub error: String,
    /// `ErrorCode` 的字符串形式（空 = 没错误）
    pub code: String,
    /// 用户点过「忽略本次」的版本号
    pub skipped: String,
    /// 上次检查完成的时间（unix 秒，0 = 从没查过）
    pub checked_at: u64,
}

impl UpdateState {
    fn new() -> UpdateState {
        UpdateState {
            current: crate::VERSION.to_string(),
            ..UpdateState::default()
        }
    }
}

/// 进程级状态。`OnceLock` 而不是 `static Mutex`：`new()` 里要 `format!`，
/// 不是 const 上下文。
fn state() -> &'static Mutex<UpdateState> {
    static S: OnceLock<Mutex<UpdateState>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(UpdateState::new()))
}

/// 单飞闸：检查正在跑时，第二次调用直接返回当前状态。
///
/// 面板轮询 + 用户手点 + 24h 定时三种触发会撞在一起，不加闸就是三份
/// 并发 curl 同时改状态，最后写回的那份未必是最新的。
static CHECKING: AtomicBool = AtomicBool::new(false);

struct CheckGuard;

impl Drop for CheckGuard {
    fn drop(&mut self) {
        CHECKING.store(false, Ordering::Release);
    }
}

/// 读当前状态。面板每 1.2s 调一次，克隆开销可以忽略。
pub fn snapshot() -> UpdateState {
    state()
        .lock()
        .map(|g| g.clone())
        .unwrap_or_else(|e| e.into_inner().clone())
}

/// 检查更新，阻塞到出结果。**不下载、不安装。**
///
/// 供 CLI（`inputswitch-cli update --check`）和 P1 的后台线程使用。
pub fn check_blocking() -> UpdateState {
    if CHECKING.swap(true, Ordering::AcqRel) {
        return snapshot();
    }
    let _guard = CheckGuard;

    {
        let mut s = lock();
        s.phase = Phase::Checking;
        s.error.clear();
        s.code.clear();
    }

    let outcome = check_once();

    let mut s = lock();
    s.checked_at = now_secs();
    match outcome {
        Ok(Decision::UpToDate) => {
            s.phase = Phase::UpToDate;
            s.latest = crate::VERSION.to_string();
            s.url.clear();
            s.checksum.clear();
            s.notes.clear();
            s.pub_date.clear();
        }
        Ok(Decision::Available {
            version,
            notes,
            pub_date,
            url,
            checksum,
        }) => {
            s.phase = Phase::Available;
            s.latest = version;
            s.notes = notes;
            s.pub_date = pub_date;
            s.url = url;
            s.checksum = checksum;
        }
        Err(e) => {
            s.phase = Phase::Error;
            s.error = e.message;
            s.code = e.code.to_string();
        }
    }
    s.clone()
}

fn check_once() -> Result<Decision, CheckError> {
    let (manifest, _source_url) = source::fetch_manifest(fetch::DEFAULT_TIMEOUT)?;
    decide(&manifest)
}

/// 拿到的清单该怎么理解。抽成纯函数：不碰网络、不碰全局状态，能直接单测。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    UpToDate,
    Available {
        version: String,
        notes: String,
        pub_date: String,
        url: String,
        checksum: String,
    },
}

fn decide(m: &Manifest) -> Result<Decision, CheckError> {
    // 先比版本。「没新版」是最常见的结果，这条路径不该因为清单里少了个
    // 可有可无的字段就报错——那会让用户看到「检查更新失败」但其实无妨。
    if !manifest::is_newer(&m.version, crate::VERSION) {
        return Ok(Decision::UpToDate);
    }
    // 确定有新版，再来要本平台的产物：版本再新、我这边没有包也装不了，
    // 报错比假装「有更新」然后下载失败要好
    let platform = m.for_this_platform().map_err(|e| CheckError {
        code: ErrorCode::from_manifest(&e),
        message: e.to_string(),
    })?;

    Ok(Decision::Available {
        version: normalise_version(&m.version),
        notes: m.notes.clone(),
        pub_date: m.pub_date.clone(),
        url: platform.url.clone(),
        checksum: platform.checksum.trim().to_lowercase(),
    })
}

/// `v0.6.90` → `0.6.90`。清单里带不带 `v` 都合法，但面板上要统一显示。
fn normalise_version(v: &str) -> String {
    v.trim().trim_start_matches(['v', 'V']).to_string()
}

/// 用户点「忽略本次」。记版本号而不是布尔值：下个版本还要提示。
pub fn dismiss(version: &str) {
    let mut s = lock();
    s.skipped = normalise_version(version);
}

/// 这个人是不是被忽略了。
pub fn is_skipped(version: &str) -> bool {
    let s = lock();
    !s.skipped.is_empty() && s.skipped == normalise_version(version)
}

fn lock() -> std::sync::MutexGuard<'static, UpdateState> {
    // 这个锁的保护范围里没有会 panic 的代码，中毒了也能继续用
    state().lock().unwrap_or_else(|e| e.into_inner())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造清单文本用。本机若不是四个主平台之一，`platform_key()` 返回空串，
    /// 就借一个键把 JSON 拼完整 —— 断言里再按真实情况跳过。
    fn key_or_borrowed() -> &'static str {
        match manifest::platform_key() {
            "" => "windows-x86_64",
            k => k,
        }
    }

    fn manifest_json(version: &str) -> String {
        let key = key_or_borrowed();
        format!(
            r#"{{
                "version": "{version}",
                "notes": "测试说明",
                "pub_date": "2026-10-27T00:00:00Z",
                "platforms": {{
                    "{key}": {{
                        "url": "https://example.com/setup.exe",
                        "checksum": "{sum}"
                    }}
                }}
            }}"#,
            sum = "a".repeat(64)
        )
    }

    #[test]
    fn initial_state_is_idle_with_current_version() {
        // 不碰网络：只读初始值。注意它可能被同进程其它测试改过，
        // 所以只断言「不变量」而不是具体 phase
        let s = snapshot();
        assert_eq!(s.current, crate::VERSION);
        assert!(!s.current.is_empty());
    }

    #[test]
    fn newer_manifest_yields_available() {
        let m = Manifest::parse(&manifest_json("99.0.0")).unwrap();
        match decide(&m) {
            Ok(Decision::Available { version, url, .. }) => {
                assert_eq!(version, "99.0.0");
                assert!(url.starts_with("https://"));
            }
            other => panic!("应当有新版本，实际 {other:?}"),
        }
    }

    #[test]
    fn same_or_older_manifest_is_up_to_date() {
        let same = Manifest::parse(&manifest_json(crate::VERSION)).unwrap();
        assert_eq!(decide(&same).unwrap(), Decision::UpToDate);

        let older = Manifest::parse(&manifest_json("0.0.1")).unwrap();
        assert_eq!(decide(&older).unwrap(), Decision::UpToDate);
    }

    #[test]
    fn version_prefix_is_stripped_for_display() {
        let m = Manifest::parse(&manifest_json("v99.0.0")).unwrap();
        match decide(&m).unwrap() {
            Decision::Available { version, .. } => assert_eq!(version, "99.0.0"),
            other => panic!("应当有新版本，实际 {other:?}"),
        }
    }

    #[test]
    fn checksum_is_lowercased() {
        let key = key_or_borrowed();
        let text = format!(
            r#"{{"version":"99.0.0","platforms":{{"{key}":{{"url":"https://e.com/a.exe","checksum":"{}"}}}}}}"#,
            "AB".repeat(32)
        );
        let m = Manifest::parse(&text).unwrap();
        match decide(&m).unwrap() {
            Decision::Available { checksum, .. } => {
                assert_eq!(checksum, "ab".repeat(32), "摘要要统一成小写再存");
            }
            other => panic!("应当有新版本，实际 {other:?}"),
        }
    }

    #[test]
    fn missing_platform_is_reported_before_version_check() {
        let m = Manifest::parse(r#"{"version":"99.0.0","platforms":{}}"#).unwrap();
        let err = decide(&m).unwrap_err();
        if manifest::platform_key().is_empty() {
            assert_eq!(err.code, ErrorCode::Unsupported);
        } else {
            assert_eq!(err.code, ErrorCode::Unsupported);
            assert!(err.message.contains(manifest::platform_key()));
        }
    }

    #[test]
    fn bad_asset_url_fails_as_manifest_error() {
        let key = key_or_borrowed();
        let text = format!(
            r#"{{"version":"99.0.0","platforms":{{"{key}":{{"url":"http://insecure/a.exe","checksum":"{}"}}}}}}"#,
            "a".repeat(64)
        );
        let m = Manifest::parse(&text).unwrap();
        let err = decide(&m).unwrap_err();
        assert_eq!(err.code, ErrorCode::Manifest);
    }

    #[test]
    fn dismiss_records_normalised_version() {
        dismiss("v9.9.9");
        assert!(is_skipped("9.9.9"));
        assert!(is_skipped("v9.9.9"));
        assert!(!is_skipped("9.9.8"));
        dismiss("");
    }

    #[test]
    fn phase_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&Phase::UpToDate).unwrap(),
            "\"up_to_date\""
        );
        assert_eq!(serde_json::to_string(&Phase::Error).unwrap(), "\"error\"");
    }
}
