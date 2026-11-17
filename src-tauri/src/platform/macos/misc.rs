pub mod disk {
    use std::{ffi::CString, mem::MaybeUninit, os::unix::ffi::OsStrExt, path::Path};

    fn stats(path: &Path) -> Option<libc::statfs> {
        let path = CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut stats = MaybeUninit::uninit();
        if unsafe { libc::statfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
            return None;
        }
        Some(unsafe { stats.assume_init() })
    }

    pub fn free_bytes(directory: &Path) -> Option<u64> {
        let stats = stats(directory)?;
        Some(stats.f_bavail * u64::from(stats.f_bsize))
    }

    pub fn is_remote(path: &str) -> bool {
        stats(Path::new(path)).is_some_and(|stats| stats.f_flags & libc::MNT_LOCAL as u32 == 0)
    }
}

pub mod time {
    use objc2::rc::{Retained, autoreleasepool};
    use objc2_foundation::{
        NSCalendar, NSCalendarIdentifierGregorian, NSCalendarUnit, NSDate, NSTimeZone,
    };

    fn local_calendar() -> Retained<NSCalendar> {
        let calendar = NSCalendar::calendarWithIdentifier(unsafe { NSCalendarIdentifierGregorian })
            .expect("Foundation must support the Gregorian calendar");
        calendar.setTimeZone(&NSTimeZone::localTimeZone());
        calendar
    }

    pub fn local_date() -> (u16, u8, u8) {
        autoreleasepool(|_| {
            let date = local_calendar().components_fromDate(
                NSCalendarUnit::Year | NSCalendarUnit::Month | NSCalendarUnit::Day,
                &NSDate::now(),
            );
            (date.year() as u16, date.month() as u8, date.day() as u8)
        })
    }

    pub fn local_day_start_ms() -> u64 {
        autoreleasepool(|_| {
            // Calendar 按本地时区解析当日起点，包含夏令时切换的日期。
            let midnight = local_calendar().startOfDayForDate(&NSDate::now());
            (midnight.timeIntervalSince1970() * 1000.0) as u64
        })
    }
}

pub mod hash {
    use crate::{history::content::Fingerprint, platform::Result};
    use sha2::{Digest, Sha256};

    pub fn sha256(bytes: &[u8]) -> Result<Fingerprint> {
        Ok(Sha256::digest(bytes).into())
    }
}

pub mod dialog {
    use std::{path::Path, process::Command};

    pub fn error(text: &str) {
        rfd::MessageDialog::new()
            .set_title("拈来")
            .set_description(text)
            .set_level(rfd::MessageLevel::Error)
            .show();
    }

    pub fn choose_backup(
        _owner: usize,
        save: bool,
        default_name: &str,
    ) -> Result<Option<String>, String> {
        let dialog = rfd::FileDialog::new().add_filter("拈来备份", &["nianlai-backup"]);
        let file = if save {
            dialog
                .set_title("导出备份")
                .set_file_name(default_name)
                .save_file()
        } else {
            dialog.set_title("选择要导入的备份").pick_file()
        };
        Ok(file.map(|path| path.to_string_lossy().into_owned()))
    }

    pub fn reveal(path: &Path) -> Result<(), String> {
        let status = Command::new("open")
            .arg("-R")
            .arg(path)
            .status()
            .map_err(|error| format!("无法打开访达：{error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err("无法打开访达".into())
        }
    }
}

pub mod memory {
    // macOS 没有 Windows 进程树采样，这里不需要。
    pub fn sample() -> serde_json::Value {
        serde_json::json!({"error": "macOS 暂不采样"})
    }
}

pub mod webview {
    use crate::{log::Logger, platform::Result};
    use std::{
        sync::{Arc, mpsc},
        time::{Duration, Instant},
    };
    use tauri::WebviewWindow;

    #[derive(Default)]
    pub struct MemoryState {}

