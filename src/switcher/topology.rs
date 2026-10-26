//! 屏幕在不在本机：拓扑等待原语 + DDC 静默期。
//!
//! switch / scan / discover 都要等"显示器离开 / 回来"，判据只能按型号名
//! （切走再回来 registry id 会变，用 uid 比对会永远认为"已经不在了"），
//! 所以这些原语共用一份、住在本文件。Apple Silicon 上拓扑变动后的重新协商
//! 窗口极脆，静默期逻辑（suppress_ddc / ddc_quiet）也跟着放这。

use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::models::Display;

use super::Switcher;

impl Switcher {
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

    /// 屏幕是否还挂在本机。为 false 通常意味着已经切到对面那台了。
    pub fn display_here(&self) -> bool {
        !self.backend.list_displays().is_empty()
    }

    /// 这台显示器还在不在本机拓扑里。按型号名判断，因为切走再回来时
    /// registry id 会重新分配，用 uid 比对会永远认为"已经不在了"。
    pub(super) fn display_named(&self, name: &str) -> bool {
        self.backend.list_displays().iter().any(|d| d.name == name)
    }

    pub(super) fn wait_until_gone(&self, display: &Display, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if !self.display_named(&display.name) {
                return true;
            }
            sleep(Duration::from_millis(150));
        }
        false
    }

    /// 切走之后再盯一会儿：显示器自己回来了，说明那个口根本没有信号。
    pub(super) fn bounced_back(&self, display: &Display, window: Duration) -> bool {
        let deadline = Instant::now() + window;
        while Instant::now() < deadline {
            if self.display_named(&display.name) {
                return true;
            }
            sleep(Duration::from_millis(200));
        }
        false
    }
}
