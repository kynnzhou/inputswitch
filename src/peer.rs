//! 双机局域网协同：UDP 广播发现 + 中继切换 + 配对签名。
//!
//! 为什么需要中继：显示器被切到 A 口后 B 机的 DDC 通道通常立刻失效，
//! 所以"当前正在用的那台"往往就是唯一能发 DDC 指令的机器。
//! 利用这个特性：让持有通道的那台替另一台干活，于是只要任意一台可用，
//! 双向都能切。
//!
//! 顺带解决了"哪个口接了主机"的问题：每台机器只要屏幕在自己这边，
//! 就能读出自己占用的 VCP 0x60 取值，随广播一起发出去。对面收到后
//! 就知道"那台机器在 0xNN 口上"，不需要逐个试。DDC 本身查不到未激活口
//! 的连接状态，这条网络侧的信息是唯一可靠的替代。
//!
//! 配对（`set_key`）打开后，每条消息都带 HMAC 签名 + 时间戳 + nonce，
//! 陌生人发的指令一律丢弃 —— 见 `pair.rs`。

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::pair::{self, NonceCache};

pub const PROTOCOL_VERSION: u32 = 1;
/// 剪贴板文件按需传输的能力版本。1 = 认识 `clip_pull`、能按清单反向拉取。
pub const CLIP_VERSION: u8 = 1;
/// 多对端（一对多）路由的能力版本。0 / 缺省 = 单对端（v0.6.43 及更早）。
///
/// 为什么要单独一位：老版本的接收循环里有一句
/// `if from.ip() != 当前会话对端.ip() { continue }`，多出来的机器发什么
/// 都会被静默丢掉 —— 表现是"半通"，最难查。只有 `kvm_v` 够高的机器
/// 才允许被配成第二、第三条链路。
///
/// 1 = 网络语义已适配多对端（回包按来源、Pong 按对端投影、指令带来源）；
/// 2 = 真的能路由（P4 落地后升到 2）。面板按 `>= 2` 判定可否加入路由。
pub const KVM_VERSION: u8 = 1;
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(2);
const PEER_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Serialize, Deserialize)]
struct Message {
    v: u32,
    #[serde(rename = "type")]
    kind: String,
    node: String,
    #[serde(default)]
    ddc_ok: bool,
    /// unix 秒。配对后用于卡时间窗。
    #[serde(default)]
    ts: u64,
    #[serde(default)]
    nonce: u64,
    #[serde(default)]
    action: String,
    #[serde(default)]
    args: serde_json::Value,
    /// 本机接的口名（usbc / dp1 ...），屏幕在本机时才能确定
    #[serde(default)]
    own_input: String,
    /// 该口对应的 VCP 0x60 取值
    #[serde(default)]
    own_value: u16,
    /// HMAC-SHA256 前 16 字节的 hex。未配对时为空。
    #[serde(default)]
    mac: String,
    /// 剪贴板文件按需传输的能力位：0/缺省 = 老版本，1 = 支持 clip_pull。
    /// **不进 canonical**：老端不认识这个字段也能验签通过。
    #[serde(default)]
    clip_v: u8,
    /// 多对端路由能力位：0/缺省 = 老版本，见 `KVM_VERSION`。
    /// **同样不进 canonical**，理由同上。
    #[serde(default)]
    kvm_v: u8,
    /// 对端的共用键鼠有没有开着。
    ///
    /// 只用来**说清楚话**：对端程序开着、但共用键鼠关着时，表现和对端
    /// 没开机一模一样（KVM 心跳不来 → `link_alive` 永远是 false），
    /// 原来一律报"对面没开程序、不在同一网段，或防火墙拦了"。
    /// **不进 canonical**：老端不认识这个字段也照样验签通过。
    #[serde(default)]
    kvm_on: bool,
}

impl Message {
    /// 参与签名的规范化消息体。
    ///
    /// 不用整段 JSON 的原因：字段顺序、空白、转义方式都可能随 serde 版本变，
    /// 两端只要有一处不一样就永远验不过。手写拼接虽然丑，但确定。
    fn canonical(&self) -> String {
        let args = serde_json::to_string(&self.args).unwrap_or_default();
        format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}",
            self.kind,
            self.node,
            self.ddc_ok as u8,
            self.ts,
            self.nonce,
            self.action,
            args,
            self.own_input,
            self.own_value
        )
    }
}

