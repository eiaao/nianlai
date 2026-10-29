use super::*;
use crate::paste::native::{self, Backend, Completion, Submission};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
};

fn publication() -> Publication {
    Publication::sample(7, || 42, || 7).unwrap()
}

#[test]
fn direct_metadata_match_checks_the_exact_publication_without_opening_clipboard() {
    let expected = publication();
    for (before, owner, after, matched) in [
        (42, 7, 42, true),
        (0, 7, 0, false),
        (42, 8, 42, false),
        (43, 7, 43, false),
        (42, 7, 43, false),
    ] {
        let operations = RefCell::new(Vec::new());
        let mut sequences = [before, after].into_iter();
        assert_eq!(
            expected.matches(
                || {
                    operations.borrow_mut().push("sequence");
                    sequences.next().unwrap()
                },
                || {
                    operations.borrow_mut().push("owner");
                    owner
                }
            ),
            matched
        );
        assert_eq!(*operations.borrow(), ["sequence", "owner", "sequence"]);
    }
}

#[derive(Clone, Copy)]
enum Step {
    Busy,
    Ready,
    OwnerChanged,
    SequenceChanged,
    Unstable,
    Zero,
    CloseFailed,
    Slow,
}

// 同一套 native::run 和 Budget::wait；只有时钟、OS 探针和消息调用被内存替身替换。
struct Script {
    time: Duration,
    steps: VecDeque<Step>,
    fallback: Step,
    current: Step,
    reads: u32,
    locked: bool,
    publication: Option<Publication>,
    publication_error: Option<&'static str>,
    budget: Option<Budget>,
    guard_failure: Option<(Duration, &'static str)>,
    guard_cost: Option<(bool, Duration, Duration)>,
    fence_failure: Option<Duration>,
    failure_after_prepare: Option<&'static str>,
    prepared: bool,
    late_cancel: bool,
    cancel_on_close: usize,
    submission: Submission,
    send_duration: Duration,
    writes: usize,
    sends: usize,
    finishes: usize,
    restores: usize,
    operations: Vec<&'static str>,
}

impl Script {
    fn new(steps: impl IntoIterator<Item = Step>) -> Self {
        Self {
            time: Duration::ZERO,
            steps: steps.into_iter().collect(),
            fallback: Step::Ready,
            current: Step::Ready,
            reads: 0,
            locked: false,
            publication: Some(publication()),
            publication_error: None,
            budget: None,
            guard_failure: None,
            guard_cost: None,
            fence_failure: None,
            failure_after_prepare: None,
            prepared: false,
            late_cancel: false,
            cancel_on_close: 0,
            submission: Submission::Sent,
            send_duration: Duration::ZERO,
            writes: 0,
            sends: 0,
            finishes: 0,
            restores: 0,
            operations: Vec::new(),
        }
    }

    fn available(&mut self) -> Result<(), &'static str> {
        let mut budget = self.budget.take().unwrap_or_else(|| Budget::new(self.time));
        let publication = self.publication;
        let result = budget.wait(self, publication).map_err(|reason| {
            if reason == CHANGED {
                self.publication_error.unwrap_or(reason)
            } else {
                reason
            }
        });
        self.budget = Some(budget);
        result
    }

    fn count(&self, operation: &str) -> usize {
        self.operations
            .iter()
            .filter(|&&op| op == operation)
            .count()
    }

    fn not_submitted(&mut self, reason: &'static str) {
        assert_eq!(native::run(self).unwrap(), Completion::NotSubmitted(reason));
        assert_eq!(self.writes, 1, "写后失败不得覆盖或再次写入");
        assert_eq!(self.sends, 0);
        assert_eq!(self.finishes, 1);
        assert_eq!(self.restores, 0);
        assert!(!self.locked);
    }
}

impl Clipboard for Script {
    fn open(&mut self) -> bool {
        assert!(!self.locked);
        self.operations.push("open");
        self.current = self.steps.pop_front().unwrap_or(self.fallback);
        self.reads = 0;
        self.locked = !matches!(self.current, Step::Busy);
        self.locked
    }
    fn sequence(&mut self) -> u32 {
        assert!(self.locked);
        self.operations.push("sequence");
        self.reads += 1;
        match self.current {
            Step::SequenceChanged => 43,
            Step::Unstable if self.reads > 1 => 43,
            Step::Zero => 0,
            _ => 42,
        }
    }
    fn owner(&mut self) -> usize {
        assert!(self.locked);
        self.operations.push("owner");
        if matches!(self.current, Step::OwnerChanged) {
            8
        } else {
            7
        }
    }
    fn close(&mut self) -> bool {
        assert!(self.locked);
        self.operations.push("close");
        self.locked = false;
        if matches!(self.current, Step::Slow) {
            self.time += BUDGET;
        }
        if self.cancel_on_close == self.count("close") {
            self.late_cancel = true;
        }
        !matches!(self.current, Step::CloseFailed)
    }
}

