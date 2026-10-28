//! 传输任务注册表：进程级状态 + 每个任务的控制手柄。
//!
//! 为什么在这：面板的「文件传输」页要一张清单（进行中 + 历史），而 `send()`
//! 是同步阻塞的，进度只能从它的 `progress` 回调里出来。注册表放在这里而不是
//! 放在调用方，是因为三个调用点（面板手动发送 / 剪贴板按需传输 / 测试文件）
//! 共用一套语义；暂停/取消的标志也由任务自己持有，`send()` 每个块之间读一次。
//!
//! 和 `update` 模块同一个模式（见 `update/mod.rs` 的 `state()`）：进程级
//! `Mutex`，面板线程只读快照（`views()`），传输线程只写自己那一条。
//!
//! 只跟踪**本机发起**的发送：设计稿的传输列表是发送视角（「发往 XX」），
//! 接收方的落盘反馈走 `on_saved` → 状态行，不进这张表。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// 历史容量。超出后**只淘汰最老的已完成条目** —— 进行中的任务一条不丢
/// （并发发送是用户自己点出来的，丢一条进行中的任务等于让那个线程白跑）。
const CAP: usize = 50;

/// 暂停时每隔多久写一个空数据帧保活。
///
/// 接收侧读超时是 60s（`serve` 里 `set_read_timeout`）：暂停期间一个字节都
/// 不发，超过 60s 对端的 `read_frame` 就超时失败、整批作废。空 data 帧对
/// 接收侧是 0 字节数据（`take = min(0, left) = 0`，写 0 字节、哈希不变），
/// 天然无害 —— 老版本对端也吃得下，不需要改协议。
pub const KEEPALIVE: std::time::Duration = std::time::Duration::from_secs(45);

/// 任务阶段。和面板上的筛选组（全部 / 进行中 / 已完成）一一对应：
/// `sending` / `paused` 算进行中，其余算已完成。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Sending,
    Paused,
    Done,
    Failed,
    Cancelled,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Sending => "sending",
            Phase::Paused => "paused",
            Phase::Done => "done",
            Phase::Failed => "failed",
            Phase::Cancelled => "cancelled",
        }
    }

    /// 终态 = 不再会有进度更新。
    pub fn finished(self) -> bool {
        !matches!(self, Phase::Sending | Phase::Paused)
    }
}

/// 一条任务在面板上的样子。序列化名即前端约定，新增字段前端不认也不影响旧页面。
/// `Deserialize` 也 derive：`Snapshot` 整体要 round-trip（panel.rs 的测试盯着）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskView {
    pub id: u64,
    /// 对端节点名（「发往 XX」那半个文件名）
    pub peer: String,
    /// 显示名：单个文件是文件名，一批是「N 个文件」
    pub name: String,
    /// 总字节数。连接建立前是 0（`collect` 在 `send()` 里面做）
    pub total: u64,
    pub sent: u64,
    /// sending / paused / done / failed / cancelled
    pub phase: String,
    /// failed 时的原因。其余阶段是空串
    pub error: String,
    /// Unix 秒
    pub started_at: u64,
}

/// 内部任务。比 `TaskView` 多两样不该出面板的东西：重试用的原始清单和控制标志。
#[derive(Clone, Debug)]
pub struct Task {
    pub id: u64,
    pub peer: String,
    pub name: String,
    pub total: u64,
    pub sent: u64,
    pub phase: Phase,
    pub error: String,
    pub started_at: u64,
    pub paths: Vec<PathBuf>,
    pub cancel: Arc<AtomicBool>,
    pub paused: Arc<AtomicBool>,
}

/// 交给 `send()` 的控制手柄。`None` = CLI 这类不需要控制的调用方。
#[derive(Clone, Default)]
pub struct Control {
    pub cancel: Arc<AtomicBool>,
    pub paused: Arc<AtomicBool>,
}

fn tasks() -> &'static Mutex<Vec<Task>> {
    static T: OnceLock<Mutex<Vec<Task>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(Vec::new()))
}

/// 中毒也照样拿锁：锁只会被 panic 的写方毒到，里面的数据还是好的，
/// 为一次 panic 让整个传输列表永久报废不值当。
fn lock() -> MutexGuard<'static, Vec<Task>> {
    tasks().lock().unwrap_or_else(|e| e.into_inner())
}

