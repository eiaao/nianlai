mod autocycle;
mod capture;
mod history;
mod log;
mod panel;
mod paste;
mod platform;
mod resuspend;
mod samples;
mod settings_window;
mod tray;

use panel::{Action, Controller};
use serde_json::json;
use std::time::Instant;
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

#[tauri::command]
fn get_history(app: AppHandle) -> history::Snapshot {
    app.state::<Controller>().history_snapshot()
}
#[tauri::command]
async fn search_history(
    app: AppHandle,
    query: String,
    request_id: u64,
) -> Result<history::SearchReply, String> {
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<Controller>().search(&query, request_id)
    })
    .await
    .map_err(|_| "search_worker_failed".to_owned())?
}
#[tauri::command]
fn frontend_ready(app: AppHandle) -> Result<(), String> {
    panel::dispatch(&app, Action::FrontendReady)
}
#[tauri::command]
fn start_search(app: AppHandle, session: u64) -> Result<(), String> {
    panel::dispatch(
        &app,
        Action::Search {
            session,
            started: None,
        },
    )
}
#[tauri::command]
fn search_ready(app: AppHandle, session: u64) -> Result<(), String> {
    panel::dispatch(&app, Action::SearchReady { session })
}
#[tauri::command]
fn hide_panel(app: AppHandle, session: u64) -> Result<(), String> {
    app.state::<Controller>().logger.event(
        "hide_source",
        json!({"session":session,"source":"frontend"}),
    );
    panel::dispatch(&app, Action::Hide { session })
}
#[tauri::command]
fn paste_item(
    app: AppHandle,
    session: u64,
    id: usize,
    mode: paste::request::Mode,
) -> Result<(), String> {
    panel::dispatch(
        &app,
        Action::Paste {
            session,
            id,
            mode,
            started: Instant::now(),
        },
    )
}
#[tauri::command]
fn report_painted(app: AppHandle, session: u64) -> Result<(), String> {
    panel::dispatch(
        &app,
        Action::Painted {
            session,
            received: Instant::now(),
        },
    )
}
// 只记输入类型：insertText 表示首字直接上屏，insertCompositionText 表示进了输入法。不记内容。
#[tauri::command]
fn report_search_input(
    app: AppHandle,
    session: u64,
    input_type: String,
    composing: bool,
    after_ready_ms: f64,
) {
    let input_type: String = input_type
        .chars()
        .filter(char::is_ascii_alphabetic)
        .take(40)
        .collect();
    app.state::<Controller>().logger.event(
        "search_first_input",
        json!({"session":session,"input_type":input_type,"composing":composing,"after_ready_ms":after_ready_ms}),
    );
}
#[tauri::command]
fn quit_prototype(app: AppHandle) -> Result<(), String> {
    app.state::<Controller>()
        .logger
        .event("quit_source", json!({"source":"frontend"}));
    panel::dispatch(&app, Action::QuitFrontend)
}

