//! 面板快照：把散在各处的状态压成面板要的一份 Snapshot。
//!
//! 唯一的汇聚点。面板不自己推 mode/role 的含义（两套判据会对不上），
//! 也不直接摸 KVM 实例 —— 都从这里出。

use std::sync::atomic::Ordering as AtomicOrdering;
use std::sync::{Arc, Mutex};

use crate::config::Config;
use crate::kvm;
use crate::models::Presence;
use crate::panel::{self, Snapshot};
use crate::peer::{Mesh, Peer};

use super::hotkey::pretty_hotkey;
use super::menu::{port_rows, PortRow};
use super::{kvm_now, HAS_DISPLAY, PANEL_PORT, PORT_NOTE};

/// 把当前所有状态压成面板要的一份快照。
#[allow(clippy::too_many_arguments)]
pub(super) fn update_snapshot(
    snapshot: &Arc<Mutex<Snapshot>>,
    status: &str,
    support: &str,
    target: &str,
    config: &Config,
    profile_key: &str,
    peers: &[Peer],
    current: Option<u16>,
    mesh: &Mesh,
    transfer_note: &str,
) {
    let mut snap = snapshot.lock().unwrap();
    snap.node = config.node.clone();
    snap.status = status.to_string();
    snap.support = support.to_string();
    snap.hotkey = pretty_hotkey(&config.hotkey);
    snap.target = if target.is_empty() {
        "未设置".to_string()
    } else {
        config.label_for(target)
    };
    snap.paired = config.pair.is_set();
    snap.pair_code = config.pair.code.clone();
    snap.rejected = mesh.rejected();
    snap.replayed = mesh.replayed();
    snap.peer_verified = peers.iter().filter(|p| p.paired).count();
    snap.has_display = HAS_DISPLAY.load(AtomicOrdering::SeqCst);
    snap.own_value = config.own_value;
    // 共用键鼠：开没开、跑哪种模式、现在键鼠在哪边、每条链路什么状态
    //
    // 顺手把"本机开没开"告诉网络层，随下一条 announce 广播出去。选在这里
    // 对齐是因为它跑得够勤（面板轮询 + 托盘 5s 刷新），而且 KVM 的所有
    // 启停路径最后都会反映到 `kvm_now()` —— 在七八个启停点上各写一遍，
    // 漏一个就是长期错误广播。
    mesh.set_kvm_on(kvm_now().is_some());
    snap.kvm = match kvm_now() {
        Some(handle) => {
            let active_idx = handle.active_index();
            panel::KvmView {
                on: true,
                mode: handle.mode().as_str().to_string(),
                role: handle.role().as_str().to_string(),
                state: handle.state().label().to_string(),
                peer_alive: handle.peer_alive(),
                active: handle.active_node().unwrap_or("").to_string(),
                // 两个布尔都由后端算：面板不该自己拿 mode/role 去推，
                // 那套判据（links[0] 的 mode）跟 do_switch 用的
                // has_follow_link()（任一链路）压根不是一回事
                follow_link: handle.has_follow_link(),
                will_follow: handle.has_follow_link() && handle.can_capture(),
                // 起点只在"键鼠已经转到别台"时才有意义，单跳会话不标
                // （否则每场会话都给当前那台挂个「起点」，纯噪音）
                root: if handle.at_root() {
                    String::new()
                } else {
                    handle.root_node().unwrap_or("").to_string()
                },
                links: handle
                    .links()
                    .iter()
                    .enumerate()
                    .map(|(i, link)| panel::LinkView {
                        node: link.node.clone(),
                        addr: link.ip.to_string(),
                        // 空 = 无门（Follow 链路），界面上不画方向
                        side: link
                            .side
                            .map(|s| s.as_str().to_string())
                            .unwrap_or_default(),
                        mode: link.mode.as_str().to_string(),
                        alive: handle.alive_at(i),
                        active: active_idx == Some(i),
                        // 键鼠已经在别台手上、这台又在线 ⇒ 点它是"改指向"
                        // （画面不动），不是"建一场新会话"
                        can_retarget: handle.state() == kvm::State::Sending
                            && active_idx != Some(i)
                            && handle.alive_at(i),
                    })
                    .collect(),
                // 手感旋钮的真实值：从配置读，不从实例读 —— 实例里的
                // 是启动快照，用户改完还没重启时两者会不一样，界面该显示
                // "即将生效"的那个值。
                recv_scale: config.kvm.recv_scale,
                auto_resume: config.kvm.auto_resume,
                keep_alive: config.kvm.keep_alive,
            }
        }
        None => panel::KvmView {
            on: false,
            mode: config.kvm.mode.clone(),
            role: config.kvm.role.clone(),
            state: "未启动".to_string(),
            peer_alive: false,
            active: String::new(),
            // 没起来时按**配置**判：摆位图和提示必须跟重启后的行为一致，
            // 否则用户会照着错的图去拖（地址/模式在 v3 里是链路级字段，
            // 空 mode 表示沿用 kvm.mode，所以要用 mode_or 折算）
            follow_link: config
                .kvm
                .links
                .iter()
                .any(|l| l.mode_or(&config.kvm.mode) == "follow"),
            // 没开着就没有任何东西会跟
            will_follow: false,
            root: String::new(),
            // 没启动时也要把配置里的链路画出来，否则用户看不到、也就改不了
            links: config
                .kvm
                .links
                .iter()
                .map(|link| panel::LinkView {
                    node: link.node.clone(),
                    addr: link.addr.clone(),
                    side: link.side.clone(),
                    mode: link.mode_or(&config.kvm.mode),
                    alive: false,
                    active: false,
                    can_retarget: false,
                })
                .collect(),
            recv_scale: config.kvm.recv_scale,
            auto_resume: config.kvm.auto_resume,
            keep_alive: config.kvm.keep_alive,
        },
    };
    snap.port_note = PORT_NOTE
        .lock()
        .map(|note| note.clone())
        .unwrap_or_default();
    snap.autostart = crate::autostart::is_enabled();
    snap.clipboard = config.clipboard.enabled;
    snap.inbox = config.transfer.inbox_dir().display().to_string();
    snap.port = PANEL_PORT.load(AtomicOrdering::SeqCst);
    // 权限由本进程（主程序）自判：CLI 探针是另一个二进制、另一个 cdhash，
    // TCC 只认主程序，它永远报 ✗，不能当判据
    snap.perms = panel::PermsView {
        capture: kvm::trusted(),
        inject: kvm::can_post(),
    };
    snap.transfer = transfer_note.to_string();
    // 传输任务清单：注册表自己管状态（自带锁），这里只把快照搬进面板。
    // 徽标 / 列表 / 暂停取消全读这一份，前端不自己推阶段
    snap.transfers = crate::transfer::tasks::views();
    snap.identity = profile_key.to_string();
    // 自动更新：update 模块自己管状态（自带锁和单飞闸），这里只把快照
    // 搬进面板。徽章 / 弹窗 / 重启过渡层全读这一份，前端不自己推阶段
    snap.update = crate::update::snapshot();
    snap.display = config
        .profile_for(profile_key)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "—".to_string());
    snap.current = current.unwrap_or(0);
    snap.current_name = current
        .and_then(|value| config.input_name_of(profile_key, value))
        .map(|name| config.label_for(&name))
        .unwrap_or_default();

    snap.peers = peers
        .iter()
        .map(|peer| panel::PeerView {
            node: peer.node.clone(),
            addr: peer.addr.ip().to_string(),
            input: peer.own_input.clone(),
            label: if peer.own_input.is_empty() {
                String::new()
            } else {
                config.label_for(&peer.own_input)
            },
            value: peer.own_value,
            paired: peer.paired,
            knows_port: peer.knows_port(),
            needs_pair: peer.wants_pair && !config.pair.is_set(),
            kvm_on: peer.kvm_on,
        })
        .collect();

    let to_view = |row: &PortRow| panel::InputView {
        name: row.name.clone(),
        label: row.label.clone(),
        value: row.value,
        current: row.current,
        presence: match row.presence {
            Presence::Occupied => "occupied",
            Presence::Idle => "idle",
            Presence::Invalid => "invalid",
            Presence::Unknown => "unknown",
        }
        .to_string(),
        verified: row.verified,
    };
    // 面板只列有本机证据的口；其余（固件声明过但没验证、也没信号）留在
    // inputs_hidden 里仅供 /api/state 排查，界面上一律不出现 —— 包括不再
    // 提供"仍然显示"的入口。要用它们：接了新设备就扫描，换显示器就重新检测。
    let (shown, hidden): (Vec<PortRow>, Vec<PortRow>) = port_rows(config, profile_key, current, true)
        .into_iter()
        .partition(PortRow::has_evidence);
    snap.inputs = shown.iter().map(to_view).collect();
    snap.inputs_hidden = hidden.iter().map(to_view).collect();
}
