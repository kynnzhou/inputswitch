//! 安装与重启：把「已下载并校验过的安装包」装上去，再把新版本拉起来。
//!
//! ## 为什么需要一个 helper 脚本，而不是直接 spawn 安装器
//!
//! 安装器要把 `InputSwitch.exe` 覆盖掉，而**这个 exe 正在运行**。Windows 上
//! 运行中的 exe 不能被覆盖（能改名、不能写），NSIS 静默模式下处理不了
//! 「目标被占用」，结果就是装一半失败。所以顺序必须是：
//!
//! 1. 本进程退出（释放 exe）
//! 2. 安装器跑 `/S` 静默安装
//! 3. 新版本被拉起来
//!
//! 但 1 做完之后本进程就没了，没法再执行 2 —— 所以把 2、3 写进一个 helper
//! 脚本，先 spawn 它再退出。helper 第一件事是**等本进程真的消失**（按 PID
//! 轮询），因为 `process::exit` 和「文件句柄释放」之间还有一点点时间差。
//!
//! 这个模式和仓库里已有的 `tray::sys::restart_app`（菜单「重启」）是同一套
//! 思路，区别只是那边 1 秒后无条件拉起、这边要等安装器跑完。
//!
//! ## macOS
//!
//! 产物是 `.app` 打的 zip（见 `build.sh`）：解包 → 删旧 bundle → 拷新的 →
//! `open -n` 拉起。同样要先等本进程退出。
//!
//! ## 失败怎么复位
//!
//! helper **spawn 失败**（磁盘满、临时目录没了）是唯一能在本进程里发现的
//! 失败。此时状态回 `ready` 而不是停在 `restarting` —— 卡在「正在重启」的
//! 用户除了杀进程没有别的办法。安装器自己跑挂的情况本进程已经看不到了，
//! helper 的输出重定向进日志文件，排查时去那里看。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::hidden::quiet;
use crate::update::source::{ErrorCode, UpdateError};
use crate::update::{lock, pending_installer, snapshot, Phase, UpdateState};

/// 倒计时秒数。与 cc-bridge 一致：够用户看清「即将重启」并伸手取消，
/// 又不至于让人等得不耐烦。
pub const RESTART_COUNTDOWN_SECS: u64 = 8;

/// macOS bundle 里可执行文件所在的那层目录名。
#[cfg(target_os = "macos")]
const MACOS_DIR: &str = "MacOS";

/// 一次安装的全部执行信息。拆出来是为了能单测——脚本正文是纯字符串，
/// 不需要真跑就能断言「该等 PID、该调安装器、该拉起新 exe」。
#[derive(Debug)]
pub struct InstallPlan {
    /// helper 脚本正文（Windows 是 cmd，macOS 是 sh）
    pub script: String,
    /// 脚本落地路径。同目录还会有一个同名的 `.log` 收 helper 的输出
    pub script_path: PathBuf,
    /// 写进日志的一行说明
    pub summary: String,
}

/// 本进程 PID。单独包一层是为了测试里能看出「脚本等的是自己」。
pub fn current_pid() -> u32 {
    std::process::id()
}

/// 按平台生成安装计划。
///
/// `installer` 是下载并校验过的安装包，`exe` 是本进程的可执行文件路径
/// （`std::env::current_exe()`，测试里可以传假的）。
pub fn plan(installer: &Path, exe: &Path) -> Result<InstallPlan, UpdateError> {
    let ext = installer
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    #[cfg(target_os = "windows")]
    {
        if ext != "exe" {
            return Err(UpdateError::new(
                ErrorCode::Unsupported,
                format!(
                    "Windows 上只支持 .exe 安装器，拿到的是 {}",
                    installer.display()
                ),
            ));
        }
        Ok(InstallPlan {
            script: windows_script(installer, exe, current_pid()),
            script_path: crate::update::download::staging_dir().join("inputswitch-install.cmd"),
            summary: format!("NSIS 静默安装 {} 后重启", installer.display()),
        })
    }

    #[cfg(target_os = "macos")]
    {
        if ext != "zip" {
            return Err(UpdateError::new(
                ErrorCode::Unsupported,
                format!(
                    "macOS 上只支持 .app 打包的 .zip，拿到的是 {}",
                    installer.display()
                ),
            ));
        }
        let bundle = macos_bundle_path(exe)?;
        Ok(InstallPlan {
            script: macos_script(installer, &bundle, exe, current_pid()),
            script_path: crate::update::download::staging_dir().join("inputswitch-install.sh"),
            summary: format!("替换 {} 后重启", bundle.display()),
        })
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = (&ext, exe);
        Err(UpdateError::new(
            ErrorCode::Unsupported,
            "这个平台没有自动安装能力（只有 Windows 和 macOS 有）",
        ))
    }
}

