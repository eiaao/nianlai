use serde::Serialize;
use std::time::{Duration, Instant};

// 切到搜索模式时补发首键的总等待上限。前端聚焦后还要等两帧让输入法接管，
// 实测最慢的一次补发已到 464 ms，500 ms 不够余量，超时整批按键会被丢弃。
pub const REPLAY_WAIT: Duration = Duration::from_millis(800);
pub const REPLAY_CAPACITY: usize = 32;

pub type Result<T> = std::result::Result<T, String>;

pub struct HookOptions {
    pub monitor: std::sync::Arc<crate::paste::permit::Monitor>,
    pub restricted: bool,
}

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
    ForegroundRevision(u64),
    Search(Instant),
    // 点击坐标、安装钩子时记下的面板范围、点击当时面板的实际范围，用于排查误判。
    Outside {
        point: [i32; 2],
        bounds: [i32; 4],
        current: [i32; 4],
    },
    // 管理员提示停留期间的按键要收起面板（M0-6b），带上结束原因；不带按的是哪个键。
    HintKey(HintEnd),
}

// 管理员提示停留时面板所处的模式，决定停留期间按键是放行给目标还是吞掉（M0-6b）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HintMode {
    // 快速模式：面板不激活，目标仍有焦点。带 Ctrl / Alt / Win 的组合键放行给目标，普通键吞掉。
    Quick,
    // 搜索模式：面板有焦点，按键吞掉，不进搜索框。
    Search,
}

// 停留被按键提前结束的原因，写进 elevated_hint_hidden 的 ended_by。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HintEnd {
    Escape,
    // 这个键放行给了目标：快速模式下带 Ctrl / Alt / Win 的组合键（例如 Ctrl+V 里的 V），或搜索模式下面板已被切走。
    KeyPassthrough,
    // 这个键被吞掉了：快速模式下的普通键（数字、字母、Enter……），或搜索模式下的非修饰键。
    KeyConsumed,
}
impl HintEnd {
    pub fn name(self) -> &'static str {
        match self {
            Self::Escape => "escape",
            Self::KeyPassthrough => "key_passthrough",
            Self::KeyConsumed => "key_consumed",
        }
    }
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
