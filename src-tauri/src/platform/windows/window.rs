use crate::platform::{
    Caret, FocusResult, ForegroundResult, HitRegion, PlacementMode, Result, Target,
};
use std::{
    collections::HashMap,
    mem::size_of,
    path::Path,
    sync::{Mutex, OnceLock},
};
use tauri::WebviewWindow;
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, GetLastError, HANDLE, HWND, POINT, RECT, SetLastError, WIN32_ERROR,
        },
        Graphics::Gdi::{
            ClientToScreen, CombineRgn, CreateRectRgn, DeleteObject, GetMonitorInfoW,
            MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
            RGN_ERROR, RGN_OR, ScreenToClient, SetWindowRgn,
        },
        System::{
            StationsAndDesktops::{GetThreadDesktop, GetUserObjectInformationW, UOI_IO},
            Threading::{
                AttachThreadInput, GetCurrentThreadId, OpenProcess, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
            },
        },
        UI::{
            HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI},
            Input::KeyboardAndMouse::{IsWindowEnabled, SetFocus},
            WindowsAndMessaging::*,
        },
    },
    core::PWSTR,
};

/// 手牌设计尺寸（96 DPI 逻辑像素）。透明边距留给抽出的牌、牌堆和情境提示。
const PANEL_WIDTH: f64 = 960.0;
const PANEL_HEIGHT: f64 = 540.0;
const QUEUE_WIDTH: f64 = 420.0;
const QUEUE_HEIGHT: f64 = 52.0;
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