/// 把脚本写盘并**脱离本进程**跑起来。返回时 helper 已经在等我们退出。
pub fn spawn_helper(plan: &InstallPlan) -> Result<(), UpdateError> {
    let dir = crate::update::download::staging_dir();
    fs::create_dir_all(&dir)
        .map_err(|e| UpdateError::new(ErrorCode::Config, format!("建不了临时目录：{e}")))?;
    let log = plan.script_path.with_extension("log");
    // 日志先清空：上一次安装残留的日志会让排查时看错
    let _ = fs::remove_file(&log);
    fs::write(&plan.script_path, plan.script.as_bytes()).map_err(|e| {
        UpdateError::new(
            ErrorCode::Config,
            format!("写安装脚本失败（{}）：{e}", plan.script_path.display()),
        )
    })?;

    let mut cmd = Command::new(interpreter());
    cmd.arg(&plan.script_path);
    // 托盘程序是 windows 子系统，spawn 控制台程序会闪一个黑窗
    quiet(&mut cmd);
    // helper 的输出全部进日志文件。它跑的时候本进程已经没了，没有任何
    // 地方能显示这些输出 —— 只有文件能留给事后排查
    let log_file = fs::File::create(&log)
        .map_err(|e| UpdateError::new(ErrorCode::Config, format!("建日志文件失败：{e}")))?;
    cmd.stdout(log_file);
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| UpdateError::new(ErrorCode::Config, format!("启动安装脚本失败：{e}")))
}

fn interpreter() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "cmd"
    }
    #[cfg(target_os = "macos")]
    {
        "sh"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        "false"
    }
}

/// Windows helper：等本进程消失 → 静默安装 → 拉起新 exe。
#[cfg(target_os = "windows")]
fn windows_script(installer: &Path, exe: &Path, pid: u32) -> String {
    // 路径一律加引号：安装目录带空格（`C:\Program Files\...`）是常态。
    // 用行向量拼而不是在一个字面量里写 `\r\n`——后者改一行要全文重对缩进。
    //
    // `%` 不转义：安装路径里出现 `%` 的概率比它带来的麻烦低得多，真遇到
    // 脚本是临时文件、一眼能看出问题。
    let i = installer.display();
    let e = exe.display();
    [
        "@echo off".to_string(),
        ":wait".to_string(),
        // tasklist 按 PID 过滤；进程没了它只会打印一行 INFO（不含 PID），
        // findstr 找不到 → errorlevel 1 → 退出等待
        format!("tasklist /NH /FI \"PID eq {pid}\" 2>nul | findstr /C:\"{pid}\" >nul"),
        "if not errorlevel 1 (".to_string(),
        "  timeout /t 1 /nobreak >nul".to_string(),
        "  goto wait".to_string(),
        ")".to_string(),
        // `/S` 是 NSIS 的静默开关。安装目录沿用上次装的位置（NSIS 自己记在
        // 注册表里），不传 `/D=` —— 让用户自己选过的路径别被更新改掉
        format!("\"{i}\" /S"),
        // `start "" "exe"`：第一个空串是窗口标题，少了它带引号的路径会被
        // 当成标题而不是要运行的程序
        format!("start \"\" \"{e}\""),
    ]
    .join("\r\n")
}

