//! 一次切屏，以及这次切屏给键鼠留下了什么。
//!
//! 从 engine_loop 里独立出来，只因为一件事：这里的结论**不能靠调用方自己拼**。
//! 七个分支各有各的说法，说错一句用户就要伸手去按显示器按钮。Keys 枚举
//! 逼着每个分支表态，文案只在这一处生成。

use std::time::Duration;

use crate::kvm;
use crate::peer::Mesh;
use crate::Switcher;

use super::engine::sync_own;
use super::hotkey::pretty_hotkey;
use super::kvmctl::{explain_silent_peer, follow_link_node};
use super::kvm_now;

/// 这次切屏给键鼠留下了什么结果 —— 成功文案的**唯一**依据。
///
/// 原来是 `handed / reclaimed / stranded` 三个互不相干的 bool：任何一个
/// 分支漏设一个，就会落到"（键鼠已跟过去）"这个默认结论上。实测暴露的
/// 三处谎报（Receiving 下 `follow()` 空转被当成功、`value == own` 时
/// `give_back()` 对 Receiving 是 no-op、纯 edge 拓扑根本没交接）全是
/// 同一个成因。改成枚举后，编译器逼着每个分支表态。
#[derive(PartialEq)]
pub(super) enum Keys {
    /// 没动过，键鼠本来就在本机
    Home,
    /// 共用键鼠没开
    Off,
    /// 这次真的开始转发了
    Sent,
    /// 键鼠本来就在对端手上，这次没有重复交接
    AtPeer(String),
    /// 键鼠在对端，我们刚发了 Takeback —— 只能承诺"请了"，不能断言收回
    AskBack,
    /// 真的从转发里收回本机（`give_back` 生效）
    Back,
    /// 想交没交成
    Stranded(String),
}

