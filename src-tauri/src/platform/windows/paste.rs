use super::{clipboard, uia, window};
use crate::{
    paste::{
        keys::{Key, Table},
        request::{Mode, Outcome, Request, conclude},
    },
    platform::{REPLAY_WAIT, ReplayKey, Target},
};
use serde_json::{Value, json};
use std::{
    mem::size_of,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    Security::{
        GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_MANDATORY_LABEL,
        TOKEN_QUERY, TokenIntegrityLevel,
    },
    System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    },
    UI::Input::KeyboardAndMouse::*,
    UI::WindowsAndMessaging::IsWindowVisible,
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

fn sequence(key: Key) -> Vec<INPUT> {
    let (modifiers, vk): (&[u16], u16) = match key {
        Key::CtrlV => (&[0x11], 0x56),
        Key::CtrlShiftV => (&[0x11, 0x10], 0x56),
        Key::ShiftInsert => (&[0x10], 0x2D),
    };
    let flags = if vk == 0x2D {
        KEYEVENTF_EXTENDEDKEY
    } else {
        KEYBD_EVENT_FLAGS(0)
    };
    let mut events: Vec<_> = modifiers
        .iter()
        .map(|&vk| input(vk, 0, KEYBD_EVENT_FLAGS(0)))
        .collect();
    events.push(input(vk, 0, flags));
    events.push(input(vk, 0, flags | KEYEVENTF_KEYUP));
    events.extend(
        modifiers
            .iter()
            .rev()
            .map(|&vk| input(vk, 0, KEYEVENTF_KEYUP)),
    );
    events
}

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn integrity(process: HANDLE) -> Option<u32> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(process, TOKEN_QUERY, &mut token).ok()?;
        let token = Handle(token);
        let mut bytes = 0;
        let _ = GetTokenInformation(token.0, TokenIntegrityLevel, None, 0, &mut bytes);
        if bytes < size_of::<TOKEN_MANDATORY_LABEL>() as u32 {
            return None;
        }
        // 用 usize 保证 TOKEN_MANDATORY_LABEL 对齐，而不是把任意 Vec<u8> 强转。
        let mut buffer = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
        GetTokenInformation(
            token.0,
            TokenIntegrityLevel,
            Some(buffer.as_mut_ptr().cast()),
            bytes,
            &mut bytes,
        )
        .ok()?;
        let label = &*buffer.as_ptr().cast::<TOKEN_MANDATORY_LABEL>();
        let count = *GetSidSubAuthorityCount(label.Label.Sid);
        if count == 0 {
            return None;
        }
        Some(*GetSidSubAuthority(label.Label.Sid, count as u32 - 1))
    }
}

fn compare_integrity(target: &Target) -> Value {
    let own = integrity(unsafe { GetCurrentProcess() });
    let other = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, target.process) }
        .ok()
        .and_then(|process| {
            let process = Handle(process);
            integrity(process.0)
        });
    let status = match (own, other) {
        (Some(own), Some(other)) if other > own => "higher",
        (Some(_), Some(_)) => "allowed",
        _ => "unknown_assume_higher",
    };
    json!({"status":status,"target_token_known":other.is_some(),"own_token_known":own.is_some()})
}

#[derive(Clone, Copy)]
struct Checks {
    foreground: bool,
    focus: bool,
    modifiers: bool,
    hidden: bool,
}
impl Checks {
    fn all(self) -> bool {
        self.foreground && self.focus && self.modifiers && self.hidden
    }
    fn values(self) -> [bool; 4] {
        [self.foreground, self.focus, self.modifiers, self.hidden]
    }
}
fn checks(request: &Request) -> Checks {
    let focus = window::confirm(&request.target);
    Checks {
        foreground: focus.foreground.success,
        focus: focus.focus_restored,
        modifiers: !MODIFIERS.iter().any(|&vk| held(vk)),
        hidden: !unsafe { IsWindowVisible(window::hwnd(request.panel)) }.as_bool(),
    }
}

