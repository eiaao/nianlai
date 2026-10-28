use super::*;
use crate::paste::native::{self, Backend, Completion, Submission};
use std::{
    sync::{
        atomic::{AtomicU32, AtomicUsize},
        mpsc::{self, Receiver, Sender},
    },
    thread,
};

const WAIT: Duration = Duration::from_secs(2);
type Trace = Arc<Mutex<Vec<&'static str>>>;

fn recv<T>(receiver: &Receiver<T>) -> T {
    receiver.recv_timeout(WAIT).expect("线程未到达预期边界")
}

fn snapshot() -> OwnerSnapshot {
    OwnerSnapshot {
        sequence_before: 1,
        owner: 2,
        sequence_after: 1,
    }
}

struct Resource(Trace);
impl Drop for Resource {
    fn drop(&mut self) {
        self.0.lock().unwrap().push("close");
    }
}

struct Script {
    access: Option<AccessGuard>,
    gate: Arc<CaptureAccess>,
    trace: Trace,
    check: Box<dyn FnMut() -> Result<(), &'static str> + Send>,
    writing: Option<(Sender<()>, Receiver<()>)>,
    submission: Submission,
    write_error: bool,
}
impl Script {
    fn new(access: AccessGuard, gate: Arc<CaptureAccess>, trace: Trace) -> Self {
        Self {
            access: Some(access),
            gate,
            trace,
            check: Box::new(|| Ok(())),
            writing: None,
            submission: Submission::Sent,
            write_error: false,
        }
    }
}
impl Backend for Script {
    fn synchronize(&mut self) -> bool {
        true
    }
    fn check(&mut self) -> Result<(), &'static str> {
        (self.check)()
    }
    fn write(&mut self) -> Result<(), String> {
        self.trace.lock().unwrap().push("write");
        if let Some((started, proceed)) = self.writing.take() {
            started.send(()).unwrap();
            recv(&proceed);
        }
        if self.write_error {
            Err("write_failed".into())
        } else {
            Ok(())
        }
    }
    fn prepare_submit(&mut self) -> Result<(), &'static str> {
        Ok(())
    }
    fn submit(&mut self) -> Result<Submission, &'static str> {
        self.trace.lock().unwrap().push("submit");
        Ok(self.submission)
    }
    fn finish(&mut self, result: Option<Completion>) {
        drop(self.access.take());
        // 清理/UI阶段已能由reader重新获得访问权。
        let _reader = self.gate.native(WAIT, || false).unwrap();
        if matches!(result, Some(Completion::Submitted(_))) {
            self.trace.lock().unwrap().push("restore");
        }
    }
}

#[test]
fn opened_reader_closes_before_waiting_native_can_write_or_submit() {
    let gate = Arc::new(CaptureAccess::default());
    let trace = Trace::default();
    let read = capture_attempt(&gate, &AtomicBool::new(false), 3, snapshot, || {
        Ok::<_, ()>(Resource(trace.clone()))
    })
    .unwrap();
    let (waiting, started) = mpsc::channel();
    let native_gate = gate.clone();
    let native_trace = trace.clone();
    let native = thread::spawn(move || {
        let calls = AtomicUsize::new(0);
        let access = native_gate
            .native(WAIT, || {
                if calls.fetch_add(1, Ordering::Relaxed) == 1 {
                    waiting.send(()).unwrap();
                }
                false
            })
            .unwrap();
        native::run(&mut Script::new(access, native_gate, native_trace)).unwrap()
    });
    recv(&started);
    assert!(trace.lock().unwrap().is_empty());
    drop(read);
    assert_eq!(
        native.join().unwrap(),
        Completion::Submitted(Submission::Sent)
    );
    assert_eq!(
        *trace.lock().unwrap(),
        ["close", "write", "submit", "restore"]
    );
}