    // macOS 没有 WebView2 挂起，这里不需要。
    pub fn set_visible(
        _panel: &WebviewWindow,
        _visible: bool,
        _logger: Logger,
        _session: u64,
        _state: Arc<MemoryState>,
    ) -> Result<()> {
        Ok(())
    }

    // macOS 没有 WebView2 合成等待，仍需通知隐藏操作可以继续。
    pub fn set_visible_with_completion(
        _panel: &WebviewWindow,
        _visible: bool,
        _logger: Logger,
        _session: u64,
        _state: Arc<MemoryState>,
        ready: Option<mpsc::Sender<Result<Instant>>>,
    ) -> Result<()> {
        let _ = ready.map(|sender| sender.send(Ok(Instant::now())));
        Ok(())
    }

    // macOS 没有 WebView2 重挂起，这里不需要。
    pub fn due(_state: &MemoryState, _interval: Duration) -> bool {
        false
    }

    // macOS 没有 WebView2 重挂起请求号，这里不需要。
    pub fn generation(_state: &MemoryState) -> u64 {
        0
    }

    // macOS 没有 WebView2 重挂起，这里不需要。
    pub fn resuspend(
        _panel: &WebviewWindow,
        _logger: Logger,
        _session: u64,
        _state: Arc<MemoryState>,
        _interval: Duration,
    ) -> Result<Option<u64>> {
        Ok(None)
    }

    pub fn focus(panel: &WebviewWindow) -> Result<()> {
        super::super::window::focus(panel)
    }

    pub fn initialize(panel: &WebviewWindow) -> Result<()> {
        super::super::window::initialize(panel)
    }
}

pub mod ime {
    // macOS 没有 Windows 输入法窗口，这里不需要。
    pub fn query(_window: usize) -> serde_json::Value {
        serde_json::json!({"error": "macOS 不读取 Windows 输入法状态"})
    }

    // macOS 没有 Windows 输入法窗口，这里不需要。
    pub fn query_focused() -> serde_json::Value {
        query(0)
    }
}

// macOS 没有 Windows UI Automation，这里不需要。
pub mod uia {}

pub mod queue_hook {
    use crate::platform::Result;
    use std::sync::{Arc, atomic::AtomicBool};

    // Beta 不拦截全局 ⌘V，仅保留核心状态机使用的事件名。
    #[allow(dead_code)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum QueueEvent {
        Paste,
        Cancel,
        Timeout,
    }
    pub struct QueueHooks;

    impl QueueHooks {
        // 只启用已有自动队列；macOS Beta 不拦截全局 ⌘V。
        pub fn install(
            _cancelled: Arc<AtomicBool>,
            _expired: impl Fn() -> bool + Send + 'static,
            _callback: impl Fn(QueueEvent) + Send + 'static,
        ) -> Result<Self> {
            Ok(Self)
        }

        // macOS Beta 没有手动逐张粘贴钩子，这里不需要。
        pub fn activate(&self) {}
    }
}

pub mod theme {
    // tray-icon 将 36px 图设为 18pt，保留 Retina 清晰度并由系统适配深浅色。
    pub(crate) const TRAY_TEMPLATE: &[u8] =
        include_bytes!("../../../icons/tray/macos-template-36.png");

    pub(crate) fn hotkey_label(value: &str) -> String {
        value
            .replace("Ctrl+", "⌃")
            .replace("Alt+", "⌥")
            .replace("Shift+", "⇧")
            .replace("Win+", "⌘")
    }

    // macOS 没有 Windows 任务栏主题，菜单栏由模板图标适配。
    pub fn taskbar_uses_light_theme() -> bool {
        true
    }

    // macOS 没有 Windows 任务栏小图标尺寸，这里不需要。
    pub(crate) fn current_small_icon_width(_tray: &tauri::tray::TrayIcon<tauri::Wry>) -> i32 {
        16
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn menu_hotkey_uses_physical_mac_modifiers() {
            assert_eq!(hotkey_label("Ctrl+Alt+Shift+Win+K"), "⌃⌥⇧⌘K");
        }

        #[test]
        fn menu_hotkey_formats_the_safe_default() {
            assert_eq!(hotkey_label("Ctrl+`"), "⌃`");
        }
    }
}

