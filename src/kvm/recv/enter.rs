//! 接管（Enter）：抢占矩阵、会话重建、门口方向按拓扑校正、光标落点。
//!
//! 这一路最繁：先按抢占矩阵决定接不接（方案 §3 P4），再建场
//! （I2：先写会话目标再改状态），最后按对端报的出门侧校正门口
//! —— 拓扑定理，不该让两台各自配。

use std::sync::atomic::Ordering;

use super::Ctx;
use crate::kvm::event::{Event, Side};
use crate::kvm::link::session_edge;
use crate::kvm::plat;
use crate::kvm::session::Session;
use crate::kvm::state::{Mode, State};
use crate::kvm::tuning::ENTER_FLAG_LANDING_ONLY;
use crate::kvm::util::{entry_spot, now_ms, release_local};

impl Ctx {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn on_enter(
        &mut self,
        seq: u16,
        from_link: usize,
        width: u16,
        height: u16,
        along: u16,
        peer_side: u8,
        flags: u16,
    ) {
        let cur = self.state.load(Ordering::Relaxed);
        let act = self.active_rx.get();

        // ---- 抢占矩阵（方案 §3 P4）----
        // | 本机         | Enter 来自 active | Enter 来自其它链路 |
        // | Idle         | 进 Receiving      | 同左              |
        // | Sending→A    | 让路：转 Receiving | **忽略**          |
        // | Receiving←A  | 幂等：只更新门口   | 先拆 A，再进 Receiving(B) |
        if cur == State::Sending as u8 && act != Some(from_link) {
            // 我们正在给 A 转发，B 也发来 Enter。接受的话 A 还在
            // 收我们的键鼠、B 同时开始注入，两台一起动 —— 必须拒。
            if now_ms().saturating_sub(self.last_ignore_ms) > 2000 {
                self.last_ignore_ms = now_ms();
                let held = act
                    .and_then(|i| self.links_rx.get(i))
                    .map(|l| l.node.as_str())
                    .unwrap_or("别的机器");
                (self.log)(format!(
                    "{} 也想过来了，但键鼠已交给 {held}，这次忽略",
                    self.links_rx[from_link].node
                ));
            }
            return;
        }
        if cur == State::Sending as u8 {
            // 同一个对端在我们转发时又发 Enter：它的用户也在
            // 推门抢。Edge 模式让路（换成它控制我们），Follow
            // 模式两边同时越界抢，让先动的那个继续。
            if self.links_rx[from_link].mode != Mode::Edge {
                (self.log)("对端也想过来了，先让它等".to_string());
                return;
            }
            // 让路：停止抓取，把还按着的键补 up，然后进入
            // **被控**状态接它的事件 —— 必须是 Receiving 而不是
            // Idle：设成 Idle 的话，对端接下来转发的事件没人
            // 注入，两边就都卡死了（v0.6.5 踩过）。
            // 不比较先后 —— 双方同时推门时两边都让路，最坏
            // 结果是这一轮谁也没拿到，再推一次即可。
            //
            // **顺序不能反**：先改状态再放抓取。反过来的话，
            // 心跳守卫那一拍会读到"状态还是 Sending、抓取已
            // 放开"，判成错位并反手再抓一次 —— 用户感知是
            // 交还之后光标还卡半秒（v0.6.31 实测）。
            self.state.store(State::Receiving as u8, Ordering::SeqCst);
            // 被控开始：武装"物理抢回"并记时刻（保护窗起点）
            self.recv_rx.store(now_ms(), Ordering::Relaxed);
            if let Some(cap) = self.capture_rx.as_ref() {
                cap.set_grabbing(false);
                cap.set_claim_armed(true);
            }
            release_local(&self.inject, &mut self.sess.pressed);
            self.out.to_link(from_link, &Event::Leave);
        }
        if cur == State::Receiving as u8 && act != Some(from_link) {
            // 正被 A 控制着，B 也发 Enter（B 的用户推门抢）。
            // 先把 A 拆干净：它的键鼠必须收回，否则我们一边
            // 注入 B 的位移、一边还在收 A 的 —— 两台抢一只光标。
            release_local(&self.inject, &mut self.sess.pressed);
            if let Some(old) = act {
                self.out.to_link(old, &Event::Takeback { along: 0xFFFF });
                self.out.to_link(old, &Event::Takeback { along: 0xFFFF });
                (self.log)(format!(
                    "{} 抢走了控制权，已通知 {} 收回",
                    self.links_rx[from_link].node,
                    self.links_rx
                        .get(old)
                        .map(|l| l.node.as_str())
                        .unwrap_or("对端")
                ));
            }
        }

        // 幂等判断要在改状态之前算：重复的 Enter 不该再挪光标
        let entering = cur != State::Receiving as u8 || act != Some(from_link);
        // 建场顺序（I2）：先写会话目标，再改状态
        self.active_rx.set(Some(from_link));
        self.state.store(State::Receiving as u8, Ordering::SeqCst);
        // 被控开始（或换了对端）：武装"物理抢回"并记时刻。
        // 只在真正进入时才刷新 —— 重复 Enter 不该把保护窗
        // 一直往后推，否则用户按 Shift 永远抢不回来。
        if entering {
            self.recv_rx.store(now_ms(), Ordering::Relaxed);
            if let Some(cap) = self.capture_rx.as_ref() {
                cap.set_claim_armed(true);
            }
        }
        // 换对端 = 整体重建，把上一场的时钟、尺寸、按键栈丢掉
        if self.sess.peer != self.links_rx[from_link].ip {
            self.sess = Session::new(
                self.links_rx[from_link].ip,
                self.links_rx[from_link].node.clone(),
                self.links_rx[from_link].side.unwrap_or_default(),
                self.links_rx[from_link].mode == Mode::Edge,
            );
        }
        self.sess.last_seq = seq;
        if width > 0 && height > 0 {
            self.sess.sender_size = (width as f64, height as f64);
            // 把对端报的屏幕尺寸和算出来的位移倍率写进日志：
            // 「回到本机鼠标变慢/变快」只能靠这两个数定位 ——
            // 是分辨率比例算错了，还是两系统加速曲线不同。
            let (lw, lh) = self.inject.screen();
            (self.log)(format!(
                "对端屏幕 {}x{}，本机 {}x{} ⇒ 位移倍率 x{:.3}（接收侧微调 x{:.2}）",
                width,
                height,
                lw as u32,
                lh as u32,
                (lw / width as f64) * self.recv_scale,
                self.recv_scale
            ));
        }
        // 对端带了自己的出门侧：本机门口 = 它的对面。
        // 这一条同时兜住「两台配置里门口方向各配一半」的情况
        //（v0.6.42 实测：Mac→Windows 每 0.5~2 秒就被交还，
        // 就是因为 Windows 那边把门口当成了自己出门的那侧）。
        // 拓扑定理在这里也升级成四向版：它从右边出门 ⇒ 我在
        // 它左边；它从上面出门 ⇒ 我在它下面。
        // `side` 现在有两种含义，靠 flags 分清：
        //   ① 真·出门侧 —— 这条链路有门；
        //   ② 只用来定落点 —— 回程（从邻居回到共用屏那台）。
        // ②必须排除在"有门证据"之外，否则那台共用屏机器
        // 会开始判推门交还，光标在它上面一顶边就把键鼠
        // 弹回 Mac（画面还在它那，用户等于摸黑）。
        let landing = flags & ENTER_FLAG_LANDING_ONLY != 0;
        if let Some(ps) = Side::from_code(peer_side) {
            // **有没有门，发送方比本地配置更有发言权。**
            //
            // 只有 Edge 链路发得出真实的出门侧（Follow 恒发
            // CODE_UNKNOWN，见 `follow()`）。所以"带了一个
            // 有效 side"这件事本身就是"那条链路有门"的证据。
            //
            // 反过来按本地配置判会出一个死局：用户往往只在
            // **发送方那台**摆过位（"把 0DFF1JU 拖到右侧"），
            // 接收方自己那份还是默认的"共用一块屏" → 这里
            // 判定 edge=false → 推门交还整段不跑 → 光标顶死
            // 在门口也不交还，症状就是"回不去上一台"
            //（v0.6.61 实测）。门是发送方拓扑的一部分，
            // 不该要求接收方把同一件事再配一遍。
            if !landing && session_edge(self.sess.edge, peer_side) && !self.sess.edge {
                self.sess.edge = true;
                (self.log)(format!(
                    "{} 从 {} 出门 ⇒ 这条链路按「有门」处理（本机配置里它没占边，已按对端为准）",
                    self.sess.node,
                    ps.as_str()
                ));
            }
            let want = ps.opposite();
            if want != self.sess.portal {
                self.sess.portal = want;
                (self.log)(format!(
                    "门口方向按对端校正：{} 从 {} 出门 → 本机门口改到 {}",
                    self.sess.node,
                    ps.as_str(),
                    want.as_str()
                ));
            }
        }
        // 把"实际走的这道门"写给平台层：交还时要按它把光标摆回
        // 门内侧。以前平台层读的是配置里那个 side，而门口方向
        // 只在这里的局部变量里被 opposite() 校正过 —— 校正过的
        // 门和配置的门相反时，交还就把光标摆到了对面那条边
        //（被控方视角：键鼠回来了，光标却在屏幕另一头）。
        if let Some(cap) = self.capture_rx.as_ref() {
            cap.set_portal(self.sess.portal);
        }
        if entering {
            self.sess.injected = 0;
            self.sess.verify = None;
            self.sess.suspect = 0;
            // 接管这一刻强制"放开抓取"：幂等调用，作用是
            // 无条件重申 CGAssociate(true) + 清钉扎点。
            // 光标若不是关联着的，注入的位移一个像素都推
            // 不动（v0.6.40 的根因），进来先把这件事做实，
            // 别等 0.8 秒后的守卫。
            if let Some(cap) = self.capture_rx.as_ref() {
                cap.set_grabbing(false);
            }
            // 光标落点：多屏共享按出门高度进门；跟随画面模式
            // 没有"门"的概念，居中。重复的 Enter 不再挪，
            // 免得光标莫名跳一下。
            //
            // `landing` 是回程：共用屏那台没有门，但我们知道
            // 用户是从哪一侧把光标推回来的，就摆到那侧边缘
            // —— 否则它只能落在屏幕正中，看起来像"凭空闪现"。
            if self.sess.edge || landing {
                // 每次接管重新锚定工作屏：物理用户实际看着的那块
                let own = self.inject.enable_own_screen(self.sess.portal);
                let src = plat::work_screen_source();
                (self.log)(format!("本机工作屏锚定：{own:?}（来源：{}，1=物理活动屏 5=窗口分布 2=主屏 3=光标屏 4=几何）", src));
                let (ex, ey) = entry_spot(&self.inject, self.sess.portal, along);
                self.inject.place_cursor(ex, ey);
            } else {
                self.inject.center();
            }
            self.sess.edge_armed = false;
            self.sess.enter_ms = now_ms();
            self.sess.edge_hold_ms = 0;
            self.sess.motion_sample.clear();
            // 把"这场会话有没有门"写进日志。**排障全靠这一行**：
            // 「从这台推边推不回去」的第一个分叉就是它到底把
            // 这条链路当成有门还是共用一块屏 —— 以前这只能靠
            // 读代码推，用户贴不出证据来。
            let kind = if self.sess.edge {
                format!("有门，门口在{}", self.sess.portal.as_str())
            } else if landing {
                format!(
                    "共用一块屏（不判门；回程从{}侧进门）",
                    self.sess.portal.as_str()
                )
            } else {
                "共用一块屏（不判门，交还只能靠对端叫回或热键）".to_string()
            };
            if cur == State::Sending as u8 {
                (self.log)(format!(
                    "{} 过来接管了，键鼠已让给它（{kind}）",
                    self.sess.node
                ));
            } else {
                (self.log)(format!(
                    "{} 接管了键鼠（画面在你这边；{kind}）",
                    self.sess.node
                ));
            }
        }
    }
}
