mod reader;

#[cfg(test)]
mod fake_password_manager_tests;

use super::clipboard;
use crate::{
    capture::SelfCheck,
    history::{History, older},
    log::{Logger, now_ms},
    paste::capture_access::CaptureAccess,
    platform::Result,
};
use serde::Serialize;
use serde_json::json;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        System::{
            DataExchange::*, LibraryLoader::GetModuleHandleW, RemoteDesktop::*,
            Threading::GetCurrentThreadId,
        },
        UI::{
            Input::KeyboardAndMouse::{
                MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey, UnregisterHotKey,
            },
            WindowsAndMessaging::*,
        },
    },
    core::w,
};

const CHECK: u32 = WM_APP + 20;
const STOP: u32 = WM_APP + 21;
const DEBUG_HOTKEY: i32 = 33;

#[derive(Clone)]
struct Notice {
    sequence: u32,
    at: Instant,
    time_ms: u64,
    source: u32,
    owner: u32,
    trigger: &'static str,
}

fn process(hwnd: HWND) -> u32 {
    let mut id = 0;
    if !hwnd.is_invalid() {
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut id));
        }
    }
    id
}

fn notice(trigger: &'static str) -> Notice {
    Notice {
        sequence: unsafe { GetClipboardSequenceNumber() },
        at: Instant::now(),
        time_ms: now_ms(),
        source: process(unsafe { GetForegroundWindow() }),
        owner: process(unsafe { GetClipboardOwner() }.unwrap_or_default()),
        trigger,
    }
}

#[derive(Default)]
struct Inbox {
    latest: Mutex<Option<Notice>>,
    ready: Condvar,
}
impl Inbox {
    fn push(&self, notice: Notice) {
        let mut latest = self.latest.lock().unwrap();
        if latest.as_ref().is_none_or(|old| {
            old.sequence != notice.sequence && !older(notice.sequence, old.sequence)
        }) {
            *latest = Some(notice);
        }
        self.ready.notify_one();
    }
    fn next(&self, stop: &AtomicBool) -> Option<Notice> {
        let mut latest = self.latest.lock().unwrap();
        while latest.is_none() && !stop.load(Ordering::Acquire) {
            latest = self.ready.wait(latest).unwrap();
        }
        if stop.load(Ordering::Acquire) {
            None
        } else {
            latest.take()
        }
    }
    fn matching(&self, sequence: u32) -> Option<Notice> {
        let mut latest = self.latest.lock().unwrap();
        if latest
            .as_ref()
            .is_some_and(|notice| notice.sequence == sequence)
        {
            latest.take()
        } else {
            None
        }
    }
}

#[derive(Default, Serialize)]
struct Counters {
    notifications: u64,
    reads: u64,
    outcomes: BTreeMap<&'static str, u64>,
    skip_reasons: BTreeMap<&'static str, u64>,
    self_check_failures: u64,
    rearm_successes: u64,
    rearm_failures: u64,
    #[serde(skip)]
    latencies: BTreeMap<u64, u64>,
}
impl Counters {
    fn p95(&self) -> Option<u64> {
        let count: u64 = self.latencies.values().sum();
        if count == 0 {
            return None;
        }
        let threshold = (count * 95).div_ceil(100);
        let mut seen = 0;
        self.latencies.iter().find_map(|(millis, count)| {
            seen += count;
            (seen >= threshold).then_some(*millis)
        })
    }
}

struct Shared {
    inbox: Inbox,
    stopping: AtomicBool,
    history: History,
    own_owner: usize,
    capture_access: Arc<CaptureAccess>,
    logger: Logger,
    counters: Mutex<Counters>,
    changed: Arc<dyn Fn() + Send + Sync>,
}
impl Shared {
    fn stop(&self) {
        // 和等待者使用同一把锁，避免停止通知落在检查条件与进入 wait 之间。
        let _pending = self.inbox.latest.lock().unwrap();
        self.stopping.store(true, Ordering::Release);
        self.inbox.ready.notify_all();
    }
}

struct Context {
    shared: Arc<Shared>,
    check: SelfCheck,
    last_system_event: Option<Instant>,
    taskbar_message: u32,
    baseline: u32,
    has_changed: bool,
}
thread_local! { static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) }; }