pub mod monitor {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSEvent, NSScreen};
    use objc2_foundation::{NSPoint, NSRect};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct WorkArea {
        pub left: i32,
        pub top: i32,
        pub right: i32,
        pub bottom: i32,
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub(crate) struct TargetMonitor {
        pub work: WorkArea,
        pub scale: f64,
    }

    pub(crate) fn current_target_monitor() -> Result<TargetMonitor, String> {
        let (_, work, _) = current_target_screen()?;
        Ok(logical_monitor(work))
    }

    fn logical_monitor(work: NSRect) -> TargetMonitor {
        // 引导使用逻辑坐标，跨不同缩放的显示器时不能用目标屏的比例缩放全局位置。
        TargetMonitor {
            work: WorkArea {
                left: work.origin.x.round() as i32,
                top: work.origin.y.round() as i32,
                right: (work.origin.x + work.size.width).round() as i32,
                bottom: (work.origin.y + work.size.height).round() as i32,
            },
            scale: 1.0,
        }
    }

    // AppKit 为左下原点；只在平台边界转为左上原点的逻辑坐标。
    pub(crate) fn logical_screen_rect(mut frame: NSRect, primary_top: f64) -> NSRect {
        frame.origin.y = primary_top - frame.origin.y - frame.size.height;
        frame
    }

    fn contains(frame: NSRect, point: NSPoint) -> bool {
        point.x >= frame.origin.x
            && point.x < frame.origin.x + frame.size.width
            && point.y >= frame.origin.y
            && point.y < frame.origin.y + frame.size.height
    }

    pub(crate) fn current_target_screen() -> Result<(NSPoint, NSRect, f64), String> {
        let mtm = MainThreadMarker::new().ok_or("显示器查询需要在主线程执行")?;
        let screens = NSScreen::screens(mtm);
        let primary = screens.firstObject().ok_or("没有可用的显示器")?;
        let primary_top = primary.frame().origin.y + primary.frame().size.height;
        let mouse = NSEvent::mouseLocation();
        let mouse = NSPoint::new(mouse.x, primary_top - mouse.y);
        let screen = screens
            .iter()
            .find(|screen| contains(logical_screen_rect(screen.frame(), primary_top), mouse))
            .unwrap_or(primary);
        let work = logical_screen_rect(screen.visibleFrame(), primary_top);
        Ok((mouse, work, screen.backingScaleFactor()))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use objc2_foundation::NSSize;

        #[test]
        fn converts_left_display_from_appkit_coordinates() {
            let work = logical_screen_rect(
                NSRect::new(NSPoint::new(-1920.0, 40.0), NSSize::new(1920.0, 1015.0)),
                1080.0,
            );
            let monitor = logical_monitor(work);
            assert_eq!(
                (
                    monitor.work.left,
                    monitor.work.top,
                    monitor.work.bottom,
                    monitor.scale
                ),
                (-1920, 25, 1040, 1.0)
            );
        }

        #[test]
        fn converts_upper_display_from_appkit_coordinates() {
            let work = logical_screen_rect(
                NSRect::new(NSPoint::new(0.0, 1120.0), NSSize::new(1920.0, 1015.0)),
                1080.0,
            );
            let monitor = logical_monitor(work);
            assert_eq!((monitor.work.top, monitor.work.bottom), (-1055, -40));
        }

        #[test]
        fn monitor_contains_top_edge_and_excludes_bottom_edge() {
            let frame = logical_screen_rect(
                NSRect::new(NSPoint::new(-1920.0, 0.0), NSSize::new(1920.0, 1080.0)),
                1080.0,
            );
            assert!(contains(frame, NSPoint::new(-100.0, 0.0)));
            assert!(!contains(frame, NSPoint::new(-100.0, 1080.0)));
            assert!(!contains(frame, NSPoint::new(0.0, 100.0)));
        }
    }
}
