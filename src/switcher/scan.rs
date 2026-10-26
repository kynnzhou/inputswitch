//! 扫输入口：逐口物理试探，扫完把画面送回本机。
//!
//! scan_presence 分两段：先做不碰屏幕的那一半（本机口、对端自报、已知
//! 无效口），剩下真正要试探的通常只有 1~2 个。试探的判据只有一条：
//! **写过去之后显示器会不会自己退回来**。退回来 = 没信号（安全）；留住 =
//! 有主机，此时本机已失去 DDC，必须靠 go_home 把画面弄回来。

use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::models::{Display, Presence, VCP_INPUT_SOURCE};

use super::types::{PortProbe, ProbeSource, ScanReport};
use super::Switcher;

impl Switcher {
    /// 扫出"每个输入口上有没有主机"。
    ///
    /// 分两段，先做不碰屏幕的那一半：
    ///
    /// 1. 本机自己占的那个口，一定是 `Occupied`；
    /// 2. 对端机器通过局域网广播过它占哪个口，直接采信。
    ///
    /// 剩下真正需要物理试探的通常只有 1~2 个。试探的判据只有一条：
    /// **写过去之后显示器会不会自己退回来**。
    ///
    /// - 退回来了 → 那个口没信号。这种试探是安全的，显示器自己就回来了。
    /// - 留住了 → 那个口有主机。此时本机已经失去 DDC，必须靠对面帮忙切回。
    ///
    /// 所以 `stop_on_first_occupied` 一般应该是 `true`：双机共用一台显示器，
    /// 找到对面就够了，再扫下去只是多切几次屏。
    pub fn scan_presence(
        &mut self,
        peers: &[(String, u16)],
        idle_window: Duration,
        stop_on_first_occupied: bool,
        only: Option<&[u16]>,
        mut relay_back: impl FnMut(u16) -> bool,
        mut on_step: impl FnMut(&str),
    ) -> Result<ScanReport, String> {
        let Some(display) = self.pick_display(None) else {
            return Err("屏幕不在本机，无法扫描（要在持有屏幕的那台运行）".into());
        };
        let key = self.identity(&display);
        // 读不到就退回"上次记住的本机口"：屏幕要是已经不在本机，这里必然读空，
        // 而扫描从头到尾都拿它当回家的路标，宁可先有个值也别是 None。
        let remembered = self.config.own_value;
        let original = self
            .backend
            .get_vcp(&display, VCP_INPUT_SOURCE)
            .or(if remembered > 0 { Some(remembered) } else { None });

        let profile = self
            .config
            .profile_for(&key)
            .cloned()
            .ok_or_else(|| format!("显示器 {} 还没有输入口列表", display.name))?;
        if profile.inputs.is_empty() {
            return Err("还没读到任何输入口，先运行一次「重新检测显示器」".into());
        }

        let mut ports: Vec<PortProbe> = Vec::new();
        let mut steps: Vec<String> = Vec::new();
        let mut pending: Vec<(u16, String)> = Vec::new();

        // ── 第一段：不碰屏幕 ──────────────────────────────
        for src in &profile.inputs {
            if Some(src.value) == original {
                ports.push(PortProbe {
                    value: src.value,
                    name: src.name.clone(),
                    presence: Presence::Occupied,
                    via: ProbeSource::SelfHost,
                });
                continue;
            }
            if let Some((node, _)) = peers.iter().find(|(_, v)| *v == src.value) {
                // **对端自报不等于本机证据**（v0.6.71）。
                //
                // 以前这里写 Occupied，于是 0x12 这种"固件声明过、本机从没
                // 切过去验证"的口通过了 has_evidence()，面板上多出一条
                // `输入源 0x12`（连名字都没有），点「切换」直接黑屏。
                // 而且每台显示器的口编号是各自固件定义的，对端说的 0x12
                // 未必就是本机这块屏的 0x12 —— 用户实测那个口上什么都没接。
                //
                // 现在只记一行日志，口的状态保持 Unknown：它仍然能被"真的
                // 切过去验证"救活，但不会凭别人一句话就出现在切换列表里。
                steps.push(format!(
                    "0x{:02X} ← {} 自己报告占用（仅作回程参考，不记为本机输入源）",
                    src.value, node
                ));
                ports.push(PortProbe {
                    value: src.value,
                    name: src.name.clone(),
                    presence: Presence::Unknown,
                    via: ProbeSource::PeerReport,
                });
                continue;
            }
            // 上次已经确认固件不接受这个取值，没必要再黑一次屏
            let cached = self.config.presence_of(&key, src.value);
            if cached == Presence::Invalid {
                steps.push(format!("0x{:02X} 上次已确认不存在，跳过", src.value));
                ports.push(PortProbe {
                    value: src.value,
                    name: src.name.clone(),
                    presence: Presence::Invalid,
                    via: ProbeSource::Cached,
                });
                continue;
            }
            pending.push((src.value, src.name.clone()));
        }

        if pending.is_empty() {
            on_step("所有口的状态都已经知道，这一轮不用切屏");
        }

        // `only` 只限制"要不要真的切过去试"，不影响已经知道的部分
        if let Some(list) = only {
            let mut kept: Vec<(u16, String)> = Vec::new();
            for (value, name) in pending.drain(..) {
                if list.contains(&value) {
                    kept.push((value, name));
                } else {
                    steps.push(format!("0x{:02X} 不在本次范围内，保留上次结果", value));
                    ports.push(PortProbe {
                        value,
                        name,
                        presence: self.config.presence_of(&key, value),
                        via: ProbeSource::Cached,
                    });
                }
            }
            pending = kept;
        }

        // ── 第二段：真的切过去看 ──────────────────────────
        let mut aborted: Option<String> = None;
        let mut back_home = true;
        let display_name = display.name.clone();

        for (value, name) in pending {
            if !self.display_named(&display_name) {
                on_step("等屏幕回到本机…");
                if !self.wait_named(&display_name, Duration::from_secs(12)) {
                    aborted = Some("屏幕不在这台机器上了，扫描中断".into());
                    back_home = false;
                    break;
                }
                // 刚回来时 DDC 通道还会哑一下
                sleep(Duration::from_millis(600));
            }

            // 每轮都重新取句柄：上一轮切走再回来时 registry id 已经变了，
            // 拿着旧句柄发 DDC 会被系统直接拒掉，然后被误判成"这个口不存在"。
            let Some(handle) = self.display_by_name(&display_name) else {
                aborted = Some("显示器句柄丢失，扫描中断".into());
                back_home = false;
                break;
            };

            let note = format!("试探 0x{:02X} {}…", value, name);
            on_step(&note);
            steps.push(note);

            if !self.backend.set_vcp(&handle, VCP_INPUT_SOURCE, value) {
                steps.push(format!("  0x{:02X} 写入被拒（固件不接受这个值）", value));
                ports.push(PortProbe {
                    value,
                    name,
                    presence: Presence::Invalid,
                    via: ProbeSource::Physical,
                });
                continue;
            }

            if !self.wait_until_gone(&handle, Duration::from_millis(3500)) {
                // 显示器毫无反应：这个取值在固件里根本不存在
                steps.push(format!("  0x{:02X} 显示器没反应 → 该口不存在", value));
                ports.push(PortProbe {
                    value,
                    name,
                    presence: Presence::Invalid,
                    via: ProbeSource::Physical,
                });
                continue;
            }

            if self.bounced_back(&handle, idle_window) {
                steps.push(format!("  0x{:02X} 切过去又退回来 → 无信号", value));
                ports.push(PortProbe {
                    value,
                    name,
                    presence: Presence::Idle,
                    via: ProbeSource::Physical,
                });
                // 退回来 ≠ 退回原口：有的显示器会落回它自己的默认口。
                // 不校正的话，后面每一步都是从一个错的口出发，最后就是
                // "扫完停在别的口上"。
                if let Some(home) = original {
                    if self.write_home(&display_name, home) {
                        steps.push("  已确认画面回到原口".into());
                    } else {
                        steps.push("  退回来的不是原口，正在纠正…".into());
                        if !self.go_home(
                            &display_name,
                            Some(home),
                            Duration::from_secs(3),
                            &mut relay_back,
                            &mut on_step,
                        ) {
                            back_home = false;
                            aborted = Some(format!(
                                "0x{value:02X} 那个口没信号，显示器退回来却落在了别的口上，且纠正失败"
                            ));
                            break;
                        }
                        steps.push("  已纠正回原口".into());
                    }
                }
                continue;
            }

            steps.push(format!("  0x{:02X} 画面留住了 → 有主机", value));
            ports.push(PortProbe {
                value,
                name,
                presence: Presence::Occupied,
                via: ProbeSource::Physical,
            });

            // 到了这里屏幕在对面，必须把自己弄回来才能继续。
            // self_wait 传 0：有主机在输出的口绝不会自己退回来，干等只是浪费时间。
            if !self.go_home(
                &display_name,
                original,
                Duration::ZERO,
                &mut relay_back,
                &mut on_step,
            ) {
                back_home = false;
                aborted = Some(format!(
                    "已切到有主机的 0x{value:02X}，但没能切回本机 —— \
                     对面那台要么没开本程序，要么两边配对码不一致（它把代切请求当\
                     未签名的包丢了）。现在需要手动把显示器切回本机。"
                ));
                break;
            }
            steps.push("  已切回本机，继续".into());

            if stop_on_first_occupied {
                break;
            }
        }

        // 不管循环因为什么退出，最后都得确认画面回到了起点。
        // 这一步是幂等的：已经在本机原口上就立刻返回，不会多切一次屏。
        if !self.go_home(
            &display_name,
            original,
            Duration::from_secs(5),
            &mut relay_back,
            &mut on_step,
        ) {
            back_home = false;
            if aborted.is_none() {
                aborted = Some("扫描已经结束，但没能把画面切回原来的输入口".into());
            }
        }

        // 结论一次性落盘
        let items: Vec<(u16, Presence)> = ports.iter().map(|p| (p.value, p.presence)).collect();
        self.config.set_presence(&key, &items);

        Ok(ScanReport {
            ports,
            original,
            back_home,
            aborted,
            steps,
        })
    }

