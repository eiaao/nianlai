use super::wide;
use std::{collections::BTreeMap, path::Path, time::Duration};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
        System::{JobObjects::*, Threading::*},
    },
    core::{PCWSTR, PWSTR},
};

pub struct Handle(pub HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
pub struct Child {
    pub pid: u32,
    handle: Handle,
}
impl Child {
    pub fn exited(&self) -> bool {
        unsafe { WaitForSingleObject(self.handle.0, 0) == WAIT_OBJECT_0 }
    }
    pub fn kill(&self) -> Result<(), String> {
        if self.exited() {
            return Ok(());
        }
        unsafe {
            TerminateProcess(self.handle.0, 1)
                .map_err(|e| format!("结束自建 PID {} 失败：{e}", self.pid))
        }
    }
    pub fn wait(&self, timeout: Duration) -> bool {
        unsafe {
            WaitForSingleObject(
                self.handle.0,
                timeout.as_millis().min(u32::MAX as u128) as u32,
            ) == WAIT_OBJECT_0
        }
    }
}
pub struct Job(Handle);
impl Job {
    pub fn owns(&self, pid: u32) -> bool {
        unsafe {
            let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return false;
            };
            let process = Handle(process);
            let mut belongs = windows::core::BOOL(0);
            IsProcessInJob(process.0, Some(self.0.0), &mut belongs).is_ok() && belongs.as_bool()
        }
    }
    pub fn new() -> Result<Self, String> {
        unsafe {
            let handle = Handle(CreateJobObjectW(None, PCWSTR::null()).map_err(|e| e.to_string())?);
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                handle.0,
                JobObjectExtendedLimitInformation,
                &info as *const _ as _,
                size_of_val(&info) as u32,
            )
            .map_err(|e| e.to_string())?;
            Ok(Self(handle))
        }
    }
    pub fn spawn(
        &self,
        exe: &Path,
        args: &[String],
        env: &[(String, Option<String>)],
    ) -> Result<Child, String> {
        spawn(exe, args, env, Some(self.0.0))
    }
}
// 记事本与 Explorer 的转交进程不能加入作业对象，尤其不能在关作业时终止 shell。
pub fn delegated(exe: &Path, args: &[String]) -> Result<Child, String> {
    spawn(exe, args, &[], None)
}
pub fn quote(arg: &str) -> String {
    // 和标准库一样只在需要时加引号：资源管理器的 `/select,路径` 整段加引号时不一定能识别。
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.into();
    }
    let mut s = String::from("\"");
    let mut slashes = 0;
    for c in arg.chars() {
        if c == '\\' {
            slashes += 1;
            continue;
        }
        if c == '"' {
            s.push_str(&"\\".repeat(slashes * 2 + 1));
        } else {
            s.push_str(&"\\".repeat(slashes));
        }
        s.push(c);
        slashes = 0;
    }
    s.push_str(&"\\".repeat(slashes * 2));
    s.push('"');
    s
}
fn spawn(
    exe: &Path,
    args: &[String],
    changes: &[(String, Option<String>)],
    job: Option<HANDLE>,
) -> Result<Child, String> {
    unsafe {
        let path = exe.to_str().ok_or("程序路径无法表示为 UTF-8")?;
        let app = wide(path);
        let mut command = wide(
            &std::iter::once(path.to_owned())
                .chain(args.iter().cloned())
                .map(|a| quote(&a))
                .collect::<Vec<_>>()
                .join(" "),
        );
        let mut env: BTreeMap<String, (String, String)> = std::env::vars()
            .map(|(k, v)| (k.to_uppercase(), (k, v)))
            .collect();
        for (key, value) in changes {
            match value {
                Some(value) => {
                    env.insert(key.to_uppercase(), (key.clone(), value.clone()));
                }
                None => {
                    env.remove(&key.to_uppercase());
                }
            }
        }
        let mut block = Vec::new();
        for (_, (key, value)) in env {
            block.extend(wide(&format!("{key}={value}")));
        }
        block.push(0);
        let startup = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut process = PROCESS_INFORMATION::default();
        CreateProcessW(
            PCWSTR(app.as_ptr()),
            Some(PWSTR(command.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
            Some(block.as_ptr() as _),
            PCWSTR::null(),
            &startup,
            &mut process,
        )
        .map_err(|e| format!("启动 {}：{e}", exe.display()))?;
        let child = Child {
            pid: process.dwProcessId,
            handle: Handle(process.hProcess),
        };
        let thread = Handle(process.hThread);
        if let Some(job) = job
            && let Err(e) = AssignProcessToJobObject(job, child.handle.0)
        {
            let _ = child.kill();
            return Err(format!("挂入作业失败，未恢复子进程：{e}"));
        }
        if ResumeThread(thread.0) == u32::MAX {
            let _ = child.kill();
            return Err("恢复自建进程失败".into());
        }
        Ok(child)
    }
}
pub fn claim_new_process(pid: u32) -> Result<Child, String> {
    unsafe {
        let handle = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE,
            false,
            pid,
        )
        .map_err(|e| e.to_string())?;
        Ok(Child {
            pid,
            handle: Handle(handle),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_arguments() {
        assert_eq!(quote(""), "\"\"");
        assert_eq!(quote("a b"), "\"a b\"");
        assert_eq!(quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(quote("x y\\"), "\"x y\\\\\"");
        assert_eq!(quote("x\\"), "x\\");
        assert_eq!(quote("/select,C:\\t\\a.txt"), "/select,C:\\t\\a.txt");
    }
}
