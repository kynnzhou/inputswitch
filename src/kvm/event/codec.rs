//! 事件枚举与编解码：`Event` 的线格式（明包）。
//!
//! 加密模式不解这里的整包，而是复用 `body()` / `decode_body()` 那一层
//! （见 seal.rs）—— 这两个方法是本文件对外的核心，改动前先看 seal.rs
//! 怎么用它们。明包的往返测试跟着住在本文件。

use super::edge::Side;
use super::{MAGIC, PEER_UNKNOWN};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// 相对位移，单位是屏幕点（点，不是像素）
    Motion { dx: i32, dy: i32 },
    /// button: 1 左 2 右 3 中 4 后退 5 前进
    Button { button: u8, down: bool },
    /// 滚轮，120 = 一格（Windows 的 WHEEL_DELTA 也是 120）
    Wheel { dx: i32, dy: i32 },
    /// 键，code 是 macOS virtual keycode
    Key { code: u16, down: bool },
    /// 控制端开始转发（画面已经切到对端了）。
    ///
    /// `along` = 出门瞬间光标在门轴上的位置比例（0..65535，65535 表示未知）：
    /// 左右门是纵坐标比例、上下门是横坐标比例。接收端据此把光标放到
    /// 自己门边的对应高度 —— 从哪个高度出去，就从哪个高度进来
    /// （Deskflow 的 jump cursor + mapToFraction 思路），不再居中猛跳。
    /// `flags` 留作以后同步 CapsLock/NumLock 等锁定键状态用，当前恒 0。
    ///
    /// `width`/`height` = 发送端屏幕尺寸（用发送端自己的单位：Windows 是
    /// 像素、macOS 是逻辑点，0 表示未知）。接收端拿它算两屏的比例，
    /// 把转发来的位移按比例缩放后再注入 —— 不然 Windows（1080p 像素）
    /// 的 delta 到 Mac（缩放后的逻辑点）上会被放大好几倍，光标一动就
    /// 冲出门被弹回去。这是 Deskflow 用绝对坐标映射屏幕形状所解决的
    /// 同一个问题的等价实现。
    Enter {
        width: u16,
        height: u16,
        along: u16,
        flags: u16,
        /// 发送端**从哪一侧出的门**（`Side::code()`，`Side::CODE_UNKNOWN`
        /// 表示旧版对端不带这一位）。
        ///
        /// 接收端据此把自己的门口方向定成它的**对面**：隔壁那台从右边
        /// 出去，我这边必然在它左边。以前这一侧完全靠两边各自配置，
        /// 有一边配反了就会「刚进门就被当成推出门」，1~2 秒弹回去
        /// （v0.6.42 实测：Mac 推过去 0.5~2 秒必被交还）。拓扑决定了
        /// 这件事，不该让人去配。
        side: u8,
    },
    /// 请求结束控制
    Leave,
    /// 心跳。Deskflow 那边叫 keep-alive，协议里有个 IDLE_TIMEOUT=2000，
    /// 超时就认定连接断了。UDP 没有连接状态，只能靠这个"证明我还活着"。
    Ping,
    /// 心跳应答。`state` 是应答方当前的状态
    /// （0=Idle 1=Sending 2=Receiving）—— 控制消息（Enter/Takeback）
    /// 是单发 UDP，丢一包两边状态机就各持一半真相卡死；心跳每 0.8s
    /// 一次，带着状态走，任何错位 2.5 秒内必然被两边发现并自愈
    /// （Deskflow 用同样的思路：keep-alive 里带往返状态）。
    Pong { state: u8 },
    /// **接收方主动交还控制权**（光标推回门口 / 发现对端状态错乱）。
    /// 发送方收到后必须收回键鼠，否则它会一直抓着键鼠往没人接收的
    /// 方向转发 —— 两边都以为对方在控制，键鼠就全卡死了。
    /// 与 `Leave` 的区别：Leave 是"抢占被对端接受"的让路应答，对端
    /// 收到后**保持** Sending；Takeback 是"我不要了，你收回"，对端
    /// 收到后必须回到 Idle。
    ///
    /// `along` = 光标交还那一刻在**门轴**上的位置比例（0xFFFF = 未知）。
    /// 发送方回程时按它把光标放回对应高度/宽度 —— 不带这一位的话，
    /// 从邻居推回来的光标只能落在屏幕正中（v0.6.62 用户实测：
    /// "从 0DFF1JU 回来，光标直接跳到 MC5K9SJ 中间"）。
    /// 旧版对端只读 kind，多出来的两字节被忽略 → 退化成居中，无害。
    Takeback {
        along: u16,
    },
    /// **绝对锚点**：发送端虚拟光标在它自己屏幕上的位置比例（0..65535）。
    ///
    /// 为什么不干脆全改成绝对坐标（Deskflow 的做法）：绝对坐标丢一个 UDP
    /// 包就是一次**跳变**，而且绕过系统指针加速，手感会突变。相对位移丢包
    /// 只是少走几像素。所以这里走中间路线 —— 位移照旧传相对的，每隔几百
    /// 毫秒补一个绝对锚点，接收端只在偏差超过阈值时才纠正。
    ///
    /// 解决的是长会话里的**累积漂移**（分辨率比例换算每笔都四舍五入），
    /// 以及"切回来光标不在原位"。
    ///
    /// `0xFFFF` = 这一维未知（发送端还没建立虚拟光标），该维不校正。
    Anchor {
        fx: u16,
        fy: u16,
    },
}