unsafe extern "system" fn procedure(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if message == WM_TIMER {
        unsafe {
            let _ = PostThreadMessageW(GetCurrentThreadId(), CHECK, WPARAM(0), LPARAM(0));
        }
        return LRESULT(0);
    }
    let handled = CONTEXT.with(|context| {
        let mut context = context.borrow_mut();
        let Some(context) = context.as_mut() else {
            return false;
        };
        if message == WM_CLIPBOARDUPDATE {
            if !context.shared.stopping.load(Ordering::Acquire) {
                let notice = notice("notification");
                context.check.notified(notice.sequence);
                context.shared.counters.lock().unwrap().notifications += 1;
                context.has_changed |= notice.sequence != context.baseline;
                if context.has_changed {
                    context.shared.inbox.push(notice);
                }
            }
            return true;
        }
        // 设置与 DPI 变化只旁路请求托盘刷新；不解指针、不消费消息，原会话与系统路径继续。
        let _ = crate::tray::window_message(message, lp);
        let event = if message == WM_POWERBROADCAST {
            match wp.0 as u32 {
                4 => Some("suspend"),
                7 => Some("resume_suspend"),
                18 => Some("resume_automatic"),
                _ => None,
            }
        } else if message == WM_WTSSESSION_CHANGE {
            match wp.0 as u32 {
                1 => Some("console_connect"),
                2 => Some("console_disconnect"),
                3 => Some("remote_connect"),
                4 => Some("remote_disconnect"),
                5 => Some("session_logon"),
                6 => Some("session_logoff"),
                7 => Some("session_lock"),
                8 => Some("session_unlock"),
                9 => Some("session_remote_control"),
                _ => None,
            }
        } else if message == context.taskbar_message {
            Some("taskbar_created")
        } else {
            None
        };
        if let Some(event) = event {
            context.last_system_event = Some(Instant::now());
            context
                .shared
                .logger
                .event("system_event", json!({"kind":event}));
            return true;
        }
        false
    });
    if handled {
        LRESULT(0)
    } else {
        unsafe { DefWindowProcW(hwnd, message, wp, lp) }
    }
}

fn create_window(instance: HINSTANCE) -> Result<HWND> {
    unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("NianlaiG0Capture"),
            w!(""),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        )
        .map_err(|_| "capture_window_failed".into())
    }
}

fn register_debug(hwnd: HWND, logger: &Logger) {
    let success = unsafe {
        RegisterHotKey(
            Some(hwnd),
            DEBUG_HOTKEY,
            MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_NOREPEAT,
            0x4c,
        )
    }
    .is_ok();
    logger.event("capture_debug_hotkey", json!({"success":success}));
}

fn configure_window(hwnd: HWND, automatic: bool, logger: &Logger) -> Result<()> {
    unsafe {
        if SetTimer(Some(hwnd), 1, 1000, None) == 0 {
            return Err("capture_timer_failed".into());
        }
        let sessions = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION).is_ok();
        logger.event("capture_session_notifications", json!({"success":sessions}));
        if !automatic {
            register_debug(hwnd, logger);
        }
    }
    Ok(())
}

fn destroy_window(hwnd: HWND) {
    unsafe {
        let _ = RemoveClipboardFormatListener(hwnd);
        let _ = WTSUnRegisterSessionNotification(hwnd);
        let _ = KillTimer(Some(hwnd), 1);
        let _ = UnregisterHotKey(Some(hwnd), DEBUG_HOTKEY);
        let _ = DestroyWindow(hwnd);
    }
}

