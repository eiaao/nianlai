use super::{foreground, paste::INPUT_MARKER, window::thread_input_desktop_receiving_input};
use crate::{
    log::Logger,
    paste::permit::Monitor,
    platform::{
        HintEnd, HintMode, HookEvent, HookOptions, REPLAY_CAPACITY, REPLAY_WAIT, ReplayBatch,
        ReplayKey, Result, Target, key_state,
    },
};
use serde_json::json;
use std::{
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tauri_plugin_global_shortcut::{Modifiers, Shortcut};
use windows::Win32::{
    Foundation::{HINSTANCE, LPARAM, LRESULT, RECT, WPARAM},
    System::{
        LibraryLoader::GetModuleHandleW,
        Threading::{GetCurrentProcessId, GetCurrentThreadId},
    },
    UI::{
        Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent},
        Input::KeyboardAndMouse::{GetAsyncKeyState, MAPVK_VK_TO_CHAR, MapVirtualKeyW},
        WindowsAndMessaging::*,
    },
};

const HOOK_COMMAND: u32 = WM_APP + 1;

enum Command {
    Hotkey {
        shortcut: Option<Shortcut>,
        reply: mpsc::SyncSender<()>,
    },
    Activation {
        ready: bool,
        reply: mpsc::SyncSender<()>,
    },
    StopInputs {
        abandon: bool,
        reply: mpsc::SyncSender<Result<bool>>,
    },
    Fence {
        reply: mpsc::SyncSender<()>,
    },
    Begin {
        started: Instant,
        reply: mpsc::SyncSender<Instant>,
    },
    Stop {
        reply: mpsc::SyncSender<Result<Option<Instant>>>,
    },
    // 正常结束提示前在钩子线程检查配对；可以结束时当场卸钩，防止回执之后又吞掉新键。
    FinishHint {
        reply: mpsc::SyncSender<Result<bool>>,
    },
}

// 停留期间一个键第一次按下时定下的处置，抬起时照此配对，目标不会收到孤立的抬起。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fate {
    // 按下放行给了目标，抬起也放行。
    Pass,
    // 按下吞掉了，抬起也吞，抬起时才请求收起面板：收起会卸钩，先收的话抬起就漏给了还回焦点的目标。
    Swallow(HintEnd),
}

// 停留规则对一个按键事件的处理。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    // 交回原逻辑（不在停留；或原逻辑本来就放行、已经接管的键）。
    Original,
    // 放行；end 有值时同时请求收起面板。
    Pass { end: Option<HintEnd> },
    // 吞掉；end 有值时同时请求收起面板。
    Swallow { end: Option<HintEnd> },
}

// 停留规则的输入。
#[derive(Clone, Copy)]
struct HintKey {
    // None：不在管理员提示停留中。
    mode: Option<HintMode>,
    vk: u16,
    // 这个键本身是 Ctrl / Shift / Alt / Win。
    modifier: bool,
    pressed: bool,
    // 按下时 Ctrl、Alt、Win 至少按着一个（取钩子原有的 modifiers() 结果，不另查）。
    chord: bool,
    // 样机自己的全局快捷键：呼出键、退出键 / 采集调试键（原逻辑里的 exit）。原逻辑放行，由全局快捷键处理。
    shortcut: bool,
    // 原逻辑放行的系统组合：Win 组合、Alt+Tab。
    system: bool,
    // 原逻辑吞了这个键的按下、还没见到抬起：按住不放的重复按下和最后的抬起仍归原逻辑。
    original: bool,
    // 面板持有前台。只在搜索模式下看：停留期间用户用 Alt+Tab 之类切走了，按键本来就到不了搜索框，
    // 放行给当前前台并收起，不吞别的程序的键。
    focused: bool,
}

// 管理员提示停留期间按键怎么处理（M0-6b）。earlier 是停留规则先前对这个键按下的处置；
// 返回处理方式和这个键之后的处置记录。
// 快速模式：按着 Ctrl / Alt / Win 的组合键（Ctrl+V、Ctrl+Z、Alt+Tab……）放行给目标并在按下时就请求收起，
// 所以照提示按 Ctrl+V 能直接粘进管理员窗口；普通键（数字、字母、Enter、方向键、空格、Shift+字母……）吞掉、
// 抬起时收起，想换一条按 2 不会把“2”打进目标；不带 Ctrl / Alt / Win 的 Esc 吞掉、抬起时收起。
// 搜索模式：Esc 和其他非修饰键都吞掉（不进搜索框）、抬起时收起，焦点由收起按原逻辑还；面板已不在前台时
// 放行给当前前台并收起（Esc 仍吞掉）。
// 请求什么时候真发出去另由 hint_end 决定（还有吞掉的键按着就等它们抬起）。
// 两种模式下修饰键单独按都放行、不收起；呼出键、退出键交给全局快捷键（快速模式交回原逻辑）；
// 搜索模式下 Win 组合、Alt+Tab 照旧放行、不收起。
fn hint_route(key: HintKey, earlier: Option<Fate>) -> (Route, Option<Fate>) {
    let Some(mode) = key.mode else {
        return (Route::Original, earlier);
    };
    if key.modifier {
        return (Route::Pass { end: None }, None);
    }
    if !key.pressed {
        return match earlier {
            Some(Fate::Pass) => (Route::Pass { end: None }, None),
            Some(Fate::Swallow(end)) => (Route::Swallow { end: Some(end) }, None),
            // 停留开始前按下的键：快速模式由原逻辑配对；搜索模式下那时没有键盘钩子，按下已到面板，抬起也放行。
            None if mode == HintMode::Quick => (Route::Original, None),
            None => (Route::Pass { end: None }, None),
        };
    }
    if let Some(fate) = earlier {
        // 按住不放的重复按下：照第一次的处置，不再重复请求收起。
        let route = match fate {
            Fate::Pass => Route::Pass { end: None },
            Fate::Swallow(_) => Route::Swallow { end: None },
        };
        return (route, Some(fate));
    }
    let passthrough = (
        Route::Pass {
            end: Some(HintEnd::KeyPassthrough),
        },
        Some(Fate::Pass),
    );
    let consumed = (
        Route::Swallow { end: None },
        Some(Fate::Swallow(HintEnd::KeyConsumed)),
    );
    let escape = (
        Route::Swallow { end: None },
        Some(Fate::Swallow(HintEnd::Escape)),
    );
    match mode {
        // 搜索模式下原逻辑的吞键记录是首键补发前留下的，可能过时，不看。
        HintMode::Quick if key.original => (Route::Original, None),
        HintMode::Quick if key.shortcut => (Route::Original, Some(Fate::Pass)),
        // 带 Ctrl / Alt / Win 的都按组合键放行，含 Alt+Tab、Win 组合、Ctrl+Esc；单按 Esc 维持原处理。
        HintMode::Quick if key.chord => passthrough,
        HintMode::Quick if key.vk == 0x1B => escape,
        HintMode::Quick => consumed,
        HintMode::Search if key.shortcut || key.system => {
            (Route::Pass { end: None }, Some(Fate::Pass))
        }
        HintMode::Search if key.vk == 0x1B => escape,
        HintMode::Search if key.focused => consumed,
        HintMode::Search => passthrough,
    }
}

// 和 keyboard() 原逻辑开头放行的修饰键一致。
fn is_modifier(vk: u16) -> bool {
    matches!(vk, 0x10..=0x12 | 0x5B | 0x5C | 0xA0..=0xA5)
}

// 下面两个合起来和 keyboard() 原逻辑里放行的组合一致（停留规则单独用，原逻辑那段没动，测试钉住一致）。
// 样机自己的全局快捷键：呼出键，以及原逻辑里的 exit（退出键 Ctrl+Alt+Shift+Q、采集调试键 Ctrl+Alt+Shift+L）。
fn app_shortcut(vk: u16, mods: Modifiers, hotkey: Option<(u16, Modifiers)>) -> bool {
    let exit = matches!(vk, 0x51 | 0x4c)
        && mods == (Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT);
    hotkey.is_some_and(|hotkey| vk == hotkey.0 && mods == hotkey.1) || exit
}
// 系统组合：Win 组合、Alt+Tab。
fn system_combo(vk: u16, mods: Modifiers) -> bool {
    mods.contains(Modifiers::SUPER) || (vk == 0x09 && mods.contains(Modifiers::ALT))
}

// Ctrl、Alt、Win 至少按着一个（Shift 不算）。
fn chord(mods: Modifiers) -> bool {
    mods.intersects(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SUPER)
}

// 收起请求什么时候发（M0-6b）：还有被停留规则吞掉的键按着时先记着，等最后一个抬起再发——收起会卸钩，
// 先收的话那些键的抬起就漏给了目标。返回（现在要发的结束原因，继续记着的）；先请求的原因优先。
fn hint_end(
    requested: Option<HintEnd>,
    deferred: Option<HintEnd>,
    swallowed_held: bool,
) -> (Option<HintEnd>, Option<HintEnd>) {
    let end = deferred.or(requested);
    if swallowed_held {
        (None, end)
    } else {
        (end, None)
    }
}

// 停留规则跨事件的状态（M0-6b）：每个键按下时的处置，以及还没发出的收起请求。每次开始停留时清空。
struct HintState {
    keys: [Option<Fate>; 256],
    end: Option<HintEnd>,
    closing: bool,
}
impl HintState {
    fn new() -> Self {
        Self {
            keys: [None; 256],
            end: None,
            closing: false,
        }
    }
    // 提示开始前已经放行的按下，重复和抬起仍放行；原路径吞掉的键由调用方排除。
    #[cfg(test)]
    fn with_passed(pressed: impl IntoIterator<Item = u16>) -> Self {
        let mut state = Self::new();
        for vk in pressed {
            if vk <= 255 && !is_modifier(vk) {
                state.keys[vk as usize] = Some(Fate::Pass);
            }
        }
        state
    }
    fn has_swallowed(&self, original_held: bool) -> bool {
        original_held
            || self
                .keys
                .iter()
                .any(|fate| matches!(fate, Some(Fate::Swallow(_))))
    }
    fn request_close(&mut self, original_held: bool) -> bool {
        self.closing = self.has_swallowed(original_held);
        !self.closing
    }
    fn close_ready(&self, original_held: bool) -> bool {
        self.closing && !self.has_swallowed(original_held)
    }
    // 处理一个按键事件（vk 不超过 255），返回（None 交回原逻辑 / Some(是否吞掉)，现在要发的收起请求）。
    fn handle(&mut self, key: HintKey) -> (Option<bool>, Option<HintEnd>) {
        let index = key.vk as usize;
        let (route, fate) = hint_route(key, self.keys[index]);
        self.keys[index] = fate;
        let (swallow, requested) = match route {
            Route::Original => return (None, None),
            Route::Pass { end } => (false, end),
            Route::Swallow { end } => (true, end),
        };
        let held = self
            .keys
            .iter()
            .any(|fate| matches!(fate, Some(Fate::Swallow(_))));
        let (end, deferred) = hint_end(requested, self.end, held);
        self.end = deferred;
        (Some(swallow), end)
    }
}

