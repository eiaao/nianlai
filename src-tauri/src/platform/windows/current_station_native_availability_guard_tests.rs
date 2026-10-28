// 真实 prepare_case / Timed / Budget；仅用内存时钟和探针，不调用 Win32。
use super::*;
use std::{cell::Cell, collections::VecDeque, rc::Rc};

struct Fake {
    clock: Rc<Cell<Duration>>,
    guard_costs: VecDeque<Duration>,
    guards: VecDeque<std::result::Result<(), &'static str>>,
    probes: VecDeque<Probe>,
    guard_calls: usize,
    probe_calls: usize,
}
impl Fake {
    fn new() -> Self {
        Self {
            clock: Rc::new(Cell::new(Duration::ZERO)),
            guard_costs: VecDeque::new(),
            guards: VecDeque::new(),
            probes: VecDeque::new(),
            guard_calls: 0,
            probe_calls: 0,
        }
    }
}
impl Environment for Fake {
    fn now(&self) -> Duration {
        self.clock.get()
    }
    fn pause(&mut self, duration: Duration) {
        self.clock.set(self.now() + duration);
    }
    fn guard(&mut self) -> std::result::Result<(), &'static str> {
        self.guard_calls += 1;
        self.clock
            .set(self.now() + self.guard_costs.pop_front().unwrap_or_default());
        self.guards.pop_front().unwrap_or(Ok(()))
    }
    fn probe(&mut self, _: Publication) -> Probe {
        self.probe_calls += 1;
        self.probes.pop_front().unwrap_or(Probe::Ready)
    }
}
fn publication() -> Publication {
    Publication::sample(7, || 42, || 7).unwrap()
}
fn sends(report: &PreparationReport) -> usize {
    let calls = Cell::new(0);
    submit_prepared(report.final_check, || calls.set(calls.get() + 1));
    calls.get()
}

#[test]
fn slow_complete_preflight_finishes_before_each_new_case_budget_starts() {
    let calls = Cell::new(0);
    for _ in 0..2 {
        let mut environment = Timed::new(Fake::new());
        let clock = environment.inner.clock.clone();
        let report = prepare_case(
            || {
                calls.set(calls.get() + 1);
                clock.set(Duration::from_millis(350));
                Ok(())
            },
            &mut environment,
            publication(),
        );
        assert_eq!(report.final_check, Ok(()));
        assert_eq!(report.elapsed, Some(Duration::ZERO));
        assert_eq!(sends(&report), 1);
        assert_eq!(environment.inner.probe_calls, 2);
        assert_eq!(report.stages[0].stage, "preflight");
        assert_eq!(report.stages[0].finished_ms, 350.0);
        assert_eq!(report.stages[1].started_ms, 350.0);
        assert!(
            environment
                .operations
                .iter()
                .all(|span| span.started_ms >= 350.0)
        );
    }
    assert_eq!(calls.get(), 2);
}

#[test]
fn failed_complete_preflight_never_initializes_budget_or_checks_probes_or_sends() {
    let mut environment = Timed::new(Fake::new());
    let clock = environment.inner.clock.clone();
    let report = prepare_case(
        || {
            clock.set(Duration::from_millis(350));
            Err("no_product_failed".into())
        },
        &mut environment,
        publication(),
    );
    assert_eq!(report.final_check, Err("diagnostic_preflight_failed"));
    assert!(report.budget.is_none());
    assert_eq!(report.elapsed, None);
    assert_eq!(report.preflight_error.as_deref(), Some("no_product_failed"));
    assert_eq!(
        (
            environment.inner.guard_calls,
            environment.inner.probe_calls,
            sends(&report)
        ),
        (0, 0, 0)
    );
    assert!(environment.operations.is_empty());
    assert_eq!(report.stages.len(), 1);
    assert_eq!(report.stages[0].status, "failed");
}

#[test]
fn slow_live_guard_exhausts_real_deadline_without_a_probe_or_send() {
    let mut fake = Fake::new();
    fake.guard_costs.push_back(Duration::from_millis(250));
    let mut environment = Timed::new(fake);
    let report = prepare_case(|| Ok(()), &mut environment, publication());
    assert_eq!(report.final_check, Err(native_access::BUSY));
    assert_eq!(report.elapsed, Some(Duration::from_millis(250)));
    assert_eq!((environment.inner.probe_calls, sends(&report)), (0, 0));
    assert_eq!(environment.operations.len(), 1);
    assert_eq!(environment.operations[0].stage, "guard");
    assert_eq!(environment.operations[0].finished_ms, 250.0);
    assert_eq!(report.stages.len(), 2, "prepare失败后没有虚构final时段");
}

#[test]
fn changed_authorization_desktop_or_identity_after_wait_stops_before_another_probe() {
    for reason in [
        "authorization_revoked",
        "desktop_changed",
        "identity_changed",
    ] {
        let mut fake = Fake::new();
        fake.guards.extend([Ok(()), Err(reason)]);
        fake.probes.extend([Probe::Busy, Probe::Ready]);
        let mut environment = Timed::new(fake);
        let report = prepare_case(|| Ok(()), &mut environment, publication());
        assert_eq!(report.final_check, Err(reason));
        assert_eq!(report.elapsed, Some(Duration::from_millis(5)));
        assert_eq!((environment.inner.probe_calls, sends(&report)), (1, 0));
        assert_eq!(
            environment
                .operations
                .iter()
                .map(|span| span.stage)
                .collect::<Vec<_>>(),
            ["guard", "probe", "pause", "guard"]
        );
        assert_eq!(environment.operations.last().unwrap().status, reason);
    }
}

#[test]
fn final_guard_keeps_prepare_time_in_the_same_budget_and_does_not_send_after_expiry() {
    let mut fake = Fake::new();
    fake.guard_costs
        .extend([Duration::from_millis(190), Duration::from_millis(15)]);
    let mut environment = Timed::new(fake);
    let report = prepare_case(|| Ok(()), &mut environment, publication());
    assert_eq!(report.final_check, Err(native_access::BUSY));
    assert_eq!(report.elapsed, Some(Duration::from_millis(205)));
    assert_eq!((environment.inner.probe_calls, sends(&report)), (1, 0));
    assert_eq!(report.stages.len(), 3);
    assert_eq!(report.stages[1].finished_ms, 190.0);
    assert_eq!(report.stages[2].started_ms, 190.0);
    assert_eq!(report.stages[2].finished_ms, 205.0);
    assert_eq!(report.budget.as_ref().unwrap().probes(), 1);
}
