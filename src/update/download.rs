//! 把安装包下载到临时目录，边下边报进度，下完校验 SHA-256。
//!
//! ## 进度是怎么来的
//!
//! curl 自带的进度条在 `-s`（静默）下不输出，在管道里也会被抑制，解析它
//! 不可靠。这里换成**轮询临时文件大小**：每 250ms 看一眼 `.part` 文件的
//! 长度，差值除以时间就是速率。好处是不依赖 curl 的输出格式（换个版本、
//! 换个语言都不会坏），而且天然是「真实落盘字节数」——比 curl 报的
//! 「已接收」更诚实。
//!
//! 总大小从 `-D`（dump headers）文件里读 `Content-Length`。ghproxy 走
//! 代理时可能是 chunked（没有这个头），那就报 0，界面只显示已下载量。
//!
//! ## 为什么先下到 `.part` 再改名
//!
//! 直接写到最终文件名的话，中途失败会留下一个看起来完整、其实截断的
//! 安装包。P2 的安装步骤只看文件名的话会拿它去装。先写 `.part`、校验通过
//! 再 `rename`，中途失败的文件名就不会骗人。

use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::update::fetch::{self, FetchError};
use crate::update::source::{ErrorCode, UpdateError};

/// 磁盘上同时存在的安装包上限。目前的包 5 MB 上下，给到 300 MB 是防止
/// 清单被改成一个 GB 级的地址把磁盘写满。
const MAX_ASSET_BYTES: u64 = 300 * 1024 * 1024;

/// 进度采样间隔。250ms 与 cc-bridge 一致：再密就是刷屏，再疏数字会跳。
pub const TICK: Duration = Duration::from_millis(250);

/// 下载超时。检查清单是 30s，安装包按「慢速家宽 + 5MB」算也够，留足余量。
pub const TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// 一次进度回调的内容。
///
/// `total == 0` 表示服务器没给 `Content-Length`（chunked），此时界面只显示
/// 已下载量、不画百分比——拿 0 当分母会算出 `inf%`。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub downloaded: u64,
    pub total: u64,
    /// 最近一个采样窗口的瞬时速率（字节/秒）
    pub bytes_per_sec: u64,
}

/// 下载并校验。返回落地后的文件路径。
///
/// `urls` 是候选地址（两级源），逐个试；`name` 是落盘文件名；
/// `expect_sha` 是清单里的摘要（小写十六进制，64 字符）。
pub fn download(
    urls: &[String],
    dir: &Path,
    name: &str,
    expect_sha: &str,
    mut on_progress: impl FnMut(Progress),
) -> Result<PathBuf, UpdateError> {
    if urls.is_empty() {
        return Err(UpdateError::new(ErrorCode::Unknown, "没有可用的下载地址"));
    }
    if expect_sha.len() != 64 || !expect_sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        // 走到这里说明清单校验有漏（`manifest.rs` 本该拦住），宁可不下
        return Err(UpdateError::new(
            ErrorCode::Checksum,
            format!("清单里的摘要不可用：{expect_sha}"),
        ));
    }
    fs::create_dir_all(dir).map_err(|e| {
        UpdateError::new(
            ErrorCode::Config,
            format!("建不了临时目录 {}：{e}", dir.display()),
        )
    })?;

    let part = dir.join(format!("{name}.part"));
    let headers = dir.join(format!("{name}.headers"));
    let stderr_file = dir.join(format!("{name}.stderr"));
    let final_path = dir.join(name);
    let mut last: Option<UpdateError> = None;

    for url in urls {
        match attempt(url, &part, &headers, &stderr_file, &mut on_progress) {
            Ok(()) => match verify_and_place(&part, &final_path, expect_sha, name) {
                Ok(path) => {
                    cleanup(&[&headers, &stderr_file]);
                    return Ok(path);
                }
                // 摘要不符也换源重试：ghproxy 缓存过坏包是这个功能真要
                // 处理的场景，换直连往往就好了
                Err(e) => {
                    cleanup(&[&part]);
                    last = Some(e);
                }
            },
            Err(e) => last = Some(e),
        }
    }

    cleanup(&[&part, &headers, &stderr_file]);
    Err(last.unwrap_or_else(|| UpdateError::new(ErrorCode::Unknown, "下载失败")))
}

