use serde::Serialize;
use std::time::{Duration, Instant};

pub const REPLAY_WAIT: Duration = Duration::from_millis(500);
pub const REPLAY_CAPACITY: usize = 32;

pub type Result<T> = std::result::Result<T, String>;

// 句柄仅作为不拥有资源的标识跨线程传递，使用前重新校验。
#[derive(Clone, Default)]
pub struct Target {
    pub window: usize,
    pub focus: usize,
    pub thread: u32,
    pub process: u32,
    pub caret: Option<Caret>,
    pub process_name: String,
    pub window_class: String,
    pub focus_class: String,
}

#[derive(Clone, Copy)]
pub struct Caret {
    pub left: i32,
    pub top: i32,
    pub bottom: i32,
}

#[derive(Default)]
pub struct ReplayBatch {
    pub keys: Vec<ReplayKey>,
    pub dropped: usize,
    pub started: Option<Instant>,
}

#[derive(Clone, Copy)]
pub struct ReplayKey {
    pub vk: u16,
    pub scan: u16,
    pub extended: bool,
    pub shift: bool,
}

pub enum HookEvent {
    Key(&'static str),
    Search(Instant),
    // 点击坐标、安装钩子时记下的面板范围、点击当时面板的实际范围，用于排查误判。
    Outside {
        point: [i32; 2],
        bounds: [i32; 4],
        current: [i32; 4],
    },
}

#[derive(Clone, Copy, Serialize)]
pub struct ForegroundResult {
    pub path: &'static str,
    pub success: bool,
}
#[derive(Clone, Copy, Serialize)]
pub struct FocusResult {
    pub foreground: ForegroundResult,
    pub focus_restored: bool,
    pub focus_unknown: bool,
}

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;
#[cfg(not(windows))]
mod stub;
#[cfg(not(windows))]
pub use stub::*;
