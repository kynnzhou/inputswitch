//! 主线程的轮询：托盘菜单、全局热键、400ms tick。
//!
//! tao 没有"菜单项被点了"的事件类型，只能用 `MenuEvent::receiver()` 主动
//! 收 —— 所以这两段都挂在 `MainEventsCleared` 上，每个 tick 跑一遍。
//! 好处是不额外开线程，代价是"点一下到响应"最多差 400ms（实测无感）。

use std::sync::atomic::Ordering as AtomicOrdering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use global_hotkey::GlobalHotKeyEvent;
use tao::event_loop::EventLoopWindowTarget;
use tray_icon::menu::MenuEvent;

use crate::panel;
use crate::panelwin::Panel;

use super::kvmctl::{explain_silent_peer, kvm_boot, pick_peer, restart_kvm};
use super::log_line;
use super::menu::{configured_value, side_label};
use super::send::start_send;
use super::{
    kvm_now, parse_port, Command, Ui, UserEvent, ID_PANEL, ID_PEER, ID_PEER_FILE, ID_PEER_HAND,
    ID_PEER_SIDE, ID_SWITCH, MENU_ASK_BACK, MENU_KVM, MENU_KVM_HOP, MENU_KVM_MODE, MENU_KVM_SIDE,
    MENU_QUIT, MENU_RESTART, PANEL_MAXIMIZED,
};

impl Ui {
    /// 400ms tick。四件事按"从轻到重"排：先处理用户已经关掉的面板窗口，
    /// 再刷面板快照和剪贴板，然后收菜单和热键，最后才考虑发心跳。
    pub(super) fn on_main_events_cleared(&mut self, elwt: &EventLoopWindowTarget<UserEvent>) {
        // 面板窗口已经被用户关掉，这里才真正释放（WebView 会先于窗口 drop）
        if self.panel_stale {
            self.panel_win = None;
            self.panel_stale = false;
            // 窗口没了，最大化态跟着归零 —— 否则下次开面板会先画成「❐」
            PANEL_MAXIMIZED.store(false, AtomicOrdering::SeqCst);
            log_line("控制面板窗口已关闭");
        }
        // 面板状态 + 剪贴板轮询：都挂在 400ms 的 tick 上，不额外开线程
        self.refresh_panel();

        self.on_menu_events(elwt);
        self.on_hotkey_events();

        // 每 5 秒一次轻量心跳：只同步广播和状态。全量重扫（EDID+能力表，
        // 十几笔 I2C）不能挂在这——显示器刚切回来时撞上这波读取，
        // WindowServer 会卡进看门狗超时（实测整机关机）。重活只留给
        // 用户主动点的「重新检测显示器」。
        if self.last_rescan.elapsed() > Duration::from_secs(5) {
            let _ = self.tx.send(Command::Heartbeat);
            self.last_rescan = Instant::now();
        }
    }

