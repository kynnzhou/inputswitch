//! 控制面板的后端：一个只监听本机的 HTTP 服务。
//!
//! 界面本身是程序自己的窗口（见 `panelwin.rs`，macOS 用 WKWebView、Windows 用
//! WebView2），这里只负责出页面和接口。之所以是 HTTP 而不是进程内直接调用：
//! tao 没有控件系统，要做"一屏看全"的界面绕不开 HTML；而 HTML 需要一个取数通道，
//! 顺手做成 HTTP 之后，命令行、脚本、别的进程甚至手机都能用同一套接口，
//! 也保住了"窗口开不出来就退回浏览器"的退路。
//!
//! 服务只绑 127.0.0.1，外网连不进来。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::transfer::human_size;

/// 面板上看到的一切。UI 线程每 400ms 刷一次。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub node: String,
    pub display: String,
    pub identity: String,
    pub status: String,
    pub support: String,
    pub current: u16,
    pub current_name: String,
    pub hotkey: String,
    pub target: String,
    pub paired: bool,
    pub pair_code: String,
    pub rejected: u64,
    /// 签名对但重复收到的包数（重发 / 多网卡各收一份），不是攻击
    pub replayed: u64,
    /// 有几个对端的签名真的验过了。`paired` 只代表本机填了码，验过才算连上。
    pub peer_verified: usize,
    /// 本机此刻有没有接通的显示器。没有就说明画面正显示另一台机器。
    pub has_display: bool,
    /// 本机上次读到的、自己挂在哪个口
    pub own_value: u16,
    /// 上一次「重新检测显示器」的结论
    pub port_note: String,
    pub autostart: bool,
    pub clipboard: bool,
    pub inbox: String,
    pub port: u16,
    pub peers: Vec<PeerView>,
    pub inputs: Vec<InputView>,
    /// 固件声明过、但本机从没验证过也没有任何信号的口。
    /// 只给 `/api/state` 排查用，界面上一律不展示 —— 它们可能连物理接口
    /// 都没有，列出来会被当成"本机的输入源"。
    #[serde(default)]
    pub inputs_hidden: Vec<InputView>,
    /// 正在进行的传输，空串表示空闲
    pub transfer: String,
    pub kvm: KvmView,
    /// 共用键鼠要的两个系统权限，**由 GUI 进程自己判断**
    ///
    /// 存在的理由：`inputswitch-cli kvm-probe` 是包里的第二个二进制，
    /// 它有自己的 cdhash，TCC 记录只认主程序 —— 它永远报「没权限」，
    /// 而 GUI 实际是通的（2026-10-20 据此误判过好几次）。
    /// 排查权限一律看这里。
    pub perms: PermsView,
}

/// 两个权限的实际状态。抓取=辅助功能，注入=辅助功能/PostEvent。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PermsView {
    /// 能抓取本机键鼠（软需求：缺了就降级成"能收不能发"）
    pub capture: bool,
    /// 能往本机注入事件（硬需求：缺了共用键鼠起不来）
    pub inject: bool,
}

/// 共用键鼠在面板上的样子
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct KvmView {
    /// 实例有没有起来
    pub on: bool,
    /// `follow` 或 `edge`
    pub mode: String,
    /// `console` 或 `remote`，只在 follow 模式有意义
    pub role: String,
    /// 空闲 / 键鼠已交给对端 / 键鼠在对面
    pub state: String,
    /// 对端最近有没有回包
    pub peer_alive: bool,
    /// 现在握着键鼠的是哪台（空串 = 没有会话）
    #[serde(default)]
    pub active: String,
    /// 链路表：一台对端一条。面板按它画"这台在哪边 / 什么模式 / 活着没 / 谁在用"
    #[serde(default)]
    pub links: Vec<LinkView>,
    /// 链路表里有没有"共用一块屏"（Follow）链路。
    ///
    /// 这是**拓扑事实**，跟共用键鼠开没开无关 —— 面板靠它决定藏不藏
    /// 「现在就把键鼠交给对端」那个按钮（画面一走就看不见面板，
    /// 在共用屏拓扑里它没有可点的时机）。
    #[serde(default)]
    pub follow_link: bool,
    /// 点「切换」是否真的会连键鼠一起走 = `follow_link && 本机能抓取`。
    ///
    /// 所有"（键鼠跟过去）"的文案都问它。以前前端自己拿 `mode` + `role`
    /// 推：`mode` 只取第一条链路，跟后端 `has_follow_link()` 不是一个
    /// 判据，两边必然打架。
    #[serde(default)]
    pub will_follow: bool,
}

