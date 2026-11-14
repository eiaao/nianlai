use crate::{
    app::Controller,
    capture::rules::gate,
    health::{HealthSnapshot, TrayState, select_tray},
    log::Logger,
    panel::{self, Action},
    platform,
    settings::Store,
    settings_window,
};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tauri::{
    AppHandle, Emitter, Manager, Position, Rect, Size, Wry,
    menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem},
    plugin::TauriPlugin,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_global_shortcut::Modifiers;
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

const OPEN_PANEL: &str = "tray_open_panel";
const STATUS: &str = "tray_status";
const SETTINGS: &str = "tray_settings";
const AUTOSTART: &str = "tray_autostart";
const PAUSE: &str = "tray_pause";
const IGNORE_NEXT: &str = "tray_ignore_next";
const ABOUT: &str = "tray_about";
const QUIT: &str = "tray_quit";
// Windows 原生菜单用文本中的 Tab 显示右侧键位；加速器始终为空，不注册第二个快捷键。
const OPEN_PANEL_ACCELERATOR: Option<&str> = None;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrayMenuEntry {
    Action(&'static str),
    Separator,
}

/// 生产 Menu::with_items 与测试共用这一份布局，不允许测试另写顺序自证。
const TRAY_MENU_LAYOUT: [TrayMenuEntry; 12] = [
    TrayMenuEntry::Action(STATUS),
    TrayMenuEntry::Separator,
    TrayMenuEntry::Action(OPEN_PANEL),
    TrayMenuEntry::Separator,
    TrayMenuEntry::Action(PAUSE),
    TrayMenuEntry::Action(IGNORE_NEXT),
    TrayMenuEntry::Separator,
    TrayMenuEntry::Action(SETTINGS),
    TrayMenuEntry::Action(AUTOSTART),
    TrayMenuEntry::Action(ABOUT),
    TrayMenuEntry::Separator,
    TrayMenuEntry::Action(QUIT),
];

fn hotkey_label(value: &str) -> String {
    const FALLBACK: &str = "Ctrl+`";
    if value.chars().any(char::is_control) {
        return FALLBACK.into();
    }
    let Ok(shortcut) = crate::shortcut::parse(value) else {
        return FALLBACK.into();
    };
    let mut parts = Vec::new();
    for (modifier, label) in [
        (Modifiers::CONTROL, "Ctrl"),
        (Modifiers::ALT, "Alt"),
        (Modifiers::SHIFT, "Shift"),
        (Modifiers::SUPER, "Win"),
    ] {
        if shortcut.mods.contains(modifier) {
            parts.push(label.to_owned());
        }
    }
    let key = shortcut.key.to_string();
    let key = match key.as_str() {
        "Backquote" => "`".into(),
        "ArrowUp" => "↑".into(),
        "ArrowDown" => "↓".into(),
        "ArrowLeft" => "←".into(),
        "ArrowRight" => "→".into(),
        _ => key
            .strip_prefix("Key")
            .filter(|key| key.len() == 1)
            .map(str::to_owned)
            .or_else(|| key.strip_prefix("Digit").map(str::to_owned))
            .or_else(|| key.strip_prefix("Numpad").map(|key| format!("小键盘{key}")))
            .unwrap_or(key),
    };
    parts.push(key);
    parts.join("+")
}

/// 生产菜单构造、后续刷新和测试共用；Tab 只是菜单文字，accelerator 参数仍为 None。
fn open_panel_label(hotkey: &str) -> String {
    format!("打开拈来\t{}", hotkey_label(hotkey))
}

/// refresh 与测试共用同一 setter 路径；测试用 fake sink 观察真实刷新调用，而非另算标签自证。
fn refresh_open_panel_label(hotkey: &str, set_text: impl FnOnce(String)) {
    set_text(open_panel_label(hotkey));
}

// 静态文字由生产构造与测试共同读取；动态状态和暂停文字仍由 tray_view 刷新。
fn static_menu_label(id: &str) -> Option<&'static str> {
    match id {
        IGNORE_NEXT => Some("下一次复制不记"),
        AUTOSTART => Some("开机时启动"),
        ABOUT => Some("关于拈来"),
        _ => None,
    }
}

// 托盘三态资源（v12 方案 A，由 tools/build_icons.py 生成）：16/24 手绘，32 由 16 最近邻放大。
const RECORDING_16: &[u8] = include_bytes!("../icons/tray/recording-16.png");
const RECORDING_24: &[u8] = include_bytes!("../icons/tray/recording-24.png");
const RECORDING_32: &[u8] = include_bytes!("../icons/tray/recording-32.png");
const PAUSED_LIGHT_16: &[u8] = include_bytes!("../icons/tray/paused-light-16.png");
const PAUSED_LIGHT_24: &[u8] = include_bytes!("../icons/tray/paused-light-24.png");
const PAUSED_LIGHT_32: &[u8] = include_bytes!("../icons/tray/paused-light-32.png");
const PAUSED_DARK_16: &[u8] = include_bytes!("../icons/tray/paused-dark-16.png");
const PAUSED_DARK_24: &[u8] = include_bytes!("../icons/tray/paused-dark-24.png");
const PAUSED_DARK_32: &[u8] = include_bytes!("../icons/tray/paused-dark-32.png");
const ERROR_16: &[u8] = include_bytes!("../icons/tray/error-16.png");
const ERROR_24: &[u8] = include_bytes!("../icons/tray/error-24.png");
const ERROR_32: &[u8] = include_bytes!("../icons/tray/error-32.png");