fn listener(shared: Arc<Shared>, automatic: bool, ready: mpsc::SyncSender<Result<u32>>) {
    let baseline = unsafe { GetClipboardSequenceNumber() };
    let setup = (|| -> Result<(HWND, HINSTANCE)> {
        let instance = HINSTANCE(
            unsafe { GetModuleHandleW(None) }
                .map_err(|_| "capture_module_failed")?
                .0,
        );
        let class = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            hInstance: instance,
            lpszClassName: w!("NianlaiG0Capture"),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&class) } == 0 {
            return Err("capture_class_failed".into());
        }
        let taskbar_message = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        if taskbar_message == 0 {
            return Err("capture_taskbar_message_failed".into());
        }
        CONTEXT.with(|context| {
            *context.borrow_mut() = Some(Context {
                shared: shared.clone(),
                check: SelfCheck::new(baseline),
                last_system_event: None,
                taskbar_message,
                baseline,
                has_changed: false,
            })
        });
        let hwnd = create_window(instance)?;
        if let Err(error) = configure_window(hwnd, automatic, &shared.logger).and_then(|()| {
            unsafe { AddClipboardFormatListener(hwnd) }.map_err(|_| "capture_listen_failed".into())
        }) {
            destroy_window(hwnd);
            return Err(error);
        }
        Ok((hwnd, instance))
    })();
    let (mut hwnd, instance) = match setup {
        Ok(value) => value,
        Err(error) => {
            let _ = ready.send(Err(error));
            CONTEXT.with(|context| context.borrow_mut().take());
            return;
        }
    };
    shared.history.set_recording(true);
    shared.logger.event("capture_listener_started", json!({"listener_attached":true,"top_level":true,"visible":false,"baseline_sequence":baseline}));
    (shared.changed)();
    let _ = ready.send(Ok(unsafe { GetCurrentThreadId() }));
    let mut message = MSG::default();
    while !shared.stopping.load(Ordering::Acquire) {
        let result = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
        if result <= 0 || message.message == STOP {
            break;
        }
        if message.message == CHECK {
            let current = unsafe { GetClipboardSequenceNumber() };
            let processed = shared.history.latest_sequence();
            let failed = CONTEXT.with(|context| {
                context
                    .borrow_mut()
                    .as_mut()
                    .unwrap()
                    .check
                    .check(current, processed)
            });
            if failed {
                let started = Instant::now();
                let since_system = CONTEXT.with(|context| {
                    context
                        .borrow()
                        .as_ref()
                        .unwrap()
                        .last_system_event
                        .map(|at| at.elapsed().as_secs_f64() * 1000.0)
                });
                let since_write = shared
                    .history
                    .last_write()
                    .map(|at| at.elapsed().as_secs_f64() * 1000.0);
                shared.counters.lock().unwrap().self_check_failures += 1;
                shared.logger.event("capture_self_check_failed", json!({"sequence":current,"since_system_event_ms":since_system,"since_own_write_ms":since_write}));
                unsafe {
                    let _ = RemoveClipboardFormatListener(hwnd);
                }
                let mut success = unsafe { AddClipboardFormatListener(hwnd) }.is_ok();
                let mut rebuilt = false;
                if !success {
                    // 旧窗口保留到新窗口连同定时器都准备好，重建失败后仍能继续自检。
                    if let Ok(replacement) = create_window(instance) {
                        if configure_window(replacement, true, &shared.logger).is_ok() {
                            destroy_window(hwnd);
                            hwnd = replacement;
                            rebuilt = true;
                            if !automatic {
                                register_debug(hwnd, &shared.logger);
                            }
                            success = unsafe { AddClipboardFormatListener(hwnd) }.is_ok();
                        } else {
                            destroy_window(replacement);
                        }
                    }
                }
                finish_rearm(&shared, success, rebuilt, started);
                if success {
                    let catch_up = notice("rearmed");
                    CONTEXT.with(|context| {
                        let mut context = context.borrow_mut();
                        let context = context.as_mut().unwrap();
                        context.check.notified(catch_up.sequence);
                        context.has_changed = true;
                    });
                    shared.inbox.push(catch_up);
                }
            }
        } else if message.message == WM_HOTKEY
            && message.wParam.0 == DEBUG_HOTKEY as usize
            && !automatic
        {
            let success = unsafe { RemoveClipboardFormatListener(hwnd) }.is_ok();
            shared.logger.event(
                "capture_listener_dropped_for_test",
                json!({"success":success}),
            );
        } else {
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
    destroy_window(hwnd);
    CONTEXT.with(|context| context.borrow_mut().take());
    shared.history.set_recording(false);
    if !shared.stopping.load(Ordering::Acquire) {
        shared.logger.event(
            "capture_listener_failed",
            json!({"reason":"message_loop_ended"}),
        );
        (shared.changed)();
    }
    shared.logger.event("capture_listener_stopped", json!({}));
}

fn finish_rearm(shared: &Shared, success: bool, rebuilt: bool, started: Instant) {
    shared.history.set_recording(success);
    {
        let mut counts = shared.counters.lock().unwrap();
        if success {
            counts.rearm_successes += 1;
        } else {
            counts.rearm_failures += 1;
        }
    }
    shared.logger.event("capture_listener_rearmed", json!({"success":success,"rebuilt_window":rebuilt,"elapsed_ms":started.elapsed().as_secs_f64()*1000.0}));
    (shared.changed)();
}

fn reader_main(shared: Arc<Shared>, done: mpsc::Sender<()>, run: impl FnOnce(&Shared)) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&shared)));
    if result.is_err() {
        shared
            .logger
            .event("capture_reader_failed", json!({"reason":"thread_panicked"}));
    }
    // 无论是意外返回还是 panic，读取线程消失都等于记录失效；正常停止不报故障。
    if !shared.stopping.load(Ordering::Acquire) {
        if result.is_ok() {
            shared
                .logger
                .event("capture_reader_failed", json!({"reason":"thread_ended"}));
        }
        shared.history.fail_capture_reader();
        shared.history.notify_health_changed();
    }
    let _ = done.send(());
}