impl Event {
    pub fn kind(&self) -> u8 {
        match self {
            Event::Motion { .. } => 1,
            Event::Button { .. } => 2,
            Event::Wheel { .. } => 3,
            Event::Key { .. } => 4,
            Event::Enter { .. } => 5,
            Event::Leave => 6,
            Event::Ping => 7,
            Event::Pong { .. } => 8,
            Event::Takeback { .. } => 9,
            Event::Anchor { .. } => 10,
        }
    }

    /// 编码。seq 用于对端丢弃乱序的旧包（UDP 不保证顺序）。
    pub fn encode(&self, seq: u16) -> Vec<u8> {
        let mut out = Vec::with_capacity(16);
        out.push(MAGIC);
        out.extend_from_slice(&self.body(seq));
        out
    }

    /// 编码去掉 MAGIC 的那部分。加密模式下这一段就是明文。
    pub fn body(&self, seq: u16) -> Vec<u8> {
        let mut out = Vec::with_capacity(12);
        out.push(self.kind());
        out.extend_from_slice(&seq.to_le_bytes());
        match *self {
            Event::Motion { dx, dy } => {
                out.extend_from_slice(&dx.to_le_bytes());
                out.extend_from_slice(&dy.to_le_bytes());
            }
            Event::Button { button, down } => {
                out.push(button);
                out.push(down as u8);
            }
            Event::Wheel { dx, dy } => {
                out.extend_from_slice(&dx.to_le_bytes());
                out.extend_from_slice(&dy.to_le_bytes());
            }
            Event::Key { code, down } => {
                out.extend_from_slice(&code.to_le_bytes());
                out.push(down as u8);
            }
            Event::Enter {
                width,
                height,
                along,
                flags,
                side,
            } => {
                out.extend_from_slice(&width.to_le_bytes());
                out.extend_from_slice(&height.to_le_bytes());
                out.extend_from_slice(&along.to_le_bytes());
                out.extend_from_slice(&flags.to_le_bytes());
                // 追加在末尾：旧版对端只读前 8 字节，多出来的直接忽略
                out.push(side);
            }
            Event::Leave | Event::Ping => {}
            // 追加 2 字节：旧版对端按 kind 认出 Takeback 就够，多余字节忽略
            Event::Takeback { along } => out.extend_from_slice(&along.to_le_bytes()),
            Event::Anchor { fx, fy } => {
                out.extend_from_slice(&fx.to_le_bytes());
                out.extend_from_slice(&fy.to_le_bytes());
            }
            Event::Pong { state } => out.push(state),
        }
        out
    }

    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < 4 || buf[0] != MAGIC {
            return None;
        }
        Self::decode_body(&buf[1..]).map(|(_, ev)| ev)
    }

    /// 解 [kind][seq][payload]，返回 (seq, 事件)。
    /// 加密模式下解密后的明文也从这里走，所以 seq 一并吐出来。
    pub fn decode_body(body: &[u8]) -> Option<(u16, Self)> {
        let kind = *body.first()?;
        let seq = u16::from_le_bytes(body.get(1..3)?.try_into().ok()?);
        let rest = &body[3..];
        let i32_at = |at: usize| -> Option<i32> {
            Some(i32::from_le_bytes(rest.get(at..at + 4)?.try_into().ok()?))
        };
        let ev = match kind {
            1 => Event::Motion {
                dx: i32_at(0)?,
                dy: i32_at(4)?,
            },
            2 => Event::Button {
                button: *rest.first()?,
                down: *rest.get(1)? != 0,
            },
            3 => Event::Wheel {
                dx: i32_at(0)?,
                dy: i32_at(4)?,
            },
            4 => Event::Key {
                code: u16::from_le_bytes(rest.get(0..2)?.try_into().ok()?),
                down: *rest.get(2)? != 0,
            },
            5 => Event::Enter {
                width: u16::from_le_bytes(rest.get(0..2)?.try_into().ok()?),
                height: u16::from_le_bytes(rest.get(2..4)?.try_into().ok()?),
                along: u16::from_le_bytes(rest.get(4..6)?.try_into().ok()?),
                flags: u16::from_le_bytes(rest.get(6..8)?.try_into().ok()?),
                // 旧版对端没有这一字节：当作"未知"，接收端继续用自己的配置
                side: rest.get(8).copied().unwrap_or(Side::CODE_UNKNOWN),
            },
            6 => Event::Leave,
            7 => Event::Ping,
            8 => Event::Pong {
                // 旧版对端的 Pong 没有状态字节：标记"未知"而不是 Idle，
                // 让状态自愈看守跳过判定（优雅降级），详见 PEER_UNKNOWN。
                state: rest.first().copied().unwrap_or(PEER_UNKNOWN),
            },
            // 旧版对端只发头部：当作"不知道位置"，回程退化为中心
            9 => Event::Takeback {
                along: rest
                    .get(0..2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .unwrap_or(0xFFFF),
            },
            // 旧版对端根本不发这个；新端发的一定带 4 字节，缺了就整条丢掉
            // （宁可少校正一次，也别拿半个坐标去拽光标）
            10 => Event::Anchor {
                fx: u16::from_le_bytes(rest.get(0..2)?.try_into().ok()?),
                fy: u16::from_le_bytes(rest.get(2..4)?.try_into().ok()?),
            },
            _ => return None,
        };
        Some((seq, ev))
    }

    /// 这个事件是不是"按下"类（释放时要补发 key-up / button-up 的清单靠它）
    pub fn is_down(&self) -> bool {
        matches!(
            self,
            Event::Button { down: true, .. } | Event::Key { down: true, .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_rejects_foreign_packet() {
        assert_eq!(Event::decode(b"\xff\x01\x00\x00"), None);
        // 0x0A 还没分配；0x09 已经是 Takeback（合法）了
        assert_eq!(Event::decode(b"\xa7\x0a\x00\x00"), None);
        assert_eq!(Event::decode(b"\xa7\x01"), None);
    }

    #[test]
    fn plain_roundtrip() {
        let cases = [
            Event::Motion { dx: -3, dy: 17 },
            Event::Button {
                button: 1,
                down: true,
            },
            Event::Wheel { dx: 0, dy: 120 },
            Event::Key {
                code: 0x0C,
                down: false,
            },
            Event::Enter {
                width: 1920,
                height: 1080,
                along: 0x8000,
                flags: 0,
                side: Side::Right.code(),
            },
            Event::Leave,
            Event::Ping,
            Event::Pong { state: 2 },
            Event::Takeback { along: 0x8000 },
            Event::Anchor {
                fx: 0x1234,
                fy: 0x8000,
            },
        ];
        for event in cases {
            let bytes = event.encode(7);
            assert_eq!(Event::decode(&bytes), Some(event), "{event:?}");
        }
    }

    #[test]
    fn takeback_without_along_means_unknown() {
        // 旧版对端的 Takeback 只有头部：回程落点退化成居中，不能崩、
        // 更不能把 0 当成"落在顶部"
        let old = [MAGIC, 9, 0x01, 0x00];
        assert_eq!(
            Event::decode(&old),
            Some(Event::Takeback { along: 0xFFFF })
        );
        // 新端发的带位置，来回一趟要一模一样
        let bytes = Event::Takeback { along: 0x1234 }.encode(3);
        assert_eq!(
            Event::decode(&bytes),
            Some(Event::Takeback { along: 0x1234 })
        );
    }

    #[test]
    fn anchor_needs_both_axes() {
        // 缺一个坐标就整条丢掉：宁可少校正一次，也不能拿半个坐标去拽光标
        let half = [MAGIC, 10, 0x01, 0x00, 0x34, 0x12];
        assert_eq!(Event::decode(&half), None);
        // 旧版对端根本不认识 kind=10，会走到 _ => None（不能崩）
        let bytes = Event::Anchor {
            fx: 0,
            fy: 0xFFFF,
        }
        .encode(1);
        assert_eq!(
            Event::decode(&bytes),
            Some(Event::Anchor {
                fx: 0,
                fy: 0xFFFF
            })
        );
    }

    #[test]
    fn pong_without_state_is_unknown() {
        // 旧版本对端的 Pong 只有头部没有状态字节：必须解成 PEER_UNKNOWN
        // 而不是 Idle，否则状态自愈看守会把混版本当成错位误杀会话
        //（v0.6.11 首日实测）。
        let old = [MAGIC, 8, 0x01, 0x00];
        assert_eq!(
            Event::decode(&old),
            Some(Event::Pong {
                state: PEER_UNKNOWN
            })
        );
    }
}
