use crate::{
    autocycle,
    history::History,
    log::Logger,
    panel::{self, Action, Panel},
    paste::keys::Table,
    platform,
};
use serde_json::json;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tauri::{AppHandle, WebviewWindow};
use tauri_plugin_global_shortcut::Shortcut;

trait HistoryChangedContext: Send + Sync + 'static {
    fn panel_history_changed(&self);
    fn guide_history_changed(&self);
    fn refresh_tray(&self);
}

struct AppHistoryChanged(AppHandle);

impl HistoryChangedContext for AppHistoryChanged {
    fn panel_history_changed(&self) {
        let _ = panel::dispatch(&self.0, Action::HistoryChanged);
    }

    fn guide_history_changed(&self) {
        crate::guide_window::history_changed(&self.0);
    }

    fn refresh_tray(&self) {
        // 复制发生或健康变化时，托盘的“今天记下 n 条”和三态跟着刷新（v11）。
        // 由监听/读取线程调用：不能同步等主线程（见 tray::refresh_from_worker）。
        crate::tray::refresh_from_worker(&self.0);
    }
}

fn history_changed_callback<C>(context: Arc<C>) -> Arc<dyn Fn() + Send + Sync>
where
    C: HistoryChangedContext,
{
    Arc::new(move || {
        // 原 panel guard 独立执行；即使隐藏时没有新 panel 事件，guide 和 tray 仍按原顺序继续。
        context.panel_history_changed();
        context.guide_history_changed();
        context.refresh_tray();
    })
}

fn health_changed_callback<C>(context: Arc<C>) -> Arc<dyn Fn() + Send + Sync>
where
    C: HistoryChangedContext,
{
    Arc::new(move || {
        // 健康变化复用既有定向 history:changed；不新增 guide 事件，也不额外刷新普通 panel。
        context.guide_history_changed();
        context.refresh_tray();
    })
}

pub(crate) fn copy_guide_sample_to(clipboard: &platform::clipboard::Service) -> Result<(), String> {
    let content = crate::history::content::Content::text(
        crate::guide_window::SAMPLE_TEXT,
        platform::hash::sha256,
    )?;
    clipboard.write_for_capture(content)
}

/// 应用的总装对象：持有面板状态机、历史服务、采集服务、剪贴板写入线程和日志。
pub struct Controller {
    pub(crate) inner: Mutex<Panel>,
    pub logger: Logger,
    // 字段按声明顺序析构：没走 shutdown 就退出时（例如启动失败），也要先停采集再停剪贴板线程。
    capture: Mutex<Option<platform::capture::Service>>,
    pub(crate) clipboard: platform::clipboard::Service,
    pub(crate) keys: Arc<Table>,
    pub(crate) stopping: Arc<AtomicBool>,
    pub(crate) history: History,
}

