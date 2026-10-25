//! 工作线程：切屏 + 状态推送 + 键鼠跟随决策。
//!
//! 独占一个 Switcher（DDC 不能并发），串行处理 Command。UI 线程只发命令、
//! 收 UserEvent，两边不共享任何可变状态。
//!
//! `do_switch` 的键鼠结论是整个文件里最容易说谎的地方（七个分支，说错一句
//! 用户就要去按显示器按钮），所以它单独一个函数、单独一个 Keys 枚举，
//! 不让任何调用方自己拼文案。

use std::sync::atomic::Ordering as AtomicOrdering;
use std::sync::mpsc::Receiver;
use std::thread;
use std::time::{Duration, Instant};

use crate::kvm;
use crate::models::{Presence, VCP_BRIGHTNESS, VCP_INPUT_SOURCE};
use crate::peer::Mesh;
use crate::transfer;
use crate::Switcher;
use tao::event_loop::EventLoopProxy;

use super::kvm_now;
use super::switch::do_switch;
use super::{log_line, CLIP_BUSY, CLIP_PENDING, Command, HAS_DISPLAY, PORT_NOTE, UserEvent};

/// 当前显示器的身份键；屏幕已经切走时为空串。
pub(super) fn profile_key_of(switcher: &Switcher) -> String {
    switcher
        .pick_display(None)
        .map(|d| switcher.identity(&d))
        .unwrap_or_default()
}

pub(super) fn status_text(switcher: &mut Switcher) -> String {
    let displays = switcher.displays();
    if displays.is_empty() {
        return "本机没有接通的显示器（画面在对面那台）".to_string();
    }
    let display = &displays[0];
    // 静默期（显示器刚切回来）用记忆里的口拼状态，绝不再发 DDC 读
    if switcher.ddc_quiet() {
        // 走 label_for：`own_input` 是配置键（`usbc` / `hdmi1`），直接印出来
        // 就是「P275MV PLUS · usbc」——托盘第一行最显眼的位置放一个内部标识，
        // 用户既看不懂也不知道跟下面那行「USB-C」是同一个口。
        let known = if switcher.config.own_input.is_empty() {
            format!("输入源 0x{:02X}", switcher.config.own_value)
        } else {
            switcher.config.label_for(&switcher.config.own_input)
        };
        return format!("{} · {}", display.name, known);
    }
    let cap = switcher.capability(display);
    // `current_input_name` 读到的是**配置键**（`usbc`）或者查不到时的
    // `输入源 0x12`。两者都过一遍 label_for：键换成口名，查不到的原样返回。
    // 托盘第一行是用户看得最多的一行，不该出现内部标识。
    let known = switcher
        .current_input_name(display)
        .map(|name| switcher.config.label_for(&name))
        .unwrap_or_else(|| "输入源未知".to_string());
    let read_ok = switcher.backend().get_vcp(display, VCP_BRIGHTNESS).is_some();
    if read_ok {
        format!("{} · {}", display.name, known)
    } else {
        format!("{} · {} {}", display.name, cap.mark(), "只能切，读不到当前输入")
    }
}


/// 把"本机现在占哪个口"同步给局域网广播。
///
/// 只有屏幕在本机时才读得到 VCP 0x60；读不到就保持上一次的值，
/// 对面仍然能看到"这台机器上次占的是哪个口"。
///
/// 返回本次读到的当前输入值（读不到返回 None）。不管有没有落盘，
/// 调用方都可以拿它判断"画面现在在哪"。
pub(super) fn sync_own(switcher: &mut Switcher, mesh: &Mesh) -> Option<u16> {
    // 静默期（显示器刚离开/回来）不读 DDC，保持上一次的值即可
    if switcher.ddc_quiet() {
        return None;
    }
    // 画面正显示对端时，0x60 读到的是**对端的口**。有的显示器切走后
    // 仍挂在本机拓扑里、DDC 照答不误（实测），照单全收会把对端的口记成
    // "本机的口"——之后对端请我们把画面切回去时，value != own 走错分支，
    // 键鼠收不回来（实测踩过）。两个信号任一命中就跳过：
    // 1) KVM 正在转发（键鼠已跟过去 = 画面在对端那边）
    // 2) 对端广播过它自己就挂在这个口
    let sending = kvm_now().is_some_and(|h| h.state() == kvm::State::Sending);
    let display = switcher.pick_display(None)?;
    let value = switcher.backend().get_vcp(&display, VCP_INPUT_SOURCE)?;
    let peer_port = mesh
        .live_peers()
        .iter()
        .any(|p| p.knows_port() && p.own_value == value);
    if !sending && !peer_port {
        let key = switcher.identity(&display);
        let name = switcher
            .config
            .input_name_of(&key, value)
            .unwrap_or_else(|| format!("0x{value:02X}"));
        // 顺手落盘：屏幕被切走之后就读不到了，这个值是请求对端切回来的唯一依据
        switcher.config.remember_own(&name, value);
        mesh.set_own(&name, value);
    }
    Some(value)
}

