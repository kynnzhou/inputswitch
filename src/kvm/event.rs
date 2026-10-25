//! 键鼠事件的定义与线格式。
//!
//! 两种模式共用这一套事件：
//!
//! - **跟随画面（软件 KVM）**：一套物理键鼠，画面切到哪台键鼠跟到哪台。
//!   触发是切屏动作本身，不需要"边缘"。
//! - **边缘跨界（多屏共享）**：两台各有屏幕各有键鼠，光标推到屏幕边缘就越界。
//!   由 `Side` + `hit_edge` 判定。
//!
//! 设计参照 lan-mouse（GPL-3.0）与 Deskflow（GPL-2.0）的公开架构说明，
//! 代码是自己写的：鼠标走**相对位移**而不是绝对坐标 —— 两台机器分辨率、
//! 缩放都不一样，绝对坐标要维护一套屏幕布局；相对位移由接收端累加并
//! 夹在自己的屏幕范围内，天然跨分辨率且不会漂。
//!
//! 线格式刻意做得很紧（一个事件一个 UDP 包，不合并、不确认）：
//!
//! ```text
//! [0xA7][kind][seq_lo][seq_hi][payload...]
//! ```
//!
//! 丢包就丢一个鼠标移动，局域网里几乎不会发生；用 TCP 反而会把
//! 鼠标粘住（Nagle + 重传）。lan-mouse 也是这么取舍的。
//!
//! 键码统一用 **macOS virtual keycode**：Mac 端收发都是原生值，
//! 只有 Windows 端需要维护一张映射表（见 kvm/win.rs）。

/// 包首字节。用来快速丢弃不是本协议的 UDP 包（会和发现层共用同一张网卡）。
pub const MAGIC: u8 = 0xA7;

/// Pong 里对端状态"未知"的标记。旧版本对端的 Pong 没有状态字节，
/// 解码时填这个而不是 Idle —— 状态自愈看守看到"未知"必须跳过判定
/// （没法判断就别动手），否则会把混版本当成状态错位，每 2.5 秒
/// 误杀一次会话（v0.6.11 首日实测踩过）。
pub const PEER_UNKNOWN: u8 = 0xFF;

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

/// 屏幕上的一侧。多屏共享模式下，鼠标撞到这一侧就切到隔壁那台。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Side {
    #[default]
    Right,
    Left,
    Top,
    Bottom,
}

impl Side {
    /// 协议里编码用的标记：旧版对端不带你想要的这一位
    pub const CODE_UNKNOWN: u8 = 0xFF;

    pub fn code(&self) -> u8 {
        match self {
            Side::Right => 0,
            Side::Left => 1,
            Side::Top => 2,
            Side::Bottom => 3,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Side::Right),
            1 => Some(Side::Left),
            2 => Some(Side::Top),
            3 => Some(Side::Bottom),
            _ => None,
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "right" | "右" => Some(Side::Right),
            "left" | "左" => Some(Side::Left),
            "top" | "上" => Some(Side::Top),
            "bottom" | "下" => Some(Side::Bottom),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Side::Right => "right",
            Side::Left => "left",
            Side::Top => "top",
            Side::Bottom => "bottom",
        }
    }

    pub fn opposite(&self) -> Self {
        match self {
            Side::Right => Side::Left,
            Side::Left => Side::Right,
            Side::Top => Side::Bottom,
            Side::Bottom => Side::Top,
        }
    }

    /// 四向掩码里的一位。一机配多台时，平台层拿到的不是"我这条边通向谁"，
    /// 而是"哪几条边真的有邻居"的掩码 —— 没有邻居的边连判定都不该做，
    /// 否则光标撞上去会发一个没人接的 Begin。
    pub fn bit(&self) -> u8 {
        1 << self.code()
    }

    /// 掩码里恰好只有一位时取出那条边。多位/零位都返回 None ——
    /// 调用方要的是"唯一那条边"，含糊的值不能猜。
    pub fn from_bit(mask: u8) -> Option<Self> {
        if mask.count_ones() != 1 {
            return None;
        }
        Self::from_code(mask.trailing_zeros() as u8)
    }

    /// 掩码里编码最小的那条边。只用于"挑一个代表"的场合（比如几何兜底
    /// 猜本机工作屏），不用于路由判定。
    pub fn first_in(mask: u8) -> Option<Self> {
        Self::all().into_iter().find(|s| mask & s.bit() != 0)
    }

    pub fn all() -> [Side; 4] {
        [Side::Right, Side::Left, Side::Top, Side::Bottom]
    }

    /// 出门瞬间沿门轴的位置比例（0..65535）：左右门看纵坐标，
    /// 上下门看横坐标。对端据此从对应高度/位置进门，不居中猛跳。
    pub fn along(&self, x: f64, y: f64, w: f64, h: f64) -> u16 {
        let f = match self {
            Side::Right | Side::Left => y / h,
            Side::Top | Side::Bottom => x / w,
        };
        (f * 65535.0).clamp(0.0, 65535.0) as u16
    }
}