fn preflight(request: &Request, stopping: &AtomicBool) -> (bool, Value) {
    let started = Instant::now();
    let mut times = [None; 4];
    let mut released = 0;
    let mut release_attempted = false;
    let mut current;
    loop {
        current = checks(request);
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        for (index, satisfied) in current.values().iter().enumerate() {
            if *satisfied && times[index].is_none() {
                times[index] = Some(elapsed);
            }
        }
        if current.all() || stopping.load(Ordering::Acquire) {
            break;
        }
        if started.elapsed() >= Duration::from_millis(300) {
            // 只有焦点仍安全时才补抬起，避免把按键送进用户刚切换的窗口。
            if !current.modifiers && current.foreground && current.focus && current.hidden {
                let releases: Vec<_> = MODIFIERS
                    .iter()
                    .copied()
                    .filter(|&vk| held(vk))
                    .map(|vk| {
                        input(
                            vk,
                            0,
                            KEYEVENTF_KEYUP
                                | if matches!(vk, 0xA3 | 0xA5 | 0x5B | 0x5C) {
                                    KEYEVENTF_EXTENDEDKEY
                                } else {
                                    KEYBD_EVENT_FLAGS(0)
                                },
                        )
                    })
                    .collect();
                release_attempted = !releases.is_empty();
                released = send(&releases);
                current = checks(request);
                for (index, satisfied) in current.values().iter().enumerate() {
                    if *satisfied && times[index].is_none() {
                        times[index] = Some(started.elapsed().as_secs_f64() * 1000.0);
                    }
                }
            }
            break;
        }
        thread::sleep(
            Duration::from_millis(5)
                .min(Duration::from_millis(300).saturating_sub(started.elapsed())),
        );
    }
    let names = ["foreground", "focus", "modifiers", "panel_hidden"];
    let failed: Vec<_> = names
        .into_iter()
        .zip(current.values())
        .filter_map(|(name, ok)| if ok { None } else { Some(name) })
        .collect();
    (
        current.all() && !stopping.load(Ordering::Acquire),
        json!({
            "foreground_ms":times[0],"focus_ms":times[1],"modifiers_ms":times[2],"panel_hidden_ms":times[3],
            "timed_out":!current.all(),"failed_checks":failed,"release_attempted":release_attempted,"released_modifiers":released,
            "deadline_reached":started.elapsed() >= Duration::from_millis(300),
        }),
    )
}

fn copy(
    request: &Request,
    clipboard: &clipboard::Service,
    report: &mut Value,
    reason: &'static str,
) -> Result<Outcome, String> {
    clipboard.write(&request.text, request.target.process, false)?;
    report["clipboard_written"] = json!(true);
    report["reason"] = json!(reason);
    Ok(conclude(request.mode, false, false, true))
}

fn perform(
    request: &Request,
    table: &Table,
    clipboard: &clipboard::Service,
    stopping: &AtomicBool,
    on_sent: &dyn Fn(),
    report: &mut Value,
) -> Result<Outcome, String> {
    if stopping.load(Ordering::Acquire) {
        return Err("request_cancelled".into());
    }
    let selected = table.select(&request.target.process_name, &request.target.window_class);
    report["key_rule"] = json!(selected);
    // 只认面板隐藏前的判断；隐藏后前台由系统重新指定，交给还焦点和发键前确认处理。
    let external_switch = request.external_switch;
    if request.restore_focus && !external_switch && window::valid(&request.target) {
        report["restore"] = json!(window::restore(&request.target));
    }
    if request.mode == Mode::Copy {
        return copy(request, clipboard, report, "user_copy");
    }
    if !window::valid(&request.target) {
        return copy(request, clipboard, report, "invalid_target");
    }
    if external_switch {
        return copy(request, clipboard, report, "external_switch");
    }
    let integrity = compare_integrity(&request.target);
    let allowed = integrity["status"] == "allowed";
    report["integrity"] = integrity;
    if !allowed {
        return copy(request, clipboard, report, "target_may_be_elevated");
    }
    let (ready, confirmation) = preflight(request, stopping);
    report["preflight"] = confirmation;
    if stopping.load(Ordering::Acquire) {
        return Err("request_cancelled".into());
    }
    if !ready {
        return copy(request, clipboard, report, "preflight_timeout");
    }
    let probe = uia::Probe::start(request.target.process, request.text.clone());
    let before = probe.baseline();
    report["layer3"] = json!(before);
    if stopping.load(Ordering::Acquire) {
        return Err("request_cancelled".into());
    }
    // UIA 和打开剪贴板期间用户可能改变焦点，所以在写入与发键前分别复核。
    if !checks(request).all() {
        return copy(request, clipboard, report, "conditions_changed");
    }
    let observation = clipboard.write(&request.text, request.target.process, true)?;
    report["clipboard_written"] = json!(true);
    if stopping.load(Ordering::Acquire) || !checks(request).all() {
        report["clipboard_materialized"] = json!(clipboard.materialize());
        report["reason"] = json!("conditions_changed");
        return Ok(Outcome::CopiedOnly);
    }
    let inputs = sequence(selected.key);
    let sent_at = Instant::now();
    let sent = send(&inputs);
    report["send_input"] = json!(sent);
    report["expected_events"] = json!(inputs.len());
    report["actual_key"] = json!(selected.key);
    report["selected_to_send_ms"] =
        json!(sent_at.duration_since(request.started).as_secs_f64() * 1000.0);
    if sent != inputs.len() as u32 {
        // 只补齐实际成功按下且尚未抬起的键，不能再发整套快捷键。
        let mut held_keys = Vec::new();
        for event in inputs.iter().take(sent as usize) {
            let key = unsafe { event.Anonymous.ki };
            if key.dwFlags.contains(KEYEVENTF_KEYUP) {
                held_keys.retain(|&(vk, _)| vk != key.wVk.0);
            } else {
                held_keys.push((key.wVk.0, key.dwFlags));
            }
        }
        let cleanup: Vec<_> = held_keys
            .into_iter()
            .rev()
            .map(|(vk, flags)| input(vk, 0, flags | KEYEVENTF_KEYUP))
            .collect();
        if !cleanup.is_empty() {
            report["cleanup_send_input"] = json!(send(&cleanup));
        }
        report["clipboard_materialized"] = json!(clipboard.materialize());
        report["reason"] = json!("send_failed");
        return Ok(Outcome::CopiedOnly);
    }
    on_sent();
    probe.sent(sent_at);
    let layer3 = probe.finish(before, sent_at);
    let confirmed = layer3.conclusion == "confirmed";
    report["layer3"] = json!(layer3);
    while sent_at.elapsed() < Duration::from_secs(1)
        && !observation.lock().unwrap().observed()
        && !stopping.load(Ordering::Acquire)
    {
        thread::sleep(Duration::from_millis(5));
    }
    report["layer2"] = observation.lock().unwrap().report(sent_at);
    Ok(conclude(request.mode, true, confirmed, false))
}

