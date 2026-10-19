//! 配对与局域网消息签名。
//!
//! 目的不是加密（局域网里抓包照样看得见内容），而是**认人**：
//! 同一网段的另一台电脑、邻居的手机、随便一个扫端口的脚本，
//! 都不该有资格让我们的显示器切来切去。
//!
//! 做法是两边填同一个配对码，用 HMAC-SHA256 派生链路密钥，
//! 每条 UDP 消息带 `ts + nonce + mac`：
//! - `ts` 卡时间窗，挡住重放
//! - `nonce` 环形去重，挡住窗内的重复投递
//! - `mac` 挡陌生人
//!
//! ⚠️ 6 位数字只有 20 bit 熵，抓包的人可以离线穷举出码。所以配对码只是"门禁"，
//! 真要高强度就把码填长一点（16 位以上基本没得猜）。家用局域网这个强度够用。

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// 允许的时间偏差（秒）。两台机器的时钟差一般远小于这个值。
pub const TIME_WINDOW_SECS: u64 = 60;

/// 配对码 → 32 字节链路密钥。空码返回 None（= 不启用签名）。
pub fn link_key(code: &str) -> Option<[u8; 32]> {
    let code = code.trim();
    if code.is_empty() {
        return None;
    }
    // 用固定串当 HMAC 的 key 做一次 HKDF 风格的派生，避免直接拿码当密钥
    let mut mac = HmacSha256::new_from_slice(b"inputswitch.link.v1").expect("常量长度的 key");
    mac.update(code.as_bytes());
    let digest = mac.finalize().into_bytes();
    let mut key = [0u8; 32];
    key.copy_from_slice(&digest);
    Some(key)
}

/// 对规范化的消息体签名，取前 16 字节的十六进制（32 个字符）。
pub fn sign(key: &[u8; 32], body: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("32 字节 key 一定合法");
    mac.update(body.as_bytes());
    to_hex(&mac.finalize().into_bytes()[..16])
}

/// 常量时间比较。别用 `==`：那会在第一个不同字节处返回，
/// 局域网里测响应时间能把它变成逐字节爆破的旁路。
pub fn verify(key: &[u8; 32], body: &str, claimed: &str) -> bool {
    let expected = sign(key, body);
    ct_eq(expected.as_bytes(), claimed.trim().as_bytes())
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// SHA-256 十六进制摘要，文件传输用来校验完整性。
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    to_hex(&hasher.finalize())
}

pub fn sha256_file(path: &std::path::Path) -> std::io::Result<String> {
    use sha2::Digest;
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 128 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(to_hex(&hasher.finalize()))
}

/// 6 位配对码。取系统熵源，不用时间戳拼 —— 那种码是可预测的。
pub fn random_code() -> String {
    let mut bytes = [0u8; 4];
    fill_random(&mut bytes);
    let n = u32::from_be_bytes(bytes) % 1_000_000;
    format!("{n:06}")
}

fn fill_random(buf: &mut [u8]) {
    #[cfg(unix)]
    {
        use std::io::Read;
        if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
            if file.read_exact(buf).is_ok() {
                return;
            }
        }
    }
    #[cfg(windows)]
    {
        // advapi32!SystemFunction036 就是 RtlGenRandom，不需要额外依赖
        extern "system" {
            fn SystemFunction036(buffer: *mut u8, length: u32) -> u8;
        }
        unsafe {
            if SystemFunction036(buf.as_mut_ptr(), buf.len() as u32) != 0 {
                return;
            }
        }
    }
    // 熵源拿不到时的兜底：宁可质量差一点，也不要返回全零
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mix = now ^ ((std::process::id() as u128) << 32);
    for (i, byte) in buf.iter_mut().enumerate() {
        *byte = ((mix >> (i * 8)) & 0xff) as u8;
    }
}

/// 本机已经见过的 nonce，用来挡住同一时间窗内的重放。
#[derive(Default)]
pub struct NonceCache {
    seen: Vec<u64>,
}

impl NonceCache {
    const CAP: usize = 512;

    /// 第一次见到返回 true；重复返回 false。
    pub fn accept(&mut self, nonce: u64) -> bool {
        if self.seen.contains(&nonce) {
            return false;
        }
        if self.seen.len() >= Self::CAP {
            self.seen.remove(0);
        }
        self.seen.push(nonce);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_derivation_is_stable_and_distinct() {
        let a = link_key("123456").unwrap();
        let b = link_key("123456").unwrap();
        let c = link_key("123457").unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(link_key("   ").is_none());
    }

    #[test]
    fn signature_roundtrip() {
        let key = link_key("123456").unwrap();
        let sig = sign(&key, "announce|mac|123");
        assert!(verify(&key, "announce|mac|123", &sig));
        assert!(!verify(&key, "announce|mac|124", &sig));
        let other = link_key("999999").unwrap();
        assert!(!verify(&other, "announce|mac|123", &sig));
    }

    #[test]
    fn nonce_replay_is_rejected() {
        let mut cache = NonceCache::default();
        assert!(cache.accept(7));
        assert!(!cache.accept(7));
        assert!(cache.accept(8));
    }

    #[test]
    fn random_code_shape() {
        for _ in 0..32 {
            let code = random_code();
            assert_eq!(code.len(), 6);
            assert!(code.chars().all(|c| c.is_ascii_digit()));
        }
    }
}
