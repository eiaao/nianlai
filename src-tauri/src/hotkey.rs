//! 呼出键校验、运行时替换与非致命降级；不改变面板的激活策略。
use crate::{
    app::Controller,
    log::Logger,
    panel::{self, Action},
    platform,
    shortcut::{parse, warning},
};
use serde::Serialize;
use serde_json::json;
use std::{
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

#[derive(Clone, Debug, Serialize)]
pub struct Validation {
    pub hotkey: String,
    pub key: String,
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub warning: Option<&'static str>,
}

#[tauri::command]
pub fn validate_hotkey(value: String) -> Result<Validation, String> {
    let shortcut = parse(&value)?;
    Ok(Validation {
        hotkey: shortcut.to_string(),
        key: shortcut.key.to_string(),
        control: shortcut.mods.contains(Modifiers::CONTROL),
        alt: shortcut.mods.contains(Modifiers::ALT),
        shift: shortcut.mods.contains(Modifiers::SHIFT),
        win: shortcut.mods.contains(Modifiers::SUPER),
        warning: warning(&shortcut),
    })
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    pub requested: String,
    pub active: Option<String>,
    pub error: Option<String>,
    pub detail: Option<String>,
    pub warning: Option<&'static str>,
    pub overridden: bool,
    pub disabled: bool,
}

// 测试只替换系统边界，不注册真实全局键，也不触碰日常样机。
trait Backend {
    fn register(&self, shortcut: Shortcut) -> Result<(), String>;
    fn unregister(&self, shortcut: Shortcut) -> Result<(), String>;
    fn sync_panel(&self, shortcut: Option<Shortcut>);
}

#[derive(Default)]
struct State {
    active: Option<Shortcut>,
    status: Status,
    override_hotkey: Option<String>,
    automatic: bool,
    // 设置页正在录制新快捷键时暂时注销的呼出键；录完（或超时）再注册回来。
    paused: Option<Shortcut>,
    pause_generation: u64,
}

impl State {
    fn apply(&mut self, requested: &str, backend: &impl Backend) {
        let requested = self
            .override_hotkey
            .as_deref()
            .unwrap_or(requested)
            .to_owned();
        self.status = Status {
            requested: requested.clone(),
            active: self.active.map(|key| key.to_string()),
            overridden: self.override_hotkey.is_some(),
            disabled: self.automatic,
            ..Status::default()
        };
        if self.automatic {
            return;
        }
        let shortcut = match parse(&requested) {
            Ok(shortcut) => shortcut,
            Err(error) => {
                self.status.error = Some(format!("呼出键无效：{error}。仍可从托盘打开面板。"));
                return;
            }
        };
        self.status.requested = shortcut.to_string();
        self.status.warning = warning(&shortcut);
        if self.active == Some(shortcut) {
            return;
        }
        if let Some(previous) = self.active {
            if let Err(error) = backend.unregister(previous) {
                self.status.error =
                    Some("旧呼出键注销失败，未注册新键；原呼出键仍保留，可重试保存。".into());
                self.status.detail = Some(error);
                return;
            }
            self.active = None;
            self.status.active = None;
            backend.sync_panel(None);
        }
        // 不先试注册、不偷偷切备用键，也不回滚旧键；设置页明确显示实际生效状态。
        match backend.register(shortcut) {
            Ok(()) => {
                self.active = Some(shortcut);
                self.status.active = Some(shortcut.to_string());
                backend.sync_panel(Some(shortcut));
            }
            Err(error) => {
                self.status.error =
                    Some("呼出键被占用或无效，请换一个；仍可从托盘打开面板。".into());
                self.status.detail = Some(error);
            }
        }
    }

    // 录制期间注销呼出键，否则按下当前组合会被系统拦走去呼出面板，录不到。
    // 录制中保存了新键时 apply 已注册新键，恢复时不再动它。
    fn pause(&mut self, paused: bool, backend: &impl Backend) -> Result<(), String> {
        if paused {
            if self.paused.is_some() {
                return Ok(());
            }
            if let Some(active) = self.active {
                backend.unregister(active)?;
                self.active = None;
                backend.sync_panel(None);
                self.paused = Some(active);
            }
        } else if let Some(paused) = self.paused.take()
            && self.active.is_none()
        {
            match backend.register(paused) {
                Ok(()) => {
                    self.active = Some(paused);
                    self.status.active = Some(paused.to_string());
                    backend.sync_panel(Some(paused));
                }
                Err(error) => {
                    self.status.active = None;
                    self.status.error =
                        Some("呼出键恢复注册失败，请重新保存；仍可从托盘打开面板。".into());
                    self.status.detail = Some(error.clone());
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    fn record(&self, logger: &Logger) {
        if let Some(reason) = self.status.warning {
            logger.event(
                "hotkey_conflict_warning",
                json!({"shortcut":self.status.requested,"reason":reason}),
            );
        }
        if let Some(reason) = &self.status.error {
            logger.event("hotkey_registration_failed", json!({
                "reason":reason,"error":self.status.detail,"active":self.status.active,"fatal":false,
            }));
        }
    }
}

pub struct Runtime {
    state: Mutex<State>,
    // 回调可能在主线程；绝不等待保存线程持有的锁，以免与插件的主线程注册互等。
    active_id: AtomicU64,
}

struct SystemBackend<'a>(&'a AppHandle);
impl Backend for SystemBackend<'_> {
    fn register(&self, shortcut: Shortcut) -> Result<(), String> {
        self.0
            .global_shortcut()
            .on_shortcut(shortcut, |app, shortcut, event| {
                let started = Instant::now();
                if event.state != ShortcutState::Pressed {
                    return;
                }
                // 已排队的旧键回调在热切换完成后也不能再呼出面板。
                if !app.state::<Runtime>().accepts(shortcut) {
                    return;
                }
                let _ = panel::dispatch(
                    app,
                    Action::Toggle {
                        started,
                        target: platform::window::capture(),
                        alt: shortcut.mods.contains(Modifiers::ALT),
                    },
                );
            })
            .map_err(|error| error.to_string())?;
        self.0.state::<Controller>().logger.event(
            "hotkey_registered",
            json!({
                "purpose":"panel","shortcut":shortcut.to_string(),"fallback":false,
            }),
        );
        Ok(())
    }

    fn unregister(&self, shortcut: Shortcut) -> Result<(), String> {
        self.0
            .global_shortcut()
            .unregister(shortcut)
            .map_err(|error| error.to_string())?;
        self.0.state::<Controller>().logger.event(
            "hotkey_unregistered",
            json!({
                "purpose":"panel","shortcut":shortcut.to_string(),
            }),
        );
        Ok(())
    }

    fn sync_panel(&self, shortcut: Option<Shortcut>) {
        self.0.state::<Runtime>().active_id.store(
            shortcut.map_or(0, |key| u64::from(key.id()) + 1),
            Ordering::Release,
        );
        self.0.state::<Controller>().set_hotkey(shortcut);
    }
}

impl Runtime {
    fn accepts(&self, shortcut: &Shortcut) -> bool {
        self.active_id.load(Ordering::Acquire) == u64::from(shortcut.id()) + 1
    }

    /// 取得保存会话：持锁期间，跨窗口并发保存按同一顺序执行写盘、注销、注册和广播。
    pub fn lock<'a>(&'a self, app: &'a AppHandle) -> Session<'a> {
        Session {
            state: self.state.lock().unwrap(),
            app,
        }
    }
}

/// 一次保存期间持有的呼出键状态；编排顺序由调用方（`save_settings`）决定。
pub struct Session<'a> {
    state: std::sync::MutexGuard<'a, State>,
    app: &'a AppHandle,
}

impl Session<'_> {
    /// 换成 `requested`；成功、冲突、错误都记在状态里，不返回错误。
    pub fn apply(&mut self, requested: &str) {
        self.state.apply(requested, &SystemBackend(self.app));
    }

    pub fn record(&self, logger: &Logger) {
        self.state.record(logger);
    }
}

/// 录制最长时长：设置页异常关闭、没来得及恢复时，到点自动把呼出键注册回来。
const PAUSE_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

#[tauri::command]
pub async fn pause_hotkey(app: AppHandle, paused: bool) -> Result<(), String> {
    // 注册/注销由插件派回主线程；不能在主线程持锁等待它们完成（同 set_settings）。
    tauri::async_runtime::spawn_blocking(move || {
        let generation = {
            let runtime = app.state::<Runtime>();
            let mut state = runtime.state.lock().unwrap();
            state.pause_generation += 1;
            state.pause(paused, &SystemBackend(&app))?;
            state.pause_generation
        };
        app.state::<Controller>()
            .logger
            .event("hotkey_paused", json!({"paused":paused}));
        if paused {
            let app = app.clone();
            let _ = std::thread::Builder::new()
                .name("hotkey-pause".into())
                .spawn(move || {
                    std::thread::sleep(PAUSE_LIMIT);
                    let runtime = app.state::<Runtime>();
                    let mut state = runtime.state.lock().unwrap();
                    if state.pause_generation == generation && state.paused.is_some() {
                        let _ = state.pause(false, &SystemBackend(&app));
                    }
                });
        }
        Ok(())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn get_hotkey_status(app: AppHandle) -> Status {
    app.state::<Runtime>().state.lock().unwrap().status.clone()
}

pub fn setup(app: &AppHandle, logger: &Logger, automatic: bool, requested: &str, overridden: bool) {
    app.manage(Runtime {
        state: Mutex::new(State {
            override_hotkey: overridden.then(|| requested.to_owned()),
            automatic,
            ..State::default()
        }),
        active_id: AtomicU64::new(0),
    });
    // 固定退出键失败同样不能中断初始化；托盘始终提供退出入口。
    let exit = Shortcut::new(
        Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT),
        tauri_plugin_global_shortcut::Code::KeyQ,
    );
    match app.global_shortcut().on_shortcut(exit, |app, _, event| {
        if event.state == ShortcutState::Pressed {
            app.state::<Controller>()
                .logger
                .event("quit_source", json!({"source":"hotkey"}));
            let _ = panel::dispatch(app, Action::Quit);
        }
    }) {
        Ok(()) => logger.event(
            "hotkey_registered",
            json!({"purpose":"quit","shortcut":exit.to_string()}),
        ),
        Err(error) => logger.event(
            "exit_hotkey_failed",
            json!({"error":error.to_string(),"fatal":false}),
        ),
    }
    let runtime = app.state::<Runtime>();
    let mut state = runtime.state.lock().unwrap();
    state.apply(requested, &SystemBackend(app));
    state.record(logger);
    // 注册失败只是一种可见运行状态：不返回 setup 错误，不退出，也不消耗守护者重启额度。
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Fake {
        calls: RefCell<Vec<String>>,
        register_error: RefCell<Option<&'static str>>,
        unregister_error: RefCell<Option<&'static str>>,
    }
    impl Backend for Fake {
        fn register(&self, key: Shortcut) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("register:{key}"));
            self.register_error
                .borrow()
                .map_or(Ok(()), |error| Err(error.into()))
        }
        fn unregister(&self, key: Shortcut) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("unregister:{key}"));
            self.unregister_error
                .borrow()
                .map_or(Ok(()), |error| Err(error.into()))
        }
        fn sync_panel(&self, key: Option<Shortcut>) {
            self.calls.borrow_mut().push(format!(
                "panel:{}",
                key.map_or("none".into(), |key| key.to_string())
            ));
        }
    }
    fn key(value: &str) -> Shortcut {
        parse(value).unwrap()
    }

    #[test]
    fn validate_command_reports_the_parsed_shortcut() {
        let value = validate_hotkey("Ctrl+`".into()).unwrap();
        assert!(value.control);
        assert!(!value.win);
        assert_eq!(value.key, "Backquote");
        assert_eq!(key(&value.hotkey), key("Ctrl+`"));
    }

    #[test]
    fn runtime_replacement_unregisters_before_register_and_updates_the_existing_panel() {
        let mut state = State::default();
        let backend = Fake::default();
        state.apply("Ctrl+`", &backend);
        backend.calls.borrow_mut().clear();
        state.apply("Ctrl+Shift+F11", &backend);
        assert_eq!(
            *backend.calls.borrow(),
            [
                format!("unregister:{}", key("Ctrl+`")),
                "panel:none".into(),
                format!("register:{}", key("Ctrl+Shift+F11")),
                format!("panel:{}", key("Ctrl+Shift+F11")),
            ]
        );
        assert_eq!(state.active, Some(key("Ctrl+Shift+F11")));
        assert!(state.status.error.is_none());
        backend.calls.borrow_mut().clear();
        state.apply("control+shift+F11", &backend);
        assert!(backend.calls.borrow().is_empty(), "等价组合不重复注册");
    }

    #[test]
    fn recording_pause_unregisters_then_restores_unless_a_new_key_was_saved_meanwhile() {
        let mut state = State::default();
        let backend = Fake::default();
        state.apply("Ctrl+`", &backend);
        backend.calls.borrow_mut().clear();
        // 暂停 → 恢复：同一个键注销再注册回来；重复暂停不重复注销。
        state.pause(true, &backend).unwrap();
        state.pause(true, &backend).unwrap();
        assert_eq!(state.active, None);
        state.pause(false, &backend).unwrap();
        assert_eq!(
            *backend.calls.borrow(),
            [
                format!("unregister:{}", key("Ctrl+`")),
                "panel:none".into(),
                format!("register:{}", key("Ctrl+`")),
                format!("panel:{}", key("Ctrl+`")),
            ]
        );
        assert_eq!(state.active, Some(key("Ctrl+`")));
        // 录制中保存了新键：apply 直接注册新键，恢复时不再把旧键注册回来。
        state.pause(true, &backend).unwrap();
        state.apply("Alt+V", &backend);
        backend.calls.borrow_mut().clear();
        state.pause(false, &backend).unwrap();
        assert!(backend.calls.borrow().is_empty());
        assert_eq!(state.active, Some(key("Alt+V")));
        // 没有生效键时暂停/恢复什么都不做。
        let mut empty = State::default();
        empty.pause(true, &backend).unwrap();
        empty.pause(false, &backend).unwrap();
        assert!(backend.calls.borrow().is_empty());
    }

    #[test]
    fn occupied_startup_or_runtime_key_does_not_panic_or_choose_a_hidden_fallback() {
        let mut state = State::default();
        let backend = Fake::default();
        *backend.register_error.borrow_mut() = Some("模拟系统返回：已占用");
        state.apply("Ctrl+`", &backend);
        assert_eq!(state.active, None);
        assert!(
            state
                .status
                .error
                .as_deref()
                .unwrap()
                .contains("被占用或无效")
        );
        assert_eq!(backend.calls.borrow().len(), 1);
        *backend.register_error.borrow_mut() = None;
        state.apply("Ctrl+`", &backend);
        assert!(state.status.error.is_none(), "占用释放后同值保存可以重试");
        backend.calls.borrow_mut().clear();
        *backend.register_error.borrow_mut() = Some("模拟系统返回：无效组合");
        state.apply("Alt+V", &backend);
        assert_eq!(state.active, None);
        assert_eq!(state.status.active, None);
        assert_eq!(
            *backend.calls.borrow(),
            [
                format!("unregister:{}", key("Ctrl+`")),
                "panel:none".into(),
                format!("register:{}", key("Alt+V"))
            ]
        );
        assert_eq!(
            state.status.detail.as_deref(),
            Some("模拟系统返回：无效组合")
        );
    }

    #[test]
    fn failed_unregister_keeps_the_old_key_and_does_not_register_a_second_key() {
        let mut state = State::default();
        let backend = Fake::default();
        state.apply("Ctrl+`", &backend);
        backend.calls.borrow_mut().clear();
        *backend.unregister_error.borrow_mut() = Some("模拟注销失败");
        state.apply("Alt+V", &backend);
        assert_eq!(state.active, Some(key("Ctrl+`")));
        assert_eq!(
            *backend.calls.borrow(),
            [format!("unregister:{}", key("Ctrl+`"))]
        );
        assert!(state.status.error.as_deref().unwrap().contains("注销失败"));
    }

    #[test]
    fn save_persists_and_broadcasts_even_when_registration_fails_then_reloads_on_restart() {
        let directory = settings::tests::Directory::new();
        let path = directory.0.join("settings.json");
        let (store, _) = settings::Store::load(path.clone());
        let mut state = State::default();
        let backend = Fake::default();
        state.apply(&store.get().hotkey, &backend);
        let mut broadcasts = Vec::new();
        *backend.register_error.borrow_mut() = Some("模拟被占用");
        let (saved, _) = settings::save(
            &store,
            json!({"hotkey":"Ctrl+Shift+F11"}),
            |saved| state.apply(&saved.hotkey, &backend),
            |saved| broadcasts.push(saved.clone()),
        )
        .unwrap();
        assert_eq!(broadcasts.as_slice(), std::slice::from_ref(&saved));
        assert_eq!(saved.hotkey, "Ctrl+Shift+F11");
        assert!(state.status.error.is_some());
        let (reloaded, issues) = settings::Store::load(path);
        assert!(issues.is_empty());
        let mut restarted = State::default();
        *backend.register_error.borrow_mut() = None;
        restarted.apply(&reloaded.get().hotkey, &backend);
        assert_eq!(restarted.active, Some(key("Ctrl+Shift+F11")));
        assert!(restarted.status.error.is_none());
        backend.calls.borrow_mut().clear();
        settings::save(
            &store,
            json!({"theme":"dark"}),
            |saved| state.apply(&saved.hotkey, &backend),
            |_| {},
        )
        .unwrap();
        assert!(
            backend.calls.borrow().is_empty(),
            "外观保存不应顺便重试抢占快捷键"
        );
    }

    #[test]
    fn invalid_or_unwritable_settings_never_unregister_or_broadcast() {
        let directory = settings::tests::Directory::new();
        let path = directory.0.join("settings.json");
        let (store, _) = settings::Store::load(path.clone());
        let mut state = State::default();
        let backend = Fake::default();
        state.apply(&store.get().hotkey, &backend);
        backend.calls.borrow_mut().clear();
        let original = std::fs::read(&path).unwrap();
        for update in [
            json!({"hotkey":"Win+L","theme":"dark"}),
            json!({"hotkey":null}),
            json!({"hotkey":"Ctrl+"}),
        ] {
            assert!(
                settings::save(
                    &store,
                    update,
                    |saved| state.apply(&saved.hotkey, &backend),
                    |_| panic!("不能广播非法设置")
                )
                .is_err()
            );
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("sentinel"), b"keep").unwrap();
        assert!(
            settings::save(
                &store,
                json!({"hotkey":"Alt+V"}),
                |saved| state.apply(&saved.hotkey, &backend),
                |_| panic!("写盘失败不能广播")
            )
            .is_err()
        );
        assert!(backend.calls.borrow().is_empty());
        assert_eq!(state.active, Some(key("Ctrl+`")));
        assert_eq!(store.get(), settings::Settings::default());
    }

    #[test]
    fn test_override_remains_effective_and_automatic_cycles_do_not_register_a_panel_key() {
        let backend = Fake::default();
        let mut state = State {
            override_hotkey: Some("Alt+V".into()),
            ..State::default()
        };
        state.apply("Ctrl+`", &backend);
        assert_eq!(state.active, Some(key("Alt+V")));
        assert!(state.status.overridden);
        let mut automatic = State {
            automatic: true,
            ..State::default()
        };
        backend.calls.borrow_mut().clear();
        automatic.apply("Ctrl+`", &backend);
        assert!(backend.calls.borrow().is_empty());
        assert!(automatic.status.disabled);
    }

    #[test]
    fn registration_failure_stays_inside_application_and_never_spends_guardian_budget() {
        let backend = Fake::default();
        *backend.register_error.borrow_mut() = Some("模拟被占用");
        let mut launches = 0;
        let mut exits = Vec::new();
        let code = crate::recovery::supervise(
            crate::recovery::Policy {
                delay: std::time::Duration::ZERO,
                ..crate::recovery::POLICY
            },
            || {
                launches += 1;
                let mut state = State::default();
                state.apply("Ctrl+`", &backend);
                assert!(state.status.error.is_some());
                // 应用继续服务，稍后由托盘正常退出；不把注册错误转换成退出码。
                Ok(Some(0))
            },
            |exit| exits.push(exit),
        );
        assert_eq!(code, 0);
        assert_eq!(launches, 1);
        assert_eq!(exits[0].decision, crate::recovery::Decision::Finished);
        assert_eq!(exits[0].restarts, 0);
    }
}
