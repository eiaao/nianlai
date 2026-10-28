// 仅显式授权的当前站准备诊断；普通测试不创建窗口、不访问剪贴板。
use super::*;
use crate::paste::native_access::{self, Environment, Probe, Publication};
use std::{
    cell::RefCell,
    sync::atomic::{AtomicU32, Ordering},
};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};

const AVAILABILITY_OPT_IN: &str = "M0_CURRENT_STATION_NATIVE_AVAILABILITY_V1";
const AVAILABILITY_OPT_IN_ENV: &str = "NIANLAI_CURRENT_STATION_NATIVE_AVAILABILITY_OPT_IN";
const AVAILABILITY_OUTPUT_ENV: &str = "NIANLAI_CURRENT_STATION_NATIVE_AVAILABILITY_OUTPUT";
const TEST: &str = "platform::windows::paste::current_station_native_clipboard_tests::availability::authorized_current_station_native_availability_diagnostic";

fn availability_opt_in(value: Option<&str>) -> Result<()> {
    if value == Some(AVAILABILITY_OPT_IN) {
        Ok(())
    } else {
        Err("缺少本轮native availability诊断的独立opt-in；不创建窗口或访问剪贴板".into())
    }
}

#[derive(Default)]
struct PasteCounts {
    entered: AtomicU32,
    completed: AtomicU32,
}
struct EditObserver {
    previous: WNDPROC,
    counts: Arc<PasteCounts>,
}
thread_local! {
    static EDIT_OBSERVER: RefCell<Option<EditObserver>> = const { RefCell::new(None) };
}

unsafe extern "system" fn counted_edit(
    hwnd: HWND,
    message: u32,
    wp: WPARAM,
    lp: LPARAM,
) -> LRESULT {
    // 只观察自有控件的消息次数；正文仍交给原系统EDIT过程，回调不访问剪贴板或写文件。
    let observer = EDIT_OBSERVER.with(|state| {
        state
            .borrow()
            .as_ref()
            .map(|observer| (observer.previous, observer.counts.clone()))
    });
    let Some((previous, counts)) = observer else {
        return unsafe { DefWindowProcW(hwnd, message, wp, lp) };
    };
    if message == WM_PASTE {
        counts.entered.fetch_add(1, Ordering::SeqCst);
    }
    let result = unsafe { CallWindowProcW(previous, hwnd, message, wp, lp) };
    if message == WM_PASTE {
        counts.completed.fetch_add(1, Ordering::SeqCst);
    }
    result
}

struct CountedEdit {
    edit: Edit,
    counts: Arc<PasteCounts>,
}
impl CountedEdit {
    fn new(guard: Guard) -> Result<Self> {
        let counts = Arc::new(PasteCounts::default());
        let observed = counts.clone();
        let edit = Edit::new_with_setup(guard, move |control| {
            unsafe { SetLastError(WIN32_ERROR(0)) };
            let previous = unsafe {
                SetWindowLongPtrW(control, GWLP_WNDPROC, counted_edit as *const () as isize)
            };
            if previous == 0 {
                return Err(format!("安装自有EDIT计数过程失败：{}", unsafe {
                    GetLastError().0
                }));
            }
            let previous = unsafe { std::mem::transmute::<isize, WNDPROC>(previous) };
            EDIT_OBSERVER.with(|state| {
                *state.borrow_mut() = Some(EditObserver {
                    previous,
                    counts: observed,
                });
            });
            Ok(())
        })?;
        Ok(Self { edit, counts })
    }
}