#[derive(Clone, Debug)]
pub struct Peer {
    pub node: String,
    pub addr: SocketAddr,
    pub last_seen: Instant,
    pub ddc_ok: bool,
    pub own_input: String,
    pub own_value: u16,
    /// 这条信息是验签过的（对端也在同一个配对里）
    pub paired: bool,
    /// 对端设了配对码。本机要是没设，我们发过去的指令会被它当未签名包丢掉 ——
    /// 扫描时"请对面切回来"就会毫无反应，所以这一位必须提前知道。
    pub wants_pair: bool,
    /// 对端的剪贴板文件传输能力（1 = 支持 clip_pull / 按需传文件）
    pub clip_v: u8,
    /// 对端的多对端路由能力（见 `KVM_VERSION`；< 2 = 不能加入多链路路由）
    pub kvm_v: u8,
    /// 对端的共用键鼠开着没有（只在上面那个 `kvm_on` 能力位存在时有意义）。
    /// 排除"对面没开机"之后，这是"键鼠交不出去"最常见的另一半原因。
    pub kvm_on: bool,
}

impl Peer {
    fn alive(&self) -> bool {
        self.last_seen.elapsed() < PEER_TIMEOUT
    }

    /// 对面那台到底挂在哪个口上。`own_value == 0` 表示它自己也不知道。
    pub fn knows_port(&self) -> bool {
        self.own_value > 0
    }
}

/// 收到对端指令时的回调：`(来源 node, action, args)`。
///
/// **为什么必须带来源**：配对密钥是全体共用一把，签过名的包任何一台都
/// 能发。只传 action 的话，接收方分不清"隔壁那台让我切屏"和"另一台
/// 让我切屏"—— 三台以上互连时日志里根本没法复盘是谁动的手。
pub type CommandHandler = Box<dyn Fn(String, String, serde_json::Value) + Send + Sync>;

/// 本机自己占用的口：(口名, VCP 取值)。0 表示还没确定。
type OwnPort = Arc<Mutex<(String, u16)>>;
type Key = Arc<Mutex<Option<[u8; 32]>>>;

#[derive(Clone)]
pub struct Mesh {
    node: String,
    port: u16,
    ddc_ok: bool,
    peers: Arc<Mutex<HashMap<String, Peer>>>,
    running: Arc<AtomicBool>,
    socket: Arc<Mutex<Option<Arc<UdpSocket>>>>,
    own: OwnPort,
    key: Key,
    /// 验签失败/时间戳过期的包数，界面上用来提示"有东西在敲你的门"
    rejected: Arc<AtomicU64>,
    /// 签名没问题但 nonce 已见过的包数。局域网里重发、多网卡重复收都会走到这里，
    /// 这类包**不代表**有人攻击，混在 `rejected` 里只会吓人。
    replayed: Arc<AtomicU64>,
    /// 本机共用键鼠开着没有。随 announce 发出去，让对端能把
    /// "它没开机" 和 "它开着但键鼠没开" 分开说。
    kvm_on: Arc<AtomicBool>,
}

impl Mesh {
    pub fn new(node: impl Into<String>, port: u16, ddc_ok: bool) -> Self {
        Self {
            node: node.into(),
            port,
            ddc_ok,
            peers: Arc::new(Mutex::new(HashMap::new())),
            running: Arc::new(AtomicBool::new(false)),
            socket: Arc::new(Mutex::new(None)),
            own: Arc::new(Mutex::new((String::new(), 0))),
            key: Arc::new(Mutex::new(None)),
            rejected: Arc::new(AtomicU64::new(0)),
            replayed: Arc::new(AtomicU64::new(0)),
            kvm_on: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 本机共用键鼠开着没有 —— 随下一条 announce 广播出去。
    ///
    /// 调用点在 `update_snapshot`：那里已经有 `kvm_now()` 的结论，
    /// 不管 KVM 是被开关、改链路还是改配对码重开的，都在那一处对齐，
    /// 不用在七八条启停路径上各写一遍（漏一条就是长期错误广播）。
    pub fn set_kvm_on(&self, on: bool) {
        self.kvm_on.store(on, Ordering::SeqCst);
    }

    /// 装上配对密钥。`None` = 关闭校验（旧行为，局域网里谁都能发指令）。
    pub fn set_key(&self, key: Option<[u8; 32]>) {
        *self.key.lock().unwrap() = key;
    }

    pub fn has_key(&self) -> bool {
        self.key.lock().unwrap().is_some()
    }

    pub fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::SeqCst)
    }

