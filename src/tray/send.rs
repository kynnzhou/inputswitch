//! 发文件：选文件 → 传给点名的对端。
//!
//! 系统文件选择器是阻塞的，所以连"弹对话框"一起放后台线程 —— 挂在 UI 线程
//! 上会让菜单和面板在整个选择过程中都没有响应。
//!
//! 每个任务在 `transfer::tasks` 注册表里登记一条（id + 控制手柄），
//! `send()` 的 progress 回调同时喂状态行和注册表；暂停/取消/重试都通过
//! 注册表操作，不直接碰传输线程。

use std::path::PathBuf;
use std::thread;

use tao::event_loop::EventLoopProxy;

use crate::config::Config;
use crate::peer::{Mesh, Peer};
use crate::transfer::{self, tasks};

use super::kvm_now;
use super::UserEvent;

/// 调系统文件选择器。浏览器给不了本地绝对路径，只能走系统对话框。
fn pick_files() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let script = "set out to \"\"\nrepeat with f in (choose file with multiple selections allowed)\nset out to out & (POSIX path of f) & linefeed\nend repeat\nreturn out";
        let output = std::process::Command::new("/usr/bin/osascript")
            .args(["-e", script])
            .output();
        // 三个平台块各自以 match 收尾，块的最后一个表达式就是函数返回值，
        // 所以这里不写 return（clippy::needless_return）——写成 return 的
        // 那个平台在编译时恰好是最后一块，就会被告。
        match output {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(|line| line.trim())
                .filter(|line| !line.is_empty())
                .map(PathBuf::from)
                .collect(),
            _ => Vec::new(),
        }
    }
    #[cfg(windows)]
    {
        let script = "Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.OpenFileDialog; $d.Multiselect = $true; if ($d.ShowDialog() -eq 'OK') { $d.FileNames }";
        let output = std::process::Command::new("powershell")
            .args(["-NoProfile", "-STA", "-Command", script])
            .output();
        // 三个平台块各自以 match 收尾，块的最后一个表达式就是函数返回值，
        // 所以这里不写 return（clippy::needless_return）——写成 return 的
        // 那个平台在编译时恰好是最后一块，就会被告。
        match output {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(|line| line.trim())
                .filter(|line| !line.is_empty())
                .map(PathBuf::from)
                .collect(),
            _ => Vec::new(),
        }
    }
    #[cfg(target_os = "linux")]
    {
        let output = std::process::Command::new("zenity")
            .args(["--file-selection", "--multiple", "--separator", "\n"])
            .output();
        // 三个平台块各自以 match 收尾，块的最后一个表达式就是函数返回值，
        // 所以这里不写 return（clippy::needless_return）——写成 return 的
        // 那个平台在编译时恰好是最后一块，就会被告。
        match output {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(|line| line.trim())
                .filter(|line| !line.is_empty())
                .map(PathBuf::from)
                .collect(),
            _ => Vec::new(),
        }
    }
}

