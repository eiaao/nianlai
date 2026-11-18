//! 新版本检测、提醒与原地更新。
//!
//! - 启动后延迟约 90 秒首次检查，之后每 24 小时一次（失败 1 小时后重试）；设置里可关闭自动检查。
//! - 每个新版本只发一次系统通知（Windows）；面板正在显示时等它关闭后再发。macOS 不发系统通知，
//!   只在菜单栏菜单里显示“有新版本”。
//! - 原地更新只在用户点“立即更新”后进行，任何一步失败旧版本都保持可用。
//! - 预览：环境变量 `NIANLAI_UPDATE_FEED=<本地 json 路径或 URL>` 覆盖版本文件来源，首次检查缩短到几秒，
//!   且状态只保存在内存里，不写设置文件，也不会真正替换程序。

pub mod feed;
mod http;
#[cfg(target_os = "macos")]
mod mac;
mod package;
pub mod policy;
pub mod process;
#[cfg(windows)]
mod toast;

use crate::{
    app::Controller,
    log::{Logger, now_ms},
    panel::{self, Action},
    settings::Store,
    settings_window, tray,
};
use feed::{Latest, Note};
use policy::{CheckResult, Notify, State};
use serde::Serialize;
use serde_json::json;
use std::{
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager};

const PREVIEW_ENV: &str = "NIANLAI_UPDATE_FEED";
const TICK: Duration = Duration::from_secs(5);
const PROGRESS_EMIT: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Idle,
    Downloading,
    Verifying,
    Installing,
    Restarting,
    Failed,
}

struct Inner {
    state: State,
    checking: bool,
    phase: Phase,
    received: u64,
    total: Option<u64>,
    error: Option<String>,
    check_error: Option<String>,
}

pub struct Updater {
    logger: Logger,
    current: &'static str,
    preview: Option<String>,
    started_ms: u64,
    install_block: Option<String>,
    inner: Mutex<Inner>,
}

/// 界面用的更新状态快照（设置窗口、托盘共用）。
#[derive(Serialize)]
pub struct Status {
    current: String,
    auto_check: bool,
    preview: bool,
    has_update: bool,
    latest_version: Option<String>,
    date: String,
    /// 只含比当前版本新的更新说明。
    notes: Vec<Note>,
    skipped: bool,
    show_dot: bool,
    show_banner: bool,
    last_check_ms: Option<u64>,
    last_result: CheckResult,
    check_error: Option<String>,
    checking: bool,
    phase: Phase,
    received: u64,
    total: Option<u64>,
    error: Option<String>,
    /// 为空表示可以原地更新；否则是不能的原因（界面改为引导去下载页）。
    install_block: Option<String>,
    download_page: &'static str,
}

fn preview_source() -> Option<String> {
    std::env::var(PREVIEW_ENV)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn compute_install_block() -> Option<String> {
    if cfg!(debug_assertions) {
        return Some("开发版不能原地更新".into());
    }
    #[cfg(windows)]
    {
        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(_) => return Some("没找到拈来的程序位置，没法自动更新".into()),
        };
        let named = exe.file_name().is_some_and(|name| {
            name.to_string_lossy()
                .eq_ignore_ascii_case(package::EXE_NAME)
        });
        if !named {
            return Some("当前程序文件名不是 nianlai.exe，没法自动更新".into());
        }
        match exe.parent() {
            Some(dir) if package::directory_writable(dir) => None,
            _ => Some("拈来所在的文件夹没有写入权限，没法自动更新。请到下载页手动更新".into()),
        }
    }
    #[cfg(target_os = "macos")]
    {
        mac::install_block()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        Some("这个系统暂不支持自动更新".into())
    }
}

