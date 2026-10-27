//! `updater.json` 的解析与版本比较。
//!
//! 清单格式与 Tauri updater / cc-bridge 同构，只把验签从 minisign 换成
//! SHA-256：本项目 **不引入签名密钥链**，`checksum` 是必填项，`signature`
//! 字段即使存在也不读。安全模型明确为「HTTPS + 内容摘要」——
//! 摘要能挡住下载截断/串包/镜像篡改，挡不住「发布源本身被攻陷」，
//! 这对本项目的威胁模型是够的（发布源就是仓库 owner 自己的 GitHub Releases）。
//!
//! 这里只做「拿到字节之后」的判断：能不能用、比当前版本新不新。
//! 网络与重试在 `source.rs`，下载与校验在 `fetch.rs`。

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

/// 远端清单。`platforms` 的键是平台标识（见 [`platform_key`]）。
#[derive(Clone, Debug, serde::Deserialize)]
pub struct Manifest {
    /// 远端版本号，可带 `v` 前缀，例如 `v0.6.90` / `0.6.90`
    ///
    /// `default` 是故意的：不要让 serde 抢先报 `missing field version`（英文、
    /// 带行号列号），统一由 [`Manifest::parse`] 报中文的 [`ManifestError::MissingVersion`]。
    #[serde(default)]
    pub version: String,
    /// 更新说明，直接来自 CHANGELOG 切段
    #[serde(default)]
    pub notes: String,
    /// 发布时间，ISO 8601；只用于展示，不参与比较
    #[serde(default)]
    pub pub_date: String,
    #[serde(default)]
    pub platforms: BTreeMap<String, Platform>,
}

/// 单个平台的产物。`checksum` 为小写十六进制 SHA-256（64 字符）。
#[derive(Clone, Debug, serde::Deserialize)]
pub struct Platform {
    pub url: String,
    #[serde(default)]
    pub checksum: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManifestError {
    /// 不是合法 JSON，或者字段类型不对
    Json(String),
    MissingVersion,
    /// 清单里没有本平台的产物（发布时漏了某个平台）
    NoPlatform(String),
    /// URL 不是 https，或者是空串
    BadUrl(String),
    /// 摘要缺失或不是 64 位十六进制
    BadChecksum(String),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::Json(e) => write!(f, "更新清单格式错误：{e}"),
            ManifestError::MissingVersion => write!(f, "更新清单缺少 version"),
            ManifestError::NoPlatform(k) => write!(f, "更新清单里没有本平台的产物（{k}）"),
            ManifestError::BadUrl(u) => write!(f, "更新包地址不可用：{u}"),
            ManifestError::BadChecksum(c) => write!(f, "更新包摘要不可用：{c}"),
        }
    }
}

impl std::error::Error for ManifestError {}

/// 本机在清单里对应的键。
///
/// 与 Tauri 的命名保持一致（`windows-x86_64` 而不是 `windows-x64`），
/// 这样同一个 `updater.json` 的生成脚本两边能共用。认不出来就返回空串，
/// 上层会退化成「这个平台不支持自动更新」，而不是猜一个键去撞。
pub fn platform_key() -> &'static str {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "windows-x86_64"
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "darwin-aarch64"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "darwin-x86_64"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "linux-x86_64"
    }
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
    )))]
    {
        ""
    }
}

impl Manifest {
    /// 解析并校验。**校验放在解析里做完**，上层拿到 `Ok` 就可以直接用。
    pub fn parse(text: &str) -> Result<Manifest, ManifestError> {
        let m: Manifest =
            serde_json::from_str(text).map_err(|e| ManifestError::Json(e.to_string()))?;
        if m.version.trim().is_empty() {
            return Err(ManifestError::MissingVersion);
        }
        Ok(m)
    }

