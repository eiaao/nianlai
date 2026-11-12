use crate::{
    app::Controller,
    autocycle,
    history::History,
    log::{Logger, now_ms},
    paste::{
        keys::Table,
        outcome::{Outcome, PasteResult, Reason},
        permit::{Monitor, Permit, Selection, Session as PermitSession, reliable_control},
        queue::{Demand, Finished, Guard, State as QueueState, Stop},
        request::{Callbacks, MODIFIER_GRACE, Mode as PasteMode, Request, RestorePolicy},
    },
    platform::{self, HintEnd, HookEvent, Hooks, REPLAY_WAIT, Target, ime, paste, webview, window},
    resuspend,
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};
use tauri_plugin_global_shortcut::Shortcut;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Hidden,
    Quick,
    Search,
    Queue,
    Auto,
}

#[derive(Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum Interaction {
    Quick,
    ActivationPending,
    ActivationFailed,
    Activated,
    Passive,
    CloseOnly,
}

impl Interaction {
    fn may_restore(self) -> bool {
        !matches!(self, Self::Passive | Self::CloseOnly)
    }
}

#[derive(Debug, PartialEq)]
enum NoSubmissionFinish {
    CloseRequested,
    CloseOnly,
    ShowCloseOnly,
    Ignore,
}

fn generic_submission_finish(
    result: &Outcome,
    same_session: bool,
    visible: bool,
    interaction: Interaction,
) -> Option<NoSubmissionFinish> {
    if !matches!(
        result.result,
        Some(PasteResult::GenericNotSubmitted | PasteResult::GenericSubmissionUnknown)
    ) {
        return None;
    }
    Some(if !same_session || (result.request_cancelled && !visible) {
        NoSubmissionFinish::Ignore
    } else if result.request_cancelled {
        if interaction == Interaction::Passive {
            NoSubmissionFinish::CloseRequested
        } else {
            NoSubmissionFinish::Ignore
        }
    } else if visible {
        NoSubmissionFinish::CloseOnly
    } else {
        // 通用路径先隐藏后发送；对同一请求的未提交/部分发送必须真正显示终止反馈。
        NoSubmissionFinish::ShowCloseOnly
    })
}

fn no_submission_finish(
    result: &Outcome,
    current: bool,
    interaction: Interaction,
) -> Option<NoSubmissionFinish> {
    let native = result.result == Some(PasteResult::NativeNotSubmitted);
    if !current {
        return native.then_some(NoSubmissionFinish::Ignore);
    }
    if !native && result.clipboard_written {
        return None;
    }
    if result.request_cancelled {
        return if interaction == Interaction::Passive {
            Some(NoSubmissionFinish::CloseRequested)
        } else {
            native.then_some(NoSubmissionFinish::Ignore)
        };
    }
    (native || result.reason == Some(Reason::ClipboardCaptureBusy))
        .then_some(NoSubmissionFinish::CloseOnly)
}

fn no_submission_message(reason: Option<&Reason>) -> &'static str {
    match reason {
        Some(Reason::GenericSubmissionUnknown) => "粘贴结果未确认，请先检查目标，避免重复粘贴",
        Some(Reason::NativeClipboardChanged | Reason::GenericClipboardChanged) => {
            "剪贴板已变化，本次未粘贴，请关闭面板后重新呼出"
        }
        Some(
            Reason::ClipboardCaptureBusy
            | Reason::ClipboardBusy
            | Reason::NativeClipboardBusy
            | Reason::NativeClipboardCloseFailed
            | Reason::GenericClipboardBusy
            | Reason::GenericClipboardCloseFailed,
        ) => "剪贴板忙，本次未粘贴，请关闭面板后重新呼出",
        Some(Reason::RecordMissing) => "这条记录已不在历史里，本次未粘贴，请关闭面板后重新呼出",
        Some(Reason::FilesMissing) => "原文件不在了，没有粘贴",
        Some(
            Reason::RequestCancelled
            | Reason::NativeInputDesktopUnavailable
            | Reason::GenericInputDesktopUnavailable
            | Reason::InvalidTarget
            | Reason::SelectionBoundaryChanged
            | Reason::NativeModifiersHeld
            | Reason::GenericModifiersHeld
            | Reason::TargetMayBeElevated
            | Reason::NativeTargetUnavailable
            | Reason::GenericRestoreUnavailable
            | Reason::ConditionsChanged
            | Reason::PreflightTimeout,
        ) => "窗口状态已变化，本次未粘贴，请关闭面板后重新呼出",
        _ => "本次未粘贴，请关闭面板后重新呼出",
    }
}

enum ShowSource {
    Hotkey { target: Target, alt: bool },
    Auto { cycle: usize, hidden_since: Instant },
}

#[derive(Clone, Copy)]
struct AutoVisible {
    cycle: usize,
    hidden_ms: f64,
}

/// 呼出位置每次呼出时现读设置，保存后下次呼出就按新位置。
fn placement_mode(app: &AppHandle) -> platform::PlacementMode {
    let store = app.state::<crate::settings::Store>();
    platform::PlacementMode::from_setting(&store.get().interaction.placement)
}

// 日志行共有的会话标记。短命探针线程不持有 Panel，抄走 session 和 target 后用同一个函数盖章。
fn stamp(mut fields: Value, session: u64, target: &Target) -> Value {
    fields["session"] = json!(session);
    fields["process_name"] = json!(target.process_name);
    fields["focus_class"] = json!(target.focus_class);
    fields["focus_unknown"] = json!(target.focus == 0);
    fields
}

// 目标程序可能以管理员身份运行时，粘贴只会降级成"只复制"，面板停留这么久把提示给用户看完再自动收起。
// 来源：M0-6（10-24 夜实测，面板在结论出来前约 36 ms 就收起了，"目标程序以管理员身份运行"的提示要
// 下次呼出才看得到）。现为 3 秒被动提示，可点关闭或使用呼出键；不承诺目标下的按键立即关闭提示。
pub const ELEVATED_HINT_DWELL: Duration = Duration::from_secs(3);

// 管理员提示的停留状态。预判要停留时先占位（dwell 为 None，等粘贴结论），结论确认后才开始计时。
struct Hint {
    since: Instant,
    dwell: Option<Duration>,
    // 停留是怎么结束的（M0-6b）：收起前由各收起路径记下，写进 elevated_hint_hidden 的 ended_by。
    ended_by: Option<&'static str>,
    reason: &'static str,
}

// 粘贴前的预判：面板要不要先别收起。判断顺序和 paste::perform 一致——复制模式、目标失效、用户切走
// 都排在完整性比较前面，这些情况照旧立即收起；只有走到完整性比较且目标不是 allowed（比样机高，或查不到、
// 按高处理）时，结论必然是 copied_only / target_may_be_elevated，此时面板留着等结论。
fn defers_hide_for_elevated(
    mode: PasteMode,
    target_valid: bool,
    external_switch: bool,
    integrity: &Value,
) -> bool {
    mode != PasteMode::Copy && target_valid && !external_switch && integrity["status"] != "allowed"
}

// 已复制且需要被动反馈，或旧路径返回“目标可能是管理员”时停留；取消请求和其他粘贴结论不停留。
fn dwell_after_paste(result: &Outcome) -> Option<Duration> {
    if result.request_cancelled {
        return None;
    }
    ((result.passive_hint && result.clipboard_written)
        || (result.result == Some(PasteResult::CopiedOnly)
            && result.reason == Some(Reason::TargetMayBeElevated)))
    .then_some(ELEVATED_HINT_DWELL)
}

// 停留结束的日志字段：计划时长、实际停留时长，以及是否到点自动收起；提前结束来源单独记录。
fn hint_hidden_fields(dwell: Duration, shown: Duration) -> Value {
    json!({
        "reason":"target_may_be_elevated",
        "dwell_ms":dwell.as_millis() as u64,
        "shown_ms":shown.as_secs_f64() * 1000.0,
        "timed_out":shown >= dwell,
    })
}

// 上面的字段再加结束原因 ended_by（M0-6b）：escape / hotkey / key_passthrough / key_consumed / outside_click /
// timeout，另有 frontend（前端 hide_panel）、quit；没记下原因的收起（例如出错后的兜底收起）为 null。不记按的是哪个键。
fn hint_ended_fields(dwell: Duration, shown: Duration, ended_by: Option<&str>) -> Value {
    let mut fields = hint_hidden_fields(dwell, shown);
    fields["ended_by"] = json!(ended_by);
    fields
}

// 停留被钩子报来的按键结束时记的收起来源：Esc 沿用快速模式原来的 escape_key，其他键是新来源。
fn hint_key_hide_source(end: HintEnd) -> &'static str {
    match end {
        HintEnd::Escape => "escape_key",
        HintEnd::KeyPassthrough | HintEnd::KeyConsumed => "elevated_hint_key",
    }
}

// 先发生的关闭原因保持不变（例如超时后等到松键），明确退出始终优先。
fn hint_end_reason(previous: Option<&'static str>, next: &'static str) -> &'static str {
    if next == "quit" {
        next
    } else {
        previous.unwrap_or(next)
    }
}

fn hint_needs_key_drain(dwelling: bool, ended_by: Option<&str>) -> bool {
    dwelling && ended_by != Some("quit")
}

// 两条调用都必须执行：即使 WebView 停用或挂起报错，也不能把透明原生窗口留在屏幕上。
fn hide_webview_then_native(
    hide_webview: impl FnOnce() -> platform::Result<()>,
    hide_native: impl FnOnce() -> platform::Result<()>,
) -> platform::Result<()> {
    let webview = hide_webview();
    let native = hide_native();
    webview.and(native)
}

struct PasteQueue {
    state: QueueState,
    mode: PasteMode,
    generation: u64,
    guard: Arc<Guard>,
    hooks: Option<platform::queue_hook::QueueHooks>,
    clipboard: platform::clipboard::Service,
}

// 只有非空且与上次不同的避让外形才值得重新判断位置（纯函数）：返回要采用的新外形。
// 没有 avoid（拖牌时上报整窗区域、旧调用）一律 None：绝不因此挪窗口。
fn avoid_update(
    last: Option<window::NativeRegion>,
    incoming: Option<window::NativeRegion>,
) -> Option<window::NativeRegion> {
    incoming.filter(|next| last != Some(*next))
}

// 队列异常停止后小指示带着错误停留多久。
const QUEUE_ERROR_DWELL: Duration = Duration::from_secs(3);

// 有错误说明的停止（超时、外部复制、管理员目标、结果未确认、写入失败、单张超时、
// 跳过后已无可粘贴……）停留；放完、取消、Esc、× 没有说明，立即收起。
fn queue_stop_dwells(error: Option<&'static str>) -> bool {
    error.is_some()
}

// 内容变化后挪窗口让开光标行：约 120ms、8 步缓出，全部在主线程 SetWindowPos（不激活、不改层次）。
const GLIDE_DURATION: Duration = Duration::from_millis(120);
const GLIDE_STEP: Duration = Duration::from_millis(15);

struct Glide {
    serial: u64,
    from: (i32, i32),
    to: (i32, i32),
    started: Instant,
}

// 缓出插值（纯函数）：t∈[0,1]，终点精确落在 to。
fn glide_position(from: (i32, i32), to: (i32, i32), t: f64) -> (i32, i32) {
    let t = t.clamp(0.0, 1.0);
    let eased = 1.0 - (1.0 - t).powi(3);
    let lerp = |a: i32, b: i32| a + ((b - a) as f64 * eased).round() as i32;
    (lerp(from.0, to.0), lerp(from.1, to.1))
}

pub(crate) struct Panel {
    window: WebviewWindow,
    mode: Mode,
    ready: bool,
    session: u64,
    target: Target,
    hotkey: Option<Shortcut>,
    hooks: Option<Hooks>,
    interaction: Interaction,
    monitor: Option<Arc<Monitor>>,
    permits: Option<PermitSession>,
    started: Option<Instant>,
    shown: Option<Instant>,
    painted: bool,
    foreground_taken: bool,
    awaiting_search: bool,
    memory: Arc<webview::MemoryState>,
    pasting: bool,
    paste_cancel: Option<Arc<AtomicBool>>,
    queue: Option<PasteQueue>,
    // 呼出时的摆放（光标行、工作区、内容包围盒避让结果）；前端上报新内容区域时据此判断要不要挪。
    layout: Option<window::Layout>,
    // 正在进行的平滑挪动。
    glide: Option<Glide>,
    // 前端最近一次上报（或呼出时默认）的避让外形，客户区物理像素；只有变了才考虑挪窗口。
    avoid: Option<window::NativeRegion>,
    // 队列异常停止后小指示停留显示错误：Some(序号)。
    queue_dwell: Option<u64>,
    dwell_serial: u64,
    // 管理员提示：粘贴预判为"目标可能是管理员"后面板不收起，这里记着停留状态；None 表示没在停留。
    hint: Option<Hint>,
    auto_enabled: bool,
    auto_plan: Option<autocycle::Plan>,
    auto_stop: Option<mpsc::Sender<()>>,
    auto_visible: Option<AutoVisible>,
    resuspend: Option<Duration>,
    resuspend_stop: Option<mpsc::Sender<()>>,
}

