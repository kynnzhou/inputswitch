//! 共用键鼠：一套键鼠控制两台机器。**两种模式，覆盖两种真实场景。**
//!
//! ```text
//! 模式 A「跟随画面」= 软件 KVM
//!   显示器 ──┬── 主机 A（键鼠插在这台）   ← Console
//!            └── 主机 B                  ← Remote
//!   画面在 A：A 正常用，程序什么都不做
//!   画面切到 B：A 把自己收到的键鼠事件全部转发给 B，B 注入回本机
//!   画面切回 A：A 停止转发，键鼠立刻回到本机
//!
//! 模式 B「边缘跨界」= 多屏共享
//!   [ 屏幕 A ][ 屏幕 B ]   两台各有屏幕、各有键鼠
//!   光标推到 A 的右边缘 → 越界到 B；在 B 推回左边缘 → 回到 A
//! ```
//!
//! **两者不能混用，因为物理前提不同：**
//!
//! - 模式 A 只有**一块屏、一套键鼠**，另一台机器根本没显示器（画面切走时系统
//!   会认为显示器被拔掉了）。这时**没有"边缘"可撞** —— 控制端的画面已经切走，
//!   用户看不见它也不知道光标在哪，靠撞边缘触发等于让人盲操作。所以触发点
//!   只能是「画面切换」本身，而且角色是固定的：键鼠插在谁身上谁是 Console。
//! - 模式 B 两台都有屏幕、都有键鼠，谁也不欠谁，角色是对等的，靠边缘判定越界。
//!
//! 早期版本只实现了模式 B，却当成通用方案——在那台没显示器的机器上
//! 永远等不到"鼠标撞边缘"，于是看起来就像完全没工作。
//! 架构参照两个项目的公开设计，代码都是自己写的（两者都是 GPL，只能借鉴）：
//!
//! - **lan-mouse**（Rust，GPL-3.0）：事件流形态、注入打标记防回环、交还补 key-up。
//! - **Deskflow**（C++，GPL-2.0，Synergy 血统）：心跳 + 空闲超时、事件流加密、
//!   断线自动交还。这三条是只看 lan-mouse 时漏掉的。
//!
//! 四条关键规则：
//! 1. **注入要打标记**：注入的事件仍会经过自己的抓取钩子，必须能认出来并放行，
//!    否则 B 注入的鼠标会被 A 当成真实输入再发回去，来回放大。
//! 2. **松手要补 key-up**：转发期间按下的键，对端只收到 down 没收到 up，
//!    交还时如果不补，对面会一直以为 Ctrl 还按着，之后所有点击都变组合键。
//! 3. **必须有心跳**：UDP 无连接，对端崩了本机不知道。没有心跳的话本机会
//!    永远停在"转发中"，把自己的键鼠全吞掉 —— 等于失联。Deskflow 用 2 秒
//!    空闲超时判定断线，这里给 2.5 秒。
//! 4. **开转之前要先确认对端活着**：否则画面切过去、键鼠也交出去，结果对端
//!    没开程序 —— 用户面前是一块显示着 B 的屏，键鼠却谁也控制不了。
//!
//! 输入事件走独立 UDP 端口（`network.port + 3`），不进发现层的 JSON 通道：
//! 鼠标 1000Hz，JSON 序列化扛不住。配对后用配对码派生的密钥做
//! ChaCha20-Poly1305 加密（Deskflow 是 TLS，效果等价：都保证只有配对过的
//! 那两台读得懂）。

pub mod event;
mod capture;
mod handle;
mod link;
mod linker;
mod opts;
mod recv;
mod rx;
mod session;
mod state;
mod tuning;
mod util;
mod watchdog;

#[cfg(target_os = "macos")]
mod mac;
#[cfg(windows)]
mod win;

// 状态机词汇表，见 `state.rs` 顶部说明。再导出保持外部 `kvm::Mode` 等路径不变。
pub use state::{Captured, Mode, Role, State};
// 启动参数，`opts.rs`。`start()` 的入参与外部装配面。
pub use opts::{Link, Options};
// 公开句柄，`handle.rs`。外部只通过它接触 KVM，字段 pub(super) 归 start() 构造。
pub use handle::Kvm;