impl Environment for Script {
    fn now(&self) -> Duration {
        self.time
    }
    fn pause(&mut self, duration: Duration) {
        assert!(!self.locked, "等待不得持有 OS 剪贴板锁");
        self.operations.push("wait");
        self.time += duration;
    }
    fn guard(&mut self) -> Result<(), &'static str> {
        if let Some((final_guard, after, duration)) = self.guard_cost
            && final_guard == self.prepared
            && self.time >= after
        {
            // 既有同步可能不可中断；结束后仍必须依据原截止时间拒绝探针和发送。
            self.time += duration;
            self.guard_cost = None;
        }
        native::guarded(self)
    }
    fn probe(&mut self, publication: Publication) -> Probe {
        probe(self, publication)
    }
}

impl Backend for Script {
    fn synchronize(&mut self) -> bool {
        assert!(!self.locked, "前台事件同步不得持有 OS 剪贴板锁");
        self.operations.push("fence");
        self.fence_failure.is_none_or(|time| self.time < time)
    }
    fn check(&mut self) -> Result<(), &'static str> {
        assert!(!self.locked);
        self.operations.push("check");
        if self.late_cancel {
            return Err("request_cancelled");
        }
        if self.prepared
            && let Some(reason) = self.failure_after_prepare
        {
            return Err(reason);
        }
        if let Some((time, reason)) = self.guard_failure
            && self.time >= time
        {
            return Err(reason);
        }
        Ok(())
    }
    fn write(&mut self) -> Result<(), String> {
        self.operations.push("write");
        self.writes += 1;
        Ok(())
    }
    fn prepare_submit(&mut self) -> Result<(), &'static str> {
        self.available()?;
        self.prepared = true;
        Ok(())
    }
    fn submit(&mut self) -> Result<Submission, &'static str> {
        self.available()?;
        if self.late_cancel {
            return Err("request_cancelled");
        }
        assert!(!self.locked, "目标必须能自行打开剪贴板");
        self.operations.push("send");
        self.sends += 1;
        self.time += self.send_duration;
        Ok(self.submission)
    }
    fn finish(&mut self, result: Option<Completion>) {
        assert!(!self.locked);
        self.operations.push("release");
        self.finishes += 1;
        if matches!(result, Some(Completion::Submitted(_))) {
            self.operations.push("restore");
            self.restores += 1;
        }
    }
}

#[test]
fn publication_requires_checked_close_then_stable_nonzero_sequence_and_exact_writer() {
    for (writer, before, owner, after, valid) in [
        (7, 42, 7, 42, true),
        (7, 0, 7, 0, false),
        (7, 0, 7, 42, false),
        (7, 42, 7, 0, false),
        (7, 42, 7, 43, false),
        (7, 42, 8, 42, false),
        (7, 42, 0, 42, false),
        (0, 42, 0, 42, false),
    ] {
        let events = RefCell::new(Vec::new());
        let mut sequences = [before, after].into_iter();
        let result = publish(
            writer,
            || {
                events.borrow_mut().push("close");
                true
            },
            || {
                events.borrow_mut().push("sequence");
                sequences.next().unwrap()
            },
            || {
                events.borrow_mut().push("owner");
                owner
            },
        );
        // 生产 write 同样在 publish 之后调用可能等待数据库的 history.reuse。
        events.borrow_mut().push("reuse");
        assert_eq!(result.is_ok(), valid);
        assert_eq!(
            *events.borrow(),
            ["close", "sequence", "owner", "sequence", "reuse"]
        );
    }
    assert_eq!(
        publish(
            7,
            || false,
            || panic!("关闭失败不能查询sequence"),
            || panic!("关闭失败不能查询owner")
        ),
        Err(CLOSE_FAILED)
    );
}

