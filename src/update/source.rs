//! 更新源：候选地址、故障转移、错误分类。
//!
//! ## 只有两级
//!
//! 不是示意图代码，标 text 别让 rustdoc 当 Rust 编译。
//! ```text
//! ghproxy.net  →  github.com 直连
//! ```
//!
//! 没有 Gitee 镜像（2026-10-27 决定）：国内直连 GitHub 的 release 下载经常
//! 抖动，所以默认先走 ghproxy；ghproxy 本身也会挂，于是留一条直连兜底。
//! 实测本机（国内家宽）直连 `api.github.com` 是通的，只是慢——两级刚好。
//!
//! ## 为什么不重试 404
//!
//! `releases/latest/download/updater.json` 是 GitHub 的固定别名，404 就说明
//! 这个 release 里真的没有这个文件（发布脚本漏了）。重试三次只是让用户多等
//! 三秒拿到同一个答案。只有**网络层**失败（DNS/连接/TLS/超时）才值得重试。

use std::time::Duration;

use crate::update::fetch::{self, FetchError};
use crate::update::manifest::{Manifest, ManifestError};

/// GitHub 仓库。清单与安装包都从这里取。
pub const REPO: &str = "kynnzhou/inputswitch";

/// 清单文件名。与 Tauri updater / cc-bridge 生成脚本的默认名一致。
pub const MANIFEST_NAME: &str = "updater.json";

/// ghproxy 前缀。它要求「前缀 + 完整原始 URL」这种拼接形式。
const GH_PROXY: &str = "https://ghproxy.net/";

const GH_DIRECT: &str = "https://github.com";

/// 覆盖用的环境变量：给了就**只**用它，不再追加 ghproxy/GitHub。
///
/// 用途是联调（起一个 `python -m http.server` 或 `fake-update-server.js`），
/// 以及给企业内网自建镜像留一条口子。注意仍然只接受 https（见 fetch）。
pub const ENV_OVERRIDE: &str = "INPUTSWITCH_UPDATE_URL";

/// 网络层最多试几次（含首次）。
const MAX_ATTEMPTS: u32 = 3;

/// 给界面的错误分类。序列化成 snake_case 直接进 JSON。
///
/// 分类存在的意义：`UpdateBadge` 要把「连不上网」和「发布里少了产物」
/// 说成人话，只给一个 `String` 的话前端只能原样展示 curl 的英文报错。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// 清单格式/字段不合法
    Manifest,
    /// DNS、连接被拒、TLS
    Network,
    Timeout,
    /// 4xx/5xx
    Http,
    /// 下载完摘要不符
    Checksum,
    /// 下载前复查发现远端版本已变（清单比 check 时新了）
    Stale,
    /// 安装器执行失败
    Install,
    /// 本机环境（没 curl、没写权限）
    Config,
    /// 清单里没有本平台的产物
    Unsupported,
    Unknown,
}

impl ErrorCode {
    pub(crate) fn from_fetch(e: &FetchError) -> ErrorCode {
        match e {
            FetchError::NoCurl => ErrorCode::Config,
            FetchError::Timeout(_) => ErrorCode::Timeout,
            FetchError::Network(_) => ErrorCode::Network,
            FetchError::Http(_) => ErrorCode::Http,
        }
    }

    pub(crate) fn from_manifest(e: &ManifestError) -> ErrorCode {
        match e {
            ManifestError::NoPlatform(_) => ErrorCode::Unsupported,
            _ => ErrorCode::Manifest,
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ErrorCode::Manifest => "manifest",
            ErrorCode::Network => "network",
            ErrorCode::Timeout => "timeout",
            ErrorCode::Http => "http",
            ErrorCode::Checksum => "checksum",
            ErrorCode::Stale => "stale",
            ErrorCode::Install => "install",
            ErrorCode::Config => "config",
            ErrorCode::Unsupported => "unsupported",
            ErrorCode::Unknown => "unknown",
        };
        f.write_str(s)
    }
}

/// 更新流程里任意一步失败的结果：给用户看的消息 + 机器可读的分类。
///
/// 检查、下载、安装共用这一个类型——它们的消费方（面板、CLI）要做的事
/// 完全相同：把 `message` 显示出来、按 `code` 决定给什么建议。
#[derive(Clone, Debug)]
pub struct UpdateError {
    pub code: ErrorCode,
    /// 中文、可展示、已经翻译过 curl 的报错
    pub message: String,
}

impl UpdateError {
    pub(crate) fn new(code: ErrorCode, message: impl Into<String>) -> UpdateError {
        UpdateError {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}（{}）", self.message, self.code)
    }
}

/// 候选清单地址，按优先级排列。
pub fn manifest_urls() -> Vec<String> {
    if let Ok(v) = std::env::var(ENV_OVERRIDE) {
        let v = v.trim().trim_end_matches('/').to_string();
        if !v.is_empty() {
            return vec![v];
        }
    }
    let direct = format!("{GH_DIRECT}/{REPO}/releases/latest/download/{MANIFEST_NAME}");
    vec![format!("{GH_PROXY}{direct}"), direct]
}

/// 安装包的候选地址，按优先级排列。
///
/// 入参是清单里给的**完整**下载地址（不是文件名）：清单对每个平台写自己的
/// `url`，我们只需要按「同一套两级」把它包一层。只有 github.com 上的地址
/// 才套 ghproxy——清单指向别处（企业自建源）时原样直连，不猜。
///
/// 与 [`manifest_urls`] 同一套顺序：清单能取到、包取不到是很常见的中间状态
/// （ghproxy 缓存了清单但还没缓存包），必须能独立降级。
pub fn asset_urls_from(url: &str) -> Vec<String> {
    let direct = url.trim();
    if direct.starts_with("https://github.com/") {
        vec![format!("{GH_PROXY}{direct}"), direct.to_string()]
    } else {
        vec![direct.to_string()]
    }
}

