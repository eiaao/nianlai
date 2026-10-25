// opt-in 前台诊断的附加证据。这里不持有、不读取也不清除外部切换锁存。
// 只用 UIA 元数据和显式 ControllerFor 关系；绝不读取 Name、Value、Text 或标题。
use super::window;
use crate::log::Logger;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    cell::Cell,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        System::{Com::*, SystemInformation::GetTickCount},
        UI::{Accessibility::*, Input::Ime::ImmGetDefaultIMEWnd, WindowsAndMessaging::*},
    },
    core::Interface,
};

const SESSION_LIMIT: usize = 8;
const QUEUE_CAPACITY: usize = 8;
const RELATION_LIMIT: i32 = 8;
const UIA_TIMEOUT_MS: u32 = 100;
// 这两个期限仅决定诊断证据是否过时，绝不用于外部窗口放行。
const MAX_QUEUE_DELAY: Duration = Duration::from_millis(250);
const MAX_OBSERVATION_AGE: Duration = Duration::from_millis(1500);

// 整个进程最多创建一个 worker。UIA 即便挂起，也只会占住这一条分离线程和有界队列；
// 不重启 worker，不在 Drop/退出路径 join，不让诊断阻塞钩子或退出。
static WORKER: OnceLock<Option<SyncSender<Job>>> = OnceLock::new();

struct Lifetime(Arc<AtomicBool>);
impl Lifetime {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }
}
impl Drop for Lifetime {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Debug, PartialEq)]
enum Admission {
    Probe,
    Limit,
    Ignore,
}

#[derive(Default)]
struct Budget(Cell<usize>);
impl Budget {
    fn admit(&self, event: usize, panel: usize, target: usize) -> Admission {
        if event == 0 || event == panel || event == target {
            return Admission::Ignore;
        }
        let count = self.0.get();
        self.0.set(count.saturating_add(1));
        if count < SESSION_LIMIT {
            Admission::Probe
        } else {
            Admission::Limit
        }
    }
}

