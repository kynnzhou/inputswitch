//! 切换编排层。
//!
//! 核心判断：DDC 写操作的返回值只说明"事务被接受了"，不能说明显示器真的换了口。
//! 唯一可靠的确认信号是**显示器从本机拓扑里消失**。

use std::cell::RefCell;
use std::collections::HashMap;
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::caps::Caps;
use crate::config::{Config, RuntimeState};
use crate::ddc::{new_backend, Backend};
use crate::edid::{self, Edid};
use crate::models::{
    normalize_model, Capability, Display, Presence, CANDIDATE_INPUT_VALUES, VCP_BRIGHTNESS,
    VCP_INPUT_SOURCE,
};

/// 一个口的探测结果是怎么来的。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeSource {
    /// 本机自己就挂在这个口上
    SelfHost,
    /// 对端机器通过局域网报的
    PeerReport,
    /// 物理切过去试出来的
    Physical,
    /// 从历史记录里带过来的
    Cached,
}

impl ProbeSource {
    pub fn label(self) -> &'static str {
        match self {
            ProbeSource::SelfHost => "本机",
            ProbeSource::PeerReport => "对端报告",
            ProbeSource::Physical => "实测",
            ProbeSource::Cached => "上次结果",
        }
    }
}

/// 一个口的最终判定。
#[derive(Clone, Debug)]
pub struct PortProbe {
    pub value: u16,
    pub name: String,
    pub presence: Presence,
    pub via: ProbeSource,
}

/// 一轮扫描的完整结果。
#[derive(Clone, Debug)]
pub struct ScanReport {
    pub ports: Vec<PortProbe>,
    /// 扫描前本机占用哪个值
    pub original: Option<u16>,
    /// 扫描结束时屏幕是否回到了本机
    pub back_home: bool,
    /// 中途放弃的原因
    pub aborted: Option<String>,
    /// 逐条过程记录
    pub steps: Vec<String>,
}

impl ScanReport {
    /// 有没有真的发过切换指令（决定要不要提示"这次动作可能改了你的屏幕"）
    pub fn touched_screen(&self) -> bool {
        self.ports.iter().any(|p| p.via == ProbeSource::Physical)
    }

    pub fn occupied(&self) -> Vec<u16> {
        self.ports
            .iter()
            .filter(|p| p.presence == Presence::Occupied)
            .map(|p| p.value)
            .collect()
    }
}

/// `wait_leave_or_value` 的结论
enum LeaveWait {
    /// 显示器离开了本机拓扑
    Gone,
    /// VCP 已读到目标值（显示器没离开拓扑也认）
    Confirmed,
    /// 超时，两个信号都没等到
    Timeout,
}

pub struct SwitchOutcome {
    pub ok: bool,
    pub message: String,
    pub input_name: String,
    /// 失败是否有确凿反证。「没有生效」这类**拿不到证据**的失败不算——
    /// 有的显示器切到别的输入源后仍挂在 macOS 拓扑里（EDID 一直活着）、
    /// VCP 读值也不更新，两个成功信号全部失效，但画面实际是切成功的。
    /// 调用方（跟随模式）对确凿失败要立刻收回键鼠，对模糊失败则不能抢收。
    pub definite: bool,
}

impl SwitchOutcome {
    fn ok(input: &str, message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: message.into(),
            input_name: input.to_string(),
            definite: true,
        }
    }

    fn fail(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
            input_name: String::new(),
            definite: true,
        }
    }

    /// 证据不足的失败：可能成功、可能失败，分不清。
    fn fail_ambiguous(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
            input_name: String::new(),
            definite: false,
        }
    }
}

pub struct Switcher {
    pub config: Config,
    pub state: RuntimeState,
    backend: Box<dyn Backend>,
    /// 按 display.uid 缓存 EDID：身份查询在菜单刷新里会被反复调用
    edid_cache: RefCell<HashMap<String, Option<Edid>>>,
    /// 能力表读取要发 5~8 次 I2C，同样必须缓存
    caps_cache: RefCell<HashMap<String, Option<Caps>>>,
    /// DDC 静默期截止时刻。显示器刚离开/回到本机拓扑的那几秒，
    /// macOS 正在重新协商显示链路（Apple Silicon 上这个窗口极脆，
    /// 大流量 I2C 探测会把 WindowServer 顶进看门狗超时），期间
    /// 主动停掉一切 DDC 读取。
    ddc_quiet_until: RefCell<Option<Instant>>,
}

