//! 主线程的状态机。
//!
//! `run()` 里那个 `event_loop.run` 闭包曾经有 1259 行：托盘图标、菜单签名、
//! 配置、面板窗口、共用键鼠句柄、剪贴板监听、热键三件套……所有可变状态都是
//! 闭包的捕获变量，每个 `Event` 分支直接改它们。改一次热键要在三个分支里找
//! 同一个 `hotkey_id`，加一个面板动作要在 500 行的 match 里插针。
//!
//! 现在闭包里只剩分发，状态收进 [`Ui`]，每个 `Event` 变成 `Ui` 上的一个
//! 方法（`on_init` / `on_status` / `on_panel` / `on_main_events_cleared` / …）。
//! 搬过来的代码逐行平移，行为没有任何改动。

use std::path::PathBuf;
use std::sync::atomic::Ordering as AtomicOrdering;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use global_hotkey::hotkey::HotKey;
use global_hotkey::GlobalHotKeyManager;
use tao::event::{ElementState, WindowEvent};
use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};
use tao::keyboard::{KeyCode, ModifiersState};
use tao::window::{Window, WindowId};
use tray_icon::{TrayIcon, TrayIconBuilder};

use crate::clipboard::{self, ClipItem, Watcher};
use crate::config::Config;
use crate::kvm;
use crate::panel::Snapshot;
use crate::panelwin::Panel;
use crate::peer::{Mesh, Peer};

use super::hotkey::{apply_hotkey, compose_spec, key_token, pretty_hotkey};
use super::kvmctl::{kvm_boot, perms_ok, pick_peer, resume_backoff};
use super::log_line;
use super::menu::{build_menu, menu_signature, own_value, refresh_menu};
use super::snapshot::update_snapshot;
use super::sys::load_icon;
use super::{CLIP_BUSY, CLIP_PENDING, Command, UserEvent, PANEL_MAXIMIZED, PANEL_PORT};

/// 主线程的全部可变状态。
///
/// 以前这些都是 `event_loop.run` 闭包的捕获变量 —— 想知道"改这个字段会影响
/// 哪些分支"，只能在几百行里人工搜。收成结构体之后每个方法签名就是答案，
/// 新增状态也只需要加一个字段。
pub(super) struct Ui {
    // ---- 热键三件套 ----
    //
    // manager 必须活到进程结束；用户在菜单/面板里改热键时要能注销旧的、
    // 注册新的，所以三个状态都得可变（见 apply_hotkey）。
    pub(super) hotkey_manager: Option<GlobalHotKeyManager>,
    pub(super) current_hotkey: Option<HotKey>,
    pub(super) hotkey_id: Option<u32>,
    /// 录制新热键用的小窗口：平时隐藏，点「按下新的组合键」时才弹出来抢焦点
    pub(super) record_window: Option<Window>,

    /// 发给工作线程的指令通道（切屏、重扫、心跳…）
    pub(super) tx: mpsc::Sender<Command>,
    /// 局域网：既是"对面那台替我做不了的事"，也是"哪台机器在哪个口"的信息来源
    pub(super) mesh: Mesh,
    /// 把面板动作送回主线程的代理（开对话框、改配置这些只能主线程做）
    pub(super) proxy: EventLoopProxy<UserEvent>,

    /// 控制面板的数据源，面板线程每 400ms 读一次
    pub(super) snapshot: Arc<Mutex<Snapshot>>,
    /// 文件接收目录（共享可变：面板上改目录要能即时生效）
    pub(super) inbox_shared: Arc<Mutex<PathBuf>>,
    /// 文件接收服务的配对钥匙。放 `Arc<Mutex>` 是因为文件服务没法像 KVM 那样
    /// "重启一下"—— 旧 TcpListener 还占着端口，重新 bind 必然 EADDRINUSE，
    /// 所以只能让 key 变成共享可变（面板改配对码时就地换掉）。
    pub(super) transfer_key: Arc<Mutex<Option<[u8; 32]>>>,

    /// 托盘图标。第一次 NewEvents(Init) 才建 —— 那之前 event loop 还没就绪
    pub(super) tray: Option<TrayIcon>,
    /// 状态行文案（菜单第一行 + 面板顶部）
    pub(super) status: String,
    /// 热键目标输入口的名
    pub(super) target: String,
    pub(super) config: Config,
    /// 当前显示器在配置里的身份键，菜单按它取输入口列表
    pub(super) profile_key: String,
    /// 支持性体检结论，显示在状态行下面
    pub(super) support: String,
    /// 上一次画出来的菜单长什么样。内容没变就不重建 —— 见 menu_signature 的注释
    pub(super) menu_sig: String,
    /// 上一次发心跳的时间
    pub(super) last_rescan: Instant,
    /// 是否正在录制新热键（录制窗口可见期间为 true）
    pub(super) recording: bool,
    pub(super) record_mods: ModifiersState,

