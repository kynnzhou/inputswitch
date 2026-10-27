//! 网络层：只负责「把一个 https URL 变成字节」。
//!
//! ## 为什么调系统 curl，而不是引一个 HTTP 库
//!
//! 1. **依赖风险**：仓库现在零 HTTPS 依赖，网络层是裸 UDP。引 `ureq`/`reqwest`
//!    会顺着 `rustls` 拉进加密后端，当前默认是 `aws-lc-rs`——它需要 C 工具链，
//!    我们「交叉编译到 Windows 不需要 C 工具链」的前提当场就破了。
//! 2. **能力够用**：Windows 10 1803+ 和 macOS 都自带 `curl`，HTTPS（走系统
//!    证书库）、代理环境变量、断点续传、进度输出、超时、跟随重定向全都有。
//! 3. **体积零增长**：本项目 release 已经 `opt-level="z"` + `lto`，加一套 TLS
//!    栈至少几百 KB，而这个功能一年跑不了几次。
//!
//! 代价是依赖系统里的 curl：找不到时明确报 [`FetchError::NoCurl`]，不静默降级
//! （静默失败会让「检查更新永远说已是最新」这种问题极难查）。
//!
//! ## 代理
//!
//! 默认继承 `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY`（curl 自己读）。但开发机上
//! 这类变量经常指向一个**已经关掉的本地代理**（本仓库就长期踩这个坑：
//! `git push` 报 `Failed to connect to github.com:443 over proxy`，而直连是通的）。
//! 所以：第一次失败且检测到代理变量时，绕过代理重试一次。

use std::io::ErrorKind;
use std::process::Command;
use std::time::Duration;

/// 连接超时。DNS 或握手卡住时不要让它拖满整个 `--max-time`。
const CONNECT_TIMEOUT_SECS: u64 = 8;

/// 清单是几 KB 的 JSON，超过这个就认为源不对（防被灌 1 GB 进内存）。
const MAX_TEXT_BYTES: usize = 1024 * 1024;

const USER_AGENT: &str = concat!("InputSwitch/", env!("CARGO_PKG_VERSION"));

/// 检查更新时给清单的默认超时。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// 系统里没有 curl。老系统或精简镜像才会出现，提示用户手动下载。
    NoCurl,
    /// 超过 `--max-time`
    Timeout(String),
    /// DNS、连接被拒、TLS 握手失败、退出码未知
    Network(String),
    /// 服务器返回 4xx/5xx，或者响应体不合规
    Http(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::NoCurl => write!(
                f,
                "系统里没有 curl，无法联网检查更新（Windows 10 1803 以上、macOS 都自带；请手动下载新版本）"
            ),
            FetchError::Timeout(e) => write!(f, "网络超时：{e}"),
            FetchError::Network(e) => write!(f, "网络错误：{e}"),
            FetchError::Http(e) => write!(f, "服务器拒绝：{e}"),
        }
    }
}

impl std::error::Error for FetchError {}

/// 取一个文本资源（清单）。
pub fn get_text(url: &str, timeout: Duration) -> Result<String, FetchError> {
    // 清单是远端给的，不能信；只允许 https，别的一律不碰
    ensure_https(url)?;
    match run(url, timeout, false) {
        Ok(s) => Ok(s),
        Err(proxy_err) => {
            if !has_proxy_env() {
                return Err(proxy_err);
            }
            match run(url, timeout, true) {
                Ok(s) => Ok(s),
                // 两次都失败时报第一次的错：用户看到「代理连不上」才会去
                // 检查代理设置，报「直连超时」反而误导
                Err(_) => Err(proxy_err),
            }
        }
    }
}

/// 只放行 https。清单和安装包地址都是远端给的，`http://` / `file://`
/// 会让「HTTPS + 摘要」这个安全模型直接失效（同一条链路上随便改）。
///
/// 唯一的例外是**单测**里的 `http://127.0.0.1:端口`：真跑一遍下载链路需要
/// 一个能控内容的服务器，给单测起 TLS + 自签证书不值得。`cfg(test)` 只在
/// `cargo test` 时编译进来，正式二进制里根本不存在这条分支。
pub(crate) fn ensure_https(url: &str) -> Result<(), FetchError> {
    if url.starts_with("https://") && url.len() > "https://".len() {
        return Ok(());
    }
    #[cfg(test)]
    if url.starts_with("http://127.0.0.1:") {
        return Ok(());
    }
    Err(FetchError::Http(format!("拒绝非 https 地址：{url}")))
}