pub fn execute(
    request: Request,
    table: &Table,
    clipboard: &clipboard::Service,
    stopping: &AtomicBool,
    on_sent: &dyn Fn(),
) -> Value {
    let mut report = json!({"paste_result":null,"reason":null,"clipboard_written":false,
        "process_name":request.target.process_name,"window_class":request.target.window_class,
        "focus_class":request.target.focus_class,"focus_unknown":request.target.focus==0,
        "key_rule":null,"actual_key":null,"preflight":null,"integrity":null,
        "send_input":0,"expected_events":0,"layer2":null,"layer3":uia::Evidence::default(),
        "selected_to_send_ms":null,"selected_to_result_ms":null});
    match perform(&request, table, clipboard, stopping, on_sent, &mut report) {
        Ok(outcome) => report["paste_result"] = json!(outcome),
        Err(reason) => report["reason"] = json!(reason),
    }
    report["selected_to_result_ms"] = json!(request.started.elapsed().as_secs_f64() * 1000.0);
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requires_all_four_preflight_conditions() {
        for mask in 0..16 {
            let conditions = Checks {
                foreground: mask & 1 != 0,
                focus: mask & 2 != 0,
                modifiers: mask & 4 != 0,
                hidden: mask & 8 != 0,
            };
            assert_eq!(conditions.all(), mask == 15);
        }
    }

    #[test]
    fn builds_virtual_key_sequences_without_sending_input() {
        for (key, expected) in [
            (Key::CtrlV, vec![0x11, 0x56, 0x56, 0x11]),
            (Key::CtrlShiftV, vec![0x11, 0x10, 0x56, 0x56, 0x10, 0x11]),
            (Key::ShiftInsert, vec![0x10, 0x2D, 0x2D, 0x10]),
        ] {
            let events = sequence(key);
            let keys: Vec<_> = events
                .iter()
                .map(|event| unsafe { event.Anonymous.ki.wVk.0 })
                .collect();
            assert_eq!(keys, expected);
            for event in events {
                let key = unsafe { event.Anonymous.ki };
                assert_eq!(key.dwExtraInfo, INPUT_MARKER);
                assert_eq!(
                    key.dwFlags.contains(KEYEVENTF_EXTENDEDKEY),
                    key.wVk.0 == 0x2D
                );
            }
        }
    }
}
