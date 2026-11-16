//! 首次启动窗口与只读旁路事件。
//!
//! 窗口按需用代码创建；普通 panel 的窗口、会话、命中区域和粘贴状态机不在这里复用或改写。

use crate::{app::Controller, paste::outcome::Outcome, platform, settings_window};
use platform::monitor::{TargetMonitor, WorkArea, current_target_monitor};
use serde_json::{Value, json};
use std::sync::{Mutex, mpsc};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub const LABEL: &str = "guide";
pub const SAMPLE_TEXT: &str = "这是拈来的一句示例，复制后就会记下。";

/// 只有未完成的三拍需要自动出现；完成和整份跳过都不能再次弹出。
pub fn should_open(phase: crate::settings::GuidePhase) -> bool {
    matches!(
        phase,
        crate::settings::GuidePhase::AwaitCopy
            | crate::settings::GuidePhase::AwaitShow
            | crate::settings::GuidePhase::AwaitPaste
    )
}

const GUIDE_SIZE: (f64, f64) = (360.0, 360.0);
const GUIDE_PAPER_WIDTH: f64 = 320.0;
const GUIDE_TRANSPARENT: bool = true;
const GUIDE_ALWAYS_ON_TOP: bool = true;
const GUIDE_SKIP_TASKBAR: bool = true;
const GUIDE_FOCUSABLE: bool = false;
const GUIDE_RESIZABLE: bool = false;
const GUIDE_DECORATIONS: bool = false;
const GUIDE_MARGIN: f64 = 16.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseAction {
    DestroySelf,
    Quit,
}