/// 鼠标撞到哪一侧了。x/y 是当前光标位置，w/h 是屏幕尺寸，
/// 判定用"离边缘不超过 margin 点"：太严会滑不出去，太松会误触发。
pub fn hit_edge(side: Side, x: f64, y: f64, w: f64, h: f64) -> bool {
    dist_to(side, x, y, w, h) <= MARGIN
}

/// 撞边的判定余量（点）。1.5 是历史值，动它会让"滑不出去 / 误触发"
/// 两种投诉同时出现，别随手改。
const MARGIN: f64 = 1.5;

/// 角落死区（点）。两条互相垂直的边**都通向邻居**时，靠近角落的这一圈
/// 不切换 —— 斜着推向角落时用户意图不明，硬挑一条会让他"想往上却去了右"。
/// 只有一条边有邻居时这个死区不生效：那时方向是确定的，不该刁难用户。
const CORNER: f64 = 24.0;

fn dist_to(side: Side, x: f64, y: f64, w: f64, h: f64) -> f64 {
    match side {
        Side::Right => w - x,
        Side::Left => x,
        Side::Top => y,
        Side::Bottom => h - y,
    }
}

/// 在一组"通向邻居的边"（掩码）里，看光标撞上了哪一条。
///
/// 单条边时与 `hit_edge` 完全等价（掩码只有一位 → 垂直线都不在掩码里 →
/// 角落死区永不生效），所以两机场景的行为一个字节都没变。
/// 多条边时取**最近的那条**；两条垂直边同时都在附近则判为角落，不切换。
pub fn hit_edge_in(mask: u8, x: f64, y: f64, w: f64, h: f64) -> Option<Side> {
    let mut best: Option<(f64, Side)> = None;
    for side in Side::all() {
        if mask & side.bit() == 0 {
            continue;
        }
        let d = dist_to(side, x, y, w, h);
        if d > MARGIN {
            continue;
        }
        if best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, side));
        }
    }
    let (_, side) = best?;
    // 角落死区：只看"垂直方向上那两条边"，且只有它们也通向邻居时才算
    // 歧义 —— 掩码单边时这里恒为 false。
    let near = |s: Side| mask & s.bit() != 0 && dist_to(s, x, y, w, h) <= CORNER;
    let ambiguous = match side {
        Side::Right | Side::Left => near(Side::Top) || near(Side::Bottom),
        Side::Top | Side::Bottom => near(Side::Left) || near(Side::Right),
    };
    if ambiguous {
        None
    } else {
        Some(side)
    }
}

/// 门重新武装所需的"离开边缘"距离（点）。
///
/// 必须**明显大于**交还落点到边缘的距离（16 点），否则落点本身就算
/// "已经离开门口"，撤防立刻失效 —— 那正是 v0.6.49 实测的来回弹：
/// 被对端物理抢回后，本机光标被摆在边缘内侧 16 点，钩子一看到"还在
/// 边上一圈"就再次越界，用户体感是"按了键没用，鼠标又跑回去了"。
pub const REARM_DIST: f64 = 48.0;

