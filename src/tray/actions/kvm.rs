//! 共用键鼠总控：开关、模式、角色、倍率、自动恢复、守护、交接。
//!
//! 八个分支分两拨：改配置的（Mode / Role / Scale / AutoResume / KeepAlive）
//! 只落盘 + 换文案；动实例的（Power / TakeBack / HandOver）要真的去起停
//! Kvm。动实例的几处注释都比代码长 —— 每一句都是实测踩过之后补的。

use std::sync::Arc;

use crate::kvm;
use crate::tray::kvmctl::{
    explain_silent_peer, follow_link_node, kvm_boot, perms_ok, pick_peer, restart_kvm,
};
use crate::tray::{kvm_now, log_line, Ui};

impl Ui {
    pub(super) fn on_kvm_power(&mut self, on: bool) {
        if !on {
            if let Some(handle) = self.kvm_handle.take() {
                handle.stop();
                if let Ok(mut slot) = crate::tray::KVM.lock() {
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
                    let log: Arc<dyn Fn(String) + Send + Sync> = Arc::new(|text| log_line(&text));
                    match kvm_boot(&mut self.config, &peers, &self.tx, log) {
                        Ok((handle, text)) => {
                            self.kvm_handle = Some(handle.clone());
                            if let Ok(mut slot) = crate::tray::KVM.lock() {
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

    pub(super) fn on_kvm_mode(&mut self, mode: String) {
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
                if let Ok(mut slot) = crate::tray::KVM.lock() {
                    *slot = None;
                }
                self.config.kvm.enabled = false;
                let _ = self.config.save();
                self.status = if next == "edge" {
                    "已切到多屏共享（两台各有屏幕和键鼠）。共用键鼠需要重新打开".to_string()
                } else {
                    "已切到跟随画面（一套键鼠，跟屏幕走）。共用键鼠需要重新打开".to_string()
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
                if next == "edge" {
                    "多屏共享"
                } else {
                    "跟随画面"
                }
            );
        }
        log_line(&self.status);
    }

    pub(super) fn on_kvm_role(&mut self, role: String) {
        let next = if role == "remote" {
            "remote"
        } else {
            "console"
        };
        let changed = self.config.kvm.role != next;
        self.config.kvm.role = next.to_string();
        let _ = self.config.save();
        if changed {
            if let Some(handle) = self.kvm_handle.take() {
                handle.stop();
                if let Ok(mut slot) = crate::tray::KVM.lock() {
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

    pub(super) fn on_kvm_scale(&mut self, v: f64) {
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

    pub(super) fn on_kvm_auto_resume(&mut self, on: bool) {
        self.config.kvm.auto_resume = on;
        let _ = self.config.save();
        self.status = if on {
            "已开启：下次启动后，对端一上线就自动恢复共用键鼠".to_string()
        } else {
            "已关闭自动恢复：启动后不会自己去开共用键鼠".to_string()
        };
        log_line(&self.status);
    }

    pub(super) fn on_kvm_keep_alive(&mut self, on: bool) {
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

    pub(super) fn on_kvm_take_back(&mut self) {
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

    pub(super) fn on_kvm_hand_over(&mut self, node: Option<String>) {
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
                            let holder =
                                kvm_now().and_then(|h| h.active_node().map(|s| s.to_string()));
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
}
