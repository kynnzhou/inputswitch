//! 切换：按名字切（switch）与按 MCCS 标准值切（switch_raw）。
//!
//! 两条路共用同一套"怎么算成功"的判据，是全文件最容易说谎的地方：有的
//! 显示器切走后仍挂在拓扑里（EDID 一直活着），"离开"信号永远等不到，只能
//! 靠 VCP 读值确认；反过来，等了 8 秒两个信号都没到的失败是**证据不足**的
//! 失败（fail_ambiguous），调用方绝不能据此把键鼠抢收回来。

use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::models::{Display, Presence, VCP_INPUT_SOURCE};

use super::types::SwitchOutcome;
use super::Switcher;

/// `wait_leave_or_value` 的结论
enum LeaveWait {
    /// 显示器离开了本机拓扑
    Gone,
    /// VCP 已读到目标值（显示器没离开拓扑也认）
    Confirmed,
    /// 超时，两个信号都没等到
    Timeout,
}

impl Switcher {
    pub fn switch(
        &mut self,
        input_name: &str,
        model: Option<&str>,
        verify: bool,
        timeout: Duration,
    ) -> SwitchOutcome {
        let Some(display) = self.pick_display(model) else {
            return SwitchOutcome::fail("没有找到可操作的显示器");
        };

        let Some(profile) = self.config.profile_for(&self.identity(&display)).cloned() else {
            return SwitchOutcome::fail(format!(
                "显示器 {} 还没有配置输入源，先运行 wizard",
                display.name
            ));
        };

        let Some(source) = profile.find(input_name).cloned() else {
            let known = if profile.inputs.is_empty() {
                "无".to_string()
            } else {
                profile
                    .inputs
                    .iter()
                    .map(|s| format!("{}({})", s.name, s.value))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            return SwitchOutcome::fail(format!("未知输入源 {}，已配置的有：{}", input_name, known));
        };

        if !self.backend.set_vcp(&display, VCP_INPUT_SOURCE, source.value) {
            return SwitchOutcome::fail("DDC 写入被拒绝（该口可能不支持 DDC/CI）");
        }

        if !verify {
            self.state.set(&profile.uid, &source.name);
            return SwitchOutcome::ok(
                &source.name,
                format!("已发送切换到 {} 的指令（未校验）", source.name),
            );
        }

        if self.wait_until_gone(&display, timeout) {
            self.state.set(&profile.uid, &source.name);
            return SwitchOutcome::ok(
                &source.name,
                format!("已切换到 {}（显示器已离开本机）", source.name),
            );
        }

        if self.backend.get_vcp(&display, VCP_INPUT_SOURCE) == Some(source.value) {
            self.state.set(&profile.uid, &source.name);
            return SwitchOutcome::ok(&source.name, format!("输入源已经是 {}", source.name));
        }

        SwitchOutcome::fail(format!(
            "指令已发出但显示器没有响应（{}={}）。常见原因：DDC/CI 在显示器 OSD 里被关闭，或该输入口不转发 DDC。",
            source.name, source.value
        ))
    }

    /// 直接按 MCCS 标准值切换，成功后把「名称→取值」固化进 profile。
    ///
    /// 这是托盘菜单走的路：用户点「USB-C」就直接写 0x1B，不需要先跑向导。
    /// 一旦切换真的生效（显示器离开本机），这个映射就被记为已验证，
    /// 下次走 `switch()` 的快速路径。
    pub fn switch_raw(&mut self, name: &str, value: u16, timeout: Duration) -> SwitchOutcome {
        let Some(display) = self.pick_display(None) else {
            // 屏幕已经不在本机 = 可能刚刚切过去了（跟随模式刚交出键鼠的场景），
            // 这不是能拿反证的失败
            return SwitchOutcome::fail_ambiguous("屏幕不在本机，需在对面那台发指令");
        };
        let key = self.identity(&display);
        // 对端代切时只报取值、不报名（它不知道我们这边叫什么），
        // 名字为空就用已登记的，再没有就退化成 0x0F 这种写法 ——
        // 绝不能让空字符串流进 profile，那会在面板上多出一个没有名字的口。
        let label = if name.trim().is_empty() {
            self.config
                .input_name_of(&key, value)
                .unwrap_or_else(|| format!("0x{value:02X}"))
        } else {
            name.to_string()
        };
        // 已经在目标口就直接报成功：反复写同值既无意义，还会因为
        // 「显示器不会离开本机」被误判成失败（实测踩过）
        if self.backend.get_vcp(&display, VCP_INPUT_SOURCE) == Some(value) {
            self.config.mark_verified(&key, value);
            return SwitchOutcome::ok(&label, format!("输入源已经在 {}", self.config.label_for(&label)));
        }
        if self.backend.set_vcp(&display, VCP_INPUT_SOURCE, value) {
            match self.wait_leave_or_value(&display, value, timeout) {
                LeaveWait::Gone => {
                    // 显示器切走后会重新枚举，registry id 会变，所以只能按型号名判断它有没有回来。
                    // 回来 = 那个口没有信号（空口，或对面主机处于 DPMS 休眠），不是切换成功。
                    if self.bounced_back(&display, Duration::from_secs(6)) {
                        // 白捡一条情报：这个口现在是空的
                        self.config.set_presence(&key, &[(value, Presence::Idle)]);
                        return SwitchOutcome::fail(format!(
                            "{}（0x{:02X}）那个口没有信号，显示器自己退回来了。\
                             检查对面那台是否开机、是否插在这个口上。",
                            self.config.label_for(&label),
                            value
                        ));
                    }
                    // 切过去还留着画面 = 那个口确实有主机在输出
                    return self.record_success(&key, &label, value);
                }
                LeaveWait::Confirmed => {
                    // 显示器没离开拓扑但 VCP 已经报新值：切成功了。有的型号
                    // 切走后仍挂着 EDID，「离开」这个信号永远等不到（实测）。
                    return self.record_success(&key, &label, value);
                }
                LeaveWait::Timeout => {}
            }
        }
        // 校验窗超时后再读一次实际值：有的显示器切换要 3~5 秒甚至更久，
        // 「没等到离开」不代表「没切过去」，值对了就算成功
        if self.backend.get_vcp(&display, VCP_INPUT_SOURCE) == Some(value) {
            self.config.set_presence(&key, &[(value, Presence::Occupied)]);
            if self.config.input_name_of(&key, value).is_some() {
                self.config.mark_verified(&key, value);
            } else {
                self.config.set_input(&key, &label, value, true);
            }
            return SwitchOutcome::ok(
                &label,
                format!("已切到 {}（{}={}，显示器切换稍慢但已生效）", self.config.label_for(&label), label, value),
            );
        }
        // 证据不足：等了 8 秒显示器也没离开本机拓扑（有的型号切走后仍挂着
        // EDID），VCP 读值又不是目标值（切换过程中读到旧值/错值，实测）。
        // 画面可能切成功了也可能没有——分不清，绝不能据此把键鼠抢收回来。
        SwitchOutcome::fail_ambiguous(format!(
            "没有生效（{}={}）。可能这个值不是该口的编码，或 DDC/CI 被关闭。",
            label, value
        ))
    }

    /// 等切换生效：两个正向信号谁先到都算。
    ///
    /// 1. 显示器离开本机拓扑（传统信号）；
    /// 2. VCP 0x60 读到目标值 —— 有的显示器切到别的输入源后**根本不离开
    ///    本机拓扑**（EDID 一直挂着，实测），正向切换等不到信号 1；
    ///    切回本机方向更是永远等不到。信号 2 是这类显示器唯一的确认渠道。
    ///
    /// 拓扑枚举 200ms 一次（零 I2C），VCP 读 1s 一次（切换窗口里少打扰
    /// 显示链路）。
    fn wait_leave_or_value(&self, display: &Display, value: u16, timeout: Duration) -> LeaveWait {
        let deadline = Instant::now() + timeout;
        let mut last_read = Instant::now() - Duration::from_secs(1);
        while Instant::now() < deadline {
            if !self.display_named(&display.name) {
                return LeaveWait::Gone;
            }
            if last_read.elapsed() >= Duration::from_secs(1) {
                last_read = Instant::now();
                if self.backend.get_vcp(display, VCP_INPUT_SOURCE) == Some(value) {
                    return LeaveWait::Confirmed;
                }
            }
            sleep(Duration::from_millis(200));
        }
        LeaveWait::Timeout
    }

    /// 把一次成功的切换落进配置：口上有主机、名字与验证标记、切换顺序。
    fn record_success(&mut self, key: &str, label: &str, value: u16) -> SwitchOutcome {
        self.config.set_presence(key, &[(value, Presence::Occupied)]);
        if self.config.input_name_of(key, value).is_some() {
            self.config.mark_verified(key, value);
        } else {
            self.config.set_input(key, label, value, true);
        }
        if !self
            .config
            .switch_order
            .iter()
            .any(|x| x.eq_ignore_ascii_case(label))
        {
            self.config.switch_order.push(label.to_string());
            let _ = self.config.save();
        }
        SwitchOutcome::ok(
            label,
            format!(
                "已切到 {}（{}={}）",
                self.config.label_for(label),
                label,
                value
            ),
        )
    }
}
