use crate::platform::{Caret, FocusResult, ForegroundResult, Result, Target};
use std::{mem::size_of, path::Path};
use tauri::WebviewWindow;
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, GetLastError, HANDLE, HWND, POINT, RECT, SetLastError, WIN32_ERROR,
        },
        Graphics::Gdi::{
            ClientToScreen, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
            MONITORINFO, MonitorFromPoint,
        },
        System::{
            StationsAndDesktops::{GetThreadDesktop, GetUserObjectInformationW, UOI_IO},
            Threading::{
                AttachThreadInput, GetCurrentThreadId, OpenProcess, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
            },
        },
        UI::{
            HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
            Input::KeyboardAndMouse::{IsWindowEnabled, SetFocus},
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
pub fn tick_count() -> u32 {
    unsafe { windows::Win32::System::SystemInformation::GetTickCount() }
}

pub(super) fn thread_input_desktop_receiving_input() -> windows::core::Result<bool> {
    unsafe {
        // GetThreadDesktop 返回借用句柄，API 明确禁止调用 CloseDesktop；UOI_IO 直接证明它正接收输入。
        let desktop = GetThreadDesktop(GetCurrentThreadId())?;
        let mut receiving_input = 0i32;
        GetUserObjectInformationW(
            HANDLE(desktop.0),
            UOI_IO,
            Some(std::ptr::addr_of_mut!(receiving_input).cast()),
            size_of::<i32>() as u32,
            None,
        )?;
        Ok(receiving_input != 0)
    }
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
        target.window_class = class_name(target.window);
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
        target.process_name = process_name(target.process);
        target
    }
}

pub fn class_name(window: usize) -> String {
    let mut class = [0u16; 256];
    let length = unsafe { GetClassNameW(hwnd(window), &mut class) };
    String::from_utf16_lossy(&class[..length.max(0) as usize])
}

pub fn process_of(window: usize) -> String {
    let mut id = 0;
    unsafe { GetWindowThreadProcessId(hwnd(window), Some(&mut id)) };
    process_name(id)
}

pub fn process_name(id: u32) -> String {
    unsafe {
        if let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, id) {
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
                let name = Path::new(&full)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let _ = CloseHandle(process);
                return name;
            }
            let _ = CloseHandle(process);
        }
        String::new()
    }
}

