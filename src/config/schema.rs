//! 配置长什么样：结构体、默认值、`Default`。
//!
//! 读写盘在 `store`，本机路径在 `paths`。serde 的缺省属性
//! （`#[serde(default = ...)]`）和小默认值函数都留在结构体旁边：
//! 改一个字段的缺省行为，只看这一个文件。

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::models::DisplayProfile;

use super::paths::{config_dir, default_node, home};

/// 配置结构版本。
///
/// 1 → 2：清掉旧版从对端广播里抄来的「有主机」标记（见 `Config::migrate`）。
/// 2 → 3：`kvm` 从"一条链路四个字段"改成链路表 + 网格坐标（见 `Kvm::links`）。
pub const CONFIG_VERSION: u32 = 3;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Network {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_true() -> bool {
    true
}
fn default_port() -> u16 {
    47831
}

impl Default for Network {
    fn default() -> Self {
        Self {
            enabled: true,
            port: default_port(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub node: String,
    #[serde(default = "default_hotkey")]
    pub hotkey: String,
    #[serde(default)]
    pub switch_order: Vec<String>,
    #[serde(default)]
    pub labels: HashMap<String, String>,
    /// 本机上次读到的、自己接在哪个口上。屏幕不在本机时读不到 DDC，
    /// 这个值就是唯一还记得的线索 —— 靠它才能请对端把画面切回来。
    #[serde(default)]
    pub own_input: String,
    #[serde(default)]
    pub own_value: u16,
    /// 热键要推到哪个口（一般是"对面那台机器"接的口）
    #[serde(default)]
    pub hotkey_target: String,
    #[serde(default)]
    pub network: Network,
    #[serde(default)]
    pub pair: Pair,
    #[serde(default)]
    pub transfer: Transfer,
    #[serde(default)]
    pub clipboard: Clipboard,
    #[serde(default)]
    pub kvm: Kvm,
    #[serde(default)]
    pub monitors: HashMap<String, DisplayProfile>,
}

/// 配对：两台机器填同一个码，之后所有局域网消息都带签名。
///
/// 码本身只是"门禁"不是加密 —— 6 位数字只有 20 bit 熵，抓包的人可以离线穷举。
/// 所以还给了长口令的余地：填 16 位以上时熵就够当真正的密钥用了。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Pair {
    #[serde(default)]
    pub code: String,
}

impl Pair {
    /// 已经配对（码够长到能当密钥材料）
    pub fn is_set(&self) -> bool {
        !self.code.trim().is_empty()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transfer {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_transfer_port")]
    pub port: u16,
    /// 接收目录。空 = 默认目录
    #[serde(default)]
    pub inbox: String,
}

fn default_transfer_port() -> u16 {
    47832
}

impl Default for Transfer {
    fn default() -> Self {
        Self {
            enabled: true,
            port: default_transfer_port(),
            inbox: String::new(),
        }
    }
}

impl Transfer {
    pub fn inbox_dir(&self) -> PathBuf {
        if self.inbox.trim().is_empty() {
            home().join("Downloads").join("InputSwitch")
        } else {
            PathBuf::from(self.inbox.trim())
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Clipboard {
    /// 文本同步总开关（老行为，即时发）
    #[serde(default)]
    pub enabled: bool,
    /// 文件/图片按需传输：复制时只记清单，对方按粘贴时才传。
    /// 默认关，因为要两端都升到 v0.6.39 才有意义。
    #[serde(default)]
    pub files: bool,
    /// 传完之后自动补按一次粘贴键（用户只按一次）。
    /// 关掉的话，传完只提示"已就绪，请再按一次"。
    #[serde(default = "default_true")]
    pub auto_paste: bool,
}

/// 共用键鼠。两种模式，见 `kvm::Mode`。
///
/// **v3 起拓扑是一张表**：一台对端一条 `LinkConfig`，每台机器自己在网格里
/// 占一格。相邻关系由坐标决定，不需要两边各配一次方向 —— 这是对着
/// Deskflow「有向边要写两遍、写漏一边就是能出去回不来」那处缺陷的直接修正。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Kvm {
    /// 是否启动。默认关：它会接管键盘鼠标，不能一装就生效。
    #[serde(default)]
    pub enabled: bool,
    /// 默认模式。`follow` = 跟随画面（软件 KVM）；
    /// `edge` = 边缘跨界（多屏共享）。链路自己没填 mode 时用它。
    #[serde(default = "default_kvm_mode")]
    pub mode: String,
    /// `console` = 键鼠插在本机；`remote` = 键鼠在对面。只在 follow 模式用。
    #[serde(default = "default_kvm_role")]
    pub role: String,
    /// 本机在拓扑网格里占的格子。右边那台 = (x+1, y)，上面那台 = (x, y-1)。
    #[serde(default)]
    pub grid_x: i32,
    #[serde(default)]
    pub grid_y: i32,
    /// 链路表：一台对端一条。空 = 老配置，由降级镜像升上来。
    #[serde(default)]
    pub links: Vec<LinkConfig>,

    /// 会话中把光标顶到邻居那侧的边，就自动把键鼠改指向过去（v0.6.60）。
    ///
    /// 默认开。关掉之后只剩 `Ctrl+Alt+方向` 那个热键 —— 手测时先关它验证
    /// 热键、再打开验证推边，两件事分开排查。真正误判（拖窗口到边、
    /// 拉滚动条）时也是这个开关兜底。
    #[serde(default = "default_true")]
    pub push_hop: bool,

    /// 接收位移倍率：把对端发来的位移乘上它再注入（默认 1.0）。
    ///
    /// 为什么需要这个旋钮：两端位移是按**屏幕分辨率比例**换算的（对端
    /// 2560px、本机 1920pt 时乘 0.75），这一步保证"跨过屏幕边界后继续
    /// 移动，走过的屏幕比例不变"。但两只操作系统的指针加速曲线不一样
    /// （macOS 的加速度、Windows 的「提高指针精确度」各算各的），同样的
    /// 位移流在两边手感必然有差 —— 实测「0DFF1JU 回到 Mac 之后鼠标比
    /// Windows 那边慢一点」。加速度曲线是用户级设置，程序没法猜，所以
    /// 留一个人工微调位：觉得慢就调大，觉得飘就调小。
    ///
    /// 只在**接收侧**生效（被控那台），发送侧不受影响。
    #[serde(default = "default_recv_scale")]
    pub recv_scale: f64,

    /// 程序启动后，若上次共用键鼠是开着的（`enabled`），对端一上线就自动恢复。
    ///
    /// 默认**关**：键鼠端用户最怕"一开机键鼠就被接管"。副机（没有键鼠那台）
    /// 必须打开，否则每次重启都要人去点一次 —— 而那台根本没有键鼠。
    ///
    /// 注意它不是"开机就抢"：判据是**对端上线**（`pick_peer` 找到人），
    /// 开机瞬间网络/显示器都还没就绪，那时候硬起只会失败。
    #[serde(default)]
    pub auto_resume: bool,

    /// 崩溃后自动重启（守护）。默认关。
    ///
    /// macOS = LaunchAgent 里的 `KeepAlive`（只在异常退出时拉起，用户主动
    /// 退出不会复活）；Windows = 一条登录触发、失败重启的计划任务。
    /// 调试时想让它安静死掉，就在面板上关掉这一条 —— 不要改代码。
    #[serde(default)]
    pub keep_alive: bool,

    // ---- 降级镜像 ----
    //
    // v3 起运行时不再读这三个字段（`peer_node` 除了"上次选的那台"这一
    // 层含义还留着）。它们的存在只为两件事：① 让还没升级的对端/旧版本
    // 读到"一条链路 + 一个方向"时不至于全空；② 旧 UI（P7 之前的菜单与
    // 面板）改的正是这三个字段，靠 `Kvm::mirror_*` 写透到主链路。
    #[serde(default = "default_side")]
    pub side: String,
    /// 上次共用键鼠实际在用的对端节点名（= links[0].node 的镜像）。
    #[serde(default)]
    pub peer_node: String,
}

/// 一条链路 = 一台对端。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LinkConfig {
    /// 对端节点名（发现层广播的 `node`）
    #[serde(default)]
    pub node: String,
    /// 从本机哪一侧出去：`left` / `right` / `top` / `bottom`。
    /// 空 = 没有门（跟随画面链路，不占边、不进掩码）。
    #[serde(default)]
    pub side: String,
    /// 这条链路的模式，空 = 用 `Kvm::mode`
    #[serde(default)]
    pub mode: String,
    /// 对端接在显示器哪个口（VCP 0x60 取值，十进制或 0x 前缀）。
    /// 空 = 用它广播的 `own_value`。
    #[serde(default)]
    pub input: String,
    /// 上次见到的地址。**只用来在它离线时占住那条边**：这样用户朝那边推
    /// 会得到一句"没有回应，不越界"，而不是静默无反应（静默会被当成程序
    /// 坏了）。真连得上时一律以发现层广播为准。
    #[serde(default)]
    pub addr: String,
}

impl LinkConfig {
    pub fn mode_or(&self, default: &str) -> String {
        if self.mode.trim().is_empty() {
            default.to_string()
        } else {
            self.mode.clone()
        }
    }
}

fn default_side() -> String {
    "right".to_string()
}

fn default_kvm_mode() -> String {
    // 默认跟随画面：装这套软件的人，绝大多数是"一套键鼠、一块屏、两台机器"
    "follow".to_string()
}

fn default_kvm_role() -> String {
    "console".to_string()
}

/// 接收位移倍率默认 1.0（不放大不缩小）。
fn default_recv_scale() -> f64 {
    1.0
}

impl Default for Kvm {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: default_kvm_mode(),
            role: default_kvm_role(),
            grid_x: 0,
            grid_y: 0,
            links: Vec::new(),
            push_hop: true,
            recv_scale: 1.0,
            auto_resume: false,
            keep_alive: false,
            side: default_side(),
            peer_node: String::new(),
        }
    }
}

impl Default for Clipboard {
    fn default() -> Self {
        Self {
            enabled: false,
            files: false,
            auto_paste: true,
        }
    }
}

fn default_version() -> u32 {
    CONFIG_VERSION
}
/// 默认切屏热键。恢复默认（面板「恢复默认」钮 → /api/hotkey-reset）和
/// 新配置的初始值都用它 —— 只有一个出处，改默认值只改这一行。
pub fn default_hotkey() -> String {
    "ctrl+alt+s".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            node: default_node(),
            hotkey: default_hotkey(),
            switch_order: Vec::new(),
            labels: HashMap::new(),
            own_input: String::new(),
            own_value: 0,
            hotkey_target: String::new(),
            network: Network::default(),
            pair: Pair::default(),
            transfer: Transfer::default(),
            clipboard: Clipboard::default(),
            kvm: Kvm::default(),
            monitors: HashMap::new(),
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        config_dir().join("config.json")
    }
}