impl Updater {
    pub fn new(logger: Logger, saved: State) -> Self {
        let preview = preview_source();
        Self {
            logger,
            current: env!("CARGO_PKG_VERSION"),
            // 预览模式不读写持久状态：每次启动都是“全新”的，方便反复看效果。
            inner: Mutex::new(Inner {
                state: if preview.is_some() {
                    State::default()
                } else {
                    saved
                },
                checking: false,
                phase: Phase::Idle,
                received: 0,
                total: None,
                error: None,
                check_error: None,
            }),
            preview,
            started_ms: now_ms(),
            install_block: compute_install_block(),
        }
    }

    fn first_delay(&self) -> u64 {
        if self.preview.is_some() {
            policy::PREVIEW_FIRST_DELAY_MS
        } else {
            policy::FIRST_DELAY_MS
        }
    }

    fn status(&self, auto_check: bool) -> Status {
        let inner = self.inner.lock().unwrap();
        let latest = inner.state.available(self.current);
        Status {
            current: self.current.to_owned(),
            auto_check,
            preview: self.preview.is_some(),
            has_update: latest.is_some(),
            latest_version: latest.map(|latest| latest.version.clone()),
            date: latest.map(|latest| latest.date.clone()).unwrap_or_default(),
            notes: latest
                .map(|latest| latest.notes_newer_than(self.current))
                .unwrap_or_default(),
            skipped: latest.is_some_and(|latest| inner.state.skipped(&latest.version)),
            show_dot: inner.state.show_dot(self.current),
            show_banner: inner.state.show_banner(self.current),
            last_check_ms: inner.state.last_check_ms,
            last_result: inner.state.last_result,
            check_error: inner.check_error.clone(),
            checking: inner.checking,
            phase: inner.phase,
            received: inner.received,
            total: inner.total,
            error: inner.error.clone(),
            install_block: self.install_block.clone(),
            download_page: feed::DOWNLOAD_PAGE,
        }
    }

    fn event(&self, name: &str, fields: serde_json::Value) {
        self.logger.event(name, fields);
    }
}

fn auto_check_enabled(app: &AppHandle) -> bool {
    app.state::<Store>().get().update.auto_check
}

fn status_of(app: &AppHandle) -> Status {
    app.state::<Updater>().status(auto_check_enabled(app))
}

/// 状态变化后：写盘（非预览）、通知界面、刷新托盘菜单项。
fn changed(app: &AppHandle, persist: bool) {
    let updater = app.state::<Updater>();
    if persist && updater.preview.is_none() {
        let snapshot = updater.inner.lock().unwrap().state.clone();
        let value = json!({"update": {
            "last_check_ms": snapshot.last_check_ms,
            "last_result": snapshot.last_result,
            "latest": snapshot.latest,
            "notified_version": snapshot.notified_version,
            "skipped_version": snapshot.skipped_version,
            "banner_dismissed_version": snapshot.banner_dismissed_version,
        }});
        if app.state::<Store>().set(value).is_err() {
            updater.event("update_state_save_failed", json!({}));
        }
    }
    let status = status_of(app);
    let tray_version = {
        let inner = updater.inner.lock().unwrap();
        inner
            .state
            .show_tray_item(updater.current)
            .then(|| status.latest_version.clone())
            .flatten()
    };
    tray::set_update_item(app, tray_version);
    let _ = app.emit("update:changed", &status);
}

fn mutate(app: &AppHandle, persist: bool, change: impl FnOnce(&mut Inner)) {
    {
        let updater = app.state::<Updater>();
        change(&mut updater.inner.lock().unwrap());
    }
    changed(app, persist);
}

// ---------- 检查 ----------

