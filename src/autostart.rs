//! 开机自启。
//!
//! macOS 写一个 LaunchAgent plist（用户级，不需要管理员权限）；
//! Windows 写 HKCU 的 Run 键。

use std::path::PathBuf;
#[cfg(windows)]
use std::sync::Mutex;

#[cfg(target_os = "macos")]
const LABEL: &str = "cn.zhouke.inputswitch";
#[cfg(windows)]
const RUN_KEY: &str = "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run";
#[cfg(windows)]
const VALUE_NAME: &str = "InputSwitch";

/// Windows 下查自启状态要开子进程（reg.exe），而托盘菜单每隔几秒就要重建一次。
/// 每重建一次就 spawn 一次，屏幕上就闪一个黑窗。所以只在第一次真去查，
/// 之后用内存里的值；enable / disable 会主动更新缓存。
#[cfg(windows)]
static CACHE: Mutex<Option<bool>> = Mutex::new(None);

fn exe_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("InputSwitch"))
}

#[cfg(target_os = "macos")]
fn plist_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{}.plist", LABEL))
}

/// reg.exe 的绝对路径，避免 PATH 被改过时找不到或被同名程序劫持。
#[cfg(windows)]
fn reg_exe() -> PathBuf {
    std::env::var("SystemRoot")
        .map(|root| PathBuf::from(root).join("System32").join("reg.exe"))
        .unwrap_or_else(|_| PathBuf::from("reg"))
}

#[cfg(windows)]
fn remember(value: bool) {
    if let Ok(mut guard) = CACHE.lock() {
        *guard = Some(value);
    }
}

/// 真正去注册表里查一次。
#[cfg(windows)]
fn query_run_key() -> bool {    let mut cmd = std::process::Command::new(reg_exe());
    cmd.args(["query", RUN_KEY, "/v", VALUE_NAME])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    crate::hidden::quiet(&mut cmd)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 守护任务：登录时启动，**异常退出后**按 1 分钟间隔重试 3 次。
///
/// 为什么不直接用 Run 键：Run 键只在登录时跑一次，程序崩了就再也不会回来。
/// 也不能用 `/sc minute` 反复拉起 —— 那会在程序正常运行时每隔一分钟多开
/// 一个实例。Task Scheduler 的 `RestartOnFailure` + `IgnoreNew` 正好对上：
/// 只在"上一次运行异常结束"时重启，正常在跑时新的触发被忽略。
#[cfg(windows)]
fn guard_task(exe: &PathBuf, create: bool) -> Result<(), String> {
    const TASK: &str = "InputSwitchGuard";
    if !create {
        let mut cmd = std::process::Command::new(task_exe());
        cmd.args(["delete", "/tn", TASK, "/f"]);
        let _ = crate::hidden::quiet(&mut cmd).status();
        return Ok(());
    }
    let path = xml_escape(&exe.display().to_string());
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>InputSwitch 崩溃自动重启</Description></RegistrationInfo>
  <Triggers><LogonTrigger><Enabled>true</Enabled></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RestartOnFailure><Interval>PT1M</Interval><Count>3</Count></RestartOnFailure>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Enabled>true</Enabled>
  </Settings>
  <Actions Context="Author"><Exec><Command>{path}</Command></Exec></Actions>
</Task>
"#
    );
    // schtasks /xml 要 UTF-16LE + BOM：按 UTF-8 写，中文描述和带中文的
    // 安装路径会被拒收或变成乱码。
    let mut bytes = vec![0xFF, 0xFE];
    for unit in xml.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let tmp = std::env::temp_dir().join("inputswitch-guard.xml");
    std::fs::write(&tmp, bytes).map_err(|e| format!("写任务 XML 失败: {e}"))?;
    let mut cmd = std::process::Command::new(task_exe());
    cmd.args(["create", "/tn", TASK, "/xml"]).arg(&tmp).arg("/f");
    let status = crate::hidden::quiet(&mut cmd)
        .status()
        .map_err(|e| format!("调用 schtasks 失败: {e}"))?;
    let _ = std::fs::remove_file(&tmp);
    if status.success() {
        Ok(())
    } else {
        Err("创建守护计划任务失败（可能被组策略挡住，可在任务计划程序里手动放行）".into())
    }
}

#[cfg(windows)]
fn task_exe() -> PathBuf {
    std::env::var("SystemRoot")
        .map(|root| PathBuf::from(root).join("System32").join("schtasks.exe"))
        .unwrap_or_else(|_| PathBuf::from("schtasks"))
}

