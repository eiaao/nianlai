use crate::tray::fire_settings_change;
use tauri::{Position, Rect, Size, Wry};
use windows::{
    Win32::{
        Foundation::{LPARAM, POINT},
        Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromPoint},
        System::Registry::{RRF_RT_REG_DWORD, RegGetValueW},
        UI::{
            HiDpi::{GetDpiForMonitor, GetSystemMetricsForDpi, MDT_EFFECTIVE_DPI},
            WindowsAndMessaging::{SM_CXSMICON, WM_DPICHANGED, WM_SETTINGCHANGE},
        },
    },
    core::PCWSTR,
};

/// 任务栏是浅色还是深色：读系统的 SystemUsesLightTheme；读不到按浅色处理（保守）。
pub(crate) const PERSONALIZE_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
pub(crate) const PERSONALIZE_KEY_WIDE: [u16; 64] = utf16(PERSONALIZE_KEY);

pub fn taskbar_uses_light_theme() -> bool {
    const VALUE: &[u16] = &utf16("SystemUsesLightTheme");
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let ok = unsafe {
        RegGetValueW(
            windows::Win32::System::Registry::HKEY_CURRENT_USER,
            PCWSTR(PERSONALIZE_KEY_WIDE.as_ptr()),
            PCWSTR(VALUE.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as _),
            Some(&mut size),
        )
    };
    let read = (ok.is_ok() && size == std::mem::size_of::<u32>() as u32).then_some(value);
    taskbar_theme_from_registry(read)
}

pub(crate) fn taskbar_theme_from_registry(value: Option<u32>) -> bool {
    // 只有系统明确给出 0 才用深色任务栏版；失败和异常值都回浅色版。
    value != Some(0)
}

const fn utf16(text: &str) -> [u16; 64] {
    let mut out = [0u16; 64];
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() && i < 63 {
        out[i] = bytes[i] as u16;
        i += 1;
    }
    out
}

fn physical_rect_center(rect: Rect) -> Option<(i32, i32)> {
    let (Position::Physical(position), Size::Physical(size)) = (rect.position, rect.size) else {
        return None;
    };
    let x = i64::from(position.x) + i64::from(size.width) / 2;
    let y = i64::from(position.y) + i64::from(size.height) / 2;
    Some((
        x.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        y.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
    ))
}

fn monitor_dpi(point: (i32, i32)) -> Option<u32> {
    let monitor = unsafe {
        MonitorFromPoint(
            POINT {
                x: point.0,
                y: point.1,
            },
            MONITOR_DEFAULTTONEAREST,
        )
    };
    let (mut dpi_x, mut dpi_y) = (0, 0);
    unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }.ok()?;
    (dpi_x > 0).then_some(dpi_x)
}

pub(crate) fn small_icon_width_from_dpi(dpi: Option<u32>, metric: impl FnOnce(u32) -> i32) -> i32 {
    const FALLBACK: i32 = 16;
    let Some(dpi) = dpi.filter(|dpi| *dpi > 0) else {
        return FALLBACK;
    };
    let width = metric(dpi);
    if width > 0 { width } else { FALLBACK }
}

pub(crate) fn small_icon_width_from_rect(
    rect: Option<Rect>,
    dpi_at: impl FnOnce((i32, i32)) -> Option<u32>,
    metric: impl FnOnce(u32) -> i32,
) -> i32 {
    let dpi = rect.and_then(physical_rect_center).and_then(dpi_at);
    small_icon_width_from_dpi(dpi, metric)
}

/// 每次刷新都通过 Tauri 实时调用 Shell_NotifyIconGetRect；失败回 16，绝不复用鼠标旧坐标。
pub(crate) fn current_small_icon_width(tray: &tauri::tray::TrayIcon<Wry>) -> i32 {
    small_icon_width_from_rect(tray.rect().ok().flatten(), monitor_dpi, |dpi| unsafe {
        GetSystemMetricsForDpi(SM_CXSMICON, dpi)
    })
}

/// 设置与 DPI 消息只请求轻量刷新；从不解引用 LPARAM，也从不消费原消息。
pub(crate) fn window_message(message: u32, _lp: LPARAM) -> bool {
    if message == WM_SETTINGCHANGE || message == WM_DPICHANGED {
        fire_settings_change();
    }
    false
}