/// 托盘图标按系统小图标的实际宽度选档：16/20（125% 先用 16）→16、24→24、32→32；
/// 其余就近档位，同距取小。
pub fn select_tray_bitmap(px: i32) -> u32 {
    [16, 24, 32]
        .into_iter()
        .min_by_key(|candidate| ((i64::from(px) - i64::from(*candidate)).abs(), *candidate))
        .unwrap()
}

/// 任务栏是浅色还是深色：读系统的 SystemUsesLightTheme；读不到按浅色处理（保守）。
const PERSONALIZE_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
const PERSONALIZE_KEY_WIDE: [u16; 64] = utf16(PERSONALIZE_KEY);

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

fn taskbar_theme_from_registry(value: Option<u32>) -> bool {
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

fn small_icon_width_from_dpi(dpi: Option<u32>, metric: impl FnOnce(u32) -> i32) -> i32 {
    const FALLBACK: i32 = 16;
    let Some(dpi) = dpi.filter(|dpi| *dpi > 0) else {
        return FALLBACK;
    };
    let width = metric(dpi);
    if width > 0 { width } else { FALLBACK }
}

fn small_icon_width_from_rect(
    rect: Option<Rect>,
    dpi_at: impl FnOnce((i32, i32)) -> Option<u32>,
    metric: impl FnOnce(u32) -> i32,
) -> i32 {
    let dpi = rect.and_then(physical_rect_center).and_then(dpi_at);
    small_icon_width_from_dpi(dpi, metric)
}

/// 每次刷新都通过 Tauri 实时调用 Shell_NotifyIconGetRect；失败回 16，绝不复用鼠标旧坐标。
fn current_small_icon_width(tray: &tauri::tray::TrayIcon<Wry>) -> i32 {
    small_icon_width_from_rect(tray.rect().ok().flatten(), monitor_dpi, |dpi| unsafe {
        GetSystemMetricsForDpi(SM_CXSMICON, dpi)
    })
}

fn tray_png(state: TrayState, light_taskbar: bool, size: u32) -> &'static [u8] {
    match (state, light_taskbar, size) {
        (TrayState::Recording, _, 16) => RECORDING_16,
        (TrayState::Recording, _, 24) => RECORDING_24,
        (TrayState::Recording, _, _) => RECORDING_32,
        (TrayState::Paused, true, 16) => PAUSED_LIGHT_16,
        (TrayState::Paused, true, 24) => PAUSED_LIGHT_24,
        (TrayState::Paused, true, _) => PAUSED_LIGHT_32,
        (TrayState::Paused, false, 16) => PAUSED_DARK_16,
        (TrayState::Paused, false, 24) => PAUSED_DARK_24,
        (TrayState::Paused, false, _) => PAUSED_DARK_32,
        (TrayState::Error, _, 16) => ERROR_16,
        (TrayState::Error, _, 24) => ERROR_24,
        (TrayState::Error, _, _) => ERROR_32,
    }
}

/// 托盘一帧的样子：图标档位、状态行、暂停菜单文字、tooltip。文案集中在测试里钉住。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayView {
    pub state: TrayState,
    pub status_line: String,
    pub pause_label: &'static str,
    pub tooltip: &'static str,
    pub ignore_next: bool,
}

pub fn tray_view(health: &HealthSnapshot, paused: bool, ignore_next: bool) -> TrayView {
    let decision = select_tray(health, paused);
    match decision.state {
        TrayState::Error => TrayView {
            state: TrayState::Error,
            status_line: "出了点问题 · 点托盘图标看看".into(),
            pause_label: if paused {
                "继续记录"
            } else {
                "暂停记录"
            },
            tooltip: "拈来 · 出了点问题，点开看看",
            ignore_next,
        },
        TrayState::Paused => TrayView {
            state: TrayState::Paused,
            status_line: "已暂停 · 现在复制的不会记下".into(),
            pause_label: "继续记录",
            tooltip: "拈来 · 已暂停，现在复制的不会记下",
            ignore_next,
        },
        TrayState::Recording => TrayView {
            state: TrayState::Recording,
            status_line: format!("正在记录 · 今天记下 {} 条", health.recorded_today),
            pause_label: "暂停记录",
            tooltip: "拈来 · 正在记录",
            ignore_next,
        },
    }
}

/// 消息线程到主线程的唯一回调；全局持有，RunEvent::Exit 必须调用 shutdown 明确释放。
static ON_SETTINGS_CHANGE: Mutex<Option<Arc<dyn Fn() + Send + Sync>>> = Mutex::new(None);

pub fn on_settings_change(callback: Arc<dyn Fn() + Send + Sync>) {
    *ON_SETTINGS_CHANGE.lock().unwrap() = Some(callback);
}

pub fn shutdown() {
    // 清掉捕获 AppHandle 的全局强引用；正在执行的局部 clone 完成后也会释放。
    ON_SETTINGS_CHANGE.lock().unwrap().take();
}

pub(crate) fn fire_settings_change() {
    // 不在全局锁内执行回调；回调只负责转主线程，不直接接触托盘原生对象。
    let callback = ON_SETTINGS_CHANGE.lock().unwrap().clone();
    if let Some(callback) = callback {
        callback();
    }
}

/// 设置保存的生产协调入口：只有 hotkey 变化才请求既有托盘 refresh。
pub(crate) fn notify_hotkey_settings_saved(update_hotkey: bool) {
    if update_hotkey {
        fire_settings_change();
    }
}