/// 客户区里的一个矩形（物理像素，半开区间）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeRegion {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}
impl NativeRegion {
    fn contains(self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
    fn union(self, other: Self) -> Self {
        Self {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }
}
#[derive(Default)]
struct HitState {
    armed: bool,
    regions: Vec<NativeRegion>,
}
static HIT_REGIONS: OnceLock<Mutex<HashMap<usize, HitState>>> = OnceLock::new();
// 钩子线程也会读：锁中毒（别处 panic）时照样取出数据，不能在钩子回调里再 panic。
fn hit_regions() -> std::sync::MutexGuard<'static, HashMap<usize, HitState>> {
    HIT_REGIONS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

/// 点击穿透只靠窗口区域：区域外的点不属于这个窗口，系统直接把点击交给下面的窗口。
/// 不再切换 WS_EX_TRANSPARENT（跨线程改样式会同步等主线程）。全部在主线程调用。
fn apply_native_region(window: HWND, regions: &[NativeRegion], armed: bool) -> Result<()> {
    unsafe {
        if !armed {
            if SetWindowRgn(window, None, true) == 0 {
                return Err("清除窗口区域失败".into());
            }
            return Ok(());
        }
        let union = CreateRectRgn(0, 0, 0, 0);
        if union.is_invalid() {
            return Err("创建窗口区域失败".into());
        }
        for region in regions {
            let next = CreateRectRgn(region.left, region.top, region.right, region.bottom);
            let combined = !next.is_invalid()
                && CombineRgn(Some(union), Some(union), Some(next), RGN_OR) != RGN_ERROR;
            if !next.is_invalid() {
                let _ = DeleteObject(next.into());
            }
            if !combined {
                let _ = DeleteObject(union.into());
                return Err("合并窗口区域失败".into());
            }
        }
        // 成功时区域归系统所有，不能再释放；失败时所有权没转移，必须自己释放。
        if SetWindowRgn(window, Some(union), true) == 0 {
            let _ = DeleteObject(union.into());
            return Err("设置窗口区域失败".into());
        }
        Ok(())
    }
}

/// 命中区域四周多留的边（96 DPI 逻辑像素），盖住阴影和抗锯齿边缘。
const HIT_MARGIN: f64 = 8.0;

/// 前端上报的 CSS 像素矩形换成客户区物理像素（纯函数）。`scale` 是窗口 DPI / 96。
/// 返回（加了边的命中区域，未加边的内容包围盒）。
fn convert_regions(
    regions: &[HitRegion],
    scale: f64,
) -> Result<(Vec<NativeRegion>, Option<NativeRegion>)> {
    if !scale.is_finite() || !(0.25..=8.0).contains(&scale) {
        return Err("命中区域缩放比例无效".into());
    }
    let margin = (HIT_MARGIN * scale).round();
    let mut hits = Vec::with_capacity(regions.len());
    let mut bounds: Option<NativeRegion> = None;
    for region in regions {
        let values = [region.left, region.top, region.right, region.bottom];
        if values
            .iter()
            .any(|value| !value.is_finite() || value.abs() > 100_000.0)
            || region.right <= region.left
            || region.bottom <= region.top
        {
            return Err("命中区域坐标无效".into());
        }
        let content = NativeRegion {
            left: (region.left * scale).floor() as i32,
            top: (region.top * scale).floor() as i32,
            right: (region.right * scale).ceil() as i32,
            bottom: (region.bottom * scale).ceil() as i32,
        };
        bounds = Some(bounds.map_or(content, |bounds| bounds.union(content)));
        hits.push(NativeRegion {
            left: (region.left * scale - margin).floor() as i32,
            top: (region.top * scale - margin).floor() as i32,
            right: (region.right * scale + margin).ceil() as i32,
            bottom: (region.bottom * scale + margin).ceil() as i32,
        });
    }
    Ok((hits, bounds))
}

/// 前端上报的避让外形（CSS 像素，客户区坐标）。
#[derive(Clone, Copy, Debug, serde::Deserialize, PartialEq)]
pub struct Avoid {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

/// 避让外形换成客户区物理像素（纯函数）；无效（非有限、非正尺寸、过大）返回 Err。
pub fn convert_avoid(avoid: Avoid, scale: f64) -> Result<NativeRegion> {
    let region = HitRegion {
        left: avoid.left,
        top: avoid.top,
        right: avoid.left + avoid.width,
        bottom: avoid.top + avoid.height,
    };
    convert_regions(&[region], scale)?
        .1
        .ok_or_else(|| "避让外形无效".into())
}

/// 只校验，不碰窗口：命令在派发到主线程之前用它把错误直接还给前端。
pub fn validate_hit_regions(regions: &[HitRegion]) -> Result<()> {
    convert_regions(regions, 1.0).map(|_| ())
}

/// 窗口当前 DPI 的缩放（DPI / 96）。读不到按 100%。
pub fn window_scale(panel: &WebviewWindow) -> f64 {
    let dpi = handle(panel)
        .map(|window| unsafe { GetDpiForWindow(hwnd(window)) })
        .unwrap_or(0);
    if dpi == 0 { 1.0 } else { dpi as f64 / 96.0 }
}

/// 设置命中区域（主线程）。缩放按窗口 DPI 算，不信前端的 devicePixelRatio。
/// 返回内容包围盒（客户区物理像素，未加边）；未武装或没有区域时为 None。
pub fn set_hit_regions(
    panel: &WebviewWindow,
    regions: &[HitRegion],
    armed: bool,
) -> Result<Option<NativeRegion>> {
    // 武装但一个区域都没有（前端还没渲染出来）按清除处理：整窗可点，而不是整窗消失。
    let armed = armed && !regions.is_empty();
    let (hits, bounds) = if armed {
        convert_regions(regions, window_scale(panel))?
    } else {
        (Vec::new(), None)
    };
    let window = handle(panel)?;
    hit_regions().insert(
        window,
        HitState {
            armed,
            regions: hits.clone(),
        },
    );
    if let Err(error) = apply_native_region(hwnd(window), &hits, armed) {
        // 区域没设上：撤掉记账，窗口保持整块可点，不出现“看得见点不到”。
        hit_regions().remove(&window);
        let _ = apply_native_region(hwnd(window), &[], false);
        return Err(error);
    }
    Ok(bounds)
}

pub fn clear_hit_regions(panel: &WebviewWindow) {
    if let Ok(window) = handle(panel) {
        hit_regions().remove(&window);
        let _ = apply_native_region(hwnd(window), &[], false);
    }
}

/// 返回 true 表示这个屏幕点落在窗口的穿透部分（区域外）。未武装时整个窗口都算面板。
/// 只在鼠标按下时由钩子线程调用：加一次锁、一次坐标换算，不发窗口消息。
pub fn passes_through(window: usize, screen_x: i32, screen_y: i32) -> bool {
    let state = hit_regions();
    let Some(state) = state.get(&window) else {
        return false;
    };
    if !state.armed {
        return false;
    }
    let mut point = POINT {
        x: screen_x,
        y: screen_y,
    };
    if !unsafe { ScreenToClient(hwnd(window), &mut point) }.as_bool() {
        return false;
    }
    !state
        .regions
        .iter()
        .any(|region| region.contains(point.x, point.y))
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

/// 呼出时的默认避让外形：必须与 src/lib/cards.ts 的 fanOutline() 逐值相同（推导见
/// src/lib/README.md），96 DPI 逻辑像素、窗口客户区坐标 (left, top, right, bottom)。
/// 前端首次上报扇面外形时与此相同，avoid_update 判为无变化，窗口不滑动。
/// 已选清单、提示等临时元素按契约不计入外形。
const DEFAULT_AVOID: (f64, f64, f64, f64) = (28.0, 27.0, 957.0, 529.0);

/// 内容相对光标行最后落在哪里（日志和前端用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// 内容整块在光标行下方（首选）。
    Below,
    /// 内容整块在光标行上方。
    Above,
    /// 上下都放不下：内容在光标右侧，压住光标行右边但不盖光标。
    Right,
    /// 同上，在左侧。
    Left,
    /// 哪儿都放不下：内容贴着空间较大一侧的工作区边缘，可能盖住光标行。
    Overlap,
    /// 屏幕中央模式或光标读不到：不做避让。
    Center,
}

impl Fit {
    pub fn name(self) -> &'static str {
        match self {
            Self::Below => "below",
            Self::Above => "above",
            Self::Right => "right",
            Self::Left => "left",
            Self::Overlap => "overlap",
            Self::Center => "center",
        }
    }
    // 避让好坏：整行让开 > 只让开光标 > 盖住。
    fn rank(self) -> u8 {
        match self {
            Self::Below | Self::Above => 2,
            Self::Right | Self::Left => 1,
            Self::Overlap | Self::Center => 0,
        }
    }
}

/// 光标行（输入光标矩形；鼠标旁时是零高度的指针点）。屏幕物理像素。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub x: i32,
    pub top: i32,
    pub bottom: i32,
}

/// 在区间 [low, high] 里取一个值：先取 `preferred`，窗口能整个留在 [inside_low, inside_high]
/// 时尽量留在里面。调用方保证 low <= high。
fn pick(low: i32, high: i32, preferred: i32, inside_low: i32, inside_high: i32) -> i32 {
    let preferred = preferred.clamp(low, high);
    if inside_low <= inside_high {
        preferred.clamp(
            inside_low.max(low).min(high),
            inside_high.min(high).max(low),
        )
    } else {
        preferred
    }
}

/// 按内容包围盒避开光标行的摆放（纯函数）。`content` 是内容在窗口客户区里的位置。
/// 只有内容必须留在工作区内；内容以外的透明部分（区域外、点击穿透）可以伸出工作区，
/// 这样小屏和高缩放下也能让开光标行。回退顺序：
/// 1. 下方：内容整块在光标行下方（离光标最近）；
/// 2. 上方：内容整块在光标行上方；
/// 3. 侧边：上下都放不下时，内容放在光标右侧或左侧（空间大的一侧先试），竖直方向对准光标行；
/// 4. 覆盖：都放不下时，内容贴到上下空间较大一侧的工作区边缘，尽量少盖。
///
/// 水平方向（1、2）窗口左边对齐光标并夹在工作区内（与原来一致）。
fn place_content(
    work: &RECT,
    row: Row,
    gap: i32,
    width: i32,
    height: i32,
    content: NativeRegion,
) -> (Placement, Fit) {
    let content = clamp_content(content, width, height);
    let inside_x = (work.left, work.right - width);
    let inside_y = (work.top, work.bottom - height);
    let default_x = row.x.clamp(work.left, (work.right - width).max(work.left));
    let content_x = (work.left - content.left, work.right - content.right);
    let content_y = (work.top - content.top, work.bottom - content.bottom);
    let x_for_rows = if content_x.0 <= content_x.1 {
        pick(content_x.0, content_x.1, default_x, inside_x.0, inside_x.1)
    } else {
        work.left - content.left
    };
    let placed = |x, y, fit| {
        (
            Placement {
                x,
                y,
                width,
                height,
            },
            fit,
        )
    };
    if content_x.0 <= content_x.1 {
        // 1. 下方
        let low = (row.bottom + gap - content.top).max(content_y.0);
        if low <= content_y.1 {
            return placed(
                x_for_rows,
                pick(low, content_y.1, low, inside_y.0, inside_y.1),
                Fit::Below,
            );
        }
        // 2. 上方
        let high = (row.top - gap - content.bottom).min(content_y.1);
        if content_y.0 <= high {
            return placed(
                x_for_rows,
                pick(content_y.0, high, high, inside_y.0, inside_y.1),
                Fit::Above,
            );
        }
    }
    // 3. 侧边
    let middle = (row.top + row.bottom) / 2 - (content.top + content.bottom) / 2;
    let side_y = if content_y.0 <= content_y.1 {
        pick(content_y.0, content_y.1, middle, inside_y.0, inside_y.1)
    } else {
        content_y.0
    };
    let right = (row.x + gap - content.left).max(content_x.0);
    let left = (row.x - gap - content.right).min(content_x.1);
    let sides = [
        (right <= content_x.1).then(|| {
            (
                pick(right, content_x.1, right, inside_x.0, inside_x.1),
                Fit::Right,
            )
        }),
        (content_x.0 <= left).then(|| {
            (
                pick(content_x.0, left, left, inside_x.0, inside_x.1),
                Fit::Left,
            )
        }),
    ];
    let right_first = work.right - row.x >= row.x - work.left;
    let order = if right_first { [0, 1] } else { [1, 0] };
    for index in order {
        if let Some((x, fit)) = sides[index] {
            return placed(x, side_y, fit);
        }
    }
    // 4. 覆盖
    let y = if work.bottom - row.bottom >= row.top - work.top {
        content_y.1.max(content_y.0.min(content_y.1))
    } else {
        content_y.0
    };
    placed(x_for_rows, y, Fit::Overlap)
}

fn clamp_content(content: NativeRegion, width: i32, height: i32) -> NativeRegion {
    let left = content.left.clamp(0, width);
    let top = content.top.clamp(0, height);
    let clamped = NativeRegion {
        left,
        top,
        right: content.right.clamp(left, width),
        bottom: content.bottom.clamp(top, height),
    };
    if clamped.right <= clamped.left || clamped.bottom <= clamped.top {
        NativeRegion {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        }
    } else {
        clamped
    }
}

/// 窗口在 (x, y) 时内容对光标行的避让等级（与 Fit::rank 同一标尺）。内容出了工作区算最差。
fn achieved(work: &RECT, row: Row, gap: i32, x: i32, y: i32, content: NativeRegion) -> Option<u8> {
    let (left, top, right, bottom) = (
        x + content.left,
        y + content.top,
        x + content.right,
        y + content.bottom,
    );
    if left < work.left || top < work.top || right > work.right || bottom > work.bottom {
        return None;
    }
    let row_clear = bottom <= row.top - gap || top >= row.bottom + gap;
    let caret_clear = right <= row.x - gap || left >= row.x + gap;
    Some(if row_clear {
        2
    } else if caret_clear {
        1
    } else {
        0
    })
}

/// 呼出后的摆放状态：前端上报新的内容包围盒（比如从扇面变成摊开）时用它重新判断要不要挪。
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    work: RECT,
    row: Option<Row>,
    gap: i32,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// DPI / 96。
    pub scale: f64,
    pub fit: Fit,
}