/// 显示名：单个文件用文件名（用户认得出），一批用个数。
pub fn name_of(files: &[PathBuf]) -> String {
    match files.len() {
        1 => files[0]
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "1 个文件".to_string()),
        n => format!("{n} 个文件"),
    }
}

/// 登记一条新任务，返回 id 和控制手柄。新的排在最前面（面板列表顺序）。
pub fn register(peer: &str, name: &str, paths: Vec<PathBuf>) -> (u64, Control) {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let ctrl = Control::default();
    let task = Task {
        id,
        peer: peer.to_string(),
        name: name.to_string(),
        total: 0,
        sent: 0,
        phase: Phase::Sending,
        error: String::new(),
        started_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        paths,
        cancel: ctrl.cancel.clone(),
        paused: ctrl.paused.clone(),
    };
    let mut tasks = lock();
    // 环形淘汰：只丢最老的已完成条目。全是进行中时不丢（见 CAP 的注释）
    if tasks.len() >= CAP {
        if let Some(pos) = tasks.iter().rposition(|t| t.phase.finished()) {
            tasks.remove(pos);
        }
    }
    tasks.insert(0, task);
    (id, ctrl)
}

/// 进度更新。第一次回调时把总量补上（`collect` 在 `send()` 内部，
/// 登记那一刻还不知道总大小）。
pub fn progress(id: u64, sent: u64, total: u64) {
    let mut tasks = lock();
    if let Some(t) = tasks.iter_mut().find(|t| t.id == id) {
        if matches!(t.phase, Phase::Sending | Phase::Paused) {
            t.sent = sent;
            if t.total == 0 && total > 0 {
                t.total = total;
            }
        }
    }
}

/// 终态。只有进行中的任务会被改写 —— 已经终态的任务（比如用户先点了取消、
/// 传输线程随后才返回）保持用户的结论。
pub fn finish(id: u64, phase: Phase, error: &str) {
    let mut tasks = lock();
    if let Some(t) = tasks.iter_mut().find(|t| t.id == id) {
        if matches!(t.phase, Phase::Sending | Phase::Paused) {
            t.phase = phase;
            t.error = error.to_string();
        }
    }
}

/// 暂停 / 继续。阶段立刻反映到列表，不等下一轮 progress（暂停期间没有 progress）。
/// 返回 false = 没有这条进行中的任务（面板入口校验用）。
pub fn set_paused(id: u64, paused: bool) -> bool {
    let mut tasks = lock();
    match tasks.iter_mut().find(|t| t.id == id) {
        Some(t) if matches!(t.phase, Phase::Sending | Phase::Paused) => {
            t.paused.store(paused, Ordering::SeqCst);
            t.phase = if paused { Phase::Paused } else { Phase::Sending };
            true
        }
        _ => false,
    }
}

/// 取消。只置标志：`send()` 块间读到就退出并走 `CANCELLED` 路径，
/// 阶段由 `finish` 置。暂停中取消也最多 200ms 内退出（见 `KEEPALIVE` 循环）。
pub fn cancel(id: u64) -> bool {
    let tasks = lock();
    match tasks.iter().find(|t| t.id == id) {
        Some(t) if matches!(t.phase, Phase::Sending | Phase::Paused) => {
            t.cancel.store(true, Ordering::SeqCst);
            true
        }
        _ => false,
    }
}

/// 重试：把一条失败/取消的任务翻回 sending，**复用同一条任务和同一个控制
/// 手柄**。新起一条的话，旧任务会永远停在终态、新任务又长得一样，历史里
/// 平白多一条。返回 None = 不在册或不是可重试的终态。
pub fn begin_retry(id: u64) -> Option<Control> {
    let mut tasks = lock();
    let t = tasks.iter_mut().find(|t| t.id == id)?;
    if !matches!(t.phase, Phase::Failed | Phase::Cancelled) {
        return None;
    }
    t.phase = Phase::Sending;
    t.sent = 0;
    t.error.clear();
    t.cancel.store(false, Ordering::SeqCst);
    t.paused.store(false, Ordering::SeqCst);
    Some(Control {
        cancel: t.cancel.clone(),
        paused: t.paused.clone(),
    })
}