fn register_hotkeys(app: &AppHandle, logger: &log::Logger, automatic: bool) -> Result<(), String> {
    let exit: Shortcut = "Ctrl+Alt+Shift+Q".parse().map_err(|e| format!("{e}"))?;
    if let Err(error) = app.global_shortcut().on_shortcut(exit, |app, _, event| {
        if event.state == ShortcutState::Pressed {
            app.state::<Controller>()
                .logger
                .event("quit_source", json!({"source":"hotkey"}));
            let _ = panel::dispatch(app, Action::Quit);
        }
    }) {
        logger.event("exit_hotkey_failed", json!({"error":error.to_string()}));
        return Err(error.to_string());
    }
    logger.event(
        "hotkey_registered",
        json!({"purpose":"quit","shortcut":exit.to_string()}),
    );
    if automatic {
        return Ok(());
    }
    let requested = std::env::var("NIANLAI_HOTKEY").unwrap_or_else(|_| "Alt+V".into());
    for (index, accelerator) in [requested.as_str(), "Ctrl+Shift+F12"]
        .into_iter()
        .enumerate()
    {
        let attempt = (|| -> Result<Shortcut, String> {
            let shortcut: Shortcut = accelerator.parse().map_err(|e| format!("{e}"))?;
            if shortcut == exit
                || shortcut
                    == "Ctrl+Alt+Shift+L"
                        .parse::<Shortcut>()
                        .map_err(|e| e.to_string())?
            {
                return Err("呼出键不能占用退出键或采集调试键".into());
            }
            if !platform::hook::supported(&shortcut) {
                return Err("此呼出键不受钩子支持".into());
            }
            app.global_shortcut()
                .on_shortcut(shortcut, |app, shortcut, event| {
                    let started = Instant::now();
                    if event.state == ShortcutState::Pressed {
                        let target = platform::window::capture();
                        let _ = panel::dispatch(
                            app,
                            Action::Toggle {
                                started,
                                target,
                                alt: shortcut.mods.contains(Modifiers::ALT),
                            },
                        );
                    }
                })
                .map_err(|e| e.to_string())?;
            Ok(shortcut)
        })();
        match attempt {
            Ok(shortcut) => {
                app.state::<Controller>().set_hotkey(shortcut);
                logger.event(
                    "hotkey_registered",
                    json!({"purpose":"panel","shortcut":shortcut.to_string(),"fallback":index==1}),
                );
                return Ok(());
            }
            Err(error) => logger.event(
                "hotkey_registration_failed",
                json!({"fallback":index==1,"error":error}),
            ),
        }
    }
    Err("呼出键和备用键均注册失败，请查看 panel-log.jsonl".into())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tray::single_instance())
        .plugin(tray::autostart())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            get_history,
            search_history,
            frontend_ready,
            start_search,
            search_ready,
            hide_panel,
            paste_item,
            report_painted,
            report_search_input,
            quit_prototype,
            settings_window::open_settings
        ])
        .setup(|app| {
            let logger = log::Logger::new()?;
            logger.event("startup", json!({"platform":std::env::consts::OS}));
            let window = app.get_webview_window("panel").ok_or("panel 窗口不存在")?;
            platform::window::no_activate(&window, true)?;
            platform::webview::initialize(&window)?;
            let plan = autocycle::Plan::from_env(&logger);
            let resuspend = resuspend::from_env(&logger);
            let experiments = panel::Experiments::from_env(&logger);
            let automatic = plan.is_some();
            app.manage(Controller::new(
                window,
                logger.clone(),
                plan,
                resuspend,
                experiments,
            )?);
            tray::setup(app.handle(), &logger)?;
            let handle = app.handle().clone();
            let capture_logger = logger.clone();
            std::thread::Builder::new()
                .name("history-startup".into())
                .spawn(move || {
                    if let Err(error) = handle
                        .state::<Controller>()
                        .start_capture(&handle, automatic)
                    {
                        capture_logger.event("capture_start_failed", json!({"error":error}));
                    }
                })?;
            if let Err(error) = register_hotkeys(app.handle(), &logger, automatic) {
                logger.flush();
                return Err(error.into());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // 设置窗口关掉就销毁，下次再按需创建；只有面板窗口的关闭请求才走退出。
                if window.label() == settings_window::LABEL {
                    return;
                }
                api.prevent_close();
                if let Some(controller) = window.app_handle().try_state::<Controller>() {
                    controller
                        .logger
                        .event("quit_source", json!({"source":"window_close"}));
                }
                let _ = panel::dispatch(window.app_handle(), Action::Quit);
            }
        })
        .build(tauri::generate_context!())
        .expect("G0 样机初始化失败");
    app.run(|app, event| {
        if matches!(event, tauri::RunEvent::Exit)
            && let Some(controller) = app.try_state::<Controller>()
        {
            controller.shutdown();
        }
    });
}
