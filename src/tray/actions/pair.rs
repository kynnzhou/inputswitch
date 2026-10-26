//! 配对码：保存 / 随机生成。
//!
//! 共同收尾：`mesh` 与文件服务的钥匙要**同时**换，然后重启 KVM 实例 ——
//! `Sealer` 是启动时快照且按设计绝不重建（nonce counter 不能从头算），
//! 不换实例的话事件流还在用旧钥匙，界面说"已签名"实际两边码不同。

use crate::tray::kvmctl::restart_kvm;
use crate::tray::log_line;
use crate::tray::sys::mask_code;
use crate::tray::Ui;

impl Ui {
    pub(super) fn on_pair(&mut self, code: String) {
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

    pub(super) fn on_generate_code(&mut self) {
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
}
