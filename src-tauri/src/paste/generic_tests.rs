use super::*;
use crate::{
    history::content::{Content, Data, FILES, HTML, PNG, RTF, TEXT},
    paste::{
        as_text,
        capture_access::{AccessGuard, CaptureAccess},
        native,
        native_access::{self, Budget, Environment, Probe, Publication},
        permit::{Monitor, Selection, Session},
        request::{Mode, Request},
    },
    platform::{Target, hash::sha256},
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
enum Change {
    Cancel,
    Switch,
    Identity,
    Desktop,
    Elevated,
    Modifiers,
}

// 平台副作用注入；事务次序来自生产 run，等待来自生产 Budget，许可与采集协调使用真实实现。
struct Script {
    request: Request,
    monitor: Arc<Monitor>,
    foreground: usize,
    access: Arc<CaptureAccess>,
    held: Option<AccessGuard>,
    published: Option<Arc<Content>>,
    publication: Option<Publication>,
    publication_error: Option<&'static str>,
    probes: VecDeque<Probe>,
    repeat_probe: Probe,
    budget: Option<Budget>,
    now: Duration,
    restore_delay: Duration,
    change: Option<(&'static str, Change)>,
    input_desktop: Option<bool>,
    ordinary: bool,
    modifiers: bool,
    write_error: Option<&'static str>,
    coordinate_error: Option<&'static str>,
    actual_events: u32,
    sends: usize,
    insertions: usize,
    after_send_busy: bool,
    content_evidence: bool,
    finishes: usize,
    operations: Vec<&'static str>,
}
impl Script {
    fn new(content: Arc<Content>) -> Self {
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
        Self {
            request: Request {
                session: 7,
                target,
                panel: 10,
                record_id: 9,
                content,
                mode: Mode::Paste,
                started: Instant::now(),
                external_switch: false,
                permit: Some(permit),
                copy_only_reason: None,
                cancelled: Arc::new(AtomicBool::new(false)),
            },
            monitor,
            foreground: 10,
            access: Arc::new(CaptureAccess::default()),
            held: None,
            published: None,
            publication: None,
            publication_error: None,
            probes: VecDeque::new(),
            repeat_probe: Probe::Ready,
            budget: None,
            now: Duration::ZERO,
            restore_delay: Duration::ZERO,
            change: None,
            input_desktop: Some(true),
            ordinary: true,
            modifiers: true,
            write_error: None,
            coordinate_error: None,
            actual_events: 4,
            sends: 0,
            insertions: 0,
            after_send_busy: false,
            content_evidence: false,
            finishes: 0,
            operations: Vec::new(),
        }
    }
    fn ready() -> Self {
        Self::new(Content::text("synthetic-only", sha256).unwrap())
    }
    fn checkpoint(&mut self, stage: &'static str) {
        self.operations.push(stage);
        if let Some((at, change)) = self.change
            && at == stage
        {
            self.change = None;
            match change {
                Change::Cancel => self.request.cancelled.store(true, Ordering::Release),
                Change::Switch => {
                    self.foreground = 30;
                    self.monitor.observe(30, 200, 30);
                }
                Change::Identity => self.request.target.focus = 22,
                Change::Desktop => self.input_desktop = None,
                Change::Elevated => self.ordinary = false,
                Change::Modifiers => self.modifiers = false,
            }
        }
    }
    fn boundary(&self) -> Result<(), &'static str> {
        if self.request.cancelled.load(Ordering::Acquire) {
            return Err("request_cancelled");
        }
        let permit = self.request.permit.as_ref().unwrap();
        if !native::input_desktop_ready(self.input_desktop, || permit.revoke()) {
            return Err("generic_input_desktop_unavailable");
        }
        if !permit.check(
            self.request.session,
            self.request.record_id,
            self.request.mode,
            &self.request.target,
            self.foreground,
        ) {
            return Err("selection_boundary_changed");
        }
        if !self.ordinary {
            return Err("target_may_be_elevated");
        }
        Ok(())
    }
    fn submission_guard(&self) -> Result<(), &'static str> {
        self.boundary()?;
        if !self.request.permit.as_ref().unwrap().can_send(
            self.request.session,
            self.request.record_id,
            self.request.mode,
            &self.request.target,
            self.foreground,
        ) {
            return Err("selection_boundary_changed");
        }
        if !self.modifiers {
            return Err("generic_modifiers_held");
        }
        Ok(())
    }
    fn acquire(&mut self) {
        self.held = Some(
            self.access
                .native(Duration::from_millis(20), || false)
                .unwrap(),
        );
    }
}
impl Environment for Script {
    fn now(&self) -> Duration {
        self.now
    }
    fn pause(&mut self, duration: Duration) {
        self.now += duration;
        self.checkpoint("pause");
    }
    fn guard(&mut self) -> Result<(), &'static str> {
        self.checkpoint("guard");
        self.submission_guard()
    }
    fn probe(&mut self, _: Publication) -> Probe {
        assert!(self.held.is_some());
        self.checkpoint("probe");
        self.probes.pop_front().unwrap_or(self.repeat_probe)
    }
}
impl Backend for Script {
    fn coordinate(&mut self) -> Result<(), String> {
        self.checkpoint("coordinate");
        if let Some(reason) = self.coordinate_error {
            return Err(reason.into());
        }
        self.acquire();
        Ok(())
    }
    fn check_before_publish(&mut self) -> Result<(), &'static str> {
        self.checkpoint("check_before_publish");
        self.boundary()
    }
    fn publish(&mut self) -> Result<(), String> {
        assert!(self.held.is_some());
        self.checkpoint("publish");
        assert!(
            self.access
                .native(Duration::from_millis(1), || false)
                .is_err(),
            "发布时自家reader不得进入"
        );
        if let Some(reason) = self.write_error {
            return Err(reason.into());
        }
        self.published = Some(self.request.content.clone());
        if self.publication_error.is_none() {
            self.publication = Publication::sample(42, || 17, || 42);
        }
        Ok(())
    }
    fn restore(&mut self) -> Result<(), &'static str> {
        drop(self.held.take());
        assert!(
            self.access
                .native(Duration::from_millis(1), || false)
                .is_ok(),
            "主线程等待前释放采集协调"
        );
        self.checkpoint("restore");
        self.boundary()?;
        assert!(
            self.request
                .permit
                .as_ref()
                .unwrap()
                .arm_return(self.foreground, 100)
        );
        self.foreground = 20;
        self.monitor.observe(20, 101, 20);
        self.now += self.restore_delay;
        Ok(())
    }
    fn prepare_submit(&mut self) -> Result<(), &'static str> {
        self.checkpoint("prepare");
        self.boundary()
    }
    fn available(&mut self) -> Result<(), &'static str> {
        self.acquire();
        let mut budget = Budget::new(self.now);
        let publication = self.publication;
        let result = budget.wait(self, publication).map_err(|reason| {
            availability_reason(if reason == native_access::CHANGED {
                self.publication_error.unwrap_or(reason)
            } else {
                reason
            })
        });
        self.budget = Some(budget);
        result
    }
    fn send(&mut self) -> Result<Submission, &'static str> {
        self.checkpoint("send_guard");
        self.submission_guard()?;
        self.checkpoint("send");
        self.sends += 1;
        self.insertions += usize::from(self.actual_events >= 2 && !self.after_send_busy);
        drop(self.held.take());
        Ok(Submission::from_input_count(self.actual_events, 4))
    }
    fn observe(&mut self) -> bool {
        self.checkpoint("observe");
        self.content_evidence && self.insertions > 0
    }
    fn finish(&mut self, _: Option<Completion>) {
        drop(self.held.take());
        self.finishes += 1;
        self.checkpoint("finish");
    }
}