/// 构造一条 curl 命令，**不含 URL 本体**（调用方可能还要加 `-o`/`-D`）。
///
/// 抽出来是为了让清单拉取和安装包下载共用同一套策略：超时、UA、证书
/// 吊销、`--max-filesize` 上限。两处各写一套必然漂移——典型事故是只在
/// 其中一处加了 `--ssl-no-revoke`，于是「检查能过、下载报证书错误」。
pub(crate) fn command(timeout: Duration, no_proxy: bool, max_bytes: u64) -> Command {
    let mut cmd = Command::new("curl");
    cmd.arg("-fsSL") // 静默但保留错误、失败时非 0 退出码、跟随重定向
        .arg("--connect-timeout")
        .arg(CONNECT_TIMEOUT_SECS.to_string())
        .arg("--max-time")
        .arg(timeout.as_secs().max(1).to_string())
        .arg("--max-filesize")
        .arg(max_bytes.to_string())
        .arg("-A")
        .arg(USER_AGENT);
    if no_proxy {
        cmd.arg("--noproxy").arg("*");
    }
    // Windows 的 curl 走 Schannel，默认要查证书吊销（CRL/OCSP）。国内网络到
    // 吊销服务器经常不可达，于是握手直接失败、报 0x80092013
    // （CRYPT_E_REVOCATION_OFFLINE）——本机访问 raw.githubusercontent.com 就是
    // 这么挂的，而 github.com 能通。仓库根的 `~/.cargo/config.toml` 也为同一个
    // 原因关掉了 cargo 的 check-revoke。
    //
    // 不查吊销换来的风险由「https + 清单里的 SHA-256」兜底：攻击者要利用，
    // 得同时控制证书链和摘要，比吊销检查失效本身难得多。
    #[cfg(windows)]
    cmd.arg("--ssl-no-revoke");
    cmd
}

/// 给已经跑完的 curl 生成一个分类过的错误。
pub(crate) fn failure(status: std::process::ExitStatus, stderr: &[u8]) -> FetchError {
    classify(status.code(), &stderr_text(stderr))
}

/// 环境里是否配了代理。curl 认大小写两种写法，这里都看。
fn has_proxy_env() -> bool {
    const KEYS: [&str; 6] = [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ];
    KEYS.iter().any(|k| {
        std::env::var(k)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
    })
}

fn run(url: &str, timeout: Duration, no_proxy: bool) -> Result<String, FetchError> {
    let mut cmd = command(timeout, no_proxy, MAX_TEXT_BYTES as u64);
    // `--` 之后的参数一律当 URL：清单地址是远端可控的，若以 `-` 开头
    // 会被 curl 当选项解析（`Command::arg` 不经 shell，所以不是命令注入，
    // 但仍然是选项注入）
    cmd.arg("--").arg(url);

    let out = cmd.output().map_err(|e| {
        if e.kind() == ErrorKind::NotFound {
            FetchError::NoCurl
        } else {
            FetchError::Network(e.to_string())
        }
    })?;

    if out.status.success() {
        if out.stdout.len() > MAX_TEXT_BYTES {
            return Err(FetchError::Http(format!(
                "响应过大（{} 字节）",
                out.stdout.len()
            )));
        }
        return String::from_utf8(out.stdout)
            .map_err(|_| FetchError::Http("响应不是合法 UTF-8".into()));
    }
    Err(failure(out.status, &out.stderr))
}

/// Windows 上 curl 的错误消息来自系统 API，编码是当前代码页（简中系统是
/// GBK），按 UTF-8 解会变成一串替换字符。解不出来就**整条丢掉**，让
/// [`classify`] 用中文兜底文案——给用户看乱码比只说「证书失败」更糟。
/// curl 自己产的消息（`Failed to connect` 之类）是 ASCII，不受影响。
fn stderr_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => String::new(),
    }
}