use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use event::{Event, Sealer, Side};

// 每对端心跳表，`rx.rs`。方法 pub(super)，本模块独占。
use rx::RxTable;

// 链路判据自由函数，`link.rs`。主循环、Kvm 句柄从这里取。
use link::*;
// 会话共享状态，`session.rs`。字段被下面的主循环直接读写，故 pub(super)。
use session::{Active, Out};
// 零散辅助函数，`util.rs`。三个线程体从这里取。
use util::*;

#[cfg(target_os = "macos")]
use mac as plat;
#[cfg(windows)]
use win as plat;

pub use plat::Inject;

/// 平台层抓取参数。
///
/// 用命名结构体而不是几个位置参数：Mac 与 Windows 两份实现要逐个对位，
/// `sides`（u8）和 `edge_trigger`（bool）一旦传反，只有交叉编译那次才会
/// 暴露出来，而且症状是"某条边莫名不响应"，排查成本极高。
pub struct CaptureOpts {
    /// 四向掩码：哪几条边通向邻居。0 = 没有门（跟随画面模式）。
    /// P3 阶段由单一 `side` 折算，P4 之后由链路表生成。
    pub sides: u8,
    /// 没抓取时要不要盯着屏幕边缘（只有多屏共享模式需要）
    pub edge_trigger: bool,
}

/// 本机有没有抓取键鼠的权限
pub fn trusted() -> bool {
    plat::trusted()
}

/// 能不能往系统里注入事件
pub fn can_post() -> bool {
    plat::can_post()
}

/// 打开系统的授权设置页（只有 macOS 需要）
pub fn open_settings() {
    plat::open_accessibility_settings();
}

/// 主屏尺寸（点）
pub fn screen_size() -> (u32, u32) {
    let (w, h) = plat::screen_size();
    (narrow(w), narrow(h))
}