impl Controller {
    pub fn new(
        window: WebviewWindow,
        logger: Logger,
        history: History,
        plan: Option<autocycle::Plan>,
        resuspend: Option<Duration>,
    ) -> platform::Result<Self> {
        let clipboard = platform::clipboard::Service::start(logger.clone(), history.clone())?;
        let keys = Arc::new(Table::load(&logger));
        Ok(Self {
            logger,
            clipboard,
            keys,
            stopping: Arc::new(AtomicBool::new(false)),
            history,
            capture: Mutex::new(None),
            inner: Mutex::new(Panel::new(window, plan, resuspend)),
        })
    }
    pub fn set_hotkey(&self, hotkey: Option<Shortcut>) {
        self.inner.lock().unwrap().set_hotkey(hotkey, &self.logger);
    }
    pub fn start_capture(&self, app: &AppHandle, automatic: bool) -> platform::Result<()> {
        self.history.initialize();
        let app = app.clone();
        let changed = history_changed_callback(Arc::new(AppHistoryChanged(app.clone())));
        let health_changed = health_changed_callback(Arc::new(AppHistoryChanged(app)));
        self.history.set_health_changed(health_changed);
        changed();
        self.history.set_changed(changed.clone());
        self.history.maintenance(changed.clone());
        let mut capture = self.capture.lock().unwrap();
        if self.stopping.load(Ordering::Acquire) {
            return Ok(());
        }
        match platform::capture::Service::start(
            self.logger.clone(),
            self.history.clone(),
            &self.clipboard,
            automatic,
            changed.clone(),
        ) {
            Ok(service) => *capture = Some(service),
            Err(error) => {
                // 记录服务起不来：托盘要报“出问题”；若已在正常退出则保留 Stopping。
                self.history.set_recording(false);
                self.history.notify_health_changed();
                return Err(error);
            }
        }
        Ok(())
    }
    /// 完整预览里选中的文字复制到剪贴板（M2-7）：照“纯文本粘贴”的做法登记回声，不会被当成新复制记进历史。
    pub fn copy_selection(&self, id: usize, text: &str) -> Result<(), String> {
        let content = self.history.get(id)?.ok_or("这张已经不在了")?;
        let copy = crate::history::content::Content::text(text, platform::hash::sha256)?;
        self.clipboard
            .write(
                platform::clipboard::Record {
                    id,
                    fingerprint: content.fingerprint,
                },
                copy,
                0,
                false,
            )
            .map(drop)
            .map_err(|error| error.to_string())
    }
    /// 首次启动的示例只承诺写入剪贴板：不登记回声、不直接写历史，是否记下由正常采集链决定。
    pub fn copy_guide_sample(&self) -> Result<(), String> {
        copy_guide_sample_to(&self.clipboard)
    }
    pub fn shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        // 先标 Stopping：随后监听/读取线程末尾的 set_recording(false) 不当成故障（v11）。
        self.history.begin_capture_shutdown();
        if let Some(capture) = self.capture.lock().unwrap().take() {
            capture.shutdown();
        }
        if let Ok(mut panel) = self.inner.lock() {
            panel.shutdown(&self.logger);
        }
        self.clipboard.shutdown();
        self.history.flush_deferred();
        self.history.summary();
        self.logger.event("shutdown", json!({}));
        self.logger.flush();
    }
}

#[cfg(test)]
mod task11_tests {
    use super::{HistoryChangedContext, health_changed_callback, history_changed_callback};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Step {
        PanelHistoryChanged,
        GuideHistoryChanged,
        TrayRefreshed,
    }

    #[derive(Default)]
    struct FakeHistoryChanged {
        steps: Mutex<Vec<Step>>,
    }

    impl HistoryChangedContext for FakeHistoryChanged {
        fn panel_history_changed(&self) {
            self.steps.lock().unwrap().push(Step::PanelHistoryChanged);
        }

        fn guide_history_changed(&self) {
            self.steps.lock().unwrap().push(Step::GuideHistoryChanged);
        }

        fn refresh_tray(&self) {
            self.steps.lock().unwrap().push(Step::TrayRefreshed);
        }
    }

    #[test]
    fn task11_fix_production_history_callback_dispatches_hidden_panel_guide_and_tray() {
        let context = Arc::new(FakeHistoryChanged::default());
        let callback = history_changed_callback(context.clone());
        callback();
        assert_eq!(
            *context.steps.lock().unwrap(),
            [
                Step::PanelHistoryChanged,
                Step::GuideHistoryChanged,
                Step::TrayRefreshed,
            ]
        );
    }

    #[test]
    fn task12_fix_health_callback_reuses_guide_history_then_tray_without_panel_dispatch() {
        let context = Arc::new(FakeHistoryChanged::default());
        let callback = health_changed_callback(context.clone());
        callback();
        assert_eq!(
            *context.steps.lock().unwrap(),
            [Step::GuideHistoryChanged, Step::TrayRefreshed]
        );
    }
}