impl Layout {
    /// 光标行相对窗口客户区的位置（CSS 像素，可能在窗口外）；中央模式或光标未知时为 None。
    pub fn caret(&self) -> Option<serde_json::Value> {
        let row = self.row?;
        let css = |value: i32| ((value as f64) / self.scale * 10.0).round() / 10.0;
        Some(serde_json::json!({
            "left": css(row.x - self.x),
            "top": css(row.top - self.y),
            "bottom": css(row.bottom - self.y),
        }))
    }

    /// 内容包围盒变了：当前位置的避让等级不比重新计算的差就不动（不来回跳），
    /// 否则返回新位置。返回 None 表示不用挪。
    pub fn adjust(&self, content: NativeRegion) -> Option<Layout> {
        let row = self.row?;
        let content = clamp_content(content, self.width, self.height);
        let (fresh, fit) =
            place_content(&self.work, row, self.gap, self.width, self.height, content);
        let current = achieved(&self.work, row, self.gap, self.x, self.y, content);
        if current.is_some_and(|current| current >= fit.rank()) {
            return None;
        }
        if (fresh.x, fresh.y) == (self.x, self.y) {
            return None;
        }
        Some(Layout {
            x: fresh.x,
            y: fresh.y,
            fit,
            ..*self
        })
    }
}