    /// 剪贴板监听。先取一次基线，否则启动瞬间会把用户手上已有的内容
    /// 当成"新复制"广播出去
    pub(super) watcher: Watcher,
    /// 传输进度 / 结果提示，面板上显示
    pub(super) transfer_note: String,

    /// 控制面板窗口：第一次点菜单才建，之后关闭只是隐藏，再点秒开
    pub(super) panel_win: Option<Panel>,
    /// 共用键鼠实例。开着的时候它会一直占着 CGEventTap / 低级钩子
    pub(super) kvm_handle: Option<Arc<kvm::Kvm>>,

    // 自动恢复的重试状态（v0.6.66）。
    //
    // 以前这里是 `kvm_restored: bool`，一次失败就永久放弃 —— 对没有键鼠的
    // 副机是死局：它开机比主机早、网络还没起、第一次授权没生效，任何一种
    // 都会让它永远停在空闲，而人没法去那台机器上点菜单。
    pub(super) resume_failures: u32,
    pub(super) resume_next: Option<Instant>,
    /// 用户点了面板窗口的关闭按钮：等这轮事件处理完再真正释放
    pub(super) panel_stale: bool,
}

impl Ui {
    /// 内容真的变了才重画菜单（签名一样就不碰，否则 macOS 会把用户正打开
    /// 着的菜单关掉）。三处轮询末尾都要走这一遍，收成一个方法。
    pub(super) fn refresh(&mut self) {
        let peers = self.mesh.live_peers();
        refresh_menu(
            &mut self.tray,
            &mut self.menu_sig,
            &self.status,
            &self.config,
            &peers,
            own_value(&self.mesh),
            &self.profile_key,
            &self.support,
        );
    }

    /// 事件循环就绪：建托盘、画第一版菜单。
    pub(super) fn on_init(&mut self, elwt: &EventLoopWindowTarget<UserEvent>) {
        let peers = self.mesh.live_peers();
        let menu = build_menu(
            &self.status,
            &self.config,
            &peers,
            own_value(&self.mesh),
            &self.profile_key,
            &self.support,
        );
        let mut builder = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("InputSwitch · 显示器输入源切换");
        if let Some(icon) = load_icon() {
            builder = builder.with_icon(icon);
        } else {
            log_line("警告：菜单栏图标解码失败");
        }
        #[cfg(target_os = "macos")]
        {
            builder = builder.with_icon_as_template(true);
        }
        match builder.build() {
            Ok(created) => {
                self.tray = Some(created);
                self.menu_sig = menu_signature(
                    &self.status,
                    &self.config,
                    &peers,
                    own_value(&self.mesh),
                    &self.profile_key,
                    &self.support,
                );
                log_line("托盘图标已创建");
            }
            Err(err) => log_line(&format!("托盘创建失败：{}", err)),
        }

        // 自测 / 需要直接进面板时用：INPUTSWITCH_OPEN_PANEL=1 启动即弹面板
        if std::env::var("INPUTSWITCH_OPEN_PANEL").is_ok() {
            let port = PANEL_PORT.load(AtomicOrdering::SeqCst);
            if port != 0 {
                match Panel::open(elwt, port, "InputSwitch 控制面板") {
                    Ok(created) => {
                        created.show();
                        self.panel_win = Some(created);
                        log_line("控制面板窗口已自动打开");
                    }
                    Err(err) => log_line(&format!("控制面板窗口创建失败：{err}")),
                }
            }
        }
    }

