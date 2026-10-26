//! 加密：把事件流用配对码派生的密钥罩起来（AEAD）。
//!
//! 帧结构、为什么 counter 放明文、为什么不用 TLS —— 在下面的块注释里，
//! 那是协议设计的一部分，搬代码时原样保留。加解密测试也在本文件。

use super::codec::Event;
use super::MAGIC;

// ---------------------------------------------------------------------------
// 加密：AEAD(ChaCha20-Poly1305)
// ---------------------------------------------------------------------------
//
// Deskflow 默认就用 TLS 把事件流罩起来，我们原来是裸 UDP —— 同一网段的人
// 抓包就能看到你敲的每一个键。这里用配对码派生出的密钥做对称加密，
// 效果等价（都是"只有配对过的那两台能读"），但不用握手、不用证书、
// 每个包开销只有 25 字节。
//
// 帧结构：
// ```text
// [0xA7][prefix 4][counter 4][密文 ...][tag 16]
// ```
// nonce = prefix(4) || counter(4) || 0(4)，12 字节。
//
// 为什么 counter 放明文：解密前就得知道 nonce。它被改会导致解密失败，
// 所以不需要额外的 AAD 保护。counter 单调递增，重放的旧包解不开也过不了
// 序号的回退检查。

/// 加密帧的头长：magic(1) + prefix(4) + counter(4)
const SEALED_HEAD: usize = 9;
/// Poly1305 认证标签长度
const TAG_LEN: usize = 16;

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};

/// 加密侧。每个会话一个，counter 单调递增。
pub struct Sealer {
    cipher: ChaCha20Poly1305,
    prefix: [u8; 4],
    counter: u32,
}

impl Sealer {
    /// key 由配对码派生（见 `crate::pair::link_key`）。
    /// prefix 每个会话随机，避免两台机器用同一把 key 时 nonce 撞车。
    pub fn new(key: &[u8; 32], prefix: [u8; 4]) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new(key.into()),
            prefix,
            counter: 0,
        }
    }

    /// 从当前时间 + 进程号凑一个 prefix。不需要密码学随机：
    /// 它只是用来区分会话，nonce 的唯一性由 counter 保证。
    pub fn fresh_prefix() -> [u8; 4] {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let pid = std::process::id();
        let mixed = (nanos as u64) ^ ((pid as u64) << 32) ^ 0x9e37_79b9_7f4a_7c15;
        let bytes = mixed.to_le_bytes();
        [bytes[0], bytes[1], bytes[2], bytes[3]]
    }

    pub fn seal(&mut self, seq: u16, ev: &Event) -> Vec<u8> {
        self.counter = self.counter.wrapping_add(1);
        let mut nonce_bytes = [0u8; 12];
        nonce_bytes[..4].copy_from_slice(&self.prefix);
        nonce_bytes[4..8].copy_from_slice(&self.counter.to_le_bytes());
        let nonce = Nonce::from(nonce_bytes);

        let plain = ev.body(seq);
        let Ok(sealed) = self.cipher.encrypt(&nonce, plain.as_slice()) else {
            // 加密失败（长度超限之类）只能丢这个包，不能退回明文 —— 那等于静默降级
            return Vec::new();
        };

        let mut out = Vec::with_capacity(SEALED_HEAD + sealed.len());
        out.push(MAGIC);
        out.extend_from_slice(&self.prefix);
        out.extend_from_slice(&self.counter.to_le_bytes());
        out.extend_from_slice(&sealed);
        out
    }
}

/// 解密侧。维护 counter 的下界来拒掉重放的旧包。
pub struct Opener {
    cipher: ChaCha20Poly1305,
    prefix: [u8; 4],
    /// 见过的最大的 counter。允许少量回退（UDP 会乱序），但不接受太久以前的。
    low_water: u32,
    /// 被拒掉的包数，用来区分"对端没配对"和"真的没包"
    pub rejected: u64,
}

/// 允许 counter 往回退多少。局域网乱序通常是几个包，
/// 给 64 已经非常宽松；再远的直接当重放丢掉。
const REPLAY_WINDOW: u32 = 64;

impl Opener {
    pub fn new(key: &[u8; 32]) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new(key.into()),
            prefix: [0; 4],
            low_water: 0,
            rejected: 0,
        }
    }

    pub fn open(&mut self, buf: &[u8]) -> Option<(u16, Event)> {
        if buf.len() < SEALED_HEAD + TAG_LEN || buf[0] != MAGIC {
            return None;
        }
        let mut prefix = [0u8; 4];
        prefix.copy_from_slice(&buf[1..5]);
        let counter = u32::from_le_bytes(buf[5..9].try_into().ok()?);

        // 换了对端（prefix 变了）就重置水位，不然新会话的 counter 从 1 开始会被当成旧包
        if self.prefix != [0; 4] && self.prefix != prefix {
            self.low_water = 0;
        }
        self.prefix = prefix;

        if counter + REPLAY_WINDOW < self.low_water {
            self.rejected += 1;
            return None;
        }

        let mut nonce_bytes = [0u8; 12];
        nonce_bytes[..4].copy_from_slice(&prefix);
        nonce_bytes[4..8].copy_from_slice(&counter.to_le_bytes());
        let nonce = Nonce::from(nonce_bytes);

        let plain = self.cipher.decrypt(&nonce, &buf[SEALED_HEAD..]).ok()?;
        if counter > self.low_water {
            self.low_water = counter;
        }
        Event::decode_body(&plain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_roundtrip() {
        let key = [7u8; 32];
        let mut sealer = Sealer::new(&key, [1, 2, 3, 4]);
        let mut opener = Opener::new(&key);
        let cases = [
            Event::Motion { dx: -3, dy: 17 },
            Event::Key {
                code: 0x0C,
                down: true,
            },
            Event::Ping,
            Event::Leave,
        ];
        for (i, ev) in cases.iter().enumerate() {
            let bytes = sealer.seal(i as u16, ev);
            assert_eq!(opener.open(&bytes), Some((i as u16, *ev)), "{ev:?}");
        }
    }

    #[test]
    fn sealed_rejects_wrong_key() {
        let mut sealer = Sealer::new(&[1u8; 32], [9, 9, 9, 9]);
        let mut opener = Opener::new(&[2u8; 32]);
        let bytes = sealer.seal(1, &Event::Motion { dx: 1, dy: 1 });
        assert_eq!(opener.open(&bytes), None, "拿错钥匙必须解不开");
    }

    #[test]
    fn sealed_rejects_tamper() {
        let key = [3u8; 32];
        let mut sealer = Sealer::new(&key, [5, 6, 7, 8]);
        let mut opener = Opener::new(&key);
        let mut bytes = sealer.seal(1, &Event::Key {
            code: 0x00,
            down: true,
        });
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        assert_eq!(opener.open(&bytes), None, "改一个字节就必须验签失败");
    }

    #[test]
    fn sealed_rejects_replay() {
        let key = [4u8; 32];
        let mut sealer = Sealer::new(&key, [1, 1, 1, 1]);
        let mut opener = Opener::new(&key);
        let first = sealer.seal(1, &Event::Motion { dx: 5, dy: 5 });
        assert!(opener.open(&first).is_some());
        // 攒够超过窗口的包，再放重放的第一个
        for i in 0..(REPLAY_WINDOW + 4) {
            let _ = opener.open(&sealer.seal(i as u16, &Event::Ping));
        }
        assert_eq!(opener.open(&first), None, "太久以前的包必须当重放丢掉");
        assert!(opener.rejected >= 1);
    }
}