#[test]
fn close_sequence_change_accepts_a_stable_post_close_snapshot_of_the_writer() {
    let sequence = Cell::new(42);
    let result = publish(
        7,
        || {
            sequence.set(43);
            true
        },
        || sequence.get(),
        || 7,
    );
    assert_eq!(result, Publication::sample(7, || 43, || 7).ok_or(CHANGED));
    let mut script = Script::new([Step::SequenceChanged, Step::SequenceChanged]);
    script.publication = result.ok();
    assert_eq!(
        native::run(&mut script).unwrap(),
        Completion::Submitted(Submission::Sent)
    );
    assert_eq!((script.writes, script.sends), (1, 1));
}

#[test]
fn busy_then_ready_submits_once_only_after_both_probes_are_closed() {
    let mut script = Script::new([Step::Busy, Step::Busy, Step::Ready, Step::Ready]);
    assert_eq!(
        native::run(&mut script).unwrap(),
        Completion::Submitted(Submission::Sent)
    );
    assert_eq!(
        (script.writes, script.sends, script.count("close")),
        (1, 1, 2)
    );
    assert_eq!(script.time, Duration::from_millis(10));
    let send = script
        .operations
        .iter()
        .position(|op| *op == "send")
        .unwrap();
    assert_eq!(script.operations[send - 1], "close");
    for (index, operation) in script.operations.iter().enumerate() {
        if *operation == "owner" {
            assert_eq!(
                script.operations[index - 1..index + 3],
                ["sequence", "owner", "sequence", "close"]
            );
        }
    }
}

#[test]
fn permanent_busy_and_null_holder_cannot_be_used_as_evidence_of_availability() {
    for (observed, process, holder) in [
        (true, 99, "other_process"),
        (false, 0, "unknown_null_or_unavailable"),
    ] {
        assert_eq!(native::open_holder(observed, process, 1, 2), holder);
        let mut script = Script::new([]);
        script.fallback = Step::Busy;
        script.not_submitted(BUSY);
        assert_eq!(script.time, BUDGET, "{holder}");
        assert_eq!(script.count("open"), 40);
        assert_eq!(script.count("sequence"), 0);
        assert_eq!(script.count("owner"), 0);
        assert_eq!(script.count("close"), 0);
    }
}

#[test]
fn changed_owner_sequence_zero_and_failed_close_all_refuse_without_rewriting() {
    for (step, reason) in [
        (Step::OwnerChanged, CHANGED),
        (Step::SequenceChanged, CHANGED),
        (Step::Unstable, CHANGED),
        (Step::Zero, CHANGED),
        (Step::CloseFailed, CLOSE_FAILED),
    ] {
        let mut script = Script::new([step]);
        script.not_submitted(reason);
        assert_eq!(script.count("close"), 1);
    }
}

#[test]
fn unavailable_publication_is_written_but_not_submitted() {
    for reason in [CHANGED, CLOSE_FAILED] {
        let mut script = Script::new([]);
        script.publication = None;
        script.publication_error = Some(reason);
        script.not_submitted(reason);
        assert_eq!(script.count("open"), 0);
    }
}

#[test]
fn every_original_guard_is_rechecked_after_waiting_before_another_probe() {
    for reason in [
        "request_cancelled",
        "selection_boundary_changed",
        "invalid_target",
        "native_input_desktop_unavailable",
        "native_modifiers_held",
        "target_may_be_elevated",
    ] {
        for final_probe in [false, true] {
            let steps = if final_probe {
                vec![Step::Ready, Step::Busy, Step::Ready]
            } else {
                vec![Step::Busy, Step::Ready]
            };
            let mut script = Script::new(steps);
            script.guard_failure = Some((POLL, reason));
            script.not_submitted(reason);
            assert_eq!(script.time, POLL);
            assert_eq!(script.count("open"), if final_probe { 2 } else { 1 });
        }
    }
    let mut script = Script::new([Step::Busy, Step::Ready]);
    script.fence_failure = Some(POLL);
    script.not_submitted("selection_boundary_changed");
    assert_eq!(script.count("open"), 1);
}

#[test]
fn prepare_return_still_has_a_guard_before_final_probe_and_send() {
    for reason in [
        "request_cancelled",
        "selection_boundary_changed",
        "invalid_target",
    ] {
        let mut script = Script::new([Step::Ready, Step::Ready]);
        script.failure_after_prepare = Some(reason);
        script.not_submitted(reason);
        assert_eq!(script.count("open"), 1);
    }
}

