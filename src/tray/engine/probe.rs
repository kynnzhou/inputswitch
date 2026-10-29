//! 探测类命令：Rescan（重新检测）/ CheckSupport（体检）/ ScanPorts（扫描输入口）。
//!
//! 共同点是要么真的去读 DDC、要么真的去切屏试探，所以都带着一段
//! "为什么不能更激进"的注释（重读不清缓存等于没点、扫完会进静默期）。

use std::sync::atomic::Ordering as AtomicOrdering;
use std::time::Duration;

use crate::models::{Presence, VCP_INPUT_SOURCE};

use super::status::{inspect, status_text, sync_own};
use super::Ctx;
use crate::tray::{log_line, HAS_DISPLAY, MAINT_BUSY, PORT_NOTE};

/// 维护动作进行中：创建时置位 MAINT_BUSY，Drop 时复位 —— panic 也不留
/// 锁（三个 handler 都是同步长任务，中途 panic 时钮必须还能点）。入口
/// `let _busy = BusyGuard::enter();` 即可，出口自动还钮。
struct BusyGuard;
impl BusyGuard {
    fn enter() -> Self {
        MAINT_BUSY.store(true, AtomicOrdering::SeqCst);
        BusyGuard
    }
}
impl Drop for BusyGuard {
    fn drop(&mut self) {
        MAINT_BUSY.store(false, AtomicOrdering::SeqCst);
    }
}

impl Ctx {
    pub(super) fn on_rescan(&mut self) {
        let _busy = BusyGuard::enter();
        // 这一步必须真的重读：缓存里存着上一次的失败结论，
        // 不清掉的话这个按钮点了等于没点（用户报的就是这个）
        let report = self.switcher.rescan();
        log_line(&format!("重新检测：{report}"));
        if let Ok(mut note) = PORT_NOTE.lock() {
            *note = report.clone();
        }
        let _ = sync_own(&mut self.switcher, &self.mesh);
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
            let own = self.switcher.config.own_value;
            if own > 0 {
                if let Some(display) = self.switcher.pick_display(None) {
                    if let Some(now) = self.switcher.backend().get_vcp(&display, VCP_INPUT_SOURCE) {
                        if now != own {
                            let name = self
                                .switcher
                                .config
                                .label_for(&self.switcher.config.own_input.clone());
                            let ok =
                                self.switcher
                                    .backend()
                                    .set_vcp(&display, VCP_INPUT_SOURCE, own);
                            log_line(&format!(
                                "重新检测把画面留在了 0x{now:02X}（本机口是 0x{own:02X} {name}），{}",
                                if ok { "已切回" } else { "切回失败，请手动切一次" }
                            ));
                            if ok {
                                self.switcher.suppress_ddc(Duration::from_millis(1500));
                            }
                        }
                    }
                }
            }
        }
        let target = self.switcher.config.hotkey_target.clone();
        let mut text = status_text(&mut self.switcher);
        if text.is_empty() {
            text = report;
        }
        self.push(&target, text);
    }

    pub(super) fn on_check_support(&mut self) {
        let _busy = BusyGuard::enter();
        let (line, detail) = inspect(&self.switcher);
        self.support = line;
        log_line(&format!("支持性检测：{}", detail));
        let target = self.switcher.config.hotkey_target.clone();
        self.push(&target, detail);
    }

    pub(super) fn on_scan_ports(&mut self) {
        let _busy = BusyGuard::enter();
        let target = self.switcher.config.hotkey_target.clone();
        log_line("开始扫描输入口");

        // 物理试探有个硬前提：**切走之后得有谁把画面送回来**。
        // 本机一旦失去 DDC 就再也发不出指令，这个前提不成立时切过去就是单程票。
        let peers_now = self.mesh.live_peers();
        let local_paired = self.switcher.config.pair.is_set();
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

        self.push(
            &target,
            if can_relay {
                "正在扫描，屏幕会黑几次…".to_string()
            } else {
                "正在汇总已知信息（不做切屏试探）…".to_string()
            },
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

        let relay_mesh = self.mesh.clone();
        // 空切片 = 一个口都不物理试探，只汇总本机与对端已经报过的信息
        let no_probe: [u16; 0] = [];
        let only = if can_relay {
            None
        } else {
            Some(no_probe.as_slice())
        };
        let result = self.switcher.scan_presence(
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
        let _ = sync_own(&mut self.switcher, &self.mesh);
        log_line(&text);
        self.push(&target, text);
    }
}
