//! 剪贴板按需传输：PasteIntent（把待传文件推给对端）/ RemoteClipboard（转交主线程）。
//!
//! PasteIntent 是全文唯一"起线程"的分支：传输可能跑几十秒，绝不能阻塞
//! 工作循环（心跳和切屏命令都还在后面排队）。

use std::sync::atomic::Ordering as AtomicOrdering;
use std::thread;

use crate::transfer;

use super::Ctx;
use crate::tray::kvm_now;
use crate::tray::{log_line, UserEvent, CLIP_BUSY, CLIP_PENDING};

impl Ctx {
    pub(super) fn on_paste_intent(&mut self) {
        // 用户在对端屏幕上按下了 Ctrl+V，那个键已经被抓取层吞掉。
        // 这里起线程把文件推过去 —— 绝不能阻塞本循环，心跳和切屏
        // 命令都还在后面排队。
        if CLIP_BUSY.swap(true, AtomicOrdering::SeqCst) {
            log_line("上一批剪贴板文件还在传，这次粘贴先忽略");
            return;
        }
        let files = CLIP_PENDING.lock().map(|g| g.clone()).unwrap_or_default();
        if files.is_empty() {
            CLIP_BUSY.store(false, AtomicOrdering::SeqCst);
            if let Some(h) = kvm_now() {
                h.set_paste_ready(false);
                h.send_key_to_peer(0x09); // V
            }
            log_line("剪贴板里没有待传文件，这次粘贴按原样转发给对端");
            return;
        }
        let peers = self.mesh.live_peers();
        let want = self.switcher.config.kvm.peer_node.clone();
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
            return;
        };
        let ip = peer.addr.ip();
        let port = self.switcher.config.transfer.port;
        let node = self.switcher.config.node.clone();
        let key = crate::pair::link_key(&self.switcher.config.pair.code);
        let count = files.len();
        let peer_name = peer.node.clone();
        let proxy_t = self.proxy.clone();
        let proxy_p = self.proxy.clone();
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
                        let _ =
                            proxy_p.send_event(UserEvent::Transfer(format!("剪贴板传输… {pct}%")));
                    }
                },
            );
            let _ = proxy_t.send_event(UserEvent::PasteDone(res));
        });
    }

    pub(super) fn on_remote_clipboard(&mut self, text: String) {
        // 写剪贴板得回主线程，这里只负责转交
        let _ = self.proxy.send_event(UserEvent::RemoteClipboard(text));
    }
}