#[test]
fn reoccupation_at_final_probe_waits_then_sends_once_without_holding_os_lock() {
    let mut script = Script::new([Step::Ready, Step::Busy, Step::Ready]);
    assert_eq!(
        native::run(&mut script).unwrap(),
        Completion::Submitted(Submission::Sent)
    );
    assert_eq!(
        (
            script.writes,
            script.sends,
            script.count("open"),
            script.count("close")
        ),
        (1, 1, 3, 2)
    );
    assert_eq!(script.time, POLL);
}

#[test]
fn final_replacement_or_close_failure_never_sends() {
    for (step, reason) in [
        (Step::OwnerChanged, CHANGED),
        (Step::SequenceChanged, CHANGED),
        (Step::CloseFailed, CLOSE_FAILED),
    ] {
        let mut script = Script::new([Step::Ready, step]);
        script.not_submitted(reason);
        assert_eq!(script.count("close"), 2);
    }
}

#[test]
fn prepare_and_final_wait_share_one_deadline_without_reset() {
    let mut steps = vec![Step::Busy; 38];
    steps.push(Step::Ready); // prepare 用掉 190ms，末次 busy 只能再等 10ms。
    let mut script = Script::new(steps);
    script.fallback = Step::Busy;
    script.not_submitted(BUSY);
    assert_eq!(script.time, BUDGET);
    assert_eq!(script.count("wait"), 40);
    assert_eq!(script.count("close"), 1);
    assert_eq!(script.budget.as_ref().unwrap().waits(), 40);
}

#[test]
fn slow_guard_can_overrun_but_never_renews_deadline_or_probes_after_expiry() {
    for final_guard in [false, true] {
        for cancelled in [false, true] {
            let mut steps = vec![Step::Busy; 38];
            steps.push(Step::Ready);
            let mut script = Script::new(steps);
            script.guard_cost = Some((
                final_guard,
                Duration::from_millis(190),
                Duration::from_millis(15),
            ));
            if cancelled {
                script.guard_failure = Some((BUDGET, "request_cancelled"));
            }
            script.not_submitted(if cancelled { "request_cancelled" } else { BUSY });
            assert_eq!(script.time, Duration::from_millis(205));
            assert_eq!(script.count("open"), if final_guard { 39 } else { 38 });
            assert_eq!(script.count("close"), usize::from(final_guard));
            let budget = script.budget.as_ref().unwrap();
            assert_eq!(budget.started, Duration::ZERO);
            assert_eq!(budget.observed_elapsed(), Duration::from_millis(205));
            assert_eq!(budget.waits(), 38);
        }
    }
}

#[test]
fn successful_probe_that_finishes_at_deadline_still_cannot_send() {
    let mut script = Script::new([Step::Slow]);
    script.not_submitted(BUSY);
    assert_eq!(script.time, BUDGET);
    assert_eq!(script.count("close"), 1);
}

#[test]
fn cancellation_before_write_is_zero_writes_and_after_probe_is_zero_sends() {
    let mut before = Script::new([]);
    before.guard_failure = Some((Duration::ZERO, "request_cancelled"));
    assert_eq!(native::run(&mut before).unwrap_err(), "request_cancelled");
    assert_eq!(
        (
            before.writes,
            before.sends,
            before.finishes,
            before.restores
        ),
        (0, 0, 1, 0)
    );
    for close in [1, 2] {
        let mut after = Script::new([Step::Ready, Step::Ready]);
        after.cancel_on_close = close;
        after.not_submitted("request_cancelled");
        assert_eq!(after.count("close"), close);
    }
}

#[test]
fn cancellation_at_the_shared_deadline_wins_over_busy_timeout() {
    let mut script = Script::new([]);
    script.fallback = Step::Busy;
    script.guard_failure = Some((BUDGET, "request_cancelled"));
    script.not_submitted("request_cancelled");
    assert_eq!(script.time, BUDGET);
}

#[test]
fn submitted_timeout_preserves_unknown_and_never_probes_or_sends_again() {
    let mut script = Script::new([Step::Busy, Step::Ready, Step::Ready]);
    script.submission = Submission::Unknown;
    script.send_duration = Duration::from_millis(300);
    let result = native::run(&mut script).unwrap();
    assert_eq!(result, Completion::Submitted(Submission::Unknown));
    assert_eq!(
        (script.writes, script.sends, script.count("open")),
        (1, 1, 3)
    );
    assert_eq!(script.finishes, 1);
    assert!(script.time > BUDGET);
    assert_eq!(script.budget.as_ref().unwrap().observed_elapsed(), POLL);
}