    fn wait_named(&self, name: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.display_named(name) {
                return true;
            }
            sleep(Duration::from_millis(200));
        }
        self.display_named(name)
    }

    /// 按型号名取当前句柄。切走再回来 registry id 会变，旧句柄不能复用，
    /// 所以每次要发 DDC 之前都得重新拿一次。
    fn display_by_name(&self, name: &str) -> Option<Display> {
        self.backend
            .list_displays()
            .into_iter()
            .find(|d| d.name == name)
    }

    /// 在"本机拿着 DDC"的前提下把输入源写回 `home`，并读回确认。
    ///
    /// 之所以要读回：写完到显示器真正切过去之间有一段几十毫秒的空档，
    /// 这时 `get_vcp` 常常读回 `None`，不重试就会把"其实成功了"判成失败。
    fn write_home(&mut self, display_name: &str, home: u16) -> bool {
        for attempt in 0..2 {
            let Some(display) = self.display_by_name(display_name) else {
                return false;
            };
            if self.backend.get_vcp(&display, VCP_INPUT_SOURCE) == Some(home) {
                return true;
            }
            if attempt > 0 {
                // 第二次写之前先歇一下，别把显示器的 DDC 队列打满
                sleep(Duration::from_millis(400));
            }
            self.backend.set_vcp(&display, VCP_INPUT_SOURCE, home);
            sleep(Duration::from_millis(900));
        }
        // 最后再等一拍再读，避开切换瞬间的静默期
        sleep(Duration::from_millis(600));
        match self.display_by_name(display_name) {
            Some(d) => self.backend.get_vcp(&d, VCP_INPUT_SOURCE) == Some(home),
            None => false,
        }
    }

    /// 把画面送回本机扫描前所在的那个口。
    ///
    /// 扫描的每一步都是"切过去再看"，随时可能把自己留在对面 —— 而一旦画面走了，
    /// 本机就再也发不出 DDC 指令。所以退出时必须主动把画面弄回来，不能指望
    /// 显示器自己会退：很多显示器切到空口后只显示"无信号"，然后就停在那里了。
    ///
    /// 三种手段按代价递增依次尝试，任何一步成功就停：
    /// 1. 本机还拿着 DDC → 直接写回去（最快，也不依赖对面）
    /// 2. 显示器只是空口 OSD 超时 → 等它自己退回，再校正到原来的口
    /// 3. 本机彻底失去 DDC → 只能请握着画面的那台代切（它得开着本程序）
    fn go_home(
        &mut self,
        display_name: &str,
        home: Option<u16>,
        self_wait: Duration,
        relay: &mut impl FnMut(u16) -> bool,
        on_step: &mut impl FnMut(&str),
    ) -> bool {
        let Some(home) = home else {
            // 连原来在哪个口都不知道，只能看画面有没有回来
            return self.display_named(display_name);
        };

        // 1) 本机还有 DDC
        if self.write_home(display_name, home) {
            return true;
        }

        // 2) 等它自己退回来（有主机在输出的口不会退，所以这一步可以给 0 秒跳过）
        if !self_wait.is_zero() {
            on_step("等显示器自己退回本机…");
            if self.wait_named(display_name, self_wait) && self.write_home(display_name, home) {
                return true;
            }
        }

        // 3) 本机发不出指令，只能请对面代切
        on_step("本机已拿不到 DDC，请对面那台切回来…");
        if relay(home) && self.wait_named(display_name, Duration::from_secs(15)) {
            if self.write_home(display_name, home) {
                return true;
            }
            // 画面回来了但停在别的口 —— 至少比完全没有画面强
            return self.display_named(display_name);
        }
        false
    }
}