struct Holder {
    release: Option<mpsc::Sender<()>>,
    finished: mpsc::Receiver<Result<()>>,
    join: Option<thread::JoinHandle<()>>,
    hwnd: usize,
    thread: u32,
    release_requested: bool,
}
impl Holder {
    fn start(guard: &Guard, lease: Lease) -> Result<Self> {
        let (ready, receive) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let (finished, receive_finished) = mpsc::channel();
        let context = guard.clone();
        let join = thread::spawn(move || {
            let mut announced = false;
            let result = (|| -> Result<()> {
                let owner = Owner::new(&context)?;
                let hwnd = owner.verify(&context)?;
                unsafe { api(OpenClipboard(Some(hwnd)))? };
                let open = Open(true);
                lease.verify()?;
                // 此线程刻意不取得CaptureAccess；模拟进程内许可无法约束的独立OS持锁者。
                ready
                    .send(Ok((owner.hwnd, owner.thread)))
                    .map_err(|_| "诊断holder的ready接收端已关闭")?;
                announced = true;
                let released = wait
                    .recv_timeout(Duration::from_secs(3))
                    .map_err(|error| format!("诊断holder未收到释放握手：{error}"));
                let closed = open.close();
                released?;
                closed
            })();
            if !announced {
                let _ = ready.send(Err(result
                    .as_ref()
                    .err()
                    .cloned()
                    .unwrap_or_else(|| "诊断holder未报告ready".into())));
            }
            let _ = finished.send(result);
        });
        let mut holder = Self {
            release: Some(release),
            finished: receive_finished,
            join: Some(join),
            hwnd: 0,
            thread: 0,
            release_requested: false,
        };
        let (hwnd, thread) = receive
            .recv_timeout(Duration::from_secs(3))
            .map_err(|error| error.to_string())??;
        holder.hwnd = hwnd;
        holder.thread = thread;
        if thread == unsafe { GetCurrentThreadId() } || hwnd == lease.owner {
            return Err("holder不是独立自有HWND/线程".into());
        }
        guard.own(window::hwnd(hwnd), unsafe { GetCurrentProcessId() }, thread)?;
        Ok(holder)
    }
    fn request_release(&mut self) -> Result<()> {
        if let Some(release) = self.release.take() {
            release
                .send(())
                .map_err(|_| "诊断holder的释放接收端已关闭")?;
            self.release_requested = true;
        }
        Ok(())
    }
    fn finish(&mut self) -> Result<()> {
        let Some(join) = self.join.take() else {
            return Ok(());
        };
        // 包括失败出口：先释放，再收取Close结果，最后join；外层30s watchdog覆盖清理。
        let release = self.request_release();
        let finished = self
            .finished
            .recv_timeout(Duration::from_secs(3))
            .map_err(|error| format!("诊断holder清理超时：{error}"));
        let joined = join.join().map_err(|_| "诊断holder线程异常");
        release?;
        finished??;
        joined.map_err(str::to_owned)
    }
}
impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scenario {
    Clean,
    ReleaseAfterBusy,
    PersistentBusy,
    Changed,
}
impl Scenario {
    fn name(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::ReleaseAfterBusy => "busy-then-release",
            Self::PersistentBusy => "persistent-busy",
            Self::Changed => "publication-changed",
        }
    }
    fn expected_reason(self) -> Option<&'static str> {
        match self {
            Self::Clean | Self::ReleaseAfterBusy => None,
            Self::PersistentBusy => Some(native_access::BUSY),
            Self::Changed => Some(native_access::CHANGED),
        }
    }
}

#[derive(serde::Serialize)]
struct Timing {
    stage: &'static str,
    started_ms: f64,
    finished_ms: f64,
    status: &'static str,
}
impl Timing {
    fn new(stage: &'static str, start: Duration, end: Duration, status: &'static str) -> Self {
        Self {
            stage,
            started_ms: start.as_secs_f64() * 1000.0,
            finished_ms: end.as_secs_f64() * 1000.0,
            status,
        }
    }
}