pub(crate) fn close_action(label: &str) -> CloseAction {
    if label == LABEL || label == settings_window::LABEL {
        CloseAction::DestroySelf
    } else {
        CloseAction::Quit
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GuideCorner {
    Tray,
    Panel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GuideLayout {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

fn scaled(value: f64, scale: f64) -> i32 {
    (value * scale).round() as i32
}

fn guide_layout(work: WorkArea, scale: f64, corner: GuideCorner) -> GuideLayout {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let margin = scaled(GUIDE_MARGIN, scale).max(0);
    let available_width = (work.right - work.left - 2 * margin).max(1);
    let available_height = (work.bottom - work.top - 2 * margin).max(1);
    let width = scaled(GUIDE_SIZE.0, scale).clamp(1, available_width);
    let height = scaled(GUIDE_SIZE.1, scale).clamp(1, available_height);
    GuideLayout {
        x: work.right - margin - width,
        y: match corner {
            GuideCorner::Tray => work.bottom - margin - height,
            GuideCorner::Panel => work.top + margin,
        },
        width,
        height,
    }
}

fn place(window: &WebviewWindow, corner: GuideCorner) -> Result<(), String> {
    let target = current_target_monitor()?;
    let layout = guide_layout(target.work, target.scale, corner);
    platform::window::present_guide(window, layout.x, layout.y, layout.width, layout.height)
}

fn configure_initial_with<W>(
    window: &W,
    target: impl FnOnce() -> Result<TargetMonitor, String>,
    present: impl FnOnce(&W, GuideLayout, &[platform::HitRegion], f64) -> Result<(), String>,
) -> Result<(), String> {
    let target = target()?;
    let layout = guide_layout(target.work, target.scale, GuideCorner::Tray);
    present(window, layout, &initial_hit_regions(), target.scale)
}

fn configure_initial(window: &WebviewWindow) -> Result<(), String> {
    configure_initial_with(
        window,
        current_target_monitor,
        |window, layout, regions, scale| {
            platform::window::present_guide_initial(
                window,
                layout.x,
                layout.y,
                layout.width,
                layout.height,
                regions,
                scale,
            )
        },
    )
}

fn initial_hit_regions() -> [platform::HitRegion; 1] {
    let left = (GUIDE_SIZE.0 - GUIDE_PAPER_WIDTH) / 2.0;
    [platform::HitRegion {
        left,
        top: 40.0,
        right: left + GUIDE_PAPER_WIDTH,
        bottom: GUIDE_SIZE.1,
    }]
}

enum OpenOperation<W> {
    Configure(W),
    Reuse(W),
}

static OPEN_LOCK: Mutex<()> = Mutex::new(());

fn open_with<W>(
    mut exists: impl FnMut() -> Option<W>,
    build: impl FnOnce() -> Result<W, String>,
    mut on_main: impl FnMut(OpenOperation<W>) -> Result<(), String>,
) -> Result<(), String> {
    // 创建、配置和复用判断同属一个生命周期临界区；并发的第二个 open 只能复用已建窗口。
    let _open = OPEN_LOCK
        .lock()
        .map_err(|_| "首次启动窗口的打开状态不可用".to_owned())?;
    // 只查询一次：关闭与复用竞争时由主线程原生调用返回错误，不做 check 后 expect。
    if let Some(window) = exists() {
        return on_main(OpenOperation::Reuse(window));
    }
    let window = build()?;
    on_main(OpenOperation::Configure(window))
}

trait GuideMainThread {
    fn run_on_main(&self, job: Box<dyn FnOnce() + Send>) -> Result<(), String>;
}

impl GuideMainThread for AppHandle {
    fn run_on_main(&self, job: Box<dyn FnOnce() + Send>) -> Result<(), String> {
        self.run_on_main_thread(job)
            .map_err(|error| error.to_string())
    }
}

fn on_main<T>(
    app: &impl GuideMainThread,
    job: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String>
where
    T: Send + 'static,
{
    let (reply, receiver) = mpsc::sync_channel(1);
    app.run_on_main(Box::new(move || {
        let _ = reply.send(job());
    }))?;
    // 只在 spawn_blocking 的 worker 等待；主线程不等待 worker，也不持有 panel/采集锁。
    receiver
        .recv()
        .map_err(|_| "首次启动窗口的主线程操作没有返回结果".to_owned())?
}

/// 建窗只能在异步 worker；建窗后的全部原生配置、展示与复用展示统一切回主线程。
pub fn open(app: &AppHandle) -> Result<(), String> {
    open_with(
        || app.get_webview_window(LABEL),
        || {
            WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("guide.html".into()))
                .title("拈来 · 首次启动")
                .inner_size(GUIDE_SIZE.0, GUIDE_SIZE.1)
                .resizable(GUIDE_RESIZABLE)
                .decorations(GUIDE_DECORATIONS)
                .transparent(GUIDE_TRANSPARENT)
                .always_on_top(GUIDE_ALWAYS_ON_TOP)
                .skip_taskbar(GUIDE_SKIP_TASKBAR)
                .visible(false)
                .focused(false)
                .focusable(GUIDE_FOCUSABLE)
                .shadow(false)
                .build()
                .map_err(|error| error.to_string())
        },
        |operation| {
            on_main(app, move || match operation {
                OpenOperation::Configure(window) => {
                    let configured = configure_initial(&window);
                    if configured.is_err() {
                        let _ = window.close();
                    }
                    configured
                }
                OpenOperation::Reuse(window) => platform::window::show_no_activate(&window),
            })
        },
    )
}

#[tauri::command]
pub async fn open_guide(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || open(&app))
        .await
        .map_err(|_| "首次启动窗口没打开，再试一次".to_owned())?
}

#[tauri::command]
pub async fn guide_copy_sample(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || app.state::<Controller>().copy_guide_sample())
        .await
        .map_err(|_| "示例没复制成，再试一次".to_owned())?
}

#[tauri::command]
pub async fn set_guide_hit_regions(
    app: AppHandle,
    regions: Vec<platform::HitRegion>,
    armed: Option<bool>,
    scale: Option<f64>,
) -> Result<(), String> {
    let _ = scale;
    platform::window::validate_hit_regions(&regions)?;
    let armed = armed.unwrap_or(true);
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let Some(window) = handle.get_webview_window(LABEL) else {
            return;
        };
        if let Err(error) = platform::window::set_hit_regions(&window, &regions, armed)
            && let Some(controller) = handle.try_state::<Controller>()
        {
            controller
                .logger
                .event("guide_hit_regions_failed", json!({"error":error}));
        }
    })
    .map_err(|error| error.to_string())
}