struct Context {
    sender: mpsc::SyncSender<HookEvent>,
    window: usize,
    target: usize,
    target_focus: usize,
    // 第一个切到前台的外部窗口；0 表示没切走。
    switched: Arc<AtomicUsize>,
    monitor: Arc<Monitor>,
    restricted: Option<bool>,
    draining: bool,
    drain_passed: [bool; 256],
    drain_snapshot_counts: key_state::Counts,
    drain_ignored_callbacks: key_state::Counts,
    drain_requires_recheck: bool,
    drain_recheck_available: bool,
    drain_recheck_samples: usize,
    drain_rechecked_releases: usize,
    drain_rechecked_presses: usize,
    bounds: RECT,
    hotkey: Option<(u16, Modifiers)>,
    replay: mpsc::SyncSender<ReplayKey>,
    dropped: Arc<AtomicUsize>,
    pending: Option<Instant>,
    swallowed: [bool; 256],
    release_action: [Option<&'static str>; 256],
    // 管理员提示停留中（M0-6b）：Some 时 keyboard() 先走停留规则；None 时完全是原逻辑。
    hint: Option<HintMode>,
    // 停留规则的逐键处置和待发的收起请求，抬起时照此配对。
    hint_state: HintState,
    // 钩子装上之后看到的修饰键按住状态（按左右键分位）；只用来给面板页脚报“按住 Ctrl/Shift”。
    held_modifiers: u8,
    reported_modifiers: u8,
}
impl Context {
    fn set_hotkey(&mut self, shortcut: Option<Shortcut>) {
        self.hotkey =
            shortcut.and_then(|shortcut| shortcut_vk(&shortcut).map(|vk| (vk, shortcut.mods)));
    }

    // 修饰键照常放行给目标，只把“只按着 Ctrl / 只按着 Shift / 都按着”的变化报给面板。
    // 钩子装上前就按住的键（例如呼出用的 Ctrl）看不到按下，松开时不会误报。
    fn report_modifier(&mut self, vk: u16, pressed: bool) {
        let bit = match vk {
            0xA2 => 1,
            0xA3 => 2,
            0xA0 => 4,
            0xA1 => 8,
            0xA4 => 16,
            0xA5 => 32,
            0x5B => 64,
            0x5C => 128,
            _ => return,
        };
        if pressed {
            self.held_modifiers |= bit;
        } else {
            self.held_modifiers &= !bit;
        }
        let held = self.held_modifiers;
        let state = if held & 0xF0 != 0 {
            0
        } else {
            u8::from(held & 3 != 0) | (u8::from(held & 12 != 0) << 1)
        };
        if state == self.reported_modifiers {
            return;
        }
        self.reported_modifiers = state;
        let name = match state {
            1 => "Mod:Ctrl",
            2 => "Mod:Shift",
            3 => "Mod:Ctrl+Shift",
            _ => "Mod:",
        };
        let _ = self.sender.try_send(HookEvent::Key(name));
    }

    fn original_hint_keys_held(&self) -> bool {
        // 搜索首键已补发后的旧 swallowed 数组不代表仍按住；只有钩子一直在位时才看它。
        ((self.hint.is_some() && self.hint != Some(HintMode::Search)) || self.pending.is_some())
            && self.swallowed.contains(&true)
    }
    fn notify_hint_close(&mut self) {
        if self.hint.is_some()
            && self.hint_state.close_ready(self.original_hint_keys_held())
            && self
                .sender
                .try_send(HookEvent::HintKey(HintEnd::KeyConsumed))
                .is_ok()
        {
            self.hint_state.closing = false;
        }
    }
    // 停留期间的按键：Some(是否吞掉) 表示停留规则已处理；None 表示交回原逻辑。
    fn hint_key(&mut self, vk: u16, pressed: bool, mode: HintMode) -> Option<bool> {
        if vk > 255 {
            return None;
        }
        if (mode == HintMode::Quick || self.pending.is_some()) && self.swallowed[vk as usize] {
            // 提示开始前吞掉的键继续吞重复，避免中途加上 Win / Alt 后被原路径重新分类为放行。
            // 抬起仍交回原路径清除 swallowed、处理 release_action，保留配对和原来的抬起动作。
            return pressed.then_some(true);
        }
        // 修饰键状态和原逻辑一样取 modifiers()，每个按键事件只取一次。
        let mods = modifiers();
        let (swallow, end) = self.hint_state.handle(HintKey {
            mode: Some(mode),
            vk,
            modifier: is_modifier(vk),
            pressed,
            chord: chord(mods),
            shortcut: app_shortcut(vk, mods, self.hotkey),
            system: system_combo(vk, mods),
            original: self.swallowed[vk as usize],
            focused: super::window::foreground() == self.window,
        });
        if let Some(end) = end
            && self.sender.try_send(HookEvent::HintKey(end)).is_ok()
        {
            self.hint_state.closing = false;
        }
        swallow
    }
    fn buffer(&self, key: &KBDLLHOOKSTRUCT, mods: Modifiers) {
        let key = ReplayKey {
            vk: key.vkCode as u16,
            scan: key.scanCode as u16,
            extended: key.flags.contains(LLKHF_EXTENDED),
            shift: mods.contains(Modifiers::SHIFT),
        };
        if self.replay.try_send(key).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}
thread_local! { static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) }; }

// 完整预览开着时（M2-7）：Z 切换图片的“适应 / 原尺寸”、Ctrl+C 复制预览里选中的文字，这两个键交给面板；
// 预览关着时它们照旧（Z 开始打字搜索，Ctrl+C 放行给目标）。面板收起时清掉。
static PREVIEW_KEYS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn set_preview_keys(open: bool) {
    PREVIEW_KEYS.store(open, std::sync::atomic::Ordering::Release);
}

pub struct Hooks {
    id: u32,
    thread: Option<JoinHandle<()>>,
    relay: Option<JoinHandle<()>>,
    commands: mpsc::Sender<Command>,
    replay: mpsc::Receiver<ReplayKey>,
    dropped: Arc<AtomicUsize>,
    switched: Arc<AtomicUsize>,
    monitor: Arc<Monitor>,
    rechecked_releases: Arc<AtomicUsize>,
}

unsafe extern "system" fn foreground_changed(
    _: HWINEVENTHOOK,
    kind: u32,
    hwnd: windows::Win32::Foundation::HWND,
    object: i32,
    child: i32,
    _: u32,
    event_time: u32,
) {
    CONTEXT.with(|context| {
        if let Some(context) = context.borrow().as_ref() {
            let event = foreground::Event {
                kind,
                window: hwnd.0 as usize,
                object,
                child,
                time: event_time,
            };
            let expected = event.is_foreground();
            if expected && !hwnd.is_invalid() {
                foreground::record_first_external(
                    &context.switched,
                    hwnd.0 as usize,
                    context.window,
                    context.target,
                );
            }
            // 精确注册的 hook 理论上只收到前台窗口事件；意外种类不能冒充归还。
            let revision = if expected {
                context
                    .monitor
                    .observe(event.window, event.time, super::window::foreground())
            } else {
                Some(context.monitor.uncertain())
            };
            if let Some(revision) = revision
                && context
                    .sender
                    .try_send(HookEvent::ForegroundRevision(revision))
                    .is_err()
            {
                // 通知队列丢失意味着前后端无法确定选择顺序，本会话不再允许自动发送。
                context.monitor.stopped();
            }
        }
    });
}

unsafe extern "system" fn original_destroyed(
    _: HWINEVENTHOOK,
    _: u32,
    hwnd: windows::Win32::Foundation::HWND,
    object: i32,
    child: i32,
    _: u32,
    _: u32,
) {
    if object != 0 || child != 0 {
        return;
    }
    CONTEXT.with(|context| {
        if let Some(context) = context.borrow().as_ref()
            && (hwnd.0 as usize == context.target || hwnd.0 as usize == context.target_focus)
        {
            let revision = context.monitor.target_destroyed();
            if context
                .sender
                .try_send(HookEvent::ForegroundRevision(revision))
                .is_err()
            {
                context.monitor.stopped();
            }
        }
    });
}

// 覆盖全局快捷键插件可注册的常用键；未知键在保存前拒绝，避免钩子阻断呼出键。
pub fn shortcut_vk(shortcut: &Shortcut) -> Option<u16> {
    let key = shortcut.key.to_string();
    if let Some(key) = key.strip_prefix("Key").filter(|key| key.len() == 1) {
        return Some(key.as_bytes()[0] as u16);
    }
    if let Some(key) = key.strip_prefix("Digit").filter(|key| key.len() == 1) {
        return Some(key.as_bytes()[0] as u16);
    }
    if let Some(number) = key
        .strip_prefix('F')
        .and_then(|n| n.parse::<u16>().ok())
        .filter(|n| (1..=24).contains(n))
    {
        return Some(0x70 + number - 1);
    }
    if let Some(number) = key
        .strip_prefix("Numpad")
        .and_then(|n| n.parse::<u16>().ok())
        .filter(|n| *n <= 9)
    {
        return Some(0x60 + number);
    }
    Some(match key.as_str() {
        "Space" => 0x20,
        "Enter" | "NumpadEnter" => 0x0D,
        "Tab" => 0x09,
        "Escape" => 0x1B,
        "Backspace" => 0x08,
        "Delete" => 0x2E,
        "Insert" => 0x2D,
        "Home" => 0x24,
        "End" => 0x23,
        "PageUp" => 0x21,
        "PageDown" => 0x22,
        "ArrowLeft" => 0x25,
        "ArrowUp" => 0x26,
        "ArrowRight" => 0x27,
        "ArrowDown" => 0x28,
        "Slash" => 0xBF,
        "Backslash" => 0xDC,
        "Comma" => 0xBC,
        "Period" => 0xBE,
        "Semicolon" => 0xBA,
        "Quote" => 0xDE,
        "BracketLeft" => 0xDB,
        "BracketRight" => 0xDD,
        "Minus" => 0xBD,
        "Equal" => 0xBB,
        "Backquote" => 0xC0,
        "NumpadAdd" => 0x6B,
        "NumpadSubtract" => 0x6D,
        "NumpadMultiply" => 0x6A,
        "NumpadDivide" => 0x6F,
        "NumpadDecimal" => 0x6E,
        "CapsLock" => 0x14,
        "NumLock" => 0x90,
        "ScrollLock" => 0x91,
        "PrintScreen" => 0x2C,
        "Pause" => 0x13,
        _ => return None,
    })
}

pub fn supported(shortcut: &Shortcut) -> bool {
    shortcut_vk(shortcut).is_some()
}

fn printable(vk: u16) -> bool {
    // MapVirtualKey 也会返回 Tab/Backspace 等控制字符，不能把它们当作搜索首字。
    let character = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_CHAR) } & 0x7fff_ffff;
    character >= 0x20 && character != 0x7f
}

fn down(key: i32) -> bool {
    unsafe { GetAsyncKeyState(key) < 0 }
}
fn modifiers() -> Modifiers {
    let mut mods = Modifiers::empty();
    if down(0x10) {
        mods |= Modifiers::SHIFT;
    }
    if down(0x11) {
        mods |= Modifiers::CONTROL;
    }
    if down(0x12) {
        mods |= Modifiers::ALT;
    }
    if down(0x5B) || down(0x5C) {
        mods |= Modifiers::SUPER;
    }
    mods
}

