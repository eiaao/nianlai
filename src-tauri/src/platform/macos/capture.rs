use super::{apps, clipboard};
use crate::{
    capture::rules,
    history::{Captured, CopyEvent, History, content::Content},
    log::Logger,
    platform::Result,
};
use objc2_app_kit::NSPasteboard;
use serde_json::json;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

pub struct Service {
    stopped: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}
impl Service {
    pub fn start(
        logger: Logger,
        history: History,
        clipboard: &clipboard::Service,
        _: bool,
        changed: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self> {
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let clipboard = clipboard.clone();
        let thread = std::thread::Builder::new()
            .name("pasteboard-capture".into())
            .spawn(move || {
                let mut previous = NSPasteboard::generalPasteboard().changeCount();
                history.set_recording(true);
                history.notify_health_changed();
                while !stop.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(300));
                    let result = objc2::rc::autoreleasepool(|_| {
                        let _lock = clipboard.lock();
                        let board = NSPasteboard::generalPasteboard();
                        let count = board.changeCount();
                        if count == previous {
                            return Ok(None);
                        }
                        previous = count;
                        clipboard.changed(count);
                        if clipboard.is_own(count) {
                            return Ok(None);
                        }
                        capture(&board, count, &history, &logger).map(Some)
                    });
                    match result {
                        Ok(Some(recorded)) => {
                            history.set_recording(true);
                            history.notify_health_changed();
                            if recorded {
                                changed();
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            history.set_recording(false);
                            history.notify_health_changed();
                            logger.event("capture_read_failed", json!({"error":error}));
                        }
                    }
                }
                history.set_recording(false);
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            stopped,
            thread: Mutex::new(Some(thread)),
        })
    }
    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(thread) = self.thread.lock().unwrap().take() {
            let _ = thread.join();
        }
    }
}
fn capture(board: &NSPasteboard, count: isize, history: &History, logger: &Logger) -> Result<bool> {
    let source = apps::frontmost();
    let (bundle, name) = source
        .as_ref()
        .map(|source| (source.bundle.as_str(), source.name.as_str()))
        .unwrap_or(("", ""));
    let types = board.types().ok_or("clipboard_read_denied")?;
    let names: Vec<_> = types.iter().map(|value| value.to_string()).collect();
    let blocked = rules::gate()
        .before_read(bundle, name)
        .or_else(|| crate::capture::blocking_marker(names.iter().map(String::as_str), |_| None));
    if let Some(reason) = blocked {
        history.skipped_sequence(count as u32);
        logger.event("capture_skipped", json!({"reason":reason}));
        return Ok(false);
    }
    let mut data = clipboard::read(board)?;
    if data.is_empty() {
        return Ok(false);
    }
    if let Err(skip) = rules::gate().after_read(&mut data) {
        history.skipped_sequence(count as u32);
        logger.event(
            "capture_skipped",
            json!({"reason":skip.reason,"rule":skip.rule}),
        );
        return Ok(false);
    }
    let content = Content::new(data, super::hash::sha256)?;
    let now = Instant::now();
    let captured = history.capture(
        content,
        CopyEvent {
            time_ms: crate::log::now_ms(),
            sequence: count as u32,
            source_process: bundle.to_owned(),
            source_path: bundle.to_owned(),
            owner_process: name.to_owned(),
            owner_pid: source.as_ref().map_or(0, |source| source.pid),
            observed: now,
        },
        false,
        now,
    )?;
    let recorded = matches!(captured, Captured::Stored(result) if result.record_id.is_some());
    if recorded {
        history.refresh_source_app(bundle);
    }
    Ok(recorded)
}
