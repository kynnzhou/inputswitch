//! 公开句柄：`Kvm` 及其全部方法。
//!
//! 从 `mod.rs` 拆出来。这个类型是外部（`tray.rs`、面板动作层）**唯一**的
//! 接触面 —— 所有跨线程共享状态都锁在它的私有字段里，方法即 API。字段
//! `pub(super)` 而不是 `pub`：构造权归 `start()`（在 `mod.rs`），外部
//! 只能读不能建。
//!
//! 方法分三组，看名字就能分：
//!
//! - **查询**：`state()` / `active_node()` / `idle_ms()` / `peer_alive()`
//!   这类，只读原子量和心跳表，任何线程都能调。
//! - **动作**：`follow()` / `hand_over_to()` / `retarget_to()` /
//!   `give_back()` / `take_back_from_peer()`。全都遵循两条不变式
//!   （I1 `Sending ⇔ grabbing && active.is_some()`、
//!   I2 `state != Idle ⇒ active.is_some()`），**改状态前先写 active、
//!   拆场先改 state 再清 active** —— 每个方法上面的注释都写了这一条
//!   对应的故障，改顺序之前先读。
//! - **杂务**：`replay_paste_to_peer()` / `send_key_to_peer()` / `stop()`。

use std::sync::atomic::{AtomicU8, AtomicU64, AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::event::{Event, Side};
use super::link::link_alive_at;
use super::linker::Linker;
use super::plat;
use super::rx::RxTable;
use super::session::{Active, Out};
use super::state::{Mode, Role, State};
use super::Link;

pub struct Kvm {
    pub(super) state: Arc<AtomicU8>,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) mode: Mode,
    pub(super) role: Role,
    /// Remote 角色下是 None：不抓取，只等对端发事件来
    pub(super) capture: Option<Arc<plat::Capture>>,
    pub(super) out: Arc<Out>,
    /// 每个对端一份的心跳/存活记录。**不能是单槽**：静音看守只该看
    /// 当前会话那台，别台的心跳替它续命的话，它崩了本机会一直卡在
    /// 转发里（键鼠全吞）。
    pub(super) peer_rx: RxTable,
    /// 链路表（与 `out.links` 同一份）
    pub(super) links: Arc<Vec<Link>>,
    /// 当前会话目标
    pub(super) active: Active,
    /// **本次会话的起点**（多跳的回程目标）。
    ///
    /// 只有"共用屏那台 → 并排那台"这种一跳才用得上：对面交还时如果
    /// 直接收回本机，画面还在起点那台上，用户看不见光标。有了它，
    /// 交还 = 改指向回起点，画面和键鼠重新一致。
    pub(super) root: Active,
    /// 刚改指向到的链路与时刻。旧目标手上有在途位移，会回 Takeback 解围，
    /// 这一毫秒级的残包不能当真（否则刚改过去就被打回 Idle）。
    pub(super) retarget_guard: Arc<Mutex<Option<(usize, u64)>>>,
    /// 上次改指向的时刻（热键重复、推边连击的节流基准）
    pub(super) hop_ms: Arc<AtomicU64>,
    pub(super) log: Arc<dyn Fn(String) + Send + Sync>,
}

impl Kvm {
    pub fn state(&self) -> State {
        State::from_u8(self.state.load(Ordering::Relaxed))
    }

    pub fn role(&self) -> Role {
        self.role
    }

    /// 全局模式。多链路时返回主链路的模式（P6 起才按链路分）。
    pub fn mode(&self) -> Mode {
        self.links.first().map(|l| l.mode).unwrap_or(self.mode)
    }

    /// 有没有"跟随画面"链路。切输入源之后要不要顺手把键鼠交过去，
    /// 问的是这个而不是 `mode()`：一机两链时（共享屏 + 跨屏并排），
    /// 主链路是 Edge 也不该让切换屏时的键鼠跟随失效。
    pub fn has_follow_link(&self) -> bool {
        self.links.iter().any(|l| l.mode == Mode::Follow)
    }

    pub fn links(&self) -> &[Link] {
        &self.links
    }

    /// 当前握着键鼠的那台（Sending/Receiving 时有值）
    pub fn active_node(&self) -> Option<&str> {
        self.active
            .get()
            .and_then(|i| self.links.get(i))
            .map(|l| l.node.as_str())
    }