/// 体检结论：第一个字符串放菜单第二行，第二个是完整说明（进日志/状态栏）。
///
/// `support::check` 会做一次写探测，约 1 秒，所以调用方必须缓存结果，
/// 不能挂在每 5 秒一次的状态刷新里。
pub(super) fn inspect(switcher: &Switcher) -> (String, String) {
    let Some(display) = switcher.pick_display(None) else {
        return (String::new(), "屏幕不在本机，无法检测".to_string());
    };
    let caps = switcher.capabilities(&display);
    let report = crate::support::check(switcher.backend(), &display, caps);
    let verdict = report.verdict();

    let line = format!("{} {}", verdict.mark(), verdict.label());

    let mut detail = report.headline();
    if !verdict.advice().is_empty() {
        detail.push('。');
        detail.push_str(verdict.advice());
    }
    (line, detail)
}

// 这里曾经有 absorb_peer_ports：把对端广播的"我挂在某个口"直接写进本机
// 这块显示器的输入口表。
//
// v0.6.35 删掉。原因是它把**别台显示器的编号**搬了过来：VCP 0x60 的取值
// 是每台显示器固件自己定义的一套编号，对端在它那块屏上是 0x12，写进本机
// 这块屏就成了"0x12 有主机"——可本机这个口根本没接东西。用户看到的就是
// 一个绿点幽灵口，点「切换」直接黑屏。
//
// 「输入源」这个区域只回答一个问题：**这台显示器**的几个口各自接了什么。
// 答案只能来自本机的 DDC（自己读、自己切、自己扫）。对端那台机器接在
// 它自己的显示器上，它的口编号对本机没有任何意义，所以一条都不该进来。
//
// 对端的口仍然在广播里（KVM 的 sync_own 要用它防误记），只是不再变成
// 本机的输入源结论。
pub(super) fn engine_loop(
    mut switcher: Switcher,
    rx: Receiver<Command>,
    proxy: EventLoopProxy<UserEvent>,
    mesh: Mesh,
) {
    /// 只在状态真的变了才往 UI 线程丢事件。
    ///
    /// 5 秒一次的自动刷新绝大多数时候结论完全一样，无条件重发会让 UI 层
    /// 每 5 秒重建一次菜单，macOS 上就表现为"菜单点开几秒后自己消失"。
    fn push(
        proxy: &EventLoopProxy<UserEvent>,
        switcher: &Switcher,
        target: &str,
        text: String,
        support: &str,
        last: &mut String,
    ) {
        let key = profile_key_of(switcher);
        // 顺便刷新"本机有没有显示器"：面板要靠它区分"没接显示器"和"显示器不报口"
        let has_display = !switcher.displays().is_empty();
        let had_display = HAS_DISPLAY.swap(has_display, AtomicOrdering::SeqCst);
        // 画面刚切回本机（用户按了显示器面板按钮、或对面替我们切回来了）：
        // 键鼠必须立刻回到本机，否则就变成"看着本机的画面、操作着对面"。
        if has_display && !had_display {
            if let Some(handle) = kvm_now() {
                handle.give_back();
            }
        }
        let sig = format!("{key}\u{1f}{target}\u{1f}{text}\u{1f}{support}");
        if *last == sig {
            return;
        }
        *last = sig;
        let _ = proxy.send_event(UserEvent::Status {
            text,
            target: target.to_string(),
            profile_key: key,
            support: support.to_string(),
        });
    }

    let mut last_push = String::new();
    // 上一轮见到的显示器名集合，心跳里用来发现拓扑变化（零 I2C 开销）
    let mut last_topology: Vec<String> = Vec::new();
    // 拓扑刚变过 → 等 DDC 静默期过去再自动检测（见 Heartbeat）
    let mut pending_detect: Option<Instant> = None;
    let start_target = switcher.config.hotkey_target.clone();
    let _ = sync_own(&mut switcher, &mesh);
    let text = status_text(&mut switcher);
    // 只算一次：写探测要 1 秒，不能跟着状态刷新反复跑
    let mut support = inspect(&switcher).0;
    push(&proxy, &switcher, &start_target, text, &support, &mut last_push);

    while let Ok(command) = rx.recv() {
        match command {
            Command::SwitchRaw { name, value } => {
                let text = do_switch(&mut switcher, &mesh, &name, value, true);
                log_line(&text);
                let target = switcher.config.hotkey_target.clone();
                push(&proxy, &switcher, &target, text, &support, &mut last_push);
            }
            Command::SwitchOnly { name, value } => {
                let text = do_switch(&mut switcher, &mesh, &name, value, false);
                log_line(&text);
                let target = switcher.config.hotkey_target.clone();
                push(&proxy, &switcher, &target, text, &support, &mut last_push);
            }
            Command::SetTarget { name, value } => {
                // 只设热键目标，不往 profile 里写没验证过的值：
                // 否则"未确认"标记会被标准值污染，下次就看不出哪个口是真的试出来的
                let _ = value;
                switcher.config.hotkey_target = name.clone();
                let _ = switcher.config.save();
                log_line(&format!("热键目标设为 {}", name));
                let text = format!("热键目标已设为 {}", switcher.config.label_for(&name));
                push(&proxy, &switcher, &name, text, &support, &mut last_push);
            }
            Command::ClearTarget => {
                switcher.config.hotkey_target.clear();
                let _ = switcher.config.save();
                push(
                    &proxy,
                    &switcher,
                    "",
                    "已取消热键目标".to_string(),
                    &support,
                    &mut last_push,
                );
            }
            Command::Rescan => {
                // 这一步必须真的重读：缓存里存着上一次的失败结论，
                // 不清掉的话这个按钮点了等于没点（用户报的就是这个）
                let report = switcher.rescan();
                log_line(&format!("重新检测：{report}"));
                if let Ok(mut note) = PORT_NOTE.lock() {
                    *note = report.clone();
                }
                let _ = sync_own(&mut switcher, &mesh);
                // ---- 重新检测的收尾：把画面切回本机口 ----
                //
                // 重新检测本身是只读的，但有些显示器被一串 DDC 读折腾之后会
                // 掉到别的输入源上 —— 实测掉到一个**没接主机**的口，画面直接
                // 黑了，而且再也不会自己回来（用户 2026-10-22 报的就是这个）。
                //
                // 判据要卡两道：①本机此刻接着显示器（`HAS_DISPLAY`）——
                // 画面在对面那台时读到的 0x60 是**对端的口**，照它"切回本机"
                // 会把对端的画面抢走；②读到的值和 own_value 不同才动手。
                if HAS_DISPLAY.load(AtomicOrdering::SeqCst) {
                    let own = switcher.config.own_value;
                    if own > 0 {
                        if let Some(display) = switcher.pick_display(None) {
                            if let Some(now) = switcher.backend().get_vcp(&display, VCP_INPUT_SOURCE) {
                                if now != own {
                                    let name = switcher.config.label_for(&switcher.config.own_input.clone());
                                    let ok = switcher.backend().set_vcp(&display, VCP_INPUT_SOURCE, own);
                                    log_line(&format!(
                                        "重新检测把画面留在了 0x{now:02X}（本机口是 0x{own:02X} {name}），{}",
                                        if ok { "已切回" } else { "切回失败，请手动切一次" }
                                    ));
                                    if ok {
                                        switcher.suppress_ddc(Duration::from_millis(1500));
                                    }
                                }
                            }
                        }
                    }
                }
                let target = switcher.config.hotkey_target.clone();
                let mut text = status_text(&mut switcher);
                if text.is_empty() {
                    text = report;
                }
                push(&proxy, &switcher, &target, text, &support, &mut last_push);
            }
            Command::PasteIntent => {
                // 用户在对端屏幕上按下了 Ctrl+V，那个键已经被抓取层吞掉。
                // 这里起线程把文件推过去 —— 绝不能阻塞本循环，心跳和切屏
                // 命令都还在后面排队。
                if CLIP_BUSY.swap(true, AtomicOrdering::SeqCst) {
                    log_line("上一批剪贴板文件还在传，这次粘贴先忽略");
                    continue;
                }
                let files = CLIP_PENDING
                    .lock()
                    .map(|g| g.clone())
                    .unwrap_or_default();
                if files.is_empty() {
                    CLIP_BUSY.store(false, AtomicOrdering::SeqCst);
                    if let Some(h) = kvm_now() {
                        h.set_paste_ready(false);
                        h.send_key_to_peer(0x09); // V
                    }
                    log_line("剪贴板里没有待传文件，这次粘贴按原样转发给对端");
                    continue;
                }
                let peers = mesh.live_peers();
                let want = switcher.config.kvm.peer_node.clone();
                let peer = peers
                    .iter()
                    .find(|p| p.node == want)
                    .cloned()
                    .or_else(|| peers.first().cloned());
                let Some(peer) = peer else {
                    CLIP_BUSY.store(false, AtomicOrdering::SeqCst);
                    if let Some(h) = kvm_now() {
                        h.set_paste_ready(false);
                        h.send_key_to_peer(0x09);
                    }
                    log_line("剪贴板传输失败：没有在线对端，这次粘贴按原样转发");
                    continue;
                };
                let ip = peer.addr.ip();
                let port = switcher.config.transfer.port;
                let node = switcher.config.node.clone();
                let key = crate::pair::link_key(&switcher.config.pair.code);
                let count = files.len();
                let peer_name = peer.node.clone();
                let proxy_t = proxy.clone();
                let proxy_p = proxy.clone();
                log_line(&format!(
                    "按需传输开始：{count} 个条目 → {peer_name}（正在整理清单并校验）"
                ));
                thread::spawn(move || {
                    let _ = proxy_t.send_event(UserEvent::Transfer("正在准备文件…".to_string()));
                    let mut last_pct = u64::MAX;
                    let res = transfer::send(
                        ip,
                        port,
                        &node,
                        &files,
                        key,
                        true, // 这批是剪贴板按需传输：收端收完要写剪贴板
                        |sent, total| {
                            // 同 start_send：checked_div 一并防住除零与溢出
                            let pct = sent
                                .checked_mul(100)
                                .and_then(|num| num.checked_div(total))
                                .unwrap_or(100);
                            if pct != last_pct {
                                last_pct = pct;
                                let _ = proxy_p.send_event(UserEvent::Transfer(format!(
                                    "剪贴板传输… {pct}%"
                                )));
                            }
                        },
                    );
                    let _ = proxy_t.send_event(UserEvent::PasteDone(res));
                });
            }
            Command::Heartbeat => {
                // 拓扑变化（显示器离开/回到本机）只靠枚举就能发现，零 I2C 开销。
                // 刚回来的头几秒链路还在重新协商，静默期把 DDC 读取全部挡掉。
                let displays_now: Vec<String> = switcher
                    .displays()
                    .into_iter()
                    .map(|d| d.name)
                    .collect();
                if displays_now != last_topology {
                    switcher.suppress_ddc(Duration::from_secs(4));
                    last_topology = displays_now;
                    // 拓扑变了有两种可能：画面切走又回来（同一台显示器），
                    // 或者真的**换了台显示器**（拔插 / 换 KVM 口）。
                    // 后者必须按新显示器自己的能力表重新建表 —— 这正是用户
                    // 心智里的"先检测显示器，再列它的输入源"。现在就做不行：
                    // 链路还在重新协商，读能力表会拿到空值并被缓存住。
                    pending_detect = Some(Instant::now());
                }
                // 静默期一过补一次自动检测：只在这台显示器还没建过档、
                // 或者建过但一个口都没读到时才真去读。已有的表（哪怕只有
                // 实测出来的几个口）一律不动，绝不因为一次拔插就重置结论。
                if let Some(t0) = pending_detect {
                    if t0.elapsed() >= Duration::from_secs(5) {
                        pending_detect = None;
                        let needs = switcher.displays().iter().any(|d| {
                            let key = switcher.identity(d);
                            switcher
                                .config
                                .profile_for(&key)
                                .map(|p| p.inputs.is_empty())
                                .unwrap_or(true)
                        });
                        if needs {
                            let report = switcher.rescan();
                            log_line(&format!("换了显示器？自动检测：{report}"));
                        }
                    }
                }
                let current = sync_own(&mut switcher, &mesh);
                // 自证收回安全网：VCP 当前值已经是本机的口，而键鼠还在往外
                // 转发 —— 画面已经回本机了（对面代切、用户按了显示器按钮、
                // 或对端消息在网络里丢了），键鼠必须立刻回来。
                // 「拓扑离开→回来」信号对从不离开本机拓扑的显示器永远不
                // 触发（实测），这条 VCP 判据是唯一可靠的兜底。
                // 注意要在 sync_own 之后比：刚才那次读值如果发现画面回本机，
                // 会顺手把 own 修正成正确值，陈旧的污染值不会误伤判断。
                // ⚠️ 只在跟随画面模式下有效：多屏共享（edge）模式显示器根本
                // 不切换输入源，VCP 永远等于自己的口 —— 不加这个守卫，转发
                // 最多 5 秒就会被误收回来（实测踩过）。
                // ⚠️ 模式要读 KVM 运行时实例的 mode()，不能读
                // switcher.config —— 菜单/面板改的是 UI 线程那份 config，
                // switcher 这份是启动时的旧副本，永远比对新（实测踩过）。
                if let Some(handle) = kvm_now() {
                    if handle.state() == kvm::State::Sending && !handle.active_is_edge() {
                        if let Some(v) = current {
                            if v > 0 && v == switcher.config.own_value {
                                handle.give_back();
                            }
                        }
                    }
                }
                let target = switcher.config.hotkey_target.clone();
                let text = status_text(&mut switcher);
                push(&proxy, &switcher, &target, text, &support, &mut last_push);
            }
            Command::AskPeerSwitch { value } => {
                // 本机没有接通的显示器，自己发不出 DDC；但对面那台现在握着画面，
                // 它的 DDC 是通的 —— 让它替我们把画面切到这个口。
                let peers = mesh.live_peers();
                let mut sent = 0usize;
                // 发不出去的原因要说出来。以前 send_command 只回 bool，"对端要码
                // 而本机没设"这种一定被丢的情况也算发出去了，界面就会谎报"几秒后
                // 屏幕会回来"——用户只能对着黑屏猜。
                let mut why: Vec<String> = Vec::new();
                for peer in &peers {
                    match mesh.send_command(
                        &peer.node,
                        "switch",
                        serde_json::json!({ "input": "", "value": value }),
                    ) {
                        Ok(()) => {
                            sent += 1;
                            log_line(&format!("已请 {} 把画面切到 0x{value:02X}", peer.node));
                        }
                        Err(err) => why.push(err),
                    }
                }
                // 口名而不是 0x 编码：这条回执是用户直接读的。画面在对面时
                // 本机 profile 是空的（压根没枚举到这块屏），所以再退一步用
                // 「上次自己插在哪个口」的记忆，最后才写编码。
                let key = profile_key_of(&switcher);
                let port = switcher
                    .config
                    .input_name_of(&key, value)
                    .or_else(|| {
                        (value == switcher.config.own_value
                            && !switcher.config.own_input.is_empty())
                        .then(|| switcher.config.own_input.clone())
                    })
                    .map(|name| switcher.config.label_for(&name))
                    .unwrap_or_else(|| format!("0x{value:02X}"));
                let text = if sent == 0 {
                    if why.is_empty() {
                        "没有在线对端可以代切".to_string()
                    } else {
                        format!("切不了：{}", why.join("；"))
                    }
                } else if value == switcher.config.own_value {
                    // 请对端切到本机插的那个口 = 把画面送回来。措辞跟着变：
                    // "切到 USB-C" 会被读成"把对面的画面切到它自己的某个口"。
                    format!("已请对端把画面切回本机（{port}），几秒后屏幕会回来")
                } else {
                    format!("已请对端把画面切到 {port}，几秒后屏幕会回来")
                };
                let target = switcher.config.hotkey_target.clone();
                push(&proxy, &switcher, &target, text, &support, &mut last_push);
            }
            Command::RemoteClipboard(text) => {
                // 写剪贴板得回主线程，这里只负责转交
                let _ = proxy.send_event(UserEvent::RemoteClipboard(text));
            }
            Command::CheckSupport => {
                let (line, detail) = inspect(&switcher);
                support = line;
                log_line(&format!("支持性检测：{}", detail));
                let target = switcher.config.hotkey_target.clone();
                push(&proxy, &switcher, &target, detail, &support, &mut last_push);
            }
            Command::ScanPorts => {
                let target = switcher.config.hotkey_target.clone();
                log_line("开始扫描输入口");

                // 物理试探有个硬前提：**切走之后得有谁把画面送回来**。
                // 本机一旦失去 DDC 就再也发不出指令，这个前提不成立时切过去就是单程票。
                let peers_now = mesh.live_peers();
                let local_paired = switcher.config.pair.is_set();
                let blocked: Vec<String> = peers_now
                    .iter()
                    .filter(|p| p.wants_pair && !local_paired)
                    .map(|p| p.node.clone())
                    .collect();
                let can_relay = !peers_now.is_empty() && blocked.is_empty();
                let skip_reason: Option<String> = if peers_now.is_empty() {
                    Some("局域网里没有别的机器在运行本程序，切过去就切不回来".to_string())
                } else if !blocked.is_empty() {
                    Some(format!(
                        "{} 设了配对码、本机没设，发给它的切换请求会被拒绝 —— \
                         两边填同一个码再来",
                        blocked.join("、")
                    ))
                } else {
                    None
                };

                push(
                    &proxy,
                    &switcher,
                    &target,
                    if can_relay {
                        "正在扫描，屏幕会黑几次…".to_string()
                    } else {
                        "正在汇总已知信息（不做切屏试探）…".to_string()
                    },
                    &support,
                    &mut last_push,
                );

                // 对面自己报过的口直接采信，这些不用切屏试探
                let peer_ports: Vec<(String, u16)> = peers_now
                    .iter()
                    .filter(|p| p.knows_port())
                    .map(|p| (p.node.clone(), p.own_value))
                    .collect();
                if !peer_ports.is_empty() {
                    log_line(&format!("局域网里有 {} 台对端主动报了口", peer_ports.len()));
                }
                if let Some(reason) = &skip_reason {
                    log_line(&format!("跳过切屏试探：{reason}"));
                }

                let relay_mesh = mesh.clone();
                // 空切片 = 一个口都不物理试探，只汇总本机与对端已经报过的信息
                let no_probe: [u16; 0] = [];
                let only = if can_relay {
                    None
                } else {
                    Some(no_probe.as_slice())
                };
                let result = switcher.scan_presence(
                    &peer_ports,
                    Duration::from_secs(10),
                    true,
                    only,
                    |home| {
                        // 对面要是会丢弃未签名包，发也白发；直接说没送出去
                        if !can_relay {
                            return false;
                        }
                        let nodes: Vec<String> = relay_mesh
                            .live_peers()
                            .into_iter()
                            .map(|p| p.node)
                            .collect();
                        let mut sent = false;
                        for node in nodes {
                            if relay_mesh
                                .send_command(
                                    &node,
                                    "switch",
                                    serde_json::json!({ "input": "", "value": home }),
                                )
                                .is_ok()
                            {
                                sent = true;
                            }
                        }
                        sent
                    },
                    log_line,
                );

                let text = match result {
                    Ok(report) => {
                        let occupied: Vec<String> = report
                            .ports
                            .iter()
                            .filter(|p| p.presence == Presence::Occupied)
                            .map(|p| p.name.clone())
                            .collect();
                        let idle = report
                            .ports
                            .iter()
                            .filter(|p| p.presence == Presence::Idle)
                            .count();
                        let mut text = if occupied.is_empty() {
                            "扫描完成：没找到有主机的口".to_string()
                        } else {
                            format!("扫描完成：{} 有主机", occupied.join("、"))
                        };
                        if idle > 0 {
                            text.push_str(&format!("，{} 个口无信号", idle));
                        }
                        if !report.back_home {
                            text.push_str(
                                "。画面没能自动切回本机 —— 要么在对面那台点一下切换，\
                                 要么用显示器 OSD 手动切回来",
                            );
                        }
                        if let Some(reason) = &skip_reason {
                            text.push_str(&format!("。这次没有切屏试探：{reason}"));
                        }
                        if let Some(reason) = &report.aborted {
                            log_line(reason);
                        }
                        text
                    }
                    Err(err) => format!("扫描失败：{}", err),
                };
                // 扫描刚结束时 DDC 常有一段静默期，这时重跑体检会误判成"通道不通"，
                // 所以只把自己的口同步一下，体检结论沿用原来的。
                let _ = sync_own(&mut switcher, &mesh);
                log_line(&text);
                push(&proxy, &switcher, &target, text, &support, &mut last_push);
            }
        }
    }
}