/// 工作线程：独占一个 Switcher，串行处理 DDC 指令。
/// 执行一次切屏，并按结果决定键鼠跟不跟过去。
///
/// `follow = false` 是"只想换个画面"，比如自己接了两个显示口想切着看，
/// 或者故意让键鼠留在本机操作。这时候不能顺手把键鼠也带走。
pub(super) fn do_switch(
    switcher: &mut Switcher,
    mesh: &Mesh,
    name: &str,
    value: u16,
    follow: bool,
) -> String {
    // 切走之前先把自己的口记下来，切完就读不到了
    let _ = sync_own(switcher, mesh);
    let own = switcher.config.own_value;

    // ---- 顺序很关键：**先交键鼠，再切画面** ----
    //
    // 反过来（先切画面、再交键鼠）是条死路：画面切走之后本机既读不到也
    // 写不了 DDC（实测 9/9 失败），而面板也跟着看不见了 —— 用户没有
    // 任何地方可以点"把键鼠交过去"。所以这一屏的键鼠必须在这里、在
    // 画面离开之前就交出去。
    //
    // 但键鼠交不出去时**不拒绝切换**：硬拒切的代价是"开了共用键鼠就连
    // 画面都切不了"（实测反馈）。降级放行，只是把真实结果说清楚。
    let mut keys = Keys::Home;
    if follow {
        match kvm_now() {
            // 共用键鼠没开：键鼠物理上在本机，但用户以为它会跟 —— 说清楚
            None => keys = Keys::Off,

            // 切回自己占的口（对端请我们切回去，或热键目标就是本机）：
            // 键鼠必须回本机，这跟「交给对端」是相反方向的同一个动作
            Some(handle) if value == own => match handle.state() {
                // 只有真的握着键鼠时 give_back 才动（release 只处理 Sending）
                kvm::State::Sending => {
                    keys = if handle.give_back() {
                        Keys::Back
                    } else {
                        Keys::Home
                    };
                }
                // 被控时 give_back() 是**空操作**，原来却照样说「已收回本机」。
                // 能要回来的只有 Takeback，而它是异步的：对端收到才松手，
                // 本机不能替它宣布已经收回。
                kvm::State::Receiving => {
                    keys = if handle.take_back_from_peer("画面切回本机") {
                        Keys::AskBack
                    } else {
                        Keys::Home
                    };
                }
                kvm::State::Idle => keys = Keys::Home,
            },

            // 切到对端，且配了"跟随画面"链路（共用一块屏的那台）
            Some(handle) if handle.has_follow_link() => {
                // **对端不在网上时，画面一步都不许动。**
                //
                // 切画面是单行道：走完之后本机既读不到也写不了 DDC（实测
                // 9/9 失败），画面又不在本机 —— 用户看不见光标、也没法操作
                // 对端，只能伸手去按显示器按钮。以前这里"降级放行"，结果是
                // 用户报「切过去键鼠用不了」（v0.6.62 实测：那台机器整机不在
                // 网上，ping 100% 丢、445 连不上，键鼠根本没处可交）。
                //
                // 对端真离线时切过去也看不到东西（它没有信号输出），所以
                // 硬拒不会挡住"我要看那台的屏幕"这种需求。
                if let Some(node) = follow_link_node(&handle) {
                    if handle.link_alive_by_node(&node) == Some(false) {
                        return format!(
                            "{node} 不在线，画面没有切 —— 切过去你就看不见本机了，\
                             键鼠也没处可交。先确认那台机器开着、和本机同一网段、\
                             防火墙没把程序拦了（Windows 网络位置若是「公用」会全拦）"
                        );
                    }
                }
                match handle.state() {
                    kvm::State::Idle => match handle.follow() {
                        Ok(true) => keys = Keys::Sent,
                        // 刚好被别的线程抢先建了会话：照实说键鼠在哪台
                        Ok(false) => {
                            keys = Keys::AtPeer(
                                handle.active_node().unwrap_or("对端").to_string(),
                            );
                        }
                        Err(err) => {
                            // 「没有回应」有两种截然不同的原因，说错就白查网线
                            let target = follow_link_node(&handle);
                            keys = Keys::Stranded(explain_silent_peer(
                                mesh,
                                target.as_deref(),
                                &err,
                            ));
                        }
                    },
                    // 已经在往外送：follow() 这时是空转，不能当"刚交过去"
                    kvm::State::Sending => {
                        keys = Keys::AtPeer(handle.active_node().unwrap_or("对端").to_string());
                    }
                    // 键鼠在对端手上（它在控制本机）。画面要切到对面去，得先
                    // 请它松手，否则它的按键还会继续打进本机。
                    kvm::State::Receiving => {
                        keys = if handle.take_back_from_peer("切到对端前先收回键鼠") {
                            Keys::AskBack
                        } else {
                            Keys::AtPeer("对端".to_string())
                        };
                    }
                }
            }

            // 没有"跟随画面"链路（多屏共享）：切画面跟键鼠无关，一律不动。
            // 但**不能**因此说"键鼠已跟过去" —— 原来是这里默认成的。
            Some(handle) => {
                keys = match handle.state() {
                    kvm::State::Idle => Keys::Home,
                    kvm::State::Sending | kvm::State::Receiving => {
                        Keys::AtPeer(handle.active_node().unwrap_or("对端").to_string())
                    }
                };
            }
        }
    }

    // 8 秒校验窗：实测这台显示器从收到 DDC 到离开本机拓扑要 2~6 秒不等，
    // 2 秒窗口会频繁误判失败，进而把刚交出去的键鼠收回来
    let outcome = switcher.switch_raw(name, value, Duration::from_secs(8));

    // 失败分两种，处理完全不同：
    //
    // 确凿失败（那个口没信号，显示器自己弹回来了）：画面明明在本机，
    // 键鼠必须立刻回本机。
    //
    // 模糊失败（校验窗超时 + VCP 读不到目标值）：这台显示器切到别的输入源
    // 后**并不离开 macOS 拓扑**（EDID 一直活着），VCP 读值也不更新——两个
    // 成功信号全部失效，但画面实际是切成功的（18:00 日志实锤：转发了
    // 1625 笔事件后误判失败、键鼠被抢收，用户看到「刚切过去能用，过一会
    // 就不能用」）。所以模糊失败时键鼠留在对端不动；真没切过去的话，
    // 用户按热键（Ctrl+Alt+Esc）收回兜底，转发中热键实测可用。
    let mut handed_ambiguous = false;
    if keys == Keys::Sent && !outcome.ok {
        if outcome.definite {
            // 确凿失败：画面明明在本机，键鼠必须立刻回本机
            if let Some(handle) = kvm_now() {
                if handle.give_back() {
                    keys = Keys::Back;
                }
            }
        } else {
            switcher.suppress_ddc(Duration::from_secs(4));
            handed_ambiguous = true;
        }
    }

    let hotkey = pretty_hotkey(&switcher.config.hotkey);
    if outcome.ok || handed_ambiguous {
        if handed_ambiguous {
            return format!(
                "键鼠已交给对端；显示器切换没能确认（{}）。\
                 若画面还在本机，按热键 {hotkey} 可收回键鼠",
                outcome.message
            );
        }
        match keys {
            Keys::Sent => format!("已切到 {name}（键鼠已跟过去）"),
            Keys::AtPeer(node) => {
                format!("已切到 {name}；键鼠已经在 {node}，这次没有重复交接")
            }
            Keys::AskBack => {
                format!("已切到 {name}；键鼠在对端，已请它交还 —— 对面收到后就能用")
            }
            // 只有画面回到本机口，才谈得上"切回"
            Keys::Back if value == own => format!("画面已切回 {name}（键鼠已收回本机）"),
            Keys::Back => format!("已切到 {name}（键鼠已收回本机）"),
            Keys::Stranded(err) => format!(
                "已切到 {name}，但键鼠留在本机（{err}）。按热键 {hotkey} 可把画面切回来"
            ),
            Keys::Off => format!("已切到 {name}（共用键鼠没开，键鼠留在本机）"),
            Keys::Home if value == own => format!("画面已切回 {name}（键鼠就在本机）"),
            Keys::Home if follow => format!("已切到 {name}（键鼠留在本机）"),
            Keys::Home => format!("已切到 {name}（只切画面，键鼠留在本机）"),
        }
    } else if keys == Keys::Back {
        // 切回本机时键鼠已经收回来了，画面切没切成是另一回事，别吓唬人
        format!("键鼠已收回本机；{}", outcome.message)
    } else if keys == Keys::AskBack {
        // Takeback 已发，但对端还没确认 —— 不能替它宣布"已收回"
        format!("已请对端交还键鼠；{}", outcome.message)
    } else {
        outcome.message
    }
}
