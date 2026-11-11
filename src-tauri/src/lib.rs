mod app;
mod autocycle;
mod capture;
mod directories;
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
mod shortcut;
mod tray;

use app::Controller;
use history::History;
use panel::Action;
use serde_json::json;
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager, State};

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
    tauri::async_runtime::spawn_blocking(move || save_settings(&app, settings))
        .await
        .map_err(|_| "设置保存任务未完成，请重试".to_owned())?
}

// 保存设置的编排：写盘 → 换呼出键 → 广播 → 即时生效 → 记日志。
// hotkey 只负责换键并留下结果；持锁让跨窗口并发保存按同一顺序执行。
fn save_settings(app: &AppHandle, update: serde_json::Value) -> Result<settings::Settings, String> {
    let runtime = app.state::<hotkey::Runtime>();
    let mut hotkey = runtime.lock(app);
    let controller = app.state::<Controller>();
    let update_hotkey = update.get("hotkey").is_some();
    let (saved, issues) = settings::save(
        &app.state::<settings::Store>(),
        update,
        |saved| hotkey.apply(&saved.hotkey),
        |saved| {
            if let Err(error) = app.emit("settings:changed", saved) {
                controller.logger.event(
                    "settings_broadcast_failed",
                    json!({"error":error.to_string()}),
                );
            }
        },
    )?;
    for issue in issues {
        issue.record(&controller.logger);
    }
    // 采集规则即时生效：下一次复制就按新规则检查。
    apply_live_settings(&saved);
    if update_hotkey {
        hotkey.record(&controller.logger);
    }
    controller.logger.event(
        "settings_saved",
        json!({"reason":"设置已保存；呼出键即时应用，其他设置按各自策略生效"}),
    );
    Ok(saved)
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
    // 当前库版本；快照版本低于它的，恢复后会自动升级。
    current_version: u32,
    // 本次启动时执行的恢复；设置页据此提示成功或失败。
    restored: Option<history::snapshots::Restored>,
    // 是否由守护者拉起：不是的话恢复后不会自动重启，需要手动打开。
    relaunch: bool,
}

fn store_directory(history: &History) -> Result<std::path::PathBuf, String> {
    history
        .store_directory()
        .ok_or_else(|| "历史库未打开，暂时不能使用快照".to_owned())
}

#[tauri::command]
fn get_snapshots(history: State<'_, History>) -> Result<SnapshotState, String> {
    Ok(SnapshotState {
        items: history::snapshots::list(&store_directory(&history)?)?,
        keep: nianlai_store::SNAPSHOT_KEEP,
        current_version: nianlai_store::VERSION,
        restored: history::snapshots::last_restore(),
        relaunch: recovery::is_child(),
    })
}

#[tauri::command]
async fn create_snapshot(history: State<'_, History>, app: AppHandle) -> Result<(), String> {
    let directory = store_directory(&history)?;
    tauri::async_runtime::spawn_blocking(move || {
        history::snapshots::manual(&directory, &app.state::<Controller>().logger)
    })
    .await
    .map_err(|_| "快照任务未完成，请重试".to_owned())?
}

