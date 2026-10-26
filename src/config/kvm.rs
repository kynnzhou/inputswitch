//! 配置里的共用键鼠：`config::Kvm` 的链路表行为。
//!
//! 跟 `crate::kvm`（运行时子系统：抓取、注入、加解密）是两回事，这里只碰
//! config.json 里那张链路表：去重、升降级镜像、旧 UI 写透。
//! 测试跟着被测代码住：normalize / 镜像写透这些不变量锁在本文件。

use super::schema::{Kvm, LinkConfig};

impl Kvm {
    /// 主链路：`peer_node` 记住的那条，找不到就第一条。
    pub fn primary(&self) -> Option<&LinkConfig> {
        if !self.peer_node.trim().is_empty() {
            if let Some(l) = self
                .links
                .iter()
                .find(|l| l.node.eq_ignore_ascii_case(self.peer_node.trim()))
            {
                return Some(l);
            }
        }
        self.links.first()
    }

    /// 归一化链路表，返回值 = 配置有没有被改动（要落盘）。
    ///
    /// 三件事：① 去掉没填节点的空行；② 同一台对端只留第一条（重复链路会
    /// 让"同一台机器占两条边"，掩码里两条边都通向它，出门方向就随机了）；
    /// ③ 表空但有旧字段时，用旧字段升成一条链路。
    pub fn normalize(&mut self) -> bool {
        let before = (self.links.len(), self.peer_node.clone());
        self.links.retain(|l| !l.node.trim().is_empty());
        let mut seen: Vec<String> = Vec::new();
        self.links.retain(|l| {
            let key = l.node.trim().to_ascii_lowercase();
            if seen.contains(&key) {
                false
            } else {
                seen.push(key);
                true
            }
        });
        // 旧配置升上来：一条链路，方向来自降级镜像
        if self.links.is_empty() && !self.peer_node.trim().is_empty() {
            self.links.push(LinkConfig {
                node: self.peer_node.trim().to_string(),
                side: self.side.clone(),
                mode: self.mode.clone(),
                input: String::new(),
                addr: String::new(),
            });
        }
        let changed = before.0 != self.links.len() || before.1 != self.peer_node;
        let mirrored = self.mirror_from_primary();
        changed || mirrored
    }

    /// 主链路 → 降级镜像。旧版本读到的仍是"一条链路 + 一个方向"。
    pub fn mirror_from_primary(&mut self) -> bool {
        let Some(l) = self.links.first() else {
            return false;
        };
        let (node, side, mode) = (l.node.clone(), l.side.clone(), l.mode.clone());
        let mut changed = false;
        if self.peer_node != node {
            self.peer_node = node;
            changed = true;
        }
        // 空 side 是"没有门"，别拿它覆盖旧字段里那个真实方向 ——
        // 否则跟随画面链路会把降级镜像清成空，老版本读不出方向
        if !side.trim().is_empty() && self.side != side {
            self.side = side;
            changed = true;
        }
        if !mode.trim().is_empty() && self.mode != mode {
            self.mode = mode;
            changed = true;
        }
        changed
    }

    /// 旧 UI 改方向时写透到主链路。返回是否真的变了。
    ///
    /// 只改镜像不写链路的话，界面上"方向"改了却毫无效果 —— 因为 v3 起
    /// 运行时只认链路表。
    pub fn set_primary_side(&mut self, side: &str) -> bool {
        self.side = side.to_string();
        match self.primary().map(|l| l.node.clone()) {
            Some(node) => {
                if let Some(l) = self.links.iter_mut().find(|l| l.node == node) {
                    l.side = side.to_string();
                } else {
                    return true;
                }
            }
            None => return true,
        }
        true
    }

    /// 旧 UI 切模式时写透到主链路（旧语义是全局一个模式，所以整表一起改）。
    pub fn set_mode_all(&mut self, mode: &str) {
        self.mode = mode.to_string();
        for l in self.links.iter_mut() {
            l.mode = mode.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(node: &str, side: &str) -> LinkConfig {
        LinkConfig {
            node: node.to_string(),
            side: side.to_string(),
            mode: String::new(),
            input: String::new(),
            addr: String::new(),
        }
    }

    /// 老配置（只有降级镜像）必须能升出链路表，方向不能丢。
    #[test]
    fn legacy_kvm_upgrades_to_one_link() {
        let mut kvm = Kvm {
            mode: "edge".into(),
            side: "left".into(),
            peer_node: "WIN-A".into(),
            ..Default::default()
        };
        assert!(kvm.normalize());
        assert_eq!(kvm.links.len(), 1);
        assert_eq!(kvm.links[0].node, "WIN-A");
        assert_eq!(kvm.links[0].side, "left");
        assert_eq!(kvm.links[0].mode, "edge");
    }

    /// 同一台对端配两条 = 两条边都通向它，出门方向会随机 → 必须去重。
    #[test]
    fn duplicate_links_collapse() {
        let mut kvm = Kvm {
            links: vec![link("WIN-A", "right"), link("win-a", "top")],
            ..Default::default()
        };
        kvm.normalize();
        assert_eq!(kvm.links.len(), 1);
        assert_eq!(kvm.links[0].side, "right");
    }

    /// 主链路写透到镜像：老版本读到的仍是"一条链路 + 一个方向"。
    #[test]
    fn mirror_follows_primary() {
        let mut kvm = Kvm {
            links: vec![link("WIN-A", "bottom"), link("WIN-B", "left")],
            ..Default::default()
        };
        kvm.links[0].mode = "edge".into();
        assert!(kvm.mirror_from_primary());
        assert_eq!(kvm.peer_node, "WIN-A");
        assert_eq!(kvm.side, "bottom");
        assert_eq!(kvm.mode, "edge");
        // 第二条不该影响镜像
        kvm.links[1].side = "right".into();
        assert!(!kvm.mirror_from_primary());
        assert_eq!(kvm.side, "bottom");
    }

    /// 跟随画面链路 side 为空，不能把镜像里的方向清成空 —— 老版本会读不出方向。
    #[test]
    fn empty_side_keeps_mirror() {
        let mut kvm = Kvm {
            side: "right".into(),
            links: vec![link("WIN-A", "")],
            ..Default::default()
        };
        kvm.mirror_from_primary();
        assert_eq!(kvm.side, "right");
    }

    /// 旧 UI 改方向必须写透到链路表，否则界面上改了、运行时没变。
    #[test]
    fn set_primary_side_writes_through() {
        let mut kvm = Kvm {
            links: vec![link("WIN-A", "right"), link("WIN-B", "left")],
            peer_node: "WIN-B".into(),
            ..Default::default()
        };
        kvm.set_primary_side("top");
        assert_eq!(kvm.links[1].side, "top");
        assert_eq!(kvm.links[0].side, "right");
    }

    /// 主链路按 peer_node 找，找不到退回第一条。
    #[test]
    fn primary_resolution() {
        let mut kvm = Kvm {
            links: vec![link("WIN-A", "right")],
            ..Default::default()
        };
        assert_eq!(kvm.primary().map(|l| l.node.as_str()), Some("WIN-A"));
        kvm.peer_node = "GONE".into();
        assert_eq!(kvm.primary().map(|l| l.node.as_str()), Some("WIN-A"));
    }
}