/// macOS helper：等本进程消失 → 解包 zip → 替换 bundle → 拉起。
#[cfg(target_os = "macos")]
fn macos_script(installer: &Path, bundle: &Path, exe: &Path, pid: u32) -> String {
    let bin = exe
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("InputSwitch");
    let z = installer.display();
    let b = bundle.display();
    [
        "#!/bin/sh".to_string(),
        format!("while kill -0 {pid} 2>/dev/null; do sleep 1; done"),
        "tmp=\"$(mktemp -d)\"".to_string(),
        format!("ditto -x -k \"{z}\" \"$tmp\" || exit 1"),
        "app=\"$(find \"$tmp\" -maxdepth 1 -name '*.app' | head -1)\"".to_string(),
        "if [ -z \"$app\" ]; then echo \"zip 里没有 .app\" >&2; exit 1; fi".to_string(),
        format!("rm -rf \"{b}\""),
        format!("cp -R \"$app\" \"{b}\" || exit 1"),
        // `-n`：强制起一个新实例，不等已有的
        format!("open -n \"{b}/Contents/{MACOS_DIR}/{bin}\""),
        "rm -rf \"$tmp\"".to_string(),
    ]
    .join("\n")
}

/// 从可执行文件路径推出 `.app` bundle 路径：
/// `.../InputSwitch.app/Contents/MacOS/InputSwitch` → `.../InputSwitch.app`
#[cfg(target_os = "macos")]
fn macos_bundle_path(exe: &Path) -> Result<PathBuf, UpdateError> {
    exe.parent() // Contents/MacOS
        .and_then(Path::parent) // Contents
        .and_then(Path::parent) // InputSwitch.app
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            UpdateError::new(
                ErrorCode::Config,
                format!("看不出 .app 位置：{}", exe.display()),
            )
        })
}

// ---------------------------------------------------------------- 安装与重启

/// 单飞闸：倒计时线程只允许有一个。重复点「立即安装」不会起第二个线程，
/// 只会把 `auto_restart_in` 压到 0（见 `begin_restart_countdown`）。
static COUNTDOWN: AtomicBool = AtomicBool::new(false);

struct CountdownGuard;

impl Drop for CountdownGuard {
    fn drop(&mut self) {
        // 线程退出就放行：否则一次取消之后再也起不了新的倒计时
        COUNTDOWN.store(false, Ordering::Release);
    }
}

/// 用户点「安装更新」：进入 8 秒倒计时，期间可以取消。
///
/// 倒计时不是装饰——安装器会关掉本进程、还可能弹 UAC，必须给用户一个
/// 「等等，我还在用它」的退路。cc-bridge 用的是同样的 8 秒。
pub fn begin_restart_countdown() -> UpdateState {
    if COUNTDOWN.swap(true, Ordering::AcqRel) {
        // 已经在倒计时了：再来一次按「立即安装」处理
        return shortcut_to_zero();
    }
    {
        let mut s = lock();
        if s.phase != Phase::Ready {
            return s.clone();
        }
        s.phase = Phase::Restarting;
        s.auto_restart_in = RESTART_COUNTDOWN_SECS;
        s.error.clear();
        s.code.clear();
    }
    std::thread::spawn(|| {
        let _guard = CountdownGuard;
        countdown_loop();
    });
    snapshot()
}

fn shortcut_to_zero() -> UpdateState {
    let mut s = lock();
    if s.phase == Phase::Restarting {
        s.auto_restart_in = 0;
    }
    s.clone()
}

/// 倒计时线程：每秒减 1，归零就装。
fn countdown_loop() {
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let (counting, left) = {
            let s = lock();
            (s.phase == Phase::Restarting, s.auto_restart_in)
        };
        if !counting {
            return; // 被取消，或者已经装完/失败
        }
        if left > 1 {
            lock().auto_restart_in = left - 1;
            continue;
        }
        // 到点。装不上要**复位**而不是停在 restarting：卡在「正在重启」
        // 的用户除了杀进程没别的办法
        match install_now() {
            // helper 已经起来、正在等本进程退出。**必须真的退** ——
            // 运行中的 exe 不能被覆盖（见文件头说明），晚一步退出安装器
            // 就撞上还在跑的进程。CLI 不走倒计时（它 install_now 完自己退），
            // 所以这里退只影响面板这条路径。测试也到不了这一格：都立刻取消，
            // 否则 exit(0) 会把测试进程本身干掉。
            Ok(()) => std::process::exit(0),
            Err(e) => {
                let mut s = lock();
                s.phase = Phase::Ready;
                s.auto_restart_in = 0;
                s.error = e.message;
                s.code = e.code.to_string();
            }
        }
        return;
    }
}