#[cfg(windows)]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// 当前是否已开启
pub fn is_enabled() -> bool {
    #[cfg(windows)]
    {
        if let Ok(guard) = CACHE.lock() {
            if let Some(value) = *guard {
                return value;
            }
        }
        let value = query_run_key();
        remember(value);
        value
    }
    #[cfg(target_os = "macos")]
    {
        plist_path().exists()
    }
    #[cfg(target_os = "linux")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home)
            .join(".config/autostart/inputswitch.desktop")
            .exists()
    }
}

pub fn enable(keep_alive: bool) -> Result<(), String> {
    let exe = exe_path();
    #[cfg(target_os = "macos")]
    {
        let path = plist_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("建目录失败: {}", e))?;
        }
        // KeepAlive 只在**异常退出**时拉起（SuccessfulExit=false）：
        // 用户从托盘「退出」是正常退出，不该复活；崩溃/被 SIGKILL 才重启。
        // ThrottleInterval 防止"一启动就崩"的循环把 CPU 打满。
        let keep = if keep_alive {
            "    <key>KeepAlive</key>\n    <dict><key>SuccessfulExit</key><false/></dict>\n    \
             <key>ThrottleInterval</key><integer>10</integer>\n"
        } else {
            ""
        };
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{}</string>
    <key>ProgramArguments</key>
    <array><string>{}</string></array>
    <key>RunAtLoad</key><true/>
{}    <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
"#,
            LABEL,
            exe.display(),
            keep
        );
        std::fs::write(&path, xml).map_err(|e| format!("写 plist 失败: {}", e))?;
        // 不调 launchctl：~/Library/LaunchAgents 下的 plist 下次登录会自动加载。
        // 主动 load 需要额外权限，而且在部分沙箱环境会卡住，得不偿失。
        Ok(())
    }
    #[cfg(windows)]
    {
        let mut cmd = std::process::Command::new(reg_exe());
        cmd.args([
            "add",
            RUN_KEY,
            "/v",
            VALUE_NAME,
            "/t",
            "REG_SZ",
            "/d",
            &format!("\"{}\"", exe.display()),
            "/f",
        ]);
        let status = crate::hidden::quiet(&mut cmd)
            .status()
            .map_err(|e| format!("调用 reg 失败: {}", e))?;
        if !status.success() {
            return Err("写入注册表失败".into());
        }
        remember(true);
        // 守护任务：Run 键只在登录时跑一次，崩了就没了。这条计划任务
        // 在登录时启动、失败按 1 分钟间隔重试 3 次（Task Scheduler 的
        // RestartOnFailure），并且 MultipleInstancesPolicy=IgnoreNew，
        // 不会因为重复触发而多开一个实例。
        if keep_alive {
            guard_task(&exe, true)?;
        } else {
            let _ = guard_task(&exe, false);
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        let _ = keep_alive; // Linux 侧还没做守护（XDG autostart 本身不支持）
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        let path = PathBuf::from(home).join(".config/autostart/inputswitch.desktop");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("建目录失败: {}", e))?;
        }
        let body = format!(
            "[Desktop Entry]\nType=Application\nName=InputSwitch\nExec={}\nX-GNOME-Autostart-enabled=true\n",
            exe.display()
        );
        std::fs::write(&path, body).map_err(|e| format!("写 desktop 文件失败: {}", e))
    }
}

pub fn disable() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let path = plist_path();
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("删除 plist 失败: {}", e))?;
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        let mut cmd = std::process::Command::new(reg_exe());
        cmd.args(["delete", RUN_KEY, "/v", VALUE_NAME, "/f"]);
        let status = crate::hidden::quiet(&mut cmd)
            .status()
            .map_err(|e| format!("调用 reg 失败: {}", e))?;
        // 守护任务一并拆掉：留着它的话，用户"关掉开机自启"之后程序
        // 崩一次还是会被拉起来 —— 开关就成了假的。
        let _ = guard_task(&exe_path(), false);
        if status.success() {
            remember(false);
            Ok(())
        } else {
            Err("删除注册表项失败".into())
        }
    }
    #[cfg(target_os = "linux")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        let path = PathBuf::from(home).join(".config/autostart/inputswitch.desktop");
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("删除失败: {}", e))?;
        }
        Ok(())
    }
}