fn fetch_feed(updater: &Updater) -> Result<Latest, String> {
    let ext = if updater.preview.is_some() {
        ""
    } else {
        feed::PACKAGE_EXT
    };
    let bytes = match &updater.preview {
        Some(source) if !source.starts_with("http://") && !source.starts_with("https://") => {
            let path = PathBuf::from(source);
            let bytes = std::fs::read(&path).map_err(|_| "读不到预览用的版本文件".to_owned())?;
            if bytes.len() > feed::FEED_MAX_BYTES {
                return Err("版本文件过大".into());
            }
            bytes
        }
        other => {
            let url = other.clone().unwrap_or_else(feed::feed_url);
            http::get_bytes(
                &url,
                other.is_some(),
                feed::FEED_MAX_BYTES,
                Duration::from_secs(15),
            )?
        }
    };
    feed::parse(&bytes, ext).map_err(str::to_owned)
}

/// 做一次检查。`manual` 为真时不受“自动检查”开关和节流限制。
fn check(app: &AppHandle, manual: bool) -> Result<(), String> {
    let updater = app.state::<Updater>();
    {
        let mut inner = updater.inner.lock().unwrap();
        if inner.checking
            || matches!(
                inner.phase,
                Phase::Downloading | Phase::Verifying | Phase::Installing | Phase::Restarting
            )
        {
            return Err("正在处理上一次操作，稍后再试".into());
        }
        inner.checking = true;
        inner.check_error = None;
    }
    changed(app, false);
    let outcome = fetch_feed(&updater);
    let now = now_ms();
    let current = updater.current;
    mutate(app, true, |inner| {
        inner.checking = false;
        match &outcome {
            Ok(latest) => inner.state.record_success(now, current, latest.clone()),
            Err(error) => {
                inner.state.record_failure(now);
                inner.check_error = Some(error.clone());
            }
        }
    });
    match &outcome {
        Ok(latest) => updater.event(
            "update_check",
            json!({"manual":manual,"result":"ok","latest":latest.version,"newer":feed::is_newer(&latest.version, current)}),
        ),
        Err(error) => updater.event(
            "update_check",
            json!({"manual":manual,"result":"failed","error":error}),
        ),
    }
    maybe_notify(app);
    outcome.map(drop)
}

// ---------- 通知 ----------

#[cfg(windows)]
fn panel_visible(app: &AppHandle) -> bool {
    app.get_webview_window("panel")
        .and_then(|window| window.is_visible().ok())
        .unwrap_or(false)
}

#[cfg(windows)]
fn maybe_notify(app: &AppHandle) {
    let updater = app.state::<Updater>();
    let (decision, version) = {
        let inner = updater.inner.lock().unwrap();
        (
            inner
                .state
                .notify_decision(updater.current, panel_visible(app)),
            inner
                .state
                .latest
                .as_ref()
                .map(|latest| latest.version.clone()),
        )
    };
    let (Notify::Now, Some(version)) = (decision, version) else {
        return;
    };
    // 先记“已通知”再发：失败也不重复打扰，回退到绿点、横幅和托盘项。
    mutate(app, true, |inner| {
        inner.state.notified_version = Some(version.clone());
    });
    let handle = app.clone();
    let result = toast::show_update(&version, move |choice| match choice {
        toast::Choice::View => open_about(&handle),
        toast::Choice::Ignore(version) => skip_version(&handle, Some(version), true),
    });
    match result {
        Ok(()) => updater.event("update_toast", json!({"version":version,"result":"shown"})),
        Err(error) => updater.event(
            "update_toast",
            json!({"version":version,"result":"failed","error":error}),
        ),
    }
}

/// macOS 不发系统通知。
#[cfg(not(windows))]
fn maybe_notify(_app: &AppHandle) {
    let _ = Notify::No;
}

pub fn open_about(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if settings_window::open_page(&app, Some("about")).is_err() {
            app.state::<Updater>()
                .event("update_open_about_failed", json!({}));
        }
    });
}

fn skip_version(app: &AppHandle, only: Option<String>, skip: bool) {
    mutate(app, true, |inner| {
        let target = inner
            .state
            .latest
            .as_ref()
            .map(|latest| latest.version.clone());
        // 旧通知上的“忽略”只对它当时的版本生效，不覆盖对更新版本的状态。
        if let Some(target) =
            target.filter(|target| only.as_ref().is_none_or(|only| only == target))
        {
            inner.state.skipped_version = skip.then_some(target);
        }
    });
}

