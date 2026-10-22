use super::window;
use crate::platform::{REPLAY_WAIT, ReplayKey, Result, Target};
use serde_json::{Value, json};
use std::{
    mem::size_of,
    thread,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{GlobalFree, HANDLE, HGLOBAL},
        System::{
            DataExchange::{
                CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW,
                SetClipboardData,
            },
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
            Ole::CF_UNICODETEXT,
        },
        UI::Input::KeyboardAndMouse::*,
    },
    core::w,
};

pub const INPUT_MARKER: usize = 0x4E4C4730;
const MODIFIERS: [u16; 8] = [0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0x5B, 0x5C];
fn held(vk: u16) -> bool {
    unsafe { GetAsyncKeyState(vk as i32) < 0 }
}
fn input(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INPUT_MARKER,
            },
        },
    }
}
fn send(inputs: &[INPUT]) -> u32 {
    unsafe { SendInput(inputs, size_of::<INPUT>() as i32) }
}

pub fn cancel_alt_menu() -> u32 {
    send(&[
        input(0xE8, 0, KEYBD_EVENT_FLAGS(0)),
        input(0xE8, 0, KEYEVENTF_KEYUP),
    ])
}

pub fn replay(keys: &[ReplayKey], panel: usize, started: Instant) -> usize {
    let can_replay = || {
        started.elapsed() < REPLAY_WAIT
            && window::foreground() == panel
            && !MODIFIERS[2..].iter().any(|&vk| held(vk))
    };
    if keys.is_empty() || !can_replay() {
        return 0;
    }
    let physical_shift = [held(0xA0), held(0xA1)];
    let mut replay_shift = physical_shift;
    let mut events = Vec::with_capacity(keys.len() * 6);
    let mut boundaries = Vec::with_capacity(keys.len());
    for key in keys {
        if key.shift && !replay_shift.iter().any(|&down| down) {
            events.push(input(0xA0, 0, KEYBD_EVENT_FLAGS(0)));
            replay_shift[0] = true;
        }
        if !key.shift {
            for (index, down) in replay_shift.iter_mut().enumerate() {
                if *down {
                    events.push(input(0xA0 + index as u16, 0, KEYEVENTF_KEYUP));
                    *down = false;
                }
            }
        }
        let flags = if key.extended {
            KEYEVENTF_EXTENDEDKEY
        } else {
            KEYBD_EVENT_FLAGS(0)
        };
        boundaries.push((events.len(), key, flags));
        events.push(input(key.vk, key.scan, flags));
        events.push(input(key.vk, key.scan, flags | KEYEVENTF_KEYUP));
    }
    // 只在必要时切换 Shift，整批结束后再恢复原状态，避免相邻键间多余的 Shift 事件。
    for (index, down) in physical_shift.iter().enumerate() {
        if replay_shift[index] != *down {
            events.push(input(
                0xA0 + index as u16,
                0,
                if *down {
                    KEYBD_EVENT_FLAGS(0)
                } else {
                    KEYEVENTF_KEYUP
                },
            ));
        }
    }
    // 一次提交整个队列，避免补发的键之间插入新的物理按键。
    if !can_replay() {
        return 0;
    }
    let sent = send(&events) as usize;
    let replayed = boundaries
        .iter()
        .filter(|(down, _, _)| down + 2 <= sent)
        .count();
    if sent > 0 && sent < events.len() {
        let mut cleanup = Vec::new();
        for (down, key, flags) in &boundaries {
            if sent == down + 1 {
                cleanup.push(input(key.vk, key.scan, *flags | KEYEVENTF_KEYUP));
            }
        }
        for (index, down) in physical_shift.iter().enumerate() {
            cleanup.push(input(
                0xA0 + index as u16,
                0,
                if *down {
                    KEYBD_EVENT_FLAGS(0)
                } else {
                    KEYEVENTF_KEYUP
                },
            ));
        }
        let _ = send(&cleanup);
    }
    replayed
}

