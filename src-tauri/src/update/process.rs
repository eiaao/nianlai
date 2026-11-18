//! 进程层面的小工具：打开网页、等旧进程退出、拉起新版本。

use std::{path::Path, process::Command, time::Duration};

/// 更新后启动的新进程带这个参数，让它先等旧进程退出再初始化（否则单实例插件会把它当成“已有实例”而退出）。
pub const WAIT_ARG: &str = "--update-wait-pid";

pub fn open_url(url: &str) -> Result<(), String> {
    // 只打开我们自己的固定地址，不接受前端传来的任意 URL。
    if !url.starts_with("https://github.com/eiaao/nianlai/") {
        return Err("地址不在允许的范围内".into());
    }
    let program = if cfg!(windows) {
        "explorer.exe"
    } else {
        "open"
    };
    Command::new(program)
        .arg(url)
        .spawn()
        .map(drop)
        .map_err(|_| "没能打开浏览器".to_owned())
}

pub fn wait_pid_from_args(args: impl IntoIterator<Item = String>) -> Option<u32> {
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == WAIT_ARG {
            return args.next()?.parse().ok().filter(|pid| *pid != 0);
        }
    }
    None
}

/// 启动早期调用：若被更新流程拉起，等旧进程退出（最多 20 秒）。
pub fn wait_for_predecessor() {
    if let Some(pid) = wait_pid_from_args(std::env::args().skip(1)) {
        wait_for_exit(pid, Duration::from_secs(20));
    }
}

#[cfg(windows)]
fn wait_for_exit(pid: u32, timeout: Duration) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };
    unsafe {
        // 进程已经不存在时 OpenProcess 失败，直接继续。
        let Ok(handle) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else {
            return;
        };
        let _ = WaitForSingleObject(handle, timeout.as_millis().min(u32::MAX as u128 - 1) as u32);
        let _ = CloseHandle(handle);
    }
}

#[cfg(target_os = "macos")]
fn wait_for_exit(pid: u32, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    // kill(pid, 0) 只探测进程是否还在。
    while unsafe { libc::kill(pid as i32, 0) } == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn wait_for_exit(_pid: u32, _timeout: Duration) {}

/// 以新进程启动已经替换好的程序（Windows）。
#[cfg(windows)]
pub fn spawn_successor(exe: &Path) -> std::io::Result<()> {
    let mut command = Command::new(exe);
    command.arg(WAIT_ARG).arg(std::process::id().to_string());
    if let Some(dir) = exe.parent() {
        command.current_dir(dir);
    }
    command.spawn().map(drop)
}

#[cfg(not(windows))]
#[allow(dead_code)]
pub fn spawn_successor(_exe: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other("不支持"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn reads_the_predecessor_pid_from_arguments() {
        assert_eq!(
            wait_pid_from_args(args(&["--child", WAIT_ARG, "1234"])),
            Some(1234)
        );
        assert_eq!(wait_pid_from_args(args(&[WAIT_ARG])), None);
        assert_eq!(wait_pid_from_args(args(&[WAIT_ARG, "abc"])), None);
        assert_eq!(wait_pid_from_args(args(&[WAIT_ARG, "0"])), None);
        assert_eq!(wait_pid_from_args(args(&["--child"])), None);
    }

    #[test]
    fn only_project_urls_can_be_opened() {
        assert!(open_url("https://example.com/").is_err());
        assert!(open_url("file:///C:/Windows/System32/calc.exe").is_err());
    }
}