/// 从真实健康状态刷新图标、首行、暂停文字和 tooltip。
pub fn refresh(app: &AppHandle) {
    let Some(controller) = app.try_state::<Controller>() else {
        return;
    };
    let health = controller.history.health();
    let paused = gate().paused();
    let ignore_next = gate().ignoring_next();
    let view = tray_view(&health, paused, ignore_next);
    let light = taskbar_uses_light_theme();
    if let Some(tray) = app.tray_by_id("nianlai") {
        let size = select_tray_bitmap(current_small_icon_width(&tray));
        if let Ok(image) = tauri::image::Image::from_bytes(tray_png(view.state, light, size)) {
            let _ = tray.set_icon(Some(image));
        }
        let _ = tray.set_tooltip(Some(view.tooltip));
    }
    if let Some(items) = app.try_state::<TrayMenuItems>() {
        let hotkey = app
            .try_state::<Store>()
            .map(|settings| settings.get().hotkey)
            .unwrap_or_else(|| "Ctrl+`".into());
        refresh_open_panel_label(&hotkey, |label| {
            let _ = items.open.set_text(label);
        });
        let _ = items.status.set_text(view.status_line);
        let _ = items.pause.set_text(view.pause_label);
        let _ = items.ignore.set_checked(view.ignore_next);
    }
    let _ = controller;
}

/// 菜单项句柄，供 refresh 就地改字。
struct TrayMenuItems {
    open: MenuItem<Wry>,
    status: MenuItem<Wry>,
    pause: MenuItem<Wry>,
    ignore: CheckMenuItem<Wry>,
    autostart: CheckMenuItem<Wry>,
}

pub fn single_instance() -> TauriPlugin<Wry> {
    tauri_plugin_single_instance::init(|app, _args, _cwd| {
        // 次实例在插件初始化中以 0 退出，由主实例接收通知时留下固定日志。
        record(app, "单实例：已有实例，已转发并退出");
        open_panel(app);
    })
}

pub fn autostart() -> TauriPlugin<Wry> {
    // 初始化只建立管理器；仅用户点击菜单时才修改自启设置。
    tauri_plugin_autostart::Builder::new().build()
}

#[tauri::command]
pub fn get_autostart(app: AppHandle) -> Result<bool, String> {
    let query = app.autolaunch().is_enabled();
    // 设置页初次进入、重进和聚焦读取时，也把托盘菜单同步到同一真实状态。
    if let Some(items) = app.try_state::<TrayMenuItems>() {
        apply_autostart_state(&app, &items.autostart, &query);
    }
    query.map_err(|error| {
        record(&app, "开机自启：状态查询失败");
        format!("没读到开机启动状态，再试一次（{error}）")
    })
}

#[tauri::command]
pub fn set_autostart(app: AppHandle, enabled: bool) -> Result<bool, String> {
    let manager = app.autolaunch();
    let (write, query) = write_autostart(
        enabled,
        |enabled| {
            if enabled {
                manager.enable()
            } else {
                manager.disable()
            }
        },
        || manager.is_enabled(),
    );
    // 设置页和托盘始终展示同一次插件回读；失败也同步真实状态，不能按请求值打勾。
    if let Some(items) = app.try_state::<TrayMenuItems>() {
        apply_autostart_state(&app, &items.autostart, &query);
    }
    write.map_err(|error| {
        record(&app, "开机自启：设置未完成，已重新查询状态");
        format!("开机启动没改成，再试一次（{error}）")
    })?;
    query.map_err(|error| {
        record(&app, "开机自启：设置后回读失败");
        format!("没读到开机启动状态，再试一次（{error}）")
    })
}