impl Switcher {
    pub fn new() -> Self {
        let backend = new_backend();
        let mut switcher = Self {
            config: Config::load(),
            state: RuntimeState::load(),
            backend,
            edid_cache: RefCell::new(HashMap::new()),
            caps_cache: RefCell::new(HashMap::new()),
            ddc_quiet_until: RefCell::new(None),
        };
        switcher.refresh();
        switcher
    }

    /// 重新识别所有在线显示器：读 EDID 定身份、读能力表补输入源。
    ///
    /// 这两件事都只在"显示器还有信号"的时候能做，所以必须早点做。
    pub fn refresh(&mut self) {
        for display in self.backend.list_displays() {
            self.sync_profile(&display);
        }
    }

    /// 丢掉缓存重新探测一遍，返回一句给人看的结论。
    ///
    /// 面板上的「重新检测显示器」走这里。必须**先清缓存**：能力表读失败会被
    /// 记成 `None` 缓存下来，不清掉的话再点一百次也还是那份空列表 ——
    /// 用户看到的就是"点了没任何效果"。
    pub fn rescan(&mut self) -> String {
        self.edid_cache.borrow_mut().clear();
        self.caps_cache.borrow_mut().clear();

        let displays = self.backend.list_displays();
        if displays.is_empty() {
            // 这个分支很常见，而且不是故障：显示器此刻正显示另一台机器，
            // 本机的显卡那边就是"没接显示器"，DDC 通道自然也不存在。
            return "本机现在没有接通的显示器（画面正显示另一台机器）".to_string();
        }

        let mut notes: Vec<String> = Vec::new();
        let mut any_inputs = false;
        for display in &displays {
            let key = self.sync_profile(display);
            let count = self
                .config
                .profile_for(&key)
                .map(|p| p.inputs.len())
                .unwrap_or(0);
            let current = self.backend.get_vcp(display, VCP_INPUT_SOURCE);
            let caps = self.capabilities(display).is_some();

            let mut note = format!("{}：{} 个输入口", display.name, count);
            match current {
                Some(value) => note.push_str(&format!("，当前 0x{value:02X}")),
                None => note.push_str("，当前输入源读不到"),
            }
            if !caps {
                note.push_str("（显示器没回能力表）");
            }
            if count > 0 {
                any_inputs = true;
            }
            notes.push(note);
        }

        let mut text = notes.join("；");
        if !any_inputs {
            text.push('。');
            text.push_str(&crate::ddc::diagnose());
        }
        text
    }

    /// 把一台显示器同步进配置，返回它的身份键。
    pub fn sync_profile(&mut self, display: &Display) -> String {
        let key = self.identity(display);
        // 老版本用型号名当键，先合并过来，别让用户实测的结果丢掉
        self.config.migrate_legacy(&display.name, &key);
        if let Some(caps) = self.capabilities(display) {
            if caps.has_inputs() {
                self.config.seed_inputs(&key, &display.name, &caps.inputs);
            }
        }
        let capability = self.capability(display);
        self.config.set_capability(&key, &display.name, capability);
        key
    }

    /// 显示器身份键：优先 EDID，读不到才退回型号名。
    pub fn identity(&self, display: &Display) -> String {
        self.edid(display)
            .map(|e| e.identity())
            .unwrap_or_else(|| normalize_model(&display.name))
    }

    pub fn edid(&self, display: &Display) -> Option<Edid> {
        if let Some(cached) = self.edid_cache.borrow().get(&display.uid) {
            return cached.clone();
        }
        let parsed = self.backend.read_edid(display).and_then(|b| edid::parse(&b));
        self.edid_cache
            .borrow_mut()
            .insert(display.uid.clone(), parsed.clone());
        parsed
    }

    pub fn capabilities(&self, display: &Display) -> Option<Caps> {
        if let Some(cached) = self.caps_cache.borrow().get(&display.uid) {
            return cached.clone();
        }
        let caps = self.backend.capabilities(display);
        self.caps_cache
            .borrow_mut()
            .insert(display.uid.clone(), caps.clone());
        caps
    }

    /// 这台显示器当前支持的输入源取值。能力表没有时返回空。
    pub fn input_values(&self, display: &Display) -> Vec<u16> {
        self.capabilities(display)
            .map(|c| c.inputs)
            .unwrap_or_default()
    }

    pub fn backend(&self) -> &dyn Backend {
        self.backend.as_ref()
    }