    /// 当前会话走的是不是"有门"的链路（Edge）。
    ///
    /// 托盘那条"VCP 已经是本机口 → 收回键鼠"的自证兜底只对跟随画面会话
    /// 成立：Edge 链路根本不切输入源，VCP 永远等于本机，按它收回等于
    /// 每 5 秒把键鼠抢回来一次。
    pub fn active_is_edge(&self) -> bool {
        self.active
            .get()
            .and_then(|i| self.links.get(i))
            .map(|l| l.mode == Mode::Edge)
            .unwrap_or(false)
    }

    fn link_alive(&self, index: usize) -> bool {
        link_alive_at(&self.peer_rx, &self.links, index)
    }

    /// 把这三个 Arc 借成一份可执行"改指向"的视图。
    fn linker(&self) -> Linker<'_> {
        Linker {
            state: &self.state,
            out: &self.out,
            links: &self.links,
            active: &self.active,
            root: &self.root,
            capture: self.capture.as_ref(),
            guard: &self.retarget_guard,
            hop_ms: &self.hop_ms,
            peer_rx: &self.peer_rx,
            log: &*self.log,
        }
    }

    /// 本次会话是从哪台开始的（多跳的回程目标）。没有会话 = None。
    pub fn root_node(&self) -> Option<&str> {
        self.root.get().and_then(|i| self.links.get(i)).map(|l| l.node.as_str())
    }

    /// 本次会话的起点是不是就是当前这台（单跳/单链路拓扑恒为 true）。
    pub fn at_root(&self) -> bool {
        match (self.root.get(), self.active.get()) {
            (Some(r), Some(a)) => r == a,
            _ => true,
        }
    }

    /// **会话中把键鼠改指向另一台**（画面不动）。
    ///
    /// 与 `hand_over_to` 的分工：那个负责"从空闲开始建一场会话"，这个负责
    /// "会话已经开着，只换收件人"。多跳的一切触发面（热键、推边、面板
    /// 按钮）最终都落到这里。
    pub fn retarget_to(&self, node: &str) -> Result<(), String> {
        let Some(target) = self.links.iter().position(|l| l.node == node) else {
            return Err(format!("没有叫 {node} 的链路"));
        };
        self.linker().retarget(target, 0xFFFF, "手动改指向")
    }

    /// 把键鼠转给「下一条有门的链路」：热键和托盘菜单用。
    ///
    /// 多台并排时"下一条"没有唯一答案，这里按链路表顺序取第一条既不是
    /// 当前目标、又活着的 Edge 链路 —— 两台并排（最常见的拓扑）下它
    /// 就是另外那台。
    pub fn retarget_next_edge(&self) -> Result<(), String> {
        let cur = self.active.get();
        let next = self
            .links
            .iter()
            .enumerate()
            .filter(|(i, l)| l.mode == Mode::Edge && Some(*i) != cur)
            .find(|(i, _)| self.link_alive(*i))
            .map(|(i, _)| i);
        match next {
            Some(i) => self.linker().retarget(i, 0xFFFF, "托盘/热键改指向"),
            None => Err("没有别的有门链路可转（先把它摆到摆位图的某个方向）".to_string()),
        }
    }

    /// 距离上次收到**当前会话对端**消息多久了（毫秒）。没有会话 = u64::MAX。
    pub fn idle_ms(&self) -> u64 {
        match self.active.get().and_then(|i| self.links.get(i)) {
            Some(l) => self.peer_rx.idle_ms(l.ip),
            None => u64::MAX,
        }
    }

    /// 有没有任何一条链路在对端活着。面板用它回答"对面在不在"——
    /// 空闲时也是 true，不能只看当前会话目标。
    pub fn peer_alive(&self) -> bool {
        (0..self.links.len()).any(|i| self.link_alive(i))
    }

    /// 跟随画面模式的目标链路：优先第一条 Follow 链路，没有就用第一条。
    ///
    /// 切屏跟键鼠跟的是"画面切到了哪台"，而 v3 的链路表里 Follow 链路
    /// 正是那些"和本机共用一块屏"的机器。多条 Follow（一屏两机以上的
    /// 少见拓扑）时取第一条，更复杂的选择留给 P6 的按口分流。
    fn follow_target(&self) -> Option<usize> {
        self.links
            .iter()
            .position(|l| l.mode == Mode::Follow)
            .or(if self.links.is_empty() { None } else { Some(0) })
    }

    /// 本机手上有"本次复制的文件清单" → 转发期间按 Ctrl+V 会被拦下来。
    /// 只在托盘侧真的读到文件剪贴板时置 true，其余时间必须是 false，
    /// 否则会误吞用户正常的 Ctrl+V。
    pub fn set_paste_ready(&self, on: bool) {
        if let Some(c) = self.capture.as_ref() {
            c.set_paste_ready(on);
        }
    }

    /// 补发一次 Ctrl+V 给对端：用户按的那一次已经被我们吞掉了，
    /// 文件落盘之后由这里替他把粘贴动作做完。
    ///
    /// 固定补 Ctrl down → V down → V up → Ctrl up：用户按完 28 秒的传输，
    /// 手早就松开了；重复发一次 Ctrl down 在 Windows 上只是重复按键，无害。
    pub fn replay_paste_to_peer(&self) {
        self.out.send(&Event::Key {
            code: 0x3B,
            down: true,
        });
        self.out.send(&Event::Key {
            code: 0x09,
            down: true,
        });
        self.out.send(&Event::Key {
            code: 0x09,
            down: false,
        });
        self.out.send(&Event::Key {
            code: 0x3B,
            down: false,
        });
    }

    /// 把一次按键转发给对端。传输失败/取消时用它把用户原本那一下补回去，
    /// 不让人感觉"按了没反应"。
    pub fn send_key_to_peer(&self, code: u16) {
        self.out.send(&Event::Key { code, down: true });
        self.out.send(&Event::Key {
            code,
            down: false,
        });
    }

    /// **画面切到对端了 → 把键鼠交过去。**
    ///
    /// 这是唯一进入转发状态的入口。切屏动作成功之后调用它。
    ///
    /// 开转之前先确认对端活着：否则画面已经切过去、键鼠也被吞掉，
    /// 用户面前是一块亮着 B 的屏，却谁也控制不了 —— 比不做还糟。
    ///
    /// 返回 `Ok(true)` = 真的开始转发了；`Ok(false)` = 已经有会话在进行，
    /// 这次什么都没做。以前空转也返回 `Ok(())`，调用方（`do_switch`、
    /// 面板「交给对端」）一律当成交接成功 —— 那是"键鼠已跟过去"这句
    /// 谎报的源头。调用方拿到的 `false` 必须原样说给用户听。
    pub fn follow(&self) -> Result<bool, String> {
        // P5：不再按 role 挡。键鼠插在哪台是**物理事实**，不是配置项 ——
        // 用户可以随时把键盘从这台拔到那台，程序不该要求他先回来改配置。
        // 能不能往外发只取决于"抓没抓到键鼠"（下面的 capture），而那件事
        // 在 start() 时已经按权限判过了。真要没抓取，那句"抓取模块没有
        // 启动"比"你的键鼠不在本机"准确得多。
        if self.state() != State::Idle {
            return Ok(false);
        }
        let Some(target) = self.follow_target() else {
            return Err("没有配置任何对端链路".to_string());
        };
        if !self.link_alive(target) {
            return Err(
                "对端没有回应（对面没开程序、不在同一网段，或防火墙拦了）。\
                 键鼠没有交出去，你还能操作本机"
                    .to_string(),
            );
        }
        let Some(capture) = self.capture.as_ref() else {
            return Err("抓取模块没有启动".to_string());
        };
        // I2：先把会话目标写上，再改状态 —— 反过来会有一个窗口，
        // 状态已经是 Sending 而 active 还是空，转发流找不到收件人被丢掉。
        self.active.set(Some(target));
        // 起点就是这里：之后从共用屏那台再转去并排那台时，回程要回这儿
        self.root.set(Some(target));
        capture.set_grabbing(true);
        self.state.store(State::Sending as u8, Ordering::SeqCst);
        // 跟随画面模式下没有"门"的概念，出门位置未知（0xFFFF），
        // 对端居中落点即可；屏幕尺寸照带，位移缩放照样生效
        let (sw, sh) = capture.screen();
        self.out.to_link(
            target,
            &Event::Enter {
                width: sw.min(65535) as u16,
                height: sh.min(65535) as u16,
                along: 0xFFFF,
                flags: 0,
                // 跟随画面模式没有门，门口方向留给对端自己决定
                side: Side::CODE_UNKNOWN,
            },
        );
        (self.log)(format!(
            "键鼠已交给 {}（画面在那边）",
            self.links[target].node
        ));
        Ok(true)
    }

    /// **把键鼠交给指定的那台。**
    ///
    /// 和 `follow()` 的差别只在"目标是哪台"：`follow` 是切屏成功之后跟画面走
    /// （靠 `follow_target()` 猜一条 Follow 链路），这个是人手指名道姓。
    /// 一机多台时不能再猜 —— 猜错的表现是"点了 A 结果键鼠跑到 B 去了"。
    ///
    /// v0.6.60 起两条路合成一条：**空闲时**建一场新会话；**已经在转发时**
    /// 换成改指向（以前这里一律拒绝，于是"面板上看得到却点不动"，用户只能
    /// 先收回再交出去 —— 而收回时画面还在对面，看不见面板）。
    pub fn hand_over_to(&self, node: &str) -> Result<(), String> {
        let Some(target) = self.links.iter().position(|l| l.node == node) else {
            return Err(format!("没有叫 {node} 的链路"));
        };
        match self.state() {
            // 被控时"交给别人"等于把别人的控制权转走，本机没这个权力：
            // 得先请对端交还（Ctrl+Alt+Esc），再交出去
            State::Receiving => {
                Err("键鼠现在在对面手里，先按 Ctrl+Alt+Esc 收回来再交给别的机器".to_string())
            }
            State::Sending => self.linker().retarget(target, 0xFFFF, "手动改指向"),
            State::Idle => {
                if !self.link_alive(target) {
                    return Err(format!(
                        "{node} 没有回应（对面没开程序、不在同一网段，或防火墙拦了）。\
                         键鼠没有交出去，你还能操作本机"
                    ));
                }
                let Some(capture) = self.capture.as_ref() else {
                    return Err("抓取模块没有启动".to_string());
                };
                // I2：先写会话目标再改状态，否则转发流有一瞬间找不到收件人
                self.active.set(Some(target));
                self.root.set(Some(target));
                capture.set_grabbing(true);
                self.state.store(State::Sending as u8, Ordering::SeqCst);
                let (sw, sh) = capture.screen();
                let link = &self.links[target];
                // 有门的链路把门的方向告诉对端（拓扑决定了它一定在相反那侧）；
                // Follow 链路没有门，交给对端沿用自己配置的方向。
                let side = link.side.map(|s| s.code()).unwrap_or(Side::CODE_UNKNOWN);
                self.out.to_link(
                    target,
                    &Event::Enter {
                        width: sw.min(65535) as u16,
                        height: sh.min(65535) as u16,
                        along: 0xFFFF,
                        flags: 0,
                        side,
                    },
                );
                (self.log)(format!("键鼠已交给 {}", link.node));
                Ok(())
            }
        }
    }

    /// 第 `index` 条链路对端活不活。面板每行都要显示，所以公开出去。
    pub fn alive_at(&self, index: usize) -> bool {
        self.link_alive(index)
    }

    /// 按节点名查对端活不活。`None` = 链路表里没有这台。
    ///
    /// 切画面之前要问这一句：**对端离线时切画面是条单行道** —— 画面走了、
    /// 键鼠还在本机，DDC 又读不到（实测切走后 9/9 失败），用户既看不见
    /// 本机也没法操作对端，只能去按显示器自己的按钮。v0.6.62 用户实测
    /// "Mac 切 MC5K9SJ 键鼠用不了"就是这么来的（那台机器当时整机不在网）。
    pub fn link_alive_by_node(&self, node: &str) -> Option<bool> {
        self.links
            .iter()
            .position(|l| l.node == node)
            .map(|i| self.link_alive(i))
    }

    /// 现在握着键鼠的是第几条链路。没有会话就是 None。
    pub fn active_index(&self) -> Option<usize> {
        self.active.get()
    }

    /// **画面切回本机了 / 要拿回键鼠 → 停止转发。**
    ///
    /// 幂等：本来就在本机时什么都不做。
    ///
    /// 注意：**只处理"键鼠在我们手里"这一种**。被控时的收回走
    /// `take_back_from_peer()` —— 两条路都是"把控制权拿回本机"，但
    /// 前者是本地停手、后者是求人交还，混在一起会让三条"画面切回来了"
    /// 的自动路径在 Receiving 时误发 Takeback。
    ///
    /// 返回 false = 本来就没什么可收的（不是 Sending）。调用方要据此决定
    /// 能不能说"键鼠已收回本机" —— 以前它是静默空转，界面照样报收回成功。
    pub fn give_back(&self) -> bool {
        self.release(None)
    }

    /// 本机有没有抓取能力。没有就只能被控（`follow()` 会直接报
    /// 「抓取模块没有启动」），面板据此不要说"键鼠跟过去"。
    pub fn can_capture(&self) -> bool {
        self.capture.is_some()
    }

    /// 被控时（对端正控制本机）把控制权要回来。
    ///
    /// 返回 false = 现在不是被控状态（或没有会话），什么都没做。
    ///
    /// 这条路的用户是**键盘用户**：面板上点「收回键鼠」是他们够不到
    /// 鼠标、也没记住热键时唯一的出路。原来 `give_back()` 在 Receiving
    /// 直接 return，点了没反应。
    pub fn take_back_from_peer(&self, why: &str) -> bool {
        if self.state() != State::Receiving {
            return false;
        }
        // **先快照目标再动状态** —— active 一清，Takeback 就不知道发给
        // 谁了，对端会一直以为它还控制着本机。
        let Some(target) = self.active.get() else {
            return false;
        };
        self.state.store(State::Idle as u8, Ordering::SeqCst);
        if let Some(capture) = self.capture.as_ref() {
            capture.set_claim_armed(false);
        }
        // 单发 UDP 会丢：丢一包对端就卡在 Sending，键鼠全被它吞掉。
        // 接收端有状态闸门，重复的 Takeback 是幂等的。
        self.out.to_link(target, &Event::Takeback { along: 0xFFFF });
        self.out.to_link(target, &Event::Takeback { along: 0xFFFF });
        self.active.set(None);
        // 会话结束了，起点也一并忘了：下一场重新定
        self.root.set(None);
        (self.log)(format!("{why}，已请对端交还键鼠"));
        true
    }

    fn release(&self, reason: Option<&str>) -> bool {
        let now = self.state.load(Ordering::Relaxed);
        if now != State::Sending as u8 {
            return false;
        }
        // CAS 抢一下，避免两个线程同时交还、重复补 key-up
        if self
            .state
            .compare_exchange(now, State::Idle as u8, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return false;
        }
        // **先快照再发**：清空 active 之后补的 up 与 Leave 就找不到收件人
        // 了，对端会一直以为 Cmd/Ctrl 还压着（I4）。
        let target = self.active.get();
        if let Some(capture) = self.capture.as_ref() {
            capture.set_grabbing(false);
        }
        // 先把可能还按着的修饰键和按钮补 up，再发 Leave。
        // 不补的话对面会一直以为 Cmd/Ctrl 还压着，之后所有点击都变成组合键。
        if let Some(t) = target {
            for code in plat::MODIFIER_KEYS {
                self.out.to_link(
                    t,
                    &Event::Key {
                        code,
                        down: false,
                    },
                );
            }
            // 按钮不盲发 up（理由见 `Linker::retarget`）：对端会把孤立的
            // button-up 当成一次点击
            self.out.to_link(t, &Event::Leave);
        }
        // I2：拆场先改 state 再 clear active（上面 CAS 已改过 state）
        self.active.set(None);
        self.root.set(None);

        match reason {
            Some(why) => (self.log)(format!("键鼠已收回：{why}")),
            None => (self.log)("键鼠已收回本机".to_string()),
        }
        true
    }

    pub fn stop(&self) {
        let _ = self.release(None);
        self.stop.store(true, Ordering::SeqCst);
        if let Some(capture) = self.capture.as_ref() {
            capture.stop();
        }
    }
}