pub fn setup(app: &AppHandle, logger: &Logger) -> Result<(), Box<dyn std::error::Error>> {
    logger.event("单实例：主实例", json!({}));
    // 第一行只说状态，不可点；暂停/故障时它和图标一起变。
    let status = MenuItem::with_id(app, STATUS, "正在打开 · 稍等", false, None::<&str>)?;
    let open = MenuItem::with_id(
        app,
        OPEN_PANEL,
        open_panel_label(&app.state::<Store>().get().hotkey),
        true,
        OPEN_PANEL_ACCELERATOR,
    )?;
    let settings = MenuItem::with_id(app, SETTINGS, "设置…", true, None::<&str>)?;
    let about = MenuItem::with_id(
        app,
        ABOUT,
        static_menu_label(ABOUT).expect("静态菜单文字与布局不一致"),
        true,
        None::<&str>,
    )?;
    let query = app.autolaunch().is_enabled();
    let state = autostart_menu_state(app, &query);
    let autostart = CheckMenuItem::with_id(
        app,
        AUTOSTART,
        static_menu_label(AUTOSTART).expect("静态菜单文字与布局不一致"),
        state.enabled,
        state.checked,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, QUIT, "退出", true, None::<&str>)?;
    // A09：暂停只在内存里，重启后自动恢复记录；菜单原位换字，不靠勾选。
    let pause = MenuItem::with_id(app, PAUSE, "暂停记录", true, None::<&str>)?;
    let ignore = CheckMenuItem::with_id(
        app,
        IGNORE_NEXT,
        static_menu_label(IGNORE_NEXT).expect("静态菜单文字与布局不一致"),
        true,
        gate().ignoring_next(),
        None::<&str>,
    )?;
    let separators = [
        PredefinedMenuItem::separator(app)?,
        PredefinedMenuItem::separator(app)?,
        PredefinedMenuItem::separator(app)?,
        PredefinedMenuItem::separator(app)?,
    ];
    let mut separator_index = 0;
    let mut menu_entries: Vec<&dyn IsMenuItem<Wry>> = Vec::with_capacity(TRAY_MENU_LAYOUT.len());
    for entry in &TRAY_MENU_LAYOUT {
        match entry {
            TrayMenuEntry::Separator => {
                menu_entries.push(&separators[separator_index]);
                separator_index += 1;
            }
            TrayMenuEntry::Action(id) => {
                let item: &dyn IsMenuItem<Wry> = match *id {
                    STATUS => &status,
                    OPEN_PANEL => &open,
                    PAUSE => &pause,
                    IGNORE_NEXT => &ignore,
                    SETTINGS => &settings,
                    AUTOSTART => &autostart,
                    ABOUT => &about,
                    QUIT => &quit,
                    _ => unreachable!("托盘菜单布局包含未知动作"),
                };
                menu_entries.push(item);
            }
        }
    }
    let menu = Menu::with_items(app, &menu_entries)?;
    {
        // 暂停或“下一次复制不记”变化（含采集线程消耗掉“忽略下一次”）时同步菜单、提示、面板横幅和托盘。
        let (ignore, app) = (ignore.clone(), app.clone());
        gate().subscribe(std::sync::Arc::new(move || {
            let paused = gate().paused();
            let _ = ignore.set_checked(gate().ignoring_next());
            refresh(&app);
            let _ = app.emit(
                "capture:changed",
                json!({"paused":paused,"ignore_next":gate().ignoring_next()}),
            );
            let _ = panel::dispatch(&app, Action::HistoryChanged);
        }));
    }
    // 设置与 DPI 变化转主线程刷新；回调只由全局槽持有，并在 RunEvent::Exit 清理。
    let settings_callback: Arc<dyn Fn() + Send + Sync> = {
        let app = app.clone();
        Arc::new(move || {
            let app = app.clone();
            let callback_app = app.clone();
            let _ = app.run_on_main_thread(move || refresh(&callback_app));
        })
    };
    on_settings_change(settings_callback);
    app.manage(TrayMenuItems {
        open: open.clone(),
        status: status.clone(),
        pause: pause.clone(),
        ignore: ignore.clone(),
        autostart: autostart.clone(),
    });
    let menu_autostart = autostart.clone();
    let initial = tauri::image::Image::from_bytes(RECORDING_16)?;

    TrayIconBuilder::with_id("nianlai")
        .icon(initial)
        .tooltip("拈来")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            OPEN_PANEL => open_panel(app),
            STATUS => {} // 状态行不可点。
            ABOUT => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    if settings_window::open_page(&app, Some("about")).is_err() {
                        record(&app, "设置：窗口打开失败");
                    }
                });
            }
            PAUSE => {
                let paused = !gate().paused();
                gate().set_paused(paused);
                record(
                    app,
                    if paused {
                        "采集：已暂停记录"
                    } else {
                        "采集：已恢复记录"
                    },
                );
            }
            IGNORE_NEXT => {
                let ignore = !gate().ignoring_next();
                gate().set_ignore_next(ignore);
                record(
                    app,
                    if ignore {
                        "采集：将忽略下一次复制"
                    } else {
                        "采集：取消忽略下一次复制"
                    },
                );
            }
            SETTINGS => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    if settings_window::open(&app).is_err() {
                        record(&app, "设置：窗口打开失败");
                    }
                });
            }
            AUTOSTART => {
                let manager = app.autolaunch();
                let (update, query) = update_autostart(
                    || manager.is_enabled(),
                    |enabled| {
                        if enabled {
                            manager.enable()
                        } else {
                            manager.disable()
                        }
                    },
                );
                if update.is_err() {
                    record(app, "开机自启：设置未完成，已重新查询状态");
                }
                apply_autostart_state(app, &menu_autostart, &query);
            }
            QUIT => {
                record(app, "托盘：请求退出");
                // 走 Panel 的退出分支和 RunEvent::Exit 清理。
                if panel::dispatch(app, Action::Quit).is_err() {
                    record(app, "托盘：退出请求未完成");
                }
            }
            _ => {}
        })
        .on_tray_icon_event(move |tray, event| {
            // 所有事件都以实时 rect 刷新；事件里的鼠标坐标不参与 DPI 判断。
            refresh(tray.app_handle());
            match event {
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } => handle_left_click(tray.app_handle()),
                TrayIconEvent::Enter { .. }
                | TrayIconEvent::Click {
                    button: MouseButton::Right,
                    button_state: MouseButtonState::Down,
                    ..
                } => {
                    let app = tray.app_handle();
                    apply_autostart_state(app, &autostart, &app.autolaunch().is_enabled());
                    refresh(app);
                }
                _ => {}
            }
        })
        .build(app)?;
    refresh(app);
    Ok(())
}

fn route_left_click(
    settings_page: Option<&'static str>,
    open_panel: impl FnOnce(),
    open_settings: impl FnOnce(&'static str),
) {
    match settings_page {
        Some(page) => open_settings(page),
        None => open_panel(),
    }
}

fn handle_left_click(app: &AppHandle) {
    // 出了问题：左键打开对应设置页（存储 → 数据与备份，记录 → 记录与隐私）。
    let error_page = app.try_state::<Controller>().and_then(|controller| {
        select_tray(&controller.history.health(), gate().paused()).settings_page
    });
    route_left_click(
        error_page,
        || open_panel(app),
        |page| {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                if settings_window::open_page(&app, Some(page)).is_err() {
                    record(&app, "设置：窗口打开失败");
                }
            });
        },
    );
}