#[test]
fn native_holds_reader_out_from_write_through_the_single_submission() {
    let gate = Arc::new(CaptureAccess::default());
    let trace = Trace::default();
    let access = gate.native(WAIT, || false).unwrap();
    let (writing, started) = mpsc::channel();
    let (proceed, continuation) = mpsc::channel();
    let mut script = Script::new(access, gate.clone(), trace.clone());
    script.writing = Some((writing, continuation));
    let native = thread::spawn(move || native::run(&mut script).unwrap());
    recv(&started);
    let (reader_started, reader_ready) = mpsc::channel();
    let (opened, opened_rx) = mpsc::channel();
    let reader_trace = trace.clone();
    let reader = thread::spawn(move || {
        reader_started.send(()).unwrap();
        let read = capture_attempt(&gate, &AtomicBool::new(false), 3, snapshot, || {
            reader_trace.lock().unwrap().push("open");
            opened.send(()).unwrap();
            Ok::<_, ()>(Resource(reader_trace))
        })
        .unwrap();
        drop(read);
    });
    recv(&reader_ready);
    assert!(opened_rx.try_recv().is_err());
    proceed.send(()).unwrap();
    assert_eq!(
        native.join().unwrap(),
        Completion::Submitted(Submission::Sent)
    );
    recv(&opened_rx);
    reader.join().unwrap();
    let events = trace.lock().unwrap();
    assert!(
        events.iter().position(|event| *event == "submit").unwrap()
            < events.iter().position(|event| *event == "open").unwrap()
    );
    assert_eq!(events.iter().filter(|event| **event == "submit").count(), 1);
}

#[test]
fn queued_old_notice_rechecks_current_owner_after_native_release_without_opening() {
    let gate = Arc::new(CaptureAccess::default());
    let access = gate.native(WAIT, || false).unwrap();
    let owner = Arc::new(AtomicUsize::new(2));
    let sequence = Arc::new(AtomicU32::new(10));
    let opens = Arc::new(AtomicUsize::new(0));
    let (started, ready) = mpsc::channel();
    let reader_owner = owner.clone();
    let reader_sequence = sequence.clone();
    let reader_opens = opens.clone();
    let reader = thread::spawn(move || {
        started.send(()).unwrap();
        capture_attempt(
            &gate,
            &AtomicBool::new(false),
            3,
            || OwnerSnapshot {
                sequence_before: reader_sequence.load(Ordering::Acquire),
                owner: reader_owner.load(Ordering::Acquire),
                sequence_after: reader_sequence.load(Ordering::Acquire),
            },
            || {
                reader_opens.fetch_add(1, Ordering::Relaxed);
                Ok::<_, ()>(())
            },
        )
        .unwrap()
    });
    recv(&ready);
    owner.store(3, Ordering::Release);
    sequence.store(11, Ordering::Release);
    drop(access);
    assert!(matches!(
        reader.join().unwrap(),
        CaptureAttempt::OwnWrite(11)
    ));
    assert_eq!(opens.load(Ordering::Relaxed), 0);
}

#[test]
fn unstable_or_unknown_self_snapshot_opens_instead_of_skipping_external_content() {
    for (before, owner, after, own) in [(10, 3, 11, 3), (0, 3, 0, 3), (1, 0, 1, 0), (1, 2, 1, 3)] {
        let gate = Arc::new(CaptureAccess::default());
        let mut opened = false;
        let result = capture_attempt(
            &gate,
            &AtomicBool::new(false),
            own,
            || OwnerSnapshot {
                sequence_before: before,
                owner,
                sequence_after: after,
            },
            || {
                opened = true;
                Ok::<_, ()>(())
            },
        )
        .unwrap();
        assert!(matches!(result, CaptureAttempt::Opened(_)));
        assert!(opened);
    }
}

#[test]
fn failed_open_releases_before_caller_backoff_and_unwind_closes_before_release() {
    let gate = Arc::new(CaptureAccess::default());
    assert!(matches!(
        capture_attempt(
            &gate,
            &AtomicBool::new(false),
            3,
            snapshot,
            || Err::<(), _>("open_failed")
        )
        .unwrap(),
        CaptureAttempt::OpenFailed("open_failed")
    ));
    let access = gate.native(WAIT, || false).unwrap();
    drop(access);
    let trace = Trace::default();
    let unwind = std::panic::catch_unwind({
        let gate = gate.clone();
        let trace = trace.clone();
        move || {
            let _read = capture_attempt(&gate, &AtomicBool::new(false), 3, snapshot, || {
                Ok::<_, ()>(Resource(trace))
            })
            .unwrap();
            panic!("读取中止");
        }
    });
    assert!(unwind.is_err());
    let _native = gate.native(WAIT, || false).unwrap();
    assert_eq!(*trace.lock().unwrap(), ["close"]);
}