    /// 取本平台的产物，并校验 URL 与摘要。
    pub fn for_this_platform(&self) -> Result<&Platform, ManifestError> {
        let key = platform_key();
        if key.is_empty() {
            return Err(ManifestError::NoPlatform("本机平台未登记".into()));
        }
        let p = self
            .platforms
            .get(key)
            .ok_or_else(|| ManifestError::NoPlatform(key.to_string()))?;
        check_url(&p.url)?;
        check_checksum(&p.checksum)?;
        Ok(p)
    }
}

/// 只接受 https。明文 http 会让摘要校验变成「同一条链路上随便改」，
/// 等于没有校验。`file://` 之类更是一律不认（清单是远端给的，不能信）。
fn check_url(url: &str) -> Result<(), ManifestError> {
    let u = url.trim();
    if !u.starts_with("https://") || u.len() <= "https://".len() {
        return Err(ManifestError::BadUrl(u.to_string()));
    }
    Ok(())
}

fn check_checksum(sum: &str) -> Result<(), ManifestError> {
    let s = sum.trim();
    let ok = s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
    if !ok {
        return Err(ManifestError::BadChecksum(s.to_string()));
    }
    Ok(())
}

/// 一个可比较的版本号。
///
/// 自己写而不引 `semver`：只需要「谁更新」这一个判断，`semver` 的
/// 依赖范围语法、build metadata 规则这里全用不上，而多一个依赖就多一份
/// 交叉编译风险（见 CLAUDE.md 的「能不加依赖就不加」）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    /// 数字段，最多三段（major.minor.patch），不足的按 0 比
    parts: Vec<u64>,
    /// 预发布后缀（`0.7.0-rc1` 里的 `rc1`）。有后缀的版本比同号正式版旧。
    pre: Option<String>,
}

impl Version {
    /// `v` 前缀可有可无。任何一段不是数字就判定解析失败（返回 `None`），
    /// 上层据此「不认为有新版本」—— 宁可不动，也不要拿畸形版本号去做升级。
    pub fn parse(input: &str) -> Option<Version> {
        let s = input.trim();
        let s = s
            .strip_prefix('v')
            .or_else(|| s.strip_prefix('V'))
            .unwrap_or(s);
        if s.is_empty() {
            return None;
        }
        let (core, pre) = match s.split_once('-') {
            Some((a, b)) if !b.trim().is_empty() => (a, Some(b.to_string())),
            Some((a, _)) => (a, None),
            None => (s, None),
        };
        let mut parts = Vec::new();
        for seg in core.split('.') {
            if seg.is_empty() || !seg.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            parts.push(seg.parse::<u64>().ok()?);
        }
        if parts.is_empty() {
            return None;
        }
        Some(Version { parts, pre })
    }