fn cleanup(paths: &[&Path]) {
    for p in paths {
        let _ = fs::remove_file(p);
    }
}

/// 校验摘要，过了才把 `.part` 改名成正式文件名。
fn verify_and_place(
    part: &Path,
    final_path: &Path,
    expect_sha: &str,
    name: &str,
) -> Result<PathBuf, UpdateError> {
    let got = sha256_file(part)?;
    if !got.eq_ignore_ascii_case(expect_sha) {
        return Err(UpdateError::new(
            ErrorCode::Checksum,
            format!("{name} 校验失败：期望 {expect_sha}，实际下载到 {got}"),
        ));
    }
    // 先删旧文件：Windows 上 rename 到已存在的路径会失败
    let _ = fs::remove_file(final_path);
    fs::rename(part, final_path).map_err(|e| {
        UpdateError::new(
            ErrorCode::Config,
            format!("改名失败（{}）：{e}", final_path.display()),
        )
    })?;
    Ok(final_path.to_path_buf())
}

/// 单个候选项：跑一次 curl 并轮询进度。
fn attempt(
    url: &str,
    part: &Path,
    headers: &Path,
    stderr_file: &Path,
    on_progress: &mut impl FnMut(Progress),
) -> Result<(), UpdateError> {
    fetch::ensure_https(url)
        .map_err(|e| UpdateError::new(ErrorCode::from_fetch(&e), e.to_string()))?;
    cleanup(&[part, headers, stderr_file]);

    // stderr 不能 piped 却不读：curl 写满管道缓冲区会阻塞、下载卡死。
    // 直接重定向到文件，跑完再读，最省事。
    let err_file = fs::File::create(stderr_file).map_err(|e| {
        UpdateError::new(
            ErrorCode::Config,
            format!("建临时文件失败（{}）：{e}", stderr_file.display()),
        )
    })?;
    let mut cmd = fetch::command(TIMEOUT, false, MAX_ASSET_BYTES);
    cmd.arg("-o")
        .arg(part)
        .arg("-D")
        .arg(headers)
        .arg("--")
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::from(err_file));

    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            UpdateError::new(ErrorCode::Config, FetchError::NoCurl.to_string())
        } else {
            UpdateError::new(ErrorCode::Config, format!("启动 curl 失败：{e}"))
        }
    })?;

    let mut last_at = Instant::now();
    let mut last_bytes = 0u64;
    let mut total = 0u64;
    let status = loop {
        std::thread::sleep(TICK);
        let bytes = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
        if total == 0 {
            total = content_length(headers).unwrap_or(0);
        }
        let now = Instant::now();
        let elapsed = now.duration_since(last_at).as_secs_f64();
        let rate = if elapsed > 0.0 {
            (bytes.saturating_sub(last_bytes) as f64 / elapsed) as u64
        } else {
            0
        };
        on_progress(Progress {
            downloaded: bytes,
            total,
            bytes_per_sec: rate,
        });
        last_at = now;
        last_bytes = bytes;

        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => continue,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(UpdateError::new(
                    ErrorCode::Unknown,
                    format!("等待 curl 失败：{e}"),
                ));
            }
        }
    };

    let stderr = fs::read(stderr_file).unwrap_or_default();
    if !status.success() {
        let e = fetch::failure(status, &stderr);
        return Err(UpdateError::new(
            ErrorCode::from_fetch(&e),
            format!("{url}：{e}"),
        ));
    }
    // 收尾再报一次：最后一次采样可能落在「还差几百 KB」的位置，
    // 不补这一下界面会停在 99% 直到下次状态刷新
    let bytes = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    on_progress(Progress {
        downloaded: bytes,
        total: if total == 0 { bytes } else { total },
        bytes_per_sec: 0,
    });
    Ok(())
}

/// 从 `-D` 导出的响应头里读 `Content-Length`。
///
/// 跟随重定向时文件里有多个响应头块，取**最后**一个（`rev()`）才是真正
/// 的响应体。
fn content_length(headers: &Path) -> Option<u64> {
    let text = fs::read_to_string(headers).ok()?;
    text.lines().rev().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        if !k.trim().eq_ignore_ascii_case("content-length") {
            return None;
        }
        v.trim().parse::<u64>().ok()
    })
}