#[test]
fn generic_busy_gate_prevents_submission() {
    let mut script = Script::ready();
    script.repeat_probe = Probe::Busy;
    assert_eq!(
        run(&mut script).unwrap(),
        Completion::NotSubmitted("generic_clipboard_busy")
    );
    assert_eq!(
        (script.sends, script.insertions, script.finishes),
        (0, 0, 1)
    );
    assert_eq!(script.now, native_access::BUDGET);
    assert!(script.held.is_none());
}
#[test]
fn generic_publishes_before_restore_and_sends_once_after_busy_releases() {
    let mut script = Script::ready();
    script.restore_delay = Duration::from_millis(800);
    script.probes.extend([Probe::Busy, Probe::Ready]);
    assert_eq!(
        run(&mut script).unwrap(),
        Completion::Submitted(Submission::Sent)
    );
    let position = |name| script.operations.iter().position(|op| *op == name).unwrap();
    assert!(position("publish") < position("restore"));
    assert!(position("restore") < position("prepare"));
    assert!(position("prepare") < position("probe"));
    assert!(position("probe") < position("send"));
    assert_eq!(
        (script.sends, script.insertions, script.finishes),
        (1, 1, 1)
    );
    assert_eq!(
        script.budget.as_ref().unwrap().observed_elapsed(),
        Duration::from_millis(5)
    );
    assert_eq!(
        script
            .operations
            .iter()
            .filter(|&&op| op == "observe")
            .count(),
        1
    );
}
#[test]
fn generic_changed_close_failure_and_missing_publication_never_send() {
    for (probe, reason) in [
        (Probe::Changed, "generic_clipboard_changed"),
        (Probe::CloseFailed, "generic_clipboard_close_failed"),
    ] {
        let mut script = Script::ready();
        script.repeat_probe = probe;
        assert_eq!(run(&mut script).unwrap(), Completion::NotSubmitted(reason));
        assert_eq!((script.sends, script.finishes), (0, 1));
    }
    for error in [native_access::CHANGED, native_access::CLOSE_FAILED] {
        let mut script = Script::ready();
        script.publication_error = Some(error);
        assert_eq!(
            run(&mut script).unwrap(),
            Completion::NotSubmitted(availability_reason(error))
        );
        assert_eq!(script.sends, 0);
        assert_eq!(script.budget.as_ref().unwrap().probes(), 0);
    }
}
#[test]
fn generic_revocation_cancel_desktop_identity_and_elevation_never_send() {
    for (change, reason) in [
        (Change::Cancel, "request_cancelled"),
        (Change::Switch, "selection_boundary_changed"),
        (Change::Identity, "selection_boundary_changed"),
        (Change::Desktop, "generic_input_desktop_unavailable"),
        (Change::Elevated, "target_may_be_elevated"),
        (Change::Modifiers, "generic_modifiers_held"),
    ] {
        for stage in ["publish", "prepare", "pause"] {
            let mut script = Script::ready();
            script.probes.extend([Probe::Busy, Probe::Ready]);
            script.change = Some((stage, change));
            assert_eq!(
                run(&mut script).unwrap(),
                Completion::NotSubmitted(reason),
                "{stage}"
            );
            assert_eq!((script.sends, script.finishes), (0, 1));
            assert!(script.held.is_none());
        }
    }
}
#[test]
fn generic_request_tuple_and_cancel_are_checked_before_publication() {
    for field in ["session", "record", "mode", "cancel"] {
        let mut script = Script::ready();
        match field {
            "session" => script.request.session += 1,
            "record" => script.request.record_id += 1,
            "mode" => script.request.mode = Mode::Text,
            "cancel" => script.request.cancelled.store(true, Ordering::Release),
            _ => unreachable!(),
        }
        assert_eq!(
            run(&mut script).unwrap(),
            Completion::NotSubmitted(if field == "cancel" {
                "request_cancelled"
            } else {
                "selection_boundary_changed"
            })
        );
        assert!(script.published.is_none());
        assert!(!script.operations.contains(&"restore"));
        assert_eq!((script.sends, script.finishes), (0, 1));
    }
}
#[test]
fn generic_write_and_coordination_errors_finish_without_restoring_or_sending() {
    for reason in [
        "clipboard_allocate_failed",
        "clipboard_format_failed",
        "clipboard_capture_busy",
    ] {
        let mut script = Script::ready();
        if reason == "clipboard_capture_busy" {
            script.coordinate_error = Some(reason);
        } else {
            script.write_error = Some(reason);
        }
        assert_eq!(run(&mut script).unwrap_err(), reason);
        assert!(!script.operations.contains(&"restore"));
        assert_eq!((script.sends, script.finishes), (0, 1));
        assert!(script.held.is_none());
    }
}
#[test]
fn generic_partial_input_is_unknown_and_post_send_competition_never_retries() {
    for count in 0..=4 {
        let mut script = Script::ready();
        script.actual_events = count;
        assert_eq!(
            run(&mut script).unwrap(),
            match count {
                0 => Completion::NotSubmitted("send_failed"),
                4 => Completion::Submitted(Submission::Sent),
                _ => Completion::Submitted(Submission::Unknown),
            }
        );
        assert_eq!((script.sends, script.finishes), (1, 1));
        assert_eq!(script.operations.contains(&"observe"), count == 4);
    }
    let mut script = Script::ready();
    script.after_send_busy = true;
    script.content_evidence = true;
    assert_eq!(
        run(&mut script).unwrap(),
        Completion::Submitted(Submission::Sent)
    );
    assert_eq!((script.sends, script.insertions), (1, 0));
    let mut script = Script::ready();
    script.content_evidence = true;
    assert_eq!(
        run(&mut script).unwrap(),
        Completion::Submitted(Submission::Confirmed)
    );
}
#[test]
fn generic_final_lightweight_guard_can_refuse_after_a_ready_probe() {
    for change in [
        Change::Cancel,
        Change::Switch,
        Change::Identity,
        Change::Desktop,
        Change::Modifiers,
    ] {
        let mut script = Script::ready();
        script.change = Some(("probe", change));
        assert!(matches!(
            run(&mut script).unwrap(),
            Completion::NotSubmitted(_)
        ));
        assert_eq!(script.sends, 0);
    }
}
#[test]
#[cfg(windows)]
fn generic_production_override_preserves_rich_files_images_and_explicit_text() {
    let plain = Content::text("synthetic-format-marker", sha256).unwrap();
    let bytes = |value: &'static [u8]| Data::Bytes(Arc::from(value));
    let rich = |name: &str| {
        Content::new(
            BTreeMap::from([
                (TEXT.into(), plain.formats[TEXT].data.clone()),
                (name.into(), bytes(b"synthetic-rich-format\0")),
            ]),
            sha256,
        )
        .unwrap()
    };
    let files = Content::new(
        BTreeMap::from([(
            FILES.into(),
            Data::Files(
                vec![
                    "C:\\synthetic\\first.txt".into(),
                    "C:\\synthetic\\second.txt".into(),
                ]
                .into(),
            ),
        )]),
        sha256,
    )
    .unwrap();
    let image = Content::new(
        BTreeMap::from([(PNG.into(), bytes(b"synthetic-png-bytes"))]),
        sha256,
    )
    .unwrap();
    for (content, native_default) in [
        (rich(HTML), true),
        (rich(RTF), true),
        (files, false),
        (image, false),
    ] {
        let (written, conversion) = as_text::apply(&content, Mode::Paste, None, None, sha256);
        assert!(conversion.is_null());
        let mut script = Script::new(written.clone());
        assert_eq!(
            crate::platform::paste::native_candidate(&script.request, &written, true),
            native_default
        );
        assert!(
            !crate::platform::paste::native_candidate(&script.request, &written, false),
            "正式覆盖规则仍走generic，包含HTML/RTF时也不绕过真实通用runner"
        );
        assert_eq!(
            run(&mut script).unwrap(),
            Completion::Submitted(Submission::Sent)
        );
        let published = script.published.unwrap();
        assert_eq!(published.formats.len(), content.formats.len());
        for (name, format) in &content.formats {
            assert!(published.formats[name].data == format.data);
        }
    }
    let mut script = Script::new(plain.clone());
    assert!(crate::platform::paste::native_candidate(
        &script.request,
        &plain,
        true
    ));
    script.request.target.focus_class = "RichEdit20W".into();
    assert!(!crate::platform::paste::native_candidate(
        &script.request,
        &plain,
        true
    ));
    let rich = rich(HTML);
    let (written, conversion) = as_text::apply(&rich, Mode::Text, None, None, sha256);
    assert_eq!(conversion["conversion"], "plain_text");
    assert_eq!(written.formats.len(), 1);
    assert_eq!(written.raw(TEXT), rich.raw(TEXT));
    let mut script = Script::new(written.clone());
    script.request.mode = Mode::Text;
    assert!(crate::platform::paste::native_candidate(
        &script.request,
        &written,
        true
    ));
    assert!(!crate::platform::paste::native_candidate(
        &script.request,
        &written,
        false
    ));
}