pub enum Action {
    HistoryChanged,
    Toggle {
        started: Instant,
        target: Target,
        alt: bool,
    },
    AutoShow {
        cycle: usize,
        hidden_since: Instant,
        triggered: Instant,
        reply: mpsc::Sender<platform::Result<(u64, Instant)>>,
    },
    AutoHide {
        cycle: usize,
        session: u64,
        reply: mpsc::Sender<platform::Result<Instant>>,
    },
    AutoDone {
        cycles: usize,
    },
    Resuspend {
        interval: Duration,
        reply: mpsc::Sender<(u64, u64)>,
    },
    FrontendReady,
    Key {
        session: u64,
        key: &'static str,
    },
    Search {
        session: u64,
        started: Option<Instant>,
    },
    SearchActivated {
        session: u64,
        success: bool,
    },
    SearchReady {
        session: u64,
    },
    RetryActivation {
        session: u64,
    },
    ForegroundRevision {
        session: u64,
        revision: u64,
    },
    ReturnForPaste {
        session: u64,
        permit: Option<Permit>,
        passive: bool,
        policy: RestorePolicy,
        requested: Instant,
        reply: mpsc::SyncSender<platform::FocusResult>,
    },
    SearchTimeout {
        session: u64,
    },
    Hide {
        session: u64,
    },
    // 钩子报告点了面板外（原来走 Hide；单列出来只为停留结束时记 ended_by=outside_click，收起逻辑相同）。
    OutsideClick {
        session: u64,
    },
    // 管理员提示停留期间钩子报来的按键要收起面板（M0-6b）。
    HintKey {
        session: u64,
        end: HintEnd,
    },
    Paste {
        session: u64,
        id: usize,
        mode: PasteMode,
        selection: Option<Selection>,
        started: Instant,
    },
    // 粘贴键已发出：证据还在后台收集，此时就允许再次呼出。
    PasteSent {
        session: u64,
    },
    PasteFinished {
        session: u64,
        result: Outcome,
    },
    StartQueue {
        session: u64,
        ids: Vec<usize>,
        automatic: bool,
        mode: PasteMode,
        selection: Option<Selection>,
    },
    QueuePaste {
        session: u64,
        generation: u64,
    },
    QueueTick {
        session: u64,
        generation: u64,
    },
    QueueClipboard {
        session: u64,
        generation: u64,
    },
    QueueCancel {
        session: u64,
    },
    // 异常停止后的错误停留到点。
    QueueDwellEnd {
        session: u64,
        serial: u64,
    },
    QueueFinished {
        session: u64,
        generation: u64,
        id: usize,
        result: Outcome,
    },
    // 前端上报内容区域（CSS 像素）。session 可省（旧调用），给出时必须是当前会话。
    SetHitRegions {
        session: Option<u64>,
        regions: Vec<platform::HitRegion>,
        armed: bool,
        avoid: Option<window::Avoid>,
    },
    Glide {
        session: u64,
        serial: u64,
    },
    // 管理员提示停留到点：计时线程发回，面板自动收起。
    HintElapsed {
        session: u64,
    },
    Painted {
        session: u64,
        received: Instant,
    },
    Quit,
    // 恢复快照等需要重新打开数据库的操作：按退出走完清理，再由守护者立刻拉起。
    Relaunch,
}

pub fn dispatch(app: &AppHandle, action: Action) -> Result<(), String> {
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let controller = handle.state::<Controller>();
        if matches!(action, Action::Toggle { .. }) {
            // 刚复制、还在暂存期（A11）的内容先写入，呼出后列表里就有它。
            // 必须在拿面板锁之前：写入后的刷新通知会在主线程上同步再进 dispatch。
            controller.history.flush_deferred();
        }
        let mut panel = controller.inner.lock().unwrap();
        if let Err(error) = panel.act(&handle, &controller.logger, action) {
            panel.log(&controller.logger, "panel_error", json!({"error":error}));
            panel.emit(
                "panel:error",
                json!({"message":"操作未完成，请查看 G0 日志。"}),
            );
            if panel.mode != Mode::Hidden {
                let _ = panel.hide(&controller.logger);
            }
        }
    })
    .map_err(|e| e.to_string())
}

fn return_for_paste(
    app: &AppHandle,
    session: u64,
    permit: Option<Permit>,
    passive: bool,
    policy: RestorePolicy,
) -> Option<platform::FocusResult> {
    let (reply, receiver) = mpsc::sync_channel(1);
    dispatch(
        app,
        Action::ReturnForPaste {
            session,
            permit,
            passive,
            policy,
            requested: Instant::now(),
            reply,
        },
    )
    .ok()?;
    receiver.recv_timeout(REPLAY_WAIT).ok()
}

// Action::ReturnForPaste 的内容，交给 Panel::return_focus。
struct ReturnRequest {
    session: u64,
    permit: Option<Permit>,
    passive: bool,
    policy: RestorePolicy,
    requested: Instant,
    reply: mpsc::SyncSender<platform::FocusResult>,
}

// Action::Paste 的内容，交给 Panel::begin_paste。
struct PasteCommand {
    session: u64,
    id: usize,
    mode: PasteMode,
    selection: Option<Selection>,
    started: Instant,
}

// Action::StartQueue 的内容，交给 Panel::start_queue。
struct StartQueueCommand {
    session: u64,
    ids: Vec<usize>,
    automatic: bool,
    mode: PasteMode,
    selection: Option<Selection>,
}

// 粘贴工作线程需要的全部东西，由 begin_paste 在主线程上备好后整体移交给 run_paste。
struct PasteJob {
    app: AppHandle,
    logger: Logger,
    session: u64,
    id: usize,
    mode: PasteMode,
    started: Instant,
    owner: usize,
    restricted: bool,
    external_switch: bool,
    copy_only_reason: Option<&'static str>,
    permit: Option<Permit>,
    hooks: Option<Hooks>,
    target: Target,
    cancelled: Arc<AtomicBool>,
    clipboard: platform::clipboard::Service,
    history: History,
    keys: Arc<Table>,
    stopping: Arc<AtomicBool>,
}

// 读库加文件现查（M2-3）：普通粘贴和逐张放共用。全部文件不在时 content 为 Ok(None)
// 且 files_missing；部分不在只留还在的，files_skipped 记跳过几个。
struct Loaded {
    content: Result<Option<Arc<crate::history::content::Content>>, String>,
    files_skipped: usize,
    files_missing: bool,
}

fn load_for_paste(history: &History, id: usize) -> Loaded {
    let mut loaded = Loaded {
        content: history.load(id).map_err(|e| e.to_string()),
        files_skipped: 0,
        files_missing: false,
    };
    if let Ok(Some(content)) = &loaded.content {
        match history.usable_files(content) {
            crate::history::files::Usable::All => {}
            crate::history::files::Usable::Partial(filtered, skipped) => {
                loaded.files_skipped = skipped;
                loaded.content = Ok(Some(filtered));
            }
            crate::history::files::Usable::Nothing => {
                loaded.files_missing = true;
                loaded.content = Ok(None);
            }
        }
    }
    loaded
}

// 没读到可粘的内容时的结论：读库出错带错误码，否则是记录或文件不在了。
fn not_loaded(error: Option<String>, files_missing: bool) -> Outcome {
    match error {
        Some(code) => {
            let mut result = Outcome::not_written(Reason::StoreReadFailed);
            result.evidence.insert("error_code".into(), json!(code));
            result
        }
        None if files_missing => Outcome::not_written(Reason::FilesMissing),
        None => Outcome::not_written(Reason::RecordMissing),
    }
}

fn note_files_skipped(result: &mut Outcome, files_skipped: usize) {
    if files_skipped > 0 && result.clipboard_written {
        result
            .evidence
            .insert("files_skipped".into(), json!(files_skipped));
    }
}