#[test]
fn cancellation_while_waiting_and_timeout_never_enter_native_backend() {
    let gate = Arc::new(CaptureAccess::default());
    let held = gate.reader(&AtomicBool::new(false)).unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    let waiting_gate = gate.clone();
    let waiting_cancelled = cancelled.clone();
    let (waiting, ready) = mpsc::channel();
    let waiter = thread::spawn(move || {
        let calls = AtomicUsize::new(0);
        waiting_gate
            .native(WAIT, || {
                if calls.fetch_add(1, Ordering::Relaxed) == 1 {
                    waiting.send(()).unwrap();
                }
                waiting_cancelled.load(Ordering::Acquire)
            })
            .err()
            .unwrap()
    });
    recv(&ready);
    cancelled.store(true, Ordering::Release);
    assert_eq!(waiter.join().unwrap(), AccessError::Cancelled);
    assert_eq!(
        gate.native(Duration::from_millis(20), || false).err(),
        Some(AccessError::Timeout)
    );
    // 到期同轮已取消时，取消优先。
    let calls = AtomicUsize::new(0);
    assert_eq!(
        gate.native(Duration::ZERO, || calls.fetch_add(1, Ordering::Relaxed) > 1)
            .err(),
        Some(AccessError::Cancelled)
    );
    drop(held);
    let _access = gate.native(WAIT, || false).unwrap();
}

#[test]
fn poison_fails_closed_and_guard_drop_still_releases_without_panicking() {
    let gate = Arc::new(CaptureAccess::default());
    let access = gate.native(WAIT, || false).unwrap();
    let poisoned = gate.clone();
    assert!(
        thread::spawn(move || {
            let _locked = poisoned.held.lock().unwrap();
            panic!("同步状态损坏");
        })
        .join()
        .is_err()
    );
    drop(access);
    assert!(!*gate.held.lock().unwrap_err().into_inner());
    assert_eq!(
        gate.native(WAIT, || false).err(),
        Some(AccessError::Poisoned)
    );
    assert_eq!(
        gate.native(WAIT, || true).err(),
        Some(AccessError::Cancelled)
    );
}

#[test]
fn stopped_reader_waiting_for_native_never_queries_or_opens_after_grant() {
    let gate = Arc::new(CaptureAccess::default());
    let held = gate.native(WAIT, || false).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let reader_stop = stop.clone();
    let (started, ready) = mpsc::channel();
    let reader = thread::spawn(move || {
        started.send(()).unwrap();
        capture_attempt(
            &gate,
            &reader_stop,
            3,
            || panic!("停止后不得查询owner"),
            || -> Result<(), ()> { panic!("停止后不得Open") },
        )
        .err()
    });
    recv(&ready);
    stop.store(true, Ordering::Release);
    // 无需等待native释放，停止轮询有界退出。
    assert_eq!(reader.join().unwrap(), Some(AccessError::Cancelled));
    drop(held);
}

