use crate::{
    log::Logger,
    paste::permit::Monitor,
    platform::{HookEvent, HookOptions, ReplayBatch, Result, Target},
};
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};
use tauri::{WebviewWindow, WindowEvent};
use tauri_plugin_global_shortcut::Shortcut;

type OutsideCallback = (u64, Box<dyn Fn(HookEvent) + Send>);
static OUTSIDE: Mutex<Option<OutsideCallback>> = Mutex::new(None);

fn clear(session: u64) {
    let mut outside = OUTSIDE.lock().unwrap();
    if outside
        .as_ref()
        .is_some_and(|(current, _)| *current == session)
    {
        outside.take();
    }
}

pub(super) fn initialize(window: &WebviewWindow) {
    window.on_window_event(|event| {
        if matches!(event, WindowEvent::Focused(false)) {
            // 回调会收起面板并释放 Hooks，不能持锁调用。
            let callback = OUTSIDE.lock().unwrap().take();
            if let Some((_, callback)) = callback {
                callback(HookEvent::Outside {
                    point: [0; 2],
                    bounds: [0; 4],
                    current: [0; 4],
                });
            }
        }
    });
}

pub struct Hooks {
    monitor: Arc<Monitor>,
    session: u64,
}
impl Drop for Hooks {
    fn drop(&mut self) {
        clear(self.session);
        self.monitor.stopped();
    }
}
// macOS 没有 Windows 预览键钩子，这里不需要。
pub fn set_preview_keys(_: bool) {}
pub fn supported(_: &Shortcut) -> bool {
    true
}
impl Hooks {
    // 面板直接接收键盘输入，只记录当前会话的失焦回调。
    pub fn install(
        _: usize,
        _: &Target,
        _: Option<Shortcut>,
        _: Logger,
        session: u64,
        options: HookOptions,
        callback: impl Fn(HookEvent) + Send + 'static,
    ) -> Result<Self> {
        *OUTSIDE.lock().unwrap() = Some((session, Box::new(callback)));
        options.monitor.started();
        Ok(Self {
            monitor: options.monitor,
            session,
        })
    }
    pub fn switched_to(&self) -> usize {
        0
    }
    // macOS 没有 Windows 键盘钩子，这里不需要。
    pub fn set_hotkey(&self, _: Option<Shortcut>) -> Result<()> {
        Ok(())
    }
    // macOS 没有 Windows 键盘钩子，这里不需要。
    pub fn activation(&self, _: bool) -> Result<()> {
        Ok(())
    }
    // macOS 没有吞键，这里不需要。
    pub fn stop_inputs(&self, _: bool) -> Result<bool> {
        // 粘贴开始后的主动收窗/归还不再当作外部点击。
        clear(self.session);
        Ok(true)
    }
    pub fn input_release_evidence(&self) -> &'static str {
        "macos_direct_input"
    }
    // macOS 没有 Windows 键盘钩子，这里不需要。
    pub fn synchronize(&self) -> bool {
        true
    }
    pub fn begin_search(&self, started: Instant) -> Result<Instant> {
        Ok(started)
    }
    // macOS 没有管理员提示，这里不需要。
    pub fn finish_hint(&self) -> Result<bool> {
        Ok(true)
    }
    // macOS 没有吞键，这里不需要。
    pub fn remove_keyboard(&self) -> Result<ReplayBatch> {
        Ok(ReplayBatch::default())
    }
}
