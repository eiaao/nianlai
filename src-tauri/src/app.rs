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
        let changed: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = panel::dispatch(&app, Action::HistoryChanged);
        });
        changed();
        self.history.set_changed(changed.clone());
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
    /// 完整预览里选中的文字复制到剪贴板（M2-7）：照“纯文本粘贴”的做法登记回声，不会被当成新复制记进历史。
    pub fn copy_selection(&self, id: usize, text: &str) -> Result<(), String> {
        let content = self.history.get(id)?.ok_or("这条记录已不在历史里")?;
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
    pub fn shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
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