/// 重试要用的东西：文件清单 + 对端名。
pub fn retry_info(id: u64) -> Option<(Vec<PathBuf>, String)> {
    let tasks = lock();
    tasks
        .iter()
        .find(|t| t.id == id)
        .map(|t| (t.paths.clone(), t.peer.clone()))
}

/// 清掉已完成的历史（面板「清空记录」）。进行中的不动。
pub fn clear_finished() {
    lock().retain(|t| !t.phase.finished());
}

/// 面板快照：克隆一份出面板。400ms 一次，开销忽略不计。
pub fn views() -> Vec<TaskView> {
    lock()
        .iter()
        .map(|t| TaskView {
            id: t.id,
            peer: t.peer.clone(),
            name: t.name.clone(),
            total: t.total,
            sent: t.sent,
            phase: t.phase.as_str().to_string(),
            error: t.error.clone(),
            started_at: t.started_at,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn register_progress_finish_round_trip() {
        let (id, ctrl) = register("peer-a", "a.txt", files(&["a.txt"]));
        assert!(!ctrl.cancel.load(Ordering::SeqCst));
        progress(id, 0, 1000); // 首帧补总量
        progress(id, 500, 1000);
        let v = views();
        let mine = v.iter().find(|t| t.id == id).unwrap();
        assert_eq!((mine.total, mine.sent), (1000, 500));
        assert_eq!(mine.phase, "sending");
        finish(id, Phase::Done, "");
        let v = views();
        let mine = v.iter().find(|t| t.id == id).unwrap();
        assert_eq!(mine.phase, "done");
    }

    #[test]
    fn ring_evicts_only_finished() {
        // 先塞满进行中的任务，再注册新的：一条都不该被淘汰
        let ids: Vec<u64> = (0..CAP).map(|i| register("p", &format!("f{i}"), files(&["f"])).0).collect();
        let (newbie, _) = register("p", "newbie", files(&["f"]));
        let v = views();
        assert_eq!(v.len(), CAP + 1, "进行中的任务不该被淘汰");
        assert!(v.iter().any(|t| t.id == newbie));
        assert!(ids.iter().all(|id| v.iter().any(|t| t.id == *id)));
        // 全部完成后注册新的：最老的那条（ids[0]）让位
        for id in &ids {
            finish(*id, Phase::Done, "");
        }
        let (newbie2, _) = register("p", "newbie2", files(&["f"]));
        let v = views();
        assert_eq!(v.len(), CAP + 1);
        assert!(!v.iter().any(|t| t.id == ids[0]), "最老的已完成条目应被淘汰");
        assert!(v.iter().any(|t| t.id == newbie2));
        clear_finished();
        assert_eq!(views().len(), 2, "只剩两条进行中的");
    }

    #[test]
    fn pause_cancel_retry_transitions() {
        let (id, ctrl) = register("p", "x", files(&["x"]));
        assert!(set_paused(id, true));
        assert!(ctrl.paused.load(Ordering::SeqCst));
        assert_eq!(views().iter().find(|t| t.id == id).unwrap().phase, "paused");
        assert!(set_paused(id, false));
        assert_eq!(views().iter().find(|t| t.id == id).unwrap().phase, "sending");
        // 终态任务不再接受控制
        finish(id, Phase::Failed, "boom");
        assert!(!set_paused(id, true));
        assert!(!cancel(id));
        assert!(!ctrl.cancel.load(Ordering::SeqCst));
        // 重试复用同一条任务和同一个手柄
        let again = begin_retry(id).expect("failed 可重试");
        assert!(!again.cancel.load(Ordering::SeqCst));
        assert_eq!(views().iter().find(|t| t.id == id).unwrap().phase, "sending");
        // 进行中不可重试
        assert!(begin_retry(id).is_none());
        // 取消只置标志，阶段等 finish
        assert!(cancel(id));
        assert!(ctrl.cancel.load(Ordering::SeqCst));
        finish(id, Phase::Cancelled, "");
        assert_eq!(views().iter().find(|t| t.id == id).unwrap().phase, "cancelled");
    }

    #[test]
    fn unknown_ids_are_noops() {
        progress(999_999, 1, 1);
        finish(999_999, Phase::Done, "");
        assert!(!set_paused(999_999, true));
        assert!(!cancel(999_999));
        assert!(begin_retry(999_999).is_none());
        assert!(retry_info(999_999).is_none());
    }
}