    /// 托盘菜单。id 全是字符串，靠前缀分发 —— `muda` 没有给菜单项挂
    /// 负载的稳定 API，而 `MenuEvent` 只带一个 id。
    fn on_menu_events(&mut self, elwt: &EventLoopWindowTarget<UserEvent>) {
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            let id = event.id.0.as_str();
            if id == MENU_QUIT {
                if let Some(handle) = self.kvm_handle.take() {
                    handle.stop();
                }
                std::process::exit(0);
            } else if id == MENU_RESTART {
                log_line("菜单里点了「重启应用」");
                super::sys::restart_app();
            } else if id == MENU_ASK_BACK {
                // 画面在对面：本机没有 DDC 通道，只能请握着画面的那台代发。
                // 用的还是本机记住的 own_value —— 那是**这块屏**的口编号，
                // 对端切的是同一块屏，所以这个编号在它那边同样成立
                // （跟"拿对端自己那块屏的编号切本屏"不是一回事）。
                self.status = "已请对面把画面切回本机…".to_string();
                let _ = self
                    .tx
                    .send(Command::AskPeerSwitch { value: self.config.own_value });
            } else if id == ID_PANEL {
                let port = super::PANEL_PORT.load(AtomicOrdering::SeqCst);
                // 已经有窗口就只是叫到前面来；没有才建，建不出来退回浏览器
                if self.panel_win.is_none() && port != 0 {
                    match Panel::open(elwt, port, "InputSwitch 控制面板") {
                        Ok(created) => {
                            log_line("控制面板窗口已创建");
                            self.panel_win = Some(created);
                        }
                        Err(err) => log_line(&format!("控制面板窗口创建失败：{err}")),
                    }
                }
                match self.panel_win.as_ref() {
                    Some(win) => win.show(),
                    None => {
                        if !panel::open_in_browser(port) {
                            self.status = "打不开控制面板，请手动访问 127.0.0.1 上的面板"
                                .to_string();
                        }
                    }
                }
            } else if id == MENU_KVM {
                if let Some(handle) = self.kvm_handle.take() {
                    handle.stop();
                    if let Ok(mut slot) = super::KVM.lock() {
                        *slot = None;
                    }
                    self.config.kvm.enabled = false;
                    let _ = self.config.save();
                    self.status = "共用键鼠已关闭".to_string();
                    log_line(&self.status);
                } else {
                    let peers = self.mesh.live_peers();
                    match pick_peer(&peers, &self.config.kvm.peer_node) {
                        None => {
                            self.status = "局域网里没找到对端，先让另一台开着本程序".to_string();
                            log_line(&self.status);
                        }
                        Some(_) => {
                            let log: Arc<dyn Fn(String) + Send + Sync> =
                                Arc::new(|text| log_line(&text));
                            match kvm_boot(&mut self.config, &peers, &self.tx, log) {
                                Ok((handle, text)) => {
                                    self.kvm_handle = Some(handle.clone());
                                    if let Ok(mut slot) = super::KVM.lock() {
                                        *slot = Some(handle);
                                    }
                                    self.config.kvm.enabled = true;
                                    let _ = self.config.save();
                                    self.status = text;
                                }
                                Err(err) => {
                                    // 十有八九是没授权：直接把设置页推给用户
                                    crate::kvm::open_settings();
                                    self.status = format!(
                                        "共用键鼠开不了：{err}（已经打开设置页，勾上本程序再点一次）"
                                    );
                                    self.config.kvm.enabled = false;
                                    let _ = self.config.save();
                                }
                            }
                            log_line(&self.status);
                        }
                    }
                }
            } else if id == MENU_KVM_HOP {
                match kvm_now() {
                    Some(handle) => {
                        self.status = match handle.retarget_next_edge() {
                            Ok(()) => format!(
                                "键鼠已转到 {}（画面没动）",
                                handle.active_node().unwrap_or("对端")
                            ),
                            Err(err) => format!("转不过去：{err}"),
                        };
                        log_line(&self.status);
                    }
                    None => self.status = "共用键鼠没开".to_string(),
                }
            } else if id == MENU_KVM_MODE {
                // 两种模式的物理前提不同，切换必须重启实例：
                // 边缘跨界要在"没抓取时"盯屏幕边缘，跟随画面则完全不动。
                let next = if self.config.kvm.mode == "edge" {
                    "follow".to_string()
                } else {
                    "edge".to_string()
                };
                self.config.kvm.set_mode_all(&next);
                let _ = self.config.save();
                let was_on = self.kvm_handle.take();
                if let Some(handle) = was_on.as_ref() {
                    handle.stop();
                }
                if let Ok(mut slot) = super::KVM.lock() {
                    *slot = None;
                }
                self.status = if next == "edge" {
                    "已切到多屏共享：两台各有屏幕和键鼠，光标推到边缘就过去".to_string()
                } else {
                    "已切到跟随画面：一套键鼠，画面切到哪台就跟到哪台".to_string()
                };
                if was_on.is_some() {
                    self.status.push_str("。共用键鼠已关闭，请重新打开");
                    self.config.kvm.enabled = false;
                    let _ = self.config.save();
                }
                log_line(&self.status);
            } else if let Some(side_value) = id.strip_prefix(MENU_KVM_SIDE) {
                // 改越界方向。抓取钩子里记死了旧方向，KVM 开着就必须
                // 重启实例；直接自动重开，别让用户再手动点一遍。
                if self.config.kvm.side != side_value {
                    // 写透到主链路：v3 起掩码由链路表生成
                    self.config.kvm.set_primary_side(side_value);
                    let _ = self.config.save();
                    let text = format!("对端方向已改为{}侧", side_label(side_value));
                    restart_kvm(
                        &mut self.config,
                        &mut self.kvm_handle,
                        &self.mesh,
                        &self.tx,
                        &mut self.status,
                        text,
                    );
                }
            } else if let Some(rest) = id.strip_prefix(ID_PEER_SIDE) {
                // `peer-side:<node>:<side|none>`。
                // 空方向走 `none`：`peer-side:<node>:` 尾部空串在 rsplit 里
                // 跟"根本没带值"分不开，两种错法表现不同、很难查。
                if let Some((node, raw)) = rest.rsplit_once(':') {
                    let side = if raw == "none" { "" } else { raw };
                    let old = self
                        .config
                        .kvm
                        .links
                        .iter()
                        .find(|l| l.node == node)
                        .map(|l| l.side.clone());
                    match old {
                        None => {
                            self.status = format!(
                                "链路表里没有 {node}（可能要重启一次程序让它重新扫描）"
                            );
                            log_line(&self.status);
                        }
                        Some(old) if old == side => {
                            self.status = format!("{node} 的方向没变");
                            log_line(&self.status);
                        }
                        Some(_) => {
                            // 方向决定模式，两个字段一起改：
                            //   进格子 = 多屏共享（edge），进「共用一块屏」= 跟随（follow）。
                            // 只改 side 不改 mode 会造出「有门却跟随」——摆位图上
                            // 画在格子里、实际根本不占边，推过去毫无反应。面板的
                            // /api/link 那条入口一直有这层纠正，托盘这条漏了。
                            if let Some(link) =
                                self.config.kvm.links.iter_mut().find(|l| l.node == node)
                            {
                                link.side = side.to_string();
                                link.mode = if side.is_empty() {
                                    "follow".to_string()
                                } else {
                                    "edge".to_string()
                                };
                            }
                            // 镜像要跟着回写：老版本只读镜像里的 side，
                            // 不回写的话两台程序看到的拓扑不一样
                            self.config.kvm.mirror_from_primary();
                            let _ = self.config.save();

                            let where_is = if side.is_empty() {
                                "共用一块屏".to_string()
                            } else {
                                format!("本机{}侧", side_label(side))
                            };
                            let mut text = format!("{node} 已设在{where_is}");
                            // 同一侧摆了两台：推过去落到谁那儿是不确定的。
                            // 不拦（物理摆位是用户的事），但必须说出来
                            if !side.is_empty() {
                                let clash: Vec<&str> = self
                                    .config
                                    .kvm
                                    .links
                                    .iter()
                                    .filter(|l| l.node != node && l.side == side)
                                    .map(|l| l.node.as_str())
                                    .collect();
                                if !clash.is_empty() {
                                    text.push_str(&format!(
                                        "。注意{}侧已经有 {}，推过去可能落到它那儿",
                                        side_label(side),
                                        clash.join("、")
                                    ));
                                }
                            }
                            restart_kvm(
                                &mut self.config,
                                &mut self.kvm_handle,
                                &self.mesh,
                                &self.tx,
                                &mut self.status,
                                text,
                            );
                        }
                    }
                }
            } else if let Some(rest) = id.strip_prefix(ID_PEER_HAND) {
                // `peer-hand:<node>`
                match kvm_now() {
                    Some(handle) => {
                        self.status = match handle.hand_over_to(rest) {
                            Ok(()) => format!("键鼠已交给 {rest}"),
                            Err(err) => format!(
                                "交不出去：{}",
                                explain_silent_peer(&self.mesh, Some(rest), &err)
                            ),
                        };
                    }
                    None => self.status = "共用键鼠没开，面板上先打开它".to_string(),
                }
                log_line(&self.status);
            } else if let Some(node) = id.strip_prefix(ID_PEER_FILE) {
                // `peer-file:<node>` —— 点名发给这一台。菜单里点了谁就发给谁，
                // 不再走"握着键鼠的那台"这种猜测
                let node = node.to_string();
                start_send(
                    &self.config,
                    &self.mesh,
                    &self.proxy,
                    &mut self.status,
                    Some(&node),
                );
            } else if let Some(rest) = id.strip_prefix(ID_SWITCH) {
                if let Some((name, value)) = parse_port(rest) {
                    self.status = format!("正在切换到 {}…", name);
                    let _ = self.tx.send(Command::SwitchRaw { name, value });
                }
            } else if let Some(rest) = id.strip_prefix(ID_PEER) {
                // 直接切到对端广播的那个口，不需要用户知道它叫 dp1 还是 usbc
                if let Some((node, value)) = rest.rsplit_once(':') {
                    if let Ok(value) = value.parse::<u16>() {
                        // **本机口径校验**：对端报的是它**自己那块屏**的端口
                        // 编号，两块屏的 0x0F/0x12 各算各的 —— 拿别人的编号
                        // 切自己的屏，按下去就是黑屏（v0.6.35 幽灵口同源）。
                        // 面板早就这么校验了，托盘这条一直没校验：两台同款
                        // 显示器时看不出来，5 台配不同显示器必踩。
                        let known = self
                            .config
                            .profile_for(&self.profile_key)
                            .map(|p| p.inputs.iter().any(|s| s.value == value))
                            .unwrap_or(false);
                        if !known {
                            self.status = format!(
                                "本机没有 0x{value:02X} 这个口（{node} 报的是它那块屏的\
                                 编号，两块屏的编号不通用），已取消切换"
                            );
                        } else {
                            let name = self
                                .mesh
                                .live_peers()
                                .into_iter()
                                .find(|p| p.node == node)
                                .map(|p| p.own_input)
                                .unwrap_or_default();
                            self.status = format!("正在切到 {}…", node);
                            let _ = self.tx.send(Command::SwitchRaw { name, value });
                        }
                    }
                }
            }
            self.refresh();
        }
    }

    /// 全局热键。`hotkey_id` 对不上就忽略 —— 用户改键之后旧的注册还
    /// 没注销完，那一两次重复事件不该触发两次切换。
    fn on_hotkey_events(&mut self) {
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if Some(event.id) != self.hotkey_id {
                continue;
            }
            if event.state != global_hotkey::HotKeyState::Pressed {
                continue;
            }
            if self.target.is_empty() {
                self.status =
                    "热键还没指定目标：到控制面板的输入源列表点「设为热键」".to_string();
            } else if let Some(value) =
                configured_value(&self.config, &self.profile_key, &self.target)
            {
                self.status = format!("正在切换到 {}…", self.target);
                let _ = self.tx.send(Command::SwitchRaw {
                    name: self.target.clone(),
                    value,
                });
            } else {
                // 宁可不切，也不要拿标准值瞎猜 —— 猜错会把屏幕切到空口上
                self.status = format!("热键目标 {} 没有对应的口，请在菜单里重新选", self.target);
            }
            self.refresh();
        }
    }
}
