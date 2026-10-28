use crate::{
    autocycle,
    history::{History, Snapshot},
    log::{Logger, now_ms},
    paste::{
        keys::Table,
        permit::{Monitor, Permit, Selection, Session as PermitSession, reliable_control},
        request::{Callbacks, Mode as PasteMode, Request, RestorePolicy},
    },
    platform::{self, HintEnd, HookEvent, Hooks, REPLAY_WAIT, Target, ime, paste, webview, window},
    resuspend,
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
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
    Ignore,
}

fn no_submission_finish(
    result: &Value,
    current: bool,
    interaction: Interaction,
) -> Option<NoSubmissionFinish> {
    let native = result["paste_result"] == "native_not_submitted";
    if !current {
        return native.then_some(NoSubmissionFinish::Ignore);
    }
    if !native && result["clipboard_written"] != false {
        return None;
    }
    if result["request_cancelled"] == true {
        return if interaction == Interaction::Passive {
            Some(NoSubmissionFinish::CloseRequested)
        } else {
            native.then_some(NoSubmissionFinish::Ignore)
        };
    }
    (native || result["reason"] == "clipboard_capture_busy")
        .then_some(NoSubmissionFinish::CloseOnly)
}

fn no_submission_message(result: &Value) -> &'static str {
    match result["reason"].as_str() {
        Some("native_clipboard_changed") => "剪贴板已变化，本次未粘贴，请关闭面板后重新呼出",
        Some(
            "clipboard_capture_busy"
            | "clipboard_busy"
            | "native_clipboard_busy"
            | "native_clipboard_close_failed",
        ) => "剪贴板忙，本次未粘贴，请关闭面板后重新呼出",
        Some("record_missing") => "这条记录已不在历史里，本次未粘贴，请关闭面板后重新呼出",
        Some(
            "request_cancelled"
            | "native_input_desktop_unavailable"
            | "invalid_target"
            | "selection_boundary_changed"
            | "native_modifiers_held"
            | "target_may_be_elevated"
            | "native_target_unavailable",
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

// 实验开关：快速模式下让搜索框持有页面内焦点。启动时读一次设置，随 panel:shown 发给前端。
#[derive(Clone, Copy, Default)]
pub struct Experiments {
    pub search_box_focus: bool,
}
impl Experiments {
    pub fn from_settings(settings: &crate::settings::Experiments, logger: &Logger) -> Self {
        let experiments = Self {
            search_box_focus: settings.searchbox_focus,
        };
        logger.event(
            "experiment_searchbox_focus",
            json!({"enabled":experiments.search_box_focus}),
        );
        experiments
    }
    fn payload(self) -> Value {
        json!({"searchBoxFocus":self.search_box_focus})
    }
}
// 保留旧开关解析的既有回归测试；运行时改用设置中的布尔值。
#[cfg(test)]
fn flag_enabled(value: Option<&str>) -> bool {
    value.map(str::trim) == Some("1")
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
fn dwell_after_paste(result: &Value) -> Option<Duration> {
    if result["request_cancelled"] == true {
        return None;
    }
    ((result["passive_hint"] == true && result["clipboard_written"] == true)
        || (result["paste_result"] == "copied_only"
            && result["reason"] == "target_may_be_elevated"))
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

pub struct Controller {
    inner: Mutex<Panel>,
    pub logger: Logger,
    // 字段按声明顺序析构：没走 shutdown 就退出时（例如启动失败），也要先停采集再停剪贴板线程。
    capture: Mutex<Option<platform::capture::Service>>,
    clipboard: platform::clipboard::Service,
    keys: Arc<Table>,
    stopping: Arc<AtomicBool>,
    history: Arc<History>,
}
struct Panel {
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
    // 管理员提示：粘贴预判为"目标可能是管理员"后面板不收起，这里记着停留状态；None 表示没在停留。
    hint: Option<Hint>,
    auto_enabled: bool,
    auto_plan: Option<autocycle::Plan>,
    auto_stop: Option<mpsc::Sender<()>>,
    auto_visible: Option<AutoVisible>,
    resuspend: Option<Duration>,
    resuspend_stop: Option<mpsc::Sender<()>>,
    experiments: Experiments,
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
        result: Value,
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
    QuitFrontend,
}

impl Controller {
    pub fn new(
        window: WebviewWindow,
        logger: Logger,
        plan: Option<autocycle::Plan>,
        resuspend: Option<Duration>,
        experiments: Experiments,
    ) -> platform::Result<Self> {
        let history = Arc::new(History::new(platform::hash::sha256, logger.clone()));
        let clipboard = platform::clipboard::Service::start(logger.clone(), history.clone())?;
        let keys = Arc::new(Table::load(&logger));
        Ok(Self {
            logger,
            clipboard,
            keys,
            stopping: Arc::new(AtomicBool::new(false)),
            history,
            capture: Mutex::new(None),
            inner: Mutex::new(Panel {
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
                hint: None,
                auto_enabled: plan.is_some(),
                auto_plan: plan,
                auto_stop: None,
                auto_visible: None,
                resuspend,
                resuspend_stop: None,
                experiments,
            }),
        })
    }
    pub fn set_hotkey(&self, hotkey: Shortcut) {
        self.inner.lock().unwrap().hotkey = Some(hotkey);
    }
    pub fn start_capture(&self, app: &AppHandle, automatic: bool) -> platform::Result<()> {
        self.history.initialize();
        let app = app.clone();
        let changed: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = dispatch(&app, Action::HistoryChanged);
        });
        changed();
        self.history.maintenance(changed.clone());
        let mut capture = self.capture.lock().unwrap();
        if self.stopping.load(Ordering::Acquire) {
            return Ok(());
        }
        *capture = Some(platform::capture::Service::start(
            self.logger.clone(),
            self.history.clone(),
            &self.clipboard,
            automatic,
            changed,
        )?);
        Ok(())
    }
    pub fn history_snapshot(&self) -> Snapshot {
        self.history.snapshot()
    }
    pub fn search(
        &self,
        query: &str,
        request_id: u64,
    ) -> Result<crate::history::SearchReply, String> {
        self.history.search(query, request_id)
    }
    pub fn shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(capture) = self.capture.lock().unwrap().take() {
            capture.shutdown();
        }
        if let Ok(mut panel) = self.inner.lock() {
            panel.auto_stop.take();
            panel.resuspend_stop.take();
            if let Err(error) = panel.replay_search(&self.logger, false, "shutdown", false) {
                panel.log(
                    &self.logger,
                    "replay_cleanup_failed",
                    json!({"error":error}),
                );
            }
            panel.hooks.take();
        }
        self.clipboard.shutdown();
        self.history.summary();
        self.logger.event("shutdown", json!({}));
        self.logger.flush();
    }
}

pub fn dispatch(app: &AppHandle, action: Action) -> Result<(), String> {
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let controller = handle.state::<Controller>();
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

impl Panel {
    fn log(&self, logger: &Logger, event: &str, fields: Value) {
        logger.event(event, stamp(fields, self.session, &self.target));
    }
    fn emit(&self, event: &str, mut payload: Value) {
        payload["session"] = json!(self.session);
        let _ = self.window.emit(event, payload);
    }
    fn current(&self, session: u64) -> bool {
        session == self.session && self.mode != Mode::Hidden
    }
    fn shown_event(&self, app: &AppHandle) {
        if let (Some(started), Some(shown)) = (self.started, self.shown) {
            // 历史随呼出事件一起发：前端不用再取一次，呼出后马上按键也拿到的是最新列表。
            let history = app.state::<Controller>().history_snapshot();
            self.emit(
                "panel:shown",
                json!({"hotkeyToShowMs":shown.duration_since(started).as_secs_f64()*1000.0,"history":history,
                    "experiments":self.experiments.payload(),"interaction":self.interaction,
                    "foregroundRevision":self.monitor.as_ref().map_or(0, |monitor| monitor.revision())}),
            );
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
        if automatic.is_some() {
            self.mode = Mode::Auto;
            window::show_auto(&self.window)?;
        } else {
            self.mode = Mode::Quick;
            window::show(&self.window, &self.target)?;
        }
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
                self.hotkey.ok_or("呼出键尚未注册")?,
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
        window::hide(&self.window)?;
        let visible_ms = self
            .shown
            .map(|shown| shown.elapsed().as_secs_f64() * 1000.0);
        self.mode = Mode::Hidden;
        self.auto_visible = None;
        self.emit("panel:hidden", json!({}));
        webview::set_visible_with_completion(
            &self.window,
            false,
            logger.clone(),
            self.session,
            self.memory.clone(),
            ready,
        )?;
        self.log(
            logger,
            "hidden",
            json!({"auto":true,"cycle":cycle,"visible_ms":visible_ms}),
        );
        Ok(())
    }
    fn hide(&mut self, logger: &Logger) -> platform::Result<bool> {
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
        window::hide(&self.window)?;
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
        self.emit("panel:hidden", json!({}));
        webview::set_visible(
            &self.window,
            false,
            logger.clone(),
            self.session,
            self.memory.clone(),
        )?;
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
    fn act(&mut self, app: &AppHandle, logger: &Logger, action: Action) -> platform::Result<()> {
        if self.auto_enabled {
            let command = match &action {
                Action::Paste { .. } => Some("paste_item"),
                Action::Search { .. } => Some("start_search"),
                Action::SearchReady { .. } => Some("search_ready"),
                Action::Hide { .. } => Some("hide_panel"),
                Action::QuitFrontend => Some("quit_prototype"),
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
            } => {
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
            }
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
            } if !self.auto_enabled => {
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
            }
            Action::FrontendReady => {
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
                        window::hide(&self.window)?;
                        webview::set_visible_with_completion(
                            &self.window,
                            false,
                            logger.clone(),
                            self.session,
                            self.memory.clone(),
                            Some(ready),
                        )?;
                        self.auto_stop =
                            Some(autocycle::spawn(app.clone(), logger.clone(), plan, hidden)?);
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
            }
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
                                    logger,
                                    session,
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
                        window::hide(&self.window)?;
                        self.emit("panel:hidden", json!({}));
                        webview::set_visible(
                            &self.window,
                            false,
                            logger.clone(),
                            session,
                            self.memory.clone(),
                        )?;
                        self.log(logger, "hidden", json!({"source":"paste"}));
                    }
                }
                let _ = reply.send(focus);
            }
            Action::Key { session, key }
                if self.current(session)
                    && self.mode == Mode::Quick
                    && self.interaction == Interaction::Quick =>
            {
                self.emit("panel:key", json!({"key":key}));
                if key == "Escape" {
                    self.log(logger, "hide_source", json!({"source":"escape_key"}));
                    self.end_hint("escape");
                    self.hide(logger)?;
                }
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
                        Some(selection) => {
                            match permits.select(session, id, mode, selection, foreground) {
                                Ok(permit) => Some(permit),
                                Err(reason) => {
                                    self.log(logger, "paste_rejected", json!({"reason":reason}));
                                    self.emit("panel:error", json!({"message":"选择期间窗口状态已变化，请重新选择条目。"}));
                                    return Ok(());
                                }
                            }
                        }
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
                let worker_logger = logger.clone();
                let app = app.clone();
                if std::thread::Builder::new().name("paste-request".into()).spawn(move || {
                    // 监听线程仍在 worker 中存活；只先停止输入拦截，读库和发送期间继续记录前台。
                    let drain = || {
                        let Some(hooks) = hooks.as_ref() else { return false; };
                        loop {
                            if stopping.load(Ordering::Acquire) { return false; }
                            let abandon = cancelled.load(Ordering::Acquire)
                                || !permit.as_ref().is_some_and(|permit| permit.current(&target, window::foreground()));
                            match hooks.stop_inputs(abandon) {
                                Ok(true) => return hooks.synchronize() && !abandon,
                                Ok(false) => std::thread::sleep(Duration::from_millis(5)),
                                Err(error) => {
                                    let waiting = Instant::now();
                                    let mut reported_waiting = false;
                                    worker_logger.event("input_drain_fallback", json!({"session":session,
                                        "stage":"started","error":error,"counts":paste::key_state_counts()}));
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
                    let loaded = history.get(id);
                    let exports = history.exports();
                    let store_read_ms = read_started.elapsed().as_secs_f64()*1000.0;
                    let mut restored = None;
                    let sent = || { let _ = dispatch(&app, Action::PasteSent { session }); };
                    let synchronize = || hooks.as_ref().is_some_and(|hooks| hooks.synchronize());
                    let mut result = {
                        let restore = |policy| {
                            if restricted || stopping.load(Ordering::Acquire) { return None; }
                            if let Some(hooks) = hooks.as_ref() { hooks.synchronize(); }
                            let passive = matches!(policy, RestorePolicy::NativeAfterSubmit { unknown: true });
                            return_for_paste(&app, session, permit.clone(), passive, policy)
                        };
                        match loaded {
                            Ok(Some(content)) => paste::execute(Request {
                                session, target:target.clone(), panel:owner, record_id:id, content, mode, started,
                                external_switch:external_switch || !drained, permit:permit.clone(), copy_only_reason, cancelled:cancelled.clone(),
                            }, &keys, &clipboard, &stopping, Callbacks { on_sent: &sent, restore: &restore, synchronize: &synchronize }, exports.as_deref()),
                            missing => {
                                // 读库失败也要结束旧请求；主线程会再次检查会话，不能收起新的面板。
                                let focus = restore(RestorePolicy::LegacyBeforeSubmit);
                                let mut result = match missing {
                                    Ok(None) => json!({"clipboard_written":false,"reason":"record_missing"}),
                                    Err(code) => json!({"clipboard_written":false,"reason":"store_read_failed","error_code":code}),
                                    Ok(Some(_)) => unreachable!(),
                                };
                                if let Some(focus) = focus { result["restore"] = json!(focus); }
                                result
                            }
                        }
                    };
                    if !restricted {
                        result["input_cleanup"] = json!(if drained {
                            hooks.as_ref().map_or("unavailable", Hooks::input_release_evidence)
                        } else { "cancelled_physical_release" });
                    }
                    if restricted {
                        let paired = drain();
                        let copied = result["clipboard_written"] == true && !cancelled.load(Ordering::Acquire);
                        result["input_cleanup"] = json!(if paired {
                            hooks.as_ref().map_or("unavailable", Hooks::input_release_evidence)
                        } else { "cancelled_physical_release" });
                        if !stopping.load(Ordering::Acquire) {
                            restored = return_for_paste(&app, session, if paired { permit } else { None }, copied, RestorePolicy::LegacyBeforeSubmit);
                        }
                        if copied {
                        let focus = window::confirm_strict(&target);
                        result["hint_message"] = json!(if restored.is_some() && focus.foreground.success
                            && focus.focus_restored && reliable_control(&target) {
                            "已复制，请按 Ctrl+V 手动粘贴"
                        } else {
                            "已复制，请先点击目标输入位置，再按 Ctrl+V"
                        });
                        result["passive_hint"] = json!(true);
                        }
                    }
                    if let Some(restored) = restored { result["restore"] = json!(restored); }
                    result["request_cancelled"] = json!(cancelled.load(Ordering::Acquire));
                    result["store_read_ms"] = json!(store_read_ms);
                    // execute 已完成发键及内容证据收集，至此才终止前台监控。
                    drop(hooks);
                    let _ = dispatch(&app, Action::PasteFinished { session, result });
                }).is_err() {
                    self.pasting = false;
                    return Err("粘贴线程启动失败".into());
                }
            }
            Action::PasteSent { session } if session == self.session => {
                self.pasting = false;
            }
            Action::PasteFinished {
                session,
                mut result,
            } => {
                // 证据收集期间可能已经再次呼出，日志要用这次粘贴自己的会话号和目标，不能用当前的。
                result["session"] = json!(session);
                if session == self.session
                    && self
                        .paste_cancel
                        .as_ref()
                        .is_some_and(|cancel| cancel.load(Ordering::Acquire))
                {
                    result["request_cancelled"] = json!(true);
                }
                logger.event("paste", result.clone());
                if session == self.session
                    && result["clipboard_written"] == true
                    && self.mode != Mode::Hidden
                {
                    self.emit("panel:history-changed", json!({}));
                }
                if session == self.session {
                    self.pasting = false;
                    self.paste_cancel = None;
                    match no_submission_finish(&result, self.current(session), self.interaction) {
                        Some(NoSubmissionFinish::CloseRequested) => {
                            // 写前/写后均兑现等待中的关闭；Passive 禁止归还或抢回前台。
                            self.hide(logger)?;
                            return Ok(());
                        }
                        Some(NoSubmissionFinish::CloseOnly) => {
                            // 未提交保留真实写入事实，但不承诺剪贴板仍有此内容，不提供重试。
                            self.hint.take();
                            self.set_interaction(Interaction::CloseOnly);
                            self.emit(
                                "panel:error",
                                json!({"message":no_submission_message(&result),"close_only":true}),
                            );
                            // 必须绕过 paste-result 和 deferred hide；前端继续复用已有 CloseOnly。
                            return Ok(());
                        }
                        Some(NoSubmissionFinish::Ignore) => return Ok(()),
                        None => {}
                    }
                    // cleanup 回执超时也必须显示未知结果；不再尝试归还，也不允许用户误重试。
                    let native_unknown = result["paste_result"] == "native_submission_unknown";
                    if native_unknown
                        && self.current(session)
                        && result["request_cancelled"] != true
                    {
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
                    if result["clipboard_written"] == false
                        && result["reason"] != "request_cancelled"
                        && result["request_cancelled"] != true
                    {
                        let message = if result["reason"] == "record_missing" {
                            "这条记录已不在历史里"
                        } else if result["reason"] == "store_read_failed" {
                            "读取历史失败，未改写剪贴板。"
                        } else if result["reason"] == "clipboard_capture_busy" {
                            "剪贴板忙，本次未提交，请关闭面板后重新呼出"
                        } else {
                            "写入剪贴板失败，请重试。"
                        };
                        self.emit("panel:error", json!({"message":message,"close_only":false}));
                    } else if result["clipboard_written"] == true {
                        if let Some(dwell) = dwell {
                            // 告诉前端面板还开着、要停留：页脚当场显示提示，停留期间不再发起粘贴。
                            result["hint_dwell_ms"] = json!(dwell.as_millis() as u64);
                        }
                        self.emit("panel:paste-result", result);
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
            Action::Quit | Action::QuitFrontend => {
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
                self.hooks.take();
                self.log(logger, "quit_requested", json!({}));
                logger.flush();
                app.exit(0);
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwritten_busy_is_close_only_but_requested_close_and_new_session_win() {
        let mut result = json!({"clipboard_written":false,"reason":"clipboard_capture_busy","request_cancelled":false});
        assert_eq!(
            no_submission_finish(&result, true, Interaction::Activated),
            Some(NoSubmissionFinish::CloseOnly)
        );
        // 即使busy已产出，UI接收前的Esc也以当前取消标记为准。
        result["request_cancelled"] = json!(true);
        assert_eq!(
            no_submission_finish(&result, true, Interaction::Passive),
            Some(NoSubmissionFinish::CloseRequested)
        );
        assert_eq!(
            no_submission_finish(&result, false, Interaction::Passive),
            None
        );
        result["reason"] = json!("request_cancelled");
        assert_eq!(
            no_submission_finish(&result, true, Interaction::Passive),
            Some(NoSubmissionFinish::CloseRequested)
        );
        result["clipboard_written"] = json!(true);
        assert_eq!(
            no_submission_finish(&result, true, Interaction::Passive),
            None
        );
        assert!(!Interaction::Passive.may_restore());
        assert!(!Interaction::CloseOnly.may_restore());
    }

    #[test]
    fn experiment_flag_only_accepts_one() {
        assert!(flag_enabled(Some("1")));
        assert!(flag_enabled(Some(" 1 ")));
        for value in [
            None,
            Some(""),
            Some("0"),
            Some("true"),
            Some("yes"),
            Some("11"),
        ] {
            assert!(!flag_enabled(value), "{value:?}");
        }
    }

    #[test]
    fn written_native_not_submitted_result_keeps_only_a_close_entry() {
        let result = json!({"paste_result":"native_not_submitted",
            "clipboard_written":true,"reason":"native_clipboard_busy","request_cancelled":false});
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
                        let result = json!({"paste_result":"native_not_submitted", "clipboard_written":written,
                            "reason":"native_clipboard_busy", "request_cancelled":cancelled});
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
                no_submission_message(&json!({"reason":reason})),
                "剪贴板忙，本次未粘贴，请关闭面板后重新呼出"
            );
        }
        assert_eq!(
            no_submission_message(&json!({"reason":"native_clipboard_changed"})),
            "剪贴板已变化，本次未粘贴，请关闭面板后重新呼出"
        );
    }

    #[test]
    fn written_cancelled_native_result_hides_quietly_and_late_session_is_ignored() {
        let result = json!({"paste_result":"native_not_submitted", "clipboard_written":true,
            "reason":"request_cancelled", "request_cancelled":true});
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
            request::{Outcome, native_outcome},
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
                let mut result = json!({"clipboard_written":written,"paste_result":null,"reason":null,"request_cancelled":cancelled});
                // execute 使用同一个映射，覆盖锁前/Service.write Err，而不是只构造新 Outcome 字符串。
                result["paste_result"] = json!(native_outcome(Err(reason.to_owned()), &mut result));
                assert_eq!(result["clipboard_written"], written);
                assert_eq!(result["reason"], reason);
                assert_eq!(result["paste_result"], "native_not_submitted");
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
            let mut result = json!({"clipboard_written":true,"request_cancelled":true});
            let outcome = native_outcome(Ok(Completion::Submitted(submission)), &mut result);
            assert_eq!(
                outcome,
                if submission == Submission::Sent {
                    Outcome::Sent
                } else {
                    Outcome::NativeSubmissionUnknown
                }
            );
            result["paste_result"] = json!(outcome);
            assert_eq!(
                no_submission_finish(&result, true, Interaction::Passive),
                None
            );
        }
        // Legacy 没有调用 native_outcome，普通写失败继续走原有错误提示。
        assert_eq!(
            no_submission_finish(
                &json!({"paste_result":null,"clipboard_written":false,"reason":"clipboard_write_failed"}),
                true,
                Interaction::Quick
            ),
            None
        );
    }

    #[test]
    fn shown_payload_carries_the_experiment() {
        assert_eq!(
            Experiments {
                search_box_focus: true
            }
            .payload(),
            json!({"searchBoxFocus":true})
        );
        assert_eq!(
            Experiments::default().payload(),
            json!({"searchBoxFocus":false})
        );
    }

    #[test]
    fn only_elevated_copy_only_results_keep_the_panel() {
        assert_eq!(ELEVATED_HINT_DWELL, Duration::from_secs(3));
        assert_eq!(
            dwell_after_paste(
                &json!({"paste_result":"copied_only","reason":"target_may_be_elevated"})
            ),
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
                dwell_after_paste(&json!({"paste_result":"copied_only","reason":reason})),
                None,
                "{reason}"
            );
        }
        // 真发了键、或 Shift+Enter 只复制：不管原因字段写什么都不停留。
        for result in ["copied", "sent", "confirmed"] {
            assert_eq!(
                dwell_after_paste(
                    &json!({"paste_result":result,"reason":"target_may_be_elevated"})
                ),
                None,
                "{result}"
            );
        }
        // 没写进剪贴板（记录不存在、读库失败）和线程报错时结论为空，不停留。
        assert_eq!(
            dwell_after_paste(
                &json!({"paste_result":null,"reason":"record_missing","clipboard_written":false})
            ),
            None
        );
        assert_eq!(dwell_after_paste(&json!({})), None);
        assert_eq!(
            dwell_after_paste(&json!({"paste_result":"copied", "clipboard_written":true,
            "passive_hint":true})),
            Some(ELEVATED_HINT_DWELL)
        );
        assert_eq!(
            dwell_after_paste(
                &json!({"paste_result":"copied_only", "reason":"target_may_be_elevated",
            "clipboard_written":true, "passive_hint":true, "request_cancelled":true})
            ),
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
