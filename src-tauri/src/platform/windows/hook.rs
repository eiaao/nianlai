use super::paste::INPUT_MARKER;
use crate::{
    log::Logger,
    platform::{HookEvent, REPLAY_CAPACITY, REPLAY_WAIT, ReplayBatch, ReplayKey, Result},
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
}
impl Context {
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
        let exit = matches!(vk, 0x51 | 0x4c) && mods == (Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT);
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