/// curl 退出码 → 我们能给用户看的分类。抽成纯函数是为了能单测。
///
/// 码表见 `curl --manual` 的 EXIT CODES 一节；这里只收本项目真会遇到的几种，
/// 其余归到 `Network` 并把原码带上，方便用户搜。
fn classify(code: Option<i32>, stderr: &str) -> FetchError {
    // curl 的 stderr 常带多行（进度、警告），最后一行才是结论
    let brief = stderr
        .lines()
        .last()
        .unwrap_or("")
        .trim()
        .trim_start_matches("curl: ")
        .to_string();
    let brief = if brief.is_empty() {
        "没有更多信息".to_string()
    } else {
        brief
    };
    match code {
        Some(6) => FetchError::Network(format!("域名解析失败（{brief}）")),
        Some(7) => FetchError::Network(format!("连不上服务器（{brief}）")),
        Some(28) => FetchError::Timeout(brief),
        // 35 握手失败 / 51 证书链 / 58 客户端证书 / 60 证书校验
        Some(35) | Some(51) | Some(58) | Some(60) => {
            FetchError::Network(format!("HTTPS 证书或握手失败（{brief}）"))
        }
        Some(22) => FetchError::Http(brief),
        Some(63) => FetchError::Http(format!("响应超过大小上限（{brief}）")),
        Some(n) => FetchError::Network(format!("curl 退出码 {n}（{brief}）")),
        None => FetchError::Network(format!("curl 异常退出（{brief}）")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_https_url_is_refused_without_touching_network() {
        assert!(matches!(
            get_text("http://example.com/a.json", DEFAULT_TIMEOUT),
            Err(FetchError::Http(_))
        ));
        assert!(matches!(
            get_text("file:///etc/passwd", DEFAULT_TIMEOUT),
            Err(FetchError::Http(_))
        ));
    }

    #[test]
    fn exit_codes_map_to_readable_categories() {
        assert!(matches!(
            classify(Some(6), "curl: could not resolve"),
            FetchError::Network(_)
        ));
        assert!(matches!(classify(Some(7), ""), FetchError::Network(_)));
        assert!(matches!(
            classify(Some(28), "Operation timed out"),
            FetchError::Timeout(_)
        ));
        assert!(matches!(
            classify(Some(60), "SSL cert"),
            FetchError::Network(_)
        ));
        assert!(matches!(classify(Some(22), "404"), FetchError::Http(_)));
        assert!(matches!(classify(Some(63), ""), FetchError::Http(_)));
        assert!(matches!(classify(Some(99), ""), FetchError::Network(_)));
        assert!(matches!(classify(None, ""), FetchError::Network(_)));
    }

    #[test]
    fn stderr_brief_drops_curl_prefix_and_keeps_last_line() {
        match classify(Some(7), "progress line\ncurl: (7) Failed to connect") {
            FetchError::Network(msg) => {
                assert!(msg.contains("Failed to connect"), "{msg}");
                assert!(!msg.contains("curl:"), "{msg}");
            }
            other => panic!("应当归到 Network，实际 {other:?}"),
        }
    }

    #[test]
    fn empty_stderr_still_has_explanation() {
        match classify(Some(7), "   ") {
            FetchError::Network(msg) => assert!(msg.contains("没有更多信息"), "{msg}"),
            other => panic!("应当归到 Network，实际 {other:?}"),
        }
    }

    #[test]
    fn non_utf8_stderr_is_dropped_instead_of_shown_as_garbage() {
        // 简中 Windows 的系统错误消息是 GBK（这里是“已禁用服务”），
        // 按 UTF-8 解必然失败 —— 与其给用户看一串 U+FFFD，不如丢掉
        let gbk = [0xD2, 0xD1, 0xBE, 0xAD, 0xB7, 0xFE, 0xCE, 0xF1];
        assert_eq!(stderr_text(&gbk), "");
        assert_eq!(
            stderr_text(b"curl: (7) Failed to connect"),
            "curl: (7) Failed to connect"
        );
        // 丢掉之后仍要有中文兜底
        match classify(Some(35), &stderr_text(&gbk)) {
            FetchError::Network(msg) => assert!(msg.contains("证书或握手失败"), "{msg}"),
            other => panic!("应当归到 Network，实际 {other:?}"),
        }
    }
}
