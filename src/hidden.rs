//! 启动子进程时压掉控制台窗口。
//!
//! Windows 的托盘程序是 windows 子系统，自己不带控制台。但一旦 spawn 一个
//! **控制台程序**（reg.exe / hostname.exe / cmd 之类），Windows 会为它新建
//! 一个黑窗口，在屏幕上闪一下——如果这个调用在菜单刷新之类的循环里，就会
//! 变成"cmd 窗口一直闪"。
//!
//! 加 CREATE_NO_WINDOW 就能彻底压掉，子进程照常运行，只是没有窗口。

/// 给 Command 加上「不创建窗口」标志，返回自身方便链式调用。
pub fn quiet(cmd: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // https://learn.microsoft.com/windows/win32/procthread/process-creation-flags
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}
