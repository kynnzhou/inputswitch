//! 发文件：选文件 → 传给点名的对端。
//!
//! 系统文件选择器是阻塞的，所以连"弹对话框"一起放后台线程 —— 挂在 UI 线程
//! 上会让菜单和面板在整个选择过程中都没有响应。

use std::path::PathBuf;
use std::thread;

use tao::event_loop::EventLoopProxy;

use crate::config::Config;
use crate::peer::Mesh;
use crate::transfer;

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

/// 选文件并发给对端。
///
/// 连"弹文件选择器"一起放到后台线程：系统对话框是阻塞的，
/// 挂在 UI 线程上会让菜单和面板在整个选择过程中都没有响应。
pub(super) fn start_send(
    config: &Config,
    mesh: &Mesh,
    proxy: &EventLoopProxy<UserEvent>,
    status: &mut String,
    target: Option<&str>,
) {
    let peers = mesh.live_peers();
    if peers.is_empty() {
        *status = "局域网里没找到对端，先让对面那台也开着本程序".to_string();
        return;
    }
    // **发给谁**：点名的 > 现在握着键鼠的那台 > 只有一台在线时就是它。
    //
    // 原来是死写 `peers.first()`：一机四台时文件必然发给排序第一的那台，
    // 而且界面上没有任何地方能看出"到底发给谁了"。
    let wanted = target
        .map(str::to_string)
        .or_else(|| kvm_now().and_then(|h| h.active_node().map(str::to_string)));
    let peer = match wanted.as_deref() {
        Some(node) => match peers.iter().find(|p| p.node == node).cloned() {
            Some(peer) => peer,
            None => {
                *status = format!("{node} 没有回应（不在线或不在同一网段），文件没有发出去");
                return;
            }
        },
        None if peers.len() == 1 => peers[0].clone(),
        None => {
            let names = peers
                .iter()
                .map(|p| p.node.as_str())
                .collect::<Vec<_>>()
                .join("、");
            *status = format!(
                "有 {} 台对端在线（{names}）：先把键鼠交给要发的那台，再点「发送文件」",
                peers.len()
            );
            return;
        }
    };
    let key = crate::pair::link_key(&config.pair.code);
    let node = config.node.clone();
    let ip = peer.addr.ip();
    let port = config.transfer.port;
    let peer_name = peer.node.clone();
    let proxy_ui = proxy.clone();
    let proxy_progress = proxy.clone();
    *status = "正在选择文件…".to_string();

    thread::spawn(move || {
        let files = pick_files();
        if files.is_empty() {
            let _ = proxy_ui.send_event(UserEvent::Transfer(String::new()));
            return;
        }
        let count = files.len();
        let _ = proxy_ui.send_event(UserEvent::Transfer(format!("正在发送 {count} 个文件…")));
        // 256KB 一个包，1GB 文件能回调四千次；只在百分比变化时才通知 UI
        let mut last_pct = u64::MAX;
        let result = transfer::send(ip, port, &node, &files, key, false, |sent, total| {
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
        });
        let note = match result {
            Ok(message) => format!("{message} → {peer_name}"),
            Err(err) => format!("发送失败：{err}"),
        };
        let _ = proxy_ui.send_event(UserEvent::Transfer(note));
    });
}