type GuideSink<'a> = dyn FnMut(&str, &str, Value) -> Result<(), String> + 'a;

fn emit_to_guide_with(
    exists: bool,
    event: &str,
    payload: Value,
    emit: &mut GuideSink<'_>,
) -> Result<bool, String> {
    if !exists {
        return Ok(false);
    }
    emit(LABEL, event, payload)?;
    Ok(true)
}

fn history_changed_with(exists: bool, emit: &mut GuideSink<'_>) -> Result<bool, String> {
    emit_to_guide_with(exists, "history:changed", json!({}), emit)
}

fn panel_shown_with(exists: bool, session: u64, emit: &mut GuideSink<'_>) -> Result<bool, String> {
    emit_to_guide_with(
        exists,
        "guide:panel-shown",
        json!({"session":session}),
        emit,
    )
}

fn paste_result_with(
    exists: bool,
    session: u64,
    id: usize,
    result: &Outcome,
    emit: &mut GuideSink<'_>,
) -> Result<bool, String> {
    emit_to_guide_with(
        exists,
        "guide:paste-result",
        json!({"session":session,"id":id,"result":result.result}),
        emit,
    )
}

fn with_guide_sink<F>(app: &AppHandle, send: F) -> Result<bool, String>
where
    F: for<'a> FnOnce(bool, &mut GuideSink<'a>) -> Result<bool, String>,
{
    let exists = app.get_webview_window(LABEL).is_some();
    let mut sink = |target: &str, event: &str, payload: Value| {
        app.emit_to(target, event, payload)
            .map_err(|error| error.to_string())
    };
    send(exists, &mut sink)
}

fn log_emit_error(app: &AppHandle, event: &str, result: &Result<bool, String>) {
    if let Err(error) = result
        && let Some(controller) = app.try_state::<Controller>()
    {
        controller
            .logger
            .event("guide_event_failed", json!({"event":event,"error":error}));
    }
}

pub fn history_changed(app: &AppHandle) {
    let result = with_guide_sink(app, history_changed_with);
    log_emit_error(app, "history:changed", &result);
}

pub fn panel_shown(app: &AppHandle, session: u64) {
    let result = with_guide_sink(app, |exists, sink| panel_shown_with(exists, session, sink));
    log_emit_error(app, "guide:panel-shown", &result);
    if result.unwrap_or(false)
        && let Some(window) = app.get_webview_window(LABEL)
        && let Err(error) = place(&window, GuideCorner::Panel)
    {
        log_emit_error(app, "guide:panel-shown:placement", &Err(error));
    }
}

/// 只读既有最终 Outcome；错误由调用点忽略，绝不参与 PasteFinished 或粘贴结论。
pub fn paste_result(
    app: &AppHandle,
    session: u64,
    id: usize,
    result: &Outcome,
) -> Result<(), String> {
    let sent = with_guide_sink(app, |exists, sink| {
        paste_result_with(exists, session, id, result, sink)
    });
    log_emit_error(app, "guide:paste-result", &sent);
    sent.map(drop)
}