    /// 工作线程推来的状态：刷新菜单，顺手做两件"时机刚好"的事 ——
    /// 链路表自动回填、共用键鼠自动恢复。
    pub(super) fn on_status(
        &mut self,
        text: String,
        target: String,
        profile_key: String,
        support: String,
    ) {
        self.status = text;
        self.target = target;
        self.config = Config::load();
        if !profile_key.is_empty() {
            self.profile_key = profile_key;
        }
        if !support.is_empty() {
            self.support = support;
        }
        let peers = self.mesh.live_peers();

        // ---- 链路表自动回填（v0.6.67）----
        //
        // 以前必须**两边都配**：A 的表里要有 B、B 的表里也要有 A ——
        // 因为收包白名单只认链路表里的 IP。只配一边的症状是"推过去
        // 键鼠消失、画面不切、日志安静"，极难自查。
        //
        // 现在：对端**已配对**（announce 签名验过）并且它自己的共用
        // 键鼠是开着的，就自动补一条"未摆位"链路，地址取自它的广播，
        // 本机不编造。用户只需要在**一边**把它拖到某个方向。
        let mut filled: Vec<String> = Vec::new();
        for p in &peers {
            if !p.paired || !p.kvm_on {
                continue;
            }
            if self.config.kvm.links.iter().any(|l| l.node == p.node) {
                continue;
            }
            self.config.kvm.links.push(crate::config::LinkConfig {
                node: p.node.clone(),
                side: String::new(),
                mode: String::new(),
                input: String::new(),
                addr: p.addr.ip().to_string(),
            });
            filled.push(p.node.clone());
        }
        if !filled.is_empty() {
            let _ = self.config.save();
            log_line(&format!(
                "已自动补上链路：{}（在摆位图里拖到某个方向就能用；\
                 不用再让对方也配一遍）",
                filled.join("、")
            ));
        }

        // ---- 自动恢复（v0.6.66 改成可重试）----
        //
        // 语义只有一条：**自愈不改用户意图**。`config.kvm.enabled`
        // 是"用户想不想开"，这里只负责"想开但没开着"时补上。
        // 所以失败**绝不**写 enabled=false（v0.6.65 之前那条
        // `config.kvm.enabled = false` 会把副机永久钉死在关闭状态）。
        //
        // 三个条件：用户要开 + 开关允许自动恢复 + 现在确实没开。
        // 再加上"对端已上线"——开机瞬间网络/显示器都还没就绪，
        // 那时候硬起只会失败，白白消耗重试次数。
        let resume_due = self.resume_next.map(|t| Instant::now() >= t).unwrap_or(true);
        if self.config.kvm.enabled
            && self.config.kvm.auto_resume
            && self.kvm_handle.is_none()
            && resume_due
            // 缺授权时重试一百次也不会成功（TCC 结论缓存在进程里，
            // 必须重启进程才认），别白刷日志、别把退避拉满。
            && perms_ok()
            && pick_peer(&peers, &self.config.kvm.peer_node).is_some()
        {
            let log: Arc<dyn Fn(String) + Send + Sync> = Arc::new(|text| log_line(&text));
            match kvm_boot(&mut self.config, &peers, &self.tx, log) {
                Ok((handle, text)) => {
                    self.kvm_handle = Some(handle.clone());
                    if let Ok(mut slot) = super::KVM.lock() {
                        *slot = Some(handle);
                    }
                    self.resume_failures = 0;
                    self.resume_next = None;
                    log_line(&format!("{text}（自动恢复上次的设置）"));
                }
                Err(err) => {
                    self.resume_failures = self.resume_failures.saturating_add(1);
                    let delay = resume_backoff(self.resume_failures);
                    self.resume_next = Some(Instant::now() + Duration::from_secs(delay));
                    // 不弹设置页：开机自启时弹窗会吓人。
                    // 但要写清"第几次、多久后重试"，否则用户以为它放弃了。
                    log_line(&format!(
                        "共用键鼠没能自动恢复（第 {} 次）：{err}；{delay} 秒后重试",
                        self.resume_failures
                    ));
                }
            }
        }

        self.refresh();
    }

    /// 对端通过局域网发来的剪贴板文本。
    ///
    /// NSPasteboard 只能在主线程碰，所以绕一圈回到这里。
    pub(super) fn on_remote_clipboard(&mut self, text: String) {
        if self.config.clipboard.enabled && clipboard::write_text(&text) {
            self.watcher.mark_seen(&text);
        }
    }

    /// 传输进度 / 结果提示。
    pub(super) fn on_transfer(&mut self, note: String) {
        self.transfer_note = note.clone();
        log_line(&note);
    }

    /// 文件收完了。`clip` = 这批是剪贴板按需传输（要写进剪贴板），
    /// 不是面板手动发文件。
    pub(super) fn on_saved(&mut self, paths: Vec<PathBuf>, clip: bool) {
        if clip {
            // 剪贴板按需传输的落点：把收到的文件写进本机剪贴板，
            // 用户按下粘贴键时就直接是本地文件了（那一刻是纯本地操作）。
            if clipboard::write_files(&paths) {
                self.watcher.mark_written();
                self.status = format!("已接收 {} 个文件并放入剪贴板", paths.len());
            } else {
                self.status = format!("已接收 {} 个文件，但写入剪贴板失败", paths.len());
            }
        } else if let Some(first) = paths.first() {
            self.status = format!("接收了 {} 个文件 → {}", paths.len(), first.display());
        }
        self.transfer_note.clear();
        log_line(&self.status);
    }

