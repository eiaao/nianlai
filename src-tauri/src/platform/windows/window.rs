use crate::platform::{Caret, FocusResult, ForegroundResult, Result, Target};
use std::{mem::size_of, path::Path};
use tauri::WebviewWindow;
use windows::{
    Win32::{
        Foundation::{CloseHandle, GetLastError, HWND, POINT, RECT, SetLastError, WIN32_ERROR},
        Graphics::Gdi::{
            ClientToScreen, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
            MONITORINFO, MonitorFromPoint,
        },
        System::Threading::{
            AttachThreadInput, GetCurrentThreadId, OpenProcess, PROCESS_NAME_WIN32,
            PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
        },
        UI::{
            HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
            Input::KeyboardAndMouse::SetFocus,
            WindowsAndMessaging::*,
        },
    },
    core::PWSTR,
};

pub fn hwnd(value: usize) -> HWND {
    HWND(value as *mut _)
}
pub fn handle(panel: &WebviewWindow) -> Result<usize> {
    panel
        .hwnd()
        .map(|h| h.0 as usize)
        .map_err(|e| e.to_string())
}
pub fn foreground() -> usize {
    unsafe { GetForegroundWindow().0 as usize }
}

pub fn capture() -> Target {
    unsafe {
        let window = GetForegroundWindow();
        let mut target = Target {
            window: window.0 as usize,
            ..Target::default()
        };
        if window.is_invalid() {
            return target;
        }
        target.thread = GetWindowThreadProcessId(window, Some(&mut target.process));
        let mut info = GUITHREADINFO {
            cbSize: size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        if GetGUIThreadInfo(target.thread, &mut info).is_ok() {
            target.focus = info.hwndFocus.0 as usize;
            let mut class = [0u16; 256];
            let length = GetClassNameW(info.hwndFocus, &mut class);
            target.focus_class = String::from_utf16_lossy(&class[..length.max(0) as usize]);
            if !info.hwndCaret.is_invalid() {
                let mut top = POINT {
                    x: info.rcCaret.left,
                    y: info.rcCaret.top,
                };
                let mut bottom = POINT {
                    x: info.rcCaret.left,
                    y: info.rcCaret.bottom,
                };
                if ClientToScreen(info.hwndCaret, &mut top).as_bool()
                    && ClientToScreen(info.hwndCaret, &mut bottom).as_bool()
                {
                    target.caret = Some(Caret {
                        left: top.x,
                        top: top.y,
                        bottom: bottom.y,
                    });
                }
            }
        }
        if let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, target.process) {
            let mut buffer = [0u16; 32768];
            let mut length = buffer.len() as u32;
            if QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                PWSTR(buffer.as_mut_ptr()),
                &mut length,
            )
            .is_ok()
            {
                let full = String::from_utf16_lossy(&buffer[..length as usize]);
                target.process_name = Path::new(&full)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
            }
            let _ = CloseHandle(process);
        }
        target
    }
}

pub fn no_activate(panel: &WebviewWindow, enabled: bool) -> Result<()> {
    panel.set_focusable(!enabled).map_err(|e| e.to_string())?;
    unsafe {
        let window = hwnd(handle(panel)?);
        let style = GetWindowLongPtrW(window, GWL_EXSTYLE);
        let style = if enabled {
            style | WS_EX_NOACTIVATE.0 as isize
        } else {
            style & !(WS_EX_NOACTIVATE.0 as isize)
        };
        SetLastError(WIN32_ERROR(0));
        if SetWindowLongPtrW(window, GWL_EXSTYLE, style) == 0 && GetLastError().0 != 0 {
            return Err(windows::core::Error::from_thread().to_string());
        }
        SetWindowPos(
            window,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER,
        )
        .map_err(|e| e.to_string())
    }
}

pub fn show(panel: &WebviewWindow, target: &Target) -> Result<()> {
    no_activate(panel, true)?;
    unsafe {
        let mut point = POINT::default();
        if let Some(caret) = target.caret {
            point = POINT {
                x: caret.left,
                y: caret.top,
            };
        } else {
            GetCursorPos(&mut point).map_err(|e| e.to_string())?;
        }
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return Err(windows::core::Error::from_thread().to_string());
        }
        let (mut dpi_x, mut dpi_y) = (96, 96);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let work = info.rcWork;
        let width = ((420.0 * dpi_x as f64 / 96.0).round() as i32).min(work.right - work.left);
        let height = ((520.0 * dpi_y as f64 / 96.0).round() as i32).min(work.bottom - work.top);
        let x = point.x.clamp(work.left, work.right - width);
        let gap = (6 * dpi_y / 96) as i32;
        let y = if let Some(caret) = target.caret {
            let below = caret.bottom + gap;
            if below + height <= work.bottom {
                below
            } else {
                caret.top - gap - height
            }
        } else {
            point.y + gap
        }
        .clamp(work.top, work.bottom - height);
        present(panel, x, y, width, height)
    }
}