#[test]
fn native_error_refusal_unknown_and_cancellation_release_before_cleanup_once() {
    for case in ["write_failed", "refused", "unknown", "cancelled"] {
        let gate = Arc::new(CaptureAccess::default());
        let access = gate.native(WAIT, || false).unwrap();
        let trace = Trace::default();
        let mut script = Script::new(access, gate, trace.clone());
        match case {
            "write_failed" => script.write_error = true,
            "refused" => script.submission = Submission::Refused,
            "unknown" => script.submission = Submission::Unknown,
            "cancelled" => script.check = Box::new(|| Err("request_cancelled")),
            _ => unreachable!(),
        }
        let result = native::run(&mut script);
        match case {
            "write_failed" => assert_eq!(result.unwrap_err(), "write_failed"),
            "refused" => assert_eq!(
                result.unwrap(),
                Completion::NotSubmitted("native_target_unavailable")
            ),
            "unknown" => assert_eq!(result.unwrap(), Completion::Submitted(Submission::Unknown)),
            "cancelled" => assert_eq!(result.unwrap_err(), "request_cancelled"),
            _ => unreachable!(),
        }
        let events = trace.lock().unwrap();
        assert_eq!(
            events.iter().filter(|event| **event == "restore").count(),
            usize::from(case == "unknown")
        );
        assert_eq!(
            events.iter().filter(|event| **event == "submit").count(),
            usize::from(matches!(case, "refused" | "unknown"))
        );
    }
}

#[test]
fn after_wait_original_selection_and_input_desktop_guards_still_refuse_submission() {
    use crate::{
        paste::{
            permit::{Monitor, Selection, Session},
            request::Mode,
        },
        platform::Target,
    };
    for scenario in ["revoked", "away_back", "desktop_false", "desktop_unknown"] {
        let gate = Arc::new(CaptureAccess::default());
        let held = gate.reader(&AtomicBool::new(false)).unwrap();
        let target = Target {
            window: 20,
            focus: 21,
            thread: 2,
            process: 3,
            focus_class: "Edit".into(),
            ..Target::default()
        };
        let monitor = Arc::new(Monitor::new(10, 20));
        monitor.started();
        let mut session = Session::new(7, target.clone(), monitor.clone());
        let permit = session
            .select(
                7,
                9,
                Mode::Paste,
                Selection {
                    revision: 0,
                    nonce: 1,
                },
                10,
            )
            .unwrap();
        let desktop = Arc::new(AtomicUsize::new(1));
        let native_desktop = desktop.clone();
        let trace = Trace::default();
        let native_trace = trace.clone();
        let native_gate = gate.clone();
        let (waiting, ready) = mpsc::channel();
        let native = thread::spawn(move || {
            let calls = AtomicUsize::new(0);
            let access = native_gate
                .native(WAIT, || {
                    if calls.fetch_add(1, Ordering::Relaxed) == 1 {
                        waiting.send(()).unwrap();
                    }
                    false
                })
                .unwrap();
            let mut script = Script::new(access, native_gate, native_trace);
            script.check = Box::new(move || {
                let input_desktop = native::input_desktop_ready(
                    match native_desktop.load(Ordering::Acquire) {
                        1 => Some(true),
                        2 => Some(false),
                        _ => None,
                    },
                    || permit.revoke(),
                );
                native::Conditions {
                    cancelled: false,
                    input_desktop,
                    target_valid: true,
                    permitted: permit.can_submit_native(7, 9, Mode::Paste, &target, 10),
                    modifiers_released: true,
                    ordinary: true,
                }
                .check()
            });
            native::run(&mut script).unwrap()
        });
        recv(&ready);
        match scenario {
            "revoked" => {
                monitor.uncertain();
            }
            "away_back" => {
                monitor.observe(99, 100, 99);
                monitor.observe(10, 101, 10);
            }
            "desktop_false" => desktop.store(2, Ordering::Release),
            "desktop_unknown" => desktop.store(0, Ordering::Release),
            _ => unreachable!(),
        }
        drop(held);
        let reason = if scenario.starts_with("desktop") {
            "native_input_desktop_unavailable"
        } else {
            "selection_boundary_changed"
        };
        assert_eq!(native.join().unwrap(), Completion::NotSubmitted(reason));
        assert_eq!(*trace.lock().unwrap(), ["write"]);
    }
}

#[test]
fn open_holder_null_is_ambiguous_and_classification_is_finite() {
    assert_eq!(
        native::open_holder(false, 0, 1, 2),
        "unknown_null_or_unavailable"
    );
    for (pid, expected) in [
        (0, "unknown_process"),
        (1, "self_process"),
        (2, "target_process"),
        (3, "other_process"),
    ] {
        assert_eq!(native::open_holder(true, pid, 1, 2), expected);
    }
}
