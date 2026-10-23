// 非 Windows 仅提供可编译的空实现，G0 交互只在 Windows 支持。
#![allow(dead_code)]
use crate::{
    log::Logger,
    platform::{FocusResult, ForegroundResult, HookEvent, ReplayBatch, Result, Target},
};
use std::time::Instant;
use tauri::WebviewWindow;
use tauri_plugin_global_shortcut::Shortcut;

pub struct Hooks;
impl Hooks {
    pub fn install(
        _: usize,
        _: usize,
        _: Shortcut,
        _: Logger,
        _: u64,
        _: impl Fn(HookEvent) + Send + 'static,
    ) -> Result<Self> {
        Err("G0 仅支持 Windows".into())
    }
    pub fn begin_search(&self, started: Instant) -> Result<Instant> {
        Ok(started)
    }
    pub fn remove_keyboard(&self) -> Result<ReplayBatch> {
        Ok(ReplayBatch::default())
    }
    pub fn external_switch(&self) -> bool {
        false
    }
}
pub mod hook {
    use tauri_plugin_global_shortcut::Shortcut;
    pub fn supported(_: &Shortcut) -> bool {
        true
    }
}
pub mod window {
    use super::*;
    pub fn handle(_: &WebviewWindow) -> Result<usize> {
        Ok(0)
    }
    pub fn foreground() -> usize {
        0
    }
    pub fn capture() -> Target {
        Target::default()
    }
    pub fn no_activate(_: &WebviewWindow, _: bool) -> Result<()> {
        Ok(())
    }
    pub fn show(_: &WebviewWindow, _: &Target) -> Result<()> {
        Err("G0 仅支持 Windows".into())
    }
    pub fn show_auto(_: &WebviewWindow) -> Result<()> {
        Err("G0 仅支持 Windows".into())
    }
    pub fn hide(_: &WebviewWindow) -> Result<()> {
        Ok(())
    }
    pub fn activate(_: usize) -> ForegroundResult {
        ForegroundResult {
            path: "unsupported",
            success: false,
        }
    }
    pub fn confirm(target: &Target) -> FocusResult {
        FocusResult {
            foreground: activate(0),
            focus_restored: false,
            focus_unknown: target.focus == 0,
        }
    }
    pub fn restore(target: &Target) -> FocusResult {
        confirm(target)
    }
}
pub mod paste {
    use super::*;
    use crate::platform::ReplayKey;
    pub fn cancel_alt_menu() -> u32 {
        0
    }
    pub fn replay(_: &[ReplayKey], _: usize, _: Instant) -> usize {
        0
    }
    pub fn execute(
        _: crate::paste::request::Request,
        _: &crate::paste::keys::Table,
        _: &super::clipboard::Service,
        _: &std::sync::atomic::AtomicBool,
        _: &dyn Fn(),
    ) -> serde_json::Value {
        serde_json::json!({"paste_result":null,"clipboard_written":false,"reason":"unsupported"})
    }
}
pub mod clipboard {
    use super::*;
    #[derive(Clone)]
    pub struct Service;
    impl Service {
        pub fn start(_: Logger) -> Result<Self> {
            Ok(Self)
        }
        pub fn shutdown(&self) {}
    }
}
pub mod webview {
    use super::*;
    #[derive(Default)]
    pub struct MemoryState;
    pub fn initialize(_: &WebviewWindow) -> Result<()> {
        Ok(())
    }
    pub fn set_visible(
        _: &WebviewWindow,
        _: bool,
        _: Logger,
        _: u64,
        _: std::sync::Arc<MemoryState>,
    ) -> Result<()> {
        Ok(())
    }
    pub fn set_visible_with_completion(
        _: &WebviewWindow,
        _: bool,
        _: Logger,
        _: u64,
        _: std::sync::Arc<MemoryState>,
        ready: Option<std::sync::mpsc::Sender<Result<Instant>>>,
    ) -> Result<()> {
        if let Some(ready) = ready {
            let _ = ready.send(Err("G0 仅支持 Windows".into()));
        }
        Ok(())
    }
    pub fn focus(_: &WebviewWindow) -> Result<()> {
        Err("G0 仅支持 Windows".into())
    }
}