    /// 签名对、但重复收到的包数（重发 / 多网卡各收一份）
    pub fn replayed(&self) -> u64 {
        self.replayed.load(Ordering::SeqCst)
    }

    /// 更新"本机当前占用的口"。屏幕不在本机时可以传之前缓存的值。
    pub fn set_own(&self, input: &str, value: u16) {
        let mut own = self.own.lock().unwrap();
        if !input.is_empty() {
            own.0 = input.to_string();
        }
        if value > 0 {
            own.1 = value;
        }
    }

    pub fn own(&self) -> (String, u16) {
        self.own.lock().unwrap().clone()
    }

    pub fn start(&self, on_command: CommandHandler) -> std::io::Result<()> {
        let socket = UdpSocket::bind(("0.0.0.0", self.port))?;
        socket.set_broadcast(true)?;
        socket.set_read_timeout(Some(Duration::from_millis(500)))?;
        let socket = Arc::new(socket);
        *self.socket.lock().unwrap() = Some(socket.clone());
        self.running.store(true, Ordering::SeqCst);

        // 接收线程
        {
            let socket = socket.clone();
            let running = self.running.clone();
            let peers = self.peers.clone();
            let me = self.node.clone();
            let key = self.key.clone();
            let rejected = self.rejected.clone();
            let replayed = self.replayed.clone();
            thread::spawn(move || {
                let mut buf = [0u8; 65536];
                let mut nonces = NonceCache::default();
                while running.load(Ordering::SeqCst) {
                    let Ok((n, addr)) = socket.recv_from(&mut buf) else {
                        continue;
                    };
                    let Ok(msg) = serde_json::from_slice::<Message>(&buf[..n]) else {
                        continue;
                    };
                    if msg.v != PROTOCOL_VERSION || msg.node == me {
                        continue;
                    }
                    // 配对开着就只认签过名的消息
                    // 对面带没带签名，是"本机没配对时它会不会丢弃我们的指令"的唯一线索
                    let wants_pair = !msg.mac.is_empty();
                    let paired = match *key.lock().unwrap() {
                        None => false,
                        Some(k) => {
                            let now = now_secs();
                            let fresh = msg.ts.abs_diff(now) <= pair::TIME_WINDOW_SECS;
                            let signed = fresh && pair::verify(&k, &msg.canonical(), &msg.mac);
                            if !signed {
                                // 签名对不上 / 时间戳偏太多：这才是"有人在敲门"
                                rejected.fetch_add(1, Ordering::SeqCst);
                                continue;
                            }
                            if !nonces.accept(msg.nonce) {
                                // 签名是对的，只是这包收过一遍 —— 局域网重发，不是攻击
                                replayed.fetch_add(1, Ordering::SeqCst);
                                continue;
                            }
                            true
                        }
                    };
                    match msg.kind.as_str() {
                        "announce" => {
                            let mut table = peers.lock().unwrap();
                            // 对面的口信息可能这一帧没带（屏幕在别处），
                            // 这时保留上一次已知的值，不要清掉
                            let previous = table.get(&msg.node).cloned();
                            let (own_input, own_value) = if msg.own_value > 0 {
                                (msg.own_input, msg.own_value)
                            } else if let Some(prev) = previous {
                                (prev.own_input, prev.own_value)
                            } else {
                                (String::new(), 0)
                            };
                            table.insert(
                                msg.node.clone(),
                                Peer {
                                    node: msg.node,
                                    addr: SocketAddr::new(addr.ip(), 0),
                                    last_seen: Instant::now(),
                                    ddc_ok: msg.ddc_ok,
                                    own_input,
                                    own_value,
                                    paired,
                                    wants_pair,
                                    clip_v: msg.clip_v,
                                    kvm_v: msg.kvm_v,
                                    kvm_on: msg.kvm_on,
                                },
                            );
                        }
                        "command" => on_command(msg.node.clone(), msg.action.clone(), msg.args.clone()),
                        _ => {}
                    }
                }
            });
        }

        // 广播线程
        {
            let socket = socket.clone();
            let running = self.running.clone();
            let me = self.node.clone();
            let port = self.port;
            let ddc_ok = self.ddc_ok;
            let own = self.own.clone();
            let key = self.key.clone();
            let kvm_on = self.kvm_on.clone();
            thread::spawn(move || {
                while running.load(Ordering::SeqCst) {
                    let (own_input, own_value) = own.lock().unwrap().clone();
                    let mut msg = Message {
                        v: PROTOCOL_VERSION,
                        kind: "announce".into(),
                        node: me.clone(),
                        ddc_ok,
                        ts: now_secs(),
                        nonce: now_nonce(),
                        action: String::new(),
                        args: serde_json::Value::Null,
                        own_input,
                        own_value,
                        mac: String::new(),
                        clip_v: CLIP_VERSION,
                        kvm_v: KVM_VERSION,
                        kvm_on: kvm_on.load(Ordering::SeqCst),
                    };
                    if let Some(k) = *key.lock().unwrap() {
                        msg.mac = pair::sign(&k, &msg.canonical());
                    }
                    if let Ok(payload) = serde_json::to_vec(&msg) {
                        let _ = socket.send_to(&payload, ("255.255.255.255", port));
                        if let Some(broadcast) = subnet_broadcast() {
                            let _ = socket.send_to(&payload, (broadcast, port));
                        }
                    }
                    thread::sleep(ANNOUNCE_INTERVAL);
                }
            });
        }

        Ok(())
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    pub fn live_peers(&self) -> Vec<Peer> {
        let mut table = self.peers.lock().unwrap();
        // 只增不删的表在 5 台互连时会一直残留早已关机的机器：8 秒超时
        // 只是让它们"不可见"，条目本身还在，`send_command` 照样能往里
        // 写字。留 10 倍超时再清，避免 DHCP 换 IP / 短暂掉线反复进表。
        table.retain(|_, p| p.last_seen.elapsed() < PEER_TIMEOUT * 10);
        let mut list: Vec<Peer> = table.values().filter(|p| p.alive()).cloned().collect();
        // 最近说话的排前面（对端列表按新鲜度倒序）
        list.sort_by_key(|p| std::cmp::Reverse(p.last_seen));
        list
    }

    pub fn find_ddc_capable(&self) -> Option<Peer> {
        self.live_peers().into_iter().find(|p| p.ddc_ok)
    }

    /// 给某台对端发一条指令。
    ///
    /// 返回 `Err` 意味着对面**一定**收不到，调用方不要再报成功。
    ///
    /// 以前这里返回 `bool`，判据是"`send_to()` 没报错"—— 而 UDP 的 `send_to`
    /// 只要内核收下了就返回 Ok，对面收不收、丢不丢它根本不知道。最典型的
    /// 假成功是"对端设了配对码、本机没设"：包发得出去，对面在收包处验签
    /// 失败直接 `continue`，界面却报"已请对端把画面切到 0x..，几秒后屏幕会回来"。
    /// `Peer.wants_pair` 早就知道这件事，之前只拿去喂面板提示，现在拿来拦。
    pub fn send_command(
        &self,
        node: &str,
        action: &str,
        args: serde_json::Value,
    ) -> Result<(), String> {
        let Some(peer) = self.peers.lock().unwrap().get(node).cloned() else {
            return Err(format!("{node} 不在对端表里（它没在广播，或者还没被发现）"));
        };
        // 表里的条目要留到 10 倍超时才清（见 `live_peers`），所以"查得到" ≠ "还活着"
        if !peer.alive() {
            return Err(format!("{node} 已经掉线，这条指令发过去没人收"));
        }
        // ⚠️ 这道守卫必须放在取 socket 之前：它跟网络状态无关，放到后面就会被
        // "监听没起来"掩盖掉，也没法在单测里覆盖（单测不起真 socket）。
        if self.key.lock().unwrap().is_none() && peer.wants_pair {
            return Err(format!(
                "{node} 设了配对码而本机没设 —— 它会把这条指令当未签名包丢掉，请两边填同一个码"
            ));
        }
        let socket = self.socket.lock().unwrap().clone();
        let Some(socket) = socket else {
            return Err("局域网监听没起来".to_string());
        };
        let (own_input, own_value) = self.own();
        let mut msg = Message {
            v: PROTOCOL_VERSION,
            kind: "command".into(),
            node: self.node.clone(),
            ddc_ok: self.ddc_ok,
            ts: now_secs(),
            nonce: now_nonce(),
            action: action.to_string(),
            args,
            own_input,
            own_value,
            mac: String::new(),
            clip_v: CLIP_VERSION,
            kvm_v: KVM_VERSION,
            // 指令包里这个位没用：对端只按 announce 更新
            kvm_on: self.kvm_on.load(Ordering::SeqCst),
        };
        if let Some(k) = *self.key.lock().unwrap() {
            msg.mac = pair::sign(&k, &msg.canonical());
        }
        let payload = serde_json::to_vec(&msg).map_err(|e| format!("消息打包失败：{e}"))?;
        let target = SocketAddr::new(peer.addr.ip(), self.port);
        socket
            .send_to(&payload, target)
            .map(|_| ())
            .map_err(|e| format!("发不出去：{e}"))
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 单调递增的 nonce。取纳秒，同一进程内几乎不可能重复；
/// 跨重启重复也无所谓，因为时间窗是分开判的。
fn now_nonce() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    nanos ^ COUNTER.fetch_add(1, Ordering::SeqCst).wrapping_mul(0x9E37_79B9)
}

/// 从默认路由猜一个子网广播地址。拿不到就只靠 255.255.255.255。
fn subnet_broadcast() -> Option<String> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let ip = socket.local_addr().ok()?.ip().to_string();
    let octets: Vec<&str> = ip.split('.').collect();
    if octets.len() == 4 {
        Some(format!("{}.{}.{}.255", octets[0], octets[1], octets[2]))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 往私有对端表里塞一台，不走网络。`Mesh::new` 不 bind 端口，所以这些
    /// 单测不需要真 socket —— 这也正是把"配对守卫"排在取 socket 之前的原因。
    fn with_peer(mesh: &Mesh, wants_pair: bool, seen_ago: Duration) {
        let peer = Peer {
            node: "peer-a".into(),
            addr: "192.0.2.7:0".parse().unwrap(),
            last_seen: Instant::now() - seen_ago,
            ddc_ok: true,
            own_input: String::new(),
            own_value: 0,
            paired: false,
            wants_pair,
            clip_v: CLIP_VERSION,
            kvm_v: KVM_VERSION,
            kvm_on: false,
        };
        mesh.peers.lock().unwrap().insert(peer.node.clone(), peer);
    }

    /// 本机没码、对端要码：包发得出去但对面一定丢，必须报错而不是报成功。
    #[test]
    fn refuses_command_when_peer_wants_pair() {
        let mesh = Mesh::new("me", 47831, false);
        with_peer(&mesh, true, Duration::from_secs(1));
        let err = mesh
            .send_command("peer-a", "switch", serde_json::Value::Null)
            .unwrap_err();
        assert!(err.contains("配对码"), "{err}");
    }

    /// 表里的条目要留到 10 倍超时才清，"查得到"不等于"还活着"。
    #[test]
    fn refuses_command_to_dead_peer() {
        let mesh = Mesh::new("me", 47831, false);
        with_peer(&mesh, false, Duration::from_secs(30));
        let err = mesh
            .send_command("peer-a", "switch", serde_json::Value::Null)
            .unwrap_err();
        assert!(err.contains("掉线"), "{err}");
    }

    /// 本机也设了码：配对这一步没理由拦，应该走到"监听没起来"。
    #[test]
    fn passes_pair_guard_when_key_set() {
        let mesh = Mesh::new("me", 47831, false);
        mesh.set_key(pair::link_key("123456"));
        with_peer(&mesh, true, Duration::from_secs(1));
        let err = mesh
            .send_command("peer-a", "switch", serde_json::Value::Null)
            .unwrap_err();
        assert!(err.contains("监听"), "{err}");
        assert!(!err.contains("配对码"), "守卫拦错了：{err}");
    }

    #[test]
    fn refuses_command_to_unknown_peer() {
        let mesh = Mesh::new("me", 47831, false);
        let err = mesh
            .send_command("没这台", "switch", serde_json::Value::Null)
            .unwrap_err();
        assert!(err.contains("不在对端表里"), "{err}");
    }
}
