use crate::platform::{FocusResult, ForegroundResult, HitRegion, PlacementMode, Result, Target};
use objc2_app_kit::{NSPopUpMenuWindowLevel, NSWindowCollectionBehavior, NSWindowStyleMask};
use serde::Deserialize;
use std::sync::LazyLock;
use std::time::Instant;
use tauri::{LogicalPosition, LogicalSize, PhysicalSize, WebviewWindow};
use tauri_nspanel::{ManagerExt, WebviewWindowExt};

tauri_nspanel::tauri_panel! {
    panel!(NianlaiPanel {
        config: { can_become_key_window: true, can_become_main_window: false }
    })
}

pub(super) fn initialize(window: &WebviewWindow) -> Result<()> {
    MainThreadMarker::new().ok_or("面板初始化需要在主线程执行")?;
    let panel = window
        .to_panel::<NianlaiPanel>()
        .map_err(|error| error.to_string())?;
    panel
        .add_style_mask(NSWindowStyleMask::NonactivatingPanel)
        .map_err(|error| error.to_string())?;
    panel.set_level(NSPopUpMenuWindowLevel as i64);
    panel.set_collection_behavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    panel.set_floating_panel(true);
    panel.set_hides_on_deactivate(false);
    panel.set_becomes_key_only_if_needed(false);
    // 保留 Tao 的 delegate，由它继续转发面板失焦事件。
    super::hook::initialize(window);
    Ok(())
}

fn native(window: &WebviewWindow) -> Result<tauri_nspanel::PanelHandle<tauri::Wry>> {
    MainThreadMarker::new().ok_or("面板操作需要在主线程执行")?;
    window
        .get_webview_panel(window.label())
        .map_err(|error| format!("面板读取失败：{error:?}"))
}