pub(super) fn pick_folder() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("/usr/bin/osascript")
            .args(["-e", "POSIX path of (choose folder)"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if path.is_empty() {
            None
        } else {
            Some(PathBuf::from(path))
        }
    }
    #[cfg(windows)]
    {
        let script = "Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.FolderBrowserDialog; if ($d.ShowDialog() -eq 'OK') { $d.SelectedPath }";
        let output = std::process::Command::new("powershell")
            .args(["-NoProfile", "-STA", "-Command", script])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if path.is_empty() {
            None
        } else {
            Some(PathBuf::from(path))
        }
    }
    #[cfg(target_os = "linux")]
    {
        let output = std::process::Command::new("zenity")
            .args(["--file-selection", "--directory"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (!path.is_empty()).then(|| PathBuf::from(path))
    }
}

/// 选路：点名的 > 现在握着键鼠的那台 > 只有一台在线时就是它。
/// 多台又没会话 = 拒绝，错误文案说清"要怎么点"。
///
/// 原来是死写 `peers.first()`：一机四台时文件必然发给排序第一的那台，
/// 而且界面上没有任何地方能看出"到底发给谁了"。
fn resolve_peer(mesh: &Mesh, target: Option<&str>) -> Result<Peer, String> {
    let peers = mesh.live_peers();
    if peers.is_empty() {
        return Err("局域网里没找到对端，先让对面那台也开着本程序".to_string());
    }
    let wanted = target
        .map(str::to_string)
        .or_else(|| kvm_now().and_then(|h| h.active_node().map(str::to_string)));
    match wanted.as_deref() {
        Some(node) => match peers.iter().find(|p| p.node == node).cloned() {
            Some(peer) => Ok(peer),
            None => Err(format!("{node} 没有回应（不在线或不在同一网段），文件没有发出去")),
        },
        None if peers.len() == 1 => Ok(peers[0].clone()),
        None => {
            let names = peers
                .iter()
                .map(|p| p.node.as_str())
                .collect::<Vec<_>>()
                .join("、");
            Err(format!(
                "有 {} 台对端在线（{names}）：先把键鼠交给要发的那台，再点「发送文件」",
                peers.len()
            ))
        }
    }
}

/// 起一条传输任务并登记进注册表。返回任务 id（调用方一般不需要）。
/// `cleanup` = 发送结束后要删掉的文件（测试文件的探针路径）。
fn spawn_send(
    config: &Config,
    proxy: &EventLoopProxy<UserEvent>,
    peer: &Peer,
    files: Vec<PathBuf>,
    clip: bool,
    label: Option<String>,
    cleanup: Option<PathBuf>,
) -> u64 {
    let name = label.unwrap_or_else(|| tasks::name_of(&files));
    let (id, ctrl) = tasks::register(&peer.node, &name, files.clone());
    run_send(
        config,
        proxy,
        peer,
        SendJob {
            files,
            clip,
            id,
            ctrl,
            name,
            cleanup,
        },
    );
    id
}

/// 一次传输的全部参数。收成一个结构体而不是一排 & 参数：注册表 id、
/// 控制手柄、显示名、探针清理路径本来就是一件事的四个侧面。
struct SendJob {
    files: Vec<PathBuf>,
    clip: bool,
    id: u64,
    ctrl: tasks::Control,
    name: String,
    cleanup: Option<PathBuf>,
}

/// 传输线程本体。注册（`spawn_send`）或重试（`begin_retry`）之后调这里。
fn run_send(config: &Config, proxy: &EventLoopProxy<UserEvent>, peer: &Peer, job: SendJob) {
    let SendJob {
        files,
        clip,
        id,
        ctrl,
        name,
        cleanup,
    } = job;
    let key = crate::pair::link_key(&config.pair.code);
    let node = config.node.clone();
    let addr = std::net::SocketAddr::new(peer.addr.ip(), config.transfer.port);
    let peer_name = peer.node.clone();
    let proxy_ui = proxy.clone();
    let proxy_progress = proxy.clone();
    thread::spawn(move || {
        // 256KB 一个包，1GB 文件能回调四千次；只在百分比变化时才通知 UI
        let mut last_pct = u64::MAX;
        let result = transfer::send(
            addr,
            &node,
            &files,
            key,
            clip,
            Some(&ctrl),
            |sent, total| {
                // 注册表和状态行是同一份进度的两个消费者
                tasks::progress(id, sent, total);
                // 同 main.rs：checked_div 一并防住除零与溢出
                let pct = sent
                    .checked_mul(100)
                    .and_then(|num| num.checked_div(total))
                    .unwrap_or(100);
                if pct != last_pct {
                    last_pct = pct;
                    let _ =
                        proxy_progress.send_event(UserEvent::Transfer(format!("正在发送… {pct}%")));
                }
            },
        );
        let note = match &result {
            Ok(message) => {
                tasks::finish(id, tasks::Phase::Done, "");
                format!("{message} → {peer_name}")
            }
            Err(err) if err == transfer::CANCELLED => {
                tasks::finish(id, tasks::Phase::Cancelled, "");
                format!("已取消发送 {name}")
            }
            Err(err) => {
                tasks::finish(id, tasks::Phase::Failed, err);
                format!("发送失败：{err}")
            }
        };
        // 探针文件发完就删，不管成败
        if let Some(path) = cleanup {
            let _ = std::fs::remove_file(path);
        }
        let _ = proxy_ui.send_event(UserEvent::Transfer(note));
    });
}

/// 选文件并发给对端（面板「发送文件…」）。
pub(super) fn start_send(
    config: &Config,
    mesh: &Mesh,
    proxy: &EventLoopProxy<UserEvent>,
    status: &mut String,
    target: Option<&str>,
) {
    let peer = match resolve_peer(mesh, target) {
        Ok(peer) => peer,
        Err(err) => {
            *status = err;
            return;
        }
    };
    *status = "正在选择文件…".to_string();
    let config = config.clone();
    let proxy = proxy.clone();
    thread::spawn(move || {
        let files = pick_files();
        if files.is_empty() {
            let _ = proxy.send_event(UserEvent::Transfer(String::new()));
            return;
        }
        spawn_send(&config, &proxy, &peer, files, false, None, None);
    });
}

/// 选文件夹发给对端（面板「选择文件夹」）。目录在 `collect()` 里递归展开。
pub(super) fn start_send_folder(
    config: &Config,
    mesh: &Mesh,
    proxy: &EventLoopProxy<UserEvent>,
    status: &mut String,
    target: Option<&str>,
) {
    let peer = match resolve_peer(mesh, target) {
        Ok(peer) => peer,
        Err(err) => {
            *status = err;
            return;
        }
    };
    *status = "正在选择文件夹…".to_string();
    let config = config.clone();
    let proxy = proxy.clone();
    thread::spawn(move || {
        let Some(dir) = pick_folder() else {
            let _ = proxy.send_event(UserEvent::Transfer(String::new()));
            return;
        };
        spawn_send(&config, &proxy, &peer, vec![dir], false, None, None);
    });
}

/// 发一个临时生成的测试文件（面板「发送测试文件」）：两台之间到底通不通，
/// 不用真去找个文件来试。生成 → 发送 → 删除，不留垃圾。
pub(super) fn start_send_test(
    config: &Config,
    mesh: &Mesh,
    proxy: &EventLoopProxy<UserEvent>,
    status: &mut String,
    target: Option<&str>,
) {
    let peer = match resolve_peer(mesh, target) {
        Ok(peer) => peer,
        Err(err) => {
            *status = err;
            return;
        }
    };
    let node = config.node.clone();
    *status = "正在发送测试文件…".to_string();
    let config = config.clone();
    let proxy = proxy.clone();
    thread::spawn(move || {
        let Some(path) = make_test_file(&node) else {
            let _ = proxy.send_event(UserEvent::Transfer("建不了临时测试文件".to_string()));
            return;
        };
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "测试文件".to_string());
        // cleanup = 同一个路径：发送结束（无论成败）后由传输线程删掉
        spawn_send(
            &config,
            &proxy,
            &peer,
            vec![path.clone()],
            false,
            Some(name),
            Some(path),
        );
    });
}

/// 重试一条失败/取消的任务：复用注册表里存的原始清单和同一个控制手柄，
/// 不开选择器。对端不在线就明说，任务保持终态不动。
pub(super) fn retry_send(
    config: &Config,
    mesh: &Mesh,
    proxy: &EventLoopProxy<UserEvent>,
    status: &mut String,
    id: u64,
) {
    let Some((paths, node)) = tasks::retry_info(id) else {
        *status = "没有这条传输记录".to_string();
        return;
    };
    if mesh.live_peers().iter().all(|p| p.node != node) {
        *status = format!("{node} 不在线，重试没有发出去");
        return;
    }
    let Some(ctrl) = tasks::begin_retry(id) else {
        *status = "这条记录现在不能重试".to_string();
        return;
    };
    // 在线性上面已经验过，这里只为拿地址
    let peer = match resolve_peer(mesh, Some(&node)) {
        Ok(peer) => peer,
        Err(err) => {
            *status = err;
            return;
        }
    };
    let name = tasks::name_of(&paths);
    *status = format!("正在重试 {name}…").to_string();
    run_send(
        config,
        proxy,
        &peer,
        SendJob {
            files: paths,
            clip: false,
            id,
            ctrl,
            name,
            cleanup: None,
        },
    );
}

/// 生成临时测试文件。内容是给人看的：收到什么、谁发的、什么时候。
fn make_test_file(node: &str) -> Option<PathBuf> {
    let ts = crate::peer::now_secs();
    let path = std::env::temp_dir().join(format!("InputSwitch-传输测试-{ts}.txt"));
    let body = format!(
        "InputSwitch 传输测试\n节点：{node}\n时间：{ts}\n\n如果你能看到这个文件，说明两台电脑之间的文件传输是通的。\n"
    );
    std::fs::write(&path, body).ok()?;
    Some(path)
}
