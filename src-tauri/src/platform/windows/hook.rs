use super::paste::INPUT_MARKER;
use crate::{
    log::Logger,
    platform::{
        HintEnd, HintMode, HookEvent, REPLAY_CAPACITY, REPLAY_WAIT, ReplayBatch, ReplayKey, Result,
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
    time::Instant,
};
use tauri_plugin_global_shortcut::{Modifiers, Shortcut};
use windows::Win32::{
    Foundation::{HINSTANCE, LPARAM, LRESULT, RECT, WPARAM},
    System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
    UI::{
        Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent},
        Input::KeyboardAndMouse::{GetAsyncKeyState, MAPVK_VK_TO_CHAR, MapVirtualKeyW},
        WindowsAndMessaging::*,
    },
};

const HOOK_COMMAND: u32 = WM_APP + 1;
enum Command {
    Begin {
        started: Instant,
        reply: mpsc::SyncSender<Instant>,
    },
    Stop {
        reply: mpsc::SyncSender<Result<Option<Instant>>>,
    },
    // 管理员提示开始停留（M0-6b）：之后的按键先按停留规则处理。搜索模式下键盘钩子已在首键补发时卸掉，
    // 在这个线程里重新挂上（同一个消息循环，不另开线程）；回执为键盘钩子是否在位。
    Hint {
        mode: HintMode,
        reply: mpsc::SyncSender<bool>,
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
    // 原逻辑本来就放行的组合：Win 组合、Alt+Tab、呼出键、退出 / 采集调试键。
    system: bool,
    // 原逻辑吞了这个键的按下、还没见到抬起：按住不放的重复按下和最后的抬起仍归原逻辑。
    original: bool,
}

// 管理员提示停留期间按键怎么处理（M0-6b）。earlier 是停留规则先前对这个键按下的处置；
// 返回处理方式和这个键之后的处置记录。
// 快速模式：Esc 吞掉、抬起时收起；其他非修饰键放行给目标（Ctrl+V 直接粘进管理员窗口）并在按下时就请求收起。
// 搜索模式：Esc 和其他非修饰键都吞掉（不进搜索框）、抬起时收起，焦点由收起按原逻辑还。
// 请求什么时候真发出去另由 hint_end 决定（还有吞掉的键按着就等它们抬起）。
// 两种模式下修饰键和原逻辑本来就放行的组合（含呼出键）都放行、不收起；呼出键照旧经全局快捷键收起。
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
    match mode {
        // 搜索模式下原逻辑的吞键记录是首键补发前留下的，可能过时，不看。
        HintMode::Quick if key.original || key.system => (Route::Original, None),
        HintMode::Search if key.system => (Route::Pass { end: None }, None),
        _ if key.vk == 0x1B => (
            Route::Swallow { end: None },
            Some(Fate::Swallow(HintEnd::Escape)),
        ),
        HintMode::Quick => (
            Route::Pass {
                end: Some(HintEnd::KeyPassthrough),
            },
            Some(Fate::Pass),
        ),
        HintMode::Search => (
            Route::Swallow { end: None },
            Some(Fate::Swallow(HintEnd::KeyConsumed)),
        ),
    }
}

// 和 keyboard() 原逻辑开头放行的修饰键一致。
fn is_modifier(vk: u16) -> bool {
    matches!(vk, 0x10..=0x12 | 0x5B | 0x5C | 0xA0..=0xA5)
}

