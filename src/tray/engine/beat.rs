//! 心跳与对端协作：Heartbeat（拓扑变化 + 自证收回）/ AskPeerSwitch（请对端代切）。
//!
//! 两者都是"本机自己发不出 DDC 或画面不在本机"时的兜底路径，判据注释
//! 比代码长 —— 每一句都是实测踩过之后补的，改前先读。

use std::time::{Duration, Instant};

use crate::kvm;

use super::status::{profile_key_of, status_text, sync_own};
use super::Ctx;
use crate::tray::kvm_now;
use crate::tray::log_line;

impl Ctx {
    pub(super) fn on_heartbeat(&mut self) {
        // 拓扑变化（显示器离开/回到本机）只靠枚举就能发现，零 I2C 开销。
        // 刚回来的头几秒链路还在重新协商，静默期把 DDC 读取全部挡掉。
        let displays_now: Vec<String> = self
            .switcher
            .displays()
            .into_iter()
            .map(|d| d.name)
            .collect();
        if displays_now != self.last_topology {
            self.switcher.suppress_ddc(Duration::from_secs(4));
            self.last_topology = displays_now;
            // 拓扑变了有两种可能：画面切走又回来（同一台显示器），
            // 或者真的**换了台显示器**（拔插 / 换 KVM 口）。
            // 后者必须按新显示器自己的能力表重新建表 —— 这正是用户
            // 心智里的"先检测显示器，再列它的输入源"。现在就做不行：
            // 链路还在重新协商，读能力表会拿到空值并被缓存住。
            self.pending_detect = Some(Instant::now());
        }
        // 静默期一过补一次自动检测：只在这台显示器还没建过档、
        // 或者建过但一个口都没读到时才真去读。已有的表（哪怕只有
        // 实测出来的几个口）一律不动，绝不因为一次拔插就重置结论。
        if let Some(t0) = self.pending_detect {
            if t0.elapsed() >= Duration::from_secs(5) {
                self.pending_detect = None;
                let needs = self.switcher.displays().iter().any(|d| {
                    let key = self.switcher.identity(d);
                    self.switcher
                        .config
                        .profile_for(&key)
                        .map(|p| p.inputs.is_empty())
                        .unwrap_or(true)
                });
                if needs {
                    let report = self.switcher.rescan();
                    log_line(&format!("换了显示器？自动检测：{report}"));
                }
            }
        }
        let current = sync_own(&mut self.switcher, &self.mesh);
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
                    if v > 0 && v == self.switcher.config.own_value {
                        handle.give_back();
                    }
                }
            }
        }
        let target = self.switcher.config.hotkey_target.clone();
        let text = status_text(&mut self.switcher);
        self.push(&target, text);
    }

    pub(super) fn on_ask_peer_switch(&mut self, value: u16) {
        // 本机没有接通的显示器，自己发不出 DDC；但对面那台现在握着画面，
        // 它的 DDC 是通的 —— 让它替我们把画面切到这个口。
        let peers = self.mesh.live_peers();
        let mut sent = 0usize;
        // 发不出去的原因要说出来。以前 send_command 只回 bool，"对端要码
        // 而本机没设"这种一定被丢的情况也算发出去了，界面就会谎报"几秒后
        // 屏幕会回来"——用户只能对着黑屏猜。
        let mut why: Vec<String> = Vec::new();
        for peer in &peers {
            match self.mesh.send_command(
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
        let key = profile_key_of(&self.switcher);
        let port = self
            .switcher
            .config
            .input_name_of(&key, value)
            .or_else(|| {
                (value == self.switcher.config.own_value
                    && !self.switcher.config.own_input.is_empty())
                .then(|| self.switcher.config.own_input.clone())
            })
            .map(|name| self.switcher.config.label_for(&name))
            .unwrap_or_else(|| format!("0x{value:02X}"));
        let text = if sent == 0 {
            if why.is_empty() {
                "没有在线对端可以代切".to_string()
            } else {
                format!("切不了：{}", why.join("；"))
            }
        } else if value == self.switcher.config.own_value {
            // 请对端切到本机插的那个口 = 把画面送回来。措辞跟着变：
            // "切到 USB-C" 会被读成"把对面的画面切到它自己的某个口"。
            format!("已请对端把画面切回本机（{port}），几秒后屏幕会回来")
        } else {
            format!("已请对端把画面切到 {port}，几秒后屏幕会回来")
        };
        let target = self.switcher.config.hotkey_target.clone();
        self.push(&target, text);
    }
}