fn open_panel(app: &AppHandle) {
    let handle = app.clone();
    if app
        .run_on_main_thread(move || {
            if handle.try_state::<Controller>().is_none() {
                record(&handle, "面板：尚未初始化，已跳过呼出");
                return;
            }
            let Some(window) = handle.get_webview_window("panel") else {
                record(&handle, "面板：未找到既有显示入口");
                return;
            };
            // 可见性检查和既有 Toggle 派发在同一主线程执行，重复打开不会反向隐藏。
            match window.is_visible() {
                Ok(true) => {}
                Ok(false) => {
                    let action = Action::Toggle {
                        started: Instant::now(),
                        target: platform::window::capture(),
                        alt: false,
                    };
                    if panel::dispatch(&handle, action).is_err() {
                        record(&handle, "面板：呼出请求未完成");
                    }
                }
                Err(_) => record(&handle, "面板：可见状态查询失败，已跳过呼出"),
            }
        })
        .is_err()
    {
        record(app, "面板：主线程呼出请求未完成");
    }
}

#[derive(Debug, PartialEq, Eq)]
struct AutostartMenuState {
    checked: bool,
    enabled: bool,
}

impl AutostartMenuState {
    fn from_query<E>(query: &Result<bool, E>) -> Self {
        Self {
            checked: query.as_ref().copied().unwrap_or(false),
            enabled: query.is_ok(),
        }
    }
}

fn write_autostart<E>(
    enabled: bool,
    mut set_enabled: impl FnMut(bool) -> Result<(), E>,
    mut query: impl FnMut() -> Result<bool, E>,
) -> (Result<(), E>, Result<bool, E>) {
    // 无论写入成败都回读插件状态；请求值只用于写入，绝不当成实际结果返回。
    let update = set_enabled(enabled);
    (update, query())
}

fn update_autostart<E>(
    mut query: impl FnMut() -> Result<bool, E>,
    mut set_enabled: impl FnMut(bool) -> Result<(), E>,
) -> (Result<(), E>, Result<bool, E>) {
    // 原生菜单可能已经自行改变勾选，切换目标始终从插件查询，写入后也必须回读。
    let update = query().and_then(|enabled| set_enabled(!enabled));
    (update, query())
}

fn autostart_menu_state(
    app: &AppHandle,
    query: &Result<bool, tauri_plugin_autostart::Error>,
) -> AutostartMenuState {
    if query.is_err() {
        record(app, "开机自启：状态查询失败，已禁用菜单项");
    }
    AutostartMenuState::from_query(query)
}

fn apply_autostart_state(
    app: &AppHandle,
    item: &CheckMenuItem<Wry>,
    query: &Result<bool, tauri_plugin_autostart::Error>,
) {
    let state = autostart_menu_state(app, query);
    let checked = item.set_checked(state.checked);
    let enabled = item.set_enabled(state.enabled);
    if checked.is_err() || enabled.is_err() {
        record(app, "开机自启：菜单状态同步失败");
    }
}

fn record(app: &AppHandle, message: &'static str) {
    if let Some(controller) = app.try_state::<Controller>() {
        controller.logger.event(message, json!({}));
        controller.logger.flush();
    } else if let Ok(logger) = crate::settings::open_logger() {
        // 单实例通知可能早于 Controller 初始化，不读取启动参数或工作目录。
        logger.event(message, json!({}));
        logger.flush();
    } else {
        eprintln!("{message}");
    }
}

