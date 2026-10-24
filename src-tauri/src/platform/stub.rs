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
    pub fn switched_to(&self) -> usize {
        0
    }
}
pub mod hook {
    use tauri_plugin_global_shortcut::Shortcut;
    pub fn supported(_: &Shortcut) -> bool {
        true
    }
}
pub mod dialog {
    pub fn error(text: &str) {
        eprintln!("{text}");
    }
}
// 输入法状态只在 Windows 上能查；这里只给出同样形状的 JSON。
pub mod ime {
    pub fn query(_: usize) -> serde_json::Value {
        serde_json::json!({"open":null,"conversion":null,"native":null,"layout":"0x00000000",
            "ime_window":false,"process_name":"","window_class":"","error":"unsupported"})
    }
    pub fn query_focused() -> serde_json::Value {
        query(0)
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
    pub fn class_name(_: usize) -> String {
        String::new()
    }
    pub fn process_of(_: usize) -> String {
        String::new()
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
    // 查不到完整性级别，和 Windows 版查不到时一样按"可能更高"处理。
    pub fn compare_integrity(_: &Target) -> serde_json::Value {
        serde_json::json!({"status":"unknown_assume_higher","target_token_known":false,"own_token_known":false})
    }
    pub fn execute(
        _: crate::paste::request::Request,
        _: &crate::paste::keys::Table,
        _: &super::clipboard::Service,
        _: &std::sync::atomic::AtomicBool,
        _: &dyn Fn(),
        _: Option<&std::path::Path>,
    ) -> serde_json::Value {
        serde_json::json!({"paste_result":null,"clipboard_written":false,"reason":"unsupported"})
    }
}
pub mod image {
    pub struct Saved {
        pub path: std::path::PathBuf,
        pub reused: bool,
        pub bytes: u64,
        pub source: &'static str,
        pub encode_ms: f64,
    }
    pub fn save_png(
        _: &crate::history::content::Content,
        _: &std::path::Path,
    ) -> Result<Saved, &'static str> {
        Err("unsupported")
    }
}
pub mod clipboard {
    use super::*;
    #[derive(Clone)]
    pub struct Service;
    impl Service {
        pub fn start(_: Logger, _: std::sync::Arc<crate::history::History>) -> Result<Self> {
            Ok(Self)
        }
        pub fn shutdown(&self) {}
    }
}
pub mod hash {
    pub fn sha256(_: &[u8]) -> super::Result<crate::history::content::Fingerprint> {
        Err("G0 仅支持 Windows".into())
    }
}
pub mod capture {
    pub struct Service;
    impl Service {
        pub fn start(
            _: super::Logger,
            _: std::sync::Arc<crate::history::History>,
            _: &super::clipboard::Service,
            _: bool,
            _: std::sync::Arc<dyn Fn() + Send + Sync>,
        ) -> super::Result<Self> {
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
    pub fn due(_: &MemoryState, _: std::time::Duration) -> bool {
        false
    }
    pub fn generation(_: &MemoryState) -> u64 {
        0
    }
    pub fn resuspend(
        _: &WebviewWindow,
        _: Logger,
        _: u64,
        _: std::sync::Arc<MemoryState>,
        _: std::time::Duration,
    ) -> Result<Option<u64>> {
        Ok(None)
    }
}
pub mod memory {
    pub fn sample() -> serde_json::Value {
        serde_json::Value::Null
    }
}
