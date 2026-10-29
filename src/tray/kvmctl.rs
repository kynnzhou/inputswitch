//! 共用键鼠的启停与话术。
//!
//! 一处的开关有七八个入口（托盘菜单、面板电源、面板模式/角色/倍率、链路摆位、
//! 开机自动恢复），全部收口到 `kvm_boot` / `restart_kvm` 这两个函数。
//! 分开写的第一天就会漂：菜单改完忘了改面板，面板点了没反应，而日志里
//! 看不出是哪条路径走的。
//!
//! 另一半是"怎么把话说清楚"：对端没回应要说出去查网线还是去开开关，
//! 键鼠交没交回去不能替对端宣布。这些判据分散在三四个函数里，但面对的是
//! 同一批用户困惑，放一起才改得动。

use std::net::IpAddr;
use std::sync::mpsc;
use std::sync::Arc;

use crate::config::Config;
use crate::kvm;
use crate::peer::{Mesh, Peer};

use super::log_line;
use super::menu::side_label;
use super::{Command, KVM};

/// 从在线对端里挑出该用哪个：优先上次记住的那个，找不到才退回第一个。
/// 局域网里只要出现过第三台装过本程序的机器（旧电脑自启没关很常见），
/// peers.first() 就会稳定选错人 —— 两边各自对着错的机器发心跳，
/// 谁也等不到回应，表现就是"两边都空闲、对端没回应"。
pub fn pick_peer<'a>(peers: &'a [Peer], remembered: &str) -> Option<&'a Peer> {
    if !remembered.is_empty() {
        if let Some(p) = peers.iter().find(|p| p.node == remembered) {
            return Some(p);
        }
    }
    peers.first()
}