unsafe extern "system" fn keyboard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    let key = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
    if key.dwExtraInfo == INPUT_MARKER {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    let vk = normalized_vk(
        key.vkCode as u16,
        key.scanCode as u16,
        key.flags.contains(LLKHF_EXTENDED),
    );
    let pressed = wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN;
    let mut hint_handled = false;
    let swallow = CONTEXT.with(|context| {
        let mut context = context.borrow_mut();
        let Some(context) = context.as_mut() else {
            return false;
        };
        if context.draining && vk <= 255 && !context.swallowed[vk as usize] {
            // 搜索的原生 Enter 已送达面板；只跟踪它和后来的放行键，等抬起后才归还前台。
            key_state::track(&mut context.drain_passed, vk, pressed);
            if !key_state::pairable(vk) {
                context.drain_ignored_callbacks.record(vk);
            }
            return false;
        }
        // M0-6b：管理员提示停留中才进这个分支；不在停留时 hint 为 None，下面的原逻辑一行没改。
        if let Some(mode) = context.hint
            && let Some(swallow) = context.hint_key(vk, pressed, mode)
        {
            hint_handled = true;
            return swallow;
        }
        if matches!(vk, 0x10..=0x12 | 0x5B | 0x5C | 0xA0..=0xA5) {
            context.report_modifier(vk, pressed);
            return false;
        }
        if vk > 255 {
            return true;
        }
        if !pressed && context.swallowed[vk as usize] {
            context.swallowed[vk as usize] = false;
            if let Some(action) = context.release_action[vk as usize].take()
                && context.pending.is_none()
                && !context.draining
                && context.restricted.is_none()
            {
                let _ = context.sender.try_send(HookEvent::Key(action));
            }
            return true;
        }
        if let Some(swallow) = activation_route(
            context.restricted,
            super::window::foreground() == context.window,
            context.draining,
            context.swallowed[vk as usize],
        ) {
            return swallow;
        }
        let mods = modifiers();
        let exit = matches!(vk, 0x51 | 0x4c)
            && mods == (Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT);
        if mods.contains(Modifiers::SUPER)
            || (vk == 0x09 && mods.contains(Modifiers::ALT))
            || context
                .hotkey
                .is_some_and(|hotkey| vk == hotkey.0 && mods == hotkey.1)
            || exit
        {
            if pressed {
                context.swallowed[vk as usize] = false;
                context.release_action[vk as usize] = None;
            }
            return false;
        }
        if !pressed {
            return false;
        }
        // 按键重复时这个键已经在吞；只有第一下按下才算“按下”。
        let first_press = !context.swallowed[vk as usize];
        context.swallowed[vk as usize] = true;
        if context.pending.is_some() {
            context.buffer(key, mods);
            return true;
        }
        let navigation = if mods.is_empty() {
            match vk {
                0x25 => Some("ArrowLeft"),
                0x26 => Some("ArrowUp"),
                0x27 => Some("ArrowRight"),
                0x28 => Some("ArrowDown"),
                0x21 => Some("PageUp"),
                0x22 => Some("PageDown"),
                0x24 => Some("Home"),
                0x23 => Some("End"),
                0x0D => Some("Enter"),
                0x1B => Some("Escape"),
                0x20 => Some("Space"),
                0x2E => Some("Delete"),
                0x31 | 0x61 => Some("1"),
                0x32 | 0x62 => Some("2"),
                0x33 | 0x63 => Some("3"),
                0x34 | 0x64 => Some("4"),
                0x35 | 0x65 => Some("5"),
                0x36 | 0x66 => Some("6"),
                0x37 | 0x67 => Some("7"),
                0x38 | 0x68 => Some("8"),
                0x39 | 0x69 => Some("9"),
                _ => None,
            }
        } else if mods == Modifiers::SHIFT && vk == 0x0D {
            Some("Shift+Enter")
        } else if mods == Modifiers::CONTROL && vk == 0x0D {
            Some("Ctrl+Enter")
        } else if mods == Modifiers::CONTROL && vk == 0x44 {
            Some("Ctrl+D")
        } else if mods == Modifiers::CONTROL && vk == 0x5A {
            Some("Ctrl+Z")
        } else if mods == Modifiers::CONTROL
            && vk == 0x43
            && PREVIEW_KEYS.load(std::sync::atomic::Ordering::Acquire)
        {
            Some("Ctrl+C")
        } else {
            None
        };
        let navigation = navigation.or_else(|| {
            (mods.is_empty()
                && vk == 0x5A
                && PREVIEW_KEYS.load(std::sync::atomic::Ordering::Acquire))
            .then_some("Z")
        });
        if let Some(name) = navigation {
            // 确认、关闭、预览开合与收藏等到抬起再投递：避免卸钩后把配对抬起漏给目标控件，
            // 也避免长按 Space 反复开合、长按 Ctrl+D 反复收藏/取消。
            if matches!(
                name,
                "Enter"
                    | "Shift+Enter"
                    | "Ctrl+Enter"
                    | "Ctrl+D"
                    | "Ctrl+Z"
                    | "Ctrl+C"
                    | "Z"
                    | "Delete"
                    | "Escape"
                    | "Space"
                    | "1"
                    | "2"
                    | "3"
                    | "4"
                    | "5"
                    | "6"
                    | "7"
                    | "8"
                    | "9"
            ) {
                // 删除收藏要按住 Delete（B12）：按下先报一次，松开照常投递 "Delete"，前端据此计时。
                if name == "Delete" && first_press {
                    let _ = context.sender.try_send(HookEvent::Key("Delete:down"));
                }
                context.release_action[vk as usize] = Some(name);
            } else {
                let _ = context.sender.try_send(HookEvent::Key(name));
            }
        } else if (mods.is_empty() && matches!(vk, 0xBF | 0x6F))
            || (mods == Modifiers::CONTROL && vk == 0x46)
        {
            let started = Instant::now();
            if context.sender.try_send(HookEvent::Search(started)).is_ok() {
                context.pending = Some(started);
            }
        } else if (mods.is_empty() || mods == Modifiers::SHIFT) && printable(vk) {
            let started = Instant::now();
            if context.sender.try_send(HookEvent::Search(started)).is_ok() {
                context.pending = Some(started);
                context.buffer(key, mods);
            }
        }
        true
    });
    // 原路径接管的键抬起也能解除延后关闭；这里只投递事件，实际卸钩由 FinishHint 再原子检查。
    CONTEXT.with(|context| {
        if let Some(context) = context.borrow_mut().as_mut() {
            context.notify_hint_close();
        }
    });
    if swallow {
        LRESULT(1)
    } else {
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }
}

// 已吞按键保持配对；失焦、未就绪或等待松键时，新按下交给实际前台。
fn activation_route(
    restricted: Option<bool>,
    _focused: bool,
    draining: bool,
    swallowed: bool,
) -> Option<bool> {
    if restricted.is_none() && !draining {
        return None;
    }
    if swallowed {
        return Some(true);
    }
    Some(false)
}

fn normalized_vk(vk: u16, scan: u16, extended: bool) -> u16 {
    match vk {
        0x10 => {
            if scan == 0x36 {
                0xA1
            } else {
                0xA0
            }
        }
        0x11 => {
            if extended {
                0xA3
            } else {
                0xA2
            }
        }
        0x12 => {
            if extended {
                0xA5
            } else {
                0xA4
            }
        }
        _ => vk,
    }
}
fn drain_snapshot(swallowed: &[bool; 256], physical: impl Fn(u16) -> bool) -> [bool; 256] {
    key_state::snapshot(swallowed, physical)
}
fn inputs_released(swallowed: &[bool; 256], passed: &[bool; 256]) -> bool {
    !swallowed.contains(&true) && !passed.contains(&true)
}

fn native_panel_input_readable(panel: usize) -> bool {
    if panel == 0 || super::window::foreground() != panel {
        return false;
    }
    unsafe {
        let mut process = 0;
        if GetWindowThreadProcessId(super::window::hwnd(panel), Some(&mut process)) == 0
            || process != GetCurrentProcessId()
        {
            return false;
        }
    }
    thread_input_desktop_receiving_input().unwrap_or(false) && super::window::foreground() == panel
}

fn drain_ready(
    swallowed: &[bool; 256],
    passed: &[bool; 256],
    requires_recheck: bool,
    recheck_available: bool,
    abandon: bool,
) -> bool {
    inputs_released(swallowed, passed) && (!requires_recheck || recheck_available || abandon)
}

#[derive(Default)]
struct DrainTrace {
    started: Option<Instant>,
    reported_waiting: bool,
    reported_abandon: bool,
    finished: bool,
}
impl DrainTrace {
    fn observe(&mut self, now: Instant, abandon: bool, finished: bool) -> Vec<&'static str> {
        if self.finished {
            return Vec::new();
        }
        let mut stages = Vec::new();
        let started = match self.started {
            Some(started) => started,
            None => {
                self.started = Some(now);
                stages.push("started");
                now
            }
        };
        if !finished
            && !self.reported_waiting
            && now.saturating_duration_since(started) >= Duration::from_secs(1)
        {
            self.reported_waiting = true;
            stages.push("waiting");
        }
        if abandon && !self.reported_abandon {
            self.reported_abandon = true;
            stages.push("abandoned");
        }
        if finished {
            self.finished = true;
            stages.push("finished");
        }
        stages
    }
}

fn report_drain(
    logger: &Logger,
    session: u64,
    trace: &mut DrainTrace,
    abandon: bool,
    finished: bool,
    keyboard_hook: bool,
    error: Option<&str>,
) {
    let now = Instant::now();
    for stage in trace.observe(now, abandon, finished) {
        let mut fields =
            CONTEXT.with(|context| {
                context.borrow().as_ref().map_or_else(|| json!({}), |context| json!({
                "pending_swallowed":context.swallowed.iter().filter(|&&held| held).count(),
                "pending_passed":context.drain_passed.iter().filter(|&&held| held).count(),
                "snapshot_counts":context.drain_snapshot_counts,
                "ignored_callback_counts":context.drain_ignored_callbacks,
                "physical_recheck_available":context.drain_recheck_available,
                "physical_recheck_samples":context.drain_recheck_samples,
                "physical_rechecked_releases":context.drain_rechecked_releases,
                "physical_rechecked_presses":context.drain_rechecked_presses,
            }))
            });
        fields["session"] = json!(session);
        fields["stage"] = json!(stage);
        fields["abandon"] = json!(abandon);
        fields["elapsed_ms"] = json!(
            trace
                .started
                .map(|started| now.saturating_duration_since(started).as_secs_f64() * 1000.0)
        );
        fields["keyboard_hook"] = json!(keyboard_hook);
        fields["error"] = json!(error);
        logger.event("input_drain", fields);
    }
}