struct Memory(HGLOBAL);
impl Memory {
    fn new(bytes: &[u8]) -> Result<Self> {
        unsafe {
            let memory = Self(GlobalAlloc(GMEM_MOVEABLE, bytes.len()).map_err(|e| e.to_string())?);
            let pointer = GlobalLock(memory.0);
            if pointer.is_null() {
                return Err(windows::core::Error::from_thread().to_string());
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer.cast(), bytes.len());
            let _ = GlobalUnlock(memory.0);
            Ok(memory)
        }
    }
    fn transfer(self, format: u32) -> Result<()> {
        unsafe {
            SetClipboardData(format, Some(HANDLE(self.0.0))).map_err(|e| e.to_string())?;
        }
        std::mem::forget(self);
        Ok(())
    }
}
impl Drop for Memory {
    fn drop(&mut self) {
        unsafe {
            let _ = GlobalFree(Some(self.0));
        }
    }
}
struct Clipboard;
impl Drop for Clipboard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

fn write_clipboard(owner: usize, text: &str) -> Result<()> {
    let bytes: Vec<u8> = text
        .encode_utf16()
        .chain(Some(0))
        .flat_map(u16::to_ne_bytes)
        .collect();
    let text = Memory::new(&bytes)?;
    let ignore = Memory::new(&1u32.to_ne_bytes())?;
    let exclude = Memory::new(&1u32.to_ne_bytes())?;
    unsafe {
        let ignore_format = RegisterClipboardFormatW(w!("Clipboard Viewer Ignore"));
        let exclude_format =
            RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing"));
        if ignore_format == 0 || exclude_format == 0 {
            return Err(windows::core::Error::from_thread().to_string());
        }
        let start = Instant::now();
        loop {
            match OpenClipboard(Some(window::hwnd(owner))) {
                Ok(()) => break,
                Err(error) if start.elapsed() >= Duration::from_millis(200) => {
                    return Err(error.to_string());
                }
                Err(_) => thread::sleep(Duration::from_millis(5)),
            }
        }
        let _clipboard = Clipboard;
        EmptyClipboard().map_err(|e| e.to_string())?;
        // 先写忽略标记，失败时不把正文暴露给剪贴板监视器。
        ignore.transfer(ignore_format)?;
        exclude.transfer(exclude_format)?;
        text.transfer(CF_UNICODETEXT.0 as u32)
    }
}

pub fn paste(owner: usize, target: &Target, text: &str, allow_paste: bool) -> Value {
    let started = Instant::now();
    while MODIFIERS.iter().any(|&vk| held(vk)) && started.elapsed() < Duration::from_millis(500) {
        thread::sleep(Duration::from_millis(5));
    }
    let waited = started.elapsed().as_secs_f64() * 1000.0;
    let remaining: Vec<_> = MODIFIERS.iter().copied().filter(|&vk| held(vk)).collect();
    let mut released = 0;
    if allow_paste && window::confirm(target).focus_restored && !remaining.is_empty() {
        let release: Vec<_> = remaining
            .iter()
            .map(|&vk| input(vk, 0, KEYEVENTF_KEYUP))
            .collect();
        released = send(&release);
    }
    if let Err(error) = write_clipboard(owner, text) {
        let confirmed = window::confirm(target);
        return json!({"paste_result":"clipboard_failed", "error":error, "modifiers_wait_ms":waited, "send_input":0, "foreground_ok":confirmed.foreground.success, "focus_restored":confirmed.focus_restored, "focus_unknown":confirmed.focus_unknown});
    }
    let confirmed = window::confirm(target);
    let can_send = allow_paste && confirmed.focus_restored && !MODIFIERS.iter().any(|&vk| held(vk));
    let sent = if can_send {
        send(&[
            input(0x11, 0, KEYBD_EVENT_FLAGS(0)),
            input(0x56, 0, KEYBD_EVENT_FLAGS(0)),
            input(0x56, 0, KEYEVENTF_KEYUP),
            input(0x11, 0, KEYEVENTF_KEYUP),
        ])
    } else {
        0
    };
    if can_send && sent != 4 {
        let _ = send(&[
            input(0x56, 0, KEYEVENTF_KEYUP),
            input(0x11, 0, KEYEVENTF_KEYUP),
        ]);
    }
    json!({"paste_result": if sent == 4 {"sent"} else if can_send {"send_failed"} else {"copied_only"},
        "foreground_ok": confirmed.foreground.success, "focus_restored": confirmed.focus_restored, "focus_unknown": confirmed.focus_unknown,
        "modifiers_wait_ms": waited, "modifiers_timed_out": !remaining.is_empty(), "released_modifiers":released, "send_input":sent})
}