// ---------- 后台线程 ----------

/// 在 `setup` 里、托盘建好之后调用。
pub fn start(app: &AppHandle) {
    // 上次更新留下的 .old/.new：旧守护进程可能还没退出，稍后重试几次。
    #[cfg(windows)]
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent().map(std::path::Path::to_path_buf)
    {
        let _ = std::thread::Builder::new()
            .name("update-cleanup".into())
            .spawn(move || {
                for delay in [0u64, 3, 10, 30, 120] {
                    std::thread::sleep(Duration::from_secs(delay));
                    if package::clean_leftovers(&dir) == 0 {
                        break;
                    }
                }
                let _ = std::fs::remove_dir_all(std::env::temp_dir().join("nianlai-update"));
            });
    }
    changed(app, false);
    let app = app.clone();
    let _ = std::thread::Builder::new()
        .name("update-check".into())
        .spawn(move || {
            loop {
                std::thread::sleep(TICK);
                let updater = app.state::<Updater>();
                let due = {
                    let inner = updater.inner.lock().unwrap();
                    !inner.checking
                        && matches!(inner.phase, Phase::Idle | Phase::Failed)
                        && policy::check_due(
                            auto_check_enabled(&app),
                            now_ms(),
                            updater.started_ms,
                            updater.first_delay(),
                            &inner.state,
                        )
                };
                if due {
                    let _ = check(&app, false);
                } else {
                    // 面板关闭后补发之前延后的通知。
                    maybe_notify(&app);
                }
            }
        });
}

// ---------- 命令 ----------

#[tauri::command]
pub fn get_update_status(app: AppHandle) -> Status {
    status_of(&app)
}

#[tauri::command]
pub async fn check_update_now(app: AppHandle) -> Result<Status, String> {
    let handle = app.clone();
    // 失败原因已经记在状态里（check_error），界面从状态读取，这里不再把错误抛给前端。
    let result = tauri::async_runtime::spawn_blocking(move || check(&handle, true))
        .await
        .map_err(|_| "检查没完成，再试一次".to_owned())?;
    if let Err(error) = result
        && error.contains("稍后再试")
    {
        return Err(error);
    }
    Ok(status_of(&app))
}

#[tauri::command]
pub fn set_update_skipped(app: AppHandle, skipped: bool) {
    skip_version(&app, None, skipped);
}

#[tauri::command]
pub fn dismiss_update_banner(app: AppHandle) {
    mutate(&app, true, |inner| {
        if let Some(latest) = &inner.state.latest {
            inner.state.banner_dismissed_version = Some(latest.version.clone());
        }
    });
}

#[tauri::command]
pub fn open_download_page() -> Result<(), String> {
    process::open_url(feed::DOWNLOAD_PAGE)
}

/// “关于”页的项目链接：GitHub 仓库、作者的 X。
#[tauri::command]
pub fn open_project_link(what: String) -> Result<(), String> {
    let url = process::project_url(&what).ok_or_else(|| "未知链接".to_owned())?;
    process::open_url(url)
}

#[tauri::command]
pub fn start_update(app: AppHandle) -> Result<(), String> {
    let updater = app.state::<Updater>();
    let latest = {
        let mut inner = updater.inner.lock().unwrap();
        if inner.checking || !matches!(inner.phase, Phase::Idle | Phase::Failed) {
            return Err("正在处理上一次操作".into());
        }
        let latest = inner
            .state
            .available(updater.current)
            .cloned()
            .ok_or("现在没有可更新的新版本")?;
        inner.phase = Phase::Downloading;
        inner.received = 0;
        inner.total = None;
        inner.error = None;
        latest
    };
    if let Some(reason) = updater.install_block.clone() {
        fail_install(&app, reason);
        return Ok(());
    }
    changed(&app, false);
    let handle = app.clone();
    std::thread::Builder::new()
        .name("update-install".into())
        .spawn(move || {
            if let Err(error) = install(&handle, &latest) {
                fail_install(&handle, error);
            }
        })
        .map(drop)
        .map_err(|_| "没能开始更新".to_owned())
}