/// 流式计算文件摘要，不整个读进内存（安装包可能几十 MB）。
pub fn sha256_file(path: &Path) -> Result<String, UpdateError> {
    let mut f = fs::File::open(path).map_err(|e| {
        UpdateError::new(
            ErrorCode::Config,
            format!("打不开下载好的安装包（{}）：{e}", path.display()),
        )
    })?;
    let mut hasher = Sha256::new();
    // 64 KB：比 4 KB 少一半系统调用，又小到不占内存
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| UpdateError::new(ErrorCode::Config, format!("读安装包失败：{e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// 落盘文件名：从 URL 尾巴取，只保留安全字符。
///
/// 必须清洗——`url` 来自远端清单，含 `../` 或 `C:` 就能写到临时目录外面
/// （路径穿越）。所以这里不放行 `/`、`\`、`:`，也不接受以 `.` 开头。
pub fn file_name_for(url: &str) -> String {
    let no_query = url.split(['?', '#']).next().unwrap_or("");
    // 先跳过 `scheme://host`，只从**路径**里取最后一段。不这么做的话
    // `https://e.com/` 会因为 host 里也有 `/` 而把 `e.com` 当文件名。
    let path = match no_query.split_once("://") {
        Some((_, rest)) => rest.split_once('/').map(|(_, p)| p).unwrap_or(""),
        None => no_query,
    };
    let tail = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let cleaned: String = tail
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_start_matches('.').to_string();
    if cleaned.is_empty() {
        "update.bin".to_string()
    } else {
        cleaned
    }
}

/// 安装包落地的目录。放系统临时目录下单独一层，卸载时残留也只是一堆
/// 可以随手删的文件。
pub fn staging_dir() -> PathBuf {
    std::env::temp_dir().join("inputswitch-update")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vector() {
        // 标准测试向量：sha256("abc")
        let dir = std::env::temp_dir().join(format!("isw-dl-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("abc.bin");
        fs::write(&p, b"abc").unwrap();
        let sum = sha256_file(&p).unwrap();
        assert_eq!(
            sum,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hex_is_lowercase_two_digits_per_byte() {
        assert_eq!(hex(&[0x00, 0x0f, 0xff]), "000fff");
        assert_eq!(hex(&[]), "");
    }

    #[test]
    fn bad_expect_sha_is_refused_before_any_io() {
        let dir = staging_dir();
        let err = download(
            &["https://example.com/a.exe".to_string()],
            &dir,
            "a.exe",
            "not-a-sha",
            |_| {},
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::Checksum);
    }

    #[test]
    fn empty_url_list_is_an_error() {
        let err = download(&[], &staging_dir(), "a.exe", &"a".repeat(64), |_| {}).unwrap_err();
        assert_eq!(err.code, ErrorCode::Unknown);
    }

    #[test]
    fn file_name_is_stripped_of_query_and_unsafe_chars() {
        assert_eq!(
            file_name_for("https://github.com/o/r/releases/download/v1/Setup.exe"),
            "Setup.exe"
        );
        // 查询串不能进文件名
        assert_eq!(file_name_for("https://e.com/a.exe?token=1"), "a.exe");
        // 路径穿越：斜杠被换掉，取不到 `../`
        assert_eq!(file_name_for("https://e.com/../../etc/passwd"), "passwd");
        // Windows 盘符与反斜杠
        assert_eq!(
            file_name_for("https://e.com/C:\\Windows\\x.exe"),
            "C__Windows_x.exe"
        );
        // 只有分隔符时兜底
        assert_eq!(file_name_for("https://e.com/"), "update.bin");
        assert_eq!(file_name_for(""), "update.bin");
        // 以点开头要剥掉（不能生成 `.hidden` 或 `.`）
        assert_eq!(file_name_for("https://e.com/.config"), "config");
    }

    #[test]
    fn content_length_takes_the_last_header_block() {
        let dir = std::env::temp_dir().join(format!("isw-dl-hdr-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("h.headers");
        fs::write(
            &p,
            "HTTP/1.1 302 Found\r\nContent-Length: 0\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 12345\r\n\r\n",
        )
        .unwrap();
        assert_eq!(content_length(&p), Some(12345));
        fs::write(&p, "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap();
        assert_eq!(content_length(&p), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn staging_dir_is_under_temp() {
        let d = staging_dir();
        assert!(d.starts_with(std::env::temp_dir()), "{}", d.display());
        assert!(d.ends_with("inputswitch-update"));
    }

    #[test]
    fn progress_without_total_is_not_a_division_by_zero() {
        // 只是把「total=0 表示未知」这个约定写成断言，防止后来者给它补默认 1
        let p = Progress::default();
        assert_eq!(p.total, 0);
    }

    /// 单测专用：在 127.0.0.1 上起一个只应答一次的 HTTP 服务器。
    ///
    /// 下载链路必须真跑一遍才算数（curl 参数、`-D` 头文件、轮询采样、改名，
    /// 每一环都可能只在真实子进程里才出问题）。给它起 TLS 要证书和额外
    /// 依赖，所以用回环 HTTP —— `fetch::ensure_https` 里有 `cfg(test)` 的
    /// 例外，正式构建里没有这条路径。
    #[cfg(test)]
    fn serve_once(body: Vec<u8>, status: &'static str) -> String {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定回环端口");
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                // 读到请求头结束就够了，body 不关心
                let mut buf = [0u8; 1024];
                let mut seen = Vec::new();
                while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => seen.extend_from_slice(&buf[..n]),
                    }
                }
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes());
                let _ = sock.write_all(&body);
            }
        });
        format!("http://{addr}/Setup.exe")
    }

    #[cfg(test)]
    fn body_with_known_sha() -> (Vec<u8>, String) {
        let body: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
        let dir = std::env::temp_dir().join(format!("isw-dl-src-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("src.bin");
        fs::write(&p, &body).unwrap();
        let sum = sha256_file(&p).unwrap();
        let _ = fs::remove_dir_all(&dir);
        (body, sum)
    }

    #[test]
    fn downloads_reports_progress_and_passes_checksum() {
        let (body, sum) = body_with_known_sha();
        let url = serve_once(body.clone(), "200 OK");
        let dir = std::env::temp_dir().join(format!("isw-dl-ok-{}", std::process::id()));

        let mut ticks = Vec::new();
        let path =
            download(&[url], &dir, "Setup.exe", &sum, |p| ticks.push(p)).expect("应当下载成功");

        assert_eq!(fs::read(&path).unwrap(), body, "落地内容要与源一致");
        assert_eq!(path.file_name().unwrap(), "Setup.exe");
        assert!(!dir.join("Setup.exe.part").exists(), "改名后不该留 .part");
        let last = ticks.last().expect("至少要报一次进度");
        assert_eq!(
            last.downloaded,
            body.len() as u64,
            "收尾要把进度补到实际大小"
        );
        assert_eq!(last.total, body.len() as u64, "总量要取自 Content-Length");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn checksum_mismatch_leaves_no_installer_behind() {
        let (body, _) = body_with_known_sha();
        let url = serve_once(body, "200 OK");
        let dir = std::env::temp_dir().join(format!("isw-dl-bad-{}", std::process::id()));

        let err = download(&[url], &dir, "Setup.exe", &"b".repeat(64), |_| {}).unwrap_err();
        assert_eq!(err.code, ErrorCode::Checksum);
        assert!(
            !dir.join("Setup.exe").exists(),
            "校验不过不能留下可执行文件"
        );
        assert!(!dir.join("Setup.exe.part").exists(), ".part 也要清掉");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn server_error_is_classified_and_cleaned_up() {
        let url = serve_once(Vec::new(), "404 Not Found");
        let dir = std::env::temp_dir().join(format!("isw-dl-404-{}", std::process::id()));
        let err = download(&[url], &dir, "Setup.exe", &"a".repeat(64), |_| {}).unwrap_err();
        assert_eq!(err.code, ErrorCode::Http, "{}", err.message);
        assert!(!dir.join("Setup.exe.part").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