    fn seg(&self, i: usize) -> u64 {
        self.parts.get(i).copied().unwrap_or(0)
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        // 数字段逐位比，短的一方补 0：`0.7` == `0.7.0` 但 `< 0.7.1`
        for i in 0..self.parts.len().max(other.parts.len()) {
            match self.seg(i).cmp(&other.seg(i)) {
                Ordering::Equal => {}
                diff => return diff,
            }
        }
        // 同号看后缀：正式版 > 预发布版
        match (&self.pre, &other.pre) {
            (None, None) => Ordering::Equal,
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            // 预发布之间按字典序。`rc10` 会排在 `rc2` 前面，这不符合
            // semver，但本项目只用 `rc1` / `beta` 这类短标记，不值得为此
            // 写一版分段比较；真需要精确排序时再补。
            (Some(a), Some(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let core = self
            .parts
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(".");
        match &self.pre {
            Some(p) => write!(f, "{core}-{p}"),
            None => write!(f, "{core}"),
        }
    }
}

/// 远端是否比当前新。任一侧解析失败都返回 `false`（保守）。
pub fn is_newer(remote: &str, current: &str) -> bool {
    match (Version::parse(remote), Version::parse(current)) {
        (Some(r), Some(c)) => r > c,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_segments_compare_numerically() {
        // 字符串比较会得出 "0.6.9" > "0.6.89"，这正是要防的错
        assert!(is_newer("0.6.90", "0.6.89"));
        assert!(is_newer("0.6.9", "0.6.8"));
        assert!(!is_newer("0.6.9", "0.6.89"));
    }

    #[test]
    fn missing_segments_pad_with_zero() {
        assert!(is_newer("0.7.0", "0.6.99"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.7", "0.7.0"));
        assert!(is_newer("0.7.1", "0.7"));
    }

    #[test]
    fn same_version_is_not_newer_and_v_prefix_is_equivalent() {
        assert!(!is_newer("0.6.89", "0.6.89"));
        assert!(!is_newer("v0.6.89", "0.6.89"));
        assert!(is_newer("V0.6.90", "v0.6.89"));
    }

    #[test]
    fn prerelease_is_older_than_release() {
        assert!(is_newer("0.7.0", "0.7.0-rc1"));
        assert!(!is_newer("0.7.0-rc1", "0.7.0"));
        assert!(is_newer("0.7.0-rc1", "0.6.89"));
    }

    #[test]
    fn malformed_version_is_never_newer() {
        for bad in ["", "v", "0.x.1", "0..1", "abc", "0.6.89.1.2.x"] {
            assert!(!is_newer(bad, "0.6.89"), "{bad} 不该算新版");
        }
        assert!(!is_newer("0.7.0", "乱码"));
    }

    #[test]
    fn manifest_picks_local_platform() {
        let text = r#"{
            "version": "0.6.90",
            "notes": "修了几个问题",
            "pub_date": "2026-10-27T00:00:00Z",
            "platforms": {
                "windows-x86_64": {
                    "url": "https://example.com/a-setup.exe",
                    "checksum": "0000000000000000000000000000000000000000000000000000000000000000"
                },
                "darwin-aarch64": {
                    "url": "https://example.com/a.dmg",
                    "checksum": "1111111111111111111111111111111111111111111111111111111111111111"
                }
            }
        }"#;
        let m = Manifest::parse(text).expect("应当解析成功");
        assert_eq!(m.version, "0.6.90");
        assert_eq!(m.notes, "修了几个问题");
        if platform_key().is_empty() {
            // 本机不是四个主平台之一：只能确认清单本身解析没问题
            return;
        }
        let p = m.for_this_platform().expect("本平台产物应当存在");
        assert!(p.url.starts_with("https://"));
    }

    #[test]
    fn missing_version_is_rejected() {
        // 缺字段和空串/纯空白都要落到同一个中文错误上
        for text in [
            r#"{"platforms":{}}"#,
            r#"{"version":"","platforms":{}}"#,
            r#"{"version":"   ","platforms":{}}"#,
        ] {
            let err = Manifest::parse(text).unwrap_err();
            assert_eq!(err, ManifestError::MissingVersion, "输入：{text}");
        }
        let err = Manifest::parse("不是 json").unwrap_err();
        assert!(matches!(err, ManifestError::Json(_)));
    }

    #[test]
    fn missing_platform_reports_key() {
        let m = Manifest::parse(r#"{"version":"1.0.0","platforms":{}}"#).unwrap();
        if platform_key().is_empty() {
            return;
        }
        match m.for_this_platform() {
            Err(ManifestError::NoPlatform(k)) => assert_eq!(k, platform_key()),
            other => panic!("应当报缺产物，实际 {other:?}"),
        }
    }

    #[test]
    fn rejects_plain_http_and_bad_checksum() {
        assert!(check_url("http://example.com/a.exe").is_err());
        assert!(check_url("file:///etc/passwd").is_err());
        assert!(check_url("https://").is_err());
        assert!(check_url("https://example.com/a.exe").is_ok());

        assert!(check_checksum("").is_err());
        assert!(check_checksum("abc").is_err());
        // 大写十六进制也接受（生成脚本可能输出大写）
        assert!(check_checksum(&"A".repeat(64)).is_ok());
        assert!(check_checksum(&"g".repeat(64)).is_err());
    }
}
