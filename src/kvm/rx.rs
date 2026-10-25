//! 每对端的心跳与存活登记表。
//!
//! 从 `mod.rs` 拆出来。这张表回答两个问题：「哪个对端还活着」和「它上次
//! 说自己是什么状态」—— 多对端下这两个答案都必须**按来源 IP 分开记**，
//! 否则 A 的包会把 B 的存活状态覆盖掉，表现为「明明两台都在，程序只认
//! 一台」。
//!
//! 全部方法 `pub(super)`：调用方集中在 `mod.rs`（链路存活判定、`Linker`
//! 改指向、`Kvm` 句柄、`start()` 收发线程），平台层不碰这张表。

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use super::now_ms;
use super::State;

/// 某个对端的心跳与存活信息
pub(super) struct PeerRx {
    /// 最后一次收到合法包的时刻（毫秒）
    last_rx: u64,
    /// 有没有真的收到过包。启动时 last_rx 就是当前时间，单看它会把
    /// "从没连上"误判成"刚刚连上"。
    seen: bool,
    /// 最近一次 Pong 里报的状态（`PEER_UNKNOWN` = 老版本，不带这个字节）
    state: u8,
}

type PeerRxMap = Arc<Mutex<HashMap<IpAddr, PeerRx>>>;

/// 每对端一份的心跳表。多对端下这三个值都必须按来源分开记，
/// 否则「谁还活着」和「谁报的什么状态」会互相覆盖。
#[derive(Clone)]
pub(super) struct RxTable(PeerRxMap);

impl RxTable {
    pub(super) fn new() -> Self {
        Self(Arc::new(Mutex::new(HashMap::new())))
    }

    fn entry(&self, ip: IpAddr) -> PeerRx {
        self.0
            .lock()
            .map(|t| {
                t.get(&ip).map(|r| PeerRx {
                    last_rx: r.last_rx,
                    seen: r.seen,
                    state: r.state,
                })
            })
            .ok()
            .flatten()
            .unwrap_or(PeerRx {
                // 没记录过：当作"刚刚收到过"（避免一上来就判超时），但
                // seen=false 让调用方知道它从没说过话
                last_rx: now_ms(),
                seen: false,
                state: State::Idle as u8,
            })
    }

    /// 收到该对端的任意合法包
    pub(super) fn touch(&self, ip: IpAddr) {
        if let Ok(mut t) = self.0.lock() {
            let e = t.entry(ip).or_insert(PeerRx {
                last_rx: 0,
                seen: false,
                state: State::Idle as u8,
            });
            e.last_rx = now_ms();
            e.seen = true;
        }
    }

    pub(super) fn idle_ms(&self, ip: IpAddr) -> u64 {
        now_ms().saturating_sub(self.entry(ip).last_rx)
    }

    pub(super) fn alive(&self, ip: IpAddr) -> (u64, bool) {
        let e = self.entry(ip);
        (now_ms().saturating_sub(e.last_rx), e.seen)
    }

    pub(super) fn state(&self, ip: IpAddr) -> u8 {
        self.entry(ip).state
    }

    pub(super) fn set_state(&self, ip: IpAddr, st: u8) {
        if let Ok(mut t) = self.0.lock() {
            let e = t.entry(ip).or_insert(PeerRx {
                last_rx: now_ms(),
                seen: true,
                state: st,
            });
            e.state = st;
        }
    }
}
