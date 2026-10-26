//! 切换编排层。
//!
//! 核心判断：DDC 写操作的返回值只说明"事务被接受了"，不能说明显示器真的换了口。
//! 唯一可靠的确认信号是**显示器从本机拓扑里消失**。
//!
//! ---- 切分轴（2026-10-25，原 979 行一个文件）----
//! 按「改一件事要动几个文件」切，不按层切：
//!   types      报告与结论的名词：ProbeSource / PortProbe / ScanReport / SwitchOutcome
//!   probe      这台显示器是谁、能干什么：身份键、EDID 与能力表缓存、挑哪台操作
//!   topology   屏幕在不在本机：拓扑等待原语 + DDC 静默期（switch / scan / discover 共用）
//!   switch     切换：按名字切与按 MCCS 标准值切，以及"怎么算切成功了"的判据
//!   scan       扫输入口：逐口物理试探 + 把画面送回本机（scan_presence 与 go_home）
//!   discover   向导：试出"另一个口"的 0x60 取值
//! 子模块是私有 `mod`，公开面一律 `pub use` 收口在本文件 —— 拆分前后
//! `crate::switcher::Switcher` 这些路径一个都没变。被兄弟模块调用的私有方法
//! 提权到 `pub(super)`（tray/ 同一路子），只给 `crate::switcher` 这层看。

mod discover;
mod probe;
mod scan;
mod switch;
mod topology;
mod types;

pub use types::{PortProbe, ProbeSource, ScanReport, SwitchOutcome};

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Instant;

use crate::caps::Caps;
use crate::config::{Config, RuntimeState};
use crate::ddc::{new_backend, Backend};
use crate::edid::Edid;

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
}

impl Default for Switcher {
    fn default() -> Self {
        Self::new()
    }
}