/// 启动共用键鼠。菜单和面板共用这一份。
///
/// 两处各写一遍的话，迟早会漂：菜单改完忘了改面板，面板点了没反应，
/// 而日志里看不出是哪条路径走的。
///
/// v3 起吃的是**整张链路表**：`config.kvm.links` 里每台对端一条，按在线
/// 情况解析成 IP。老配置（只有 `peer_node` + `side`）由 `Kvm::normalize`
/// 升成一条链路，所以两机场景的行为和 v0.6.46 完全一致。
pub fn kvm_boot(
    config: &mut Config,
    peers: &[Peer],
    tx: &mpsc::Sender<Command>,
    log: Arc<dyn Fn(String) + Send + Sync>,
) -> Result<(Arc<kvm::Kvm>, String), String> {
    // 紧急收回（Ctrl+Alt+Esc）要能连画面一起切回来。
    // 只收回键鼠的话，用户"能操作本机了，但看不见本机"，等于卡在原地。
    let panic_tx = tx.clone();
    let own_v = config.own_value;
    let own_n = config.own_input.clone();
    let on_panic: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        if own_v > 0 {
            let name = if own_n.is_empty() {
                format!("0x{own_v:02X}")
            } else {
                own_n.clone()
            };
            let _ = panic_tx.send(Command::SwitchRaw { name, value: own_v });
        }
    });

    let mode = kvm::Mode::parse(&config.kvm.mode).unwrap_or(kvm::Mode::Follow);
    let role = kvm::Role::parse(&config.kvm.role).unwrap_or(kvm::Role::Console);

    // ---- 链路表 → 在线 IP ----
    //
    // 链路解析分两遍：
    //
    // ① 在线的那几台 → 正常工作，顺手把 IP 记进配置（下次它离线时要用）
    // ② 离线但有旧 IP 的 → **仍然占住那条边**。不占的话，用户朝那边推会
    //    静默无反应，看起来跟程序坏了没区别；占住之后会得到一句
    //    「光标到了边缘，X 没有回应，不越界」。这一轮它不参与路由，
    //    所以也不会出现"先吞掉键鼠再等 2.5 秒"。
    //
    // 一台都不在线 → 退回老行为（pick_peer 挑一台），保持两机场景不变：
    // 单链路时对端换了 IP（DHCP）必须还能连上，不能被旧地址卡死。
    let mut links: Vec<kvm::Link> = Vec::new();
    let mut offline: Vec<String> = Vec::new(); // 连旧地址都没有，占不住边
    let mut dead: Vec<String> = Vec::new(); // 有旧地址，占边但只提醒
    let mut addr_fix: Vec<(String, String)> = Vec::new();

    let parse_side = |lc: &crate::config::LinkConfig, lmode: kvm::Mode| {
        if lmode == kvm::Mode::Edge {
            kvm::event::Side::parse(&lc.side)
                .or_else(|| kvm::event::Side::parse(&config.kvm.side))
                .or(Some(kvm::event::Side::default()))
        } else {
            None
        }
    };

    for lc in config.kvm.links.clone() {
        let node = lc.node.trim().to_string();
        if node.is_empty() {
            continue;
        }
        let Some(p) = peers.iter().find(|p| p.node == node) else {
            continue; // 第二遍处理
        };
        let lmode = kvm::Mode::parse(&lc.mode_or(&config.kvm.mode)).unwrap_or(mode);
        let ip = p.addr.ip();
        if lc.addr.trim() != ip.to_string() {
            addr_fix.push((node.clone(), ip.to_string()));
        }
        links.push(kvm::Link {
            node: p.node.clone(),
            ip,
            side: parse_side(&lc, lmode),
            mode: lmode,
        });
    }

    if !links.is_empty() {
        for lc in config.kvm.links.clone() {
            let node = lc.node.trim().to_string();
            if node.is_empty() || peers.iter().any(|p| p.node == node) {
                continue;
            }
            let Ok(ip) = lc.addr.trim().parse::<IpAddr>() else {
                offline.push(node);
                continue;
            };
            let lmode = kvm::Mode::parse(&lc.mode_or(&config.kvm.mode)).unwrap_or(mode);
            let side = parse_side(&lc, lmode);
            if side.is_none() {
                // 跟随画面链路没有门，占边没有任何意义
                continue;
            }
            links.push(kvm::Link {
                node: node.clone(),
                ip,
                side,
                mode: lmode,
            });
            dead.push(node);
        }
    }

    let main_node;
    if links.is_empty() {
        let Some(peer) = pick_peer(peers, &config.kvm.peer_node) else {
            return Err("没有发现任何已配对的在线对端".to_string());
        };
        let side = if mode == kvm::Mode::Edge {
            Some(kvm::event::Side::parse(&config.kvm.side).unwrap_or_default())
        } else {
            None
        };
        main_node = peer.node.clone();
        links.push(kvm::Link {
            node: peer.node.clone(),
            ip: peer.addr.ip(),
            side,
            mode,
        });
        // 顺手把它写进链路表：下次就有据可查，也是面板的起点
        config.kvm.links = vec![crate::config::LinkConfig {
            node: peer.node.clone(),
            side: config.kvm.side.clone(),
            mode: config.kvm.mode.clone(),
            input: String::new(),
            addr: peer.addr.ip().to_string(),
        }];
    } else {
        main_node = links[0].node.clone();
        for (node, addr) in addr_fix {
            if let Some(l) = config.kvm.links.iter_mut().find(|l| l.node == node) {
                l.addr = addr;
            }
        }
    }

    // 转发期间按 Ctrl+V 且手上有待传清单：抓取层吞掉键，这里把它转给引擎线程
    let paste_tx = tx.clone();
    let on_paste: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = paste_tx.send(Command::PasteIntent);
    });
    let routes: Vec<String> = links
        .iter()
        .map(|l| match l.side {
            Some(s) if l.mode == kvm::Mode::Edge => {
                format!("{}侧→{}", side_label(s.as_str()), l.node)
            }
            _ => format!("随画面→{}", l.node),
        })
        .collect();
    let handle = Arc::new(kvm::start(kvm::Options {
        links,
        mode,
        role,
        base_port: config.network.port,
        // 配对过就加密事件流（等价于 Deskflow 的默认 TLS）
        key: crate::pair::link_key(&config.pair.code),
        on_panic: Some(on_panic),
        log,
        on_paste: Some(on_paste),
        // 推边自动改指向（v0.6.60）：误判时可从配置里关掉，只剩热键
        push_hop: config.kvm.push_hop,
        // 接收位移倍率（v0.6.64）：两系统指针加速曲线不同，手感靠它微调
        recv_scale: config.kvm.recv_scale,
    })?);

    let text = match (mode, role) {
        (kvm::Mode::Follow, kvm::Role::Console) => format!(
            "共用键鼠已开启：画面切走时，键鼠跟着画面走（{}）",
            routes.join("，")
        ),
        (kvm::Mode::Follow, kvm::Role::Remote) => format!(
            "共用键鼠已开启：本机不抓键鼠，等对端把画面切过来后由它接管（{}）",
            routes.join("，")
        ),
        (kvm::Mode::Edge, _) => format!("共用键鼠已开启：{}", routes.join("，")),
    };
    if !offline.is_empty() {
        log_line(&format!(
            "{} 既不在线、也没有留下过地址，这几条边先不启用（在面板里重新连一次它就会记下地址）",
            offline.join("、")
        ));
    }
    if !dead.is_empty() {
        log_line(&format!(
            "{} 不在线，但它占的方向还留着：光标推过去不会有反应，键鼠也不会丢；等它上线就恢复",
            dead.join("、")
        ));
    }
    // 记住这次用的主链路：下次自动恢复/重开时优先找它。
    // 记住的和这次选的不一样 = 上次那台不在线，退回了第一台 ——
    // 局域网里多一台装过本程序的机器就会发生，必须说清楚。
    if config.kvm.peer_node != main_node {
        if !config.kvm.peer_node.is_empty() {
            log_line(&format!(
                "上次的对端 {} 不在线，这次先用 {}。局域网里还有别的机器装了本程序的话，\
                 关掉它的开机自启可以避免下次又选错",
                config.kvm.peer_node, main_node
            ));
        }
        config.kvm.peer_node = main_node;
    }
    // 链路表可能变了（地址回填 / 首次升上来），一并落盘
    let _ = config.save();
    Ok((handle, text))
}