struct Timed<E> {
    inner: E,
    operations: Vec<Timing>,
}
impl<E> Timed<E> {
    fn new(inner: E) -> Self {
        // 200ms / 5ms 条件轮询；热路径仅记录内存，不构造 JSON 或写文件。
        Self {
            inner,
            operations: Vec::with_capacity(128),
        }
    }
}
impl<E: Environment> Environment for Timed<E> {
    fn now(&self) -> Duration {
        self.inner.now()
    }
    fn pause(&mut self, duration: Duration) {
        let start = self.now();
        self.inner.pause(duration);
        self.operations
            .push(Timing::new("pause", start, self.now(), "returned"));
    }
    fn guard(&mut self) -> std::result::Result<(), &'static str> {
        let start = self.now();
        let result = self.inner.guard();
        self.operations.push(Timing::new(
            "guard",
            start,
            self.now(),
            result.err().unwrap_or("ready"),
        ));
        result
    }
    fn probe(&mut self, publication: Publication) -> Probe {
        let start = self.now();
        let result = self.inner.probe(publication);
        let status = match result {
            Probe::Ready => "ready",
            Probe::Busy => "busy",
            Probe::Changed => "changed",
            Probe::CloseFailed => "close_failed",
        };
        self.operations
            .push(Timing::new("probe", start, self.now(), status));
        result
    }
}

struct PreparationReport {
    budget: Option<native_access::Budget>,
    final_check: std::result::Result<(), &'static str>,
    elapsed: Option<Duration>,
    preflight_error: Option<String>,
    stages: Vec<Timing>,
}

fn prepare_case(
    preflight: impl FnOnce() -> Result<()>,
    environment: &mut impl Environment,
    publication: Publication,
) -> PreparationReport {
    let mut stages = Vec::with_capacity(3);
    let start = environment.now();
    let preflight = preflight();
    stages.push(Timing::new(
        "preflight",
        start,
        environment.now(),
        if preflight.is_ok() { "ready" } else { "failed" },
    ));
    if let Err(error) = preflight {
        return PreparationReport {
            budget: None,
            final_check: Err("diagnostic_preflight_failed"),
            elapsed: None,
            preflight_error: Some(error),
            stages,
        };
    }
    // 完整检查耗时不属于可用性预算；之后继续使用原生产 Budget 的真实单调时间。
    let mut budget = native_access::Budget::new(environment.now());
    let start = environment.now();
    let prepare = budget.wait(environment, Some(publication));
    stages.push(Timing::new(
        "prepare",
        start,
        environment.now(),
        prepare.err().unwrap_or("ready"),
    ));
    let final_check = prepare.and_then(|()| {
        let start = environment.now();
        let result = budget.wait(environment, Some(publication));
        stages.push(Timing::new(
            "final",
            start,
            environment.now(),
            result.err().unwrap_or("ready"),
        ));
        result
    });
    let elapsed = Some(budget.elapsed(environment.now()));
    PreparationReport {
        budget: Some(budget),
        final_check,
        elapsed,
        preflight_error: None,
        stages,
    }
}

fn submit_prepared<T>(
    result: std::result::Result<(), &'static str>,
    submit: impl FnOnce() -> T,
) -> Option<T> {
    result.ok().map(|()| submit())
}

struct Preparation<'a> {
    guard: &'a Guard,
    edit: &'a Edit,
    clock: Instant,
    holder: Option<&'a mut Holder>,
    release_after_busy: bool,
    busy_probes: u32,
    guard_calls: u32,
    holder_error: Option<String>,
}
impl Environment for Preparation<'_> {
    fn now(&self) -> Duration {
        self.clock.elapsed()
    }
    fn pause(&mut self, duration: Duration) {
        // 只有生产Budget的真实Busy分支才调用；不按预设时长释放holder。
        thread::sleep(duration);
    }
    fn guard(&mut self) -> std::result::Result<(), &'static str> {
        self.guard_calls += 1;
        if self.holder_error.is_some() {
            return Err("diagnostic_holder_failed");
        }
        availability_opt_in(std::env::var(AVAILABILITY_OPT_IN_ENV).ok().as_deref())
            .and_then(|()| {
                self.guard.own_live(
                    window::hwnd(self.edit.control),
                    unsafe { GetCurrentProcessId() },
                    self.edit.thread,
                )
            })
            .map_err(|error| {
                self.holder_error = Some(error);
                "diagnostic_guard_failed"
            })
    }
    fn probe(&mut self, publication: Publication) -> Probe {
        let result = clipboard::probe_native(publication);
        if result == Probe::Busy {
            self.busy_probes += 1;
            if self.release_after_busy
                && let Some(holder) = self.holder.as_mut()
                && let Err(error) = holder.request_release()
            {
                self.holder_error = Some(error);
            }
        }
        result
    }
}

