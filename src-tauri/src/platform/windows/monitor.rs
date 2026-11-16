use std::mem::size_of;
use windows::Win32::{
    Foundation::{POINT, RECT},
    Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
        MonitorFromPoint,
    },
    UI::{
        HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
        WindowsAndMessaging::GetCursorPos,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WorkArea {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl From<RECT> for WorkArea {
    fn from(rect: RECT) -> Self {
        Self {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TargetMonitor {
    pub work: WorkArea,
    pub scale: f64,
}

pub(crate) fn current_target_monitor() -> Result<TargetMonitor, String> {
    unsafe {
        let mut point = POINT::default();
        let primary = GetCursorPos(&mut point).is_err();
        let monitor = MonitorFromPoint(
            point,
            if primary {
                MONITOR_DEFAULTTOPRIMARY
            } else {
                MONITOR_DEFAULTTONEAREST
            },
        );
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return Err(windows::core::Error::from_thread().to_string());
        }
        // 与 panel 一致：先由指针选定目标 monitor，再读同一 monitor 的 DPI。
        // 绝不能读尚未移过去的新窗口 scale，那仍是创建屏的 DPI。
        let (mut dpi_x, mut dpi_y) = (96, 96);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        Ok(TargetMonitor {
            work: info.rcWork.into(),
            scale: dpi_x as f64 / 96.0,
        })
    }
}