/// 改链路时把正在进行的会话停下来，留下的是什么。
///
/// 一个 bool 会逼着文案替对端宣布它还没做的事：`take_back_from_peer`
/// 只是把 Takeback 发出去，对端收到才松手。所以这里必须分四态。
pub(super) enum Handback {
    /// 本来就没有会话，没什么可交还的
    None,
    /// 本机在转发 → 已经收回本机
    Back,
    /// 本机被控 → 已请对端交还（异步，不能断言收回）
    Asked,
    /// 有会话但没能正常收场
    Stuck,
}

/// 改完链路方向/模式之后重启共用键鼠实例。
///
/// 必须重启的理由：平台层的抓取钩子里**记死了边缘掩码**（哪几条边通向邻居），
/// 这是启动时按链路表算出来的。热改配置只会让"配置说的"和"钩子判的"不一致，
/// 表现是"改了方向但推那侧还是没反应"，而日志里一切正常。
///
/// `ok_text` 是成功时写进状态条的话（已含"改成了什么"），失败再往后拼。
pub fn restart_kvm(
    config: &mut Config,
    kvm_handle: &mut Option<Arc<kvm::Kvm>>,
    mesh: &Mesh,
    tx: &mpsc::Sender<Command>,
    status: &mut String,
    ok_text: String,
) {
    let was_on = kvm_handle.take();
    let mut handback = Handback::None;
    if let Some(handle) = was_on.as_ref() {
        // 改链路必然重建实例（四向掩码是启动时按链路表算出来的），正在
        // 转发的会话会被掐断。**先把键鼠交还回去再停**：不交还的话两端
        // 都以为自己在转发 —— 本机在 Sending 直接停手，对端以为还在被
        // 控制；本机在 Receiving 停掉，对端仍往一个已经没人注入的实例上
        // 发。用户看到的就是"改个方向，鼠标键盘全部失去控制"。
        handback = match handle.state() {
            kvm::State::Sending => {
                if handle.give_back() {
                    Handback::Back
                } else {
                    Handback::Stuck
                }
            }
            kvm::State::Receiving => {
                if handle.take_back_from_peer("改链路") {
                    Handback::Asked
                } else {
                    Handback::Stuck
                }
            }
            kvm::State::Idle => Handback::None,
        };
        handle.stop();
    }
    // 说清楚"这次改动打断了正在进行的转发"。不说的话，用户看到的是
    // "我刚把方向改对，键鼠却没反应了"，而其实键鼠已经回到本机 ——
    // 需要的是重新推一次边、或再点一次「切换」。
    let ok_text = match handback {
        Handback::None => ok_text,
        Handback::Back => format!("{ok_text}（键鼠已收回本机）"),
        Handback::Asked => format!("{ok_text}（已请对端交还键鼠）"),
        Handback::Stuck => format!("{ok_text}（键鼠没能收回，请按 Ctrl+Alt+Esc）"),
    };
    if let Ok(mut slot) = KVM.lock() {
        *slot = None;
    }
    if was_on.is_none() {
        *status = format!("{ok_text}，下次开启共用键鼠生效");
        log_line(status);
        return;
    }
    let peers_now = mesh.live_peers();
    match pick_peer(&peers_now, &config.kvm.peer_node) {
        None => {
            config.kvm.enabled = false;
            let _ = config.save();
            *status = format!("{ok_text}；没找到对端，共用键鼠已关闭，请重新打开");
        }
        Some(_) => {
            let log: Arc<dyn Fn(String) + Send + Sync> = Arc::new(|text| log_line(&text));
            match kvm_boot(config, &peers_now, tx, log) {
                Ok((handle, text)) => {
                    *kvm_handle = Some(handle.clone());
                    if let Ok(mut slot) = KVM.lock() {
                        *slot = Some(handle);
                    }
                    *status = format!("{ok_text}，{text}");
                }
                Err(err) => {
                    config.kvm.enabled = false;
                    let _ = config.save();
                    *status = format!("{ok_text}，但共用键鼠重启失败：{err}");
                }
            }
        }
    }
    log_line(status);
}