/// 拉清单：逐候选源、逐次重试，返回第一个可用的。
///
/// 返回 `(清单, 实际生效的源)` 而不是只返回清单——出错时用户最需要知道的
/// 就是「到底哪个源坏了」。
pub fn fetch_manifest(timeout: Duration) -> Result<(Manifest, String), UpdateError> {
    let urls = manifest_urls();
    let mut last: Option<UpdateError> = None;

    for url in &urls {
        for attempt in 0..MAX_ATTEMPTS {
            if attempt > 0 {
                // 1s、2s：列表只有两级，退避不需要更长；再长用户就以为卡死了
                std::thread::sleep(Duration::from_secs(1 << (attempt - 1)));
            }
            match fetch::get_text(url, timeout) {
                Ok(text) => match Manifest::parse(&text) {
                    Ok(m) => return Ok((m, url.clone())),
                    Err(e) => {
                        // 清单内容坏了：换源也没用（同一份文件），直接失败
                        return Err(UpdateError {
                            code: ErrorCode::from_manifest(&e),
                            message: e.to_string(),
                        });
                    }
                },
                Err(e) => {
                    let retry = should_retry(&e);
                    last = Some(UpdateError {
                        code: ErrorCode::from_fetch(&e),
                        message: format!("{url}：{e}"),
                    });
                    if !retry {
                        break; // 换下一个源
                    }
                }
            }
        }
    }

    Err(last.unwrap_or(UpdateError {
        code: ErrorCode::Unknown,
        message: "没有可用的更新源".into(),
    }))
}

/// 只有网络层抖动才值得重试。见模块文档。
fn should_retry(e: &FetchError) -> bool {
    matches!(e, FetchError::Timeout(_) | FetchError::Network(_))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_level_order_is_ghproxy_then_github() {
        // 清掉可能存在的覆盖，确保测的是默认两级
        std::env::remove_var(ENV_OVERRIDE);
        let urls = manifest_urls();
        assert_eq!(urls.len(), 2, "{urls:?}");
        assert!(
            urls[0].starts_with(GH_PROXY),
            "第一级应是 ghproxy：{}",
            urls[0]
        );
        assert!(
            urls[1].starts_with("https://github.com/"),
            "第二级应是直连：{}",
            urls[1]
        );
        // 两级指向同一个逻辑资源
        assert!(urls[0].ends_with(&urls[1]));
        assert!(urls[1].ends_with("/releases/latest/download/updater.json"));
    }

    #[test]
    fn asset_urls_keep_the_same_two_levels() {
        let direct = format!(
            "{GH_DIRECT}/{REPO}/releases/download/v0.6.90/InputSwitch_0.6.90_x64-setup.exe"
        );
        let urls = asset_urls_from(&direct);
        assert_eq!(urls.len(), 2, "{urls:?}");
        assert!(
            urls[0].starts_with(GH_PROXY),
            "第一级应是 ghproxy：{}",
            urls[0]
        );
        assert!(urls[0].ends_with(&direct), "两级要指向同一个包");
        assert_eq!(urls[1], direct, "第二级应是原地址");
    }

    #[test]
    fn non_github_asset_is_not_proxied() {
        // 企业自建源 / 假更新服务器：不该被塞进 ghproxy 前缀里
        let urls = asset_urls_from("https://mirror.example.com/dl/a-setup.exe");
        assert_eq!(
            urls,
            vec!["https://mirror.example.com/dl/a-setup.exe".to_string()]
        );
    }

    #[test]
    fn env_override_replaces_both_levels_and_trims_slash() {
        std::env::set_var(ENV_OVERRIDE, " https://mirror.example.com/updater.json/ ");
        let urls = manifest_urls();
        std::env::remove_var(ENV_OVERRIDE);
        assert_eq!(
            urls,
            vec!["https://mirror.example.com/updater.json".to_string()]
        );
    }

    #[test]
    fn blank_override_falls_back_to_defaults() {
        std::env::set_var(ENV_OVERRIDE, "   ");
        let urls = manifest_urls();
        std::env::remove_var(ENV_OVERRIDE);
        assert_eq!(urls.len(), 2);
    }

    #[test]
    fn only_transient_network_errors_are_retried() {
        assert!(should_retry(&FetchError::Timeout("t".into())));
        assert!(should_retry(&FetchError::Network("n".into())));
        // 404 再试三次也是 404；没 curl 再试也是没有
        assert!(!should_retry(&FetchError::Http("404".into())));
        assert!(!should_retry(&FetchError::NoCurl));
    }

    #[test]
    fn error_codes_map_to_expected_buckets() {
        assert_eq!(
            ErrorCode::from_fetch(&FetchError::NoCurl),
            ErrorCode::Config
        );
        assert_eq!(
            ErrorCode::from_fetch(&FetchError::Timeout("x".into())),
            ErrorCode::Timeout
        );
        assert_eq!(
            ErrorCode::from_fetch(&FetchError::Http("404".into())),
            ErrorCode::Http
        );
        assert_eq!(
            ErrorCode::from_manifest(&ManifestError::NoPlatform("windows-x86_64".into())),
            ErrorCode::Unsupported
        );
        assert_eq!(
            ErrorCode::from_manifest(&ManifestError::MissingVersion),
            ErrorCode::Manifest
        );
    }

    #[test]
    fn error_code_serializes_as_snake_case() {
        let j = serde_json::to_string(&ErrorCode::Unsupported).unwrap();
        assert_eq!(j, "\"unsupported\"");
        assert_eq!(ErrorCode::Checksum.to_string(), "checksum");
    }
}