/// 启动。Console 会装上抓取钩子但**不吞事件**；只有 `follow()` 之后才开始抓。
pub fn start(opts: Options) -> Result<Kvm, String> {
    let port = opts.base_port.saturating_add(3);
    if opts.links.is_empty() {
        return Err("没有配置任何对端链路".to_string());
    }
    let links = Arc::new(opts.links);
    let active = Active::new();
    // 会话起点：多跳的回程目标（见 Kvm::root）
    let root = Active::new();
    // 改指向的残包闸与节流
    let retarget_guard: Arc<Mutex<Option<(usize, u64)>>> = Arc::new(Mutex::new(None));
    let hop_ms = Arc::new(AtomicU64::new(0));
    // 四向掩码：只有 Edge 链路占边。Follow 链路靠画面切换触发，
    // 不产生"撞边缘"事件 —— 混进掩码的话，用户朝那侧推一下就会
    // 在画面没切走的情况下被吞掉键鼠。
    let sides_mask = link_mask(&links);
    let any_edge = links.iter().any(|l| l.mode == Mode::Edge);
    // 链路级判据，不是全局 opts.mode：v0.6.58 起模式下沉到每台对端，
    // 「全局 edge + 单条 follow 链路」是合法配置，这时本机照样要抓键鼠
    // 才能在切屏时把它送过去。用全局值会让那种配置下 follow() 直接报
    // 「抓取模块没有启动」。
    let any_follow = links.iter().any(|l| l.mode == Mode::Follow);
    if sides_mask == 0 && any_edge {
        return Err(
            "边缘跨界链路没有填方向：每条 edge 链路都要说清从本机哪一侧出去".to_string(),
        );
    }

    // 收事件要在固定端口上等着；发送用同一个 socket 发回去即可
    let socket = UdpSocket::bind(("0.0.0.0", port))
        .map_err(|e| format!("KVM 端口 {port} 绑定失败：{e}"))?;
    socket
        .set_read_timeout(Some(Duration::from_millis(200)))
        .map_err(|e| format!("设置超时失败：{e}"))?;

    // 两个角色要的权限不一样，别把 Console 的检查套到 Remote 头上：
    // Remote 只需要"能注入"，它根本没有键鼠可抓。
    // Edge 模式下两边都是对等的，两边都要能抓能注入。
    // 权限按**能力**分两块，不再按 role 一刀切（P5 控制台迁移的前提）：
    //
    //   can_post（注入）= 硬需求：没有它连"被控制"都做不到，直接报错
    //   trusted（抓取）= 软需求：没有它本机不能主动把键鼠推出去，但仍然
    //                    可以被对面控制 —— 降级，不罢工
    //
    // 这两项在 macOS 上确实是两个独立授权（AXIsProcessTrusted vs
    // CGPreflightPostEventAccess），所以"一个有一个没有"是真实状态。
    if !plat::can_post() {
        return Err(
            "缺少权限，没法把对端的键鼠注入本机（macOS：系统设置 → 隐私与安全性 → 辅助功能）。\
             **勾完必须重启本程序**：macOS 把 TCC 结论缓存在进程里，已经在跑的进程\
             即使你刚勾上也还是「没权限」，从托盘菜单点「重启应用」即可"
                .into(),
        );
    }
    let capture_ok = plat::trusted();
    if !capture_ok {
        (opts.log)(
            "⚠️ 缺少抓取权限：本机只能被对端控制，键鼠推不出去\
             （macOS：系统设置 → 隐私与安全性 → 辅助功能）——\
             如果刚勾上，从托盘菜单点「重启应用」让它生效"
                .to_string(),
        );
    }
    // Follow 模式两端都要能抓：键鼠插在哪台是不确定的（用户随时可能把
    // 键盘挪到另一台），所以不能用 role 决定谁装钩子。
    let needs_capture =
        capture_ok && (any_edge || any_follow || opts.role == Role::Console);

    let state = Arc::new(AtomicU8::new(State::Idle as u8));
    let stop = Arc::new(AtomicBool::new(false));
    // 每对端一份：最后一次收到包的时刻 + 它报的状态（0=Idle 1=Sending
    // 2=Receiving）。控制消息单发 UDP 会丢，丢一包两边状态机就错位卡死；
    // 心跳 0.8s 一次每次都带状态，任何错位 2.5 秒内必然被看见并自愈。
    let peer_rx = RxTable::new();
    // 本机最近一次"收回键鼠"的时间戳：收回后短暂冷却 Begin，防止用户的
    // 手还压在门口就立刻再次越界（日志实测 0.1s 内反复乒乓）
    let last_reclaim_ms = Arc::new(AtomicU64::new(0));
    // 最近一次进入**被控**（Receiving）的时刻：物理抢回要对它算保护窗。
    //
    // 为什么不放 `Session` 里：抢回信号是抓取线程收的，而 `Session` 归
    // 接收线程独占；两边共用一个原子量比把 Session 上锁便宜得多。
    let last_recv_ms = Arc::new(AtomicU64::new(0));

    let sealer = opts
        .key
        .map(|key| Sealer::new(&key, Sealer::fresh_prefix()));
    let encrypted = sealer.is_some();
    let out = Arc::new(Out {
        socket,
        port,
        links: links.clone(),
        active: active.clone(),
        sealer: Mutex::new(sealer),
        seq: Mutex::new(0),
    });

    // ---- 抓取方向 ----
    //
    // Follow 模式只有键鼠端需要抓；Edge 模式两边对等，都要抓。
    // `edge_trigger` 决定"没抓取时"要不要盯着边缘：只要有 Edge 链路就需要，
    // Follow 链路装了钩子但完全不判断位置（否则切屏后光标停在边缘就会误触发）。
    // 线程本体在 `capture/`（原 `capture.rs` 600 行，按 match 分支拆成六文件）。
    let mut capture: Option<Arc<plat::Capture>> = None;
    if needs_capture {
        let (tx, rx) = mpsc::channel::<Captured>();
        let cap = Arc::new(plat::start_capture(
            CaptureOpts {
                sides: sides_mask,
                edge_trigger: any_edge,
            },
            tx,
        )?);
        // 参数顺序与 `capture/mod.rs::spawn` 的签名一一对应（= 当年闭包的
        // 捕获清单），最后一个 `rx` 是直接移进闭包的那条通道。
        capture::spawn(
            state.clone(),
            stop.clone(),
            out.clone(),
            cap.clone(),
            opts.log.clone(),
            opts.on_panic.clone(),
            peer_rx.clone(),
            last_reclaim_ms.clone(),
            opts.on_paste.clone(),
            links.clone(),
            active.clone(),
            root.clone(),
            retarget_guard.clone(),
            hop_ms.clone(),
            last_recv_ms.clone(),
            opts.push_hop,
            sides_mask,
            rx,
        );
        capture = Some(cap);
    }

    // ---- 接收方向：对端发来的输入 → 注入本机 ----
    // 线程本体在 `recv.rs`（730 行，三个线程里最大的一个）。
    recv::spawn(
        state.clone(),
        stop.clone(),
        out.clone(),
        peer_rx.clone(),
        opts.log.clone(),
        opts.key,
        capture.clone(),
        last_recv_ms.clone(),
        links.clone(),
        active.clone(),
        root.clone(),
        retarget_guard.clone(),
        hop_ms.clone(),
        // 手感倍率：接收线程自己算一份（发送线程那份在另一个块里）
        opts.recv_scale.clamp(0.5, 2.0),
        last_reclaim_ms.clone(),
        sides_mask,
    );

    // ---- 心跳 + 断线看守 ----
    //
    // UDP 没有连接状态。对端进程崩了、网线拔了、Wi-Fi 断了，本机这边
    // 一个字节都收不到，但仍然停在 Sending —— 于是把自己的键鼠全吞了，
    // 用户看到的是"鼠标没了、键盘也死了"。
    // 线程本体在 `watchdog.rs`（260 行：发心跳 + 断线自愈 + 错位自愈）。
    watchdog::spawn(
        state.clone(),
        stop.clone(),
        out.clone(),
        peer_rx.clone(),
        opts.log.clone(),
        capture.clone(),
        last_reclaim_ms.clone(),
        links.clone(),
        active.clone(),
        root.clone(),
        sides_mask,
    );

    let secret = if encrypted {
        "事件流已加密"
    } else {
        "未配对，事件流是明文，同网段可嗅探"
    };
    // 每条链路一行说清楚"从哪侧出去能到谁"。多链路时这句话就是用户
    // 排障时唯一能对照拓扑的凭据 —— 方向配反了，日志里一眼能看出来。
    let routes: Vec<String> = links
        .iter()
        .map(|l| match l.side {
            Some(side) if l.mode == Mode::Edge => format!(
                "{}侧→{}",
                match side {
                    Side::Right => "右",
                    Side::Left => "左",
                    Side::Top => "上",
                    Side::Bottom => "下",
                },
                l.node
            ),
            _ => format!("随画面→{}", l.node),
        })
        .collect();
    // 文案按**链路表**说，不按全局 opts.mode：v0.6.58 起一台机器可以
    // 同时挂"共用一块屏"和"旁边并排"两种链路，全局值只能说出其中一半，
    // 用户照着日志排查时会被引到错的方向。
    let headline = match (any_follow, any_edge) {
        (true, true) => "混合模式：共用屏那台跟画面走，并排那台推到边才过去",
        (true, false) => match opts.role {
            Role::Console => "跟随画面模式，本机是键鼠端 —— 画面切到对端时自动跟过去",
            Role::Remote => "跟随画面模式，本机不抓键鼠，等对端接管",
        },
        (false, true) => "多屏共享模式：光标推到边缘越界",
        (false, false) => "还没有配置任何对端链路",
    };
    (opts.log)(format!(
        "共用键鼠已就绪（{secret}）· {headline}（{}）",
        routes.join("，")
    ));

    Ok(Kvm {
        state,
        stop,
        mode: opts.mode,
        role: opts.role,
        capture,
        out,
        peer_rx,
        links,
        active,
        root,
        retarget_guard,
        hop_ms,
        log: opts.log,
    })
}

#[cfg(test)]
mod tests;
