//! 链路摆位：KvmLink。
//!
//! 面板把"已发现但还没配链路"的对端拖进格子时走这。链路表里没有它就地建
//! 一条（只认**在线**对端，地址来自发现层广播，本机不编造），然后按方向和
//! 模式互相纠正："有门却跟随"画在格子里却不占边，"无门的 Edge"整屏都被吞。
//! 改完必须重启实例 —— 掩码是按链路表算出来的。

use crate::tray::kvmctl::restart_kvm;
use crate::tray::log_line;
use crate::tray::Ui;

impl Ui {
    pub(super) fn on_kvm_link(&mut self, node: String, side: Option<String>, mode: Option<String>) {
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
                    if link.side.is_empty() && link.mode_or(&self.config.kvm.mode) == "edge" {
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
                            "这条链路没有方向，不能设成多屏共享（先给它选个方向）".to_string(),
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
            if problem.is_none() && !link.side.trim().is_empty() && link.mode.trim() == "follow" {
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
}