/// 设置与 DPI 消息只请求轻量刷新；从不解引用 LPARAM，也从不消费原消息。
pub(crate) fn window_message(message: u32, _lp: LPARAM) -> bool {
    if message == WM_SETTINGCHANGE || message == WM_DPICHANGED {
        fire_settings_change();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::{CaptureHealth, StoreHealth};
    use tauri::{Position, Rect, Size};
    use windows::Win32::UI::WindowsAndMessaging::WM_CLIPBOARDUPDATE;

    static MESSAGE_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn health(store: StoreHealth, capture: CaptureHealth, today: usize) -> HealthSnapshot {
        HealthSnapshot {
            store,
            capture,
            recorded_today: today,
        }
    }

    #[test]
    fn bitmap_selection_covers_100_125_150_200_percent() {
        assert_eq!(select_tray_bitmap(16), 16);
        assert_eq!(select_tray_bitmap(20), 16, "125% 先用 16");
        assert_eq!(select_tray_bitmap(24), 24);
        assert_eq!(select_tray_bitmap(32), 32);
        assert_eq!(select_tray_bitmap(i32::MIN), 16);
        assert_eq!(select_tray_bitmap(0), 16);
        assert_eq!(select_tray_bitmap(12), 16);
        assert_eq!(select_tray_bitmap(19), 16);
        assert_eq!(select_tray_bitmap(21), 24);
        assert_eq!(select_tray_bitmap(23), 24);
        assert_eq!(select_tray_bitmap(27), 24);
        assert_eq!(select_tray_bitmap(28), 24, "28 与 24/32 同距取小");
        assert_eq!(select_tray_bitmap(29), 32);
        assert_eq!(select_tray_bitmap(33), 32);
        assert_eq!(select_tray_bitmap(40), 32);
        assert_eq!(select_tray_bitmap(i32::MAX), 32);
    }

    #[test]
    fn monitor_dpi_drives_metrics_and_failures_fall_back_to_16() {
        for (dpi, px, expected) in [(96, 16, 16), (120, 20, 16), (144, 24, 24), (192, 32, 32)] {
            let mut queried = None;
            let actual = small_icon_width_from_dpi(Some(dpi), |requested| {
                queried = Some(requested);
                px
            });
            assert_eq!(queried, Some(dpi));
            assert_eq!(actual, px);
            assert_eq!(select_tray_bitmap(actual), expected);
        }
        assert_eq!(
            small_icon_width_from_dpi(None, |_| panic!("没有托盘坐标不能读主屏")),
            16
        );
        assert_eq!(
            small_icon_width_from_dpi(Some(0), |_| panic!("无效 DPI 不能读指标")),
            16
        );
        assert_eq!(small_icon_width_from_dpi(Some(192), |_| 0), 16);
    }

    fn physical_rect(x: i32, y: i32) -> Rect {
        Rect {
            position: Position::Physical((x, y).into()),
            size: Size::Physical((32, 32).into()),
        }
    }

    #[test]
    fn every_metric_read_uses_the_new_live_rect_without_mouse_position_cache() {
        let probes = std::cell::RefCell::new(Vec::new());
        for (rect, dpi, px, expected) in [
            (physical_rect(0, 0), 120, 20, 16),
            (physical_rect(3000, 0), 192, 32, 32),
        ] {
            let actual = small_icon_width_from_rect(
                Some(rect),
                |point| {
                    probes.borrow_mut().push(point);
                    Some(dpi)
                },
                |requested| {
                    assert_eq!(requested, dpi);
                    px
                },
            );
            assert_eq!(select_tray_bitmap(actual), expected);
        }
        assert_eq!(*probes.borrow(), [(16, 16), (3016, 16)]);
        assert_eq!(
            small_icon_width_from_rect(
                None,
                |_| panic!("无实时 rect 不得复用旧点"),
                |_| { panic!("无实时 DPI 不得读指标") }
            ),
            16,
            "旧 rect 消失后必须回 16，不能沿用上一次跨屏位置"
        );
    }

    #[test]
    fn registry_path_constant_is_the_exact_personalize_key() {
        assert_eq!(
            PERSONALIZE_KEY,
            r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"
        );
        let expected: Vec<u16> = PERSONALIZE_KEY.encode_utf16().collect();
        assert_eq!(&PERSONALIZE_KEY_WIDE[..expected.len()], expected);
        assert_eq!(PERSONALIZE_KEY_WIDE[expected.len()], 0);
    }

    #[test]
    fn registry_theme_only_explicit_zero_is_dark() {
        assert!(taskbar_theme_from_registry(None), "读取失败回浅色版");
        assert!(taskbar_theme_from_registry(Some(1)));
        assert!(taskbar_theme_from_registry(Some(2)), "异常值也保守回浅色版");
        assert!(!taskbar_theme_from_registry(Some(0)));
    }

    #[test]
    fn setting_and_dpi_messages_refresh_without_consuming_or_reading_lparam() {
        let _guard = MESSAGE_TEST_LOCK.lock().unwrap();
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let callback = {
            let calls = calls.clone();
            Arc::new(move || {
                calls.fetch_add(1, Ordering::Relaxed);
            })
        };
        on_settings_change(callback);
        for lp in [LPARAM(0), LPARAM(1)] {
            assert!(!window_message(WM_SETTINGCHANGE, lp));
            assert!(!window_message(WM_DPICHANGED, lp));
        }
        assert!(!window_message(WM_CLIPBOARDUPDATE, LPARAM(1)));
        assert_eq!(calls.load(Ordering::Relaxed), 4);
        shutdown();
    }

    #[test]
    fn hotkey_save_notification_calls_the_registered_refresh_only_for_hotkey_updates() {
        let _guard = MESSAGE_TEST_LOCK.lock().unwrap();
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let callback = {
            let calls = calls.clone();
            Arc::new(move || {
                calls.fetch_add(1, Ordering::Relaxed);
            })
        };
        on_settings_change(callback);
        notify_hotkey_settings_saved(false);
        assert_eq!(calls.load(Ordering::Relaxed), 0, "非呼出键保存不刷新标签");
        notify_hotkey_settings_saved(true);
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "呼出键保存必须进入生产 refresh 回调"
        );
        shutdown();
    }

    #[test]
    fn shutdown_releases_the_registered_callback_and_clears_its_slot() {
        let _guard = MESSAGE_TEST_LOCK.lock().unwrap();
        let callback: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        let callback_weak = Arc::downgrade(&callback);
        on_settings_change(callback);
        assert!(callback_weak.upgrade().is_some());
        assert!(ON_SETTINGS_CHANGE.lock().unwrap().is_some());
        shutdown();
        assert!(ON_SETTINGS_CHANGE.lock().unwrap().is_none());
        assert!(
            callback_weak.upgrade().is_none(),
            "明确 teardown 必须释放生产全局槽持有的回调"
        );
    }

    #[test]
    fn production_menu_layout_contains_every_action_in_v11_order() {
        let actions: Vec<_> = TRAY_MENU_LAYOUT
            .iter()
            .filter_map(|entry| match entry {
                TrayMenuEntry::Action(id) => Some(*id),
                TrayMenuEntry::Separator => None,
            })
            .collect();
        assert_eq!(
            actions,
            [
                STATUS,
                OPEN_PANEL,
                PAUSE,
                IGNORE_NEXT,
                SETTINGS,
                AUTOSTART,
                ABOUT,
                QUIT
            ]
        );
        assert_eq!(
            TRAY_MENU_LAYOUT
                .iter()
                .filter(|entry| matches!(entry, TrayMenuEntry::Separator))
                .count(),
            4
        );
    }

    #[test]
    fn production_static_menu_labels_match_v11_copy() {
        let labels: Vec<_> = TRAY_MENU_LAYOUT
            .iter()
            .filter_map(|entry| match entry {
                TrayMenuEntry::Action(id) => static_menu_label(id).map(|label| (*id, label)),
                TrayMenuEntry::Separator => None,
            })
            .collect();
        assert_eq!(
            labels,
            [
                (IGNORE_NEXT, "下一次复制不记"),
                (AUTOSTART, "开机时启动"),
                (ABOUT, "关于拈来"),
            ]
        );
    }

    #[test]
    fn left_click_up_handler_routes_open_panel_and_both_error_pages() {
        fn route(page: Option<&'static str>) -> Vec<String> {
            let calls = std::cell::RefCell::new(Vec::new());
            route_left_click(
                page,
                || calls.borrow_mut().push("OpenPanel".to_owned()),
                |page| calls.borrow_mut().push(format!("OpenSettings:{page}")),
            );
            calls.into_inner()
        }
        assert_eq!(route(None), ["OpenPanel"]);
        assert_eq!(route(Some("data")), ["OpenSettings:data"]);
        assert_eq!(route(Some("privacy")), ["OpenSettings:privacy"]);
    }

    #[test]
    fn tray_view_lines_match_the_v11_copy() {
        let view = tray_view(
            &health(StoreHealth::Ready, CaptureHealth::Recording, 42),
            false,
            false,
        );
        assert_eq!(view.status_line, "正在记录 · 今天记下 42 条");
        assert_eq!(view.pause_label, "暂停记录");
        assert_eq!(view.tooltip, "拈来 · 正在记录");
        assert!(!view.ignore_next);

        let view = tray_view(
            &health(StoreHealth::Ready, CaptureHealth::Recording, 0),
            true,
            true,
        );
        assert_eq!(view.status_line, "已暂停 · 现在复制的不会记下");
        assert_eq!(view.pause_label, "继续记录");
        assert_eq!(view.tooltip, "拈来 · 已暂停，现在复制的不会记下");
        assert!(view.ignore_next, "暂停不能把忽略下一次勾选丢掉");

        let view = tray_view(
            &health(StoreHealth::Failed, CaptureHealth::Recording, 3),
            false,
            true,
        );
        assert_eq!(view.status_line, "出了点问题 · 点托盘图标看看");
        assert_eq!(view.pause_label, "暂停记录");
        assert_eq!(view.tooltip, "拈来 · 出了点问题，点开看看");
        assert!(view.ignore_next, "故障刷新也不能把忽略下一次勾选丢掉");
        // 故障 + 暂停并存：故障优先，暂停菜单显示“继续记录”。
        let view = tray_view(
            &health(StoreHealth::Failed, CaptureHealth::Recording, 3),
            true,
            false,
        );
        assert_eq!(view.status_line, "出了点问题 · 点托盘图标看看");
        assert_eq!(view.pause_label, "继续记录");
    }

    #[test]
    fn tray_png_picks_state_size_and_taskbar_theme() {
        assert_eq!(tray_png(TrayState::Recording, false, 16), RECORDING_16);
        assert_eq!(tray_png(TrayState::Recording, true, 24), RECORDING_24);
        assert_eq!(tray_png(TrayState::Recording, false, 32), RECORDING_32);
        assert_eq!(tray_png(TrayState::Paused, true, 16), PAUSED_LIGHT_16);
        assert_eq!(tray_png(TrayState::Paused, false, 16), PAUSED_DARK_16);
        assert_eq!(tray_png(TrayState::Paused, false, 24), PAUSED_DARK_24);
        assert_eq!(tray_png(TrayState::Paused, true, 32), PAUSED_LIGHT_32);
        // 出问题徽标不分深浅。
        assert_eq!(tray_png(TrayState::Error, true, 16), ERROR_16);
        assert_eq!(tray_png(TrayState::Error, false, 32), ERROR_32);
        for state in [TrayState::Recording, TrayState::Paused, TrayState::Error] {
            for light in [false, true] {
                for size in [16, 24, 32] {
                    let bytes = tray_png(state, light, size);
                    assert_eq!(
                        crate::health::png_dimensions(bytes),
                        (size, size),
                        "{state:?} / light={light} / {size}px 必须使用真实同尺寸资源"
                    );
                }
            }
        }
        assert_ne!(PAUSED_LIGHT_16, PAUSED_DARK_16);
        assert_ne!(PAUSED_LIGHT_24, PAUSED_DARK_24);
        assert_ne!(PAUSED_LIGHT_32, PAUSED_DARK_32);
        assert_ne!(RECORDING_16, RECORDING_24);
        assert_ne!(RECORDING_24, RECORDING_32);
    }

    #[test]
    fn taskbar_theme_registry_read_defaults_to_light() {
        // 真机 / CI 上注册表多半存在；无论哪种，都不应 panic。
        let _ = taskbar_uses_light_theme();
    }

    #[test]
    fn autostart_menu_reflects_enabled_disabled_and_unknown_states() {
        for (query, checked, enabled) in [
            (Ok(true), true, true),
            (Ok(false), false, true),
            (Err("查询失败"), false, false),
        ] {
            assert_eq!(
                AutostartMenuState::from_query(&query),
                AutostartMenuState { checked, enabled }
            );
        }
    }

    #[test]
    fn autostart_toggle_uses_plugin_state_and_reads_back_after_writing() {
        for initial in [false, true] {
            let calls = std::cell::RefCell::new(Vec::new());
            let mut reads = [Ok::<_, &str>(initial), Ok(!initial)].into_iter();
            let (update, query) = update_autostart(
                || {
                    calls.borrow_mut().push("查询");
                    reads.next().unwrap()
                },
                |enabled| {
                    assert_eq!(enabled, !initial);
                    calls.borrow_mut().push("设置");
                    Ok(())
                },
            );
            assert_eq!(update, Ok(()));
            assert_eq!(query, Ok(!initial));
            assert_eq!(*calls.borrow(), ["查询", "设置", "查询"]);
        }
    }

    #[test]
    fn failed_autostart_change_restores_the_queried_state() {
        let mut reads = [Ok(true), Ok(true)].into_iter();
        let (update, query) = update_autostart(
            || reads.next().unwrap(),
            |enabled| {
                assert!(!enabled);
                Err("设置失败")
            },
        );
        assert_eq!(update, Err("设置失败"));
        assert_eq!(
            AutostartMenuState::from_query(&query),
            AutostartMenuState {
                checked: true,
                enabled: true,
            }
        );
    }

    #[test]
    fn initial_query_failure_never_changes_autostart() {
        let mut reads = [Err("查询失败"), Ok(false)].into_iter();
        let (update, query) = update_autostart(
            || reads.next().unwrap(),
            |_| panic!("查询失败时不应写入自启设置"),
        );
        assert_eq!(update, Err("查询失败"));
        assert_eq!(query, Ok(false));
    }

    #[test]
    fn external_autostart_change_wins_over_the_requested_state() {
        let mut reads = [Ok::<_, &str>(false), Ok(false)].into_iter();
        let (update, query) = update_autostart(
            || reads.next().unwrap(),
            |enabled| {
                assert!(enabled);
                Ok(())
            },
        );
        assert_eq!(update, Ok(()));
        assert_eq!(
            AutostartMenuState::from_query(&query),
            AutostartMenuState {
                checked: false,
                enabled: true,
            }
        );
    }

    #[test]
    fn failed_readback_disables_the_autostart_menu() {
        let mut reads = [Ok(false), Err("回读失败")].into_iter();
        let (update, query) = update_autostart(|| reads.next().unwrap(), |_| Ok(()));
        assert_eq!(update, Ok(()));
        assert_eq!(
            AutostartMenuState::from_query(&query),
            AutostartMenuState {
                checked: false,
                enabled: false,
            }
        );
    }

    fn refreshed_open_panel_labels(hotkey: &str) -> Vec<String> {
        let mut labels = Vec::new();
        refresh_open_panel_label(hotkey, |label| labels.push(label));
        labels
    }

    #[test]
    fn open_panel_label_shows_current_hotkey_with_safe_fallbacks() {
        assert_eq!(refreshed_open_panel_labels("Ctrl+`"), ["打开拈来\tCtrl+`"]);
        assert_eq!(
            refreshed_open_panel_labels("Ctrl+Shift+F11"),
            ["打开拈来\tCtrl+Shift+F11"]
        );
        assert_eq!(
            refreshed_open_panel_labels("shift+control+KeyK"),
            ["打开拈来\tCtrl+Shift+K"]
        );
        for invalid in ["", "   ", "not-a-key", "Ctrl+\tF11", "Ctrl+\nF11"] {
            assert_eq!(
                refreshed_open_panel_labels(invalid),
                ["打开拈来\tCtrl+`"],
                "{invalid:?}"
            );
        }
        assert_eq!(
            refreshed_open_panel_labels("Ctrl+`").len(),
            1,
            "刷新必须调用一次真实文字 sink"
        );
        assert_eq!(open_panel_label("Ctrl+`").matches('\t').count(), 1);
        assert!(!open_panel_label("Ctrl+`").contains("undefined"));
        assert!(
            OPEN_PANEL_ACCELERATOR.is_none(),
            "标签文字不得变成第二个快捷键"
        );
        assert!(matches!(
            TRAY_MENU_LAYOUT[2],
            TrayMenuEntry::Action(OPEN_PANEL)
        ));
    }

    #[test]
    fn saved_and_reloaded_hotkey_drive_the_same_open_panel_label() {
        let directory = crate::settings::tests::Directory::new();
        let path = directory.0.join("settings.json");
        let (store, _) = crate::settings::Store::load(path.clone());
        assert_eq!(
            refreshed_open_panel_labels(&store.get().hotkey),
            ["打开拈来\tCtrl+`"]
        );
        let (saved, _) =
            crate::settings::save(&store, json!({"hotkey":"Ctrl+Shift+F11"}), |_| {}, |_| {})
                .unwrap();
        assert_eq!(
            refreshed_open_panel_labels(&saved.hotkey),
            ["打开拈来\tCtrl+Shift+F11"]
        );
        let (reloaded, issues) = crate::settings::Store::load(path);
        assert!(issues.is_empty());
        assert_eq!(
            refreshed_open_panel_labels(&reloaded.get().hotkey),
            ["打开拈来\tCtrl+Shift+F11"]
        );
    }

    #[test]
    fn direct_autostart_write_always_reads_back_and_external_state_wins() {
        let calls = std::cell::RefCell::new(Vec::new());
        let (write, query) = write_autostart(
            true,
            |enabled| {
                calls.borrow_mut().push(format!("设置:{enabled}"));
                Ok::<_, &str>(())
            },
            || {
                calls.borrow_mut().push("回读".into());
                Ok(false)
            },
        );
        assert_eq!(write, Ok(()));
        assert_eq!(query, Ok(false), "外部 false 优先于请求 true");
        assert_eq!(*calls.borrow(), ["设置:true", "回读"]);
    }

    #[test]
    fn direct_autostart_write_failure_still_queries_actual_state() {
        let mut queried = false;
        let (write, query) = write_autostart(
            true,
            |_| Err("写失败"),
            || {
                queried = true;
                Ok(false)
            },
        );
        assert_eq!(write, Err("写失败"));
        assert_eq!(query, Ok(false));
        assert!(queried);
    }

    #[test]
    fn direct_autostart_readback_failure_is_not_a_success() {
        let (write, query) = write_autostart(true, |_| Ok::<_, &str>(()), || Err("回读失败"));
        assert_eq!(write, Ok(()));
        assert_eq!(query, Err("回读失败"));
    }
}