#[tauri::command]
async fn restore_snapshot(
    history: State<'_, History>,
    app: AppHandle,
    name: String,
) -> Result<(), String> {
    let directory = store_directory(&history)?;
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

/// 备份与导入（M2-4）。
#[tauri::command]
async fn backup_summary(history: State<'_, History>) -> Result<history::backup::Summary, String> {
    blocking(history, |history| history.backup_summary()).await
}
/// 原生“另存为”；归设置窗口所有。取消返回 None。
#[tauri::command]
async fn choose_backup_save(
    app: AppHandle,
    default_name: String,
) -> Result<Option<String>, String> {
    choose_backup(app, true, default_name).await
}
#[tauri::command]
async fn choose_backup_open(app: AppHandle) -> Result<Option<String>, String> {
    choose_backup(app, false, String::new()).await
}
async fn choose_backup(
    app: AppHandle,
    save: bool,
    default_name: String,
) -> Result<Option<String>, String> {
    let owner = app
        .get_webview_window(settings_window::LABEL)
        .and_then(|window| platform::window::handle(&window).ok())
        .unwrap_or(0);
    // 只取文件名部分，防止默认名带路径。
    let name = std::path::Path::new(&default_name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // 对话框会一直等到用户操作，不能占着主线程或异步线程。
    tauri::async_runtime::spawn_blocking(move || {
        platform::dialog::choose_backup(owner, save, &name)
    })
    .await
    .map_err(|_| "文件对话框没有完成，请重试".to_owned())?
}
#[tauri::command]
async fn export_backup(
    history: State<'_, History>,
    path: String,
) -> Result<history::backup::Exported, String> {
    blocking(history, move |history| history.export_backup(&path)).await
}
#[tauri::command]
async fn reveal_in_explorer(path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || history::backup::reveal(&path))
        .await
        .map_err(|_| "操作没有完成，请重试".to_owned())?
}
#[tauri::command]
async fn inspect_backup(
    history: State<'_, History>,
    path: String,
) -> Result<nianlai_store::Inspection, String> {
    blocking(history, move |history| history.inspect_backup(&path)).await
}
#[tauri::command]
async fn import_backup(
    history: State<'_, History>,
    app: AppHandle,
    token: String,
    mode: history::backup::Mode,
) -> Result<history::backup::Imported, String> {
    let imported = blocking(history, move |history| history.import_backup(&token, mode)).await?;
    if imported.relaunch {
        // 覆盖已登记：和恢复快照一样，退出并由守护者重启，启动时完成替换。
        app.state::<Controller>()
            .logger
            .event("quit_source", json!({"source":"backup_import"}));
        panel::dispatch(&app, Action::Relaunch)?;
    }
    Ok(imported)
}
#[tauri::command]
async fn discard_import(history: State<'_, History>, token: String) -> Result<(), String> {
    blocking(history, move |history| {
        history.discard_import(&token);
        Ok(())
    })
    .await
}

#[tauri::command]
fn get_history(history: State<'_, History>) -> history::Snapshot {
    history.snapshot()
}
#[tauri::command]
async fn search_history(
    history: State<'_, History>,
    query: String,
    request_id: u64,
) -> Result<history::SearchReply, String> {
    let history = history.inner().clone();
    tauri::async_runtime::spawn_blocking(move || history.search(&query, request_id))
        .await
        .map_err(|_| "search_worker_failed".to_owned())?
}
// E01 收藏：命令层经 State<History> 取历史服务。
#[tauri::command]
fn get_favorites(history: State<'_, History>) -> Result<Vec<history::Preview>, String> {
    history.favorites()
}
#[tauri::command]
fn set_favorite(
    history: State<'_, History>,
    id: usize,
    favorite: bool,
) -> Result<Option<history::Preview>, String> {
    history.set_favorite(id, favorite)
}
// E03/E04 收藏整理：ids 是界面当前展示的全部收藏的新顺序。
#[tauri::command]
fn reorder_favorites(
    history: State<'_, History>,
    ids: Vec<usize>,
) -> Result<Vec<history::Preview>, String> {
    history.reorder_favorites(&ids)
}
#[tauri::command]
fn set_favorite_label(
    history: State<'_, History>,
    id: usize,
    title: String,
    note: String,
) -> Result<history::Preview, String> {
    history.set_favorite_label(id, &title, &note)
}
#[tauri::command]
fn delete_item(history: State<'_, History>, id: usize) -> Result<bool, String> {
    history.delete(id)
}
#[tauri::command]
fn undo_delete(history: State<'_, History>) -> Result<Option<history::Preview>, String> {
    history.undo_delete()
}
#[tauri::command]
fn get_trash(history: State<'_, History>) -> Result<Vec<history::Trashed>, String> {
    history.trash()
}
#[tauri::command]
fn restore_item(
    history: State<'_, History>,
    app: AppHandle,
    id: usize,
) -> Result<Option<history::Preview>, String> {
    let restored = history.restore(id)?;
    // 面板开着时列表跟着刷新。
    let _ = panel::dispatch(&app, Action::HistoryChanged);
    Ok(restored)
}
/// 物理删除回收站里的条目；`ids` 为空表示清空回收站。
#[tauri::command]
fn purge_items(history: State<'_, History>, ids: Option<Vec<usize>>) -> Result<usize, String> {
    history.purge(ids)
}
/// 存储管理（M2-3）的命令都可能读写较多数据，放到阻塞线程池里跑，不卡主线程。
async fn blocking<T: Send + 'static>(
    history: State<'_, History>,
    job: impl FnOnce(History) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let history = history.inner().clone();
    tauri::async_runtime::spawn_blocking(move || job(history))
        .await
        .map_err(|_| "操作没有完成，请重试".to_owned())?
}
/// 文件条目的每个路径现在还在不在（预览用）；不是文件条目返回 None。
#[tauri::command]
async fn get_file_paths(
    history: State<'_, History>,
    id: usize,
) -> Result<Option<Vec<history::files::PathCheck>>, String> {
    blocking(history, move |history| history.file_paths(id)).await
}
#[tauri::command]
async fn get_storage(history: State<'_, History>) -> Result<history::storage::Overview, String> {
    blocking(history, |history| history.storage_overview()).await
}
/// 立即全量检查文件条目（设置页“重新检查”）。
#[tauri::command]
async fn check_files(history: State<'_, History>) -> Result<(), String> {
    blocking(history, |history| {
        if history.check_files(None, "manual") {
            history.notify_changed();
        }
        Ok(())
    })
    .await
}
#[tauri::command]
async fn clean_missing(history: State<'_, History>) -> Result<usize, String> {
    blocking(history, |history| history.clean_missing()).await
}
/// 清理截图类图片：`older_days` 为空表示全部。
#[tauri::command]
async fn clean_images(
    history: State<'_, History>,
    older_days: Option<u64>,
) -> Result<usize, String> {
    blocking(history, move |history| history.clean_images(older_days)).await
}
#[tauri::command]
async fn trash_items(history: State<'_, History>, ids: Vec<usize>) -> Result<usize, String> {
    blocking(history, move |history| history.trash_items(ids)).await
}
#[tauri::command]
async fn get_images(
    history: State<'_, History>,
) -> Result<Vec<history::storage::ImageItem>, String> {
    blocking(history, |history| history.images()).await
}
#[tauri::command]
async fn get_thumbnail(history: State<'_, History>, id: usize) -> Result<Option<String>, String> {
    blocking(history, move |history| history.thumbnail(id)).await
}
#[tauri::command]
async fn clean_exports(history: State<'_, History>) -> Result<history::storage::Files, String> {
    blocking(history, |history| history.clean_exports()).await
}
#[tauri::command]
async fn clean_leftovers(history: State<'_, History>) -> Result<history::storage::Files, String> {
    blocking(history, |history| history.clean_leftovers()).await
}
#[tauri::command]
async fn compact_history(history: State<'_, History>) -> Result<u64, String> {
    blocking(history, |history| history.compact()).await
}
/// 完整预览的全文（M2-7）：`limit` 为空表示全部。
#[tauri::command]
async fn get_full_text(
    history: State<'_, History>,
    id: usize,
    limit: Option<usize>,
) -> Result<Option<history::preview::TextChunk>, String> {
    blocking(history, move |history| history.full_text(id, limit)).await
}
/// 预览用的图片：`max` 为长边上限，空表示原尺寸。
#[tauri::command]
async fn get_image(
    history: State<'_, History>,
    id: usize,
    max: Option<u32>,
) -> Result<Option<history::preview::ImageView>, String> {
    blocking(history, move |history| history.image(id, max)).await
}
/// 完整预览里选中的文字复制到剪贴板。
#[tauri::command]
async fn copy_selection(app: AppHandle, id: usize, text: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<Controller>().copy_selection(id, &text)
    })
    .await
    .map_err(|_| "复制没有完成，请重试".to_owned())?
}
/// 完整预览开着时让钩子把 Z、Ctrl+C 交给面板。
#[tauri::command]
fn set_preview_keys(open: bool) {
    platform::hook::set_preview_keys(open);
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
// 启动和保存设置共用：采集规则、快照上限、重复复制置顶都即时生效。
pub(crate) fn apply_live_settings(settings: &settings::Settings) {
    capture::rules::gate().apply(&settings.capture);
    history::snapshots::set_limit(settings.snapshots.max_mb);
    history::set_reuse_to_top(settings.history.reuse_to_top);
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
            backup_summary,
            choose_backup_save,
            choose_backup_open,
            export_backup,
            reveal_in_explorer,
            inspect_backup,
            import_backup,
            discard_import,
            get_history,
            search_history,
            get_favorites,
            set_favorite,
            reorder_favorites,
            set_favorite_label,
            delete_item,
            undo_delete,
            get_trash,
            restore_item,
            purge_items,
            get_file_paths,
            get_storage,
            check_files,
            clean_missing,
            clean_images,
            trash_items,
            get_images,
            get_thumbnail,
            clean_exports,
            clean_leftovers,
            compact_history,
            get_full_text,
            get_image,
            copy_selection,
            set_preview_keys,
            frontend_ready,
            start_search,
            search_ready,
            retry_activation,
            hide_panel,
            paste_item,
            report_painted,
            report_search_input,
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
            apply_live_settings(&resolved.settings);
            let resuspend = resuspend::from_settings(&resolved.settings.memory, &logger);
            let automatic = plan.is_some();
            let history = History::new(platform::hash::sha256, logger.clone());
            // 命令层经 State<History> 取同一个历史服务，先于任何前端调用登记。
            app.manage(history.clone());
            app.manage(Controller::new(
                window,
                logger.clone(),
                history,
                plan,
                resuspend,
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
                resolved.hotkey_overridden,
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
