//! 控制面板转发过来的动作。
//!
//! 面板里凡是"只能主线程做"的事（开系统文件选择器、开浏览器、改配置、
//! 重启进程）都走 `UserEvent::Panel` 回到这里。纯粹的切屏/重扫不需要
//! 主线程，在 `run()` 的 dispatcher 里就转成 `Command` 发出去了。
//!
//! 这一层的方法有一个共同点：**都会改 `self.status`**。所以每个分支末尾
//! 都有 `log_line(&self.status)` —— 菜单第一行和面板顶部显示的是同一句。

use std::sync::Arc;

use tao::keyboard::ModifiersState;

use crate::kvm;
use crate::panel::Action;

use super::kvmctl::{
    explain_silent_peer, follow_link_node, kvm_boot, perms_ok, pick_peer, restart_kvm,
};
use super::log_line;
use super::send::{pick_folder, start_send};
use super::sys::{mask_code, open_path};
use super::{kvm_now, Ui};

impl Ui {
    /// 面板动作分发。`Action` 的定义在 `crate::panel`，加一个动作要同时
    /// 看那边和这里。
    pub(super) fn on_panel(&mut self, action: Action) {
        match action {
            Action::Pair(code) => {
                let code = code.trim().to_string();
                // 码没变就别重开共用键鼠：面板上点一次"保存"就把正在转发的那场
                // 断掉，属于无缘无故的伤害。变了才需要换钥匙。
                let changed = self.config.pair.code.trim() != code;
                self.config.pair.code = code.clone();
                let _ = self.config.save();
                self.mesh.set_key(crate::pair::link_key(&code));
                // 文件接收服务的 key 是共享可变的，就地换掉即可（不用重启监听器）
                if let Ok(mut k) = self.transfer_key.lock() {
                    *k = crate::pair::link_key(&code);
                }
                let text = if code.is_empty() {
                    // 空码 = 取消配对：mesh 会真的把 key 置空（link_key 对空串返回 None），
                    // 之后本机不再签名也不再验签。这句必须把后果说全，不然用户不知道
                    // 自己刚把门打开了。
                    "已取消配对：本机不再验证对方身份，同一网段的任何设备都能发指令。\
                     对面还留着码的话，它会拒收你的指令 —— 两边都取消，或重新填同一个码"
                        .to_string()
                } else {
                    format!("已保存配对码 {}，对面填同一个码就能连上", mask_code(&code))
                };
                if changed {
                    // 共用键鼠的 `Sealer` 是**启动时快照**、且按设计绝不重建（nonce
                    // counter 不能从头算），所以换码只能换实例。不重开的话事件流还在
                    // 用旧钥匙 —— 界面说"双向消息已签名"，实际两边码不同却谁也发现不了；
                    // 取消配对更糟：KVM 仍在加密，对端没码就当垃圾丢掉，表现成"都开着就是不通"。
                    restart_kvm(
                        &mut self.config,
                        &mut self.kvm_handle,
                        &self.mesh,
                        &self.tx,
                        &mut self.status,
                        text,
                    );
                } else {
                    self.status = text;
                    log_line(&self.status);
                }
            }
            Action::GenerateCode => {
                let code = crate::pair::random_code();
                self.config.pair.code = code.clone();
                let _ = self.config.save();
                self.mesh.set_key(crate::pair::link_key(&code));
                if let Ok(mut k) = self.transfer_key.lock() {
                    *k = crate::pair::link_key(&code);
                }
                let text = format!("新配对码 {code}，请在对面填同一个");
                restart_kvm(
                    &mut self.config,
                    &mut self.kvm_handle,
                    &self.mesh,
                    &self.tx,
                    &mut self.status,
                    text,
                );
            }
            Action::ToggleAutostart(on) => {
                let result = if on {
                    // 守护开关跟着一起写进 LaunchAgent / 计划任务：
                    // 两件事在系统层是同一份配置，分开写必然不同步。
                    crate::autostart::enable(self.config.kvm.keep_alive)
                } else {
                    crate::autostart::disable()
                };
                self.status = match result {
                    Ok(()) => {
                        if on {
                            "已开启开机自启".to_string()
                        } else {
                            "已关闭开机自启".to_string()
                        }
                    }
                    Err(err) => format!("开机自启设置失败：{err}"),
                };
            }
            Action::ToggleClipboard(on) => {
                self.config.clipboard.enabled = on;
                let _ = self.config.save();
                if on {
                    let _ = self.watcher.poll();
                }
                self.status = if on {
                    "剪贴板同步已开启".to_string()
                } else {
                    "剪贴板同步已关闭".to_string()
                };
                log_line(&self.status);
            }
            Action::ChooseInbox => {
                if let Some(dir) = pick_folder() {
                    self.config.transfer.inbox = dir.display().to_string();
                    let _ = self.config.save();
                    *self.inbox_shared.lock().unwrap() = dir.clone();
                    self.status = format!("接收目录已改为 {}", dir.display());
                    log_line(&self.status);
                }
            }
            Action::OpenInbox => {
                let dir = self.config.transfer.inbox_dir();
                let _ = std::fs::create_dir_all(&dir);
                open_path(&dir);
            }
            Action::SendFiles(node) => start_send(
                &self.config,
                &self.mesh,
                &self.proxy,
                &mut self.status,
                node.as_deref(),
            ),
            Action::KvmPower(on) => {
                if !on {
                    if let Some(handle) = self.kvm_handle.take() {
                        handle.stop();
                        if let Ok(mut slot) = super::KVM.lock() {
                            *slot = None;
                        }
                        self.config.kvm.enabled = false;
                        let _ = self.config.save();
                        self.status = "共用键鼠已关闭".to_string();
                    }
                } else if self.kvm_handle.is_some() {
                    self.status = "共用键鼠已经在运行".to_string();
                } else {
                    let peers = self.mesh.live_peers();
                    match pick_peer(&peers, &self.config.kvm.peer_node) {
                        None => {
                            self.status = "局域网里没找到对端，先让另一台开着本程序".to_string();
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
                                    // 缺权限时**别再说"勾上再点一次"** —— 勾上
                                    // 也不会生效：macOS 把 TCC 结论缓存在进程里，
                                    // 必须重启进程。这句话误导过用户两次。
                                    if perms_ok() {
                                        self.status = format!("共用键鼠开不了：{err}");
                                    } else {
                                        kvm::open_settings();
                                        self.status = "共用键鼠开不了：缺系统授权。设置页已打开，\
                                                  勾上本程序后点托盘里的「重启应用」，\
                                                  重启后才会生效"
                                            .to_string();
                                    }
                                    // **不改用户意图**：他点了「开启」，那就是
                                    // 想要开。以前这里写 enabled=false，等于
                                    // "失败一次就永远不再尝试" —— 副机（没有
                                    // 键鼠那台）碰上开机时网络没起就彻底废了，
                                    // 而人根本没法去那台机器上再点一次。
                                    self.config.kvm.enabled = true;
                                    let _ = self.config.save();
                                    if self.config.kvm.auto_resume && perms_ok() {
                                        self.status.push_str("；会自动重试");
                                    }
                                }
                            }
                        }
                    }
                }
                log_line(&self.status);
            }
            Action::KvmMode(mode) => {
                // ⚠️ 面板上**已经没有这个入口**（v0.6.58 删掉了「跟随画面 /
                // 多屏共享」两个全局按钮）。留着只为 CLI / 老调用方兼容。
                //
                // 为什么删：`set_mode_all` 会把同一个 mode 写给**每一条**
                // 链路，而一台机器完全可能"这台共用一块屏、那台并排共享"。
                // 全局按钮一点，那种混拓扑就被压平成清一色 follow，
                // 并排那台也跟着落进「没有门」—— 用户实测就是这么撞上的。
                // 模式现在只由摆位决定：摆在格子里 = edge，放进「没有门」= follow。
                //
                // 两种模式的物理前提不同，切换必须重启实例：
                // 边缘跨界要在「没抓取时」盯屏幕边缘，跟随画面则完全不动。
                let next = if mode == "edge" { "edge" } else { "follow" };
                let changed = self.config.kvm.mode != next;
                // 写透到链路表：v3 起运行时只认 links，只改镜像等于没改
                self.config.kvm.set_mode_all(next);
                let _ = self.config.save();
                if changed {
                    if let Some(handle) = self.kvm_handle.take() {
                        handle.stop();
                        if let Ok(mut slot) = super::KVM.lock() {
                            *slot = None;
                        }
                        self.config.kvm.enabled = false;
                        let _ = self.config.save();
                        self.status = if next == "edge" {
                            "已切到多屏共享（两台各有屏幕和键鼠）。共用键鼠需要重新打开"
                                .to_string()
                        } else {
                            "已切到跟随画面（一套键鼠，跟屏幕走）。共用键鼠需要重新打开"
                                .to_string()
                        };
                    } else {
                        self.status = if next == "edge" {
                            "已切到多屏共享：鼠标推到边缘越界".to_string()
                        } else {
                            "已切到跟随画面：画面切到哪台，键鼠跟到哪台".to_string()
                        };
                    }
                } else {
                    self.status = format!(
                        "已经是{}模式",
                        if next == "edge" { "多屏共享" } else { "跟随画面" }
                    );
                }
                log_line(&self.status);
            }
            Action::KvmRole(role) => {
                let next = if role == "remote" { "remote" } else { "console" };
                let changed = self.config.kvm.role != next;
                self.config.kvm.role = next.to_string();
                let _ = self.config.save();
                if changed {
                    if let Some(handle) = self.kvm_handle.take() {
                        handle.stop();
                        if let Ok(mut slot) = super::KVM.lock() {
                            *slot = None;
                        }
                        self.config.kvm.enabled = false;
                        let _ = self.config.save();
                    }
                    self.status = if next == "console" {
                        "已设为键鼠端（本机插着键鼠）。共用键鼠需要重新打开".to_string()
                    } else {
                        "已设为被控端（键鼠在对面）。共用键鼠需要重新打开".to_string()
                    };
                } else {
                    self.status = "角色没变".to_string();
                }
                log_line(&self.status);
            }
            Action::KvmScale(v) => {
                // 手感旋钮：只影响**被控时**注入的位移，发送侧不动。
                // 倍率在 KVM 启动时被快照进接收循环，所以改完要重建实例
                // —— restart_kvm 会先把键鼠交还回去再停，不会留两头
                // 都以为自己在转发的僵尸会话。
                let v = v.clamp(0.5, 2.0);
                let changed = (self.config.kvm.recv_scale - v).abs() > 0.001;
                self.config.kvm.recv_scale = v;
                let _ = self.config.save();
                if changed && self.config.kvm.enabled {
                    restart_kvm(
                        &mut self.config,
                        &mut self.kvm_handle,
                        &self.mesh,
                        &self.tx,
                        &mut self.status,
                        format!("接收位移倍率已改为 x{v:.2}"),
                    );
                } else {
                    self.status = if changed {
                        format!("接收位移倍率 x{v:.2}（下次开启共用键鼠生效）")
                    } else {
                        format!("接收位移倍率本来就是 x{v:.2}")
                    };
                    log_line(&self.status);
                }
            }
            Action::KvmAutoResume(on) => {
                self.config.kvm.auto_resume = on;
                let _ = self.config.save();
                self.status = if on {
                    "已开启：下次启动后，对端一上线就自动恢复共用键鼠".to_string()
                } else {
                    "已关闭自动恢复：启动后不会自己去开共用键鼠".to_string()
                };
                log_line(&self.status);
            }
            Action::KvmKeepAlive(on) => {
                self.config.kvm.keep_alive = on;
                let _ = self.config.save();
                // 守护是写在"开机自启"那份配置里的（plist / 计划任务），
                // 自启没开时它无处可挂 —— 顺手把自启带上并说清楚。
                let mut note = String::new();
                if on && !crate::autostart::is_enabled() {
                    if crate::autostart::enable(true).is_ok() {
                        note = "（同时打开了开机自启 —— 守护要靠它才生效）".to_string();
                    }
                } else if crate::autostart::is_enabled() {
                    // 自启已经开着：重写一次，把 KeepAlive / 计划任务加进去或去掉
                    let _ = crate::autostart::enable(on);
                }
                self.status = if on {
                    format!("已开启崩溃自动重启{note}")
                } else {
                    "已关闭崩溃自动重启（下次退出后不会自己回来）".to_string()
                };
                log_line(&self.status);
            }
            Action::Submachine(on) => {
                // 一键把"没有键鼠那台"该有的配置全打开：自启 + 自动恢复 +
                // 守护 + 角色 remote + 共用键鼠开着。少一条它就是个哑终端。
                self.config.kvm.auto_resume = on;
                self.config.kvm.keep_alive = on;
                if on {
                    self.config.kvm.role = "remote".to_string();
                    self.config.kvm.enabled = true;
                }
                let _ = self.config.save();
                let auto = if on {
                    crate::autostart::enable(true)
                } else {
                    crate::autostart::disable()
                };
                self.status = match auto {
                    Ok(()) if on => "副机模式已开启：开机自启 + 自动恢复 + 崩溃重启。\
                         记得给这台配固定 IP —— 共用键鼠按 IP 认机器，不认机器名"
                        .to_string(),
                    Ok(()) => "副机模式已关闭：不再自启、不再自动恢复、不再守护".to_string(),
                    Err(err) => format!("副机模式设置失败：{err}"),
                };
                log_line(&self.status);
            }
            Action::SetLabel { name, label } => {
                // 空串 = 恢复默认名（删掉别名，让 label_for 按 MCCS 渲染）
                let text = label.trim().to_string();
                if text.is_empty() {
                    self.config.labels.remove(&name);
                    self.status = format!("已恢复默认名：{}", self.config.label_for(&name));
                } else {
                    self.config.labels.insert(name.clone(), text.clone());
                    self.status = format!("已把这个口改名为「{text}」");
                }
                let _ = self.config.save();
                log_line(&self.status);
            }
            Action::KvmHandOver(node) => {
                // 不带 node 就是面板上那个全局按钮。多机时它靠 follow_target() 猜，
                // 猜错的表现是"点了 A、键鼠跑到 B" —— 直接拒绝，让人点名。
                let ambiguous = node.is_none() && self.mesh.live_peers().len() > 1;
                self.status = if ambiguous {
                    "有多台对端在线，得点名：点下面某一行末尾的「交给它」".to_string()
                } else {
                    match kvm_now() {
                        Some(handle) => {
                            // 点名交给哪台；不带 node 就是老行为（挑一条 Follow 链路）
                            let outcome = match node.as_deref() {
                                Some(node) => handle.hand_over_to(node).map(|()| true),
                                None => handle.follow(),
                            };
                            match outcome {
                                // 这个按钮**不碰画面**（只抓键鼠 + 发 Enter）。
                                // 所以不能说"画面这边黑着是正常的" —— 画面
                                // 既不会黑也不会切，一直显示本机。
                                Ok(true) => "键鼠已交给对端；画面还在这边。\
                                             想连画面一起过去，请点上面的「切换」"
                                    .to_string(),
                                Ok(false) => {
                                    let holder = kvm_now()
                                        .and_then(|h| h.active_node().map(|s| s.to_string()));
                                    match holder {
                                        Some(node) => {
                                            format!("键鼠已经在 {node} 手上，这次没有重复交接")
                                        }
                                        None => "已经有会话在进行，先收回键鼠再交".to_string(),
                                    }
                                }
                                Err(err) => {
                                    let target = match node.as_deref() {
                                        Some(n) => Some(n.to_string()),
                                        None => follow_link_node(&handle),
                                    };
                                    format!(
                                        "交不出去：{}",
                                        explain_silent_peer(&self.mesh, target.as_deref(), &err)
                                    )
                                }
                            }
                        }
                        None => "共用键鼠没开，先在面板上打开它".to_string(),
                    }
                };
                log_line(&self.status);
            }
            Action::KvmLink { node, side, mode } => {
                let mut changed = false;
                let mut problem: Option<String> = None;
                // 面板上可以把"已发现但还没配链路"的对端拖进格子 —— 链路表里没有它
                // 时现建一条。不建的话第二台机器只能手改 config.json（五台的拓扑下
                // 这就是硬阻塞），而且拖拽托盘里那类"未配链路"的卡片永远落不了地。
                //
                // 只认**在线**的对端：地址来自发现层广播，本机不编造。
                if !self.config.kvm.links.iter().any(|l| l.node == node) {
                    if let Some(p) = self.mesh.live_peers().into_iter().find(|p| p.node == node) {
                        self.config.kvm.links.push(crate::config::LinkConfig {
                            node: node.clone(),
                            side: String::new(),
                            mode: String::new(),
                            input: String::new(),
                            addr: p.addr.ip().to_string(),
                        });
                        changed = true;
                        log_line(&format!("新建了一条到 {node} 的链路"));
                    }
                }
                if let Some(link) = self.config.kvm.links.iter_mut().find(|l| l.node == node) {
                    if let Some(side) = side {
                        let side = side.trim().to_string();
                        // 空串 = 去掉门（变成跟随画面链路），这是合法值
                        if side != "left"
                            && side != "right"
                            && side != "top"
                            && side != "bottom"
                            && !side.is_empty()
                        {
                            problem = Some(format!("方向只能是 left/right/top/bottom：{side}"));
                        } else {
                            if link.side != side {
                                link.side = side;
                                changed = true;
                            }
                            if link.side.is_empty() && link.mode_or(&self.config.kvm.mode) == "edge"
                            {
                                // 无门的 Edge 链路是自相矛盾的组合：整屏都会被吞
                                link.mode = "follow".to_string();
                            }
                        }
                    }
                    if problem.is_none() {
                        if let Some(mode) = mode {
                            let mode = if mode == "follow" { "follow" } else { "edge" };
                            if mode == "edge" && link.side.trim().is_empty() {
                                problem = Some(
                                    "这条链路没有方向，不能设成多屏共享（先给它选个方向）"
                                        .to_string(),
                                );
                            } else if link.mode != mode {
                                link.mode = mode.to_string();
                                changed = true;
                            }
                        }
                    }
                    // 有门就是多屏共享、没门就是跟随画面 —— 这两个字段是绑定的。
                    // 「有门却跟随」在摆位图上会画在格子里，实际却根本不占边，
                    // 是那种看着正常、推过去毫无反应的组合，顺手纠正掉。
                    if problem.is_none()
                        && !link.side.trim().is_empty()
                        && link.mode.trim() == "follow"
                    {
                        link.mode = "edge".to_string();
                        changed = true;
                    }
                } else {
                    problem = Some(format!(
                        "没有叫 {node} 的链路，它现在也不在局域网里 —— 先让对面开机并开着本程序"
                    ));
                }

                if let Some(problem) = problem {
                    self.status = problem;
                } else if changed {
                    // v3 起运行时只认链路表，改完要回写镜像让老版本也能读到
                    self.config.kvm.mirror_from_primary();
                    let _ = self.config.save();
                    // 掩码是按链路表算出来的，改了必须重启实例才生效。
                    // 交给 restart_kvm 自动重开，不再"关掉让你自己再点一次" ——
                    // 摆位图是拖的，一张图要摆好几台，每拖一次都重开一遍没人受得了。
                    // （托盘菜单里改方向走的是同一个函数，两条路行为一致）
                    restart_kvm(
                        &mut self.config,
                        &mut self.kvm_handle,
                        &self.mesh,
                        &self.tx,
                        &mut self.status,
                        "链路已改".to_string(),
                    );
                } else {
                    self.status = "链路没变".to_string();
                }
                log_line(&self.status);
            }
            Action::RestartApp => {
                // 先落日志再退出：重启之后用户/我都要能对上"是这一步干的"
                log_line("收到重启请求（权限刚改过，旧进程里的 TCC 结论已过期）");
                super::sys::restart_app();
            }
            Action::KvmTakeBack => {
                self.status = match kvm_now() {
                    Some(handle) => {
                        // 被控时本地没有任何东西可以停 —— 只能请对端交还。
                        // 键盘用户在面板上点这个按钮，是他不推鼠标时的唯一出路。
                        let before = handle.state();
                        if handle.take_back_from_peer("面板上点了收回键鼠") {
                            "已请对端交还键鼠".to_string()
                        } else if before == kvm::State::Sending {
                            handle.give_back();
                            "键鼠已收回本机".to_string()
                        } else {
                            "键鼠本来就在本机".to_string()
                        }
                    }
                    None => "共用键鼠没开".to_string(),
                };
                log_line(&self.status);
            }
            Action::RecordHotkey => {
                // 面板上点「改键」→ 弹出这个常驻的原生小窗抢焦点收键盘事件。
                // 网页里收不到全局组合键，只能这么绕。
                if let Some(win) = self.record_window.as_ref() {
                    self.recording = true;
                    self.record_mods = ModifiersState::empty();
                    win.set_visible(true);
                    win.set_focus();
                    self.status = "请按下新的组合键（Esc 取消）…".to_string();
                } else {
                    self.status = "这台机器打不开录制窗口，请直接改配置里的 hotkey".to_string();
                }
                log_line(&self.status);
            }
            Action::OpenConfigDir => super::sys::open_config_dir(),
            _ => {}
        }
    }
}
