use super::hwnd;
use crate::preflight::{Environment, Process};
use serde::Serialize;
use windows::{
    Win32::{
        Foundation::{CloseHandle, HWND, LPARAM, POINT, RECT},
        Graphics::Gdi::*,
        System::{Diagnostics::ToolHelp::*, Threading::*},
        UI::{Input::KeyboardAndMouse::GetKeyboardLayoutList, WindowsAndMessaging::*},
    },
    core::{BOOL, PWSTR},
};

pub fn processes() -> Result<Vec<Process>, String> {
    unsafe {
        let handle = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).map_err(|e| e.to_string())?;
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut list = Vec::new();
        let mut ok = Process32FirstW(handle, &mut entry).is_ok();
        while ok {
            let len = entry
                .szExeFile
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(entry.szExeFile.len());
            list.push(Process {
                pid: entry.th32ProcessID,
                name: String::from_utf16_lossy(&entry.szExeFile[..len]),
            });
            ok = Process32NextW(handle, &mut entry).is_ok();
        }
        let _ = CloseHandle(handle);
        Ok(list)
    }
}
pub fn process_name(pid: u32) -> String {
    unsafe {
        let Ok(p) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return "无法读取".into();
        };
        let mut buf = [0u16; 2048];
        let mut length = buf.len() as u32;
        let result = if QueryFullProcessImageNameW(
            p,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut length,
        )
        .is_ok()
        {
            String::from_utf16_lossy(&buf[..length as usize])
                .rsplit('\\')
                .next()
                .unwrap_or("")
                .to_owned()
        } else {
            "无法读取".into()
        };
        let _ = CloseHandle(p);
        result
    }
}
pub fn class(raw: isize) -> String {
    unsafe {
        let mut buf = [0u16; 256];
        let n = GetClassNameW(hwnd(raw), &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}
pub fn pid(raw: isize) -> u32 {
    let mut p = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd(raw), Some(&mut p));
    }
    p
}
pub fn foreground() -> isize {
    unsafe { GetForegroundWindow().0 as isize }
}
pub fn visible(raw: isize) -> bool {
    unsafe { IsWindowVisible(hwnd(raw)).as_bool() && IsWindow(Some(hwnd(raw))).as_bool() }
}
pub fn focus(raw: isize) -> isize {
    unsafe {
        let thread = GetWindowThreadProcessId(hwnd(raw), None);
        let mut gui = GUITHREADINFO {
            cbSize: size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        if GetGUIThreadInfo(thread, &mut gui).is_ok() {
            gui.hwndFocus.0 as isize
        } else {
            0
        }
    }
}
pub fn rect(raw: isize) -> Option<RECT> {
    let mut r = RECT::default();
    unsafe {
        GetWindowRect(hwnd(raw), &mut r).ok()?;
    }
    Some(r)
}

#[derive(Clone, Debug)]
pub struct Window {
    pub raw: isize,
    pub pid: u32,
    pub class: String,
    pub process: String,
}
pub fn windows() -> Result<Vec<Window>, String> {
    unsafe extern "system" fn callback(h: HWND, data: LPARAM) -> BOOL {
        unsafe {
            let list = &mut *(data.0 as *mut Vec<Window>);
            if IsWindowVisible(h).as_bool() {
                let raw = h.0 as isize;
                let pid = pid(raw);
                list.push(Window {
                    raw,
                    pid,
                    class: class(raw),
                    process: process_name(pid),
                });
            }
            BOOL(1)
        }
    }
    let mut list = Vec::new();
    unsafe {
        EnumWindows(Some(callback), LPARAM(&mut list as *mut _ as isize))
            .map_err(|e| e.to_string())?;
    }
    Ok(list)
}
// 只允许已经通过进程、类名和新窗口检查的候选调用，空跑不走这里。
pub fn owned_title(raw: isize) -> String {
    unsafe {
        let mut buf = vec![0u16; 8192];
        let n = GetWindowTextW(hwnd(raw), &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Monitor {
    pub id: isize,
    pub primary: bool,
    pub rect: [i32; 4],
}
pub fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn callback(h: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        unsafe {
            let list = &mut *(data.0 as *mut Vec<Monitor>);
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(h, &mut info).as_bool() {
                let r = info.rcMonitor;
                list.push(Monitor {
                    id: h.0 as isize,
                    primary: info.dwFlags & 1 != 0,
                    rect: [r.left, r.top, r.right, r.bottom],
                });
            }
            BOOL(1)
        }
    }
    let mut list = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(callback),
            LPARAM(&mut list as *mut _ as isize),
        );
    }
    list
}
pub fn monitor_window(raw: isize) -> isize {
    unsafe { MonitorFromWindow(hwnd(raw), MONITOR_DEFAULTTONEAREST).0 as isize }
}
pub fn monitor_mouse() -> isize {
    unsafe {
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST).0 as isize
    }
}
pub fn environment() -> Environment {
    let mut result = Environment {
        platform: "Windows".into(),
        ..Default::default()
    };
    match processes() {
        Ok(p) => result.processes = p,
        Err(e) => result.errors.push(format!("进程枚举失败：{e}")),
    }
    match windows() {
        Ok(w) => result.window_classes = w.into_iter().map(|w| w.class).collect(),
        Err(e) => result.errors.push(format!("窗口枚举失败：{e}")),
    }
    result.monitors = monitors().len();
    unsafe {
        let n = GetKeyboardLayoutList(None);
        let mut layouts = vec![Default::default(); n.max(0) as usize];
        GetKeyboardLayoutList(Some(&mut layouts));
        result.chinese_keyboard = layouts.iter().any(|l| (l.0 as usize & 0x3ff) == 4);
        result.remote = GetSystemMetrics(SM_REMOTESESSION) != 0;
    }
    result
}