#[cfg(test)]
mod capability_tests {
    /// 前端测试用的是假 IPC，不经过 Tauri ACL；这里守住真实窗口需要的核心权限。
    /// 缺了它们，真机上引导关不掉（plugin:window|close not allowed by ACL）、收不到定向通知。
    fn permissions(json: &str) -> (Vec<String>, Vec<String>) {
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        let list = |key: &str| {
            value[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item.as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        (list("windows"), list("permissions"))
    }

    #[test]
    fn guide_window_can_listen_notify_panel_and_close_itself() {
        let (windows, permissions) = permissions(include_str!("../capabilities/guide.json"));
        assert_eq!(windows, [super::LABEL]);
        for needed in [
            "core:event:allow-listen",
            "core:event:allow-unlisten",
            "core:event:allow-emit-to",
            "core:window:allow-close",
        ] {
            assert!(permissions.iter().any(|p| p == needed), "guide 缺 {needed}");
        }
    }

    #[test]
    fn settings_window_can_send_targeted_guide_notices_but_not_broadcast() {
        let (_, permissions) = permissions(include_str!("../capabilities/settings.json"));
        assert!(permissions.iter().any(|p| p == "core:event:allow-emit-to"));
        assert!(
            permissions.iter().any(|p| p == "core:app:allow-version"),
            "关于页读版本号"
        );
        assert!(!permissions.iter().any(|p| p == "core:event:allow-emit"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        history,
        paste::outcome::{Outcome, PasteResult, Reason},
    };
    use serde_json::{Value, json};
    use std::{
        cell::Cell,
        sync::{Arc, Mutex, mpsc},
        thread,
        time::Duration,
    };
    #[cfg(windows)]
    use windows::Win32::UI::WindowsAndMessaging::WS_EX_NOACTIVATE;

    fn outcome(result: PasteResult) -> Outcome {
        Outcome::from_report(result.into(), json!({"clipboard_written":true}))
    }

    #[derive(Clone)]
    struct FakeMain {
        jobs: mpsc::Sender<Box<dyn FnOnce() + Send>>,
    }

    impl GuideMainThread for FakeMain {
        fn run_on_main(&self, job: Box<dyn FnOnce() + Send>) -> Result<(), String> {
            self.jobs
                .send(job)
                .map_err(|_| "测试主线程已退出".to_owned())
        }
    }

    fn fake_main() -> (FakeMain, thread::JoinHandle<thread::ThreadId>) {
        let (jobs, receiver) = mpsc::channel::<Box<dyn FnOnce() + Send>>();
        let main = thread::spawn(move || {
            let id = thread::current().id();
            while let Ok(job) = receiver.recv() {
                job();
            }
            id
        });
        (FakeMain { jobs }, main)
    }

    #[test]
    fn task11_history_changed_reaches_guide_without_a_panel_snapshot() {
        let mut events = Vec::new();
        let mut emit = |target: &str, event: &str, payload: Value| {
            events.push((target.to_owned(), event.to_owned(), payload));
            Ok(())
        };
        assert_eq!(history_changed_with(true, &mut emit), Ok(true));
        assert_eq!(
            events,
            [("guide".into(), "history:changed".into(), json!({}))]
        );
    }

    #[test]
    fn task11_absent_guide_suppresses_every_new_event_and_broadcast() {
        let result = outcome(PasteResult::Confirmed);
        let exists = false;
        let mut calls = 0;
        let mut emit = |_: &str, _: &str, _: Value| {
            calls += 1;
            Ok(())
        };
        assert_eq!(history_changed_with(exists, &mut emit), Ok(false));
        assert_eq!(panel_shown_with(exists, 91, &mut emit), Ok(false));
        assert_eq!(
            paste_result_with(exists, 91, 17, &result, &mut emit),
            Ok(false)
        );
        assert_eq!(calls, 0);
    }

    #[test]
    fn task12_fix_health_notification_reuses_targeted_history_and_absent_guide_sends_nothing() {
        let mut absent_calls = 0;
        {
            let mut emit = |_: &str, _: &str, _: Value| {
                absent_calls += 1;
                Ok(())
            };
            assert_eq!(history_changed_with(false, &mut emit), Ok(false));
        }
        assert_eq!(absent_calls, 0, "guide 不存在时健康回调零发送、零广播");

        let mut events = Vec::new();
        let mut emit = |target: &str, event: &str, payload: Value| {
            events.push((target.to_owned(), event.to_owned(), payload));
            Ok(())
        };
        assert_eq!(history_changed_with(true, &mut emit), Ok(true));
        assert_eq!(
            events,
            [("guide".into(), "history:changed".into(), json!({}))]
        );
    }

    #[test]
    fn task11_panel_shown_uses_the_existing_session_and_only_targets_guide() {
        let mut events = Vec::new();
        let mut emit = |target: &str, event: &str, payload: Value| {
            events.push((target.to_owned(), event.to_owned(), payload));
            Ok(())
        };
        assert_eq!(panel_shown_with(true, 0x1234, &mut emit), Ok(true));
        assert_eq!(
            events,
            [(
                "guide".into(),
                "guide:panel-shown".into(),
                json!({"session":0x1234})
            )]
        );
    }

    #[test]
    fn task11_paste_result_serializes_every_success_and_failure_exactly() {
        for (paste_result, expected) in [
            (PasteResult::Sent, "sent"),
            (PasteResult::Confirmed, "confirmed"),
            (PasteResult::Copied, "copied"),
            (PasteResult::CopiedOnly, "copied_only"),
            (
                PasteResult::NativeSubmissionUnknown,
                "native_submission_unknown",
            ),
            (PasteResult::NativeNotSubmitted, "native_not_submitted"),
            (
                PasteResult::GenericSubmissionUnknown,
                "generic_submission_unknown",
            ),
            (PasteResult::GenericNotSubmitted, "generic_not_submitted"),
        ] {
            let result = outcome(paste_result);
            let before = result.clone();
            let mut events = Vec::new();
            let mut emit = |target: &str, event: &str, payload: Value| {
                events.push((target.to_owned(), event.to_owned(), payload));
                Ok(())
            };
            assert_eq!(
                paste_result_with(true, 77, 19, &result, &mut emit),
                Ok(true)
            );
            assert_eq!(
                events,
                [(
                    "guide".into(),
                    "guide:paste-result".into(),
                    json!({"session":77,"id":19,"result":expected})
                )]
            );
            assert_eq!(result, before, "旁路不得改写最终 Outcome");
        }

        let failure = Outcome::not_written(Reason::RecordMissing);
        let before = failure.clone();
        let mut emitted = None;
        let mut emit = |_: &str, _: &str, payload: Value| {
            emitted = Some(payload);
            Ok(())
        };
        assert_eq!(
            paste_result_with(true, 78, 20, &failure, &mut emit),
            Ok(true)
        );
        assert_eq!(
            emitted,
            Some(json!({"session":78,"id":20,"result":null})),
            "读库或记录失败也必须发出准确 null 结论"
        );
        assert_eq!(failure, before);
    }

    #[test]
    fn task11_emit_failure_does_not_change_the_existing_outcome() {
        let result = outcome(PasteResult::Confirmed);
        let before = result.clone();
        let mut emit = |_: &str, _: &str, _: Value| Err("注入发送失败".to_owned());
        assert_eq!(
            paste_result_with(true, 5, 6, &result, &mut emit),
            Err("注入发送失败".to_owned())
        );
        assert_eq!(result, before);
    }

    #[test]
    fn task12_only_unfinished_phases_open_automatically() {
        for phase in [
            crate::settings::GuidePhase::AwaitCopy,
            crate::settings::GuidePhase::AwaitShow,
            crate::settings::GuidePhase::AwaitPaste,
        ] {
            assert!(should_open(phase));
        }
        for phase in [
            crate::settings::GuidePhase::Completed,
            crate::settings::GuidePhase::Skipped,
        ] {
            assert!(!should_open(phase));
        }
    }

    #[cfg(windows)]
    #[test]
    fn task11_window_spec_is_transparent_topmost_unfocused_and_taskbarless() {
        assert_eq!(GUIDE_SIZE, (360.0, 360.0));
        assert_eq!(GUIDE_PAPER_WIDTH, 320.0);
        const {
            assert!(GUIDE_TRANSPARENT);
            assert!(GUIDE_ALWAYS_ON_TOP);
            assert!(GUIDE_SKIP_TASKBAR);
            assert!(!GUIDE_FOCUSABLE);
            assert!(!GUIDE_RESIZABLE);
            assert!(!GUIDE_DECORATIONS);
        }
        let style = platform::window::no_activate_style(0, true);
        assert_ne!(style & WS_EX_NOACTIVATE.0 as isize, 0);
    }

    #[test]
    fn task11_close_destroys_only_guide_and_settings_windows() {
        for label in [LABEL, settings_window::LABEL] {
            assert_eq!(close_action(label), CloseAction::DestroySelf);
        }
        assert_eq!(close_action("panel"), CloseAction::Quit);
    }

    #[test]
    fn task11_layout_stays_in_current_work_area_at_bottom_then_top_right() {
        let work = WorkArea {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        assert_eq!(
            guide_layout(work, 1.0, GuideCorner::Tray),
            GuideLayout {
                x: 1544,
                y: 704,
                width: 360,
                height: 360,
            }
        );
        assert_eq!(
            guide_layout(work, 1.0, GuideCorner::Panel),
            GuideLayout {
                x: 1544,
                y: 16,
                width: 360,
                height: 360,
            }
        );
        let scaled = guide_layout(work, 1.5, GuideCorner::Tray);
        assert_eq!((scaled.width, scaled.height), (540, 540));
        assert_eq!((scaled.x, scaled.y), (1356, 516));
        let small = guide_layout(
            WorkArea {
                left: 10,
                top: 20,
                right: 210,
                bottom: 220,
            },
            1.0,
            GuideCorner::Panel,
        );
        assert_eq!((small.width, small.height), (168, 168));
        assert_eq!((small.x, small.y), (26, 36));
    }

    #[cfg(windows)]
    #[test]
    fn task11_guide_hit_regions_use_independent_dpi_conversion() {
        let regions = [platform::HitRegion {
            left: 20.0,
            top: 300.0,
            right: 340.0,
            bottom: 360.0,
        }];
        let (hits, bounds) = platform::window::convert_hit_regions(&regions, 1.5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(
            (hits[0].left, hits[0].top, hits[0].right, hits[0].bottom),
            (18, 438, 522, 552)
        );
        assert_eq!(
            bounds.map(|r| (r.left, r.top, r.right, r.bottom)),
            Some((30, 450, 510, 540))
        );
    }

    #[test]
    fn task11_sample_text_is_fixed_and_copied_as_plain_text() {
        assert_eq!(SAMPLE_TEXT, "这是拈来的一句示例，复制后就会记下。");
        let content = history::content::Content::text(SAMPLE_TEXT, platform::hash::sha256).unwrap();
        assert_eq!(content.text_value().as_deref(), Some(SAMPLE_TEXT));
        assert_eq!(content.formats.len(), 1);
        assert!(content.formats.contains_key(history::content::TEXT));
    }

    #[test]
    fn task11_fix_open_runs_create_configuration_and_reuse_show_on_main() {
        let (main, main_thread) = fake_main();
        let native_calls = Arc::new(Mutex::new(Vec::new()));
        let build_threads = Arc::new(Mutex::new(Vec::new()));
        let worker = thread::spawn(move || {
            let worker_id = thread::current().id();
            let exists = Cell::new(false);
            for _ in 0..2 {
                open_with(
                    || exists.get().then_some(7),
                    || {
                        build_threads.lock().unwrap().push(thread::current().id());
                        exists.set(true);
                        Ok(7)
                    },
                    |operation| {
                        let native_calls = native_calls.clone();
                        on_main(&main, move || {
                            let (step, window) = match operation {
                                OpenOperation::Configure(window) => ("configure", window),
                                OpenOperation::Reuse(window) => ("reuse", window),
                            };
                            native_calls.lock().unwrap().push((
                                step,
                                window,
                                thread::current().id(),
                            ));
                            Ok(())
                        })
                    },
                )?;
            }
            Ok::<_, String>((worker_id, build_threads, native_calls))
        });
        let (worker_id, build_threads, native_calls) = worker.join().unwrap().unwrap();
        let main_id = main_thread.join().unwrap();
        assert_eq!(build_threads.lock().unwrap().as_slice(), &[worker_id]);
        assert_ne!(worker_id, main_id, "建窗必须留在异步 worker");
        let calls = native_calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "configure");
        assert_eq!(calls[1].0, "reuse");
        assert!(
            calls.iter().all(|call| call.2 == main_id),
            "测试必须穿过生产 on_main；直接在 worker 执行 native 会在这里失败"
        );
    }

    #[test]
    fn task11_fix_open_serializes_concurrent_creation_and_reuses_the_window() {
        #[derive(Default)]
        struct Registry {
            building: bool,
            window: Option<usize>,
            builds: usize,
        }

        let registry = Arc::new(std::sync::Mutex::new(Registry::default()));
        let (building, built) = mpsc::sync_channel(1);
        let (release, released) = mpsc::sync_channel(1);
        let first_registry = registry.clone();
        let first = thread::spawn(move || {
            open_with(
                || first_registry.lock().unwrap().window,
                || {
                    let mut registry = first_registry.lock().unwrap();
                    if registry.building {
                        return Err("duplicate label".to_owned());
                    }
                    registry.building = true;
                    registry.builds += 1;
                    drop(registry);
                    building.send(()).unwrap();
                    released.recv().unwrap();
                    let mut registry = first_registry.lock().unwrap();
                    registry.building = false;
                    registry.window = Some(1);
                    Ok(1)
                },
                |_| Ok(()),
            )
        });
        built.recv_timeout(Duration::from_secs(1)).unwrap();

        let second_registry = registry.clone();
        let (checked, second_checked) = mpsc::sync_channel(1);
        let second = thread::spawn(move || {
            open_with(
                || {
                    let window = second_registry.lock().unwrap().window;
                    let _ = checked.send(());
                    window
                },
                || {
                    let mut registry = second_registry.lock().unwrap();
                    if registry.building {
                        return Err("duplicate label".to_owned());
                    }
                    registry.builds += 1;
                    registry.window = Some(2);
                    Ok(2)
                },
                |_| Ok(()),
            )
        });
        let _ = second_checked.recv_timeout(Duration::from_millis(100));
        release.send(()).unwrap();
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();
        let registry = registry.lock().unwrap();
        assert_eq!(registry.builds, 1, "并发 open 只能创建一个 guide");
        assert_eq!(registry.window, Some(1));
    }

    #[test]
    fn task11_fix_close_race_and_main_native_errors_return_without_panic() {
        let (main, main_thread) = fake_main();
        let closed = open_with(
            || Some(9),
            || panic!("复用分支不能建窗"),
            |_| on_main(&main, || Err("guide 已关闭".to_owned())),
        );
        assert_eq!(closed, Err("guide 已关闭".to_owned()));
        let configure_failed = open_with(
            || None,
            || Ok(10),
            |_| on_main(&main, || Err("命中区域失败".to_owned())),
        );
        assert_eq!(configure_failed, Err("命中区域失败".to_owned()));
        drop(main);
        main_thread.join().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn task11_fix_initial_layout_uses_target_monitor_dpi_not_creation_monitor() {
        struct CreatedWindow {
            creation_scale: f64,
        }
        let window = CreatedWindow {
            creation_scale: 1.0,
        };
        let target = TargetMonitor {
            work: WorkArea {
                left: 1920,
                top: 0,
                right: 3840,
                bottom: 1080,
            },
            scale: 1.5,
        };
        configure_initial_with(
            &window,
            || Ok(target),
            |window, layout, regions, scale| {
                assert_eq!(window.creation_scale, 1.0, "创建屏为 100%");
                assert_eq!(scale, 1.5, "移动前必须选定指针所在 150% 目标屏");
                assert_eq!(
                    layout,
                    GuideLayout {
                        x: 3276,
                        y: 516,
                        width: 540,
                        height: 540,
                    }
                );
                let (hits, bounds) = platform::window::convert_hit_regions(regions, scale).unwrap();
                assert_eq!(
                    (hits[0].left, hits[0].top, hits[0].right, hits[0].bottom),
                    (18, 48, 522, 552)
                );
                assert_eq!(
                    bounds.map(|r| (r.left, r.top, r.right, r.bottom)),
                    Some((30, 60, 510, 540)),
                    "初始命中区域也必须在最终位置按目标 DPI 换算"
                );
                Ok(())
            },
        )
        .unwrap();
    }
}