fn availability_case(
    guard: &Guard,
    directory: &Path,
    publication: Publication,
    current: Lease,
    scenario: Scenario,
) -> Result<()> {
    guard.verify_thread(unsafe { GetCurrentThreadId() })?;
    current.verify()?;
    let target = CountedEdit::new(guard.clone())?;
    let before = target.edit.snapshot(guard)?;
    if before != digest("")
        || target.counts.entered.load(Ordering::SeqCst) != 0
        || target.counts.completed.load(Ordering::SeqCst) != 0
    {
        return Err("本例自有EDIT并非空白或已收到WM_PASTE".into());
    }
    let mut holder = if matches!(
        scenario,
        Scenario::ReleaseAfterBusy | Scenario::PersistentBusy
    ) {
        Some(Holder::start(guard, current)?)
    } else {
        None
    };
    let (report, operations, busy_probes, guard_calls, guard_error) = {
        let mut environment = Timed::new(Preparation {
            guard,
            edit: &target.edit,
            clock: Instant::now(),
            holder: holder.as_mut(),
            release_after_busy: scenario == Scenario::ReleaseAfterBusy,
            busy_probes: 0,
            guard_calls: 0,
            holder_error: None,
        });
        let report = prepare_case(
            || {
                availability_opt_in(std::env::var(AVAILABILITY_OPT_IN_ENV).ok().as_deref())?;
                guard.own(
                    window::hwnd(target.edit.control),
                    unsafe { GetCurrentProcessId() },
                    target.edit.thread,
                )?;
                current.verify()
            },
            &mut environment,
            publication,
        );
        (
            report,
            environment.operations,
            environment.inner.busy_probes,
            environment.inner.guard_calls,
            environment.inner.holder_error,
        )
    };
    let final_check = report.final_check;
    let elapsed = report.elapsed.unwrap_or_default();
    let probes = report
        .budget
        .as_ref()
        .map_or(0, native_access::Budget::probes);
    let waits = report
        .budget
        .as_ref()
        .map_or(0, native_access::Budget::waits);
    // 只有两个准备阶段都成功才允许调用一次生产wrapper；失败、超时后均无补发。
    let delivery = submit_prepared(final_check, || {
        submit_native_message(
            target.edit.control,
            target.edit.thread,
            unsafe { GetCurrentProcessId() },
            200,
        )
    });
    let release_requested_before_cleanup = holder
        .as_ref()
        .is_some_and(|holder| holder.release_requested);
    // 持续Busy的释放发生在Budget返回之后，不能在测试时钟到点前提前放行。
    let holder_cleanup = holder.as_mut().map_or(Ok(()), Holder::finish);
    // 先保存阶段证据，再做可能失败的快照；末次探针到唯一发送之间不序列化/写文件。
    save(
        directory,
        &format!("{}-timing.json", scenario.name()),
        &json!({
            "case":scenario.name(),"product_absence_scope":"per_case_preflight",
            "clock":"monotonic_ms","intervals_overlap_do_not_sum":true,
            "stages":report.stages,"operations":operations,"preflight_error":report.preflight_error,
            "guard_error":guard_error,"preparation_reason":final_check.err(),
            "budget_started":report.budget.is_some(),"budget_ms":native_access::BUDGET.as_millis() as u64,
            "elapsed_ms":report.elapsed.map(|elapsed| elapsed.as_secs_f64()*1000.0),
            "deadline_exhausted":report.elapsed.is_some_and(|elapsed| elapsed >= native_access::BUDGET),
            "real_busy_probes":busy_probes,"submission_calls":u32::from(delivery.is_some()),
            "holder_cleanup_error":holder_cleanup.as_ref().err(),
        }),
    )?;
    let after = target.edit.snapshot(guard)?;
    let received = target.counts.entered.load(Ordering::SeqCst);
    let completed = target.counts.completed.load(Ordering::SeqCst);
    let expected_submissions = u32::from(scenario.expected_reason().is_none());
    let expected = digest(if expected_submissions == 1 {
        SENTINEL
    } else {
        ""
    });
    let lease_result = current.verify();
    let reason = final_check.err();
    let passed = reason == scenario.expected_reason()
        && guard_error.is_none()
        && holder_cleanup.is_ok()
        && lease_result.is_ok()
        && received == expected_submissions
        && completed == expected_submissions
        && after == expected
        && delivery.is_some() == (expected_submissions == 1)
        && delivery.is_none_or(|delivery| delivery.attempted && delivery.acknowledged)
        && match scenario {
            Scenario::Clean => waits == 0 && busy_probes == 0,
            Scenario::ReleaseAfterBusy => {
                busy_probes > 0 && release_requested_before_cleanup && waits > 0
            }
            Scenario::PersistentBusy => {
                busy_probes > 0
                    && !release_requested_before_cleanup
                    && elapsed >= native_access::BUDGET
            }
            Scenario::Changed => waits == 0,
        };
    save(
        directory,
        &format!("{}.json", scenario.name()),
        &json!({
            "case":scenario.name(),"passed":passed,"before":before,"after":after,"expected":expected,
            "preparation_reason":reason,"expected_reason":scenario.expected_reason(),
            "probes":probes,"waits":waits,"real_busy_probes":busy_probes,
            "guard_calls":guard_calls,"guard_error":guard_error,
            "elapsed_ms":elapsed.as_secs_f64()*1000.0,"budget_ms":native_access::BUDGET.as_millis() as u64,
            "one_budget_for_prepare_and_final":true,"product_absence_scope":"per_case_preflight",
            "deadline_exhausted":report.elapsed.is_some_and(|elapsed| elapsed >= native_access::BUDGET),
            "submission_calls":u32::from(delivery.is_some()),
            "expected_submissions":expected_submissions,"received_wm_paste":received,"completed_wm_paste":completed,
            "attempted":delivery.is_some_and(|delivery| delivery.attempted),
            "acknowledged":delivery.is_some_and(|delivery| delivery.acknowledged),
            "error_code":delivery.and_then(|delivery| delivery.error_code),
            "holder_same_process":holder.is_some(),"holder_outside_capture_access":holder.is_some(),
            "release_requested_before_cleanup":release_requested_before_cleanup,
            "holder_cleanup_error":holder_cleanup.err(),"current_lease_error":lease_result.err(),
            "clipboard_body_read_by_probe":false,"input_events_sent":0,"focus_requested":false,
            "target_hidden":true,"no_retry":true
        }),
    )?;
    if passed {
        Ok(())
    } else {
        Err(format!(
            "{}诊断未满足预期；停止后续用例，无补发",
            scenario.name()
        ))
    }
}