    /// 剪贴板按需传输结束（成功/失败），据此补按粘贴键。
    pub(super) fn on_paste_done(&mut self, result: Result<String, String>) {
        CLIP_BUSY.store(false, AtomicOrdering::SeqCst);
        let pending = CLIP_PENDING.lock().map(|g| g.len()).unwrap_or(0);
        if let Ok(mut slot) = CLIP_PENDING.lock() {
            slot.clear();
        }
        match result {
            Ok(msg) => {
                if let Some(h) = super::kvm_now() {
                    h.set_paste_ready(false);
                    if self.config.clipboard.auto_paste {
                        // 用户按的那一下被我们吞掉了，这里替他把粘贴动作补完。
                        // 注入事件带 INJECT_TAG，不会被当成"又一次要传"。
                        h.replay_paste_to_peer();
                    }
                }
                self.status = if self.config.clipboard.auto_paste {
                    format!("已把 {pending} 个文件送到对端并替你按下粘贴（{msg}）")
                } else {
                    format!("已把 {pending} 个文件送到对端：再按一次 Ctrl+V 即可")
                };
            }
            Err(err) => {
                if let Some(h) = super::kvm_now() {
                    h.set_paste_ready(false);
                    // 把用户原本那一下补回去：至少粘出剪贴板里原来的内容，
                    // 不能让他觉得"粘贴键坏了"。
                    h.send_key_to_peer(0x09); // V
                }
                // 用户在面板上点了取消：不是故障，说清"按原样粘了"
                self.status = if err == crate::transfer::CANCELLED {
                    "已取消剪贴板传输，这次粘贴按原样转发给对端".to_string()
                } else {
                    format!("剪贴板文件传输失败：{err}")
                };
            }
        }
        self.transfer_note.clear();
        log_line(&self.status);
    }

    /// 400ms tick：面板快照 + 剪贴板轮询 + 重画菜单。
    pub(super) fn refresh_panel(&mut self) {
        let peers = self.mesh.live_peers();
        update_snapshot(
            &self.snapshot,
            &self.status,
            &self.support,
            &self.target,
            &self.config,
            &self.profile_key,
            &peers,
            own_value(&self.mesh),
            &self.mesh,
            &self.transfer_note,
        );
        if self.config.clipboard.enabled {
            match self.watcher.poll() {
                Some(ClipItem::Text(text)) => {
                    // 复制的是文本：走老路，即时发。
                    // 同时清掉"待传文件"状态 —— 用户改主意了，
                    // 再按 Ctrl+V 不该还去传上一条文件。
                    CLIP_PENDING.lock().map(|mut g| g.clear()).ok();
                    if let Some(h) = super::kvm_now() {
                        h.set_paste_ready(false);
                    }
                    // **发给谁**：有会话就只发"现在握着键鼠"的那台，
                    // 没有会话才广播。P7 之前是无条件广播 —— 一机四台时
                    // 复制一句话会同时改掉四台机器的剪贴板，既无意义也
                    // 会踩掉人家正在用的内容。
                    let active_node: Option<String> = super::kvm_now()
                        .and_then(|h| h.active_node().map(str::to_string));
                    let mut targets: Vec<&Peer> = match active_node.as_deref() {
                        Some(node) => peers.iter().filter(|p| p.node == node).collect(),
                        None => peers.iter().collect(),
                    };
                    // 会话那台刚好掉线（active 还指着它，但它已不在
                    // live_peers 里）：退回广播，别因为一台掉线整条同步哑掉
                    if targets.is_empty() {
                        targets = peers.iter().collect();
                    }
                    let mut sent = 0;
                    for peer in targets {
                        // 这里只关心"有没有发出去"：一次复制可能广播给
                        // 好几台，逐条报错会刷屏（真正该解释的是代切那条路）
                        if self
                            .mesh
                            .send_command(
                                &peer.node,
                                "clipboard",
                                serde_json::json!({ "text": text }),
                            )
                            .is_ok()
                        {
                            sent += 1;
                        }
                    }
                    if sent > 0 {
                        log_line(&format!("剪贴板已同步（{} 字）", text.chars().count()));
                    }
                }
                // 开关关着时整个分支不做（不是错误：用户只是没开
                // 文件传输）。写成 match guard 会让这一支失去兜底、
                // 还得补 `_ => {}` 才能过穷尽性检查，反而更绕。
                #[allow(clippy::collapsible_match)]
                Some(ClipItem::Files(paths)) => {
                    // 复制的是文件：**一个字节都不发**。
                    // 只在本机记住清单，把所有动作推迟到用户真的按下粘贴。
                    if self.config.clipboard.files {
                        if !peers.iter().any(|p| p.clip_v >= 1) {
                            log_line("剪贴板里是文件，但对端版本不支持按需传输，已忽略");
                        } else {
                            let n = paths.len();
                            if let Ok(mut slot) = CLIP_PENDING.lock() {
                                *slot = paths;
                            }
                            if let Some(h) = super::kvm_now() {
                                h.set_paste_ready(true);
                            }
                            log_line(&format!(
                                "剪贴板里有 {n} 个文件：推到对端按 Ctrl+V 时才传"
                            ));
                        }
                    }
                }
                None => {}
            }
        }

        // 每轮同步一次"能不能拦粘贴键"：用户完全可能先复制文件、
        // 后开共用键鼠（那时 poll 早就跑完了），只在 poll 里置位会漏。
        // 一个原子写，400ms 一次，开销可忽略。
        let pending_n = CLIP_PENDING.lock().map(|g| g.len()).unwrap_or(0);
        if let Some(h) = super::kvm_now() {
            h.set_paste_ready(pending_n > 0 && self.config.clipboard.files);
        }

        self.refresh();
    }