/// 只移动、不改大小和层次、不激活（主线程调用）。
pub fn move_to(panel: &WebviewWindow, x: i32, y: i32) -> Result<()> {
    unsafe {
        SetWindowPos(
            hwnd(handle(panel)?),
            None,
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        )
        .map_err(|e| e.to_string())
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

/// 摆放方式：光标矩形旁 / 以指针点为零高度锚点 / 工作区居中。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Anchor {
    Rect { x: i32, top: i32, bottom: i32 },
    Center,
}

/// 按呼出位置设置决定锚点，以及用哪个点选屏幕（纯函数，便于测试）。
/// 鼠标旁、屏幕中央总在指针所在屏；输入光标旁在光标所在屏，找不到光标时退回鼠标旁。
fn resolve_anchor(
    mode: PlacementMode,
    caret: Option<Caret>,
    pointer: (i32, i32),
) -> (Anchor, (i32, i32)) {
    match (mode, caret) {
        (PlacementMode::Caret, Some(caret)) => (
            Anchor::Rect {
                x: caret.left,
                top: caret.top,
                bottom: caret.bottom,
            },
            (caret.left, caret.top),
        ),
        (PlacementMode::Center, _) => (Anchor::Center, pointer),
        _ => (
            Anchor::Rect {
                x: pointer.0,
                top: pointer.1,
                bottom: pointer.1,
            },
            pointer,
        ),
    }
}

/// 呼出锚点（含降级，纯函数，便于测试）。前台是管理员窗口或安全桌面时 GetCursorPos 会
/// 返回 0x80070005：这种“目标未知”不让呼出失败——输入光标旁若已知仍照常锚定，否则退回
/// 主显示器工作区居中（`primary` 为 true 时调用方用 MONITOR_DEFAULTTOPRIMARY 选主显示器）。
fn resolve_anchor_or_degrade(
    mode: PlacementMode,
    caret: Option<Caret>,
    pointer: Option<(i32, i32)>,
) -> (Anchor, (i32, i32), bool) {
    if let (PlacementMode::Caret, Some(caret)) = (mode, caret) {
        return (
            Anchor::Rect {
                x: caret.left,
                top: caret.top,
                bottom: caret.bottom,
            },
            (caret.left, caret.top),
            false,
        );
    }
    match pointer {
        Some(pointer) => {
            let (anchor, probe) = resolve_anchor(mode, caret, pointer);
            (anchor, probe, false)
        }
        None => (Anchor::Center, (0, 0), true),
    }
}

/// 呼出结果：cursor_known=false 表示光标/前台信息读不到（管理员前台、安全桌面），位置已按
/// “目标未知”降级，但面板照常呼出。粘贴时的完整性判断不受影响。
pub struct Shown {
    pub cursor_known: bool,
    pub layout: Layout,
}

/// 屏幕中央：在工作区内水平垂直居中。
fn place_center(work: &RECT, width: i32, height: i32) -> Placement {
    Placement {
        x: work.left + ((work.right - work.left - width) / 2).max(0),
        y: work.top + ((work.bottom - work.top - height) / 2).max(0),
        width,
        height,
    }
}

/// 默认避让外形（客户区物理像素）；窗口被工作区压小时按比例收进窗口。
fn default_content(width: i32, height: i32, dpi_y: u32) -> NativeRegion {
    let (left, top, right, bottom) = DEFAULT_AVOID;
    // 与前端上报走同一个换算（convert_avoid），保证同一外形在任何缩放下换算结果逐像素相同。
    let region = convert_avoid(
        Avoid {
            left,
            top,
            width: right - left,
            height: bottom - top,
        },
        dpi_y as f64 / 96.0,
    )
    .expect("默认避让外形是有效矩形");
    // 前端按客户区左上角绝对定位；窗口被工作区压小时超出的部分本来就看不到，夹进窗口即可。
    clamp_content(region, width, height)
}

/// 窗口当前的默认避让外形（呼出后、前端还没上报时用来比较“是否和上次不同”）。
pub fn default_avoid(layout: &Layout) -> NativeRegion {
    default_content(
        layout.width,
        layout.height,
        (layout.scale * 96.0).round() as u32,
    )
}

fn place(
    work: &RECT,
    anchor: Anchor,
    gap: i32,
    width: i32,
    height: i32,
    content: NativeRegion,
) -> (Placement, Fit) {
    match anchor {
        Anchor::Rect { x, top, bottom } => {
            place_content(work, Row { x, top, bottom }, gap, width, height, content)
        }
        Anchor::Center => (place_center(work, width, height), Fit::Center),
    }
}

pub fn show(panel: &WebviewWindow, target: &Target, mode: PlacementMode) -> Result<Shown> {
    no_activate(panel, true)?;
    clear_hit_regions(panel);
    unsafe {
        let mut point = POINT::default();
        let cursor = GetCursorPos(&mut point).is_ok();
        let (anchor, probe, primary) =
            resolve_anchor_or_degrade(mode, target.caret, cursor.then_some((point.x, point.y)));
        let monitor = MonitorFromPoint(
            POINT {
                x: probe.0,
                y: probe.1,
            },
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
        let (mut dpi_x, mut dpi_y) = (96, 96);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let work = info.rcWork;
        let (width, height) = panel_size(&work, dpi_x, dpi_y);
        // 保持原整数截断语义（125% 缩放下 gap 与旧行为一致）。
        let gap = PANEL_GAP * (dpi_y as i32) / 96;
        let content = default_content(width, height, dpi_y);
        let (placement, fit) = place(&work, anchor, gap, width, height, content);
        present(
            panel,
            placement.x,
            placement.y,
            placement.width,
            placement.height,
        )?;
        Ok(Shown {
            cursor_known: cursor,
            layout: Layout {
                work,
                row: match anchor {
                    Anchor::Rect { x, top, bottom } => Some(Row { x, top, bottom }),
                    Anchor::Center => None,
                },
                gap,
                x: placement.x,
                y: placement.y,
                width,
                height,
                scale: dpi_y as f64 / 96.0,
                fit,
            },
        })
    }
}

pub fn show_auto(panel: &WebviewWindow) -> Result<()> {
    no_activate(panel, true)?;
    clear_hit_regions(panel);
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
        // 右下角 fallback 与光标附近呼出共用同一尺寸来源（960×540，DPI 缩放一致）。
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

fn place_queue(
    work: &RECT,
    anchor_x: i32,
    anchor_top: i32,
    anchor_bottom: i32,
    gap: i32,
    width: i32,
    height: i32,
) -> Placement {
    let center_y = (anchor_top + anchor_bottom - height) / 2;
    let right = anchor_x + gap;
    let left = anchor_x - gap - width;
    let (x, y) = if right + width <= work.right {
        (right, center_y)
    } else if left >= work.left {
        (left, center_y)
    } else {
        let x = (anchor_x - width / 2).clamp(work.left, (work.right - width).max(work.left));
        let below = anchor_bottom + gap;
        let above = anchor_top - gap - height;
        let y = if below + height <= work.bottom {
            below
        } else if above >= work.top {
            above
        } else {
            center_y.clamp(work.top, (work.bottom - height).max(work.top))
        };
        (x, y)
    };
    Placement {
        x,
        y: y.clamp(work.top, (work.bottom - height).max(work.top)),
        width,
        height,
    }
}

/// 逐张放小指示的锚点（含降级，纯函数，便于测试）：优先输入光标行，其次光标点；两者都读
/// 不到（管理员前台/安全桌面）时按主显示器工作区右缘竖直居中降级，呼出不因此失败。
fn queue_anchor(caret: Option<Caret>, pointer: Option<(i32, i32)>) -> (QueueAnchor, bool) {
    match (caret, pointer) {
        (Some(caret), _) => (
            QueueAnchor::At {
                x: caret.left,
                top: caret.top,
                bottom: caret.bottom,
            },
            false,
        ),
        (None, Some((x, y))) => (
            QueueAnchor::At {
                x,
                top: y,
                bottom: y,
            },
            false,
        ),
        (None, None) => (QueueAnchor::PrimaryRight, true),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueueAnchor {
    At {
        x: i32,
        top: i32,
        bottom: i32,
    },
    /// 主显示器工作区右缘竖直居中（光标与光标信息都读不到时的降级位置）。
    PrimaryRight,
}

/// 逐张放只保留一条小指示，优先放在光标所在行右侧，左侧放不下再换边，绝不抢焦点。
/// 队列期间窗口区域只有这条小指示：先清掉手牌的命中区域和穿透样式，× 按钮才点得到，
/// 样式不会卡在武装手牌时留下的 WS_EX_TRANSPARENT 上。
/// 返回光标是否读得到。
pub fn show_queue(panel: &WebviewWindow, target: &Target) -> Result<bool> {
    no_activate(panel, true)?;
    clear_hit_regions(panel);
    unsafe {
        let mut pointer = POINT::default();
        let cursor = GetCursorPos(&mut pointer).is_ok();
        let (anchor, primary) =
            queue_anchor(target.caret, cursor.then_some((pointer.x, pointer.y)));
        // 锚点所在屏；降级时主显示器。
        let probe = match anchor {
            QueueAnchor::At { x, top, .. } => POINT { x, y: top },
            QueueAnchor::PrimaryRight => POINT::default(),
        };
        let monitor = MonitorFromPoint(
            probe,
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
        let (mut dpi_x, mut dpi_y) = (96, 96);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let work = info.rcWork;
        let width = scale_dpi(QUEUE_WIDTH, dpi_x)
            .min(work.right - work.left)
            .max(1);
        let height = scale_dpi(QUEUE_HEIGHT, dpi_y)
            .min(work.bottom - work.top)
            .max(1);
        let gap = PANEL_GAP * dpi_y as i32 / 96;
        let (x, top, bottom) = match anchor {
            QueueAnchor::At { x, top, bottom } => (x, top, bottom),
            QueueAnchor::PrimaryRight => {
                let middle = work.top + (work.bottom - work.top) / 2;
                (work.right, middle, middle)
            }
        };
        let placement = place_queue(&work, x, top, bottom, gap, width, height);
        present(
            panel,
            placement.x,
            placement.y,
            placement.width,
            placement.height,
        )?;
        Ok(cursor)
    }
}

fn present(panel: &WebviewWindow, x: i32, y: i32, width: i32, height: i32) -> Result<()> {
    // 与 main 一致：先在隐藏状态下摆好位置和大小，再显示，再重新确认置顶。
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
    clear_hit_regions(panel);
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
    direct_only: bool,
) -> FocusResult {
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
    if direct_only {
        crate::platform::focus_restore::run_direct(&mut backend, |_, _| {})
    } else {
        crate::platform::focus_restore::run(&mut backend, |_, _| {})
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

    fn full(width: i32, height: i32) -> NativeRegion {
        NativeRegion {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        }
    }

    // 内容占满整个窗口时的摆放（旧测试沿用：下方、上方的结果与原来逐像素一致）。
    fn place_near_anchor(
        work: &RECT,
        x: i32,
        top: i32,
        bottom: i32,
        gap: i32,
        width: i32,
        height: i32,
    ) -> Placement {
        place_content(
            work,
            Row { x, top, bottom },
            gap,
            width,
            height,
            full(width, height),
        )
        .0
    }

    fn place(work: &RECT, anchor: Anchor, gap: i32, width: i32, height: i32) -> Placement {
        super::place(work, anchor, gap, width, height, full(width, height)).0
    }

    #[test]
    fn panel_size_scales_with_dpi_and_caps_to_work_area() {
        // 100% DPI：960×540 原样。
        assert_eq!(panel_size(&work(0, 0, 1920, 1080), 96, 96), (960, 540));
        // 150% DPI：1440×810。
        assert_eq!(panel_size(&work(0, 0, 3000, 2000), 144, 144), (1440, 810));
        // 宽度不足：按工作区宽自适应；高度不足：按工作区高自适应。
        assert_eq!(panel_size(&work(0, 0, 500, 1080), 96, 96), (500, 540));
        assert_eq!(panel_size(&work(0, 0, 1920, 400), 96, 96), (960, 400));
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
            assert_eq!((w, h), (1440, 810));
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

    #[test]
    fn mouse_mode_anchors_at_pointer_even_with_caret() {
        let caret = Some(Caret {
            left: 100,
            top: 200,
            bottom: 216,
        });
        let (anchor, probe) = resolve_anchor(PlacementMode::Mouse, caret, (900, 500));
        assert_eq!(
            anchor,
            Anchor::Rect {
                x: 900,
                top: 500,
                bottom: 500
            }
        );
        assert_eq!(probe, (900, 500));
        let work_area = work(0, 0, 1920, 1080);
        let p = place(&work_area, anchor, 6, 680, 520);
        assert_eq!((p.x, p.y), (900, 506));
    }

    #[test]
    fn caret_mode_uses_caret_and_falls_back_to_pointer() {
        let caret = Some(Caret {
            left: 100,
            top: 200,
            bottom: 216,
        });
        let (anchor, probe) = resolve_anchor(PlacementMode::Caret, caret, (900, 500));
        assert_eq!(
            anchor,
            Anchor::Rect {
                x: 100,
                top: 200,
                bottom: 216
            }
        );
        assert_eq!(probe, (100, 200));
        let (anchor, probe) = resolve_anchor(PlacementMode::Caret, None, (900, 500));
        assert_eq!(
            anchor,
            Anchor::Rect {
                x: 900,
                top: 500,
                bottom: 500
            }
        );
        assert_eq!(probe, (900, 500));
    }

    #[test]
    fn center_mode_centers_in_pointer_screen_work_area() {
        let caret = Some(Caret {
            left: 100,
            top: 200,
            bottom: 216,
        });
        let (anchor, probe) = resolve_anchor(PlacementMode::Center, caret, (2500, 300));
        assert_eq!(anchor, Anchor::Center);
        // 多屏：用指针所在屏（右侧副屏 1920..3840）的工作区，而不是光标所在屏。
        assert_eq!(probe, (2500, 300));
        let second = work(1920, 0, 3840, 1040);
        let p = place(&second, anchor, 6, 680, 520);
        assert_eq!((p.x, p.y), (1920 + 620, 260));
        assert!(p.within_work(&second));
    }

    #[test]
    fn center_mode_shrunk_panel_stays_inside_small_work_area() {
        let small = work(0, 0, 500, 400);
        let p = place_center(&small, 500, 400);
        assert_eq!((p.x, p.y), (0, 0));
        assert!(p.within_work(&small));
        // 负坐标的左侧副屏。
        let left = work(-1920, 0, 0, 1080);
        let p = place_center(&left, 680, 520);
        assert_eq!((p.x, p.y), (-1920 + 620, 280));
        assert!(p.within_work(&left));
    }

    #[test]
    fn mouse_mode_pulls_panel_back_in_at_screen_edges() {
        let screen = work(0, 0, 1920, 1040);
        // 右下角：右侧夹紧、下方放不下改放上方。
        let (anchor, _) = resolve_anchor(PlacementMode::Mouse, None, (1915, 1035));
        let p = place(&screen, anchor, 6, 680, 520);
        assert!(p.within_work(&screen), "{p:?}");
        assert_eq!(p.x, 1920 - 680);
        assert_eq!(p.y, 1035 - 6 - 520);
        // 左上角。
        let (anchor, _) = resolve_anchor(PlacementMode::Mouse, None, (0, 0));
        let p = place(&screen, anchor, 6, 680, 520);
        assert!(p.within_work(&screen), "{p:?}");
        // 副屏左边缘（负坐标）。
        let left = work(-1920, 0, 0, 1080);
        let (anchor, _) = resolve_anchor(PlacementMode::Mouse, None, (-5, 540));
        let p = place(&left, anchor, 6, 680, 520);
        assert!(p.within_work(&left), "{p:?}");
    }

    #[test]
    fn queue_pill_uses_row_side_then_falls_back_above_or_below() {
        let area = work(0, 0, 1920, 1080);
        let right = place_queue(&area, 600, 300, 320, 20, 420, 52);
        assert_eq!((right.x, right.y), (620, 284));
        let left = place_queue(&area, 1700, 300, 320, 20, 420, 52);
        assert_eq!(left.x, 1700 - 20 - 420);
        let narrow = work(0, 0, 700, 1080);
        let below = place_queue(&narrow, 350, 300, 320, 20, 420, 52);
        assert!(below.y >= 320, "两侧都放不下时不能压住当前行：{below:?}");
    }

    // 呼出容错（审查问题 5）：前台是管理员窗口或安全桌面时 GetCursorPos 返回 0x80070005，
    // 呼出不硬失败——按“目标未知”降级到主显示器工作区居中；光标旁且输入光标已知仍照常。
    #[test]
    fn show_degrades_to_primary_center_when_cursor_is_unreadable() {
        let caret = Some(Caret {
            left: 100,
            top: 200,
            bottom: 216,
        });
        let (anchor, _, primary) = resolve_anchor_or_degrade(PlacementMode::Caret, caret, None);
        assert_eq!(
            anchor,
            Anchor::Rect {
                x: 100,
                top: 200,
                bottom: 216
            }
        );
        assert!(!primary, "输入光标旁且光标已知：不降级");
        for mode in [PlacementMode::Mouse, PlacementMode::Center] {
            let (anchor, _, primary) = resolve_anchor_or_degrade(mode, caret, None);
            assert_eq!(anchor, Anchor::Center, "{mode:?}");
            assert!(primary, "{mode:?}：降级到主显示器");
        }
        let (anchor, _, primary) = resolve_anchor_or_degrade(PlacementMode::Caret, None, None);
        assert_eq!(anchor, Anchor::Center);
        assert!(primary, "找不到光标目标时同样降级");
        // 光标可读时与原来一致。
        let (anchor, probe, primary) =
            resolve_anchor_or_degrade(PlacementMode::Mouse, caret, Some((900, 500)));
        assert_eq!(
            anchor,
            Anchor::Rect {
                x: 900,
                top: 500,
                bottom: 500
            }
        );
        assert_eq!(probe, (900, 500));
        assert!(!primary);
        // 降级（居中）位置也在工作区内。
        let area = work(0, 0, 1920, 1080);
        let placed = place(&area, Anchor::Center, 9, 960, 540);
        assert!(placed.within_work(&area), "{placed:?}");
    }

    #[test]
    fn queue_pill_degrades_to_primary_right_when_cursor_is_unreadable() {
        let caret = Some(Caret {
            left: 100,
            top: 200,
            bottom: 216,
        });
        match queue_anchor(caret, None) {
            (QueueAnchor::At { x, top, bottom }, false) => {
                assert_eq!((x, top, bottom), (100, 200, 216));
            }
            other => panic!("光标已知时小指示照常锚定：{other:?}"),
        }
        match queue_anchor(None, Some((800, 300))) {
            (QueueAnchor::At { x, top, bottom }, false) => {
                assert_eq!((x, top, bottom), (800, 300, 300));
            }
            other => panic!("{other:?}"),
        }
        let (anchor, primary) = queue_anchor(None, None);
        assert!(matches!(anchor, QueueAnchor::PrimaryRight));
        assert!(primary, "降级时用主显示器");
        // 降级锚点（主显示器工作区右缘竖直居中）经 place_queue 后仍在工作区内。
        let area = work(0, 0, 1920, 1080);
        let middle = (area.top + area.bottom) / 2;
        let placed = place_queue(&area, area.right, middle, middle, 20, 420, 52);
        assert!(placed.within_work(&area), "{placed:?}");
    }

    #[test]
    fn native_hit_regions_use_half_open_client_rectangles() {
        let region = NativeRegion {
            left: 10,
            top: 20,
            right: 30,
            bottom: 40,
        };
        assert!(region.contains(10, 20));
        assert!(region.contains(29, 39));
        assert!(!region.contains(30, 39));
        assert!(!region.contains(29, 40));
    }

    struct Screen {
        name: &'static str,
        work: RECT,
        dpi: u32,
    }

    fn screens() -> [Screen; 4] {
        [
            Screen {
                name: "1920x1080@100%",
                work: work(0, 0, 1920, 1040),
                dpi: 96,
            },
            Screen {
                name: "1920x1080@125%",
                work: work(0, 0, 1920, 1032),
                dpi: 120,
            },
            Screen {
                name: "1920x1080@150%",
                work: work(0, 0, 1920, 1020),
                dpi: 144,
            },
            Screen {
                name: "1366x768@100%",
                work: work(0, 0, 1366, 728),
                dpi: 96,
            },
        ]
    }

    fn rows(screen: &Screen) -> [(&'static str, Row); 3] {
        let line = scale_dpi(20.0, screen.dpi);
        let w = &screen.work;
        let mid = (w.top + w.bottom) / 2;
        [
            (
                "top",
                Row {
                    x: 300,
                    top: w.top + 30,
                    bottom: w.top + 30 + line,
                },
            ),
            (
                "middle",
                Row {
                    x: w.right / 2,
                    top: mid,
                    bottom: mid + line,
                },
            ),
            (
                "bottom",
                Row {
                    x: w.right - 200,
                    top: w.bottom - 40 - line,
                    bottom: w.bottom - 40,
                },
            ),
        ]
    }

    // 默认避让外形 = 扇面（含拈起的最高一张）的真实外形，按 DPI 缩放。
    #[test]
    fn default_avoid_is_the_fan_with_a_lifted_card() {
        // 与 cards.ts fanOutline() 相同：{left 28, top 27, width 929, height 502}。
        assert_eq!(
            default_content(960, 540, 96),
            NativeRegion {
                left: 28,
                top: 27,
                right: 957,
                bottom: 529
            }
        );
        // 任何缩放下都与前端上报同一外形的换算结果相同，呼出后首次上报不会被判为变化。
        let fan = Avoid {
            left: 28.0,
            top: 27.0,
            width: 929.0,
            height: 502.0,
        };
        for dpi in [96u32, 120, 144, 168, 192] {
            let scale = dpi as f64 / 96.0;
            let (w, h) = ((960.0 * scale) as i32, (540.0 * scale) as i32);
            assert_eq!(
                default_content(w, h, dpi),
                clamp_content(convert_avoid(fan, scale).unwrap(), w, h),
                "dpi {dpi}"
            );
        }
        // 窗口被工作区压小：夹进窗口。
        assert_eq!(
            default_content(900, 500, 96),
            NativeRegion {
                left: 28,
                top: 27,
                right: 900,
                bottom: 500
            }
        );
    }

    // 问题 11：常见分辨率和缩放下，光标在上方或下方时扇面整块让开光标行；光标在正中时
    // 外形太高（约占窗口 92%），除 1920×1080@100% 能放到上方外只能退到贴边覆盖，内容仍在工作区内。
    #[test]
    fn default_fan_clears_the_caret_row_unless_it_is_mid_screen() {
        for screen in screens() {
            let (width, height) = panel_size(&screen.work, screen.dpi, screen.dpi);
            let gap = PANEL_GAP * screen.dpi as i32 / 96;
            let content = default_content(width, height, screen.dpi);
            for (name, row) in rows(&screen) {
                let (p, fit) = place_content(&screen.work, row, gap, width, height, content);
                let rank = achieved(&screen.work, row, gap, p.x, p.y, content);
                assert!(
                    rank.is_some(),
                    "{} {name}: 内容出了工作区 {p:?}",
                    screen.name
                );
                assert_eq!(rank, Some(fit.rank()), "{} {name}: {fit:?}", screen.name);
                if name != "middle" {
                    assert_eq!(rank, Some(2), "{} {name}: {fit:?} {p:?}", screen.name);
                }
                if name == "middle" && screen.name == "1920x1080@100%" {
                    assert_eq!(fit, Fit::Above);
                }
            }
        }
        let screen = &screens()[0];
        let (width, height) = panel_size(&screen.work, 96, 96);
        let content = default_content(width, height, 96);
        let row = Row {
            x: 900,
            top: 500,
            bottom: 520,
        };
        let (p, fit) = place_content(&screen.work, row, 6, width, height, content);
        assert_eq!(fit, Fit::Below, "放得下时优先下方");
        assert_eq!(p.y + content.top, 526);
    }

    fn layout_for(screen: &Screen, row: Row) -> (Layout, NativeRegion) {
        let (width, height) = panel_size(&screen.work, screen.dpi, screen.dpi);
        let gap = PANEL_GAP * screen.dpi as i32 / 96;
        let fan = default_content(width, height, screen.dpi);
        let (p, fit) = place_content(&screen.work, row, gap, width, height, fan);
        let layout = Layout {
            work: screen.work,
            row: Some(row),
            gap,
            x: p.x,
            y: p.y,
            width,
            height,
            scale: screen.dpi as f64 / 96.0,
            fit,
        };
        (layout, fan)
    }

    // 合并审查第 1 条：首次上报的扇面外形与默认外形相同（或落在其中）时不挪窗口。
    #[test]
    fn reporting_the_default_fan_or_a_smaller_shape_never_moves() {
        for screen in screens() {
            for (name, row) in rows(&screen) {
                let (layout, fan) = layout_for(&screen, row);
                assert!(layout.adjust(fan).is_none(), "{} {name}", screen.name);
                let lifted_less = NativeRegion {
                    top: fan.top + 36,
                    ..fan
                };
                assert!(
                    layout.adjust(lifted_less).is_none(),
                    "{} {name}",
                    screen.name
                );
                let narrower = NativeRegion {
                    left: fan.left + 80,
                    right: fan.right - 20,
                    ..fan
                };
                assert!(layout.adjust(narrower).is_none(), "{} {name}", screen.name);
            }
        }
    }

    // 摊开姿态（约 616×514 的一列）在 150% 下上下都放不下：退到侧边，至少不盖光标。
    #[test]
    fn tall_spread_content_falls_back_to_the_side_then_overlap() {
        let screen = &screens()[2];
        let (width, height) = panel_size(&screen.work, 144, 144);
        let gap = PANEL_GAP * 144 / 96;
        let column_w = scale_dpi(616.0, 144);
        let column_h = scale_dpi(514.0, 144);
        let spread = NativeRegion {
            left: (width - column_w) / 2,
            top: height - column_h,
            right: (width + column_w) / 2,
            bottom: height,
        };
        let row = Row {
            x: 960,
            top: 500,
            bottom: 530,
        };
        let (p, fit) = place_content(&screen.work, row, gap, width, height, spread);
        assert!(matches!(fit, Fit::Right | Fit::Left), "{fit:?}");
        assert_eq!(achieved(&screen.work, row, gap, p.x, p.y, spread), Some(1));
        // 光标在屏幕正中、左右也都放不下（内容比半屏还宽）：贴边覆盖，内容仍在工作区内。
        let wide = NativeRegion {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        let (p, fit) = place_content(&screen.work, row, gap, width, height, wide);
        assert_eq!(fit, Fit::Overlap);
        assert!(achieved(&screen.work, row, gap, p.x, p.y, wide).is_some());
    }

    #[test]
    fn layout_moves_when_the_spread_shape_can_avoid_better() {
        let screen = &screens()[2];
        let row = Row {
            x: 960,
            top: 500,
            bottom: 530,
        };
        let (layout, _) = layout_for(screen, row);
        assert_eq!(layout.fit, Fit::Overlap, "150% 正中：扇面只能贴边覆盖");
        let (width, height) = (layout.width, layout.height);
        let column_w = scale_dpi(616.0, 144);
        let spread = NativeRegion {
            left: (width - column_w) / 2,
            top: height - scale_dpi(514.0, 144),
            right: (width + column_w) / 2,
            bottom: height,
        };
        let moved = layout.adjust(spread).expect("摊开后能侧移让开光标");
        assert!(matches!(moved.fit, Fit::Right | Fit::Left));
        assert!(moved.adjust(spread).is_none(), "到位后不再来回挪");
        let centered = Layout {
            row: None,
            fit: Fit::Center,
            ..layout
        };
        assert!(centered.adjust(spread).is_none(), "中央模式永远不挪");
        assert!(centered.caret().is_none());
    }

    #[test]
    fn avoid_converts_with_window_scale_without_margin() {
        let avoid = Avoid {
            left: 0.0,
            top: 35.0,
            width: 960.0,
            height: 495.0,
        };
        assert_eq!(
            convert_avoid(avoid, 1.5).unwrap(),
            NativeRegion {
                left: 0,
                top: 52,
                right: 1440,
                bottom: 795
            }
        );
        assert!(
            convert_avoid(
                Avoid {
                    width: 0.0,
                    ..avoid
                },
                1.0
            )
            .is_err()
        );
        assert!(
            convert_avoid(
                Avoid {
                    left: f64::INFINITY,
                    ..avoid
                },
                1.0
            )
            .is_err()
        );
    }

    #[test]
    fn caret_position_is_reported_relative_to_the_window_in_css_pixels() {
        let layout = Layout {
            work: work(0, 0, 1920, 1020),
            row: Some(Row {
                x: 600,
                top: 450,
                bottom: 480,
            }),
            gap: 9,
            x: 300,
            y: 150,
            width: 1440,
            height: 810,
            scale: 1.5,
            fit: Fit::Below,
        };
        assert_eq!(
            layout.caret().unwrap(),
            serde_json::json!({"left":200.0,"top":200.0,"bottom":220.0})
        );
    }

    // 问题 10：命中区域按窗口 DPI 换算，包围盒不含边距；坐标无效直接报错。
    #[test]
    fn hit_regions_convert_with_window_scale_and_report_content_bounds() {
        let regions = [
            HitRegion {
                left: 10.0,
                top: 300.0,
                right: 110.0,
                bottom: 540.0,
            },
            HitRegion {
                left: 200.0,
                top: 250.5,
                right: 300.0,
                bottom: 540.0,
            },
        ];
        let (hits, bounds) = convert_regions(&regions, 1.5).unwrap();
        assert_eq!(
            hits[0],
            NativeRegion {
                left: 3,
                top: 438,
                right: 177,
                bottom: 822
            },
            "四周各多 12px（8 × 1.5）"
        );
        assert_eq!(
            bounds,
            Some(NativeRegion {
                left: 15,
                top: 375,
                right: 450,
                bottom: 810
            })
        );
        assert_eq!(convert_regions(&[], 1.0).unwrap(), (Vec::new(), None));
        let bad = HitRegion {
            left: 5.0,
            top: 0.0,
            right: 5.0,
            bottom: 1.0,
        };
        assert!(convert_regions(&[bad], 1.0).is_err());
        let nan = HitRegion {
            left: f64::NAN,
            top: 0.0,
            right: 5.0,
            bottom: 1.0,
        };
        assert!(validate_hit_regions(&[nan]).is_err());
        assert!(convert_regions(&regions, 0.0).is_err());
    }
}