/// 一条对端链路在面板上的样子
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LinkView {
    pub node: String,
    pub addr: String,
    /// 对端在本机的哪一侧（右侧 / 下方 …）。空 = 无门（跟随画面链路）
    pub side: String,
    /// `follow` 或 `edge`
    pub mode: String,
    /// 对端最近有没有回包
    pub alive: bool,
    /// 键鼠现在是不是在这台手上
    pub active: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PeerView {
    pub node: String,
    pub addr: String,
    pub input: String,
    pub label: String,
    pub value: u16,
    pub paired: bool,
    pub knows_port: bool,
    /// 对面设了配对码，而本机没设 —— 这时候发给它的指令会被当未签名包丢掉
    #[serde(default)]
    pub needs_pair: bool,
    /// 对端的共用键鼠开着没有。它开着时键鼠交不过去，跟"对面没开机"
    /// 表现一样（KVM 心跳不来），所以必须分开显示
    #[serde(default)]
    pub kvm_on: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct InputView {
    pub name: String,
    pub label: String,
    pub value: u16,
    pub current: bool,
    pub presence: String,
    pub verified: bool,
}

/// 面板能触发的动作。tray 侧负责翻译成内部命令。
#[derive(Clone, Debug)]
pub enum Action {
    /// `follow = true` 时，切屏成功后键鼠会跟过去；false 就只换画面
    Switch {
        name: String,
        value: u16,
        follow: bool,
    },
    SetTarget { name: String, value: u16 },
    ClearTarget,
    Rescan,
    ScanPorts,
    CheckSupport,
    Pair(String),
    GenerateCode,
    ToggleAutostart(bool),
    ToggleClipboard(bool),
    ChooseInbox,
    OpenInbox,
    /// 选文件发给对端。多机时必须点名 —— 不带 `node` 时按
    /// "握着键鼠的那台 > 唯一在线那台" 兜底，多台又没会话就直接拒绝。
    SendFiles(Option<String>),
    /// 弹出原生录制窗口让用户按一下新的组合键。
    /// 放不到网页里做：网页只收得到"焦点在页面上"的按键，按不出全局热键。
    RecordHotkey,
    /// 在文件管理器里打开配置目录
    OpenConfigDir,
    /// 本机读不到 DDC（画面在对面），请对端替我把画面切到指定口
    AskPeer(u16),
    /// 开 / 关共用键鼠
    KvmPower(bool),
    /// 换模式：`follow` 或 `edge`
    KvmMode(String),
    /// 换角色：`console` 或 `remote`（只在 follow 模式有用）
    KvmRole(String),
    /// 手动把键鼠交给对端（不用切屏，先试试通不通）。
    /// 多机时带上是哪一台；None 表示"随便挑一条 Follow 链路"（老接口的行为）。
    KvmHandOver(Option<String>),
    /// 手动收回键鼠
    KvmTakeBack,
    /// 改某一条链路的方向 / 模式。`None` 表示这项不动。
    KvmLink {
        node: String,
        side: Option<String>,
        mode: Option<String>,
    },
    /// 重启本程序。
    ///
    /// 只为一件事存在：macOS 把 TCC 授权结论**缓存在进程里**。用户在设置里
    /// 勾上以后，已经在跑的进程照样报"没权限"，点「开启共用键鼠」只会再弹
    /// 一次设置页 —— 看起来像"勾选无效 / 装错版本"。唯一解法是重启进程。
    RestartApp,
}

pub type State = Arc<Mutex<Snapshot>>;
pub type Dispatcher = Arc<dyn Fn(Action) + Send + Sync>;

/// 起服务，返回真正绑定成功的端口（首选端口被占时会顺延）。
pub fn start(state: State, dispatch: Dispatcher, preferred: u16) -> std::io::Result<u16> {
    let mut bound = None;
    for offset in 0..8 {
        let port = preferred + offset;
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => {
                bound = Some((listener, port));
                break;
            }
            Err(_) => continue,
        }
    }
    let Some((listener, port)) = bound else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "47833–47840 都被占用了",
        ));
    };

    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(stream) = incoming else { continue };
            let state = state.clone();
            let dispatch = dispatch.clone();
            std::thread::spawn(move || {
                let _ = handle(stream, state, dispatch, port);
            });
        }
    });
    Ok(port)
}