pub fn show_auto(panel: &WebviewWindow) -> Result<()> {
    no_activate(panel, true)?;
    unsafe {
        let monitor = MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return Err(windows::core::Error::from_thread().to_string());
        }
        let (mut dpi_x, mut dpi_y) = (96, 96);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let work = info.rcWork;
        let margin_x = (12.0 * dpi_x as f64 / 96.0).round() as i32;
        let margin_y = (12.0 * dpi_y as f64 / 96.0).round() as i32;
        let width = ((420.0 * dpi_x as f64 / 96.0).round() as i32)
            .min((work.right - work.left - 2 * margin_x).max(1));
        let height = ((520.0 * dpi_y as f64 / 96.0).round() as i32)
            .min((work.bottom - work.top - 2 * margin_y).max(1));
        present(
            panel,
            work.right - margin_x - width,
            work.bottom - margin_y - height,
            width,
            height,
        )
    }
}

fn present(panel: &WebviewWindow, x: i32, y: i32, width: i32, height: i32) -> Result<()> {
    unsafe {
        let window = hwnd(handle(panel)?);
        SetWindowPos(
            window,
            Some(HWND_TOPMOST),
            x,
            y,
            width,
            height,
            SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        )
        .map_err(|e| e.to_string())?;
        let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
        SetWindowPos(
            window,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }
}

pub fn hide(panel: &WebviewWindow) -> Result<()> {
    unsafe {
        let _ = ShowWindow(hwnd(handle(panel)?), SW_HIDE);
    }
    no_activate(panel, true)
}

struct Attached {
    current: u32,
    other: u32,
    attached: bool,
}
impl Attached {
    fn new(other: u32) -> Self {
        let current = unsafe { GetCurrentThreadId() };
        let attached = other != 0
            && current != other
            && unsafe { AttachThreadInput(current, other, true).as_bool() };
        Self {
            current,
            other,
            attached,
        }
    }
}
impl Drop for Attached {
    fn drop(&mut self) {
        if self.attached {
            unsafe {
                let _ = AttachThreadInput(self.current, self.other, false);
            }
        }
    }
}

pub fn activate(window: usize) -> ForegroundResult {
    unsafe {
        if !IsWindow(Some(hwnd(window))).as_bool() {
            return ForegroundResult {
                path: "invalid",
                success: false,
            };
        }
        let _ = SetForegroundWindow(hwnd(window));
        if foreground() == window {
            return ForegroundResult {
                path: "direct",
                success: true,
            };
        }
        let thread = GetWindowThreadProcessId(GetForegroundWindow(), None);
        let attached = Attached::new(thread);
        if attached.attached {
            let _ = SetForegroundWindow(hwnd(window));
        }
        ForegroundResult {
            path: "attach_thread_input",
            success: foreground() == window,
        }
    }
}

pub fn valid(target: &Target) -> bool {
    unsafe {
        let mut process = 0;
        target.window != 0
            && IsWindow(Some(hwnd(target.window))).as_bool()
            && GetWindowThreadProcessId(hwnd(target.window), Some(&mut process)) == target.thread
            && process == target.process
    }
}

pub fn confirm(target: &Target) -> FocusResult {
    let foreground_ok = valid(target) && foreground() == target.window;
    let mut info = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    let focus_unknown = target.focus == 0;
    let focus_restored = foreground_ok
        && (focus_unknown
            || (unsafe { GetGUIThreadInfo(target.thread, &mut info).is_ok() }
                && info.hwndFocus.0 as usize == target.focus));
    FocusResult {
        foreground: ForegroundResult {
            path: "unchanged",
            success: foreground_ok,
        },
        focus_restored,
        focus_unknown,
    }
}

pub fn restore(target: &Target) -> FocusResult {
    if !valid(target) {
        return confirm(target);
    }
    let foreground_result = activate(target.window);
    if foreground_result.success && target.focus != 0 {
        unsafe {
            let focus = hwnd(target.focus);
            let mut process = 0;
            let thread = GetWindowThreadProcessId(focus, Some(&mut process));
            if IsWindow(Some(focus)).as_bool()
                && thread == target.thread
                && process == target.process
            {
                let _attached = Attached::new(thread);
                let _ = SetFocus(Some(focus));
            }
        }
    }
    let mut result = confirm(target);
    result.foreground.path = foreground_result.path;
    result
}

pub fn rect(window: usize) -> Result<RECT> {
    let mut rect = RECT::default();
    unsafe {
        GetWindowRect(hwnd(window), &mut rect).map_err(|e| e.to_string())?;
    }
    Ok(rect)
}
