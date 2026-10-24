use crate::{log::Logger, platform::Result};
use serde_json::json;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tauri::WebviewWindow;
use webview2_com::{Microsoft::Web::WebView2::Win32::*, TrySuspendCompletedHandler};
use windows::core::Interface;

#[derive(Default)]
pub struct MemoryState {
    generation: AtomicU64,
    visible: AtomicBool,
    // 最近一次请求挂起的时刻，供 NIANLAI_G0_RESUSPEND_MIN 判断是否到期；显示时清空。
    suspended_at: Mutex<Option<Instant>>,
}

fn memory_target_level(core: &ICoreWebView2) -> String {
    let result = core.cast::<ICoreWebView2_19>().and_then(|memory| {
        let mut level = COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL;
        unsafe {
            memory.MemoryUsageTargetLevel(&mut level)?;
        }
        Ok(level)
    });
    match result {
        Ok(COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW) => "low".into(),
        Ok(COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL) => "normal".into(),
        Ok(level) => format!("未知内存目标：{}", level.0),
        Err(error) => format!("读取失败：{error}"),
    }
}

fn complete(ready: &Option<mpsc::Sender<Result<Instant>>>, result: Result<()>) {
    if let Some(ready) = ready {
        let _ = ready.send(result.map(|()| Instant::now()));
    }
}

pub fn set_visible(
    panel: &WebviewWindow,
    visible: bool,
    logger: Logger,
    session: u64,
    state: Arc<MemoryState>,
) -> Result<()> {
    set_visible_with_completion(panel, visible, logger, session, state, None)
}

pub fn set_visible_with_completion(
    panel: &WebviewWindow,
    visible: bool,
    logger: Logger,
    session: u64,
    state: Arc<MemoryState>,
    ready: Option<mpsc::Sender<Result<Instant>>>,
) -> Result<()> {
    state.visible.store(visible, Ordering::SeqCst);
    *state.suspended_at.lock().unwrap() = (!visible).then(Instant::now);
    let generation = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
    panel.with_webview(move |webview| unsafe {
        if state.generation.load(Ordering::SeqCst) != generation {
            complete(&ready, Err("可见性请求已过期".into()));
            return;
        }
        let controller = webview.controller();
        let outcome = (|| -> windows::core::Result<()> {
            controller.SetIsVisible(visible)?;
            let core = controller.CoreWebView2()?;
            if logger.tier == 1 {
                logger.event("webview_visibility", json!({"session":session,"visible":visible,"suspend_result":"tier_1_skipped"}));
                complete(&ready, Ok(()));
                return Ok(());
            }
            let suspend = core.cast::<ICoreWebView2_3>()?;
            if visible {
                suspend.Resume()?;
                logger.event("webview_resumed", json!({"session":session,"visible":true,"memory_target_level":memory_target_level(&core)}));
                complete(&ready, Ok(()));
            } else {
                let callback_log = logger.clone();
                let callback_core = core.clone();
                let callback_suspend = suspend.clone();
                let callback_controller = controller.clone();
                let callback_ready = ready.clone();
                let callback = TrySuspendCompletedHandler::create(Box::new(move |result, success| {
                    let stale = state.generation.load(Ordering::SeqCst) != generation;
                    let suspended = result.is_ok() && success;
                    let error = result.err().map(|error| error.to_string());
                    callback_log.event("webview_suspended", json!({
                        "session":session,"suspend_result":suspended,"stale":stale,"error":error,
                        "memory_target_level":memory_target_level(&callback_core),
                    }));
                    // 挂起失败已记在上面的事件里，计时照常继续，避免一次失败作废整轮测试。
                    complete(&callback_ready, if stale {
                        Err("挂起请求已过期".into())
                    } else {
                        Ok(())
                    });
                    // 旧挂起请求可能在重新显示后完成，此时必须立即恢复。
                    if stale && state.visible.load(Ordering::SeqCst) {
                        callback_controller.SetIsVisible(true)?;
                        callback_suspend.Resume()?;
                        callback_log.event("webview_resumed", json!({
                            "session":session,"visible":true,"stale":true,
                            "memory_target_level":memory_target_level(&callback_core),
                        }));
                    }
                    Ok(())
                }));
                suspend.TrySuspend(&callback)?;
            }
            Ok(())
        })();
        if let Err(error) = outcome {
            logger.event("webview_failed", json!({"session":session,"visible":visible,"suspend_result":false,"error":error.to_string()}));
            complete(&ready, Err(error.to_string()));
        }
    }).map_err(|e|e.to_string())
}

