use crate::{
    autocycle,
    history::{History, Snapshot},
    log::{Logger, now_ms},
    paste::{
        keys::Table,
        request::{Mode as PasteMode, Request},
    },
    platform::{self, HookEvent, Hooks, REPLAY_WAIT, Target, paste, webview, window},
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

enum ShowSource {
    Hotkey { target: Target, alt: bool },
    Auto { cycle: usize, hidden_since: Instant },
}

#[derive(Clone, Copy)]
struct AutoVisible {
    cycle: usize,
    hidden_ms: f64,
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
    started: Option<Instant>,
    shown: Option<Instant>,
    painted: bool,
    foreground_taken: bool,
    awaiting_search: bool,
    memory: Arc<webview::MemoryState>,
    pasting: bool,
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
    SearchTimeout {
        session: u64,
    },
    Hide {
        session: u64,
    },
    Paste {
        session: u64,
        id: usize,
        mode: PasteMode,
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
                started: None,
                shown: None,
                painted: false,
                foreground_taken: false,
                awaiting_search: false,
                memory: Arc::new(webview::MemoryState::default()),
                pasting: false,
                auto_enabled: plan.is_some(),
                auto_plan: plan,
                auto_stop: None,
                auto_visible: None,
                resuspend,
                resuspend_stop: None,
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
            if let Err(error) = panel.replay_search(&self.logger, false, "shutdown") {
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

impl Panel {
    fn log(&self, logger: &Logger, event: &str, mut fields: Value) {
        fields["session"] = json!(self.session);
        fields["process_name"] = json!(self.target.process_name);
        fields["focus_class"] = json!(self.target.focus_class);
        fields["focus_unknown"] = json!(self.target.focus == 0);
        logger.event(event, fields);
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
                json!({"hotkeyToShowMs":shown.duration_since(started).as_secs_f64()*1000.0,"history":history}),
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
            self.hooks = Some(Hooks::install(
                window::handle(&self.window)?,
                self.target.window,
                self.hotkey.ok_or("呼出键尚未注册")?,
                logger.clone(),
                session,
                move |event| {
                    let action = match event {
                        HookEvent::Key(key) => Action::Key { session, key },
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
                            Action::Hide { session }
                        }
                    };
                    let _ = dispatch(&app, action);
                },
            )?);
        }
        self.shown_event(app);
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
        let foreground = window::foreground();
        let panel = window::handle(&self.window)?;
        let may_restore = foreground == panel || foreground == self.target.window;
        if let Err(error) = self.replay_search(logger, false, "hidden") {
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
        if self.mode != Mode::Quick {
            return Ok(());
        }
        self.mode = Mode::Search;
        self.awaiting_search = true;
        let started = self
            .hooks
            .as_ref()
            .ok_or("键盘钩子尚未安装")?
            .begin_search(started.unwrap_or_else(Instant::now))?;
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
    fn replay_search(&self, logger: &Logger, ready: bool, reason: &str) -> platform::Result<()> {
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
        let replayed = if ready && !expired {
            if let Some(started) = batch.started {
                paste::replay(&batch.keys, window::handle(&self.window)?, started)
            } else {
                0
            }
        } else {
            0
        };
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
    fn finish_search(&mut self, logger: &Logger, ready: bool) -> platform::Result<()> {
        if !self.awaiting_search {
            return Ok(());
        }
        self.replay_search(logger, ready, if ready { "ready" } else { "timeout" })?;
        self.awaiting_search = false;
        self.log(logger, "search_ready", json!({"search_box_ready":ready,"foreground_ok":window::foreground()==window::handle(&self.window)?}));
        Ok(())
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
            Action::Key { session, key } if self.current(session) && self.mode == Mode::Quick => {
                self.emit("panel:key", json!({"key":key}));
                if key == "Escape" {
                    self.log(logger, "hide_source", json!({"source":"escape_key"}));
                    self.hide(logger)?;
                }
            }
            Action::Search { session, started } if self.current(session) => {
                self.begin_search(app, logger, started)?
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
            Action::Hide { session } if self.current(session) => {
                self.hide(logger)?;
            }
            Action::Paste {
                session,
                id,
                mode,
                started,
            } if self.current(session) && !self.pasting => {
                let owner = window::handle(&self.window)?;
                let foreground = window::foreground();
                let switched_to = self.hooks.as_ref().map_or(0, Hooks::switched_to);
                let external_switch =
                    switched_to != 0 || (foreground != owner && foreground != self.target.window);
                if external_switch {
                    // 排查用：记下抢走前台的窗口，只记类名和进程名，不记标题。
                    let other = if switched_to != 0 {
                        switched_to
                    } else {
                        foreground
                    };
                    self.log(
                        logger,
                        "paste_switch",
                        json!({"detected_by":if switched_to != 0 { "foreground_event" } else { "foreground_check" },
                            "switched_class":window::class_name(other),"switched_process":window::process_of(other)}),
                    );
                }
                let target = self.target.clone();
                let restore_focus = self.foreground_taken || foreground == owner;
                let hooks = self.hooks.take();
                self.mode = Mode::Hidden;
                self.awaiting_search = false;
                window::hide(&self.window)?;
                self.emit("panel:hidden", json!({}));
                webview::set_visible(
                    &self.window,
                    false,
                    logger.clone(),
                    self.session,
                    self.memory.clone(),
                )?;
                self.log(logger, "hide_source", json!({"source":"paste"}));
                self.log(logger, "hidden", json!({}));
                self.pasting = true;
                let controller = app.state::<Controller>();
                let clipboard = controller.clipboard.clone();
                let history = controller.history.clone();
                let keys = controller.keys.clone();
                let stopping = controller.stopping.clone();
                let worker_logger = logger.clone();
                let app = app.clone();
                if std::thread::Builder::new()
                    .name("paste-request".into())
                    .spawn(move || {
                        if let Some(hooks) = hooks {
                            match hooks.remove_keyboard() {
                                Ok(batch) if batch.started.is_some() || !batch.keys.is_empty() || batch.dropped > 0 => {
                                    worker_logger.event("search_replay", json!({"session":session,
                                        "replayed_keys":0,"dropped_keys":batch.dropped+batch.keys.len(),
                                        "wait_ms":batch.started.map(|at| at.elapsed().as_secs_f64()*1000.0),
                                        "search_box_ready":false,"reason":"paste"}));
                                }
                                Err(_) => worker_logger.event("replay_cleanup_failed", json!({"session":session,"reason":"hook_cleanup_failed"})),
                                _ => {}
                            }
                            // 不在这里再读切换标记：面板隐藏后系统自动激活的窗口会被误记成用户切走。
                            drop(hooks);
                        }
                        let sent = || {
                            let _ = dispatch(&app, Action::PasteSent { session });
                        };
                        // 先按原流程还焦点，再读取大内容。读完不再还焦点，避免抢回用户在读库期间切走的窗口。
                        let restored = (restore_focus && !external_switch).then(|| window::restore(&target));
                        let read_started = Instant::now();
                        let loaded = history.get(id);
                        let exports = history.exports();
                        let store_read_ms = read_started.elapsed().as_secs_f64() * 1000.0;
                        let external_switch = external_switch || window::foreground() != target.window;
                        let mut result = match loaded {
                            Ok(Some(content)) => paste::execute(Request {
                                target, panel: owner, record_id: id, content, mode, started,
                                external_switch, restore_focus: false,
                            }, &keys, &clipboard, &stopping, &sent, exports.as_deref()),
                            Ok(None) => json!({"clipboard_written":false,"reason":"record_missing"}),
                            Err(code) => json!({"clipboard_written":false,"reason":"store_read_failed","error_code":code}),
                        };
                        if let Some(restored) = restored { result["restore"] = json!(restored); }
                        result["store_read_ms"] = json!(store_read_ms);
                        let _ = dispatch(&app, Action::PasteFinished { session, result });
                    })
                    .is_err()
                {
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
                logger.event("paste", result.clone());
                if result["clipboard_written"] == true && self.mode != Mode::Hidden {
                    self.emit("panel:history-changed", json!({}));
                }
                if session == self.session {
                    self.pasting = false;
                    if result["clipboard_written"] == false {
                        let message = if result["reason"] == "record_missing" {
                            "这条记录已不在历史里"
                        } else if result["reason"] == "store_read_failed" {
                            "读取历史失败，未改写剪贴板。"
                        } else {
                            "写入剪贴板失败，请重试。"
                        };
                        self.emit("panel:error", json!({"message":message}));
                    } else {
                        self.emit("panel:paste-result", result);
                    }
                }
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