/// 两个系统权限都齐了吗。缺任一，共用键鼠要么开不起来、要么只能单向。
pub fn perms_ok() -> bool {
    kvm::trusted() && kvm::can_post()
}

///
/// `link_alive` 只看**共用键鼠的心跳包**。对端程序开着、但它的共用键鼠
/// 关着的时候，心跳一个都不会来 —— 表现和对端关机、不在同一网段、
/// 防火墙拦了**完全一样**。announce 里带了 `kvm_on` 能力位，于是这两种
/// 情况现在能分开说：让人去对面把开关打开，而不是去查网线。
pub(super) fn explain_silent_peer(mesh: &Mesh, node: Option<&str>, err: &str) -> String {
    let peers = mesh.live_peers();
    let hit = match node {
        Some(n) => peers.iter().find(|p| p.node == n),
        // 不知道是哪台（follow 链路里唯一那条）时：在线的那几台里
        // 只要有键鼠没开的，它就是最可疑的那个
        None => peers.iter().find(|p| !p.kvm_on),
    };
    match hit {
        Some(p) if !p.kvm_on => format!("{err}（{} 的程序在线，是它的共用键鼠没开）", p.node),
        _ => err.to_string(),
    }
}

/// 一条链路的方向（`follow_target` 用的是同一条判据：第一条 Follow 链路）。
pub(super) fn follow_link_node(handle: &kvm::Kvm) -> Option<String> {
    handle
        .links()
        .iter()
        .find(|l| l.mode == kvm::Mode::Follow)
        .map(|l| l.node.clone())
}

/// 自动恢复失败后的等待时间（秒）：1 → 5 → 15 → 60 → 300，之后固定 300。
///
/// 为什么不是指数退避：副机开机时的失败几乎都是"网络还没起 / 对端还没上线"，
/// 几十秒内就会好；指数退避会让它几分钟都不再试，用户看到的就是"自启没生效"。
/// 也不能一秒一次：真失败（缺授权）会刷屏，还会反复弹系统设置页。
pub(super) fn resume_backoff(failures: u32) -> u64 {
    match failures {
        0 | 1 => 1,
        2 => 5,
        3 => 15,
        4 => 60,
        _ => 300,
    }
}