struct Threads {
    listener: JoinHandle<()>,
    reader: JoinHandle<()>,
    reader_done: mpsc::Receiver<()>,
    listener_id: u32,
}
pub struct Service {
    shared: Arc<Shared>,
    threads: Mutex<Option<Threads>>,
}

impl Service {
    pub fn start(
        logger: Logger,
        history: History,
        clipboard: &clipboard::Service,
        automatic: bool,
        changed: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self> {
        let shared = Arc::new(Shared {
            inbox: Inbox::default(),
            stopping: AtomicBool::new(false),
            history,
            own_owner: clipboard.owner(),
            capture_access: clipboard.capture_access(),
            logger,
            counters: Mutex::new(Counters::default()),
            changed,
        });
        let (done, reader_done) = mpsc::channel();
        let read_shared = shared.clone();
        let reader = thread::Builder::new()
            .name("capture-reader".into())
            .spawn(move || reader_main(read_shared, done, reader::run))
            .map_err(|_| "capture_reader_start_failed")?;
        let listen_shared = shared.clone();
        let listener_failure = shared.clone();
        let (ready, started) = mpsc::sync_channel(1);
        let listener = match thread::Builder::new()
            .name("capture-listener".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    listener(listen_shared, automatic, ready);
                }));
                if result.is_err() {
                    listener_failure.logger.event(
                        "capture_listener_failed",
                        json!({"reason":"thread_panicked"}),
                    );
                    listener_failure.history.set_recording(false);
                    listener_failure.history.notify_health_changed();
                }
            }) {
            Ok(thread) => thread,
            Err(_) => {
                shared.stop();
                let _ = reader.join();
                return Err("capture_listener_start_failed".into());
            }
        };
        match started
            .recv()
            .map_err(|_| "capture_listener_start_failed".into())
            .and_then(|value| value)
        {
            Ok(listener_id) => Ok(Self {
                shared,
                threads: Mutex::new(Some(Threads {
                    listener,
                    reader,
                    reader_done,
                    listener_id,
                })),
            }),
            Err(error) => {
                shared.stop();
                let _ = listener.join();
                let _ = reader.join();
                Err(error)
            }
        }
    }

    pub fn shutdown(&self) {
        let Some(threads) = self.threads.lock().unwrap().take() else {
            return;
        };
        self.shared.stop();
        unsafe {
            let _ = PostThreadMessageW(threads.listener_id, STOP, WPARAM(0), LPARAM(0));
        }
        let wait = Instant::now();
        let _ = threads.listener.join();
        let finished = threads
            .reader_done
            .recv_timeout(Duration::from_secs(1).saturating_sub(wait.elapsed()))
            .is_ok();
        if finished {
            let _ = threads.reader.join();
        }
        self.shared.logger.event("capture_reader_stopped", json!({"finished":finished,"wait_ms":wait.elapsed().as_secs_f64()*1000.0,"reason":if finished {"stopped"} else {"read_wait_timeout"}}));
        let counts = self.shared.counters.lock().unwrap();
        let mut summary = serde_json::to_value(&*counts).unwrap_or_default();
        summary["latency_p95_ms"] = json!(counts.p95());
        summary["reader_finished"] = json!(finished);
        self.shared.logger.event("capture_summary", summary);
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared_for_reader_fault(directory: &crate::settings::tests::Directory) -> Arc<Shared> {
        let directories = crate::settings::Directories {
            settings_file: directory.0.join("settings.json"),
            data: directory.0.join("data"),
            logs: directory.0.join("logs"),
            data_source: "测试",
            test_data_dir: false,
        };
        let logger = Logger::with_settings(&directories, 20).unwrap();
        let history = History::new(nianlai_store::hash::sha256, logger.clone());
        history.set_recording(true);
        Arc::new(Shared {
            inbox: Inbox::default(),
            stopping: AtomicBool::new(false),
            history,
            own_owner: 0,
            capture_access: Arc::new(CaptureAccess::default()),
            logger,
            counters: Mutex::new(Counters::default()),
            changed: Arc::new(|| {}),
        })
    }

    #[test]
    fn task2_fix_reader_panic_then_rearm_success_stays_failed() {
        let directory = crate::settings::tests::Directory::new();
        let shared = shared_for_reader_fault(&directory);
        let reader_shared = shared.clone();
        let (done, reader_done) = mpsc::channel();
        let reader = thread::spawn(move || {
            reader_main(reader_shared, done, |_| panic!("注入读取线程故障"));
        });
        reader_done.recv_timeout(Duration::from_secs(2)).unwrap();
        reader.join().unwrap();
        assert_eq!(
            shared.history.capture_health(),
            crate::health::CaptureHealth::Failed
        );
        // 走生产重挂完成路径，不是手写健康 setter 序列。
        finish_rearm(&shared, true, false, Instant::now());
        assert_eq!(
            shared.history.capture_health(),
            crate::health::CaptureHealth::Failed
        );
    }

    #[test]
    fn task2_fix_reader_panic_and_rearm_race_stays_failed() {
        let directory = crate::settings::tests::Directory::new();
        let shared = shared_for_reader_fault(&directory);
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let reader_shared = shared.clone();
        let reader_barrier = barrier.clone();
        let (done, reader_done) = mpsc::channel();
        let reader = thread::spawn(move || {
            reader_main(reader_shared, done, move |_| {
                reader_barrier.wait();
                panic!("注入并发读取线程故障");
            });
        });
        let rearm_shared = shared.clone();
        let rearm = thread::spawn(move || {
            barrier.wait();
            finish_rearm(&rearm_shared, true, false, Instant::now());
        });
        reader_done.recv_timeout(Duration::from_secs(2)).unwrap();
        reader.join().unwrap();
        rearm.join().unwrap();
        assert_eq!(
            shared.history.capture_health(),
            crate::health::CaptureHealth::Failed
        );
    }

    #[test]
    fn busy_reader_keeps_latest_without_a_time_debounce() {
        let inbox = Inbox::default();
        let stopping = AtomicBool::new(false);
        let at = Instant::now();
        let sample = |sequence, at| Notice {
            sequence,
            at,
            time_ms: 0,
            source: 1,
            owner: 2,
            trigger: "notification",
        };
        for sequence in 1..=5 {
            inbox.push(sample(sequence, at));
            assert_eq!(inbox.next(&stopping).unwrap().sequence, sequence);
        }
        inbox.push(sample(6, at));
        inbox.push(sample(7, at));
        inbox.push(sample(7, at + Duration::from_millis(20)));
        inbox.push(sample(6, at));
        let latest = inbox.next(&stopping).unwrap();
        assert_eq!(latest.sequence, 7);
        assert_eq!(latest.at, at);
    }

    #[test]
    fn notifications_during_native_access_keep_latest_for_the_waiting_reader() {
        use crate::paste::capture_access::{CaptureAttempt, OwnerSnapshot, capture_attempt};
        use std::sync::atomic::AtomicU32;
        let access = Arc::new(CaptureAccess::default());
        let native = access.native(Duration::from_secs(2), || false).unwrap();
        let inbox = Arc::new(Inbox::default());
        let sequence = Arc::new(AtomicU32::new(5));
        let sample = |sequence| Notice {
            sequence,
            at: Instant::now(),
            time_ms: 0,
            source: 1,
            owner: 2,
            trigger: "notification",
        };
        inbox.push(sample(5));
        let reader_inbox = inbox.clone();
        let reader_sequence = sequence.clone();
        let (waiting, ready) = mpsc::channel();
        let reader = thread::spawn(move || {
            let stop = AtomicBool::new(false);
            assert_eq!(reader_inbox.next(&stop).unwrap().sequence, 5);
            waiting.send(()).unwrap();
            let read = capture_attempt(
                &access,
                &stop,
                3,
                || {
                    let sequence = reader_sequence.load(Ordering::Acquire);
                    OwnerSnapshot {
                        sequence_before: sequence,
                        owner: 2,
                        sequence_after: sequence,
                    }
                },
                || Ok::<_, ()>(()),
            )
            .unwrap();
            assert!(matches!(read, CaptureAttempt::Opened(_)));
            assert_eq!(reader_inbox.matching(7).unwrap().sequence, 7);
            drop(read);
        });
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        inbox.push(sample(6));
        inbox.push(sample(7));
        inbox.push(sample(6));
        sequence.store(7, Ordering::Release);
        drop(native);
        reader.join().unwrap();
        assert!(inbox.latest.lock().unwrap().is_none());
    }
}