    /// 显示器拓扑刚变动（切走/切回/插拔），静默一段时间不碰 DDC。
    pub fn suppress_ddc(&self, window: Duration) {
        let until = Instant::now() + window;
        let mut slot = self.ddc_quiet_until.borrow_mut();
        // 取更晚的那个：连着两次拓扑事件时不要把静默期缩短
        *slot = Some(match *slot {
            Some(existing) if existing > until => existing,
            _ => until,
        });
    }

    /// 静默期内返回 true，调用方应当跳过所有 DDC 读取。
    pub fn ddc_quiet(&self) -> bool {
        matches!(*self.ddc_quiet_until.borrow(), Some(until) if Instant::now() < until)
    }

    pub fn displays(&self) -> Vec<Display> {
        self.backend.list_displays()
    }

    pub fn pick_display(&self, model: Option<&str>) -> Option<Display> {
        let all = self.displays();
        let filtered: Vec<Display> = match model {
            Some(m) => {
                let key = normalize_model(m);
                all.into_iter()
                    .filter(|d| normalize_model(&d.name) == key)
                    .collect()
            }
            None => all,
        };
        filtered
            .iter()
            .find(|d| !d.is_main)
            .or_else(|| filtered.first())
            .cloned()
    }

    pub fn capability(&self, display: &Display) -> Capability {
        if self.backend.get_vcp(display, VCP_BRIGHTNESS).is_some() {
            return Capability::ReadWrite;
        }
        if self.backend.transport_ok(display) {
            Capability::WriteOnly
        } else {
            Capability::Unsupported
        }
    }

    pub fn current_input_name(&self, display: &Display) -> Option<String> {
        let value = self.backend.get_vcp(display, VCP_INPUT_SOURCE)?;
        let key = self.identity(display);
        self.config
            .input_name_of(&key, value)
            .or_else(|| Some(format!("输入源 0x{:02X}", value)))
    }

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

    /// 屏幕是否还挂在本机。为 false 通常意味着已经切到对面那台了。
    pub fn display_here(&self) -> bool {
        !self.backend.list_displays().is_empty()
    }

    /// 这台显示器还在不在本机拓扑里。按型号名判断，因为切走再回来时
    /// registry id 会重新分配，用 uid 比对会永远认为"已经不在了"。
    fn display_named(&self, name: &str) -> bool {
        self.backend.list_displays().iter().any(|d| d.name == name)
    }

    fn wait_until_gone(&self, display: &Display, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if !self.display_named(&display.name) {
                return true;
            }
            sleep(Duration::from_millis(150));
        }
        false
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

    /// 切走之后再盯一会儿：显示器自己回来了，说明那个口根本没有信号。
    fn bounced_back(&self, display: &Display, window: Duration) -> bool {
        let deadline = Instant::now() + window;
        while Instant::now() < deadline {
            if self.display_named(&display.name) {
                return true;
            }
            sleep(Duration::from_millis(200));
        }
        false
    }

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

    /// 找出另一个输入口的 0x60 取值。
    ///
    /// 先把当前值读出来（这就是 own 的取值），再逐个写候选值，
    /// 哪个让显示器从拓扑里消失，哪个就是 other 的取值。
    ///
    /// 副作用提醒：一旦命中，本机就失去 DDC 通道了，恢复要靠对面主机或 OSD。
    pub fn discover(
        &mut self,
        own_input: &str,
        other_input: &str,
        model: Option<&str>,
        max_tries: usize,
        mut on_try: impl FnMut(u8),
    ) -> Result<u16, String> {
        let Some(display) = self.pick_display(model) else {
            return Err("没有找到可操作的显示器".into());
        };

        let own_value = self.backend.get_vcp(&display, VCP_INPUT_SOURCE);
        let key = self.identity(&display);
        if let Some(v) = own_value {
            self.config.set_input(&key, own_input, v, true);
        }

        let candidates: Vec<u16> = CANDIDATE_INPUT_VALUES
            .iter()
            .map(|v| *v as u16)
            .filter(|v| Some(*v) != own_value)
            .take(max_tries)
            .collect();

        for value in candidates {
            on_try(value as u8);
            if !self.backend.set_vcp(&display, VCP_INPUT_SOURCE, value) {
                continue;
            }
            if self.wait_until_gone(&display, Duration::from_secs(2)) {
                self.config
                    .set_input(&key, other_input, value, true);
                return Ok(value);
            }
        }
        Err(format!(
            "试完 {} 个候选值都没有命中",
            CANDIDATE_INPUT_VALUES.len().min(max_tries)
        ))
    }
}

impl Default for Switcher {
    fn default() -> Self {
        Self::new()
    }
}