// 和 keyboard() 原逻辑里放行的组合一致（停留规则单独用，原逻辑那段没动）。
fn system_combo(vk: u16, mods: Modifiers, hotkey: (u16, Modifiers)) -> bool {
    let exit = matches!(vk, 0x51 | 0x4c)
        && mods == (Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT);
    mods.contains(Modifiers::SUPER)
        || (vk == 0x09 && mods.contains(Modifiers::ALT))
        || (vk == hotkey.0 && mods == hotkey.1)
        || exit
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
}
impl HintState {
    fn new() -> Self {
        Self {
            keys: [None; 256],
            end: None,
        }
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
    // 第一个切到前台的外部窗口；0 表示没切走。
    switched: Arc<AtomicUsize>,
    bounds: RECT,
    hotkey: (u16, Modifiers),
    replay: mpsc::SyncSender<ReplayKey>,
    dropped: Arc<AtomicUsize>,
    pending: Option<Instant>,
    swallowed: [bool; 256],
    release_action: [Option<&'static str>; 256],
    // 管理员提示停留中（M0-6b）：Some 时 keyboard() 先走停留规则；None 时完全是原逻辑。
    hint: Option<HintMode>,
    // 停留规则的逐键处置和待发的收起请求，抬起时照此配对。
    hint_state: HintState,
}
impl Context {
    // 停留期间的按键：Some(是否吞掉) 表示停留规则已处理；None 表示交回原逻辑。
    fn hint_key(&mut self, vk: u16, pressed: bool, mode: HintMode) -> Option<bool> {
        if vk > 255 {
            return None;
        }
        let (swallow, end) = self.hint_state.handle(HintKey {
            mode: Some(mode),
            vk,
            modifier: is_modifier(vk),
            pressed,
            system: system_combo(vk, modifiers(), self.hotkey),
            original: self.swallowed[vk as usize],
        });
        if let Some(end) = end {
            let _ = self.sender.try_send(HookEvent::HintKey(end));
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

pub struct Hooks {
    id: u32,
    thread: Option<JoinHandle<()>>,
    relay: Option<JoinHandle<()>>,
    commands: mpsc::Sender<Command>,
    replay: mpsc::Receiver<ReplayKey>,
    dropped: Arc<AtomicUsize>,
    switched: Arc<AtomicUsize>,
}

unsafe extern "system" fn foreground_changed(
    _: HWINEVENTHOOK,
    _: u32,
    hwnd: windows::Win32::Foundation::HWND,
    _: i32,
    _: i32,
    _: u32,
    _: u32,
) {
    CONTEXT.with(|context| {
        if let Some(context) = context.borrow().as_ref()
            && !hwnd.is_invalid()
            && hwnd.0 as usize != context.window
            && hwnd.0 as usize != context.target
        {
            let _ = context.switched.compare_exchange(
                0,
                hwnd.0 as usize,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    });
}

// 覆盖全局快捷键插件可注册的常用键；未知键在注册前回退，避免钩子阻断呼出键。
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
    let vk = key.vkCode as u16;
    let pressed = wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN;
    let swallow = CONTEXT.with(|context| {
        let mut context = context.borrow_mut();
        let Some(context) = context.as_mut() else {
            return false;
        };
        // M0-6b：管理员提示停留中才进这个分支；不在停留时 hint 为 None，下面的原逻辑一行没改。
        if let Some(mode) = context.hint
            && let Some(swallow) = context.hint_key(vk, pressed, mode)
        {
            return swallow;
        }
        if matches!(vk, 0x10..=0x12 | 0x5B | 0x5C | 0xA0..=0xA5) {
            return false;
        }
        if vk > 255 {
            return true;
        }
        if !pressed && context.swallowed[vk as usize] {
            context.swallowed[vk as usize] = false;
            if let Some(action) = context.release_action[vk as usize].take()
                && context.pending.is_none()
            {
                let _ = context.sender.try_send(HookEvent::Key(action));
            }
            return true;
        }
        let mods = modifiers();
        let exit = matches!(vk, 0x51 | 0x4c)
            && mods == (Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT);
        if mods.contains(Modifiers::SUPER)
            || (vk == 0x09 && mods.contains(Modifiers::ALT))
            || (vk == context.hotkey.0 && mods == context.hotkey.1)
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
        context.swallowed[vk as usize] = true;
        if context.pending.is_some() {
            context.buffer(key, mods);
            return true;
        }
        let navigation = if mods.is_empty() {
            match vk {
                0x26 => Some("ArrowUp"),
                0x28 => Some("ArrowDown"),
                0x21 => Some("PageUp"),
                0x22 => Some("PageDown"),
                0x24 => Some("Home"),
                0x23 => Some("End"),
                0x0D => Some("Enter"),
                0x1B => Some("Escape"),
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
        } else {
            None
        };
        if let Some(name) = navigation {
            // 确认与关闭等到抬起再投递，避免卸钩后把 Enter 抬起漏给目标控件。
            if matches!(
                name,
                "Enter"
                    | "Shift+Enter"
                    | "Ctrl+Enter"
                    | "Escape"
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
    if swallow {
        LRESULT(1)
    } else {
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
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
        target: usize,
        shortcut: Shortcut,
        logger: Logger,
        session: u64,
        callback: impl Fn(HookEvent) + Send + 'static,
    ) -> Result<Self> {
        let bounds = super::window::rect(window)?;
        let vk = shortcut_vk(&shortcut).ok_or("不支持此快捷键")?;
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
        let thread = thread::Builder::new().name("panel-hooks".into()).spawn(move || unsafe {
            CONTEXT.with(|context| *context.borrow_mut() = Some(Context {
                sender, window, target, switched:switched_context, bounds, hotkey: (vk, shortcut.mods), replay: replay_sender,
                dropped: overflow, pending: None, swallowed: [false; 256], release_action: [None; 256],
                hint: None, hint_state: HintState::new(),
            }));
            let mut message = MSG::default();
            let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
            let module = GetModuleHandleW(None).ok().map(|m| HINSTANCE(m.0));
            let mut keyboard_hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), module, 0).ok();
            let mouse_hook = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), module, 0).ok();
            let foreground_hook = SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND,
                None, Some(foreground_changed), 0, 0, WINEVENT_OUTOFCONTEXT);
            let installed = keyboard_hook.is_some() && mouse_hook.is_some() && !foreground_hook.is_invalid();
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
                                Command::Hint { mode, reply } => {
                                    let reinstalled = keyboard_hook.is_none();
                                    if reinstalled {
                                        keyboard_hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), module, 0).ok();
                                    }
                                    CONTEXT.with(|context| {
                                        if let Some(context) = context.borrow_mut().as_mut() {
                                            context.hint = Some(mode);
                                            context.hint_state = HintState::new();
                                        }
                                    });
                                    logger.event("hint_keys", json!({"session":session,"panel_mode":mode.name(),
                                        "keyboard_hook":keyboard_hook.is_some(),"reinstalled":reinstalled}));
                                    let _ = reply.send(keyboard_hook.is_some());
                                }
                            }
                        }
                    } else { let _ = TranslateMessage(&message); DispatchMessageW(&message); }
                }
            }
            if let Some(hook) = keyboard_hook { let _ = UnhookWindowsHookEx(hook); }
            if let Some(hook) = mouse_hook { let _ = UnhookWindowsHookEx(hook); }
            if !foreground_hook.is_invalid() { let _ = UnhookWinEvent(foreground_hook); }
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

    // 管理员提示开始停留：之后的按键按停留规则处理，搜索模式下顺带重新挂上键盘钩子。返回键盘钩子是否在位。
    pub fn begin_hint(&self, mode: HintMode) -> Result<bool> {
        let (reply, receiver) = mpsc::sync_channel(1);
        self.command(Command::Hint { mode, reply })?;
        receiver
            .recv_timeout(REPLAY_WAIT)
            .map_err(|e| e.to_string())
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

    const ESC: u16 = 0x1B;
    const V: u16 = 0x56;
    const X: u16 = 0x58;
    const ENTER: u16 = 0x0D;
    const CTRL: u16 = 0xA2;
    const PASS: Route = Route::Pass { end: None };
    const SWALLOW: Route = Route::Swallow { end: None };

    fn key(mode: HintMode, vk: u16, pressed: bool) -> HintKey {
        HintKey {
            mode: Some(mode),
            vk,
            modifier: is_modifier(vk),
            pressed,
            system: false,
            original: false,
        }
    }
    fn system(mut key: HintKey) -> HintKey {
        key.system = true;
        key
    }
    fn original(mut key: HintKey) -> HintKey {
        key.original = true;
        key
    }

    // 表驱动用例：名称、输入、这个键先前的处置、期望的处理方式、期望的处置记录。
    type Case = (&'static str, HintKey, Option<Fate>, Route, Option<Fate>);

    // 任务说明表格的每一格，外加重复按下、停留前按下的键。
    #[test]
    fn hint_rules_cover_every_cell() {
        use HintMode::{Quick, Search};
        let passthrough = Route::Pass {
            end: Some(HintEnd::KeyPassthrough),
        };
        let consumed = Fate::Swallow(HintEnd::KeyConsumed);
        let escape = Fate::Swallow(HintEnd::Escape);
        let pass = Fate::Pass;
        #[rustfmt::skip]
        let cases: Vec<Case> = vec![
            // Esc：两种模式都吞掉，抬起时收起（抬起也吞）。
            ("快速 Esc 按下", key(Quick, ESC, true), None, SWALLOW, Some(escape)),
            ("快速 Esc 抬起", key(Quick, ESC, false), Some(escape), Route::Swallow { end: Some(HintEnd::Escape) }, None),
            ("搜索 Esc 按下", key(Search, ESC, true), None, SWALLOW, Some(escape)),
            ("搜索 Esc 抬起", key(Search, ESC, false), Some(escape), Route::Swallow { end: Some(HintEnd::Escape) }, None),
            // 呼出键：快速模式交回原逻辑（原逻辑放行，全局快捷键收起）；搜索模式直接放行。都不由停留规则收起。
            ("快速 呼出键按下", system(key(Quick, V, true)), None, Route::Original, None),
            ("快速 呼出键抬起", system(key(Quick, V, false)), None, Route::Original, None),
            ("搜索 呼出键按下", system(key(Search, V, true)), None, PASS, None),
            ("搜索 呼出键抬起", system(key(Search, V, false)), None, PASS, None),
            // 其他非修饰键：快速模式放行并立即收起，抬起也放行；搜索模式吞掉，抬起也吞并收起。
            ("快速 V 按下", key(Quick, V, true), None, passthrough, Some(pass)),
            ("快速 V 抬起", key(Quick, V, false), Some(pass), PASS, None),
            ("快速 字母 A", key(Quick, 0x41, true), None, passthrough, Some(pass)),
            ("快速 数字 1", key(Quick, 0x31, true), None, passthrough, Some(pass)),
            ("快速 小键盘 1", key(Quick, 0x61, true), None, passthrough, Some(pass)),
            ("快速 Enter", key(Quick, ENTER, true), None, passthrough, Some(pass)),
            ("快速 方向键", key(Quick, 0x28, true), None, passthrough, Some(pass)),
            ("搜索 V 按下", key(Search, V, true), None, SWALLOW, Some(consumed)),
            ("搜索 V 抬起", key(Search, V, false), Some(consumed), Route::Swallow { end: Some(HintEnd::KeyConsumed) }, None),
            ("搜索 字母 A", key(Search, 0x41, true), None, SWALLOW, Some(consumed)),
            ("搜索 数字 1", key(Search, 0x31, true), None, SWALLOW, Some(consumed)),
            ("搜索 Enter", key(Search, ENTER, true), None, SWALLOW, Some(consumed)),
            // 按住不放的重复按下：照第一次的处置，不再请求收起。
            ("快速 V 重复按下", key(Quick, V, true), Some(pass), PASS, Some(pass)),
            ("搜索 V 重复按下", key(Search, V, true), Some(consumed), SWALLOW, Some(consumed)),
            ("快速 Esc 重复按下", key(Quick, ESC, true), Some(escape), SWALLOW, Some(escape)),
            // 停留开始前按下、原逻辑已吞掉的键：快速模式下重复按下和抬起都交回原逻辑配对。
            ("快速 原逻辑的键按下", original(key(Quick, V, true)), None, Route::Original, None),
            ("快速 停留前按下的键抬起", key(Quick, V, false), None, Route::Original, None),
            // 搜索模式下原逻辑的吞键记录是补发前留下的，不看；停留前按下的键那时没有钩子，抬起放行。
            ("搜索 过时的原逻辑记录", original(key(Search, V, true)), None, SWALLOW, Some(consumed)),
            ("搜索 停留前按下的键抬起", key(Search, V, false), None, PASS, None),
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
                    assert_eq!(
                        hint_route(key(mode, vk, pressed), None),
                        (PASS, None),
                        "{mode:?} {vk:#x} {pressed}"
                    );
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
            for pressed in [true, false] {
                for system in [false, true] {
                    for original in [false, true] {
                        let input = HintKey {
                            mode: None,
                            vk,
                            modifier: is_modifier(vk),
                            pressed,
                            system,
                            original,
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
    }

    #[test]
    fn system_combos_match_the_original_passthrough() {
        let alt_v = (V, Modifiers::ALT);
        assert!(system_combo(V, Modifiers::ALT, alt_v), "呼出键");
        assert!(
            !system_combo(V, Modifiers::CONTROL, alt_v),
            "Ctrl+V 不是呼出键"
        );
        assert!(!system_combo(V, Modifiers::ALT | Modifiers::SHIFT, alt_v));
        assert!(!system_combo(V, Modifiers::empty(), alt_v));
        assert!(system_combo(0x44, Modifiers::SUPER, alt_v), "Win+D");
        assert!(system_combo(0x09, Modifiers::ALT, alt_v), "Alt+Tab");
        assert!(!system_combo(0x09, Modifiers::empty(), alt_v), "Tab");
        let exit = Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT;
        assert!(system_combo(0x51, exit, alt_v), "退出键");
        assert!(system_combo(0x4c, exit, alt_v), "采集调试键");
        assert!(!system_combo(0x51, Modifiers::CONTROL, alt_v));
        assert!(!system_combo(ENTER, Modifiers::empty(), alt_v));
        assert!(!system_combo(ESC, Modifiers::empty(), alt_v));
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

    // 用 Context::hint_key 同一份状态跑一串事件。
    fn run(mode: HintMode, events: &[(u16, bool)]) -> Vec<Outcome> {
        let mut state = HintState::new();
        events
            .iter()
            .map(|&(vk, pressed)| state.handle(key(mode, vk, pressed)))
            .collect()
    }

    #[test]
    fn ctrl_v_passes_in_quick_mode_and_is_consumed_in_search_mode() {
        let ctrl_v = [(CTRL, true), (V, true), (V, false), (CTRL, false)];
        // 快速模式：Ctrl 放行；V 的按下放行并立即请求收起，抬起也放行、不再请求。
        assert_eq!(
            run(HintMode::Quick, &ctrl_v),
            [
                (Some(false), None),
                (Some(false), Some(HintEnd::KeyPassthrough)),
                (Some(false), None),
                (Some(false), None),
            ]
        );
        // 搜索模式：V 的按下吞掉，抬起也吞，抬起时才请求收起（收起会卸钩，抬起不能漏给还回焦点的目标）。
        assert_eq!(
            run(HintMode::Search, &ctrl_v),
            [
                (Some(false), None),
                (Some(true), None),
                (Some(true), Some(HintEnd::KeyConsumed)),
                (Some(false), None),
            ]
        );
    }

    #[test]
    fn held_keys_keep_their_first_decision() {
        let held = [(V, true), (V, true), (V, true), (V, false)];
        assert_eq!(
            run(HintMode::Quick, &held),
            [
                (Some(false), Some(HintEnd::KeyPassthrough)),
                (Some(false), None),
                (Some(false), None),
                (Some(false), None),
            ]
        );
        let escape = [(ESC, true), (ESC, true), (ESC, false)];
        for mode in [HintMode::Quick, HintMode::Search] {
            assert_eq!(
                run(mode, &escape),
                [
                    (Some(true), None),
                    (Some(true), None),
                    (Some(true), Some(HintEnd::Escape)),
                ],
                "{mode:?}"
            );
        }
    }

    #[test]
    fn overlapping_keys_hide_after_the_last_swallowed_release() {
        // 搜索模式下快速连打 V、X（V 还没抬起 X 就按下了）：V 抬起时 X 还被吞着按住，等 X 抬起再收起。
        let rollover = [(V, true), (X, true), (V, false), (X, false)];
        assert_eq!(
            run(HintMode::Search, &rollover),
            [
                (Some(true), None),
                (Some(true), None),
                (Some(true), None),
                (Some(true), Some(HintEnd::KeyConsumed)),
            ]
        );
        // 快速模式下按住 Esc 时按 V：V 放行，收起等 Esc 抬起，原因按先到的 key_passthrough 记。
        let escape_held = [(ESC, true), (V, true), (V, false), (ESC, false)];
        assert_eq!(
            run(HintMode::Quick, &escape_held),
            [
                (Some(true), None),
                (Some(false), None),
                (Some(false), None),
                (Some(true), Some(HintEnd::KeyPassthrough)),
            ]
        );
    }

    // 按下和抬起成对：停留规则放行了按下，抬起也放行；吞掉了按下，抬起也吞。交叉按键、重复按下也一样；
    // 收起请求只在没有吞掉的键还按着时发出，按过非修饰键、全部抬起后一定已经发出过。
    // 用固定种子的伪随机序列把常见键的各种交错都走一遍。
    #[test]
    fn every_release_follows_its_press() {
        let keys = [V, X, 0x31, ENTER, ESC, 0x28, CTRL, 0xA0];
        let mut seed = 0x2545_F491_4F6C_DD1D_u64;
        for mode in [HintMode::Quick, HintMode::Search] {
            for _ in 0..300 {
                let mut held = [false; 256];
                let mut events = Vec::new();
                for _ in 0..24 {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    let vk = keys[(seed % keys.len() as u64) as usize];
                    // 已按住的键随机再按（自动重复）或抬起；没按住的只会按下。
                    let pressed = !held[vk as usize] || (seed >> 32).is_multiple_of(3);
                    held[vk as usize] = pressed;
                    events.push((vk, pressed));
                }
                for vk in keys {
                    if held[vk as usize] {
                        events.push((vk, false));
                    }
                }
                let outcomes = run(mode, &events);
                let mut first: [Option<bool>; 256] = [None; 256];
                let mut requested = false;
                for (&(vk, pressed), &(swallow, end)) in events.iter().zip(&outcomes) {
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
                let typed = events.iter().any(|&(vk, _)| !is_modifier(vk));
                assert_eq!(requested, typed, "{mode:?} {events:?}");
            }
        }
    }
}
