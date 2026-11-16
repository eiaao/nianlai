use crate::{
    log::Logger,
    platform::{HookEvent, HookOptions, ReplayBatch, Result, Target},
};
use std::time::Instant;
use tauri_plugin_global_shortcut::Shortcut;

pub struct Hooks;
// macOS 没有 Windows 预览键钩子，这里不需要。
pub fn set_preview_keys(_: bool) {}
pub fn supported(_: &Shortcut) -> bool {
    true
}
impl Hooks {
    // macOS 没有 Windows 键盘钩子，这里不需要。
    pub fn install(
        _: usize,
        _: &Target,
        _: Option<Shortcut>,
        _: Logger,
        _: u64,
        _: HookOptions,
        _: impl Fn(HookEvent) + Send + 'static,
    ) -> Result<Self> {
        Ok(Self)
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