// 粘贴工作线程（线程名 paste-request）：读库、执行粘贴、补上证据，最后把结果发回主线程。
fn run_paste(job: PasteJob) {
    let PasteJob {
        app,
        logger: worker_logger,
        session,
        id,
        mode,
        started,
        owner,
        restricted,
        external_switch,
        copy_only_reason,
        permit,
        hooks,
        target,
        cancelled,
        clipboard,
        history,
        keys,
        stopping,
    } = job;
    // 监听线程仍在 worker 中存活；只先停止输入拦截，读库和发送期间继续记录前台。
    let drain = || {
        let Some(hooks) = hooks.as_ref() else {
            return false;
        };
        loop {
            if stopping.load(Ordering::Acquire) {
                return false;
            }
            let abandon = cancelled.load(Ordering::Acquire)
                || !permit
                    .as_ref()
                    .is_some_and(|permit| permit.current(&target, window::foreground()));
            match hooks.stop_inputs(abandon) {
                Ok(true) => return hooks.synchronize() && !abandon,
                Ok(false) => std::thread::sleep(Duration::from_millis(5)),
                Err(error) => {
                    let waiting = Instant::now();
                    let mut reported_waiting = false;
                    worker_logger.event(
                        "input_drain_fallback",
                        json!({"session":session,
                        "stage":"started","error":error,"counts":paste::key_state_counts()}),
                    );
                    while !paste::keys_released() && !stopping.load(Ordering::Acquire) {
                        if !reported_waiting && waiting.elapsed() >= Duration::from_secs(1) {
                            reported_waiting = true;
                            worker_logger.event("input_drain_fallback", json!({"session":session,
                                "stage":"waiting","elapsed_ms":waiting.elapsed().as_secs_f64()*1000.0,
                                "counts":paste::key_state_counts()}));
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    worker_logger.event("input_drain_fallback", json!({"session":session,
                        "stage":"finished","elapsed_ms":waiting.elapsed().as_secs_f64()*1000.0,
                        "stopping":stopping.load(Ordering::Acquire),"counts":paste::key_state_counts()}));
                    return false;
                }
            }
        }
    };
    let drained = restricted || drain();
    let read_started = Instant::now();
    let Loaded {
        content: loaded,
        files_skipped,
        files_missing,
    } = load_for_paste(&history, id);
    let exports = history.exports();
    let store_read_ms = read_started.elapsed().as_secs_f64() * 1000.0;
    let mut restored = None;
    let sent = || {
        let _ = dispatch(&app, Action::PasteSent { session });
    };
    let synchronize = || hooks.as_ref().is_some_and(|hooks| hooks.synchronize());
    let mut result: Outcome = {
        let restore = |policy| {
            if restricted || stopping.load(Ordering::Acquire) {
                return None;
            }
            if let Some(hooks) = hooks.as_ref() {
                hooks.synchronize();
            }
            let passive = matches!(policy, RestorePolicy::NativeAfterSubmit { unknown: true });
            return_for_paste(&app, session, permit.clone(), passive, policy)
        };
        match loaded {
            Ok(Some(content)) => paste::execute(
                Request {
                    session,
                    target: target.clone(),
                    panel: owner,
                    record_id: id,
                    content,
                    mode,
                    started,
                    external_switch: external_switch || !drained,
                    permit: permit.clone(),
                    copy_only_reason,
                    text_separator: false,
                    modifier_grace: MODIFIER_GRACE,
                    clipboard_guard: None,
                    cancelled: cancelled.clone(),
                },
                &keys,
                &clipboard,
                &stopping,
                Callbacks {
                    on_sent: &sent,
                    restore: &restore,
                    synchronize: &synchronize,
                },
                exports.as_deref(),
            ),
            missing => {
                // 读库失败也要结束旧请求；主线程会再次检查会话，不能收起新的面板。
                let focus = restore(RestorePolicy::LegacyBeforeSubmit);
                let mut result = not_loaded(missing.err(), files_missing);
                if let Some(focus) = focus {
                    result.evidence.insert("restore".into(), json!(focus));
                }
                result
            }
        }
    };
    note_files_skipped(&mut result, files_skipped);
    if !restricted {
        result.evidence.insert(
            "input_cleanup".into(),
            json!(if drained {
                hooks
                    .as_ref()
                    .map_or("unavailable", Hooks::input_release_evidence)
            } else {
                "cancelled_physical_release"
            }),
        );
    }
    if restricted {
        let paired = drain();
        let copied = result.clipboard_written && !cancelled.load(Ordering::Acquire);
        result.evidence.insert(
            "input_cleanup".into(),
            json!(if paired {
                hooks
                    .as_ref()
                    .map_or("unavailable", Hooks::input_release_evidence)
            } else {
                "cancelled_physical_release"
            }),
        );
        if !stopping.load(Ordering::Acquire) {
            restored = return_for_paste(
                &app,
                session,
                if paired { permit } else { None },
                copied,
                RestorePolicy::LegacyBeforeSubmit,
            );
        }
        if copied {
            let focus = window::confirm_strict(&target);
            result.evidence.insert(
                "hint_message".into(),
                json!(if restored.is_some()
                    && focus.foreground.success
                    && focus.focus_restored
                    && reliable_control(&target)
                {
                    "已复制，请按 Ctrl+V 手动粘贴"
                } else {
                    "已复制，请先点击目标输入位置，再按 Ctrl+V"
                }),
            );
            result.passive_hint = true;
        }
    }
    if let Some(restored) = restored {
        result.evidence.insert("restore".into(), json!(restored));
    }
    result.request_cancelled = cancelled.load(Ordering::Acquire);
    result
        .evidence
        .insert("store_read_ms".into(), json!(store_read_ms));
    // execute 已完成发键及内容证据收集，至此才终止前台监控。
    drop(hooks);
    let _ = dispatch(&app, Action::PasteFinished { session, result });
}

struct QueueJob {
    app: AppHandle,
    logger: Logger,
    session: u64,
    generation: u64,
    id: usize,
    mode: PasteMode,
    text_separator: bool,
    guard: Arc<Guard>,
    clipboard: platform::clipboard::Service,
    history: History,
    keys: Arc<Table>,
    stopping: Arc<AtomicBool>,
}

// 这次 Ctrl+V 落在哪里不能放：没有前台窗口，或前台就是拈来自己（搜索面板还回目标失败时
// 可能如此）。这种情况不写剪贴板，留在原张，提示用户点回目标再按。
fn queue_target_rejected(target: &Target, own_process: u32) -> bool {
    target.window == 0 || target.process == 0 || target.process == own_process
}

// 每一次后续 Ctrl+V 都是用户刚做出的新选择：在 worker 里重新捕获当前目标，并为这一次
// 粘贴建立独立的 quick 许可。实际写剪贴板、格式降级、完整性检查（compare_integrity，
// 管理员目标只复制并停止）和 SendInput 仍走 paste::execute。
// 不等按键松开：Ctrl+V 的 V 已被钩子吞掉，Ctrl 是用户有意按住的；发键前确认对修饰键
// 不留宽限（modifier_grace=0），其余条件仍按原 300ms 确认。
fn run_queue_item(job: QueueJob) {
    let QueueJob {
        app,
        logger,
        session,
        generation,
        id,
        mode,
        text_separator,
        guard,
        clipboard,
        history,
        keys,
        stopping,
    } = job;
    let cancel = guard.cancelled.clone();
    let started = Instant::now();
    let target = window::capture();
    let cancelled = || cancel.load(Ordering::Acquire) || stopping.load(Ordering::Acquire);
    let mut files_skipped = 0;
    let mut result = if cancelled() {
        Outcome::not_written(Reason::RequestCancelled)
    } else if queue_target_rejected(&target, std::process::id()) {
        Outcome::not_written(Reason::InvalidTarget)
    } else {
        let loaded = load_for_paste(&history, id);
        files_skipped = loaded.files_skipped;
        match loaded.content {
            Ok(Some(content)) => {
                let exports = history.exports();
                let monitor = Arc::new(Monitor::new(0, target.window));
                monitor.started();
                let permits = PermitSession::new(session, target.clone(), monitor);
                let permit = permits.quick(id, mode);
                let restore = |_| Some(window::confirm(&target));
                let sent = || {};
                let synchronize = || true;
                paste::execute(
                    Request {
                        session,
                        target: target.clone(),
                        // 队列的小指示不是提交目标；0 恒满足执行器的“面板已隐藏”检查。
                        panel: 0,
                        record_id: id,
                        content,
                        mode,
                        started,
                        external_switch: false,
                        permit: Some(permit),
                        copy_only_reason: None,
                        text_separator,
                        modifier_grace: Duration::ZERO,
                        clipboard_guard: Some(guard.clone()),
                        cancelled: cancel.clone(),
                    },
                    &keys,
                    &clipboard,
                    &stopping,
                    Callbacks {
                        on_sent: &sent,
                        restore: &restore,
                        synchronize: &synchronize,
                    },
                    exports.as_deref(),
                )
            }
            missing => not_loaded(missing.err(), loaded.files_missing),
        }
    };
    note_files_skipped(&mut result, files_skipped);
    result.request_cancelled = cancel.load(Ordering::Acquire);
    result.evidence.insert("session".into(), json!(session));
    logger.event(
        "queue_item_worker",
        json!({"session":session,"generation":generation,"record_id":id,
            "clipboard_written":result.clipboard_written,"process_name":target.process_name}),
    );
    let _ = dispatch(
        &app,
        Action::QueueFinished {
            session,
            generation,
            id,
            result,
        },
    );
}

impl Panel {
    pub(crate) fn new(
        window: WebviewWindow,
        plan: Option<autocycle::Plan>,
        resuspend: Option<Duration>,
    ) -> Self {
        Self {
            window,
            mode: Mode::Hidden,
            ready: false,
            session: 0,
            target: Target::default(),
            hotkey: None,
            hooks: None,
            interaction: Interaction::Quick,
            monitor: None,
            permits: None,
            started: None,
            shown: None,
            painted: false,
            foreground_taken: false,
            awaiting_search: false,
            memory: Arc::new(webview::MemoryState::default()),
            pasting: false,
            paste_cancel: None,
            queue: None,
            layout: None,
            glide: None,
            avoid: None,
            queue_dwell: None,
            dwell_serial: 0,
            hint: None,
            auto_enabled: plan.is_some(),
            auto_plan: plan,
            auto_stop: None,
            auto_visible: None,
            resuspend,
            resuspend_stop: None,
        }
    }
    pub(crate) fn set_hotkey(&mut self, hotkey: Option<Shortcut>, logger: &Logger) {
        self.hotkey = hotkey;
        // 就地更新放行组合，不重建钩子、不改焦点，也不丢弃已吞按键的抬起配对。
        if let Some(hooks) = &self.hooks
            && let Err(error) = hooks.set_hotkey(hotkey)
        {
            logger.event("hotkey_hook_update_failed", json!({"error":error}));
        }
    }
    /// 退出前的面板清理：停掉自动循环和重新挂起线程，结束搜索回放，拆掉键盘钩子。
    pub(crate) fn shutdown(&mut self, logger: &Logger) {
        self.auto_stop.take();
        self.resuspend_stop.take();
        if let Err(error) = self.replay_search(logger, false, "shutdown", false) {
            self.log(logger, "replay_cleanup_failed", json!({"error":error}));
        }
        self.hooks.take();
        if let Some(queue) = self.queue.take() {
            queue.guard.stop(Stop::Cancelled);
            let _ = queue.clipboard.set_clipboard_watcher(None);
        }
    }
    fn log(&self, logger: &Logger, event: &str, fields: Value) {
        logger.event(event, stamp(fields, self.session, &self.target));
    }
    fn emit(&self, event: &str, mut payload: Value) {
        payload["session"] = json!(self.session);
        let _ = self.window.emit(event, payload);
    }
    // 面板收起：通知前端，并让历史服务放掉读连接池的空闲连接和缩略图缓存
    // （关连接很快，不会阻塞主线程；下次读再按需新开）。
    fn emit_hidden(&self) {
        self.emit("panel:hidden", json!({}));
        if let Some(controller) = self.window.app_handle().try_state::<Controller>() {
            controller.history.release_idle();
        }
    }
    fn emit_queue(&self) {
        if let Some(queue) = self.queue.as_ref() {
            self.emit(
                "queue:changed",
                json!({
                    "remaining":queue.state.ids.len(),"total":queue.state.total,"automatic":queue.state.automatic,
                    "busy":queue.state.busy(),"error":queue.state.error,"skipped":queue.state.skipped,
                }),
            );
        }
    }

    // 所有真正收窗路径共用：先停止 WebView 合成，再隐藏原生窗口；挂起请求仍由原函数发出。
    fn conceal(
        &self,
        logger: &Logger,
        ready: Option<mpsc::Sender<platform::Result<Instant>>>,
    ) -> platform::Result<()> {
        hide_webview_then_native(
            || {
                webview::set_visible_with_completion(
                    &self.window,
                    false,
                    logger.clone(),
                    self.session,
                    self.memory.clone(),
                    ready,
                )
            },
            || window::hide(&self.window),
        )
    }

    fn start_queue(
        &mut self,
        app: &AppHandle,
        logger: &Logger,
        command: StartQueueCommand,
    ) -> platform::Result<()> {
        let StartQueueCommand {
            session,
            ids,
            automatic,
            mode,
            selection,
        } = command;
        if let Err(message) = crate::paste::queue::validate_ids(&ids) {
            // 不走 Err：dispatch 的通用错误会收起面板并只显示“操作未完成”。
            self.log(
                logger,
                "queue_rejected",
                json!({"reason":"invalid_ids","count":ids.len()}),
            );
            self.emit("panel:error", json!({"message":message}));
            return Ok(());
        }
        if mode == PasteMode::Copy {
            self.log(logger, "queue_rejected", json!({"reason":"copy_mode"}));
            self.emit(
                "panel:error",
                json!({"message":"逐张粘贴只支持粘贴模式，请重新选择。"}),
            );
            return Ok(());
        }
        if self.interaction == Interaction::Activated {
            // 主路径这种目标（可能以管理员运行）只复制；低权限的键盘钩子也收不到发往它的
            // Ctrl+V，逐张放必然失效，所以直接拒绝。
            self.log(
                logger,
                "queue_rejected",
                json!({"reason":"target_may_be_elevated"}),
            );
            self.emit(
                "panel:error",
                json!({"message":"目标窗口可能以管理员身份运行，逐张粘贴不可用；请逐条粘贴。"}),
            );
            return Ok(());
        }
        if self.mode == Mode::Search {
            let Some(selection) = selection else {
                self.emit(
                    "panel:error",
                    json!({"message":"请在搜索面板重新选择条目。"}),
                );
                return Ok(());
            };
            let foreground = window::foreground();
            let permits = self.permits.as_mut().ok_or("前台监控尚未建立")?;
            if let Err(reason) = permits.select(session, ids[0], mode, selection, foreground) {
                self.log(logger, "queue_rejected", json!({"reason":reason}));
                self.emit(
                    "panel:error",
                    json!({"message":"选择期间窗口状态已变化，请重新选择条目。"}),
                );
                return Ok(());
            }
        } else if selection.is_some() {
            self.emit(
                "panel:error",
                json!({"message":"面板模式已变化，请重新选择条目。"}),
            );
            return Ok(());
        }

        // 新钩子先以 disabled 状态安装，watcher 也先就绪；此前失败完全不破坏旧面板。
        let clipboard = app.state::<Controller>().clipboard.clone();
        let guard = match Guard::new(clipboard.owner(), clipboard.stamp()) {
            Ok(guard) => Arc::new(guard),
            Err(_) => {
                self.emit(
                    "panel:error",
                    json!({"message":"剪贴板状态无法确认，逐张粘贴未启动；请重新复制后再试。"}),
                );
                return Ok(());
            }
        };
        let generation = session;
        let callback_app = app.clone();
        let hooks = match platform::queue_hook::QueueHooks::install(
            guard.cancelled.clone(),
            {
                let guard = guard.clone();
                // 主线程按 IDLE_TIMEOUT 停止；钩子线程多给 5 秒作为主线程卡住时的兜底。
                move || guard.idle() >= crate::paste::queue::IDLE_TIMEOUT + Duration::from_secs(5)
            },
            move |event| {
                let action = match event {
                    platform::queue_hook::QueueEvent::Paste => Action::QueuePaste {
                        session,
                        generation,
                    },
                    platform::queue_hook::QueueEvent::Cancel => Action::QueueCancel { session },
                    platform::queue_hook::QueueEvent::Timeout => Action::QueueTick {
                        session,
                        generation,
                    },
                };
                let _ = dispatch(&callback_app, action);
            },
        ) {
            Ok(hooks) => hooks,
            Err(error) => {
                self.log(logger, "queue_hook_install_failed", json!({"error":error}));
                self.emit(
                    "panel:error",
                    json!({"message":Stop::InstallFailed.message()}),
                );
                return Ok(());
            }
        };
        let watcher_app = app.clone();
        let watcher_guard = guard.clone();
        if let Err(error) = clipboard.set_clipboard_watcher(Some(Box::new(move |stamp| {
            if stamp.owner == 0 || stamp.sequence == 0 || stamp.owner != watcher_guard.writer() {
                // 回调与写入同在剪贴板线程；这里只做无锁撤销，不反向等待写租约。
                watcher_guard.stop(Stop::ClipboardChanged);
            }
            let _ = dispatch(
                &watcher_app,
                Action::QueueClipboard {
                    session,
                    generation,
                },
            );
        }))) {
            self.log(
                logger,
                "queue_watcher_install_failed",
                json!({"error":error}),
            );
            self.emit(
                "panel:error",
                json!({"message":"剪贴板监听安装失败，逐张粘贴未启动；请重试。"}),
            );
            return Ok(());
        }

        if let Err(error) = self.replay_search(logger, false, "queue", false) {
            self.log(logger, "replay_cleanup_failed", json!({"error":error}));
        }
        self.hooks.take();
        platform::hook::set_preview_keys(false);
        let foreground = window::foreground();
        let panel = window::handle(&self.window)?;
        let mut returned = true;
        if self.foreground_taken || foreground == panel {
            let focus = window::restore(&self.target);
            self.foreground_taken = false;
            returned = focus.foreground.success && window::foreground() != panel;
            self.log(
                logger,
                "queue_focus_restored",
                json!({"foreground_ok":focus.foreground.success,"focus_restored":focus.focus_restored,
                    "returned":returned}),
            );
        }
        let total = ids.len();
        self.mode = Mode::Queue;
        self.awaiting_search = false;
        self.hint = None;
        self.permits = None;
        self.monitor = None;
        self.layout = None;
        self.glide = None;
        self.avoid = None;
        self.queue = Some(PasteQueue {
            state: QueueState::new(ids, automatic),
            mode,
            generation,
            guard: guard.clone(),
            hooks: Some(hooks),
            clipboard,
        });
        if let Err(error) = window::show_queue(&self.window, &self.target) {
            self.log(logger, "queue_window_failed", json!({"error":error}));
            return self.fail_queue(app, logger, Stop::InstallFailed);
        }
        let tick_app = app.clone();
        let tick_guard = guard.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("queue-tick".into())
            .spawn(move || {
                while !tick_guard.cancelled.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(50));
                    if tick_guard.cancelled.load(Ordering::Acquire)
                        || dispatch(
                            &tick_app,
                            Action::QueueTick {
                                session,
                                generation,
                            },
                        )
                        .is_err()
                    {
                        break;
                    }
                }
            })
        {
            self.log(
                logger,
                "queue_tick_failed",
                json!({"error":error.to_string()}),
            );
            return self.fail_queue(app, logger, Stop::WorkerFailed);
        }
        if let Some(queue) = self.queue.as_mut() {
            queue.state.installed(true, queue.guard.elapsed());
            queue.guard.touch();
            if !returned {
                // 没能把焦点还给目标：先别自动放，请用户点回目标；之后的 Ctrl+V 照常生效。
                queue
                    .state
                    .notice("请先点回要粘贴的位置，再按 Ctrl+V；Esc 取消");
            }
            if let Some(hooks) = queue.hooks.as_ref() {
                hooks.activate();
            }
        }
        // 手牌隐藏但 WebView 继续醒着，队列进度和取消入口仍实时更新。
        self.emit("panel:hidden", json!({"queue":true}));
        self.emit_queue();
        self.log(logger, "queue_started", json!({"total":total,"automatic":automatic,"mode":match mode { PasteMode::Paste => "paste", PasteMode::Copy => "copy", PasteMode::Text => "text" }}));
        if automatic && returned {
            self.begin_queue_item(app, logger)?;
        }
        Ok(())
    }

    fn begin_queue_item(&mut self, app: &AppHandle, logger: &Logger) -> platform::Result<()> {
        let Some(queue) = self.queue.as_ref() else {
            return Ok(());
        };
        if let Some(reason) = queue.guard.stop_reason() {
            self.stop_queue(logger, reason)?;
            return Ok(());
        }
        let now = queue.guard.elapsed();
        if let Some(reason) = queue.state.expired(now) {
            self.stop_queue(logger, reason)?;
            return Ok(());
        }
        let Some(queue) = self.queue.as_mut() else {
            return Ok(());
        };
        let demand = queue.state.request(now);
        if demand != Demand::Inactive {
            queue.guard.touch();
        }
        let id = match demand {
            Demand::Start(id) => id,
            Demand::Buffered | Demand::Full => {
                self.emit_queue();
                return Ok(());
            }
            Demand::Inactive => return Ok(()),
        };
        let generation = queue.generation;
        let guard = queue.guard.clone();
        let mode = queue.mode;
        let text_separator = queue.state.automatic && queue.state.ids.len() > 1;
        let clipboard = queue.clipboard.clone();
        self.emit_queue();
        let controller = app.state::<Controller>();
        let job = QueueJob {
            app: app.clone(),
            logger: logger.clone(),
            session: self.session,
            generation,
            id,
            mode,
            text_separator,
            guard,
            clipboard,
            history: controller.history.clone(),
            keys: controller.keys.clone(),
            stopping: controller.stopping.clone(),
        };
        if let Err(error) = std::thread::Builder::new()
            .name("queue-paste".into())
            .spawn(move || run_queue_item(job))
        {
            self.log(
                logger,
                "queue_worker_failed",
                json!({"error":error.to_string()}),
            );
            return self.fail_queue(app, logger, Stop::WorkerFailed);
        }
        Ok(())
    }

    fn tick_queue(
        &mut self,
        app: &AppHandle,
        logger: &Logger,
        generation: u64,
    ) -> platform::Result<()> {
        let Some(queue) = self.queue.as_ref() else {
            return Ok(());
        };
        if queue.generation != generation {
            return Ok(());
        }
        if let Some(reason) = queue.guard.stop_reason() {
            return self.stop_queue(logger, reason).map(|_| ());
        }
        let now = queue.guard.elapsed();
        if let Some(reason) = queue.state.expired(now) {
            return self.stop_queue(logger, reason).map(|_| ());
        }
        if !queue.state.busy() {
            let guard = queue.guard.clone();
            let clipboard = queue.clipboard.clone();
            if let Some(reason) = guard.inspect(|| clipboard.stamp()) {
                return self.stop_queue(logger, reason).map(|_| ());
            }
        }
        if self
            .queue
            .as_ref()
            .is_some_and(|queue| queue.state.due(now))
        {
            self.begin_queue_item(app, logger)?;
        }
        Ok(())
    }

    fn inspect_queue_clipboard(
        &mut self,
        logger: &Logger,
        generation: u64,
    ) -> platform::Result<()> {
        let Some(queue) = self.queue.as_ref() else {
            return Ok(());
        };
        if queue.generation != generation {
            return Ok(());
        }
        let mut reason = queue.guard.stop_reason();
        if reason.is_none() && !queue.state.busy() {
            let guard = queue.guard.clone();
            let clipboard = queue.clipboard.clone();
            reason = guard.inspect(|| clipboard.stamp());
        }
        if let Some(reason) = reason {
            self.stop_queue(logger, reason)?;
        }
        Ok(())
    }

    fn finish_queue_item(
        &mut self,
        app: &AppHandle,
        logger: &Logger,
        session: u64,
        generation: u64,
        id: usize,
        result: Outcome,
    ) -> platform::Result<()> {
        let stale = session != self.session
            || self.queue.as_ref().is_none_or(|queue| {
                queue.generation != generation
                    || !queue.state.busy()
                    || queue.state.ids.front() != Some(&id)
            });
        // 过期的回执（队列已停、换了会话）同样记 paste 日志：剪贴板可能已经写过。
        let mut report = result.to_value();
        report["queue"] = json!({"generation":generation,"record_id":id,"stale":stale});
        logger.event("paste", report);
        if stale {
            return Ok(());
        }
        if let Some(reason) = self
            .queue
            .as_ref()
            .and_then(|queue| queue.guard.stop_reason())
        {
            self.stop_queue(logger, reason)?;
            return Ok(());
        }
        // 自写事件可能晚于 worker 回执；推进前复核最终 owner + sequence，外部新内容永不丢。
        let inspected = self.queue.as_ref().map(|queue| {
            let guard = queue.guard.clone();
            let clipboard = queue.clipboard.clone();
            guard.inspect(|| clipboard.stamp())
        });
        if let Some(Some(reason)) = inspected {
            self.stop_queue(logger, reason)?;
            return Ok(());
        }
        let Some(queue) = self.queue.as_mut() else {
            return Ok(());
        };
        let now = queue.guard.elapsed();
        queue.guard.touch();
        match queue.state.finish(id, &result, now) {
            Finished::Stopped(reason) => {
                self.stop_queue(logger, reason)?;
            }
            Finished::Ready => self.emit_queue(),
            Finished::Next => {
                self.emit_queue();
                self.begin_queue_item(app, logger)?;
            }
            Finished::Stale => {}
        }
        Ok(())
    }

    fn stop_queue(&mut self, logger: &Logger, reason: Stop) -> platform::Result<bool> {
        self.stop_queue_with(logger, reason, true)
    }

    // 停止队列：卸掉 Ctrl+V 接管和剪贴板观察，广播最终状态。异常停止（有错误说明）时小指示
    // 带着错误停留 QUEUE_ERROR_DWELL 再收起；放完、取消、Esc、× 立即收起。
    fn stop_queue_with(
        &mut self,
        logger: &Logger,
        reason: Stop,
        may_dwell: bool,
    ) -> platform::Result<bool> {
        let Some(mut queue) = self.queue.take() else {
            return Ok(false);
        };
        queue.guard.stop(reason);
        queue.state.stop(reason);
        let remaining_ids: Vec<usize> = queue.state.ids.iter().copied().collect();
        let total = queue.state.total;
        let skipped = queue.state.skipped;
        let error = queue.state.error;
        if let Err(error) = queue.clipboard.set_clipboard_watcher(None) {
            self.log(
                logger,
                "queue_watcher_cleanup_failed",
                json!({"error":error}),
            );
        }
        // 这里卸钩：停留期间 Ctrl+V、Esc 都不再被接管。
        drop(queue);
        let dwell = may_dwell && queue_stop_dwells(error);
        self.emit(
            "queue:changed",
            json!({"remaining":0,"remaining_ids":remaining_ids,"total":total,"automatic":false,
                "busy":false,"error":error,"skipped":skipped,"stopped":reason.name(),
                "dwell_ms":if dwell { QUEUE_ERROR_DWELL.as_millis() as u64 } else { 0 }}),
        );
        self.log(
            logger,
            "queue_stopped",
            json!({"reason":reason.name(),"message":error,"remaining":remaining_ids.len(),
                "skipped":skipped,"dwell":dwell}),
        );
        if dwell {
            self.dwell_serial += 1;
            let serial = self.dwell_serial;
            self.queue_dwell = Some(serial);
            let app = self.window.app_handle().clone();
            let session = self.session;
            if std::thread::Builder::new()
                .name("queue-dwell".into())
                .spawn(move || {
                    std::thread::sleep(QUEUE_ERROR_DWELL);
                    let _ = dispatch(&app, Action::QueueDwellEnd { session, serial });
                })
                .is_ok()
            {
                return Ok(true);
            }
        }
        self.end_queue_window(logger)?;
        Ok(true)
    }

    fn end_queue_window(&mut self, logger: &Logger) -> platform::Result<()> {
        self.queue_dwell = None;
        self.mode = Mode::Hidden;
        self.awaiting_search = false;
        self.emit_hidden();
        self.conceal(logger, None)
    }

    // 队列已切换后若窗口或线程启动失败，不能留下半激活指示；恢复只读错误面板供用户关闭。
    fn fail_queue(
        &mut self,
        app: &AppHandle,
        logger: &Logger,
        reason: Stop,
    ) -> platform::Result<()> {
        self.stop_queue_with(logger, reason, false)?;
        self.show_submission_feedback(app, logger)?;
        self.emit(
            "panel:error",
            json!({"message":reason.message(),"close_only":true}),
        );
        Ok(())
    }

    // regions 只决定点击区域（窗口裁剪）；avoid 是当前姿态的避让外形，只用于定位。
    // 只有收到非空且与上次不同的 avoid 才考虑挪窗口；没有 avoid（例如拖牌时上报整窗区域）
    // 绝不挪。出错只记日志：不能因为一次上报失败就收起面板。
    #[allow(clippy::too_many_arguments)]
    fn set_hit_regions(
        &mut self,
        app: &AppHandle,
        logger: &Logger,
        session: Option<u64>,
        regions: &[platform::HitRegion],
        armed: bool,
        avoid: Option<window::Avoid>,
    ) {
        if session.is_some_and(|session| session != self.session) {
            return;
        }
        if self.mode == Mode::Queue || (self.mode == Mode::Hidden && armed) {
            // 小指示整块可点（× 在里面），不受手牌布局的旧区域影响；隐藏期的迟到上报不生效。
            window::clear_hit_regions(&self.window);
            return;
        }
        if let Err(error) = window::set_hit_regions(&self.window, regions, armed) {
            self.log(logger, "hit_regions_failed", json!({"error":error}));
        }
        let incoming = match avoid
            .map(|avoid| window::convert_avoid(avoid, window::window_scale(&self.window)))
            .transpose()
        {
            Ok(incoming) => incoming,
            Err(error) => {
                self.log(logger, "avoid_invalid", json!({"error":error}));
                return;
            }
        };
        let Some(content) = avoid_update(self.avoid, incoming) else {
            return;
        };
        self.avoid = Some(content);
        if self.pasting || self.hint.is_some() || !matches!(self.mode, Mode::Quick | Mode::Search) {
            return;
        }
        let Some(layout) = self.layout else {
            return;
        };
        let Some(next) = layout.adjust(content) else {
            return;
        };
        let from = self.glide.as_ref().map_or((layout.x, layout.y), |glide| {
            glide_position(
                glide.from,
                glide.to,
                glide.started.elapsed().as_secs_f64() / GLIDE_DURATION.as_secs_f64(),
            )
        });
        let serial = self.glide.as_ref().map_or(1, |glide| glide.serial + 1);
        self.layout = Some(next);
        self.glide = Some(Glide {
            serial,
            from,
            to: (next.x, next.y),
            started: Instant::now(),
        });
        self.log(
            logger,
            "panel_reposition",
            json!({"from":[from.0,from.1],"to":[next.x,next.y],"placement":next.fit.name()}),
        );
        self.emit(
            "panel:placement",
            json!({"caret":next.caret(),"placement":next.fit.name()}),
        );
        let ticker = app.clone();
        let session = self.session;
        if std::thread::Builder::new()
            .name("panel-glide".into())
            .spawn(move || {
                let started = Instant::now();
                while started.elapsed() < GLIDE_DURATION + GLIDE_STEP {
                    std::thread::sleep(GLIDE_STEP);
                    if dispatch(&ticker, Action::Glide { session, serial }).is_err() {
                        break;
                    }
                }
            })
            .is_err()
        {
            // 线程起不来就一步到位。
            self.glide = None;
            if let Err(error) = window::move_to(&self.window, next.x, next.y) {
                self.log(logger, "panel_reposition_failed", json!({"error":error}));
            }
        }
    }

    fn step_glide(&mut self, logger: &Logger, serial: u64) {
        let Some(glide) = self.glide.as_ref().filter(|glide| glide.serial == serial) else {
            return;
        };
        if !matches!(self.mode, Mode::Quick | Mode::Search) {
            self.glide = None;
            return;
        }
        let t = glide.started.elapsed().as_secs_f64() / GLIDE_DURATION.as_secs_f64();
        let (x, y) = glide_position(glide.from, glide.to, t);
        if t >= 1.0 {
            self.glide = None;
        }
        if let Err(error) = window::move_to(&self.window, x, y) {
            self.glide = None;
            self.log(logger, "panel_reposition_failed", json!({"error":error}));
        }
    }

    fn current(&self, session: u64) -> bool {
        session == self.session && self.mode != Mode::Hidden
    }
    fn shown_event(&self, app: &AppHandle) {
        if let (Some(started), Some(shown)) = (self.started, self.shown) {
            // 历史随呼出事件一起发：前端不用再取一次，呼出后马上按键也拿到的是最新列表。
            let controller = app.state::<Controller>();
            let history = controller.history.snapshot();
            self.emit(
                "panel:shown",
                json!({"hotkeyToShowMs":shown.duration_since(started).as_secs_f64()*1000.0,"history":history,
                    "interaction":self.interaction,
                    "terminal_feedback":self.interaction == Interaction::CloseOnly,
                    // 光标行在窗口客户区里的位置（CSS 像素，可能在窗口外）；前端可选用来避让。
                    "caret":self.layout.as_ref().and_then(window::Layout::caret),
                    "placement":self.layout.as_ref().map(|layout| layout.fit.name()),
                    "foregroundRevision":self.monitor.as_ref().map_or(0, |monitor| monitor.revision())}),
            );
            controller.history.check_visible_files();
        }
    }
    fn show(
        &mut self,
        app: &AppHandle,
        logger: &Logger,
        started: Instant,
        source: ShowSource,
    ) -> platform::Result<()> {
        if !self.ready || self.pasting {
            self.log(
                logger,
                "show_skipped",
                json!({"ready":self.ready,"pasting":self.pasting}),
            );
            return Ok(());
        }
        self.session += 1;
        self.started = Some(started);
        self.shown = None;
        self.painted = false;
        self.foreground_taken = false;
        self.awaiting_search = false;
        self.hint = None;
        self.monitor = None;
        self.permits = None;
        self.auto_visible = None;
        let automatic = match source {
            ShowSource::Hotkey { target, alt } => {
                self.target = target;
                self.log(logger, "hotkey", json!({
                    "hotkey_time_ms":now_ms().saturating_sub(started.elapsed().as_millis() as u64),
                }));
                if alt {
                    self.log(
                        logger,
                        "alt_menu_cancel",
                        json!({"send_input":paste::cancel_alt_menu()}),
                    );
                }
                None
            }
            ShowSource::Auto {
                cycle,
                hidden_since,
            } => {
                self.target = Target::default();
                self.auto_visible = Some(AutoVisible {
                    cycle,
                    hidden_ms: 0.0,
                });
                Some((cycle, hidden_since))
            }
        };
        let webview_started = Instant::now();
        let restricted = automatic.is_none()
            && defers_hide_for_elevated(
                PasteMode::Paste,
                true,
                false,
                &paste::compare_integrity(&self.target),
            );
        self.interaction = if restricted {
            Interaction::ActivationPending
        } else {
            Interaction::Quick
        };
        webview::set_visible(
            &self.window,
            true,
            logger.clone(),
            self.session,
            self.memory.clone(),
        )?;
        let webview_visible_ms = webview_started.elapsed().as_secs_f64() * 1000.0;
        let cursor_known = if automatic.is_some() {
            self.mode = Mode::Auto;
            window::show_auto(&self.window)?;
            self.layout = None;
            None
        } else {
            self.mode = Mode::Quick;
            let shown = window::show(&self.window, &self.target, placement_mode(app))?;
            self.avoid = Some(window::default_avoid(&shown.layout));
            self.layout = Some(shown.layout);
            self.glide = None;
            Some(shown.cursor_known)
        };
        let shown = Instant::now();
        self.shown = Some(shown);
        self.auto_visible = automatic.map(|(cycle, hidden_since)| AutoVisible {
            cycle,
            hidden_ms: shown.saturating_duration_since(hidden_since).as_secs_f64() * 1000.0,
        });
        let trigger_to_show_ms = shown.duration_since(started).as_secs_f64() * 1000.0;
        self.log(logger, "shown", json!({
            "show_time_ms":now_ms(), "hotkey_to_show_ms":trigger_to_show_ms,
            "webview_visible_ms":webview_visible_ms, "auto":automatic.is_some(),
            "cycle":self.auto_visible.map(|auto|auto.cycle),
            "hidden_ms":self.auto_visible.map(|auto|auto.hidden_ms),
            "trigger_to_show_ms":automatic.map(|_|trigger_to_show_ms),
            "cursor_known":cursor_known,
            "foreground_ok":if automatic.is_none() { Some(window::foreground()==self.target.window) } else { None },
        }));
        if automatic.is_none() {
            let session = self.session;
            let app = app.clone();
            let hook_logger = logger.clone();
            let monitor = Arc::new(Monitor::new(
                window::handle(&self.window)?,
                self.target.window,
            ));
            self.monitor = Some(monitor.clone());
            self.permits = Some(PermitSession::new(
                session,
                self.target.clone(),
                monitor.clone(),
            ));
            self.hooks = Some(Hooks::install(
                window::handle(&self.window)?,
                &self.target,
                self.hotkey,
                logger.clone(),
                session,
                platform::HookOptions {
                    monitor,
                    restricted,
                },
                move |event| {
                    let action = match event {
                        HookEvent::Key(key) => Action::Key { session, key },
                        HookEvent::ForegroundRevision(revision) => {
                            Action::ForegroundRevision { session, revision }
                        }
                        HookEvent::Search(started) => Action::Search {
                            session,
                            started: Some(started),
                        },
                        HookEvent::Outside {
                            point,
                            bounds,
                            current,
                        } => {
                            hook_logger.event(
                                "hide_source",
                                json!({"session":session,"source":"outside_click",
                                    "point":point,"bounds":bounds,"current":current}),
                            );
                            Action::OutsideClick { session }
                        }
                        HookEvent::HintKey(end) => Action::HintKey { session, end },
                    };
                    let _ = dispatch(&app, action);
                },
            )?);
        }
        self.shown_event(app);
        if restricted {
            self.activate_restricted(logger)?;
        }
        Ok(())
    }

    fn set_interaction(&mut self, interaction: Interaction) {
        self.interaction = interaction;
        self.emit("panel:interaction", json!({"interaction":interaction}));
    }
    fn show_submission_feedback(
        &mut self,
        app: &AppHandle,
        logger: &Logger,
    ) -> platform::Result<()> {
        // 不建立新会话、许可或监听，也不激活窗口；仅恢复同一请求的关闭入口。
        self.interaction = Interaction::CloseOnly;
        self.mode = Mode::Quick;
        self.awaiting_search = false;
        self.foreground_taken = false;
        self.permits = None;
        self.monitor = None;
        self.shown = Some(Instant::now());
        self.painted = false;
        webview::set_visible(
            &self.window,
            true,
            logger.clone(),
            self.session,
            self.memory.clone(),
        )?;
        let layout = window::show(&self.window, &self.target, placement_mode(app))?.layout;
        self.avoid = Some(window::default_avoid(&layout));
        self.layout = Some(layout);
        self.glide = None;
        self.shown_event(app);
        self.log(
            logger,
            "submission_feedback",
            json!({"interaction":"close_only","reactivated":false}),
        );
        Ok(())
    }
    fn activate_restricted(&mut self, logger: &Logger) -> platform::Result<()> {
        self.set_interaction(Interaction::ActivationPending);
        let owner = window::handle(&self.window)?;
        if let Some(hooks) = self.hooks.as_ref() {
            hooks.activation(false)?;
        }
        window::no_activate(&self.window, false)?;
        let result = window::activate(owner);
        let ready =
            result.success && window::foreground() == owner && webview::focus(&self.window).is_ok();
        if let Some(hooks) = self.hooks.as_ref() {
            hooks.activation(ready)?;
        }
        self.foreground_taken |= ready;
        if !ready {
            window::no_activate(&self.window, true)?;
        }
        self.set_interaction(if ready {
            Interaction::Activated
        } else {
            Interaction::ActivationFailed
        });
        self.log(
            logger,
            "restricted_activation",
            json!({"foreground_path":result.path,"foreground_ok":ready}),
        );
        Ok(())
    }
    fn hide_auto(
        &mut self,
        logger: &Logger,
        ready: Option<mpsc::Sender<platform::Result<Instant>>>,
    ) -> platform::Result<()> {
        let cycle = self.auto_visible.map(|auto| auto.cycle);
        // panel:hidden 先于 SetIsVisible(false)/TrySuspend 发出，前端在挂起前清屏。
        self.emit_hidden();
        self.conceal(logger, ready)?;
        let visible_ms = self
            .shown
            .map(|shown| shown.elapsed().as_secs_f64() * 1000.0);
        self.mode = Mode::Hidden;
        self.auto_visible = None;
        self.log(
            logger,
            "hidden",
            json!({"auto":true,"cycle":cycle,"visible_ms":visible_ms}),
        );
        Ok(())
    }
    fn hide(&mut self, logger: &Logger) -> platform::Result<bool> {
        platform::hook::set_preview_keys(false);
        if self.mode == Mode::Queue {
            // 正在逐张粘贴：取消并收起；已停止、正在停留显示错误：直接收起。
            if !self.stop_queue_with(logger, Stop::Cancelled, false)? {
                self.end_queue_window(logger)?;
            }
            return Ok(false);
        }
        if self.mode == Mode::Auto {
            self.hide_auto(logger, None)?;
            return Ok(false);
        }
        if self.mode == Mode::Hidden {
            return Ok(false);
        }
        if self.pasting && self.hooks.is_none() {
            // worker 持有输入钩子时，先撤销请求；等全部按键松开后由它收起，不能在这里抢先归还。
            if let Some(cancel) = &self.paste_cancel {
                cancel.store(true, Ordering::Release);
            }
            if let Some(monitor) = &self.monitor {
                monitor.stopped();
            }
            self.set_interaction(Interaction::Passive);
            self.log(logger, "hide_deferred", json!({"reason":"paste_key_drain"}));
            return Ok(false);
        }
        // 超时、外部点击、呼出键和排队的按键收起都必须通过同一道检查。
        // 钩子线程在确认没有待配对键后立即卸钩，不留检查后再吞键的窗口；退出不等待。
        if hint_needs_key_drain(
            self.dwelling(),
            self.hint.as_ref().and_then(|hint| hint.ended_by),
        ) && let Some(hooks) = self.hooks.as_ref()
            && !hooks.finish_hint()?
        {
            self.log(
                logger,
                "hint_close_deferred",
                json!({"reason":"swallowed_keys_held"}),
            );
            return Ok(false);
        }
        // 被动提示到点或经显式关闭路径收起时，记录停留结束及实际时长。
        // 预判后还没等到结论就收起的（占位、dwell 为 None）只清掉标记，不算停留过。
        if let Some(hint) = self.hint.take()
            && let Some(dwell) = hint.dwell
        {
            self.log(logger, "elevated_hint_hidden", {
                let mut fields = hint_ended_fields(dwell, hint.since.elapsed(), hint.ended_by);
                fields["reason"] = json!(hint.reason);
                fields
            });
        }
        let foreground = window::foreground();
        let panel = window::handle(&self.window)?;
        let may_restore = self.interaction.may_restore()
            && (foreground == panel || foreground == self.target.window);
        if let Err(error) = self.replay_search(logger, false, "hidden", false) {
            self.log(logger, "replay_cleanup_failed", json!({"error":error}));
        }
        self.mode = Mode::Hidden;
        self.hooks.take();
        self.awaiting_search = false;
        self.emit_hidden();
        self.conceal(logger, None)?;
        let focus = if (self.foreground_taken || foreground == panel) && may_restore {
            window::restore(&self.target)
        } else {
            window::confirm(&self.target)
        };
        self.log(
            logger,
            "focus_restored",
            json!({"foreground_path":focus.foreground.path,"foreground_ok":focus.foreground.success,
            "focus_restored":focus.focus_restored,"focus_unknown":focus.focus_unknown,"external_switch":!may_restore}),
        );
        self.log(logger, "hidden", json!({}));
        Ok(may_restore && focus.foreground.success && focus.focus_restored)
    }
    fn begin_search(
        &mut self,
        app: &AppHandle,
        logger: &Logger,
        started: Option<Instant>,
    ) -> platform::Result<()> {
        if self.mode != Mode::Quick
            || !matches!(
                self.interaction,
                Interaction::Quick | Interaction::Activated
            )
        {
            return Ok(());
        }
        self.mode = Mode::Search;
        self.awaiting_search = true;
        if self.interaction == Interaction::Activated {
            // 激活式面板已经由 DOM 持有焦点及输入法组合，切搜索只做模式握手，不再次聚焦或补发首字。
            self.emit("panel:mode", json!({"mode":"search"}));
            self.log(
                logger,
                "search_dom_handoff",
                json!({
                    "foreground_ok":window::foreground()==window::handle(&self.window)?
                }),
            );
            let app = app.clone();
            let session = self.session;
            std::thread::spawn(move || {
                std::thread::sleep(REPLAY_WAIT);
                let _ = dispatch(&app, Action::SearchTimeout { session });
            });
            return Ok(());
        }
        let started = self
            .hooks
            .as_ref()
            .ok_or("键盘钩子尚未安装")?
            .begin_search(started.unwrap_or_else(Instant::now))?;
        // 拿前台之前先记目标程序的输入法状态，和面板拿到焦点后的几条对照。焦点控件未知时退回主窗口。
        // 查询在状态机线程里做，每条消息最多等 50 ms，不进钩子回调。
        let probed = if self.target.focus != 0 {
            self.target.focus
        } else {
            self.target.window
        };
        self.log(
            logger,
            "ime_state",
            json!({"stage":"target_before_activate","ime":ime::query(probed)}),
        );
        let panel = window::handle(&self.window)?;
        window::no_activate(&self.window, false)?;
        let result = window::activate(panel);
        self.foreground_taken = result.success;
        self.log(
            logger,
            "search_foreground",
            json!({"foreground_path":result.path,"foreground_ok":result.success}),
        );
        if result.success {
            self.prepare_search()?;
        }
        let app = app.clone();
        let session = self.session;
        std::thread::spawn(move || {
            if !result.success {
                while window::foreground() != panel && started.elapsed() < REPLAY_WAIT {
                    std::thread::sleep(Duration::from_millis(5));
                }
                let _ = dispatch(
                    &app,
                    Action::SearchActivated {
                        session,
                        success: window::foreground() == panel,
                    },
                );
            }
            if let Some(remaining) = REPLAY_WAIT.checked_sub(started.elapsed()) {
                std::thread::sleep(remaining);
            }
            let _ = dispatch(&app, Action::SearchTimeout { session });
        });
        Ok(())
    }
    fn prepare_search(&self) -> platform::Result<()> {
        webview::focus(&self.window)?;
        self.emit("panel:mode", json!({"mode":"search"}));
        Ok(())
    }
    fn replay_search(
        &self,
        logger: &Logger,
        ready: bool,
        reason: &str,
        probe_ime: bool,
    ) -> platform::Result<()> {
        let Some(hooks) = self.hooks.as_ref() else {
            return Ok(());
        };
        let batch = hooks.remove_keyboard()?;
        if batch.started.is_none() && batch.keys.is_empty() && batch.dropped == 0 {
            return Ok(());
        }
        let expired = batch
            .started
            .is_some_and(|started| started.elapsed() >= REPLAY_WAIT);
        // 补发前记一次面板焦点窗口（应在 msedgewebview2.exe 里）的输入法状态：卸钩之后、SendInput 之前，
        // 在状态机线程里查，不进钩子回调。只有 finish_search 传 true，收起和退出时的清理不查。
        let before = probe_ime.then(ime::query_focused);
        let replayed = if ready && !expired {
            if let Some(started) = batch.started {
                paste::replay(&batch.keys, window::handle(&self.window)?, started)
            } else {
                0
            }
        } else {
            0
        };
        if let Some(ime) = before {
            self.log(
                logger,
                "ime_state",
                json!({"stage":"panel_before_replay","replayed":replayed,"ime":ime}),
            );
            self.probe_after_replay(logger, replayed);
        }
        self.log(
            logger,
            "search_replay",
            json!({
                "replayed_keys":replayed,
                "dropped_keys":batch.dropped + batch.keys.len() - replayed,
                "wait_ms":batch.started.map(|started| started.elapsed().as_secs_f64() * 1000.0),
                "search_box_ready":ready,
                "reason":if expired { "timeout" } else { reason },
            }),
        );
        Ok(())
    }
    // 豆包、微信输入法会在焦点到达后几十毫秒异步恢复状态，补发后 50 ms 和 150 ms 各查一次，不能省。
    // 用短命线程睡，不让状态机等；线程里只做跨进程的只读查询和写日志。
    fn probe_after_replay(&self, logger: &Logger, replayed: usize) {
        let probe_logger = logger.clone();
        let session = self.session;
        let target = self.target.clone();
        let replayed_at = Instant::now();
        let spawned = std::thread::Builder::new()
            .name("ime-probe".into())
            .spawn(move || {
                for (stage, sleep_ms) in [("panel_after_replay_50", 50), ("panel_after_replay_150", 100)] {
                    std::thread::sleep(Duration::from_millis(sleep_ms));
                    let after_replay_ms = replayed_at.elapsed().as_secs_f64() * 1000.0;
                    let ime = ime::query_focused();
                    probe_logger.event(
                        "ime_state",
                        stamp(
                            json!({"stage":stage,"replayed":replayed,"after_replay_ms":after_replay_ms,"ime":ime}),
                            session,
                            &target,
                        ),
                    );
                }
            });
        if spawned.is_err() {
            self.log(logger, "ime_probe_failed", json!({"replayed":replayed}));
        }
    }
    fn finish_search(&mut self, logger: &Logger, ready: bool) -> platform::Result<()> {
        if !self.awaiting_search {
            return Ok(());
        }
        if self.dwelling() {
            // 提示已覆盖搜索操作；不能先卸钩、补键、重挂，否则中间会漏掉待配对的抬起。
            // 原缓冲留到真正收起时清理，不再把首键补进提示期间的搜索框。
            self.awaiting_search = false;
            self.log(
                logger,
                "search_replay_deferred",
                json!({"reason":"elevated_hint"}),
            );
            return Ok(());
        }
        if self.interaction == Interaction::Activated {
            self.replay_search(logger, false, "activated_dom", false)?;
        } else {
            self.replay_search(logger, ready, if ready { "ready" } else { "timeout" }, true)?;
        }
        self.awaiting_search = false;
        self.log(logger, "search_ready", json!({"search_box_ready":ready,"foreground_ok":window::foreground()==window::handle(&self.window)?}));
        Ok(())
    }
    fn mode_name(&self) -> &'static str {
        match self.mode {
            Mode::Hidden => "hidden",
            Mode::Quick => "quick",
            Mode::Search => "search",
            Mode::Queue => "queue",
            Mode::Auto => "auto",
        }
    }
    // 复制和松键清理完成、受控归还已尝试后，开始被动提示计时；输入钩子已撤下，到点自动收起。
    fn begin_hint(
        &mut self,
        app: &AppHandle,
        logger: &Logger,
        dwell: Duration,
        reason: &'static str,
    ) -> platform::Result<()> {
        let session = self.session;
        let app = app.clone();
        std::thread::Builder::new()
            .name("passive-hint".into())
            .spawn(move || {
                std::thread::sleep(dwell);
                let _ = dispatch(&app, Action::HintElapsed { session });
            })
            .map_err(|error| format!("被动提示计时线程启动失败：{error}"))?;
        self.hint = Some(Hint {
            since: Instant::now(),
            dwell: Some(dwell),
            ended_by: None,
            reason,
        });
        // 已批准 A：归还后只保留被动反馈，不再承诺管理员目标下能截键或按键立即关闭。
        self.set_interaction(Interaction::Passive);
        window::no_activate(&self.window, true)?;
        // target_foreground 只记录此刻目标是否在前台，不把归还尝试当成已成功。
        self.log(logger, "elevated_hint_shown", json!({
            "reason":reason,"dwell_ms":dwell.as_millis() as u64,
            "panel_mode":self.mode_name(),"target_foreground":window::foreground()==self.target.window,
            "hint_keys":false,"hint_keys_error":null,
        }));
        Ok(())
    }
    // 是否正在管理员提示停留中（结论已确认、在计时；预判占位不算）。
    fn dwelling(&self) -> bool {
        self.hint.as_ref().is_some_and(|hint| hint.dwell.is_some())
    }
    // 停留中要收起前记下结束原因，hide() 写进 elevated_hint_hidden。不在停留时什么也不做。
    fn end_hint(&mut self, ended_by: &'static str) {
        if let Some(hint) = self.hint.as_mut()
            && hint.dwell.is_some()
        {
            hint.ended_by = Some(hint_end_reason(hint.ended_by, ended_by));
        }
    }
    fn auto_show(
        &mut self,
        app: &AppHandle,
        logger: &Logger,
        cycle: usize,
        hidden_since: Instant,
        triggered: Instant,
        reply: mpsc::Sender<platform::Result<(u64, Instant)>>,
    ) -> platform::Result<()> {
        if !self.auto_enabled || !self.ready || self.mode != Mode::Hidden {
            return Ok(());
        }
        let result = self
            .show(
                app,
                logger,
                triggered,
                ShowSource::Auto {
                    cycle,
                    hidden_since,
                },
            )
            .and_then(|()| {
                self.shown
                    .map(|at| (self.session, at))
                    .ok_or("自动显示未完成".into())
            });
        let _ = reply.send(result.clone());
        result?;
        Ok(())
    }

    fn toggle(
        &mut self,
        app: &AppHandle,
        logger: &Logger,
        started: Instant,
        target: Target,
        alt: bool,
    ) -> platform::Result<()> {
        if self.mode == Mode::Hidden {
            self.show(app, logger, started, ShowSource::Hotkey { target, alt })?;
        } else {
            if alt {
                self.log(
                    logger,
                    "alt_menu_cancel",
                    json!({"send_input":paste::cancel_alt_menu()}),
                );
            }
            self.log(logger, "hide_source", json!({"source":"hotkey"}));
            self.end_hint("hotkey");
            self.hide(logger)?;
        }
        Ok(())
    }

    fn frontend_ready(&mut self, app: &AppHandle, logger: &Logger) -> platform::Result<()> {
        let first = !self.ready;
        self.ready = true;
        self.log(logger, "frontend_ready", json!({}));
        // 普通模式和自动循环模式都要启动重挂起线程。
        if first && let Some(interval) = self.resuspend {
            self.resuspend_stop = Some(resuspend::spawn(
                app.clone(),
                logger.clone(),
                interval,
                self.memory.clone(),
            )?);
        }
        if self.auto_enabled {
            if first {
                let plan = self.auto_plan.take().ok_or("自动循环计划不存在")?;
                let (ready, hidden) = mpsc::channel();
                self.conceal(logger, Some(ready))?;
                self.auto_stop = Some(autocycle::spawn(app.clone(), logger.clone(), plan, hidden)?);
            }
            return Ok(());
        }
        if self.mode == Mode::Hidden {
            webview::set_visible(
                &self.window,
                false,
                logger.clone(),
                self.session,
                self.memory.clone(),
            )?;
        } else {
            self.shown_event(app);
        }
        Ok(())
    }

    // 粘贴线程请求把焦点还给原窗口：主线程做完再用 reply 回给线程。
    fn return_focus(
        &mut self,
        app: &AppHandle,
        logger: &Logger,
        request: ReturnRequest,
    ) -> platform::Result<()> {
        let ReturnRequest {
            session,
            permit,
            passive,
            policy,
            requested,
            reply,
        } = request;
        if session != self.session || !self.pasting || self.mode == Mode::Hidden {
            let _ = reply.send(platform::FocusResult {
                foreground: platform::ForegroundResult {
                    path: "stale_request",
                    success: false,
                },
                focus_restored: false,
                focus_unknown: true,
            });
            return Ok(());
        }
        let mut focus = window::confirm(&self.target);
        if session == self.session && self.pasting && self.mode != Mode::Hidden {
            let native_unknown =
                matches!(policy, RestorePolicy::NativeAfterSubmit { unknown: true });
            if native_unknown && self.hint.is_none() {
                self.hint = Some(Hint {
                    since: Instant::now(),
                    dwell: None,
                    ended_by: None,
                    reason: "native_submission_unknown",
                });
            }
            let foreground = window::foreground();
            let can_restore = requested.elapsed() < REPLAY_WAIT
                && !app.state::<Controller>().stopping.load(Ordering::Acquire)
                && window::valid(&self.target)
                && permit
                    .as_ref()
                    .is_some_and(|permit| permit.current(&self.target, foreground));
            if passive {
                self.set_interaction(Interaction::Passive);
            }
            window::no_activate(&self.window, true)?;
            let armed_at = window::tick_count();
            if !can_restore && let Some(permit) = permit.as_ref() {
                permit.revoke();
            }
            if can_restore
                && permit
                    .as_ref()
                    .is_some_and(|permit| permit.arm_return(foreground, armed_at))
            {
                if foreground == window::handle(&self.window)? {
                    // WinEvent 与 GetTickCount 同 tick 时无法证明先后，跨到下一 tick 再进行受控归还。
                    let waiting = Instant::now();
                    while window::tick_count() == armed_at
                        && waiting.elapsed() < Duration::from_millis(30)
                    {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    if window::tick_count() != armed_at
                        && requested.elapsed() < REPLAY_WAIT
                        && !app.state::<Controller>().stopping.load(Ordering::Acquire)
                        && let Some(permit) = permit.as_ref()
                        && permit.return_pending(&self.target, window::foreground())
                    {
                        focus = window::restore_for_paste(
                            &self.target,
                            permit,
                            matches!(policy, RestorePolicy::NativeAfterSubmit { .. }),
                        );
                    } else if let Some(permit) = permit.as_ref() {
                        // 等待期间切走、到期或退出后只复制，不能让提前观察到的目标事件保留发送资格。
                        permit.revoke();
                    }
                } else {
                    focus = window::confirm(&self.target);
                }
            }
            if !passive {
                self.mode = Mode::Hidden;
                self.awaiting_search = false;
                self.emit_hidden();
                self.conceal(logger, None)?;
                self.log(logger, "hidden", json!({"source":"paste"}));
            }
        }
        let _ = reply.send(focus);
        Ok(())
    }

    fn begin_paste(
        &mut self,
        app: &AppHandle,
        logger: &Logger,
        command: PasteCommand,
    ) -> platform::Result<()> {
        let PasteCommand {
            session,
            id,
            mode,
            selection,
            started,
        } = command;
        let owner = window::handle(&self.window)?;
        let foreground = window::foreground();
        let search = self.mode == Mode::Search;
        let restricted = self.interaction == Interaction::Activated;
        let external_switch = !search
            && self
                .hooks
                .as_ref()
                .is_some_and(|hooks| hooks.switched_to() != 0);
        let permits = self.permits.as_mut().ok_or("前台监控尚未建立")?;
        let permit = if search {
            match selection {
                Some(selection) => match permits.select(session, id, mode, selection, foreground) {
                    Ok(permit) => Some(permit),
                    Err(reason) => {
                        self.log(logger, "paste_rejected", json!({"reason":reason}));
                        self.emit(
                            "panel:error",
                            json!({"message":"选择期间窗口状态已变化，请重新选择条目。"}),
                        );
                        return Ok(());
                    }
                },
                None => {
                    self.emit(
                        "panel:error",
                        json!({"message":"请在搜索面板重新选择条目。"}),
                    );
                    return Ok(());
                }
            }
        } else if selection.is_some() {
            self.emit(
                "panel:error",
                json!({"message":"面板模式已变化，请重新选择条目。"}),
            );
            return Ok(());
        } else {
            Some(permits.quick(id, mode))
        };
        let mut copy_only_reason = None;
        if search && !reliable_control(&self.target) && copy_only_reason.is_none() {
            copy_only_reason = Some("target_control_unverified");
        }
        if restricted && mode != PasteMode::Copy {
            copy_only_reason = Some("target_may_be_elevated");
        }
        if restricted {
            self.hint = Some(Hint {
                since: Instant::now(),
                dwell: None,
                ended_by: None,
                reason: "target_may_be_elevated",
            });
        }
        let hooks = self.hooks.take();
        let target = self.target.clone();
        self.pasting = true;
        let cancelled = Arc::new(AtomicBool::new(false));
        self.paste_cancel = Some(cancelled.clone());
        let controller = app.state::<Controller>();
        let clipboard = controller.clipboard.clone();
        let history = controller.history.clone();
        let keys = controller.keys.clone();
        let stopping = controller.stopping.clone();
        let job = PasteJob {
            app: app.clone(),
            logger: logger.clone(),
            session,
            id,
            mode,
            started,
            owner,
            restricted,
            external_switch,
            copy_only_reason,
            permit,
            hooks,
            target,
            cancelled,
            clipboard,
            history,
            keys,
            stopping,
        };
        if std::thread::Builder::new()
            .name("paste-request".into())
            .spawn(move || run_paste(job))
            .is_err()
        {
            self.pasting = false;
            return Err("粘贴线程启动失败".into());
        }
        Ok(())
    }

    fn finish_paste(
        &mut self,
        app: &AppHandle,
        logger: &Logger,
        session: u64,
        mut result: Outcome,
    ) -> platform::Result<()> {
        // 证据收集期间可能已经再次呼出，日志要用这次粘贴自己的会话号和目标，不能用当前的。
        result.evidence.insert("session".into(), json!(session));
        if session == self.session
            && self
                .paste_cancel
                .as_ref()
                .is_some_and(|cancel| cancel.load(Ordering::Acquire))
        {
            result.request_cancelled = true;
        }
        logger.event("paste", result.to_value());
        if session == self.session && result.clipboard_written && self.mode != Mode::Hidden {
            self.emit("panel:history-changed", json!({}));
        }
        if session == self.session {
            self.pasting = false;
            self.paste_cancel = None;
            match generic_submission_finish(
                &result,
                true,
                self.mode != Mode::Hidden,
                self.interaction,
            )
            .or_else(|| no_submission_finish(&result, self.current(session), self.interaction))
            {
                Some(NoSubmissionFinish::CloseRequested) => {
                    // 写前/写后均兑现等待中的关闭；Passive 禁止归还或抢回前台。
                    self.hide(logger)?;
                    return Ok(());
                }
                Some(
                    finish @ (NoSubmissionFinish::CloseOnly | NoSubmissionFinish::ShowCloseOnly),
                ) => {
                    // 未提交保留真实写入事实，但不承诺剪贴板仍有此内容，不提供重试。
                    self.hint.take();
                    if finish == NoSubmissionFinish::ShowCloseOnly {
                        self.show_submission_feedback(app, logger)?;
                    } else {
                        self.set_interaction(Interaction::CloseOnly);
                    }
                    self.emit(
                                "panel:error",
                                json!({"message":no_submission_message(result.reason.as_ref()),"close_only":true}),
                            );
                    // 必须绕过 paste-result 和 deferred hide；前端继续复用已有 CloseOnly。
                    return Ok(());
                }
                Some(NoSubmissionFinish::Ignore) => return Ok(()),
                None => {}
            }
            // cleanup 回执超时也必须显示未知结果；不再尝试归还，也不允许用户误重试。
            let native_unknown = result.result == Some(PasteResult::NativeSubmissionUnknown);
            if native_unknown && self.current(session) && !result.request_cancelled {
                self.hint.get_or_insert(Hint {
                    since: Instant::now(),
                    dwell: None,
                    ended_by: None,
                    reason: "native_submission_unknown",
                });
                self.set_interaction(Interaction::Passive);
            }
            // 预判时没收起的面板：按实际结论决定是停留提示还是现在收起。
            let deferred = self.hint.take().is_some() && self.current(session);
            let dwell = if deferred {
                dwell_after_paste(&result)
            } else {
                None
            };
            if !result.clipboard_written
                && result.reason != Some(Reason::RequestCancelled)
                && !result.request_cancelled
            {
                let message = match result.reason {
                    Some(Reason::RecordMissing) => "这条记录已不在历史里",
                    Some(Reason::FilesMissing) => "原文件不在了，没有粘贴",
                    Some(Reason::StoreReadFailed) => "读取历史失败，未改写剪贴板。",
                    Some(Reason::ClipboardCaptureBusy) => {
                        "剪贴板忙，本次未提交，请关闭面板后重新呼出"
                    }
                    _ => "写入剪贴板失败，请重试。",
                };
                self.emit("panel:error", json!({"message":message,"close_only":false}));
            } else if result.clipboard_written {
                if let Some(dwell) = dwell {
                    // 告诉前端面板还开着、要停留：页脚当场显示提示，停留期间不再发起粘贴。
                    result
                        .evidence
                        .insert("hint_dwell_ms".into(), json!(dwell.as_millis() as u64));
                }
                self.emit("panel:paste-result", result.to_value());
            }
            if deferred {
                match dwell {
                    Some(dwell) => self.begin_hint(
                        app,
                        logger,
                        dwell,
                        if native_unknown {
                            "native_submission_unknown"
                        } else {
                            "target_may_be_elevated"
                        },
                    )?,
                    None => {
                        // 预判要停留但结论不是"管理员、只复制"（期间目标关了、用户切走、记录没了）：
                        // 不扩大停留范围，现在收起。
                        self.log(
                            logger,
                            "hide_source",
                            json!({"source":"paste_without_hint"}),
                        );
                        self.hide(logger)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn quit(&mut self, app: &AppHandle, logger: &Logger, relaunch: bool) {
        app.state::<Controller>()
            .stopping
            .store(true, Ordering::Release);
        self.auto_stop.take();
        self.resuspend_stop.take();
        self.end_hint("quit");
        if self.mode != Mode::Hidden
            && let Err(error) = self.hide(logger)
        {
            self.log(logger, "quit_cleanup_failed", json!({"error":error}));
        }
        // 粘贴松键清理中的 hide() 会只撤销请求、暂不收窗；退出不等清理，但仍按同一顺序收尾。
        if self.mode != Mode::Hidden
            && let Err(error) = self.conceal(logger, None)
        {
            self.log(logger, "quit_cleanup_failed", json!({"error":error}));
        }
        self.hooks.take();
        self.log(logger, "quit_requested", json!({"relaunch":relaunch}));
        logger.flush();
        app.exit(if relaunch {
            crate::recovery::RELAUNCH_CODE
        } else {
            0
        });
    }

    fn act(&mut self, app: &AppHandle, logger: &Logger, action: Action) -> platform::Result<()> {
        if self.auto_enabled {
            let command = match &action {
                Action::Paste { .. } => Some("paste_item"),
                Action::StartQueue { .. } => Some("start_queue"),
                Action::Search { .. } => Some("start_search"),
                Action::SearchReady { .. } => Some("search_ready"),
                Action::Hide { .. } => Some("hide_panel"),
                _ => None,
            };
            if let Some(command) = command {
                self.log(
                    logger,
                    "autocycle_command_ignored",
                    json!({"command":command}),
                );
                return Ok(());
            }
        }
        match action {
            Action::HistoryChanged if self.mode != Mode::Hidden => {
                self.emit("panel:history-changed", json!({}));
            }
            Action::AutoShow {
                cycle,
                hidden_since,
                triggered,
                reply,
            } => self.auto_show(app, logger, cycle, hidden_since, triggered, reply)?,
            Action::AutoHide {
                cycle,
                session,
                reply,
            } if self.mode == Mode::Auto
                && session == self.session
                && self.auto_visible.is_some_and(|auto| auto.cycle == cycle) =>
            {
                self.hide_auto(logger, Some(reply))?;
            }
            Action::AutoDone { cycles } if self.auto_enabled && self.mode == Mode::Hidden => {
                self.auto_stop.take();
                self.log(logger, "autocycle_done", json!({"steps":cycles}));
            }
            Action::Toggle {
                started,
                target,
                alt,
            } if !self.auto_enabled => self.toggle(app, logger, started, target, alt)?,
            Action::FrontendReady => self.frontend_ready(app, logger)?,
            Action::ForegroundRevision { session, revision } if self.current(session) => {
                self.emit("panel:foreground-revision", json!({"revision":revision}));
            }
            Action::RetryActivation { session }
                if self.current(session)
                    && !self.pasting
                    && self.interaction == Interaction::ActivationFailed =>
            {
                self.activate_restricted(logger)?;
            }
            Action::ReturnForPaste {
                session,
                permit,
                passive,
                policy,
                requested,
                reply,
            } => {
                let request = ReturnRequest {
                    session,
                    permit,
                    passive,
                    policy,
                    requested,
                    reply,
                };
                self.return_focus(app, logger, request)?
            }
            Action::Key { session, key }
                if self.current(session)
                    && self.mode == Mode::Quick
                    && self.interaction == Interaction::Quick =>
            {
                // Space / Esc 都先交给前端：有浮层时 Esc 只关浮层；无浮层时前端沿用 hide_panel 关闭。
                // 不在这里按 Esc 直接收起，否则后端会抢在分层键位处理之前结束会话。
                self.emit("panel:key", json!({"key":key}));
            }
            Action::Search { session, started }
                if self.current(session)
                    && self.hint.is_none()
                    && matches!(
                        self.interaction,
                        Interaction::Quick | Interaction::Activated
                    ) =>
            {
                self.begin_search(app, logger, started)?
            }
            // 管理员提示停留期间不切搜索模式（不抢焦点、不开新的键处理）：快速模式下钩子已把这些键吞掉并
            // 缓冲，收起时按原逻辑作废并记入 search_replay。
            Action::Search { session, .. } if self.current(session) => {
                self.log(logger, "search_ignored", json!({"reason":"elevated_hint"}));
            }
            Action::SearchActivated { session, success }
                if self.current(session) && self.mode == Mode::Search =>
            {
                self.log(
                    logger,
                    "search_foreground_wait",
                    json!({"foreground_ok":success}),
                );
                if success {
                    self.foreground_taken = true;
                    self.prepare_search()?;
                } else {
                    self.log(
                        logger,
                        "hide_source",
                        json!({"source":"search_activation_failed"}),
                    );
                    self.hide(logger)?;
                }
            }
            Action::SearchReady { session }
                if self.current(session) && self.mode == Mode::Search =>
            {
                self.finish_search(logger, true)?
            }
            Action::SearchTimeout { session }
                if self.current(session) && self.mode == Mode::Search =>
            {
                self.finish_search(logger, false)?
            }
            // 前端的 hide_panel（lib.rs 已记 hide_source=frontend）。
            Action::Hide { session } if self.current(session) => {
                self.end_hint("frontend");
                self.hide(logger)?;
            }
            // 点面板外：hide_source=outside_click 已由钩子回调记下，收起和原来走 Hide 时一样。
            Action::OutsideClick { session } if self.current(session) => {
                self.end_hint("outside_click");
                self.hide(logger)?;
            }
            // 管理员提示停留期间的按键（M0-6b）：放行给目标的键（快速模式的组合键，key_passthrough）按下时就报；
            // 吞掉的键（快速模式的普通键、搜索模式的键，key_consumed；Esc）抬起时才报，抬起也已吞掉；还有被吞的键
            // 按着时钩子会等最后一个抬起再报。这里立即收起，焦点按原逻辑还。停留已结束（到点、其他路径先收了）就不再处理。
            Action::HintKey { session, end } if self.current(session) && self.dwelling() => {
                self.log(
                    logger,
                    "hide_source",
                    json!({"source":hint_key_hide_source(end)}),
                );
                self.end_hint(end.name());
                self.hide(logger)?;
            }
            Action::Paste {
                session,
                id,
                mode,
                selection,
                started,
            } if self.current(session)
                && !self.pasting
                && self.hint.is_none()
                && matches!(
                    self.interaction,
                    Interaction::Quick | Interaction::Activated
                ) =>
            {
                let command = PasteCommand {
                    session,
                    id,
                    mode,
                    selection,
                    started,
                };
                self.begin_paste(app, logger, command)?
            }
            Action::StartQueue {
                session,
                ids,
                automatic,
                mode,
                selection,
            } if self.current(session)
                && !self.pasting
                && self.hint.is_none()
                && self.queue.is_none()
                && matches!(self.mode, Mode::Quick | Mode::Search)
                && matches!(
                    self.interaction,
                    Interaction::Quick | Interaction::Activated
                ) =>
            {
                let command = StartQueueCommand {
                    session,
                    ids,
                    automatic,
                    mode,
                    selection,
                };
                self.start_queue(app, logger, command)?
            }
            Action::QueuePaste {
                session,
                generation,
            } if session == self.session
                && self.mode == Mode::Queue
                && self
                    .queue
                    .as_ref()
                    .is_some_and(|queue| queue.generation == generation) =>
            {
                self.begin_queue_item(app, logger)?
            }
            Action::QueueTick {
                session,
                generation,
            } if session == self.session
                && self.mode == Mode::Queue
                && self
                    .queue
                    .as_ref()
                    .is_some_and(|queue| queue.generation == generation) =>
            {
                self.tick_queue(app, logger, generation)?
            }
            Action::QueueClipboard {
                session,
                generation,
            } if session == self.session
                && self.mode == Mode::Queue
                && self
                    .queue
                    .as_ref()
                    .is_some_and(|queue| queue.generation == generation) =>
            {
                self.inspect_queue_clipboard(logger, generation)?
            }
            Action::QueueCancel { session }
                if session == self.session && self.mode == Mode::Queue =>
            {
                if !self.stop_queue(logger, Stop::Escape)? {
                    // 停留期间点 ×：直接收起。
                    self.end_queue_window(logger)?;
                }
            }
            Action::QueueDwellEnd { session, serial }
                if session == self.session
                    && self.mode == Mode::Queue
                    && self.queue_dwell == Some(serial) =>
            {
                self.end_queue_window(logger)?;
            }
            Action::QueueFinished {
                session,
                generation,
                id,
                result,
            } => self.finish_queue_item(app, logger, session, generation, id, result)?,
            Action::SetHitRegions {
                session,
                regions,
                armed,
                avoid,
            } => self.set_hit_regions(app, logger, session, &regions, armed, avoid),
            Action::Glide { session, serial } if session == self.session => {
                self.step_glide(logger, serial);
            }
            Action::PasteSent { session } if session == self.session => {
                self.pasting = false;
            }
            Action::PasteFinished { session, result } => {
                self.finish_paste(app, logger, session, result)?
            }
            Action::HintElapsed { session }
                if self.current(session)
                    && self.hint.as_ref().is_some_and(|hint| hint.dwell.is_some()) =>
            {
                self.log(
                    logger,
                    "hide_source",
                    json!({"source":"elevated_hint_timeout"}),
                );
                self.end_hint("timeout");
                self.hide(logger)?;
            }
            // 停留期间不再发起粘贴（前端 busy 也保持着，这里是兜底）。
            Action::Paste { session, .. } if self.current(session) && self.hint.is_some() => {
                self.log(logger, "paste_ignored", json!({"reason":"elevated_hint"}));
            }
            Action::Painted { session, received } if self.current(session) && !self.painted => {
                if let (Some(started), Some(shown)) = (self.started, self.shown) {
                    self.painted = true;
                    let first = shown.duration_since(started).as_secs_f64() * 1000.0;
                    let second = received.saturating_duration_since(shown).as_secs_f64() * 1000.0;
                    self.log(logger, "painted", json!({
                        "paint_time_ms":now_ms(),"hotkey_to_show_ms":first,"show_to_paint_ms":second,
                        "auto":self.auto_visible.is_some(),
                        "cycle":self.auto_visible.map(|auto|auto.cycle),
                        "hidden_ms":self.auto_visible.map(|auto|auto.hidden_ms),
                        "trigger_to_paint_ms":received.saturating_duration_since(started).as_secs_f64()*1000.0,
                    }));
                    self.emit(
                        "panel:timing",
                        json!({"hotkeyToShowMs":first,"showToPaintMs":second}),
                    );
                }
            }
            Action::Resuspend { interval, reply }
                if self.mode == Mode::Hidden && self.ready && !self.pasting =>
            {
                // 条件不满足时落到 _，reply 被丢弃，线程便知道这次跳过了。
                if let Some(generation) = webview::resuspend(
                    &self.window,
                    logger.clone(),
                    self.session,
                    self.memory.clone(),
                    interval,
                )? {
                    let _ = reply.send((self.session, generation));
                }
            }
            Action::Quit | Action::Relaunch => {
                let relaunch = matches!(action, Action::Relaunch);
                self.quit(app, logger, relaunch);
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(
        result: Option<PasteResult>,
        reason: Option<&str>,
        written: bool,
        cancelled: bool,
    ) -> Outcome {
        let mut outcome =
            Outcome::from_report(result, json!({"clipboard_written":written,"reason":reason}));
        outcome.request_cancelled = cancelled;
        outcome
    }

    // 平台层的做法：结论和原因映射写进报告，再变成有类型的结果。
    fn reported(
        result: impl FnOnce(&mut Value) -> PasteResult,
        written: bool,
        cancelled: bool,
    ) -> Outcome {
        let mut report = json!({"clipboard_written":written,"request_cancelled":false});
        let result = result(&mut report);
        let mut outcome = Outcome::from_report(Some(result), report);
        outcome.request_cancelled = cancelled;
        outcome
    }

    #[test]
    fn generic_terminal_results_are_consumed_for_visible_hidden_cancelled_and_new_sessions() {
        use crate::paste::{
            generic::{Completion, Submission},
            request::generic_outcome,
        };
        for written in [false, true] {
            for completion in [
                Err("clipboard_allocate_failed".to_owned()),
                Err("clipboard_capture_busy".to_owned()),
                Ok(Completion::NotSubmitted("generic_clipboard_changed")),
                Ok(Completion::NotSubmitted("generic_clipboard_busy")),
                Ok(Completion::NotSubmitted("generic_clipboard_close_failed")),
                Ok(Completion::Submitted(Submission::Unknown)),
            ] {
                let mut result =
                    reported(|report| generic_outcome(completion, report), written, false);
                for visible in [false, true] {
                    assert_eq!(
                        generic_submission_finish(&result, true, visible, Interaction::Activated),
                        Some(if visible {
                            NoSubmissionFinish::CloseOnly
                        } else {
                            NoSubmissionFinish::ShowCloseOnly
                        })
                    );
                    assert_eq!(
                        generic_submission_finish(&result, false, visible, Interaction::Activated),
                        Some(NoSubmissionFinish::Ignore)
                    );
                    let message = no_submission_message(result.reason.as_ref());
                    assert!(!message.contains("已复制"));
                    assert!(!message.contains("Ctrl+V"));
                }
                result.request_cancelled = true;
                assert_eq!(
                    generic_submission_finish(&result, true, false, Interaction::Passive),
                    Some(NoSubmissionFinish::Ignore)
                );
                assert_eq!(
                    generic_submission_finish(&result, true, true, Interaction::Passive),
                    Some(NoSubmissionFinish::CloseRequested)
                );
            }
        }
        let result = reported(
            |report| generic_outcome(Ok(Completion::Submitted(Submission::Sent)), report),
            true,
            false,
        );
        assert_eq!(
            generic_submission_finish(&result, true, false, Interaction::Activated),
            None
        );
        assert_eq!(
            no_submission_message(Some(&Reason::GenericSubmissionUnknown)),
            "粘贴结果未确认，请先检查目标，避免重复粘贴"
        );
    }

    #[test]
    fn unwritten_busy_is_close_only_but_requested_close_and_new_session_win() {
        let mut result = outcome(None, Some("clipboard_capture_busy"), false, false);
        assert_eq!(
            no_submission_finish(&result, true, Interaction::Activated),
            Some(NoSubmissionFinish::CloseOnly)
        );
        // 即使busy已产出，UI接收前的Esc也以当前取消标记为准。
        result.request_cancelled = true;
        assert_eq!(
            no_submission_finish(&result, true, Interaction::Passive),
            Some(NoSubmissionFinish::CloseRequested)
        );
        assert_eq!(
            no_submission_finish(&result, false, Interaction::Passive),
            None
        );
        result.reason = Some(Reason::RequestCancelled);
        assert_eq!(
            no_submission_finish(&result, true, Interaction::Passive),
            Some(NoSubmissionFinish::CloseRequested)
        );
        result.clipboard_written = true;
        assert_eq!(
            no_submission_finish(&result, true, Interaction::Passive),
            None
        );
        assert!(!Interaction::Passive.may_restore());
        assert!(!Interaction::CloseOnly.may_restore());
    }

    #[test]
    fn written_native_not_submitted_result_keeps_only_a_close_entry() {
        let result = outcome(
            Some(PasteResult::NativeNotSubmitted),
            Some("native_clipboard_busy"),
            true,
            false,
        );
        assert_eq!(
            no_submission_finish(&result, true, Interaction::Activated),
            Some(NoSubmissionFinish::CloseOnly)
        );
    }

    #[test]
    fn native_not_submitted_always_consumes_the_result_before_frontend_paste_result() {
        for written in [false, true] {
            for current in [false, true] {
                for cancelled in [false, true] {
                    for interaction in [
                        Interaction::Quick,
                        Interaction::ActivationPending,
                        Interaction::ActivationFailed,
                        Interaction::Activated,
                        Interaction::Passive,
                        Interaction::CloseOnly,
                    ] {
                        let result = outcome(
                            Some(PasteResult::NativeNotSubmitted),
                            Some("native_clipboard_busy"),
                            written,
                            cancelled,
                        );
                        let expected =
                            if !current || (cancelled && interaction != Interaction::Passive) {
                                NoSubmissionFinish::Ignore
                            } else if cancelled {
                                NoSubmissionFinish::CloseRequested
                            } else {
                                NoSubmissionFinish::CloseOnly
                            };
                        // Some 的三个分支均在实际 PasteFinished 处理里 early return，绝不 emit 新前端类型。
                        assert_eq!(
                            no_submission_finish(&result, current, interaction),
                            Some(expected)
                        );
                    }
                }
            }
        }
        for reason in [
            "native_clipboard_busy",
            "clipboard_capture_busy",
            "clipboard_busy",
            "native_clipboard_close_failed",
        ] {
            assert_eq!(
                no_submission_message(Some(&Reason::from(reason))),
                "剪贴板忙，本次未粘贴，请关闭面板后重新呼出"
            );
        }
        assert_eq!(
            no_submission_message(Some(&Reason::NativeClipboardChanged)),
            "剪贴板已变化，本次未粘贴，请关闭面板后重新呼出"
        );
    }

    #[test]
    fn written_cancelled_native_result_hides_quietly_and_late_session_is_ignored() {
        let result = outcome(
            Some(PasteResult::NativeNotSubmitted),
            Some("request_cancelled"),
            true,
            true,
        );
        assert_eq!(
            no_submission_finish(&result, true, Interaction::Passive),
            Some(NoSubmissionFinish::CloseRequested)
        );
        assert_eq!(
            no_submission_finish(&result, false, Interaction::Passive),
            Some(NoSubmissionFinish::Ignore)
        );
        assert!(!Interaction::Passive.may_restore());
        assert!(!Interaction::CloseOnly.may_restore());
    }

    #[test]
    fn actual_native_error_shapes_are_terminal_without_reopening_a_legacy_retry() {
        use crate::paste::{
            native::{Completion, Submission},
            request::native_outcome,
        };
        for written in [false, true] {
            for reason in [
                "clipboard_capture_busy",
                "clipboard_busy",
                "clipboard_write_failed",
                "record_missing",
                "request_cancelled",
            ] {
                let cancelled = reason == "request_cancelled";
                // execute 使用同一个映射，覆盖锁前/Service.write Err，而不是只构造新的结论。
                let result = reported(
                    |report| native_outcome(Err(reason.to_owned()), report),
                    written,
                    cancelled,
                );
                assert_eq!(result.clipboard_written, written);
                assert_eq!(result.reason, Some(Reason::from(reason)));
                assert_eq!(result.result, Some(PasteResult::NativeNotSubmitted));
                assert_eq!(
                    no_submission_finish(
                        &result,
                        true,
                        if cancelled {
                            Interaction::Passive
                        } else {
                            Interaction::Quick
                        }
                    ),
                    Some(if cancelled {
                        NoSubmissionFinish::CloseRequested
                    } else {
                        NoSubmissionFinish::CloseOnly
                    })
                );
                assert_eq!(
                    no_submission_finish(&result, false, Interaction::Quick),
                    Some(NoSubmissionFinish::Ignore)
                );
            }
        }
        for submission in [Submission::Sent, Submission::Unknown] {
            let result = reported(
                |report| native_outcome(Ok(Completion::Submitted(submission)), report),
                true,
                true,
            );
            assert_eq!(
                result.result,
                Some(if submission == Submission::Sent {
                    PasteResult::Sent
                } else {
                    PasteResult::NativeSubmissionUnknown
                })
            );
            assert_eq!(
                no_submission_finish(&result, true, Interaction::Passive),
                None
            );
        }
        // Legacy 没有调用 native_outcome，普通写失败继续走原有错误提示。
        assert_eq!(
            no_submission_finish(
                &outcome(None, Some("clipboard_write_failed"), false, false),
                true,
                Interaction::Quick
            ),
            None
        );
    }

    #[test]
    fn only_elevated_copy_only_results_keep_the_panel() {
        assert_eq!(ELEVATED_HINT_DWELL, Duration::from_secs(3));
        assert_eq!(
            dwell_after_paste(&outcome(
                Some(PasteResult::CopiedOnly),
                Some("target_may_be_elevated"),
                false,
                false
            )),
            Some(ELEVATED_HINT_DWELL)
        );
        // 其他 copied_only 原因保持现状，不停留。
        for reason in [
            "external_switch",
            "invalid_target",
            "preflight_timeout",
            "conditions_changed",
            "send_failed",
            "user_copy",
        ] {
            assert_eq!(
                dwell_after_paste(&outcome(
                    Some(PasteResult::CopiedOnly),
                    Some(reason),
                    false,
                    false
                )),
                None,
                "{reason}"
            );
        }
        // 真发了键、或 Shift+Enter 只复制：不管原因字段写什么都不停留。
        for result in [
            PasteResult::Copied,
            PasteResult::Sent,
            PasteResult::Confirmed,
        ] {
            assert_eq!(
                dwell_after_paste(&outcome(
                    Some(result),
                    Some("target_may_be_elevated"),
                    false,
                    false
                )),
                None,
                "{result:?}"
            );
        }
        // 没写进剪贴板（记录不存在、读库失败）和线程报错时结论为空，不停留。
        assert_eq!(
            dwell_after_paste(&Outcome::not_written(Reason::RecordMissing)),
            None
        );
        assert_eq!(dwell_after_paste(&outcome(None, None, false, false)), None);
        assert_eq!(
            dwell_after_paste(&{
                let mut result = outcome(Some(PasteResult::Copied), None, true, false);
                result.passive_hint = true;
                result
            }),
            Some(ELEVATED_HINT_DWELL)
        );
        assert_eq!(
            dwell_after_paste(&{
                let mut result = outcome(
                    Some(PasteResult::CopiedOnly),
                    Some("target_may_be_elevated"),
                    true,
                    true,
                );
                result.passive_hint = true;
                result
            }),
            None
        );
    }

    #[test]
    fn hide_is_deferred_only_when_perform_would_reach_the_integrity_check() {
        let higher = json!({"status":"higher"});
        let unknown = json!({"status":"unknown_assume_higher"});
        let allowed = json!({"status":"allowed"});
        assert!(defers_hide_for_elevated(
            PasteMode::Paste,
            true,
            false,
            &higher
        ));
        assert!(defers_hide_for_elevated(
            PasteMode::Text,
            true,
            false,
            &higher
        ));
        // 查不到目标权限时 perform 也按"可能更高"降级，预判要一致。
        assert!(defers_hide_for_elevated(
            PasteMode::Paste,
            true,
            false,
            &unknown
        ));
        assert!(defers_hide_for_elevated(
            PasteMode::Paste,
            true,
            false,
            &json!({})
        ));
        // Shift+Enter 只复制、目标已失效、用户切走：perform 在完整性比较之前就返回，照旧立即收起。
        assert!(!defers_hide_for_elevated(
            PasteMode::Copy,
            true,
            false,
            &higher
        ));
        assert!(!defers_hide_for_elevated(
            PasteMode::Paste,
            false,
            false,
            &higher
        ));
        assert!(!defers_hide_for_elevated(
            PasteMode::Paste,
            true,
            true,
            &higher
        ));
        assert!(!defers_hide_for_elevated(
            PasteMode::Paste,
            true,
            false,
            &allowed
        ));
        assert!(!defers_hide_for_elevated(
            PasteMode::Text,
            true,
            false,
            &allowed
        ));
    }

    #[test]
    fn hint_hidden_fields_tell_timeout_from_early_hide() {
        let early = hint_hidden_fields(ELEVATED_HINT_DWELL, Duration::from_millis(1200));
        assert_eq!(
            early,
            json!({"reason":"target_may_be_elevated","dwell_ms":3000,"shown_ms":1200.0,"timed_out":false})
        );
        assert_eq!(
            hint_hidden_fields(ELEVATED_HINT_DWELL, Duration::from_millis(3004))["timed_out"],
            true
        );
        assert_eq!(
            hint_hidden_fields(ELEVATED_HINT_DWELL, ELEVATED_HINT_DWELL)["timed_out"],
            true
        );
    }

    // M0-6b：停留结束原因写进 elevated_hint_hidden，原有字段不变；不记按的是哪个键。
    #[test]
    fn hint_ended_fields_add_the_reason_only() {
        let shown = Duration::from_millis(800);
        for ended_by in [
            "escape",
            "hotkey",
            "key_passthrough",
            "key_consumed",
            "outside_click",
            "timeout",
        ] {
            let fields = hint_ended_fields(ELEVATED_HINT_DWELL, shown, Some(ended_by));
            let mut expected = hint_hidden_fields(ELEVATED_HINT_DWELL, shown);
            expected["ended_by"] = json!(ended_by);
            assert_eq!(fields, expected, "{ended_by}");
        }
        assert_eq!(
            hint_ended_fields(ELEVATED_HINT_DWELL, shown, None)["ended_by"],
            Value::Null
        );
        assert_eq!(
            HintEnd::Escape.name(),
            "escape",
            "钩子报的 Esc 和快速模式原路径记同一个 ended_by"
        );
        assert_eq!(HintEnd::KeyPassthrough.name(), "key_passthrough");
        assert_eq!(HintEnd::KeyConsumed.name(), "key_consumed");
    }

    #[test]
    fn hint_key_hide_sources() {
        assert_eq!(hint_key_hide_source(HintEnd::Escape), "escape_key");
        assert_eq!(
            hint_key_hide_source(HintEnd::KeyPassthrough),
            "elevated_hint_key"
        );
        assert_eq!(
            hint_key_hide_source(HintEnd::KeyConsumed),
            "elevated_hint_key"
        );
    }

    #[test]
    fn all_normal_hint_closes_drain_keys_and_keep_the_first_reason() {
        for reason in [
            "timeout",
            "outside_click",
            "hotkey",
            "frontend",
            "escape",
            "key_consumed",
            "key_passthrough",
        ] {
            assert!(hint_needs_key_drain(true, Some(reason)));
            assert!(!hint_needs_key_drain(false, Some(reason)));
            assert_eq!(hint_end_reason(None, reason), reason);
            assert_eq!(hint_end_reason(Some(reason), "key_consumed"), reason);
            assert_eq!(hint_end_reason(Some(reason), "quit"), "quit");
        }
        assert!(hint_needs_key_drain(true, None));
        assert!(
            !hint_needs_key_drain(true, Some("quit")),
            "明确退出不能等松键"
        );
    }

    #[test]
    fn only_a_new_non_empty_avoid_shape_is_considered() {
        let shape = |top| window::NativeRegion {
            left: 0,
            top,
            right: 960,
            bottom: 530,
        };
        assert_eq!(avoid_update(Some(shape(35)), None), None, "没有 avoid 不挪");
        assert_eq!(avoid_update(None, None), None);
        assert_eq!(
            avoid_update(Some(shape(35)), Some(shape(35))),
            None,
            "与上次相同不挪"
        );
        assert_eq!(
            avoid_update(Some(shape(35)), Some(shape(60))),
            Some(shape(60))
        );
        assert_eq!(avoid_update(None, Some(shape(60))), Some(shape(60)));
    }

    #[test]
    fn only_explained_queue_stops_dwell() {
        use crate::paste::queue::Stop;
        for reason in [
            Stop::Timeout,
            Stop::ItemTimeout,
            Stop::ClipboardChanged,
            Stop::WriteFailed,
            Stop::SubmissionUncertain,
            Stop::Elevated,
        ] {
            assert!(queue_stop_dwells(reason.message()), "{reason:?}");
        }
        for reason in [Stop::Done, Stop::Cancelled, Stop::Escape] {
            assert!(!queue_stop_dwells(reason.message()), "{reason:?}");
        }
        assert!(queue_stop_dwells(Some("这条记录已不在历史里，已跳过这条")));
    }

    #[test]
    fn glide_eases_out_and_lands_exactly_on_target() {
        assert_eq!(glide_position((0, 0), (100, -40), 0.0), (0, 0));
        assert_eq!(glide_position((0, 0), (100, -40), 1.0), (100, -40));
        assert_eq!(glide_position((0, 0), (100, -40), 7.0), (100, -40));
        let half = glide_position((0, 0), (100, 0), 0.5);
        assert!(half.0 > 50, "缓出：前半程走得多 {half:?}");
    }

    // 问题 8：前台是拈来自己（还回目标失败）或没有前台时，这张不写剪贴板、可重试。
    #[test]
    fn queue_refuses_own_or_missing_target() {
        let own = 4242;
        let target = |window, process| Target {
            window,
            process,
            ..Target::default()
        };
        assert!(queue_target_rejected(&target(0, 77), own));
        assert!(queue_target_rejected(&target(10, 0), own));
        assert!(queue_target_rejected(&target(10, own), own));
        assert!(!queue_target_rejected(&target(10, 77), own));
    }

    #[test]
    fn conceal_stops_webview_before_hiding_the_native_window() {
        use std::cell::RefCell;

        let calls = RefCell::new(Vec::new());
        hide_webview_then_native(
            || {
                calls.borrow_mut().push("webview");
                Ok(())
            },
            || {
                calls.borrow_mut().push("native");
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(calls.into_inner(), ["webview", "native"]);
    }

    #[test]
    fn conceal_still_hides_native_window_when_webview_stop_fails() {
        use std::cell::RefCell;

        let calls = RefCell::new(Vec::new());
        let result = hide_webview_then_native(
            || {
                calls.borrow_mut().push("webview");
                Err("webview failed".into())
            },
            || {
                calls.borrow_mut().push("native");
                Ok(())
            },
        );
        assert_eq!(result, Err("webview failed".into()));
        assert_eq!(calls.into_inner(), ["webview", "native"]);
    }

    #[test]
    fn stamp_adds_session_and_target_without_touching_other_fields() {
        let target = Target {
            process_name: "notepad.exe".into(),
            focus_class: "Edit".into(),
            ..Target::default()
        };
        let row = stamp(
            json!({"stage":"panel_before_replay","ime":{"open":1}}),
            7,
            &target,
        );
        assert_eq!(
            row,
            json!({"stage":"panel_before_replay","ime":{"open":1},"session":7,
                "process_name":"notepad.exe","focus_class":"Edit","focus_unknown":true})
        );
    }
}