pub(super) fn focus(window: &WebviewWindow) -> Result<()> {
    native(window)?.make_key_window();
    // Window::set_focus 会激活整个 App；这里只让 WKWebView 接收输入。
    let webview: &tauri::Webview = window.as_ref();
    webview.set_focus().map_err(|error| error.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeRegion {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct Avoid {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}
#[derive(Debug, Clone, Copy)]
pub struct Fit(&'static str);
impl Fit {
    pub fn name(self) -> &'static str {
        self.0
    }
}
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub scale: f64,
    pub fit: Fit,
}
impl Layout {
    pub fn caret(&self) -> Option<serde_json::Value> {
        None
    }
    // macOS 没有 Windows 命中区域，这里不需要。
    pub fn adjust(&self, _: NativeRegion) -> Option<Layout> {
        None
    }
}
pub struct Shown {
    pub cursor_known: bool,
    pub layout: Layout,
}
pub fn convert_avoid(avoid: Avoid, scale: f64) -> Result<NativeRegion> {
    Ok(NativeRegion {
        left: (avoid.left * scale) as i32,
        top: (avoid.top * scale) as i32,
        right: ((avoid.left + avoid.width) * scale) as i32,
        bottom: ((avoid.top + avoid.height) * scale) as i32,
    })
}
// macOS 没有 Windows 命中区域，这里不需要。
pub fn validate_hit_regions(_: &[HitRegion]) -> Result<()> {
    Ok(())
}
// macOS 没有 Windows 命中区域，这里不需要。
pub fn set_hit_regions(
    _: &WebviewWindow,
    _: &[HitRegion],
    _: bool,
) -> Result<Option<NativeRegion>> {
    Ok(None)
}
// macOS 没有 Windows 命中区域，这里不需要。
pub fn clear_hit_regions(_: &WebviewWindow) {}
pub fn window_scale(panel: &WebviewWindow) -> f64 {
    match panel.scale_factor() {
        Ok(scale) => scale,
        Err(error) => {
            log::error!("窗口缩放读取失败：{error}");
            1.0
        }
    }
}
pub fn default_avoid(layout: &Layout) -> NativeRegion {
    NativeRegion {
        left: 0,
        top: 0,
        right: layout.width,
        bottom: layout.height,
    }
}
pub fn handle(_: &WebviewWindow) -> Result<usize> {
    Ok(std::process::id() as usize)
}
pub fn foreground() -> usize {
    super::apps::frontmost().map_or(0, |app| app.pid as usize)
}
pub fn tick_count() -> u32 {
    static START: LazyLock<Instant> = LazyLock::new(Instant::now);
    START.elapsed().as_millis() as u32
}
pub fn capture() -> Target {
    match super::apps::frontmost() {
        Some(app) => Target {
            window: app.pid as usize,
            focus: app.pid as usize,
            process: app.pid,
            process_name: app.bundle,
            ..Target::default()
        },
        None => Target::default(),
    }
}
// macOS 没有 WS_EX_NOACTIVATE，这里不需要。
pub fn no_activate(_: &WebviewWindow, _: bool) -> Result<()> {
    Ok(())
}
fn layout(mouse: NSPoint, work: NSRect, scale: f64, mode: PlacementMode) -> Layout {
    let width = 960.0_f64.min(work.size.width);
    let height = 540.0_f64.min(work.size.height);
    let (x, y, fit) = if mode == PlacementMode::Center {
        (
            work.origin.x + (work.size.width - width) / 2.0,
            work.origin.y + (work.size.height - height) / 2.0,
            Fit("center"),
        )
    } else {
        let below = mouse.y + 6.0;
        let (y, fit) = if below + height <= work.origin.y + work.size.height {
            (below, Fit("below"))
        } else {
            (mouse.y - height - 6.0, Fit("above"))
        };
        (mouse.x + 6.0, y, fit)
    };
    Layout {
        x: (x.clamp(work.origin.x, work.origin.x + work.size.width - width) * scale).round() as i32,
        y: (y.clamp(work.origin.y, work.origin.y + work.size.height - height) * scale).round()
            as i32,
        width: (width * scale).round() as i32,
        height: (height * scale).round() as i32,
        scale,
        fit,
    }
}

pub fn show(panel: &WebviewWindow, _: &Target, mode: PlacementMode) -> Result<Shown> {
    let (mouse, work, scale) = super::monitor::current_target_screen()?;
    let layout = layout(mouse, work, scale, mode);
    panel
        .set_size(LogicalSize::new(
            f64::from(layout.width) / layout.scale,
            f64::from(layout.height) / layout.scale,
        ))
        .map_err(|error| error.to_string())?;
    // 跨 Retina / 非 Retina 屏时，物理坐标会被 Tao 按旧屏缩放换算。
    panel
        .set_position(LogicalPosition::new(
            f64::from(layout.x) / layout.scale,
            f64::from(layout.y) / layout.scale,
        ))
        .map_err(|error| error.to_string())?;
    native(panel)?.order_front_regardless();
    focus(panel)?;
    Ok(Shown {
        cursor_known: true,
        layout,
    })
}
pub fn show_auto(panel: &WebviewWindow) -> Result<()> {
    show(panel, &Target::default(), PlacementMode::Center).map(|_| ())
}
pub fn show_queue(panel: &WebviewWindow, target: &Target) -> Result<bool> {
    show(panel, target, PlacementMode::Mouse).map(|shown| shown.cursor_known)
}
pub fn move_to(panel: &WebviewWindow, x: i32, y: i32) -> Result<()> {
    let scale = panel.scale_factor().map_err(|error| error.to_string())?;
    panel
        .set_position(LogicalPosition::new(
            f64::from(x) / scale,
            f64::from(y) / scale,
        ))
        .map_err(|error| error.to_string())
}
pub fn hide(panel: &WebviewWindow) -> Result<()> {
    native(panel)?.hide();
    Ok(())
}
pub fn show_no_activate(panel: &WebviewWindow) -> Result<()> {
    panel.show().map_err(|error| error.to_string())
}
pub fn present_guide(panel: &WebviewWindow, x: i32, y: i32, width: i32, height: i32) -> Result<()> {
    panel
        .set_size(PhysicalSize::new(width as u32, height as u32))
        .map_err(|error| error.to_string())?;
    move_to(panel, x, y)?;
    panel.show().map_err(|error| error.to_string())
}
#[allow(clippy::too_many_arguments)]
pub fn present_guide_initial(
    panel: &WebviewWindow,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    _: &[HitRegion],
    _: f64,
) -> Result<()> {
    present_guide(panel, x, y, width, height)
}
pub fn activate(window: usize) -> ForegroundResult {
    ForegroundResult {
        path: "macos",
        success: foreground() == window,
    }
}
pub fn valid(target: &Target) -> bool {
    objc2_app_kit::NSRunningApplication::runningApplicationWithProcessIdentifier(
        target.process as i32,
    )
    .is_some()
}
pub fn confirm(target: &Target) -> FocusResult {
    FocusResult {
        foreground: activate(target.window),
        focus_restored: foreground() == target.window,
        focus_unknown: false,
    }
}
pub fn confirm_strict(target: &Target) -> FocusResult {
    confirm(target)
}
pub fn restore(target: &Target) -> FocusResult {
    confirm(target)
}
pub fn restore_for_paste(
    target: &Target,
    _: &crate::paste::permit::Permit,
    _: bool,
) -> FocusResult {
    confirm(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_placement_keeps_left_screen_coordinates_at_retina_scale() {
        let placed = layout(
            NSPoint::new(-1700.0, 250.0),
            NSRect::new(NSPoint::new(-1920.0, 0.0), NSSize::new(1920.0, 1080.0)),
            2.0,
            PlacementMode::Mouse,
        );
        assert_eq!((placed.x, placed.y), (-3388, 512));
    }

    #[test]
    fn center_placement_keeps_upper_screen_coordinates() {
        let placed = layout(
            NSPoint::new(400.0, -500.0),
            NSRect::new(NSPoint::new(0.0, -1080.0), NSSize::new(1920.0, 1056.0)),
            1.0,
            PlacementMode::Center,
        );
        assert_eq!((placed.x, placed.y), (480, -822));
    }

    #[test]
    fn panel_fits_small_visible_work_area() {
        let placed = layout(
            NSPoint::new(799.0, 470.0),
            NSRect::new(NSPoint::new(0.0, 24.0), NSSize::new(800.0, 450.0)),
            1.0,
            PlacementMode::Mouse,
        );
        assert_eq!(
            (placed.x, placed.y, placed.width, placed.height),
            (0, 24, 800, 450)
        );
    }
}
