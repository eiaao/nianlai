mod autocycle;
mod capture;
mod history;
mod hotkey;
mod log;
mod panel;
mod paste;
mod platform;
mod recovery;
mod resuspend;
mod samples;
mod settings;
mod settings_window;
mod tray;

use panel::{Action, Controller};
use serde_json::json;
use std::time::Instant;
use tauri::{AppHandle, Manager};

#[tauri::command]
fn get_settings(app: AppHandle) -> settings::Settings {
    app.state::<settings::Store>().get()
}

#[tauri::command]
async fn set_settings(
    app: AppHandle,
    settings: serde_json::Value,
) -> Result<settings::Settings, String> {
    // 注册/注销由插件派回主线程；不能在主线程持锁等待它们完成。
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<hotkey::Runtime>().save(&app, settings)
    })
    .await
    .map_err(|_| "设置保存任务未完成，请重试".to_owned())?
}

#[derive(serde::Serialize)]
struct CaptureState {
    paused: bool,
    ignore_next: bool,
    rules: &'static [capture::sensitive::Rule],
    default_excluded_apps: [&'static str; 6],
}

#[tauri::command]
fn get_capture_state() -> CaptureState {
    let gate = capture::rules::gate();
    CaptureState {
        paused: gate.paused(),
        ignore_next: gate.ignoring_next(),
        rules: capture::sensitive::RULES,
        default_excluded_apps: settings::DEFAULT_EXCLUDED_APPS,
    }
}

#[tauri::command]
fn set_capture_paused(paused: bool) {
    capture::rules::gate().set_paused(paused);
}

#[tauri::command]
fn set_ignore_next(ignore: bool) {
    capture::rules::gate().set_ignore_next(ignore);
}

#[derive(serde::Serialize)]
struct SnapshotState {
    items: Vec<nianlai_store::SnapshotInfo>,
    keep: usize,
    // 本次启动时执行的恢复；设置页据此提示成功或失败。
    restored: Option<history::snapshots::Restored>,
    // 是否由守护者拉起：不是的话恢复后不会自动重启，需要手动打开。
    relaunch: bool,
}

fn store_directory() -> Result<std::path::PathBuf, String> {
    history::active()
        .and_then(|history| history.store_directory())
        .ok_or_else(|| "历史库未打开，暂时不能使用快照".to_owned())
}

#[tauri::command]
fn get_snapshots() -> Result<SnapshotState, String> {
    Ok(SnapshotState {
        items: history::snapshots::list(&store_directory()?)?,
        keep: nianlai_store::SNAPSHOT_KEEP,
        restored: history::snapshots::last_restore(),
        relaunch: recovery::is_child(),
    })
}

#[tauri::command]
async fn create_snapshot(app: AppHandle) -> Result<(), String> {
    let directory = store_directory()?;
    tauri::async_runtime::spawn_blocking(move || {
        history::snapshots::manual(&directory, &app.state::<Controller>().logger)
    })
    .await
    .map_err(|_| "快照任务未完成，请重试".to_owned())?
}

#[tauri::command]
async fn restore_snapshot(app: AppHandle, name: String) -> Result<(), String> {
    let directory = store_directory()?;
    let handle = app.clone();
    // 完整校验可能要几秒，不能占着主线程。
    tauri::async_runtime::spawn_blocking(move || {
        history::snapshots::schedule_restore(
            &directory,
            &name,
            &handle.state::<Controller>().logger,
        )
    })
    .await
    .map_err(|_| "恢复任务未完成，请重试".to_owned())??;
    app.state::<Controller>()
        .logger
        .event("quit_source", json!({"source":"snapshot_restore"}));
    panel::dispatch(&app, Action::Relaunch)
}

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
// E01 收藏：面板控制器不暴露历史服务字段，命令层经 history::active() 取当前实例。
#[tauri::command]
fn get_favorites() -> Result<Vec<history::Preview>, String> {
    history::active()
        .ok_or_else(|| "历史服务未就绪".to_owned())?
        .favorites()
}
#[tauri::command]
fn set_favorite(id: usize, favorite: bool) -> Result<Option<history::Preview>, String> {
    history::active()
        .ok_or_else(|| "历史服务未就绪".to_owned())?
        .set_favorite(id, favorite)
}
#[tauri::command]
fn delete_item(id: usize) -> Result<bool, String> {
    history::active()
        .ok_or_else(|| "历史服务未就绪".to_owned())?
        .delete(id)
}
#[tauri::command]
fn undo_delete() -> Result<Option<history::Preview>, String> {
    history::active()
        .ok_or_else(|| "历史服务未就绪".to_owned())?
        .undo_delete()
}
#[tauri::command]
fn get_trash() -> Result<Vec<history::Trashed>, String> {
    history::active()
        .ok_or_else(|| "历史服务未就绪".to_owned())?
        .trash()
}
#[tauri::command]
fn restore_item(app: AppHandle, id: usize) -> Result<Option<history::Preview>, String> {
    let restored = history::active()
        .ok_or_else(|| "历史服务未就绪".to_owned())?
        .restore(id)?;
    // 面板开着时列表跟着刷新。
    let _ = panel::dispatch(&app, Action::HistoryChanged);
    Ok(restored)
}
/// 物理删除回收站里的条目；`ids` 为空表示清空回收站。
#[tauri::command]
fn purge_items(ids: Option<Vec<usize>>) -> Result<usize, String> {
    history::active()
        .ok_or_else(|| "历史服务未就绪".to_owned())?
        .purge(ids)
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
fn retry_activation(app: AppHandle, session: u64) -> Result<(), String> {
    panel::dispatch(&app, Action::RetryActivation { session })
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
    selection: Option<paste::permit::Selection>,
) -> Result<(), String> {
    panel::dispatch(
        &app,
        Action::Paste {
            session,
            id,
            mode,
            selection,
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 守护者在这里就结束，不初始化 Tauri；真正的应用（--child）继续往下走。
    if let Some(code) = recovery::guard() {
        std::process::exit(code);
    }
    let app = tauri::Builder::default()
        .plugin(tray::single_instance())
        .plugin(tray::autostart())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            get_settings,
            set_settings,
            get_capture_state,
            set_capture_paused,
            set_ignore_next,
            hotkey::validate_hotkey,
            hotkey::get_hotkey_status,
            hotkey::pause_hotkey,
            get_snapshots,
            create_snapshot,
            restore_snapshot,
            get_history,
            search_history,
            get_favorites,
            set_favorite,
            delete_item,
            undo_delete,
            get_trash,
            restore_item,
            purge_items,
            frontend_ready,
            start_search,
            search_ready,
            retry_activation,
            hide_panel,
            paste_item,
            report_painted,
            report_search_input,
            quit_prototype,
            settings_window::open_settings
        ])
        .setup(|app| {
            let settings_file = settings::settings_path();
            let (store, issues) = settings::Store::load(settings_file.clone());
            let resolved = settings::Resolved::new(store.get(), settings_file);
            let logger = log::Logger::with_settings(
                &resolved.directories,
                resolved.settings.log.max_total_mb,
            )?;
            logger.event(
                "startup",
                json!({
                    "platform":std::env::consts::OS,
                    "version":env!("CARGO_PKG_VERSION"),
                    "guarded":recovery::is_child(),
                }),
            );
            logger.event(
                "directories",
                json!({
                    "settings_dir":logger.directories.settings_file.parent(),
                    "data_dir":logger.directories.data,
                    "log_dir":logger.directories.logs,
                }),
            );
            for issue in issues.iter().chain(&resolved.issues) {
                issue.record(&logger);
            }
            app.manage(store);
            let window = app.get_webview_window("panel").ok_or("panel 窗口不存在")?;
            platform::window::no_activate(&window, true)?;
            platform::webview::initialize(&window)?;
            let plan = autocycle::Plan::from_env(&logger);
            capture::rules::gate().apply(&resolved.settings.capture);
            history::snapshots::set_limit(resolved.settings.snapshots.max_mb);
            let resuspend = resuspend::from_settings(&resolved.settings.memory, &logger);
            let experiments =
                panel::Experiments::from_settings(&resolved.settings.experiments, &logger);
            let automatic = plan.is_some();
            app.manage(Controller::new(
                window,
                logger.clone(),
                plan,
                resuspend,
                experiments,
            )?);
            // A06 图标：asset 协议只放开 <数据目录>\icons 这一个目录（非递归），
            // 数据目录其余内容（库、日志、导出文件）不进入 webview 可读范围；
            // 数据目录可由设置指定，所以范围在运行时登记，配置里只留空 scope。
            let icons = resolved.directories.data.join("icons");
            std::fs::create_dir_all(&icons)?;
            app.asset_protocol_scope().allow_directory(&icons, false)?;
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
            hotkey::setup(
                app.handle(),
                &logger,
                automatic,
                &resolved.settings.hotkey,
                std::env::var_os("NIANLAI_HOTKEY").is_some(),
            );
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
