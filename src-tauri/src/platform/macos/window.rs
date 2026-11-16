use crate::platform::{FocusResult, ForegroundResult, HitRegion, PlacementMode, Result, Target};
use serde::Deserialize;
use std::sync::LazyLock;
use std::time::Instant;
use tauri::{PhysicalPosition, PhysicalSize, WebviewWindow};

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
pub struct Fit;
impl Fit {
    pub fn name(self) -> &'static str {
        "center"
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
pub fn show(panel: &WebviewWindow, _: &Target, _: PlacementMode) -> Result<Shown> {
    let monitor = panel
        .current_monitor()
        .map_err(|error| error.to_string())?
        .ok_or("monitor_missing")?;
    let scale = monitor.scale_factor();
    let width = (960.0 * scale) as i32;
    let height = (540.0 * scale) as i32;
    panel
        .set_size(PhysicalSize::new(width as u32, height as u32))
        .map_err(|error| error.to_string())?;
    panel.center().map_err(|error| error.to_string())?;
    panel.show().map_err(|error| error.to_string())?;
    let position = panel.outer_position().map_err(|error| error.to_string())?;
    Ok(Shown {
        cursor_known: false,
        layout: Layout {
            x: position.x,
            y: position.y,
            width,
            height,
            scale,
            fit: Fit,
        },
    })
}
pub fn show_auto(panel: &WebviewWindow) -> Result<()> {
    show(panel, &Target::default(), PlacementMode::Center).map(|_| ())
}
pub fn show_queue(panel: &WebviewWindow, target: &Target) -> Result<bool> {
    show(panel, target, PlacementMode::Mouse).map(|shown| shown.cursor_known)
}
pub fn move_to(panel: &WebviewWindow, x: i32, y: i32) -> Result<()> {
    panel
        .set_position(PhysicalPosition::new(x, y))
        .map_err(|error| error.to_string())
}
pub fn hide(panel: &WebviewWindow) -> Result<()> {
    panel.hide().map_err(|error| error.to_string())
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