unsafe extern "system" fn mouse(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0
        && matches!(
            wparam.0 as u32,
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
        )
    {
        let mouse = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        CONTEXT.with(|context| {
            if let Some(context) = context.borrow().as_ref() {
                let r = context.bounds;
                if mouse.pt.x < r.left
                    || mouse.pt.x >= r.right
                    || mouse.pt.y < r.top
                    || mouse.pt.y >= r.bottom
                {
                    let current = super::window::rect(context.window).unwrap_or_default();
                    let _ = context.sender.try_send(HookEvent::Outside {
                        point: [mouse.pt.x, mouse.pt.y],
                        bounds: [r.left, r.top, r.right, r.bottom],
                        current: [current.left, current.top, current.right, current.bottom],
                    });
                }
            }
        });
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

impl Hooks {
    pub fn install(
        window: usize,
        target: &Target,
        shortcut: Option<Shortcut>,
        logger: Logger,
        session: u64,
        options: HookOptions,
        callback: impl Fn(HookEvent) + Send + 'static,
    ) -> Result<Self> {
        let target_focus = target.focus;
        let target = target.window;
        let monitor = options.monitor;
        let restricted = options.restricted;
        let bounds = super::window::rect(window)?;
        let hotkey = shortcut
            .map(|shortcut| {
                shortcut_vk(&shortcut)
                    .map(|vk| (vk, shortcut.mods))
                    .ok_or("不支持此快捷键")
            })
            .transpose()?;
        let (sender, receiver) = mpsc::sync_channel(128);
        let relay = thread::Builder::new()
            .name("panel-hook-events".into())
            .spawn(move || {
                while let Ok(event) = receiver.recv() {
                    callback(event);
                }
            })
            .map_err(|e| e.to_string())?;
        let (ready, receive_ready) = mpsc::sync_channel(1);
        let (replay_sender, replay) = mpsc::sync_channel(REPLAY_CAPACITY);
        let (command_sender, commands) = mpsc::channel();
        let dropped = Arc::new(AtomicUsize::new(0));
        let overflow = dropped.clone();
        let switched = Arc::new(AtomicUsize::new(0));
        let switched_context = switched.clone();
        let monitor_context = monitor.clone();
        let rechecked_releases = Arc::new(AtomicUsize::new(0));
        let releases_context = rechecked_releases.clone();
        let thread = thread::Builder::new().name("panel-hooks".into()).spawn(move || unsafe {
            CONTEXT.with(|context| *context.borrow_mut() = Some(Context {
                sender, window, target, target_focus, switched:switched_context, monitor:monitor_context.clone(),
                restricted:restricted.then_some(false), draining:false, drain_passed:[false;256],
                drain_snapshot_counts:key_state::Counts::default(), drain_ignored_callbacks:key_state::Counts::default(),
                drain_requires_recheck:false, drain_recheck_available:false, drain_recheck_samples:0,
                drain_rechecked_releases:0, drain_rechecked_presses:0,
                bounds, hotkey, replay: replay_sender,
                dropped: overflow, pending: None, swallowed: [false; 256], release_action: [None; 256],
                hint: None, hint_state: HintState::new(), held_modifiers: 0, reported_modifiers: 0,
            }));
            let mut message = MSG::default();
            let mut drain_trace = DrainTrace::default();
            let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
            let module = GetModuleHandleW(None).ok().map(|m| HINSTANCE(m.0));
            let mut keyboard_hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), module, 0).ok();
            let mut mouse_hook = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), module, 0).ok();
            let foreground_hook = SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND,
                None, Some(foreground_changed), 0, 0, WINEVENT_OUTOFCONTEXT);
            let destroy_hook = SetWinEventHook(EVENT_OBJECT_DESTROY, EVENT_OBJECT_DESTROY,
                None, Some(original_destroyed), 0, 0, WINEVENT_OUTOFCONTEXT);
            let installed = keyboard_hook.is_some() && mouse_hook.is_some() && !foreground_hook.is_invalid() && !destroy_hook.is_invalid();
            if installed { monitor_context.started(); }
            logger.event("hooks_installed", json!({"session":session,"keyboard_hook":keyboard_hook.is_some(),"mouse_hook":mouse_hook.is_some(),
                "bounds":[bounds.left, bounds.top, bounds.right, bounds.bottom]}));
            let _ = ready.send(if installed { Ok(GetCurrentThreadId()) } else { Err("低层钩子安装失败".to_owned()) });
            if installed {
                loop {
                    let result = GetMessageW(&mut message, None, 0, 0).0;
                    if result <= 0 { break; }
                    if message.message == HOOK_COMMAND {
                        while let Ok(command) = commands.try_recv() {
                            match command {
                                Command::Hotkey { shortcut, reply } => {
                                    CONTEXT.with(|context| {
                                        if let Some(context) = context.borrow_mut().as_mut() {
                                            context.set_hotkey(shortcut);
                                        }
                                    });
                                    let _ = reply.send(());
                                }
                                Command::Activation { ready, reply } => {
                                    CONTEXT.with(|context| {
                                        if let Some(context) = context.borrow_mut().as_mut() {
                                            context.restricted = Some(ready);
                                        }
                                    });
                                    let _ = reply.send(());
                                }
                                Command::Fence { reply } => { let _ = reply.send(()); }
                                Command::StopInputs { abandon, reply } => {
                                    let reinstalled = keyboard_hook.is_none();
                                    if reinstalled {
                                        let install = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), module, 0);
                                        let error_code = install.as_ref().err().map(|error| error.code().0);
                                        keyboard_hook = install.ok();
                                        logger.event("drain_hook_install", json!({"session":session,
                                            "success":keyboard_hook.is_some(),"error_code":error_code}));
                                    }
                                    let ready = CONTEXT.with(|context| {
                                        let mut context = context.borrow_mut();
                                        let Some(context) = context.as_mut() else { return true; };
                                        if !context.draining {
                                            if reinstalled { context.swallowed = [false;256]; context.release_action = [None;256]; }
                                            let pressed:[bool;256] = std::array::from_fn(|vk| down(vk as i32));
                                            context.drain_snapshot_counts = key_state::Counts::sample(|vk| pressed[vk as usize]);
                                            context.drain_passed = drain_snapshot(&context.swallowed, |vk| pressed[vk as usize]);
                                            context.drain_requires_recheck = reinstalled || context.restricted.is_some()
                                                || super::window::foreground() == context.window;
                                            context.draining = true;
                                        }
                                        if !abandon && context.drain_requires_recheck {
                                            let readable = native_panel_input_readable(context.window);
                                            let recheck = key_state::recheck(&mut context.drain_passed, &context.swallowed,
                                                readable, |vk| down(i32::from(vk)), || native_panel_input_readable(context.window));
                                            context.drain_recheck_available = recheck.is_some();
                                            if let Some(recheck) = recheck {
                                                context.drain_recheck_samples = context.drain_recheck_samples.saturating_add(1);
                                                context.drain_rechecked_releases = context.drain_rechecked_releases.saturating_add(recheck.released);
                                                context.drain_rechecked_presses = context.drain_rechecked_presses.saturating_add(recheck.newly_pressed);
                                                let _ = releases_context.try_update(Ordering::Relaxed, Ordering::Relaxed,
                                                    |count| Some(count.saturating_add(recheck.released)));
                                            }
                                        }
                                        if abandon && super::window::foreground() != 0 {
                                            // 撤销后不再归还或发送；跨完整性可能缺回调，只依据可见的物理松键清理。
                                            for vk in 0..256 {
                                                if !down(vk as i32) {
                                                    context.swallowed[vk] = false;
                                                    context.drain_passed[vk] = false;
                                                    context.release_action[vk] = None;
                                                }
                                            }
                                        }
                                        drain_ready(&context.swallowed, &context.drain_passed, context.drain_requires_recheck,
                                            context.drain_recheck_available, abandon)
                                            && !context.hint_state.has_swallowed(false)
                                    });
                                    let result = if keyboard_hook.is_none() { Err("松键监听安装失败".to_owned()) } else if !ready { Ok(false) } else {
                                        let result = if let Some(hook) = keyboard_hook {
                                            UnhookWindowsHookEx(hook).map_err(|e| e.to_string())
                                        } else { Ok(()) };
                                        result.and_then(|()| {
                                            keyboard_hook = None;
                                            if let Some(hook) = mouse_hook {
                                                UnhookWindowsHookEx(hook).map_err(|e| e.to_string())?;
                                                mouse_hook = None;
                                            }
                                            Ok(true)
                                        })
                                    };
                                    if result.is_err() { monitor_context.stopped(); }
                                    report_drain(&logger, session, &mut drain_trace, abandon,
                                        !matches!(result, Ok(false)), keyboard_hook.is_some(), result.as_ref().err().map(String::as_str));
                                    let _ = reply.send(result);
                                }
                                Command::Begin { started, reply } => {
                                    let started = CONTEXT.with(|context| {
                                        let mut context = context.borrow_mut();
                                        let context = context.as_mut().unwrap();
                                        *context.pending.get_or_insert(started)
                                    });
                                    let _ = reply.send(started);
                                }
                                Command::Stop { reply } => {
                                    let result = if let Some(hook) = keyboard_hook {
                                        UnhookWindowsHookEx(hook).map_err(|e| e.to_string())
                                    } else { Ok(()) };
                                    let result = result.map(|()| {
                                        keyboard_hook = None;
                                        CONTEXT.with(|context| context.borrow_mut().as_mut().and_then(|context| context.pending.take()))
                                    });
                                    logger.event("keyboard_hook_removed", json!({"session":session,"keyboard_hook":keyboard_hook.is_some(),"mouse_hook":true,"error":result.as_ref().err()}));
                                    // 回执排在卸钩之后；收到回执再取队列，才不会漏掉最后一次回调。
                                    let _ = reply.send(result);
                                }
                                Command::FinishHint { reply } => {
                                    let ready = CONTEXT.with(|context| {
                                        let mut context = context.borrow_mut();
                                        let Some(context) = context.as_mut() else { return true; };
                                        let original_held = context.original_hint_keys_held();
                                        context.hint_state.request_close(original_held)
                                    });
                                    let result = if !ready { Ok(false) } else {
                                        let removed = if let Some(hook) = keyboard_hook {
                                            UnhookWindowsHookEx(hook).map_err(|e| e.to_string())
                                        } else { Ok(()) };
                                        removed.map(|()| { keyboard_hook = None; true })
                                    };
                                    logger.event("hint_keys_finish", json!({"session":session,
                                        "ready":ready,"keyboard_hook":keyboard_hook.is_some(),"error":result.as_ref().err()}));
                                    let _ = reply.send(result);
                                }
                            }
                        }
                    } else { let _ = TranslateMessage(&message); DispatchMessageW(&message); }
                }
            }
            if drain_trace.started.is_some() && !drain_trace.finished {
                report_drain(&logger, session, &mut drain_trace, true, true, keyboard_hook.is_some(), Some("监听线程退出"));
            }
            monitor_context.stopped();
            if let Some(hook) = keyboard_hook.take() { let _ = UnhookWindowsHookEx(hook); }
            if let Some(hook) = mouse_hook { let _ = UnhookWindowsHookEx(hook); }
            if !foreground_hook.is_invalid() { let _ = UnhookWinEvent(foreground_hook); }
            if !destroy_hook.is_invalid() { let _ = UnhookWinEvent(destroy_hook); }
            CONTEXT.with(|context| *context.borrow_mut() = None);
            logger.event("hooks_removed", json!({"session":session,"keyboard_hook":false,"mouse_hook":false}));
        }).map_err(|e| e.to_string())?;
        match receive_ready.recv().map_err(|e| e.to_string())? {
            Ok(id) => Ok(Self {
                id,
                thread: Some(thread),
                relay: Some(relay),
                commands: command_sender,
                replay,
                dropped,
                switched,
                monitor,
                rechecked_releases,
            }),
            Err(error) => {
                let _ = thread.join();
                let _ = relay.join();
                Err(error)
            }
        }
    }

    // 切走时前台到了哪个窗口；0 表示没切走。
    pub fn switched_to(&self) -> usize {
        self.switched.load(Ordering::Acquire)
    }

    pub fn set_hotkey(&self, shortcut: Option<Shortcut>) -> Result<()> {
        let (reply, receiver) = mpsc::sync_channel(1);
        self.command(Command::Hotkey { shortcut, reply })?;
        receiver
            .recv_timeout(REPLAY_WAIT)
            .map_err(|e| e.to_string())
    }

    pub fn activation(&self, ready: bool) -> Result<()> {
        let (reply, receiver) = mpsc::sync_channel(1);
        self.command(Command::Activation { ready, reply })?;
        receiver
            .recv_timeout(REPLAY_WAIT)
            .map_err(|e| e.to_string())
    }
    pub fn stop_inputs(&self, abandon: bool) -> Result<bool> {
        let (reply, receiver) = mpsc::sync_channel(1);
        let result = self
            .command(Command::StopInputs { abandon, reply })
            .and_then(|()| {
                receiver
                    .recv_timeout(REPLAY_WAIT)
                    .map_err(|e| e.to_string())?
            });
        if result.is_err() {
            self.monitor.stopped();
        }
        result
    }
    pub fn input_release_evidence(&self) -> &'static str {
        if self.rechecked_releases.load(Ordering::Acquire) > 0 {
            "active_panel_physical_recheck"
        } else {
            "no_pending_or_observed_up"
        }
    }
    pub fn synchronize(&self) -> bool {
        let (reply, receiver) = mpsc::sync_channel(1);
        let ok = self.command(Command::Fence { reply }).is_ok()
            && receiver.recv_timeout(REPLAY_WAIT).is_ok();
        if !ok {
            self.monitor.stopped();
        }
        ok
    }

    fn command(&self, command: Command) -> Result<()> {
        self.commands.send(command).map_err(|e| e.to_string())?;
        unsafe {
            PostThreadMessageW(self.id, HOOK_COMMAND, WPARAM(0), LPARAM(0))
                .map_err(|e| e.to_string())
        }
    }

    pub fn begin_search(&self, started: Instant) -> Result<Instant> {
        let (reply, receiver) = mpsc::sync_channel(1);
        self.command(Command::Begin { started, reply })?;
        receiver
            .recv_timeout(REPLAY_WAIT)
            .map_err(|e| e.to_string())
    }

    // false：有被吞的键还没松开，保持钩子，松键后通过原事件通道重试收起。
    pub fn finish_hint(&self) -> Result<bool> {
        let (reply, receiver) = mpsc::sync_channel(1);
        self.command(Command::FinishHint { reply })?;
        receiver
            .recv_timeout(REPLAY_WAIT)
            .map_err(|e| e.to_string())?
    }

    pub fn remove_keyboard(&self) -> Result<ReplayBatch> {
        let (reply, receiver) = mpsc::sync_channel(1);
        self.command(Command::Stop { reply })?;
        let started = receiver
            .recv_timeout(REPLAY_WAIT)
            .map_err(|e| e.to_string())??;
        Ok(ReplayBatch {
            keys: self.replay.try_iter().collect(),
            dropped: self.dropped.swap(0, Ordering::Relaxed),
            started,
        })
    }
}
impl Drop for Hooks {
    fn drop(&mut self) {
        unsafe {
            let _ = PostThreadMessageW(self.id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if let Some(relay) = self.relay.take() {
            let _ = relay.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_context(sender: mpsc::SyncSender<HookEvent>, monitor: Arc<Monitor>) -> Context {
        let (replay, _) = mpsc::sync_channel(1);
        Context {
            sender,
            window: 0,
            target: 0,
            target_focus: 0,
            switched: Arc::new(AtomicUsize::new(0)),
            monitor,
            restricted: None,
            draining: false,
            drain_passed: [false; 256],
            drain_snapshot_counts: key_state::Counts::default(),
            drain_ignored_callbacks: key_state::Counts::default(),
            drain_requires_recheck: false,
            drain_recheck_available: false,
            drain_recheck_samples: 0,
            drain_rechecked_releases: 0,
            drain_rechecked_presses: 0,
            bounds: RECT::default(),
            hotkey: Some((V, Modifiers::CONTROL)),
            replay,
            dropped: Arc::new(AtomicUsize::new(0)),
            pending: None,
            swallowed: [false; 256],
            release_action: [None; 256],
            hint: None,
            hint_state: HintState::new(),
            held_modifiers: 0,
            reported_modifiers: 0,
        }
    }

    #[test]
    fn modifier_hold_reports_only_state_changes_and_ignores_keys_held_before_install() {
        let (sender, events) = mpsc::sync_channel(16);
        let mut context = test_context(sender, Arc::new(Monitor::new(0, 0)));
        let drain = || {
            let mut names = Vec::new();
            while let Ok(HookEvent::Key(name)) = events.try_recv() {
                names.push(name);
            }
            names
        };
        // 呼出时就按着的 Ctrl：钩子没看到按下，松开不报。
        context.report_modifier(0xA2, false);
        assert!(drain().is_empty());
        context.report_modifier(0xA2, true);
        context.report_modifier(0xA2, true);
        context.report_modifier(0xA0, true);
        context.report_modifier(0xA2, false);
        context.report_modifier(0xA0, false);
        assert_eq!(drain(), ["Mod:Ctrl", "Mod:Ctrl+Shift", "Mod:Shift", "Mod:"]);
        // 左右 Ctrl 都按着时松开一个仍算按着；Alt 加入时不算“只按 Ctrl”。
        context.report_modifier(0xA2, true);
        context.report_modifier(0xA3, true);
        context.report_modifier(0xA2, false);
        context.report_modifier(0xA4, true);
        context.report_modifier(0xA4, false);
        context.report_modifier(0xA3, false);
        assert_eq!(drain(), ["Mod:Ctrl", "Mod:", "Mod:Ctrl", "Mod:"]);
    }

    #[test]
    fn unavailable_or_replaced_hotkey_keeps_navigation_and_existing_key_pairs() {
        let (sender, _) = mpsc::sync_channel(1);
        let mut context = test_context(sender, Arc::new(Monitor::new(0, 0)));
        context.swallowed[V as usize] = true;
        context.release_action[V as usize] = Some("Space");
        context.set_hotkey(Some("Alt+V".parse().unwrap()));
        assert!(app_shortcut(V, Modifiers::ALT, context.hotkey));
        assert!(!app_shortcut(V, Modifiers::CONTROL, context.hotkey));
        context.set_hotkey(None);
        assert_eq!(context.hotkey, None);
        assert!(!app_shortcut(V, Modifiers::ALT, context.hotkey));
        let exit = Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT;
        assert!(app_shortcut(0x51, exit, context.hotkey));
        assert!(context.swallowed[V as usize]);
        assert_eq!(context.release_action[V as usize], Some("Space"));
        assert_eq!(activation_route(None, false, false, false), None);
    }

    #[test]
    fn unexpected_callback_revokes_instead_of_confirming_the_armed_return() {
        use crate::paste::{
            permit::{Selection, Session},
            request::Mode,
        };
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                CONTEXT.with(|context| *context.borrow_mut() = None);
            }
        }
        for (kind, object, child) in [
            (EVENT_OBJECT_FOCUS, 0, 0),
            (EVENT_SYSTEM_FOREGROUND, -4, 0),
            (EVENT_SYSTEM_FOREGROUND, 0, 1),
        ] {
            let monitor = Arc::new(Monitor::new(10, 20));
            monitor.started();
            let target = crate::platform::Target {
                window: 20,
                focus: 21,
                thread: 2,
                process: 3,
                focus_class: "Edit".into(),
                ..Default::default()
            };
            let mut session = Session::new(7, target.clone(), monitor.clone());
            let permit = session
                .select(
                    7,
                    9,
                    Mode::Paste,
                    Selection {
                        revision: 0,
                        nonce: 1,
                    },
                    10,
                )
                .unwrap();
            assert!(permit.arm_return(10, 100));
            let (sender, events) = mpsc::sync_channel(1);
            let mut context = test_context(sender, monitor.clone());
            context.window = 10;
            context.target = 20;
            CONTEXT.with(|slot| {
                assert!(slot.borrow().is_none());
                *slot.borrow_mut() = Some(context);
            });
            let _reset = Reset;
            // 非预期分支不查询桌面；大时间戳也不能污染前台事件的排序。
            unsafe {
                foreground_changed(
                    HWINEVENTHOOK::default(),
                    kind,
                    super::super::window::hwnd(20),
                    object,
                    child,
                    2,
                    u32::MAX,
                );
            }
            assert!(matches!(
                events.try_recv(),
                Ok(HookEvent::ForegroundRevision(1))
            ));
            assert_eq!(monitor.revision(), 1);
            monitor.observe(20, 101, 20);
            assert!(!permit.can_send(7, 9, Mode::Paste, &target, 20));
            CONTEXT.with(|slot| {
                assert_eq!(
                    slot.borrow()
                        .as_ref()
                        .unwrap()
                        .switched
                        .load(Ordering::Acquire),
                    0
                )
            });
        }
    }

    #[test]
    fn current_thread_input_desktop_query_is_available_readonly() {
        let receiving_input = thread_input_desktop_receiving_input()
            .expect("只读 GetThreadDesktop/UOI_IO 查询必须可用");
        eprintln!("input_desktop_query available=true receiving_input={receiving_input}");
    }

    #[test]
    fn ime_synthetic_and_reserved_states_cannot_hold_search_drain_open() {
        let swallowed = [false; 256];
        let passed = drain_snapshot(&swallowed, |vk| matches!(vk, 0xE5 | 0xE7 | 0xD8));
        assert!(
            inputs_released(&swallowed, &passed),
            "PROCESSKEY、PACKET 与保留虚拟状态没有物理抬起配对保证，不能阻止搜索选择完成"
        );
        let passed = drain_snapshot(&swallowed, |vk| matches!(vk, 0xE5 | 0x0D));
        assert!(
            !inputs_released(&swallowed, &passed),
            "原生 Enter 仍须等抬起"
        );
    }

    #[test]
    fn activated_panel_new_keys_are_owned_by_dom_even_with_foreground() {
        assert_eq!(
            activation_route(Some(true), true, false, false),
            Some(false),
            "面板已激活时，新按键必须进入 DOM，不能再依赖管理员场景未送达的低层钩子动作"
        );
        assert_eq!(
            activation_route(Some(true), true, false, true),
            Some(true),
            "历史吞键仍需保持配对"
        );
        assert_eq!(
            activation_route(None, true, false, false),
            None,
            "普通快速模式继续走原钩子路由"
        );
    }

    #[test]
    fn activation_leaves_new_keys_to_dom_but_preserves_existing_key_pairs() {
        for ready in [false, true] {
            for focused in [false, true] {
                assert_eq!(
                    activation_route(Some(ready), focused, false, false),
                    Some(false)
                );
                assert_eq!(
                    activation_route(Some(ready), focused, false, true),
                    Some(true)
                );
                assert_eq!(
                    activation_route(Some(ready), focused, true, false),
                    Some(false)
                );
            }
        }
        assert_eq!(
            activation_route(None, false, false, false),
            None,
            "普通 quick 保留原路由"
        );
    }

    #[test]
    fn drain_diagnostics_are_bounded_across_thousands_of_poll_cycles() {
        let started = Instant::now();
        let mut trace = DrainTrace::default();
        let mut stages = Vec::new();
        for cycle in 0..5000 {
            stages.extend(trace.observe(
                started + Duration::from_millis(cycle * 5),
                cycle >= 1000,
                cycle >= 4000,
            ));
        }
        assert_eq!(stages, ["started", "waiting", "abandoned", "finished"]);
    }

    #[test]
    fn native_drain_needs_readable_context_and_cannot_poll_away_swallowed_keys() {
        let mut swallowed = [false; 256];
        let passed = [false; 256];
        assert!(!drain_ready(&swallowed, &passed, true, false, false));
        assert!(drain_ready(&swallowed, &passed, true, true, false));
        swallowed[0x41] = true;
        assert!(!drain_ready(&swallowed, &passed, true, true, false));
        assert!(!drain_ready(&swallowed, &passed, true, true, true));
    }

    #[test]
    fn drain_waits_for_native_enter_and_every_other_consumed_or_passed_key() {
        let mut swallowed = [false; 256];
        swallowed[0x41] = true;
        let mut passed = drain_snapshot(&swallowed, |vk| {
            matches!(vk, 0x01 | 0x10 | 0x11 | 0x0D | 0xA2)
        });
        assert!(
            !passed[0x01] && !passed[0x10] && !passed[0x11],
            "鼠标与通用修饰键别名不进入键盘配对表"
        );
        assert!(passed[0x0D] && passed[0xA2]);
        assert!(!inputs_released(&swallowed, &passed));
        passed[0x0D] = false;
        assert!(!inputs_released(&swallowed, &passed));
        passed[normalized_vk(0x11, 0, false) as usize] = false;
        assert!(
            !inputs_released(&swallowed, &passed),
            "选择键松开仍需等早先吞掉的 A"
        );
        swallowed[0x41] = false;
        passed[0x42] = true;
        assert!(
            !inputs_released(&swallowed, &passed),
            "drain 后来的放行键也要等抬起"
        );
        passed[0x42] = false;
        assert!(inputs_released(&swallowed, &passed));
        assert_eq!(normalized_vk(0x10, 0x36, false), 0xA1);
        assert_eq!(normalized_vk(0x12, 0, true), 0xA5);
    }

    const ESC: u16 = 0x1B;
    const V: u16 = 0x56;
    const X: u16 = 0x58;
    const ENTER: u16 = 0x0D;
    const CTRL: u16 = 0xA2;
    const ALT: u16 = 0xA4;
    const SHIFT: u16 = 0xA0;
    const PASS: Route = Route::Pass { end: None };
    const SWALLOW: Route = Route::Swallow { end: None };

    fn key(mode: HintMode, vk: u16, pressed: bool) -> HintKey {
        HintKey {
            mode: Some(mode),
            vk,
            modifier: is_modifier(vk),
            pressed,
            chord: false,
            shortcut: false,
            system: false,
            original: false,
            // 搜索模式下面板通常持有前台；切走的情形单独测。
            focused: mode == HintMode::Search,
        }
    }
    // 按着 Ctrl / Alt / Win 按的组合键。
    fn chord_key(mut key: HintKey) -> HintKey {
        key.chord = true;
        key
    }
    // 呼出键、退出键（都带 Ctrl / Alt 之类，所以 chord 也为真）。
    fn shortcut(mut key: HintKey) -> HintKey {
        key.shortcut = true;
        key.chord = true;
        key
    }
    // Win 组合、Alt+Tab。
    fn system(mut key: HintKey) -> HintKey {
        key.system = true;
        key.chord = true;
        key
    }
    fn original(mut key: HintKey) -> HintKey {
        key.original = true;
        key
    }
    fn unfocused(mut key: HintKey) -> HintKey {
        key.focused = false;
        key
    }
    fn focused(mut key: HintKey) -> HintKey {
        key.focused = true;
        key
    }

    // 表驱动用例：名称、输入、这个键先前的处置、期望的处理方式、期望的处置记录。
    type Case = (&'static str, HintKey, Option<Fate>, Route, Option<Fate>);

    // 快速模式（审查后的规则）：组合键放行并收起，普通键吞掉并收起，Esc / 呼出键 / 退出键 / 修饰键维持原处理。
    #[test]
    fn quick_mode_rules_cover_every_cell() {
        use HintMode::Quick;
        let passthrough = Route::Pass {
            end: Some(HintEnd::KeyPassthrough),
        };
        let consumed = Fate::Swallow(HintEnd::KeyConsumed);
        let escape = Fate::Swallow(HintEnd::Escape);
        let pass = Fate::Pass;
        let release_consumed = Route::Swallow {
            end: Some(HintEnd::KeyConsumed),
        };
        #[rustfmt::skip]
        let cases: Vec<Case> = vec![
            // 1）按着 Ctrl / Alt / Win 的非修饰键：按下放行给目标并请求收起，抬起也放行。
            ("Ctrl+V 按下", chord_key(key(Quick, V, true)), None, passthrough, Some(pass)),
            ("Ctrl+V 抬起", key(Quick, V, false), Some(pass), PASS, None),
            ("Ctrl+Z", chord_key(key(Quick, 0x5A, true)), None, passthrough, Some(pass)),
            ("Ctrl+Enter", chord_key(key(Quick, ENTER, true)), None, passthrough, Some(pass)),
            ("Alt+Tab", system(key(Quick, 0x09, true)), None, passthrough, Some(pass)),
            ("Win+D", system(key(Quick, 0x44, true)), None, passthrough, Some(pass)),
            ("Ctrl+Esc 按组合键处理", chord_key(key(Quick, ESC, true)), None, passthrough, Some(pass)),
            // 2）没按着 Ctrl / Alt / Win 的普通键：吞掉，抬起也吞并收起。
            ("数字 2 按下", key(Quick, 0x32, true), None, SWALLOW, Some(consumed)),
            ("数字 2 抬起", key(Quick, 0x32, false), Some(consumed), release_consumed, None),
            ("小键盘 2", key(Quick, 0x62, true), None, SWALLOW, Some(consumed)),
            ("字母 V", key(Quick, V, true), None, SWALLOW, Some(consumed)),
            ("Enter", key(Quick, ENTER, true), None, SWALLOW, Some(consumed)),
            ("方向键", key(Quick, 0x28, true), None, SWALLOW, Some(consumed)),
            ("空格", key(Quick, 0x20, true), None, SWALLOW, Some(consumed)),
            ("Shift+字母（Shift 不算组合）", key(Quick, 0x41, true), None, SWALLOW, Some(consumed)),
            // 3）Esc、呼出键、退出键维持原处理。
            ("Esc 按下", key(Quick, ESC, true), None, SWALLOW, Some(escape)),
            ("Esc 抬起", key(Quick, ESC, false), Some(escape), Route::Swallow { end: Some(HintEnd::Escape) }, None),
            ("呼出键按下", shortcut(key(Quick, V, true)), None, Route::Original, Some(pass)),
            ("呼出键抬起", shortcut(key(Quick, V, false)), None, Route::Original, None),
            ("退出键", shortcut(key(Quick, 0x51, true)), None, Route::Original, Some(pass)),
            // 按住不放的重复按下：照第一次的处置，不再请求收起；中途按下或松开 Ctrl 也不改。
            ("Ctrl+V 重复按下", chord_key(key(Quick, V, true)), Some(pass), PASS, Some(pass)),
            ("松开 Ctrl 后 V 重复按下", key(Quick, V, true), Some(pass), PASS, Some(pass)),
            ("普通键按住后再按 Ctrl", chord_key(key(Quick, V, true)), Some(consumed), SWALLOW, Some(consumed)),
            ("Esc 重复按下", key(Quick, ESC, true), Some(escape), SWALLOW, Some(escape)),
            // 停留开始前按下、原逻辑已吞掉的键：重复按下和抬起都交回原逻辑配对。
            ("原逻辑的键按下", original(key(Quick, V, true)), None, Route::Original, None),
            ("原逻辑的组合键按下", original(chord_key(key(Quick, V, true))), None, Route::Original, None),
            ("停留前按下的键抬起", key(Quick, V, false), None, Route::Original, None),
            // 快速模式只看模式，focused 不影响。
            ("focused 为真的普通键", focused(key(Quick, V, true)), None, SWALLOW, Some(consumed)),
            ("focused 为真的组合键", focused(chord_key(key(Quick, V, true))), None, passthrough, Some(pass)),
        ];
        for (name, input, earlier, route, fate) in cases {
            assert_eq!(hint_route(input, earlier), (route, fate), "{name}");
        }
    }

    #[test]
    fn passed_system_key_keeps_pairing_after_modifier_release() {
        let mut state = HintState::new();
        assert_eq!(
            state.handle(system(key(HintMode::Search, 0x44, true))),
            (Some(false), None)
        );
        // Win 已松开，D 仍自动重复：最初的按下放行，后续也必须放行。
        assert_eq!(
            state.handle(key(HintMode::Search, 0x44, true)),
            (Some(false), None)
        );
        assert_eq!(
            state.handle(key(HintMode::Search, 0x44, false)),
            (Some(false), None)
        );
    }

    #[test]
    fn passed_app_shortcut_keeps_pairing_after_modifier_release() {
        for mode in [HintMode::Quick, HintMode::Search] {
            let mut state = HintState::new();
            let first = state.handle(shortcut(key(mode, V, true)));
            assert_eq!(
                first,
                (
                    if mode == HintMode::Quick {
                        None
                    } else {
                        Some(false)
                    },
                    None
                )
            );
            assert_eq!(state.handle(key(mode, V, true)), (Some(false), None));
            assert_eq!(state.handle(key(mode, V, false)), (Some(false), None));
        }
    }

    #[test]
    fn preexisting_passed_key_keeps_repeats_and_release_in_both_modes() {
        for mode in [HintMode::Quick, HintMode::Search] {
            let mut state = HintState::with_passed([ENTER, CTRL]);
            assert_eq!(state.handle(key(mode, ENTER, true)), (Some(false), None));
            assert_eq!(state.handle(key(mode, ENTER, false)), (Some(false), None));
            // 松开以后新按的 Enter 才属于停留规则。
            assert_eq!(state.handle(key(mode, ENTER, true)), (Some(true), None));
            assert!(!state.request_close(false));
        }
    }

    #[test]
    fn close_waits_for_every_swallowed_release_in_both_modes() {
        for mode in [HintMode::Quick, HintMode::Search] {
            let mut state = HintState::new();
            assert_eq!(state.handle(key(mode, V, true)), (Some(true), None));
            assert_eq!(state.handle(key(mode, X, true)), (Some(true), None));
            // 模拟超时、外部点击或呼出键请求关闭；原配对状态不能被清掉。
            assert!(!state.request_close(false));
            assert!(!state.close_ready(false));
            assert_eq!(state.handle(key(mode, V, false)).0, Some(true));
            assert!(!state.close_ready(false));
            assert!(!state.request_close(false));
            assert_eq!(state.handle(key(mode, X, false)).0, Some(true));
            assert!(state.close_ready(false));
            assert!(state.request_close(false));
        }
    }

    #[test]
    fn queued_hide_rechecks_keys_arriving_after_the_close_event() {
        for mode in [HintMode::Quick, HintMode::Search] {
            let mut state = HintState::new();
            state.handle(key(mode, V, true));
            let released = state.handle(key(mode, V, false));
            assert!(released.1.is_some(), "松键已投递关闭事件");
            // 主线程尚未处理关闭事件，钩子又吞了 X 的按下。
            state.handle(key(mode, X, true));
            assert!(!state.request_close(false), "实际卸钩前必须再检查");
            assert_eq!(state.handle(key(mode, X, false)).0, Some(true));
            assert!(state.request_close(false));
        }
    }

    #[test]
    fn close_also_waits_for_original_keys_but_not_passed_chords() {
        let mut state = HintState::new();
        assert!(!state.request_close(true));
        assert!(!state.close_ready(true));
        assert!(state.close_ready(false));
        assert!(state.request_close(false));
        let mut state = HintState::new();
        assert_eq!(
            state.handle(chord_key(key(HintMode::Quick, V, true))).0,
            Some(false)
        );
        assert!(state.request_close(false), "放行的 Ctrl+V 不应延后收起");
    }

    #[test]
    fn original_keys_and_full_event_queue_keep_close_pending() {
        for mode in [HintMode::Quick, HintMode::Search] {
            let (sender, events) = mpsc::sync_channel(1);
            let mut context = test_context(sender, Arc::new(Monitor::new(0, 0)));
            context.pending = Some(Instant::now());
            context.hint = Some(mode);
            context.swallowed[V as usize] = true;
            // 真正调用停留入口：原键的重复直接吞掉，不再读取当前修饰键重新分类。
            assert_eq!(context.hint_key(V, true, mode), Some(true));
            assert_eq!(context.hint_key(V, false, mode), None);
            assert!(
                !context
                    .hint_state
                    .request_close(context.original_hint_keys_held())
            );
            context.notify_hint_close();
            assert!(events.try_recv().is_err(), "原路径尚未处理抬起，不能收起");

            // 模拟原路径消费抬起，再模拟事件队列暂满；不能丢掉延后关闭请求。
            context.swallowed[V as usize] = false;
            assert!(context.sender.try_send(HookEvent::Key("Escape")).is_ok());
            context.notify_hint_close();
            assert!(context.hint_state.closing);
            assert!(matches!(events.try_recv(), Ok(HookEvent::Key("Escape"))));
            context.notify_hint_close();
            assert!(matches!(
                events.try_recv(),
                Ok(HookEvent::HintKey(HintEnd::KeyConsumed))
            ));
            assert!(!context.hint_state.closing);
            context.notify_hint_close();
            assert!(events.try_recv().is_err(), "成功入队后不重复通知");

            // 搜索首键已经补发后，原数组里的旧记录不能阻止正常收起。
            context.pending = None;
            context.swallowed[V as usize] = true;
            assert_eq!(context.original_hint_keys_held(), mode == HintMode::Quick);
        }
    }

    // 搜索模式：按下时的放行决定也要留到抬起。
    #[test]
    fn search_mode_rules_cover_every_cell() {
        use HintMode::Search;
        let passthrough = Route::Pass {
            end: Some(HintEnd::KeyPassthrough),
        };
        let consumed = Fate::Swallow(HintEnd::KeyConsumed);
        let escape = Fate::Swallow(HintEnd::Escape);
        let pass = Fate::Pass;
        #[rustfmt::skip]
        let cases: Vec<Case> = vec![
            ("Esc 按下", key(Search, ESC, true), None, SWALLOW, Some(escape)),
            ("Esc 抬起", key(Search, ESC, false), Some(escape), Route::Swallow { end: Some(HintEnd::Escape) }, None),
            ("呼出键按下", shortcut(key(Search, V, true)), None, PASS, Some(pass)),
            ("呼出键抬起", shortcut(key(Search, V, false)), None, PASS, None),
            ("Alt+Tab", system(key(Search, 0x09, true)), None, PASS, Some(pass)),
            ("Win+D", system(key(Search, 0x44, true)), None, PASS, Some(pass)),
            // 其他非修饰键（带不带 Ctrl 都一样）：吞掉，抬起也吞并收起。
            ("V 按下", key(Search, V, true), None, SWALLOW, Some(consumed)),
            ("V 抬起", key(Search, V, false), Some(consumed), Route::Swallow { end: Some(HintEnd::KeyConsumed) }, None),
            ("Ctrl+V", chord_key(key(Search, V, true)), None, SWALLOW, Some(consumed)),
            ("数字 1", key(Search, 0x31, true), None, SWALLOW, Some(consumed)),
            ("Enter", key(Search, ENTER, true), None, SWALLOW, Some(consumed)),
            ("V 重复按下", key(Search, V, true), Some(consumed), SWALLOW, Some(consumed)),
            // 原逻辑的吞键记录是补发前留下的，不看；停留前按下的键那时没有钩子，抬起放行。
            ("过时的原逻辑记录", original(key(Search, V, true)), None, SWALLOW, Some(consumed)),
            ("停留前按下的键抬起", key(Search, V, false), None, PASS, None),
            // 停留期间面板被切走（Alt+Tab 之类）：放行给当前前台并收起，不吞别的程序的键；Esc 仍吞掉。
            ("切走后 V 按下", unfocused(key(Search, V, true)), None, passthrough, Some(pass)),
            ("切走后 V 抬起", unfocused(key(Search, V, false)), Some(pass), PASS, None),
            ("切走后 Esc 按下", unfocused(key(Search, ESC, true)), None, SWALLOW, Some(escape)),
            ("切走后 呼出键", shortcut(unfocused(key(Search, V, true))), None, PASS, Some(pass)),
            ("切走后 停留前按下的键抬起", unfocused(key(Search, V, false)), None, PASS, None),
            ("吞掉后切走再抬起", unfocused(key(Search, V, false)), Some(consumed), Route::Swallow { end: Some(HintEnd::KeyConsumed) }, None),
        ];
        for (name, input, earlier, route, fate) in cases {
            assert_eq!(hint_route(input, earlier), (route, fate), "{name}");
        }
    }

    #[test]
    fn modifiers_alone_pass_without_hiding() {
        for mode in [HintMode::Quick, HintMode::Search] {
            for vk in [
                0x10, 0x11, 0x12, 0x5B, 0x5C, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5,
            ] {
                for pressed in [true, false] {
                    for chord in [false, true] {
                        let mut input = key(mode, vk, pressed);
                        input.chord = chord;
                        assert_eq!(
                            hint_route(input, None),
                            (PASS, None),
                            "{mode:?} {vk:#x} {pressed} {chord}"
                        );
                    }
                }
            }
        }
    }

    // 不在停留状态（mode 为 None）时，不管什么键、什么记录，一律交回原逻辑，记录原样保留，也不请求收起。
    #[test]
    fn outside_the_dwell_everything_goes_to_the_original_logic() {
        let fates = [
            None,
            Some(Fate::Pass),
            Some(Fate::Swallow(HintEnd::Escape)),
            Some(Fate::Swallow(HintEnd::KeyConsumed)),
        ];
        for vk in 0..=255u16 {
            for flags in 0..32u8 {
                for pressed in [true, false] {
                    let input = HintKey {
                        mode: None,
                        vk,
                        modifier: is_modifier(vk),
                        pressed,
                        chord: flags & 1 != 0,
                        shortcut: flags & 2 != 0,
                        system: flags & 4 != 0,
                        original: flags & 8 != 0,
                        focused: flags & 16 != 0,
                    };
                    for earlier in fates {
                        assert_eq!(hint_route(input, earlier), (Route::Original, earlier));
                    }
                    let mut state = HintState::new();
                    state.end = Some(HintEnd::KeyConsumed);
                    assert_eq!(state.handle(input), (None, None));
                }
            }
        }
    }

    // keyboard() 原逻辑里放行组合的原样副本，用来钉住 app_shortcut / system_combo 合起来和它一致。
    fn original_passthrough(vk: u16, mods: Modifiers, hotkey: (u16, Modifiers)) -> bool {
        let exit = matches!(vk, 0x51 | 0x4c)
            && mods == (Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT);
        mods.contains(Modifiers::SUPER)
            || (vk == 0x09 && mods.contains(Modifiers::ALT))
            || (vk == hotkey.0 && mods == hotkey.1)
            || exit
    }
    // Shift / Ctrl / Alt / Win 的 16 种组合。
    fn all_mods() -> Vec<Modifiers> {
        let each = [
            Modifiers::SHIFT,
            Modifiers::CONTROL,
            Modifiers::ALT,
            Modifiers::SUPER,
        ];
        (0..16usize)
            .map(|bits| {
                each.iter()
                    .enumerate()
                    .filter(|(index, _)| bits & (1 << index) != 0)
                    .fold(Modifiers::empty(), |mods, (_, &one)| mods | one)
            })
            .collect()
    }

    #[test]
    fn shortcut_and_system_together_match_the_original_passthrough() {
        let hotkeys = [
            (V, Modifiers::ALT),
            (0x7B, Modifiers::CONTROL | Modifiers::SHIFT),
            (0x78, Modifiers::empty()),
        ];
        for hotkey in hotkeys {
            for vk in 0..=255u16 {
                for mods in all_mods() {
                    assert_eq!(
                        app_shortcut(vk, mods, Some(hotkey)) || system_combo(vk, mods),
                        original_passthrough(vk, mods, hotkey),
                        "{vk:#x} {mods:?} {hotkey:?}"
                    );
                }
            }
        }
        let alt_v = Some((V, Modifiers::ALT));
        let exit = Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT;
        assert!(app_shortcut(V, Modifiers::ALT, alt_v), "呼出键");
        assert!(
            !app_shortcut(V, Modifiers::CONTROL, alt_v),
            "Ctrl+V 不是呼出键"
        );
        assert!(app_shortcut(0x51, exit, alt_v), "退出键");
        assert!(app_shortcut(0x4c, exit, alt_v), "采集调试键");
        assert!(!app_shortcut(0x51, Modifiers::CONTROL, alt_v));
        assert!(system_combo(0x44, Modifiers::SUPER), "Win+D");
        assert!(system_combo(0x09, Modifiers::ALT), "Alt+Tab");
        assert!(!system_combo(0x09, Modifiers::empty()), "Tab");
        assert!(!system_combo(V, Modifiers::CONTROL), "Ctrl+V");
    }

    #[test]
    fn chord_means_ctrl_alt_or_win() {
        for mods in all_mods() {
            let expected = mods.contains(Modifiers::CONTROL)
                || mods.contains(Modifiers::ALT)
                || mods.contains(Modifiers::SUPER);
            assert_eq!(chord(mods), expected, "{mods:?}");
        }
        assert!(!chord(Modifiers::SHIFT), "Shift 不算组合");
        assert!(!chord(Modifiers::empty()));
    }

    #[test]
    fn modifier_keys_are_the_ones_the_original_logic_passes() {
        for vk in 0..=255u16 {
            let expected = matches!(vk, 0x10..=0x12 | 0x5B | 0x5C | 0xA0..=0xA5);
            assert_eq!(is_modifier(vk), expected, "{vk:#x}");
        }
    }

    #[test]
    fn hide_waits_for_the_last_swallowed_key() {
        let consumed = Some(HintEnd::KeyConsumed);
        let passthrough = Some(HintEnd::KeyPassthrough);
        // 没有别的吞掉的键按着：立即发。
        assert_eq!(hint_end(consumed, None, false), (consumed, None));
        // 还有吞掉的键按着：先记着。
        assert_eq!(hint_end(consumed, None, true), (None, consumed));
        // 最后一个抬起时发出，先请求的原因优先。
        assert_eq!(hint_end(consumed, passthrough, false), (passthrough, None));
        assert_eq!(hint_end(None, passthrough, true), (None, passthrough));
        assert_eq!(hint_end(None, None, false), (None, None));
    }

    // 一个按键事件经停留规则后的结果：Some(是否吞掉)，None 表示交回原逻辑；以及现在请求收起的原因。
    type Outcome = (Option<bool>, Option<HintEnd>);

    // Ctrl / Alt / Win 键（左右都算）。
    fn chord_vk(vk: u16) -> bool {
        matches!(vk, 0x11 | 0x12 | 0x5B | 0x5C | 0xA2..=0xA5)
    }

    // 用 Context::hint_key 同一份状态跑一串事件。chord 按序列里此刻按着的 Ctrl / Alt / Win 算——和钩子里
    // modifiers() 读到的一样是这个事件之前的状态；面板是否在前台逐个事件给出。
    fn run_focus(mode: HintMode, events: &[(u16, bool, bool)]) -> Vec<Outcome> {
        let mut state = HintState::new();
        let mut held = [false; 256];
        events
            .iter()
            .map(|&(vk, pressed, focused)| {
                let mut input = key(mode, vk, pressed);
                input.chord = (0..=255u16).any(|other| held[other as usize] && chord_vk(other));
                input.focused = focused;
                if is_modifier(vk) {
                    held[vk as usize] = pressed;
                }
                state.handle(input)
            })
            .collect()
    }
    // 同上，面板是否在前台取模式的常态。
    fn run(mode: HintMode, events: &[(u16, bool)]) -> Vec<Outcome> {
        let focused = mode == HintMode::Search;
        let events: Vec<_> = events
            .iter()
            .map(|&(vk, pressed)| (vk, pressed, focused))
            .collect();
        run_focus(mode, &events)
    }

    const PASSED: Outcome = (Some(false), None);
    const SWALLOWED: Outcome = (Some(true), None);

    #[test]
    fn quick_mode_passes_chords_and_consumes_plain_keys() {
        let quick = HintMode::Quick;
        let passthrough = (Some(false), Some(HintEnd::KeyPassthrough));
        let consumed = (Some(true), Some(HintEnd::KeyConsumed));
        // Ctrl+V：Ctrl 放行；V 按下放行给目标并立即请求收起，抬起也放行。
        let ctrl_v = [(CTRL, true), (V, true), (V, false), (CTRL, false)];
        assert_eq!(run(quick, &ctrl_v), [PASSED, passthrough, PASSED, PASSED]);
        // 先松开 Ctrl 再松开 V：V 的抬起照按下的处置放行。
        let ctrl_first = [(CTRL, true), (V, true), (CTRL, false), (V, false)];
        assert_eq!(
            run(quick, &ctrl_first),
            [PASSED, passthrough, PASSED, PASSED]
        );
        // Alt+Tab 同样放行并收起。
        let alt_tab = [(ALT, true), (0x09, true), (0x09, false), (ALT, false)];
        assert_eq!(run(quick, &alt_tab), [PASSED, passthrough, PASSED, PASSED]);
        // 想换一条按 2：吞掉，抬起也吞，抬起时收起，“2”不会进目标。
        assert_eq!(
            run(quick, &[(0x32, true), (0x32, false)]),
            [SWALLOWED, consumed]
        );
        // Shift+字母照普通键吞掉。
        let shift_a = [(SHIFT, true), (0x41, true), (0x41, false), (SHIFT, false)];
        assert_eq!(run(quick, &shift_a), [PASSED, SWALLOWED, consumed, PASSED]);
        // 先按 V（吞掉）再按 Ctrl：V 的重复按下和抬起照第一次吞掉。
        let late_ctrl = [
            (V, true),
            (CTRL, true),
            (V, true),
            (V, false),
            (CTRL, false),
        ];
        assert_eq!(
            run(quick, &late_ctrl),
            [SWALLOWED, PASSED, SWALLOWED, consumed, PASSED]
        );
    }

    #[test]
    fn search_mode_consumes_ctrl_v() {
        let ctrl_v = [(CTRL, true), (V, true), (V, false), (CTRL, false)];
        assert_eq!(
            run(HintMode::Search, &ctrl_v),
            [
                PASSED,
                SWALLOWED,
                (Some(true), Some(HintEnd::KeyConsumed)),
                PASSED
            ]
        );
    }

    #[test]
    fn held_keys_keep_their_first_decision() {
        let held = [(V, true), (V, true), (V, true), (V, false)];
        let consumed = (Some(true), Some(HintEnd::KeyConsumed));
        for mode in [HintMode::Quick, HintMode::Search] {
            assert_eq!(
                run(mode, &held),
                [SWALLOWED, SWALLOWED, SWALLOWED, consumed],
                "{mode:?}"
            );
        }
        let ctrl_v_held = [
            (CTRL, true),
            (V, true),
            (V, true),
            (V, true),
            (V, false),
            (CTRL, false),
        ];
        assert_eq!(
            run(HintMode::Quick, &ctrl_v_held),
            [
                PASSED,
                (Some(false), Some(HintEnd::KeyPassthrough)),
                PASSED,
                PASSED,
                PASSED,
                PASSED
            ]
        );
        let escape = [(ESC, true), (ESC, true), (ESC, false)];
        for mode in [HintMode::Quick, HintMode::Search] {
            assert_eq!(
                run(mode, &escape),
                [SWALLOWED, SWALLOWED, (Some(true), Some(HintEnd::Escape))],
                "{mode:?}"
            );
        }
    }

    #[test]
    fn overlapping_keys_hide_after_the_last_swallowed_release() {
        let consumed = (Some(true), Some(HintEnd::KeyConsumed));
        // 快速连打 V、X（V 还没抬起 X 就按下了）：V 抬起时 X 还被吞着按住，等 X 抬起再收起。两种模式一样。
        let rollover = [(V, true), (X, true), (V, false), (X, false)];
        for mode in [HintMode::Quick, HintMode::Search] {
            assert_eq!(
                run(mode, &rollover),
                [SWALLOWED, SWALLOWED, SWALLOWED, consumed],
                "{mode:?}"
            );
        }
        // 快速模式下按住 Esc 时按 Ctrl+V：V 放行，收起等 Esc 抬起，原因按先到的 key_passthrough 记。
        let escape_held = [
            (ESC, true),
            (CTRL, true),
            (V, true),
            (V, false),
            (CTRL, false),
            (ESC, false),
        ];
        assert_eq!(
            run(HintMode::Quick, &escape_held),
            [
                SWALLOWED,
                PASSED,
                PASSED,
                PASSED,
                PASSED,
                (Some(true), Some(HintEnd::KeyPassthrough))
            ]
        );
    }

    // 按下和抬起成对：停留规则放行了按下，抬起也放行；吞掉了按下，抬起也吞。交叉按键、重复按下、
    // 中途按下 / 松开 Ctrl、Alt、搜索模式下中途切走 / 切回也一样；收起请求只在没有吞掉的键还按着时发出，
    // 按过非修饰键、全部抬起后一定已经发出过。用固定种子的伪随机序列把常见键的各种交错都走一遍。
    #[test]
    fn every_release_follows_its_press() {
        let keys = [V, X, 0x32, ENTER, ESC, 0x28, CTRL, SHIFT, ALT];
        let mut seed = 0x2545_F491_4F6C_DD1D_u64;
        for mode in [HintMode::Quick, HintMode::Search] {
            for _ in 0..300 {
                let mut held = [false; 256];
                let mut focused = mode == HintMode::Search;
                let mut events = Vec::new();
                for _ in 0..24 {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    let vk = keys[(seed % keys.len() as u64) as usize];
                    // 已按住的键随机再按（自动重复）或抬起；没按住的只会按下。
                    let pressed = !held[vk as usize] || (seed >> 32).is_multiple_of(3);
                    held[vk as usize] = pressed;
                    if mode == HintMode::Search && (seed >> 40).is_multiple_of(5) {
                        focused = !focused;
                    }
                    events.push((vk, pressed, focused));
                }
                for vk in keys {
                    if held[vk as usize] {
                        events.push((vk, false, focused));
                    }
                }
                let outcomes = run_focus(mode, &events);
                let mut first: [Option<bool>; 256] = [None; 256];
                let mut requested = false;
                for (&(vk, pressed, _), &(swallow, end)) in events.iter().zip(&outcomes) {
                    // 停留开始后才按下的键都由停留规则处理，不会交回原逻辑。
                    let swallow = swallow.unwrap_or_else(|| panic!("{mode:?} {vk:#x} {events:?}"));
                    if pressed {
                        let decided = *first[vk as usize].get_or_insert(swallow);
                        assert_eq!(swallow, decided, "{mode:?} 重复按下 {vk:#x} {events:?}");
                    } else {
                        let decided = first[vk as usize].take();
                        assert_eq!(Some(swallow), decided, "{mode:?} 抬起 {vk:#x} {events:?}");
                    }
                    if end.is_some() {
                        requested = true;
                        assert!(
                            !first.contains(&Some(true)),
                            "{mode:?} 还有吞掉的键按着就请求收起 {events:?}"
                        );
                    }
                }
                let typed = events.iter().any(|&(vk, _, _)| !is_modifier(vk));
                assert_eq!(requested, typed, "{mode:?} {events:?}");
            }
        }
    }
}