/// 用户点「取消」：回到 `ready`，倒计时线程下一个 tick 自己会退出。
pub fn cancel_restart() -> UpdateState {
    let mut s = lock();
    if s.phase == Phase::Restarting {
        s.phase = Phase::Ready;
        s.auto_restart_in = 0;
    }
    s.clone()
}

/// 真正执行安装：拉起 helper 脚本，让它等本进程退出后跑安装器、再起新版本。
///
/// **调用方负责退出进程**（`std::process::exit(0)`）。分开是为了让 CLI
/// 和托盘走同一条安装路径、但各自决定怎么退。
pub fn install_now() -> Result<(), UpdateError> {
    let installer = pending_installer().ok_or_else(|| {
        UpdateError::new(ErrorCode::Config, "没有已就绪的安装包（请先下载并校验）")
    })?;
    if !installer.is_file() {
        return Err(UpdateError::new(
            ErrorCode::Config,
            format!("安装包不见了（{}）", installer.display()),
        ));
    }
    let exe = std::env::current_exe()
        .map_err(|e| UpdateError::new(ErrorCode::Config, format!("找不到自己的可执行文件：{e}")))?;
    let plan = plan(&installer, &exe)?;
    crate::update::log(&format!("自动更新：{}", plan.summary));
    spawn_helper(&plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("isw-install-test-{name}"))
    }

    #[test]
    fn wrong_installer_kind_is_refused_before_any_spawn() {
        // 平台无关：拿一个 .json 当安装器，任何平台都该拒绝
        let err = plan(&fake_path("updater.json"), &fake_path("app")).unwrap_err();
        assert_eq!(err.code, ErrorCode::Unsupported);
    }

    #[test]
    fn script_waits_for_pid_then_installs_then_relaunches() {
        let installer = fake_path("InputSwitch_0.6.90_x64-setup.exe");
        let exe = fake_path("InputSwitch.exe");
        match plan(&installer, &exe) {
            Ok(p) => {
                let s = &p.script;
                let pid = current_pid().to_string();
                assert!(s.contains(&pid), "脚本必须等本进程退出：{s}");
                assert!(
                    s.contains(installer.to_str().unwrap()),
                    "脚本必须调安装器：{s}"
                );
                assert!(s.contains(exe.to_str().unwrap()), "脚本必须拉起新 exe：{s}");
            }
            Err(e) => assert_eq!(e.code, ErrorCode::Unsupported),
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_script_is_nsis_silent_with_quoted_paths() {
        let installer = PathBuf::from("C:\\Users\\me\\AppData\\Local\\Temp\\setup.exe");
        let exe = PathBuf::from("C:\\Program Files\\InputSwitch\\InputSwitch.exe");
        let s = windows_script(&installer, &exe, 4242);
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines[0], "@echo off", "{s}");
        assert_eq!(lines[1], ":wait", "等待标签要在最前面：{s}");
        assert!(lines.iter().any(|l| l.contains("PID eq 4242")), "{s}");
        assert!(lines.iter().any(|l| l.contains("\"4242\"")), "{s}");
        // 安装器那行：路径加引号 + /S，且 /S 是唯一参数（不传 /D= 保留用户选过的目录）
        let install = lines
            .iter()
            .find(|l| l.contains("setup.exe"))
            .expect("要有安装那一行");
        assert_eq!(*install, format!("\"{}\" /S", installer.display()), "{s}");
        // 拉起那行：start "" "exe"，空标题不能省（省了带引号的路径会被当标题）
        assert!(
            lines
                .iter()
                .any(|l| *l == format!("start \"\" \"{}\"", exe.display())),
            "{s}"
        );
        // 等待块要把 sleep 和 goto 包在 if 里，否则会无条件循环
        assert!(s.contains("if not errorlevel 1 ("), "{s}");
        assert!(s.contains("goto wait"), "{s}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_script_replaces_bundle_and_relaunches() {
        let exe = PathBuf::from("/Applications/InputSwitch.app/Contents/MacOS/InputSwitch");
        let bundle = macos_bundle_path(&exe).unwrap();
        assert_eq!(bundle, PathBuf::from("/Applications/InputSwitch.app"));
        let s = macos_script(&fake_path("InputSwitch.zip"), &bundle, &exe, 99);
        assert!(s.contains("kill -0 99"), "要等本进程退出：{s}");
        assert!(
            s.contains("rm -rf \"/Applications/InputSwitch.app\""),
            "要删旧 bundle：{s}"
        );
        assert!(
            s.contains("open -n \"/Applications/InputSwitch.app/Contents/MacOS/InputSwitch\""),
            "要拉起新 bundle：{s}"
        );
        assert!(s.contains("ditto -x -k"), "解 zip 用 ditto：{s}");
        assert!(s.contains("exit 1"), "任一步失败都要让脚本非零退出：{s}");
    }

    /// 倒计时会改全局状态，而 `cargo test` 是同进程多测试共用的。
    /// 每个动状态的测试都先把它按回 ready，断言也只验「不变量」。
    fn reset_to_ready() {
        let mut s = lock();
        s.phase = Phase::Ready;
        s.auto_restart_in = 0;
        s.error.clear();
        s.code.clear();
    }

    #[test]
    fn restart_countdown_starts_then_cancels_back_to_ready() {
        reset_to_ready();
        let s = begin_restart_countdown();
        assert_eq!(s.phase, Phase::Restarting);
        assert_eq!(s.auto_restart_in, RESTART_COUNTDOWN_SECS);
        // 倒计时线程可能刚好在这一秒里把它减掉，所以只验「没超上限」
        assert!(s.auto_restart_in <= RESTART_COUNTDOWN_SECS);

        let s = cancel_restart();
        assert_eq!(s.phase, Phase::Ready, "取消必须回到可重试状态");
        assert_eq!(s.auto_restart_in, 0);
    }

    #[test]
    fn cancel_outside_countdown_is_a_noop() {
        reset_to_ready();
        let s = cancel_restart();
        assert_eq!(s.phase, Phase::Ready);
        assert_eq!(s.auto_restart_in, 0);
    }

    #[test]
    fn install_without_ready_package_is_a_config_error() {
        reset_to_ready();
        // pending_installer 是 take()：取走就没了，所以这里只验「没有包时报错」
        let _ = crate::update::pending_installer();
        let err = install_now().unwrap_err();
        assert_eq!(err.code, ErrorCode::Config, "{}", err.message);
        assert!(
            err.message.contains("下载"),
            "要说清要先下载：{}",
            err.message
        );
    }

    #[test]
    fn spawn_helper_writes_script_and_clears_old_log() {
        // 用 `exit 0` 当脚本真跑一遍 spawn 路径：它只写盘、起一个立刻
        // 退出的解释器进程，不碰安装器。断言的是文件副作用。
        let dir = fake_path("spawn");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let script_path = dir.join("inputswitch-install.cmd");
        let log = script_path.with_extension("log");
        fs::write(&log, "上一次安装的残留").unwrap();

        let plan = InstallPlan {
            script: "exit 0\n".to_string(),
            script_path: script_path.clone(),
            summary: "测试".to_string(),
        };
        // 平台不支持时 spawn_helper 仍要能跑（interpreter 是 false / sh）
        let _ = spawn_helper(&plan);
        assert!(script_path.exists(), "脚本要落盘");
        // spawn_helper 自己要建日志文件收 helper 的输出，所以断言「是空的」
        // 而不是「不存在」—— 上一次的残留必须被清掉
        let leftover = fs::read_to_string(&log).unwrap_or_default();
        assert!(
            leftover.is_empty(),
            "旧日志要清空，否则排查时看错：{leftover}"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