/// 光标是否离**某条通向邻居的边**不超过 `dist` 点。
///
/// 和 `hit_edge_in` 的区别：这个只回答"在不在门口这一圈里"，不判角落、
/// 不挑边。用来做门的撤防/武装：交还之后光标就落在门口那一圈里，此时
/// 门必须保持撤防，直到用户把光标真正带回屏幕内部（≥ `REARM_DIST`）。
pub fn within_edge(mask: u8, x: f64, y: f64, w: f64, h: f64, dist: f64) -> bool {
    Side::all()
        .into_iter()
        .any(|s| mask & s.bit() != 0 && dist_to(s, x, y, w, h) <= dist)
}

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

    #[test]
    fn plain_rejects_foreign_packet() {
        assert_eq!(Event::decode(b"\xff\x01\x00\x00"), None);
        // 0x0A 还没分配；0x09 已经是 Takeback（合法）了
        assert_eq!(Event::decode(b"\xa7\x0a\x00\x00"), None);
        assert_eq!(Event::decode(b"\xa7\x01"), None);
    }

    #[test]
    fn edge_detection() {
        assert!(hit_edge(Side::Right, 1728.0, 500.0, 1728.0, 1117.0));
        assert!(!hit_edge(Side::Right, 1700.0, 500.0, 1728.0, 1117.0));
        assert!(hit_edge(Side::Left, 0.0, 500.0, 1728.0, 1117.0));
        assert!(hit_edge(Side::Top, 300.0, 1.0, 1728.0, 1117.0));
        assert!(hit_edge(Side::Bottom, 300.0, 1116.5, 1728.0, 1117.0));
        assert_eq!(Side::Right.opposite(), Side::Left);
        assert_eq!(Side::parse("下"), Some(Side::Bottom));
    }

    #[test]
    fn edge_rearm_zone_covers_landing_spot() {
        // 交还落点 = 离边 16 点。它必须落在"还没武装"的范围内，
        // 否则放开抓取的那一刻门就是开的，手一抖就又被推出去
        //（v0.6.49 实测：抢回后光标被弹回去，看起来像抢回没生效）
        let (w, h) = (1728.0, 1117.0);
        let right = Side::Right.bit();
        assert!(within_edge(right, w - 16.0, 500.0, w, h, REARM_DIST));
        // 出门必须比落点走得远得多才重新武装
        assert!(!within_edge(right, w - 60.0, 500.0, w, h, REARM_DIST));
        // 只看掩码里的边：掩码只有右边时，光标贴在左边不算"在门口"
        assert!(!within_edge(right, 16.0, 500.0, w, h, REARM_DIST));
        // 空掩码（跟随画面模式）任何位置都不算
        assert!(!within_edge(0, w - 1.0, 500.0, w, h, REARM_DIST));
        // 四向齐全时，每条边的落点都在圈里
        let all = Side::all().iter().fold(0u8, |m, s| m | s.bit());
        for (x, y) in [
            (w - 16.0, 500.0),
            (16.0, 500.0),
            (500.0, 16.0),
            (500.0, h - 16.0),
        ] {
            assert!(within_edge(all, x, y, w, h, REARM_DIST), "({x},{y}) 没被算在门口");
        }
    }

    #[test]
    fn edge_mask_matches_single_side() {
        // 掩码只有一位时，判定结果必须与老的 hit_edge 完全一致 ——
        // 两机场景（P3 上线时的全部用户）行为不许有任何变化
        for side in Side::all() {
            let mask = side.bit();
            for (x, y) in [
                (0.0, 0.0),
                (1728.0, 0.0),
                (0.0, 1117.0),
                (1728.0, 1117.0),
                (500.0, 500.0),
                (1727.0, 500.0),
                (1.0, 500.0),
                (500.0, 1.0),
                (500.0, 1116.0),
                (1727.0, 1116.5),
            ] {
                let want = if hit_edge(side, x, y, 1728.0, 1117.0) {
                    Some(side)
                } else {
                    None
                };
                assert_eq!(
                    hit_edge_in(mask, x, y, 1728.0, 1117.0),
                    want,
                    "单边掩码 {side:?} 在 ({x},{y}) 上判定变了"
                );
            }
        }
    }

    #[test]
    fn edge_mask_picks_nearest_and_skips_corners() {
        // 右边 + 下边都通向邻居：右下角 24 点内算歧义，不切换
        let mask = Side::Right.bit() | Side::Bottom.bit();
        assert_eq!(hit_edge_in(mask, 1727.0, 1116.0, 1728.0, 1117.0), None);
        // 离右边 100 多点、只碰到下边 → 不在角落，正常从下边出
        assert_eq!(
            hit_edge_in(mask, 1620.0, 1116.0, 1728.0, 1117.0),
            Some(Side::Bottom)
        );
        // 离开角落（纵向离底边 30 点）→ 右边照常
        assert_eq!(
            hit_edge_in(mask, 1727.0, 1087.0, 1728.0, 1117.0),
            Some(Side::Right)
        );
        // 离开角落（横向离右边 30 点）→ 下边照常
        assert_eq!(
            hit_edge_in(mask, 1698.0, 1116.0, 1728.0, 1117.0),
            Some(Side::Bottom)
        );
        // 没有邻居的边不参与判定
        assert_eq!(hit_edge_in(Side::Left.bit(), 1727.0, 500.0, 1728.0, 1117.0), None);
        assert_eq!(hit_edge_in(0, 0.0, 0.0, 1728.0, 1117.0), None);
        assert_eq!(Side::from_bit(Side::Top.bit()), Some(Side::Top));
        assert_eq!(Side::from_bit(mask), None, "两位不算唯一");
        assert_eq!(Side::first_in(mask), Some(Side::Right));
        // along：左右门看 y、上下门看 x
        assert_eq!(Side::Right.along(1728.0, 0.0, 1728.0, 1117.0), 0);
        assert_eq!(Side::Bottom.along(0.0, 1117.0, 1728.0, 1117.0), 0);
        assert!(Side::Left.along(0.0, 1117.0 / 2.0, 1728.0, 1117.0) > 32000);
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