    /// 录制窗口的键盘事件；面板窗口点关闭只是收起来，不销毁。
    pub(super) fn on_window_event(&mut self, event: WindowEvent, window_id: WindowId) {
        match event {
            // 面板窗口关掉：tao 没有 prevent_close，窗口会被系统销毁，
            // 这里只做标记，等这一轮事件处理完再释放，避免在窗口回调里自毁。
            WindowEvent::CloseRequested => {
                if self
                    .panel_win
                    .as_ref()
                    .is_some_and(|win| win.owns(window_id))
                {
                    self.panel_stale = true;
                } else {
                    self.recording = false;
                    if let Some(win) = self.record_window.as_ref() {
                        win.set_visible(false);
                    }
                }
            }
            // WebView 是子视图，窗口变大变小要手动跟
            WindowEvent::Resized(_) => {
                if let Some(win) = self.panel_win.as_ref() {
                    if win.owns(window_id) {
                        win.fit();
                        // 最大化态会被 Win+↑、双击拖拽区、系统窗口菜单改掉，
                        // 不是只有我们那个钮能改。每次尺寸变化重新读一次，
                        // 快照带给前端翻「□ / ❐」。
                        PANEL_MAXIMIZED.store(win.is_maximized(), AtomicOrdering::SeqCst);
                    }
                }
            }
            WindowEvent::ModifiersChanged(mods) => self.record_mods = mods,
            WindowEvent::KeyboardInput { event, .. } => {
                let from_record = self
                    .record_window
                    .as_ref()
                    .is_some_and(|win| win.id() == window_id);
                if !self.recording || !from_record || event.state != ElementState::Pressed {
                    return;
                }
                if event.physical_key == KeyCode::Escape {
                    self.recording = false;
                    if let Some(win) = self.record_window.as_ref() {
                        win.set_visible(false);
                    }
                    self.status = "已取消录制".to_string();
                    return;
                }
                let Some(token) = key_token(event.physical_key) else {
                    return;
                };
                let Some(spec) = compose_spec(self.record_mods, token) else {
                    // 只按住修饰键或只按普通键：继续等，不要吞掉
                    self.status = "再带一个 Ctrl / Alt / Shift / Cmd 一起按".to_string();
                    return;
                };
                match apply_hotkey(
                    &mut self.hotkey_manager,
                    &mut self.current_hotkey,
                    &mut self.hotkey_id,
                    &spec,
                ) {
                    Ok(()) => {
                        self.config.hotkey = spec.clone();
                        let _ = self.config.save();
                        self.status = format!("热键已改为 {}", pretty_hotkey(&spec));
                        log_line(&self.status);
                    }
                    Err(err) => {
                        self.status = err;
                        log_line(&self.status);
                    }
                }
                self.recording = false;
                if let Some(win) = self.record_window.as_ref() {
                    win.set_visible(false);
                }
            }
            _ => {}
        }
    }
}