pub fn no_activate(panel: &WebviewWindow, enabled: bool) -> Result<()> {
    // 不能调 tao 的 set_focusable：面板由我们自己显示，tao 仍认为窗口隐藏，
    // 它重算样式时会顺手 ShowWindow(SW_HIDE)，切到搜索模式时面板就消失了。
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

// 新搜索许可和手动粘贴提示必须确认已捕获的原生控件；未知焦点不再当作成功。
pub fn confirm_strict(target: &Target) -> FocusResult {
    let mut result = confirm(target);
    result.focus_restored &= original_control_valid(target);
    result
}

fn original_control_valid(target: &Target) -> bool {
    let mut process = 0;
    target.focus != 0
        && unsafe {
            IsWindow(Some(hwnd(target.focus))).as_bool()
                && GetWindowThreadProcessId(hwnd(target.focus), Some(&mut process)) == target.thread
                && process == target.process
                && GetAncestor(hwnd(target.focus), GA_ROOT).0 as usize == target.window
        }
        && class_name(target.focus) == target.focus_class
}

pub fn native_control_valid(target: &Target) -> bool {
    if !valid(target)
        || !original_control_valid(target)
        || !crate::paste::native::standard_edit(&target.focus_class)
        || target.thread == unsafe { GetCurrentThreadId() }
    {
        return false;
    }
    unsafe {
        let mut control = hwnd(target.focus);
        if !IsWindowUnicode(control).as_bool()
            || !IsWindowVisible(control).as_bool()
            || GetWindowLongPtrW(control, GWL_STYLE) & ES_READONLY as isize != 0
        {
            return false;
        }
        // 子窗口自己的 enabled 位不足以证明父容器可交互；逐级检查到捕获的原窗口。
        for _ in 0..64 {
            if control.is_invalid() || !IsWindowEnabled(control).as_bool() {
                return false;
            }
            if control == hwnd(target.window) {
                return true;
            }
            control = GetAncestor(control, GA_PARENT);
        }
    }
    false
}

pub(crate) fn native_edit_style(target: &Target) -> Option<u32> {
    if !native_control_valid(target) {
        return None;
    }
    unsafe {
        SetLastError(WIN32_ERROR(0));
        let style = GetWindowLongPtrW(hwnd(target.focus), GWL_STYLE);
        (style != 0 || GetLastError().0 == 0).then_some(style as u32)
    }
}

struct RestoreBackend<'a, G> {
    target: &'a Target,
    permitted: G,
    direct_only: bool,
}
impl<G: Fn() -> bool> crate::platform::focus_restore::Backend for RestoreBackend<'_, G> {
    type Attachment = Attached;
    fn snapshot(&mut self) -> crate::platform::focus_restore::Snapshot {
        crate::platform::focus_restore::Snapshot {
            focus: confirm(self.target),
            target_valid: valid(self.target),
            original_valid: original_control_valid(self.target),
            permitted: (self.permitted)(),
        }
    }
    fn activate(&mut self) -> ForegroundResult {
        if !(self.permitted)() || !valid(self.target) {
            return ForegroundResult {
                path: "permission_changed",
                success: false,
            };
        }
        if self.direct_only {
            unsafe {
                let _ = SetForegroundWindow(hwnd(self.target.window));
            }
            ForegroundResult {
                path: "direct_only",
                success: foreground() == self.target.window,
            }
        } else {
            activate(self.target.window)
        }
    }
    fn attach(&mut self) -> Attached {
        Attached::new(self.target.thread)
    }
    fn set_focus(&mut self) {
        unsafe {
            let _ = SetFocus(Some(hwnd(self.target.focus)));
        }
    }
}

pub fn restore(target: &Target) -> FocusResult {
    crate::platform::focus_restore::run(
        &mut RestoreBackend {
            target,
            permitted: || true,
            direct_only: false,
        },
        |_, _| {},
    )
}

pub fn restore_for_paste(
    target: &Target,
    permit: &crate::paste::permit::Permit,
    logger: &crate::log::Logger,
    session: u64,
    direct_only: bool,
) -> FocusResult {
    // 同一 opt-in 开关控制所有归还取证；默认路径不生成阶段 JSON。
    let started =
        super::foreground::enabled(std::env::var_os("NIANLAI_G0_FOREGROUND_TRACE").as_deref())
            .then(std::time::Instant::now);
    let mut backend = RestoreBackend {
        target,
        direct_only,
        permitted: || {
            if direct_only
                && !crate::paste::native::input_desktop_ready(
                    thread_input_desktop_receiving_input().ok(),
                    || permit.revoke(),
                )
            {
                return false;
            }
            if direct_only && !native_control_valid(target) {
                return false;
            }
            let current = foreground();
            permit.current(target, current) || permit.return_pending(target, current)
        },
    };
    let observe = |stage, snapshot: crate::platform::focus_restore::Snapshot| {
        if let Some(started) = started {
            logger.event("paste_restore_phase", serde_json::json!({
                    "session":session,"stage":stage,
                    "elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
                    "revision":permit.current_revision(),
                    "target_valid":snapshot.target_valid,
                    "target_foreground":snapshot.focus.foreground.success,
                    "original_control_valid":snapshot.original_valid,
                    "original_focus_restored":snapshot.original_valid && snapshot.focus.focus_restored,
                    "focus_unknown":snapshot.focus.focus_unknown,"permitted":snapshot.permitted,
                }));
        }
    };
    if direct_only {
        crate::platform::focus_restore::run_direct(&mut backend, observe)
    } else {
        crate::platform::focus_restore::run(&mut backend, observe)
    }
}

pub fn rect(window: usize) -> Result<RECT> {
    let mut rect = RECT::default();
    unsafe {
        GetWindowRect(hwnd(window), &mut rect).map_err(|e| e.to_string())?;
    }
    Ok(rect)
}