fn availability_diagnostic(guard: &Guard, directory: &Path) -> Result<()> {
    let reader = Owner::new(guard)?;
    let (service, production, observation) = production_write_observed(guard, directory)?;
    let (publication, publication_error) = {
        let observation = observation
            .lock()
            .map_err(|_| "生产writer observation已poison")?;
        (observation.publication, observation.publication_error)
    };
    let publication = publication
        .ok_or_else(|| format!("生产writer没有有效publication：{publication_error:?}；不提交"))?;
    save(
        directory,
        "production-clipboard-read.json",
        &read_own_sample(guard, &reader, production)?,
    )?;
    let access = service
        .0
        .capture_access()
        .native(native_access::BUDGET, || false)
        .map_err(|error| error.reason().to_owned())?;
    for scenario in [
        Scenario::Clean,
        Scenario::ReleaseAfterBusy,
        Scenario::PersistentBusy,
    ] {
        availability_case(guard, directory, publication, production, scenario)?;
    }
    // 最后才替换。minimal_write在OS锁内核对旧lease，外部意外替换时绝不Empty/覆盖。
    let replacement = minimal_write(guard, &reader, production)?;
    availability_case(
        guard,
        directory,
        publication,
        replacement,
        Scenario::Changed,
    )?;
    lease_final(guard, replacement)?;
    drop(access);
    save(
        directory,
        "case-summary.json",
        &json!({
            "passed":true,"production_writer":true,"production_probe":true,"production_budget":true,
            "cases":["clean","busy-then-release","persistent-busy","publication-changed"],
            "capture_access_held":true,"holder_acquires_capture_access":false,
            "replacement_after_other_cases":true,"replacement_verified_old_lease_under_lock":true,
            "no_retry":true,"other_process_classification_verified":false,"physical_c2_verified":false,
            "limitations":["自有持锁线程属于同进程，不验证other_process分类",
                "隐藏自有EDIT，不验证生产可见目标资格、选择许可、IME或物理C2",
                "末次Close到目标Open之间仍有TOCTOU，本诊断不证明原子交付"]
        }),
    )
}