#[derive(Default)]
struct Rejections {
    queue_full: Cell<usize>,
    worker_stopped: Cell<usize>,
    worker_unavailable: Cell<usize>,
    session_limit: Cell<usize>,
}
impl Rejections {
    fn record(&self, reason: &'static str) {
        let count = match reason {
            "queue_full" => &self.queue_full,
            "worker_stopped" => &self.worker_stopped,
            "worker_unavailable" => &self.worker_unavailable,
            "session_limit" => &self.session_limit,
            _ => return,
        };
        count.set(count.get().saturating_add(1));
    }
    fn any(&self) -> bool {
        self.queue_full.get() != 0
            || self.worker_stopped.get() != 0
            || self.worker_unavailable.get() != 0
            || self.session_limit.get() != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
struct Identity {
    #[serde(skip)]
    hwnd: usize,
    pid: u32,
    thread: u32,
}
impl Identity {
    fn read(raw: usize) -> Option<Self> {
        unsafe {
            if raw == 0 || !IsWindow(Some(window::hwnd(raw))).as_bool() {
                return None;
            }
            let mut pid = 0;
            let thread = GetWindowThreadProcessId(window::hwnd(raw), Some(&mut pid));
            (pid != 0 && thread != 0).then_some(Self {
                hwnd: raw,
                pid,
                thread,
            })
        }
    }
}

// 只有 foreground::Trace::from_env 通过精确值 "1" 的检查后才创建。
pub(super) struct Session {
    logger: Logger,
    session: u64,
    panel: usize,
    target: usize,
    panel_identity: Option<Identity>,
    target_identity: Option<Identity>,
    lifetime: Lifetime,
    budget: Budget,
    rejected: Rejections,
    sender: Option<SyncSender<Job>>,
}
impl Session {
    pub(super) fn new(logger: &Logger, session: u64, panel: usize, target: usize) -> Self {
        Self {
            logger: logger.clone(),
            session,
            panel,
            target,
            panel_identity: Identity::read(panel),
            target_identity: Identity::read(target),
            lifetime: Lifetime::new(),
            budget: Budget::default(),
            rejected: Rejections::default(),
            sender: worker().cloned(),
        }
    }

    pub(super) fn record(&self, sequence: usize, event: usize, event_thread: u32, event_time: u32) {
        let rejection = match self.budget.admit(event, self.panel, self.target) {
            Admission::Ignore => return,
            Admission::Limit => Some("session_limit"),
            Admission::Probe => {
                let queued = Instant::now();
                // WinEvent 的发生时刻可能早于回调送达；不能从入队时刻重新计算证据年龄。
                let delivery_age = event_age(unsafe { GetTickCount() }, event_time);
                let job = Job {
                    logger: self.logger.clone(),
                    session: self.session,
                    sequence,
                    event,
                    event_identity: Identity::read(event),
                    event_thread,
                    event_time,
                    panel: self.panel,
                    target: self.target,
                    panel_identity: self.panel_identity,
                    target_identity: self.target_identity,
                    active: self.lifetime.0.clone(),
                    queued,
                    delivery_age,
                };
                match &self.sender {
                    Some(sender) => enqueue(sender, job).err(),
                    None => Some("worker_unavailable"),
                }
            }
        };
        if let Some(reason) = rejection {
            // 回调只更新计数，不为拒绝项创建 JSON 或投递日志。
            self.rejected.record(reason);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.lifetime.0.store(false, Ordering::Release);
        if self.rejected.any() {
            self.logger.event(
                "ime_association_summary",
                json!({
                    "session":self.session,"diagnostic_only":true,"session_limit":SESSION_LIMIT,
                    "queue_full":self.rejected.queue_full.get(),
                    "worker_stopped":self.rejected.worker_stopped.get(),
                    "worker_unavailable":self.rejected.worker_unavailable.get(),
                    "session_limit_rejected":self.rejected.session_limit.get(),
                }),
            );
        }
    }
}

fn enqueue<T>(sender: &SyncSender<T>, job: T) -> Result<(), &'static str> {
    sender.try_send(job).map_err(|error| match error {
        TrySendError::Full(_) => "queue_full",
        TrySendError::Disconnected(_) => "worker_stopped",
    })
}

struct Job {
    logger: Logger,
    session: u64,
    sequence: usize,
    event: usize,
    event_identity: Option<Identity>,
    event_thread: u32,
    event_time: u32,
    panel: usize,
    target: usize,
    panel_identity: Option<Identity>,
    target_identity: Option<Identity>,
    active: Arc<AtomicBool>,
    queued: Instant,
    delivery_age: Duration,
}

#[derive(Debug)]
struct Failure {
    status: &'static str,
    reason: &'static str,
    error_code: Option<i32>,
}
impl Failure {
    fn expired(reason: &'static str) -> Self {
        Self {
            status: "expired",
            reason,
            error_code: None,
        }
    }
    fn api(reason: &'static str, error: windows::core::Error) -> Self {
        Self {
            status: "unsupported",
            reason,
            error_code: Some(error.code().0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct NativeState {
    event: Identity,
    panel: Identity,
    target: Option<Identity>,
    foreground: Identity,
    focus: Identity,
    default_ime: Option<Identity>,
}

fn read_state(job: &Job) -> Result<NativeState, Failure> {
    let read = |raw, reason| Identity::read(raw).ok_or_else(|| Failure::expired(reason));
    let event = read(job.event, "event_window_invalid")?;
    let panel = read(job.panel, "panel_window_invalid")?;
    let target = if job.target == 0 {
        None
    } else {
        Some(read(job.target, "target_window_invalid")?)
    };
    let foreground = read(window::foreground(), "foreground_unknown")?;
    let mut info = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetGUIThreadInfo(0, &mut info) }.map_err(|e| Failure::api("gui_thread_info", e))?;
    let focus = read(info.hwndFocus.0 as usize, "focus_unknown")?;
    let default_ime =
        Identity::read(unsafe { ImmGetDefaultIMEWnd(window::hwnd(focus.hwnd)) }.0 as usize);
    if Identity::read(window::foreground()) != Some(foreground) {
        return Err(Failure::expired("foreground_changed_during_capture"));
    }
    Ok(NativeState {
        event,
        panel,
        target,
        foreground,
        focus,
        default_ime,
    })
}

// 与 Win32/COM 无关，防止句柄被跨进程/线程复用，或迟到的关联结果归入下一轮。
fn stale_reason(before: NativeState, after: NativeState, active: bool) -> Option<&'static str> {
    if !active {
        Some("session_ended")
    } else if before.event != after.event
        || before.panel != after.panel
        || before.target != after.target
    {
        Some("window_identity_changed")
    } else if before.foreground != after.foreground {
        Some("foreground_changed")
    } else if before.focus != after.focus {
        Some("focus_changed")
    } else if before.default_ime != after.default_ime {
        Some("default_ime_changed")
    } else {
        None
    }
}

fn event_age(now: u32, event: u32) -> Duration {
    Duration::from_millis(now.wrapping_sub(event) as u64)
}

fn age_reason(age: Duration, delivery_age: Duration, queued: bool) -> Option<&'static str> {
    if queued && age > MAX_QUEUE_DELAY {
        Some("queue_expired")
    } else if delivery_age.saturating_add(age) > MAX_OBSERVATION_AGE {
        Some("observation_expired")
    } else {
        None
    }
}

fn check_job(job: &Job, queued: bool) -> Result<(), Failure> {
    if !job.active.load(Ordering::Acquire) {
        Err(Failure::expired("session_ended"))
    } else if let Some(reason) = age_reason(job.queued.elapsed(), job.delivery_age, queued) {
        Err(Failure::expired(reason))
    } else {
        Ok(())
    }
}

fn worker() -> Option<&'static SyncSender<Job>> {
    WORKER
        .get_or_init(|| {
            let (sender, receiver) = mpsc::sync_channel::<Job>(QUEUE_CAPACITY);
            thread::Builder::new()
                .name("ime-association".into())
                .spawn(move || {
                    let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
                    let _com = initialized.is_ok().then(|| Com);
                    let automation = (|| -> windows::core::Result<IUIAutomation> {
                        initialized.ok()?;
                        let automation: IUIAutomation = unsafe {
                            CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)?
                        };
                        let options = automation.cast::<IUIAutomation2>()?;
                        unsafe {
                            options.SetConnectionTimeout(UIA_TIMEOUT_MS)?;
                            options.SetTransactionTimeout(UIA_TIMEOUT_MS)?;
                        }
                        Ok(automation)
                    })();
                    while let Ok(job) = receiver.recv() {
                        probe(&job, &automation);
                    }
                })
                .ok()
                .map(|handle| {
                    drop(handle);
                    sender
                })
        })
        .as_ref()
}

struct Com;
impl Drop for Com {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

fn probe(job: &Job, automation: &windows::core::Result<IUIAutomation>) {
    let started = Instant::now();
    let queue_delay_ms = job.queued.elapsed().as_secs_f64() * 1000.0;
    let result = (|| -> Result<(Value, Result<Value, Failure>), Failure> {
        check_job(job, true)?;
        let before = read_state(job)?;
        if job.event_identity != Some(before.event) {
            return Err(Failure::expired("event_identity_changed_before_probe"));
        }
        if job.panel_identity != Some(before.panel) || job.target_identity != before.target {
            return Err(Failure::expired("anchor_identity_changed"));
        }
        if before.event.thread != job.event_thread {
            return Err(Failure::expired("event_thread_mismatch"));
        }
        // 只查询面板持有前台时的编辑上下文；真正外部前台不查询其他应用的 UIA 焦点。
        if before.foreground != before.panel {
            return Err(Failure {
                status: "rejected",
                reason: "foreground_not_panel",
                error_code: None,
            });
        }
        let native = native_metadata(before);
        check_job(job, false)?;
        let evidence = match automation {
            Ok(automation) => unsafe { query_relations(automation, before, job) },
            Err(error) => Err(Failure {
                status: "unsupported",
                reason: "uia_unavailable",
                error_code: Some(error.code().0),
            }),
        };
        let after = read_state(job)?;
        check_job(job, false)?;
        if let Some(reason) = stale_reason(before, after, job.active.load(Ordering::Acquire)) {
            return Err(Failure::expired(reason));
        }
        Ok((native, evidence))
    })();
    let mut row = json!({
        "session":job.session,"sequence":job.sequence,"win_event_time_ms":job.event_time,
        "event_thread":job.event_thread,"queue_delay_ms":queue_delay_ms,
        "delivery_age_ms":job.delivery_age.as_millis() as u64,
        "probe_ms":started.elapsed().as_secs_f64()*1000.0,"diagnostic_only":true,
    });
    match result {
        Ok((native, Ok(evidence))) => {
            row["status"] = evidence["status"].clone();
            row["reason"] = evidence["reason"].clone();
            row["native"] = native;
            row["uia"] = evidence;
        }
        Ok((native, Err(error))) => {
            row["native"] = native;
            row["status"] = json!(error.status);
            row["reason"] = json!(error.reason);
            row["error_code"] = json!(error.error_code);
        }
        Err(error) => {
            row["status"] = json!(error.status);
            row["reason"] = json!(error.reason);
            row["error_code"] = json!(error.error_code);
        }
    }
    job.logger.event("ime_association", row);
}

fn in_panel(raw: usize, panel: usize) -> bool {
    raw != 0
        && unsafe {
            raw == panel
                || IsChild(window::hwnd(panel), window::hwnd(raw)).as_bool()
                || GetAncestor(window::hwnd(raw), GA_ROOTOWNER).0 as usize == panel
        }
}

fn gui_metadata(thread: u32, state: NativeState) -> Value {
    let mut info = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    match unsafe { GetGUIThreadInfo(thread, &mut info) } {
        Ok(()) => json!({
            "available":true,"flags":info.flags.0,
            "active_is_panel":info.hwndActive.0 as usize == state.panel.hwnd,
            "focus_matches_captured":info.hwndFocus.0 as usize == state.focus.hwnd,
            "focus_in_panel":in_panel(info.hwndFocus.0 as usize, state.panel.hwnd),
            "caret_in_panel":in_panel(info.hwndCaret.0 as usize, state.panel.hwnd),
            "focus_identity":Identity::read(info.hwndFocus.0 as usize),
        }),
        Err(error) => json!({"available":false,"error_code":error.code().0}),
    }
}

fn native_metadata(state: NativeState) -> Value {
    let ime = state.default_ime.map_or(0, |id| id.hwnd);
    json!({
        "event_identity":state.event,"focus_identity":state.focus,"default_ime_identity":state.default_ime,
        "event_is_current":state.event == state.foreground,
        "focus_in_panel":in_panel(state.focus.hwnd, state.panel.hwnd),
        "event_is_default_ime":ime != 0 && state.event.hwnd == ime,
        "event_owner_is_default_ime":ime != 0 && unsafe { GetWindow(window::hwnd(state.event.hwnd), GW_OWNER) }.is_ok_and(|owner| owner.0 as usize == ime),
        "event_root_owner_is_default_ime":ime != 0 && unsafe { GetAncestor(window::hwnd(state.event.hwnd), GA_ROOTOWNER) }.0 as usize == ime,
        "event_gui":gui_metadata(state.event.thread, state),
        "foreground_gui":gui_metadata(0, state),
    })
}

#[derive(Default, Serialize)]
struct Relation {
    supported: bool,
    exact_element: bool,
    exact_hwnd: bool,
    examined: i32,
    truncated: bool,
    error_code: Option<i32>,
}

unsafe fn controller_for(
    automation: &IUIAutomation,
    source: &IUIAutomationElement,
    destination: &IUIAutomationElement,
    destination_hwnd: usize,
    job: &Job,
) -> Result<Relation, Failure> {
    let result = (|| -> Result<Relation, Failure> {
        check_job(job, false)?;
        let array = unsafe { source.CurrentControllerFor() }
            .map_err(|e| Failure::api("uia_controller_for", e))?;
        let count = unsafe { array.Length() }.map_err(|e| Failure::api("uia_relation_count", e))?;
        let mut relation = Relation {
            supported: count > 0,
            truncated: count > RELATION_LIMIT,
            ..Default::default()
        };
        for index in 0..count.min(RELATION_LIMIT) {
            check_job(job, false)?;
            let element = unsafe { array.GetElement(index) }
                .map_err(|e| Failure::api("uia_relation_element", e))?;
            relation.exact_element |= unsafe { automation.CompareElements(&element, destination) }
                .map_err(|e| Failure::api("uia_relation_identity", e))?
                .as_bool();
            relation.exact_hwnd |= destination_hwnd != 0
                && unsafe { element.CurrentNativeWindowHandle() }
                    .map_err(|e| Failure::api("uia_relation_hwnd", e))?
                    .0 as usize
                    == destination_hwnd;
            relation.examined += 1;
        }
        Ok(relation)
    })();
    match result {
        Ok(relation) => Ok(relation),
        Err(error) if error.status == "expired" => Err(error),
        Err(error) => Ok(Relation {
            error_code: error.error_code,
            ..Default::default()
        }),
    }
}

unsafe fn query_relations(
    automation: &IUIAutomation,
    state: NativeState,
    job: &Job,
) -> Result<Value, Failure> {
    check_job(job, false)?;
    let focus = unsafe { automation.GetFocusedElement() }
        .map_err(|e| Failure::api("uia_focused_element", e))?;
    let candidate = unsafe { automation.ElementFromHandle(window::hwnd(state.event.hwnd)) }
        .map_err(|e| Failure::api("uia_event_element", e))?;
    let candidate_pid =
        unsafe { candidate.CurrentProcessId() }.map_err(|e| Failure::api("uia_event_pid", e))?;
    let focus_pid =
        unsafe { focus.CurrentProcessId() }.map_err(|e| Failure::api("uia_focus_pid", e))?;
    if candidate_pid as u32 != state.event.pid || focus_pid as u32 != state.focus.pid {
        return Err(Failure::expired("uia_identity_mismatch"));
    }
    let candidate_hwnd = unsafe { candidate.CurrentNativeWindowHandle() }
        .map_err(|e| Failure::api("uia_event_hwnd", e))?
        .0 as usize;
    let focus_hwnd = unsafe { focus.CurrentNativeWindowHandle() }
        .map_err(|e| Failure::api("uia_focus_hwnd", e))?
        .0 as usize;
    let to_focus =
        unsafe { controller_for(automation, &candidate, &focus, state.focus.hwnd, job) }?;
    let to_candidate =
        unsafe { controller_for(automation, &focus, &candidate, state.event.hwnd, job) }?;
    let candidate_type = unsafe { candidate.CurrentControlType() }
        .ok()
        .map(|kind| kind.0);
    let focus_type = unsafe { focus.CurrentControlType() }
        .ok()
        .map(|kind| kind.0);
    check_job(job, false)?;
    let final_focus = unsafe { automation.GetFocusedElement() }
        .map_err(|e| Failure::api("uia_final_focus", e))?;
    if !unsafe { automation.CompareElements(&focus, &final_focus) }
        .map_err(|e| Failure::api("uia_final_focus_identity", e))?
        .as_bool()
    {
        return Err(Failure::expired("uia_focus_changed"));
    }
    check_job(job, false)?;
    let reported = to_focus.exact_element
        || to_focus.exact_hwnd
        || to_candidate.exact_element
        || to_candidate.exact_hwnd;
    Ok(json!({
        "status":if reported {"relationship_reported"} else {"unsupported"},
        "reason":if reported {"controller_for_relationship"} else {"no_explicit_controller_for_relationship"},
        "event_native_matches":candidate_hwnd == state.event.hwnd,
        "focus_native_matches":focus_hwnd != 0 && focus_hwnd == state.focus.hwnd,
        "event_control_type":candidate_type,"focus_control_type":focus_type,"uia_focus_stable":true,
        "candidate_to_focus":to_focus,"focus_to_candidate":to_candidate,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(hwnd: usize) -> Identity {
        Identity {
            hwnd,
            pid: 100,
            thread: 200,
        }
    }
    fn state() -> NativeState {
        NativeState {
            event: identity(30),
            panel: identity(10),
            target: Some(identity(20)),
            foreground: identity(10),
            focus: identity(11),
            default_ime: Some(identity(40)),
        }
    }

    #[test]
    fn ending_a_session_invalidates_queued_work_even_if_the_number_is_reused() {
        let first = Lifetime::new();
        let queued = first.0.clone();
        drop(first);
        let next = Lifetime::new();
        assert_eq!(
            stale_reason(state(), state(), queued.load(Ordering::Acquire)),
            Some("session_ended")
        );
        assert_eq!(
            stale_reason(state(), state(), next.0.load(Ordering::Acquire)),
            None
        );
    }

    #[test]
    fn recycled_handles_and_focus_changes_cannot_produce_valid_relationship_evidence() {
        let before = state();
        for changed_anchor in 0..3 {
            let mut after = before;
            let anchor = match changed_anchor {
                0 => &mut after.event,
                1 => &mut after.panel,
                _ => after.target.as_mut().unwrap(),
            };
            anchor.pid += 1;
            assert_eq!(
                stale_reason(before, after, true),
                Some("window_identity_changed")
            );
        }
        let mut after = before;
        after.event.thread += 1;
        assert_eq!(
            stale_reason(before, after, true),
            Some("window_identity_changed")
        );
        after = before;
        after.foreground = identity(99);
        assert_eq!(
            stale_reason(before, after, true),
            Some("foreground_changed")
        );
        after = before;
        after.focus = identity(12);
        assert_eq!(stale_reason(before, after, true), Some("focus_changed"));
        after = before;
        after.focus.pid += 1;
        assert_eq!(stale_reason(before, after, true), Some("focus_changed"));
        after = before;
        after.default_ime = None;
        assert_eq!(
            stale_reason(before, after, true),
            Some("default_ime_changed")
        );
    }

    #[test]
    fn internal_events_do_not_consume_the_external_probe_budget() {
        let budget = Budget::default();
        for _ in 0..100 {
            for hwnd in [0, 10, 20] {
                assert_eq!(budget.admit(hwnd, 10, 20), Admission::Ignore);
            }
        }
        for _ in 0..SESSION_LIMIT {
            assert_eq!(budget.admit(30, 10, 20), Admission::Probe);
        }
        assert_eq!(budget.admit(40, 10, 20), Admission::Limit);
        for _ in 0..100 {
            assert_eq!(budget.admit(50, 10, 20), Admission::Limit);
        }
        assert_eq!(Budget::default().admit(30, 10, 20), Admission::Probe);
    }

    #[test]
    fn a_stalled_or_closed_queue_rejects_without_waiting() {
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        for value in 0..QUEUE_CAPACITY {
            assert_eq!(enqueue(&sender, value), Ok(()));
        }
        assert_eq!(enqueue(&sender, 99), Err("queue_full"));
        assert_eq!(receiver.recv().unwrap(), 0);
        assert_eq!(enqueue(&sender, 100), Ok(()));
        drop(receiver);
        assert_eq!(enqueue(&sender, 101), Err("worker_stopped"));
    }

    #[test]
    fn delayed_work_and_results_expire_without_a_policy_exception() {
        assert_eq!(age_reason(MAX_QUEUE_DELAY, Duration::ZERO, true), None);
        assert_eq!(
            age_reason(
                MAX_QUEUE_DELAY + Duration::from_millis(1),
                Duration::ZERO,
                true
            ),
            Some("queue_expired")
        );
        assert_eq!(age_reason(MAX_OBSERVATION_AGE, Duration::ZERO, false), None);
        assert_eq!(
            age_reason(
                MAX_OBSERVATION_AGE + Duration::from_millis(1),
                Duration::ZERO,
                false
            ),
            Some("observation_expired")
        );
    }

    #[test]
    fn late_win_events_do_not_gain_a_fresh_observation_window() {
        assert_eq!(event_age(20, u32::MAX - 29), Duration::from_millis(50));
        assert_eq!(
            age_reason(Duration::ZERO, event_age(5000, 2000), true),
            Some("observation_expired")
        );
        // 送达时尚有效，但排队与查询时间必须继续累计进同一个事件期限。
        assert_eq!(
            age_reason(
                Duration::from_millis(200),
                Duration::from_millis(1400),
                true
            ),
            Some("observation_expired")
        );
        assert_eq!(
            age_reason(
                Duration::from_millis(200),
                Duration::from_millis(1300),
                false
            ),
            None
        );
    }

    #[test]
    fn rejection_summary_retains_every_reason_without_event_payloads() {
        let rejected = Rejections::default();
        assert!(!rejected.any());
        for reason in [
            "queue_full",
            "queue_full",
            "worker_stopped",
            "worker_unavailable",
            "session_limit",
        ] {
            rejected.record(reason);
        }
        assert_eq!(rejected.queue_full.get(), 2);
        assert_eq!(rejected.worker_stopped.get(), 1);
        assert_eq!(rejected.worker_unavailable.get(), 1);
        assert_eq!(rejected.session_limit.get(), 1);
    }
}