fn handle(
    mut stream: TcpStream,
    state: State,
    dispatch: Dispatcher,
    port: u16,
) -> std::io::Result<()> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find(&buf, b"\r\n\r\n") {
            break pos;
        }
        if buf.len() > 128 * 1024 {
            return Ok(());
        }
    };

    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let request = lines.next().unwrap_or_default().to_string();
    let mut parts = request.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts.next().unwrap_or("/").to_string();

    let mut content_length = 0usize;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    let body = String::from_utf8_lossy(&body).to_string();

    let (route, query) = match path.split_once('?') {
        Some((a, b)) => (a.to_string(), b.to_string()),
        None => (path.clone(), String::new()),
    };

    match (method.as_str(), route.as_str()) {
        ("GET", "/") => respond(&mut stream, 200, "text/html; charset=utf-8", PANEL_HTML),
        ("GET", "/api/state") => {
            let snap = state.lock().unwrap().clone();
            let json = serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into());
            respond(&mut stream, 200, "application/json; charset=utf-8", &json)
        }
        ("POST", "/api/switch") => {
            if let Some(value) = param_u16(&query, "value") {
                let name = state
                    .lock()
                    .unwrap()
                    .inputs
                    .iter()
                    .find(|i| i.value == value)
                    .map(|i| i.name.clone())
                    .unwrap_or_default();
                // follow 缺省为 true：正常点「切换」就是"画面过去、键鼠也过去"。
                // 只有「仅切换画面」这个按钮才显式带 follow=false。
                let follow = param_bool(&query, "follow").unwrap_or(true);
                dispatch(Action::Switch {
                    name,
                    value,
                    follow,
                });
                respond_json(&mut stream, r#"{"ok":true}"#)
            } else {
                respond_json(&mut stream, r#"{"ok":false,"error":"缺 value"}"#)
            }
        }
        ("POST", "/api/target") => {
            if let Some(value) = param_u16(&query, "value") {
                let name = state
                    .lock()
                    .unwrap()
                    .inputs
                    .iter()
                    .find(|i| i.value == value)
                    .map(|i| i.name.clone())
                    .unwrap_or_default();
                if value == 0 {
                    dispatch(Action::ClearTarget);
                } else {
                    dispatch(Action::SetTarget { name, value });
                }
                respond_json(&mut stream, r#"{"ok":true}"#)
            } else {
                respond_json(&mut stream, r#"{"ok":false,"error":"缺 value"}"#)
            }
        }
        ("POST", "/api/pair") => {
            let code = json_field(&body, "code");
            match code {
                Some(code) => {
                    dispatch(Action::Pair(code));
                    respond_json(&mut stream, r#"{"ok":true}"#)
                }
                None => respond_json(&mut stream, r#"{"ok":false,"error":"缺 code"}"#),
            }
        }
        ("POST", "/api/generate-code") => {
            dispatch(Action::GenerateCode);
            respond_json(&mut stream, r#"{"ok":true}"#)
        }
        ("POST", "/api/autostart") => {
            let on = body.contains("true");
            dispatch(Action::ToggleAutostart(on));
            respond_json(&mut stream, r#"{"ok":true}"#)
        }
        ("POST", "/api/clipboard") => {
            let on = body.contains("true");
            dispatch(Action::ToggleClipboard(on));
            respond_json(&mut stream, r#"{"ok":true}"#)
        }
        ("POST", "/api/rescan") => {
            dispatch(Action::Rescan);
            respond_json(&mut stream, r#"{"ok":true}"#)
        }
        ("POST", "/api/scan") => {
            dispatch(Action::ScanPorts);
            respond_json(&mut stream, r#"{"ok":true}"#)
        }
        ("POST", "/api/check") => {
            dispatch(Action::CheckSupport);
            respond_json(&mut stream, r#"{"ok":true}"#)
        }
        ("POST", "/api/hotkey-record") => {
            dispatch(Action::RecordHotkey);
            respond_json(&mut stream, r#"{"ok":true}"#)
        }
        ("POST", "/api/open-config") => {
            dispatch(Action::OpenConfigDir);
            respond_json(&mut stream, r#"{"ok":true}"#)
        }
        ("POST", "/api/ask-peer") => {
            match param_u16(&query, "value") {
                Some(value) => {
                    dispatch(Action::AskPeer(value));
                    respond_json(&mut stream, r#"{"ok":true}"#)
                }
                None => respond_json(&mut stream, r#"{"ok":false,"error":"缺 value"}"#),
            }
        }
        ("POST", "/api/send") => {
            dispatch(Action::SendFiles(param_str(&query, "node")));
            respond_json(&mut stream, r#"{"ok":true}"#)
        }
        ("POST", "/api/inbox") => {
            dispatch(Action::ChooseInbox);
            respond_json(&mut stream, r#"{"ok":true}"#)
        }
        ("POST", "/api/open-inbox") => {
            dispatch(Action::OpenInbox);
            respond_json(&mut stream, r#"{"ok":true}"#)
        }
        ("POST", "/api/kvm") => {
            let on = param_bool(&query, "on").unwrap_or(false);
            dispatch(Action::KvmPower(on));
            respond_json(&mut stream, r#"{"ok":true}"#)
        }
        ("POST", "/api/kvm-mode") => match param_str(&query, "mode") {
            Some(mode) => {
                dispatch(Action::KvmMode(mode));
                respond_json(&mut stream, r#"{"ok":true}"#)
            }
            None => respond_json(&mut stream, r#"{"ok":false,"error":"缺 mode"}"#),
        },
        ("POST", "/api/kvm-role") => match param_str(&query, "role") {
            Some(role) => {
                dispatch(Action::KvmRole(role));
                respond_json(&mut stream, r#"{"ok":true}"#)
            }
            None => respond_json(&mut stream, r#"{"ok":false,"error":"缺 role"}"#),
        },
        ("POST", "/api/kvm-handover") => {
            // 多机时点名交给某一台；不带 node 就是老行为（挑一条 Follow 链路）
            dispatch(Action::KvmHandOver(param_str(&query, "node")));
            respond_json(&mut stream, r#"{"ok":true}"#)
        }
        ("POST", "/api/link") => match param_str(&query, "node") {
            Some(node) => {
                let side = param_str(&query, "side");
                let mode = param_str(&query, "mode");
                // 在入口就把明显错的挡掉：dispatch 是异步的，等不到结果，
                // 这里回 ok:true 而实际没改，前端只会看见"改了没生效"。
                let bad_side = side.as_deref().is_some_and(|s| {
                    let s = s.trim();
                    !s.is_empty()
                        && s != "left"
                        && s != "right"
                        && s != "top"
                        && s != "bottom"
                });
                let bad_mode = mode
                    .as_deref()
                    .is_some_and(|m| m != "edge" && m != "follow");
                if bad_side || bad_mode {
                    respond_json(
                        &mut stream,
                        r#"{"ok":false,"error":"side 只能是 left/right/top/bottom 或空，mode 只能是 edge/follow"}"#,
                    )
                } else if side.is_none() && mode.is_none() {
                    respond_json(&mut stream, r#"{"ok":false,"error":"side 和 mode 都没给"}"#)
                } else {
                    // 链路表里没有这台，但发现层看到过（snap.peers）就放行 ——
                    // 托盘侧会现建一条链路。这是面板上"把还没配的对端拖进格子"
                    // 那条路；两边都没有才拒。
                    //
                    // 为什么入口要挡：dispatch 是异步的，等不到结果，
                    // 这里回 ok:true 而实际没改，网页只会看见"改了没生效"。
                    let (known, discovered) = state
                        .lock()
                        .map(|snap| {
                            (
                                snap.kvm.links.iter().any(|link| link.node == node),
                                snap.peers.iter().any(|p| p.node == node),
                            )
                        })
                        .unwrap_or((false, false));
                    if known || discovered {
                        dispatch(Action::KvmLink { node, side, mode });
                        respond_json(&mut stream, r#"{"ok":true}"#)
                    } else {
                        respond_json(
                            &mut stream,
                            r#"{"ok":false,"error":"链路表里没有这个节点，局域网里也没发现它"}"#,
                        )
                    }
                }
            }
            None => respond_json(&mut stream, r#"{"ok":false,"error":"缺 node"}"#),
        },
        ("POST", "/api/kvm-takeback") => {
            dispatch(Action::KvmTakeBack);
            respond_json(&mut stream, r#"{"ok":true}"#)
        }
        // 先回包再重启：进程一退，连接就断了，客户端看不到 ok 会以为失败
        ("POST", "/api/restart") => {
            dispatch(Action::RestartApp);
            respond_json(&mut stream, r#"{"ok":true}"#)
        }
        _ => respond(&mut stream, 404, "text/plain; charset=utf-8", "not found"),
    }?;

    let _ = port;
    Ok(())
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn param_u16(query: &str, key: &str) -> Option<u16> {
    param_str(query, key)?.parse().ok()
}

fn param_str(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn param_bool(query: &str, key: &str) -> Option<bool> {
    match param_str(query, key)?.as_str() {
        "1" | "true" | "on" | "yes" => Some(true),
        "0" | "false" | "off" | "no" => Some(false),
        _ => None,
    }
}

/// 极简 JSON 取值：面板只会发 `{"code":"123456"}` 这种一层结构。
fn json_field(body: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let start = body.find(&needle)? + needle.len();
    let rest = &body[start..];
    let colon = rest.find(':')? + 1;
    let rest = rest[colon..].trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn respond_json(stream: &mut TcpStream, body: &str) -> std::io::Result<()> {
    respond(stream, 200, "application/json; charset=utf-8", body)
}

fn respond(
    stream: &mut TcpStream,
    code: u16,
    content_type: &str,
    body: &str,
) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {code} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        if code == 200 { "OK" } else { "Not Found" },
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()
}

pub fn human(bytes: u64) -> String {
    human_size(bytes)
}

/// 退路：窗口创建失败时（比如 Windows 没装 WebView2 运行时）用系统浏览器打开。
pub fn open_in_browser(port: u16) -> bool {
    let url = format!("http://127.0.0.1:{port}/");
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("/usr/bin/open");
        c.arg(&url);
        c
    };
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("rundll32.exe");
        c.args(["url.dll,FileProtocolHandler", &url]);
        c
    };
    #[cfg(target_os = "linux")]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(&url);
        c
    };
    crate::hidden::quiet(&mut cmd);
    cmd.spawn().is_ok()
}

const PANEL_HTML: &str = include_str!("../assets/panel.html");