#[test]
fn native_availability_opt_in_requires_new_exact_token() {
    for value in [
        None,
        Some(""),
        Some(OPT_IN),
        Some("true"),
        Some("M0_CURRENT_STATION_NATIVE_AVAILABILITY_V1 "),
    ] {
        assert!(availability_opt_in(value).is_err());
    }
    assert!(availability_opt_in(Some(AVAILABILITY_OPT_IN)).is_ok());
}

#[path = "current_station_native_availability_guard_tests.rs"]
mod guard_tests;

#[test]
#[ignore = "必须新获本轮空档授权、双固定opt-in、无nianlai.exe后--exact显式运行；会写固定测试剪贴板"]
fn authorized_current_station_native_availability_diagnostic() {
    with_watchdog(|| {
        availability_opt_in(std::env::var(AVAILABILITY_OPT_IN_ENV).ok().as_deref())?;
        let guard = Guard::enter()?;
        let base = PathBuf::from(
            std::env::var_os(AVAILABILITY_OUTPUT_ENV).ok_or("必须提供独立诊断输出目录")?,
        );
        let directory = base.join(format!(
            "native-availability-run-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        ));
        fs::create_dir_all(&base).map_err(|error| error.to_string())?;
        fs::create_dir(&directory).map_err(|error| error.to_string())?;
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        save(
            &directory,
            "source.json",
            &json!({
                "schema":1,"test":TEST,"execution_mode":"authorized_native_availability_diagnostic",
                "authorization_record":"父端仅在用户明确本轮空档授权后设置双固定opt-in并--exact运行；授权原文及时间由父端另存",
                "executable":executable,
                "executable_sha256":hex(&hash::sha256(&fs::read(&executable).map_err(|error|error.to_string())?).unwrap()),
                "station":"WinSta0","desktop":guard.desktop,"watchdog_ms":30000,
                "original_clipboard_read":false,"foreground_or_input_operations":false,
                "other_process_classification_verified":false,"physical_c2_verified":false
            }),
        )?;
        let result = availability_diagnostic(&guard, &directory);
        save(
            &directory,
            "result.json",
            &json!({
                "passed":result.is_ok(),"error":result.as_ref().err(),"no_retry":true,
                "original_clipboard_read":false,"physical_c2_verified":false
            }),
        )?;
        println!("authorized_native_availability_evidence={}", directory.display());
        result
    })
    .unwrap();
}