fn fail_install(app: &AppHandle, message: String) {
    let updater = app.state::<Updater>();
    updater.event("update_install", json!({"result":"failed","error":message}));
    let _ = std::fs::remove_dir_all(std::env::temp_dir().join("nianlai-update"));
    mutate(app, false, |inner| {
        inner.phase = Phase::Failed;
        inner.error = Some(message);
    });
}

fn set_phase(app: &AppHandle, phase: Phase) {
    mutate(app, false, |inner| inner.phase = phase);
}

/// 下载 → 校验 → 替换 → 启动新进程 → 当前进程退出。
fn install(app: &AppHandle, latest: &Latest) -> Result<(), String> {
    let updater = app.state::<Updater>();
    let work = std::env::temp_dir().join("nianlai-update");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|_| "没法创建临时目录".to_owned())?;
    let file_name = latest
        .url
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .ok_or("下载地址无效")?;
    let package = work.join(file_name);
    updater.event(
        "update_install",
        json!({"result":"start","version":latest.version}),
    );

    let mut last_emit = Instant::now() - PROGRESS_EMIT;
    package::download(
        &latest.url,
        updater.preview.is_some(),
        &package,
        &mut |received, total| {
            if last_emit.elapsed() >= PROGRESS_EMIT {
                last_emit = Instant::now();
                mutate(app, false, |inner| {
                    inner.received = received;
                    inner.total = total;
                });
            }
        },
    )
    .map_err(|error| format!("下载没完成：{error}"))?;

    set_phase(app, Phase::Verifying);
    let actual = package::sha256_file(&package)?;
    if actual != latest.sha256 {
        return Err("下载的文件没通过校验，已丢弃，没有改动现有版本".into());
    }
    if updater.preview.is_some() {
        return Err("预览模式只演示界面，不会真正替换程序".into());
    }

    set_phase(app, Phase::Installing);
    install_platform(&package, &work)?;

    set_phase(app, Phase::Restarting);
    updater.event(
        "update_install",
        json!({"result":"ok","version":latest.version}),
    );
    app.state::<Controller>()
        .logger
        .event("quit_source", json!({"source":"update"}));
    panel::dispatch(app, Action::Quit).map_err(|_| "新版本已装好，请手动重新打开拈来".to_owned())
}

#[cfg(windows)]
fn install_platform(package_path: &std::path::Path, _work: &std::path::Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|_| "没找到拈来的程序位置".to_owned())?;
    let dir = exe.parent().ok_or("没找到拈来的程序位置")?.to_path_buf();
    let names = package::validate_portable_zip(package_path)?;
    if let Err(error) = package::extract_as_new(package_path, &names, &dir) {
        package::discard_new(&dir, &names);
        return Err(error);
    }
    package::swap_in(&dir, &names)?;
    // 新进程带着旧进程的 PID 启动，等旧进程退出后再初始化（单实例交接）。
    if process::spawn_successor(&dir.join(package::EXE_NAME)).is_err() {
        package::rollback(&dir, &names.iter().collect::<Vec<_>>());
        return Err("新版本没能启动，已还原旧版本".into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn install_platform(package_path: &std::path::Path, work: &std::path::Path) -> Result<(), String> {
    mac::install(package_path, work)?;
    mac::relaunch()
}

#[cfg(not(any(windows, target_os = "macos")))]
fn install_platform(_package: &std::path::Path, _work: &std::path::Path) -> Result<(), String> {
    Err("这个系统暂不支持自动更新".into())
}
