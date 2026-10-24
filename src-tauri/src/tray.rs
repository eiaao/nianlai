use crate::{
    log::Logger,
    panel::{self, Action, Controller},
    platform,
};
use serde_json::json;
use std::time::Instant;
use tauri::{
    AppHandle, Manager, Wry,
    menu::{CheckMenuItem, Menu, MenuItem},
    plugin::TauriPlugin,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_autostart::ManagerExt;

const OPEN_PANEL: &str = "tray_open_panel";
const AUTOSTART: &str = "tray_autostart";
const QUIT: &str = "tray_quit";

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

pub fn setup(app: &AppHandle, logger: &Logger) -> Result<(), Box<dyn std::error::Error>> {
    logger.event("单实例：主实例", json!({}));
    let open = MenuItem::with_id(app, OPEN_PANEL, "打开面板", true, None::<&str>)?;
    let query = app.autolaunch().is_enabled();
    let state = autostart_menu_state(app, &query);
    let autostart = CheckMenuItem::with_id(
        app,
        AUTOSTART,
        "开机自启",
        state.enabled,
        state.checked,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, QUIT, "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &autostart, &quit])?;
    let menu_autostart = autostart.clone();
    let icon = app.default_window_icon().cloned().ok_or("托盘图标不存在")?;

    TrayIconBuilder::with_id("nianlai")
        .icon(icon)
        .tooltip("拈来")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            OPEN_PANEL => open_panel(app),
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
                apply_autostart_state(app, &menu_autostart, query);
            }
            QUIT => {
                record(app, "托盘：请求退出");
                // 与 quit_prototype 共用 Panel 的退出分支和 RunEvent::Exit 清理。
                if panel::dispatch(app, Action::Quit).is_err() {
                    record(app, "托盘：退出请求未完成");
                }
            }
            _ => {}
        })
        .on_tray_icon_event(move |tray, event| match event {
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } => open_panel(tray.app_handle()),
            TrayIconEvent::Enter { .. }
            | TrayIconEvent::Click {
                button: MouseButton::Right,
                button_state: MouseButtonState::Down,
                ..
            } => apply_autostart_state(
                tray.app_handle(),
                &autostart,
                tray.app_handle().autolaunch().is_enabled(),
            ),
            _ => {}
        })
        .build(app)?;
    Ok(())
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
    query: Result<bool, tauri_plugin_autostart::Error>,
) {
    let state = autostart_menu_state(app, &query);
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
    } else if let Ok(logger) = Logger::new() {
        // 单实例通知可能早于 Controller 初始化，不读取启动参数或工作目录。
        logger.event(message, json!({}));
        logger.flush();
    } else {
        eprintln!("{message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

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
            let calls = RefCell::new(Vec::new());
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
}