// 到期 = 面板不可见，且距最近一次挂起请求已超过 interval。
pub fn due(state: &MemoryState, interval: Duration) -> bool {
    !state.visible.load(Ordering::SeqCst)
        && state
            .suspended_at
            .lock()
            .unwrap()
            .is_some_and(|at| at.elapsed() >= interval)
}

pub fn generation(state: &MemoryState) -> u64 {
    state.generation.load(Ordering::SeqCst)
}

// 隐藏期间先 Resume 再立刻 TrySuspend，验证能否让 WebView2 重新压缩内存。
// 仅由面板状态机在 UI 线程调用，with_webview 在此线程同步执行；到期执行并返回请求号，否则返回 None。
pub fn resuspend(
    panel: &WebviewWindow,
    logger: Logger,
    session: u64,
    state: Arc<MemoryState>,
    interval: Duration,
) -> Result<Option<u64>> {
    let hidden_seconds = {
        let mut suspended_at = state.suspended_at.lock().unwrap();
        if state.visible.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let Some(hidden_since) = *suspended_at else {
            return Ok(None);
        };
        if hidden_since.elapsed() < interval {
            return Ok(None);
        }
        *suspended_at = Some(Instant::now());
        hidden_since.elapsed().as_secs_f64()
    };
    let generation = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
    panel
        .with_webview(move |webview| unsafe {
            if state.generation.load(Ordering::SeqCst) != generation {
                return;
            }
            let controller = webview.controller();
            let before = super::memory::sample();
            let outcome = (|| -> windows::core::Result<()> {
                let core = controller.CoreWebView2()?;
                let suspend = core.cast::<ICoreWebView2_3>()?;
                let resume_started = Instant::now();
                suspend.Resume()?;
                let resume_ms = resume_started.elapsed().as_secs_f64() * 1000.0;
                let suspend_started = Instant::now();
                let callback_log = logger.clone();
                let callback_core = core.clone();
                let callback_suspend = suspend.clone();
                let callback_controller = controller.clone();
                let callback = TrySuspendCompletedHandler::create(Box::new(move |result, success| {
                    let suspend_ms = suspend_started.elapsed().as_secs_f64() * 1000.0;
                    let stale = state.generation.load(Ordering::SeqCst) != generation;
                    let suspended = result.is_ok() && success;
                    let error = result.err().map(|error| error.to_string());
                    callback_log.event("webview_resuspended", json!({
                        "session":session,"generation":generation,"suspend_result":suspended,"stale":stale,"error":error,
                        "memory_target_level":memory_target_level(&callback_core),
                        "hidden_seconds":hidden_seconds,"resume_ms":resume_ms,"suspend_ms":suspend_ms,
                        "before":before,"after":super::memory::sample(),
                    }));
                    // 旧挂起请求可能在重新显示后完成，此时必须立即恢复。
                    if stale && state.visible.load(Ordering::SeqCst) {
                        callback_controller.SetIsVisible(true)?;
                        callback_suspend.Resume()?;
                        callback_log.event("webview_resumed", json!({
                            "session":session,"visible":true,"stale":true,
                            "memory_target_level":memory_target_level(&callback_core),
                        }));
                    }
                    Ok(())
                }));
                suspend.TrySuspend(&callback)?;
                Ok(())
            })();
            if let Err(error) = outcome {
                logger.event("webview_resuspend_failed", json!({
                    "session":session,"generation":generation,"error":error.to_string(),
                }));
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(Some(generation))
}

// 仅由面板状态机在 UI 线程调用，with_webview 在此线程同步执行。
pub fn focus(panel: &WebviewWindow) -> Result<()> {
    let (sender, receiver) = mpsc::sync_channel(1);
    panel
        .with_webview(move |webview| {
            let result = unsafe {
                webview
                    .controller()
                    .MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC)
            }
            .map_err(|e| e.to_string());
            let _ = sender.send(result);
        })
        .map_err(|e| e.to_string())?;
    receiver.try_recv().map_err(|e| e.to_string())?
}

pub fn initialize(panel: &WebviewWindow) -> Result<()> {
    let (sender, receiver) = mpsc::sync_channel(1);
    panel
        .with_webview(move |webview| unsafe {
            let result = (|| -> windows::core::Result<()> {
                let settings = webview.controller().CoreWebView2()?.Settings()?;
                settings.SetAreDefaultContextMenusEnabled(false)?;
                settings
                    .cast::<ICoreWebView2Settings3>()?
                    .SetAreBrowserAcceleratorKeysEnabled(false)?;
                Ok(())
            })()
            .map_err(|e| e.to_string());
            let _ = sender.send(result);
        })
        .map_err(|e| e.to_string())?;
    receiver.try_recv().map_err(|e| e.to_string())?
}
