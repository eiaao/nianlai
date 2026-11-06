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

/// 面板设计尺寸（96 DPI 逻辑像素）：v7 面板自然宽 680、高 520。
/// `show`（光标附近）与 `show_auto`（右下角 fallback）共用，避免两处各写一遍。
const PANEL_WIDTH: f64 = 680.0;
const PANEL_HEIGHT: f64 = 520.0;
/// 面板与光标/工作区边缘的间距（96 DPI 逻辑像素）。
const PANEL_GAP: i32 = 6;
/// 右下角 fallback 距工作区边缘的留白（96 DPI 逻辑像素）。
const PANEL_AUTO_MARGIN: f64 = 12.0;

/// 面板尺寸按显示器 DPI 缩放（保持原 `(值 * dpi / 96).round()` 语义），
/// 工作区放不下时按工作区自适应，至少 1px 防退化。
fn panel_size(work: &RECT, dpi_x: u32, dpi_y: u32) -> (i32, i32) {
    let width = scale_dpi(PANEL_WIDTH, dpi_x)
        .min(work.right - work.left)
        .max(1);
    let height = scale_dpi(PANEL_HEIGHT, dpi_y)
        .min(work.bottom - work.top)
        .max(1);
    (width, height)
}

fn scale_dpi(value: f64, dpi: u32) -> i32 {
    (value * dpi as f64 / 96.0).round() as i32
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Placement {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl Placement {
    /// 面板矩形任何情况下不超出工作区。（仅测试断言使用）
    #[cfg(test)]
    fn within_work(&self, work: &RECT) -> bool {
        self.x >= work.left
            && self.y >= work.top
            && self.x + self.width <= work.right
            && self.y + self.height <= work.bottom
    }
}

/// 光标附近的摆放位置（纯函数，便于测试）：垂直方向光标下方优先，放不下放上方，
/// 上下都不够时贴工作区顶部（保证面板顶栏与列表可见）；水平方向贴光标并夹紧在工作区内。
/// `anchor_top`/`anchor_bottom` 为光标矩形（无光标目标时传光标点，两者相等）。
fn place_near_anchor(
    work: &RECT,
    anchor_x: i32,
    anchor_top: i32,
    anchor_bottom: i32,
    gap: i32,
    width: i32,
    height: i32,
) -> Placement {
    let below = anchor_bottom + gap;
    let above = anchor_top - gap - height;
    let y = if below + height <= work.bottom {
        below
    } else if above >= work.top {
        above
    } else {
        work.top
    };
    // panel_size 保证 width <= 工作区宽，max 再防退化输入导致 clamp 区间倒置。
    let x = anchor_x.clamp(work.left, (work.right - width).max(work.left));
    Placement {
        x,
        y,
        width,
        height,
    }
}

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
    process_path(id)
        .and_then(|full| {
            Path::new(&full)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_default()
}

// 进程可执行文件的完整路径（A06 来源应用）；权限不够或进程已退出时返回 None。
pub fn process_path(id: u32) -> Option<String> {
    if id == 0 {
        return None;
    }
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, id).ok()?;
        let mut buffer = [0u16; 32768];
        let mut length = buffer.len() as u32;
        let result = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
        .is_ok();
        let _ = CloseHandle(process);
        result.then(|| String::from_utf16_lossy(&buffer[..length as usize]))
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
        // 无光标目标时以光标点为零高度锚点，走同一套下方优先 → 上方 → 贴顶规则。
        let (anchor_x, anchor_top, anchor_bottom) = match target.caret {
            Some(caret) => (caret.left, caret.top, caret.bottom),
            None => {
                let mut point = POINT::default();
                GetCursorPos(&mut point).map_err(|e| e.to_string())?;
                (point.x, point.y, point.y)
            }
        };
        let monitor = MonitorFromPoint(
            POINT {
                x: anchor_x,
                y: anchor_top,
            },
            MONITOR_DEFAULTTONEAREST,
        );
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
        let (width, height) = panel_size(&work, dpi_x, dpi_y);
        // 保持原整数截断语义（125% 缩放下 gap 与旧行为一致）。
        let gap = PANEL_GAP * (dpi_y as i32) / 96;
        let placement = place_near_anchor(
            &work,
            anchor_x,
            anchor_top,
            anchor_bottom,
            gap,
            width,
            height,
        );
        present(
            panel,
            placement.x,
            placement.y,
            placement.width,
            placement.height,
        )
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
        let margin_x = scale_dpi(PANEL_AUTO_MARGIN, dpi_x);
        let margin_y = scale_dpi(PANEL_AUTO_MARGIN, dpi_y);
        // 右下角 fallback 与光标附近呼出共用同一尺寸来源（680×520，DPI 缩放一致）。
        let width =
            scale_dpi(PANEL_WIDTH, dpi_x).min((work.right - work.left - 2 * margin_x).max(1));
        let height =
            scale_dpi(PANEL_HEIGHT, dpi_y).min((work.bottom - work.top - 2 * margin_y).max(1));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn work(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn panel_size_scales_with_dpi_and_caps_to_work_area() {
        // 100% DPI：680×520 原样。
        assert_eq!(panel_size(&work(0, 0, 1920, 1080), 96, 96), (680, 520));
        // 150% DPI：1020×780。
        assert_eq!(panel_size(&work(0, 0, 3000, 2000), 144, 144), (1020, 780));
        // 宽度不足：按工作区宽自适应；高度不足：按工作区高自适应。
        assert_eq!(panel_size(&work(0, 0, 500, 1080), 96, 96), (500, 520));
        assert_eq!(panel_size(&work(0, 0, 1920, 400), 96, 96), (680, 400));
        // 高 DPI 下工作区比缩放后尺寸小：同样自适应。
        assert_eq!(panel_size(&work(0, 0, 900, 700), 144, 144), (900, 700));
    }

    #[test]
    fn places_below_caret_when_space_allows() {
        let work_area = work(0, 0, 1920, 1080);
        let p = place_near_anchor(&work_area, 400, 300, 316, 6, 680, 520);
        assert_eq!(p.y, 322); // caret_bottom + gap
        assert_eq!(p.x, 400);
        assert!(p.within_work(&work_area));
    }

    #[test]
    fn places_above_caret_when_below_insufficient() {
        // 光标在屏幕底部，下方放不下 → 放上方。
        let work_area = work(0, 0, 1920, 1080);
        let p = place_near_anchor(&work_area, 400, 1000, 1016, 6, 680, 520);
        assert_eq!(p.y, 1000 - 6 - 520);
        assert!(p.within_work(&work_area));
    }

    #[test]
    fn sticks_to_work_top_when_neither_side_fits() {
        // 矮屏幕（可用高度 < 面板高度）：上下都不够 → 贴工作区顶，顶栏与列表可见。
        let work_area = work(0, 0, 1920, 400);
        let p = place_near_anchor(&work_area, 400, 180, 196, 6, 680, 400);
        assert_eq!(p.y, 0);
        assert!(p.within_work(&work_area));
        // 光标贴屏幕顶：上方同样不够 → 贴顶。
        let p = place_near_anchor(&work_area, 400, 0, 16, 6, 680, 400);
        assert_eq!(p.y, 0);
        // 任务栏在顶部（work.top = 40）的矮屏幕：贴的是工作区顶而不是屏幕顶。
        let work_area = work(0, 40, 1920, 440);
        let p = place_near_anchor(&work_area, 400, 220, 236, 6, 680, 400);
        assert_eq!(p.y, 40);
        assert!(p.within_work(&work_area));
    }

    #[test]
    fn adapts_width_and_clamps_x_when_work_area_narrow() {
        // 工作区比 680 窄：宽度自适应后贴右缘，不越界。
        let work_area = work(0, 0, 500, 1080);
        let p = place_near_anchor(&work_area, 480, 300, 316, 6, 500, 520);
        assert_eq!(p.width, 500);
        assert_eq!(p.x + p.width, 500);
        assert!(p.within_work(&work_area));
        // 锚点在工作区左缘外（多显示器边缘 case）→ 夹回左缘。
        let p = place_near_anchor(&work_area, -200, 300, 316, 6, 500, 520);
        assert_eq!(p.x, 0);
        assert!(p.within_work(&work_area));
    }

    #[test]
    fn never_escapes_work_area_at_edges() {
        // 光标在工作区四角与中心：任何情况下面板不超出工作区。
        let work_area = work(0, 0, 1920, 1080);
        for (x, top, bottom) in [(0, 0, 16), (1919, 0, 16), (0, 1079, 1080)] {
            let p = place_near_anchor(&work_area, x, top, bottom, 6, 680, 520);
            assert!(
                p.within_work(&work_area),
                "anchor=({x},{top},{bottom}) placement={p:?}"
            );
        }
        for (x, top, bottom) in [(1919, 1079, 1080), (960, 540, 556)] {
            let p = place_near_anchor(&work_area, x, top, bottom, 6, 680, 520);
            assert!(
                p.within_work(&work_area),
                "anchor=({x},{top},{bottom}) placement={p:?}"
            );
        }
        // 150% 缩放 + 任务栏占底部（工作区 2560×1040）：缩放尺寸经 panel_size 后仍不越界。
        let work_area = work(0, 0, 2560, 1040);
        for (x, top, bottom) in [(0, 0, 20), (2559, 1020, 1040), (1000, 500, 520)] {
            let (w, h) = panel_size(&work_area, 144, 144);
            let gap = PANEL_GAP * 144 / 96;
            let p = place_near_anchor(&work_area, x, top, bottom, gap, w, h);
            assert_eq!((w, h), (1020, 780));
            assert!(
                p.within_work(&work_area),
                "anchor=({x},{top},{bottom}) placement={p:?}"
            );
        }
    }

    #[test]
    fn no_caret_uses_cursor_point_as_zero_height_anchor() {
        // 无光标目标：以光标点为零高度锚点，下方优先 → 上方 → 贴顶同样生效。
        let work_area = work(0, 0, 1920, 1080);
        let p = place_near_anchor(&work_area, 800, 300, 300, 6, 680, 520);
        assert_eq!(p.y, 306); // 下方优先
        let p = place_near_anchor(&work_area, 800, 1000, 1000, 6, 680, 520);
        assert_eq!(p.y, 1000 - 6 - 520); // 底部放不下 → 上方
        let p = place_near_anchor(&work_area, 800, 1079, 1079, 6, 680, 520);
        assert_eq!(p.y, 1079 - 6 - 520); // 上方刚好放下
        let p = place_near_anchor(&work_area, 800, 1050, 1050, 6, 680, 520);
        assert_eq!(p.y, 1050 - 6 - 520); // 较矮锚点同样放上方
    }
}
