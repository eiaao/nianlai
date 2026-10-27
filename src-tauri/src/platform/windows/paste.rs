use super::{clipboard, hash, uia, window};
use crate::{
    history::content::{Content, TEXT},
    paste::{
        as_text,
        keys::{Key, Match, Table},
        native,
        request::{Callbacks, Mode, Outcome, Request, RestorePolicy, conclude},
    },
    platform::{REPLAY_WAIT, ReplayKey, Target, key_state},
};
use serde_json::{Value, json};
use std::{
    mem::size_of,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::{CloseHandle, GetLastError, HANDLE, LPARAM, SetLastError, WIN32_ERROR, WPARAM},
    Security::{
        GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_ELEVATION,
        TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenElevation, TokenIntegrityLevel,
    },
    System::Threading::{
        GetCurrentProcess, GetCurrentThreadId, OpenProcess, OpenProcessToken,
        PROCESS_QUERY_LIMITED_INFORMATION,
    },
    UI::Input::KeyboardAndMouse::*,
    UI::WindowsAndMessaging::{
        GetWindowThreadProcessId, IsWindowVisible, SMTO_ABORTIFHUNG, SMTO_BLOCK, SMTO_ERRORONEXIT,
        SendMessageTimeoutW, WM_PASTE,
    },
};

pub const INPUT_MARKER: usize = 0x4E4C4730;
const MODIFIERS: [u16; 8] = [0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0x5B, 0x5C];
fn held(vk: u16) -> bool {
    unsafe { GetAsyncKeyState(vk as i32) < 0 }
}
fn cancelled(request: &Request, stopping: &AtomicBool) -> bool {
    stopping.load(Ordering::Acquire) || request.cancelled.load(Ordering::Acquire)
}
pub fn keys_released() -> bool {
    key_state::released(held)
}
pub fn key_state_counts() -> key_state::Counts {
    key_state::Counts::sample(held)
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

fn elevated(process: HANDLE) -> Option<bool> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(process, TOKEN_QUERY, &mut token).ok()?;
        let token = Handle(token);
        let mut elevation = TOKEN_ELEVATION::default();
        let mut bytes = 0;
        GetTokenInformation(
            token.0,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut bytes,
        )
        .ok()?;
        Some(elevation.TokenIsElevated != 0)
    }
}

// 面板在粘贴前也用它预判目标是不是管理员（M0-6：预判为是就先不收起面板），判定和 perform 里的一致。
pub fn compare_integrity(target: &Target) -> Value {
    let own = integrity(unsafe { GetCurrentProcess() });
    let own_elevated = elevated(unsafe { GetCurrentProcess() });
    let (other, other_elevated) =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, target.process) }
            .ok()
            .map(|process| {
                let process = Handle(process);
                (integrity(process.0), elevated(process.0))
            })
            .unwrap_or((None, None));
    let status = match (own, other) {
        (Some(own), Some(other)) if other > own => "higher",
        (Some(_), Some(_)) => "allowed",
        _ => "unknown_assume_higher",
    };
    // 同为高 IL 也可能 allowed；新路径另要求双方明确是普通 medium IL、均未提升。
    let native_ordinary = native::ordinary_tokens(own, own_elevated, other, other_elevated);
    json!({"status":status,"target_token_known":other.is_some(),"own_token_known":own.is_some(),
        "native_ordinary":native_ordinary})
}

#[derive(Clone, Copy)]
struct Checks {
    foreground: bool,
    focus: bool,
    modifiers: bool,
    hidden: bool,
    permission: bool,
}
impl Checks {
    fn all(self) -> bool {
        self.foreground && self.focus && self.modifiers && self.hidden && self.permission
    }
    fn values(self) -> [bool; 5] {
        [
            self.foreground,
            self.focus,
            self.modifiers,
            self.hidden,
            self.permission,
        ]
    }
}
fn checks(request: &Request) -> Checks {
    let focus = if request
        .permit
        .as_ref()
        .is_some_and(|permit| permit.search())
    {
        window::confirm_strict(&request.target)
    } else {
        window::confirm(&request.target)
    };
    Checks {
        foreground: focus.foreground.success,
        focus: focus.focus_restored,
        modifiers: !MODIFIERS.iter().any(|&vk| held(vk)),
        hidden: !unsafe { IsWindowVisible(window::hwnd(request.panel)) }.as_bool(),
        permission: request.permit.as_ref().is_some_and(|permit| {
            permit.can_send(
                request.session,
                request.record_id,
                request.mode,
                &request.target,
                window::foreground(),
            )
        }),
    }
}

fn preflight(request: &Request, stopping: &AtomicBool) -> (bool, Value) {
    let started = Instant::now();
    let mut times = [None; 5];
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
        if current.all() || cancelled(request, stopping) {
            break;
        }
        if started.elapsed() >= Duration::from_millis(300) {
            // 只有焦点仍安全时才补抬起，避免把按键送进用户刚切换的窗口。
            if !current.modifiers
                && current.foreground
                && current.focus
                && current.hidden
                && current.permission
            {
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
    let names = [
        "foreground",
        "focus",
        "modifiers",
        "panel_hidden",
        "permission",
    ];
    let failed: Vec<_> = names
        .into_iter()
        .zip(current.values())
        .filter_map(|(name, ok)| if ok { None } else { Some(name) })
        .collect();
    (
        current.all() && !cancelled(request, stopping),
        json!({
            "foreground_ms":times[0],"focus_ms":times[1],"modifiers_ms":times[2],"panel_hidden_ms":times[3],
            "permission_ms":times[4],
            "timed_out":!current.all(),"failed_checks":failed,"release_attempted":release_attempted,"released_modifiers":released,
            "deadline_reached":started.elapsed() >= Duration::from_millis(300),
        }),
    )
}

fn record(request: &Request) -> clipboard::Record {
    clipboard::Record {
        id: request.record_id,
        fingerprint: request.content.fingerprint,
    }
}

fn copy(
    request: &Request,
    written: &Arc<Content>,
    clipboard: &clipboard::Service,
    report: &mut Value,
    reason: &'static str,
) -> Result<Outcome, String> {
    if request.cancelled.load(Ordering::Acquire) {
        return Err("request_cancelled".into());
    }
    clipboard.write(
        record(request),
        written.clone(),
        request.target.process,
        false,
    )?;
    report["clipboard_written"] = json!(true);
    report["reason"] = json!(reason);
    Ok(conclude(request.mode, false, false, true))
}

const NATIVE_TIMEOUT_MS: u32 = 200;

#[cfg(test)]
#[path = "native_message_tests.rs"]
mod native_message_tests;

pub(crate) fn submit_native_message(
    control: usize,
    expected_thread: u32,
    expected_process: u32,
    timeout_ms: u32,
) -> native::Delivery {
    let mut process = 0;
    let thread = unsafe { GetWindowThreadProcessId(window::hwnd(control), Some(&mut process)) };
    // 专用 paste-request 线程在此之前从未 AttachThreadInput；同线程不受 SMTO 超时保障，明确拒绝。
    if thread == 0
        || thread != expected_thread
        || process != expected_process
        || thread == unsafe { GetCurrentThreadId() }
    {
        return native::Delivery {
            attempted: false,
            acknowledged: false,
            error_code: None,
        };
    }
    let acknowledged = unsafe {
        SetLastError(WIN32_ERROR(0));
        SendMessageTimeoutW(
            window::hwnd(control),
            WM_PASTE,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG | SMTO_BLOCK | SMTO_ERRORONEXIT,
            timeout_ms,
            None,
        )
        .0 != 0
    };
    native::Delivery {
        attempted: true,
        acknowledged,
        error_code: (!acknowledged).then(|| unsafe { GetLastError().0 }),
    }
}

fn pure_unicode(content: &Content) -> bool {
    content.formats.len() == 1
        && content.raw(TEXT).is_some_and(|bytes| {
            bytes.len() % 2 == 0
                && String::from_utf16(
                    &bytes
                        .chunks_exact(2)
                        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                        .take_while(|unit| *unit != 0)
                        .collect::<Vec<_>>(),
                )
                .is_ok()
        })
}

struct Native<'a> {
    request: &'a Request,
    written: &'a Arc<Content>,
    clipboard: &'a clipboard::Service,
    stopping: &'a AtomicBool,
    restore: &'a dyn Fn(RestorePolicy) -> Option<crate::platform::FocusResult>,
    on_sent: &'a dyn Fn(),
    synchronize: &'a dyn Fn() -> bool,
    report: &'a mut Value,
}
impl native::Backend for Native<'_> {
    fn synchronize(&mut self) -> bool {
        if cancelled(self.request, self.stopping) {
            return true;
        }
        let synchronized = (self.synchronize)();
        if !synchronized && let Some(permit) = self.request.permit.as_ref() {
            permit.revoke();
        }
        synchronized
    }
    fn check(&mut self) -> Result<(), &'static str> {
        if cancelled(self.request, self.stopping) {
            return Err("request_cancelled");
        }
        let integrity = compare_integrity(&self.request.target);
        let ordinary = integrity["status"] == "allowed" && integrity["native_ordinary"] == true;
        self.report["integrity"] = integrity;
        let target_valid = window::native_control_valid(&self.request.target);
        let modifiers_released = !MODIFIERS.iter().any(|&vk| held(vk));
        // 可能耗时的 token/身份查询在前；许可与取消状态紧邻实际写入/提交再次取值。
        let input_desktop = native::input_desktop_ready(
            window::thread_input_desktop_receiving_input().ok(),
            || {
                if let Some(permit) = self.request.permit.as_ref() {
                    permit.revoke();
                }
            },
        );
        let permitted = self.request.permit.as_ref().is_some_and(|permit| {
            permit.can_submit_native(
                self.request.session,
                self.request.record_id,
                self.request.mode,
                &self.request.target,
                window::foreground(),
            )
        });
        native::Conditions {
            cancelled: cancelled(self.request, self.stopping),
            input_desktop,
            target_valid,
            permitted,
            modifiers_released,
            ordinary,
        }
        .check()
    }
    fn write(&mut self) -> Result<(), String> {
        if cancelled(self.request, self.stopping) {
            return Err("request_cancelled".into());
        }
        // 原生提交不需要延迟渲染；固定的实际格式只写一次。
        self.clipboard.write(
            record(self.request),
            self.written.clone(),
            self.request.target.process,
            false,
        )?;
        self.report["clipboard_written"] = json!(true);
        Ok(())
    }
    fn submit(&mut self) -> native::Submission {
        let sent_at = Instant::now();
        // 返回值只证明消息调用完成，绝不读取 WM_PASTE 的结果值推断内容成功。
        let delivery = submit_native_message(
            self.request.target.focus,
            self.request.target.thread,
            self.request.target.process,
            NATIVE_TIMEOUT_MS,
        );
        self.report["native_submission"] = json!({"attempted":delivery.attempted,"acknowledged":delivery.acknowledged,
            "status":if !delivery.attempted {"not_submitted"} else if delivery.acknowledged {"sent"} else {"unknown"},
            "timeout_ms":NATIVE_TIMEOUT_MS,"error_code":delivery.error_code});
        self.report["selected_to_send_ms"] =
            json!(sent_at.duration_since(self.request.started).as_secs_f64() * 1000.0);
        delivery.submission()
    }
    fn finish(&mut self, result: Option<native::Completion>) {
        let unknown = result == Some(native::Completion::Submitted(native::Submission::Unknown));
        if unknown {
            self.report["reason"] = json!("native_submission_unknown");
            self.report["passive_hint"] = json!(true);
            self.report["hint_message"] = json!("粘贴结果未确认，请先检查目标，避免重复粘贴");
        }
        let cleanup = (self.restore)(RestorePolicy::NativeAfterSubmit { unknown });
        self.report["native_cleanup"] = json!({"reply_received":cleanup.is_some()});
        if let Some(focus) = cleanup {
            self.report["restore"] = json!(focus);
        }
        // 归还/提示编排完成后才允许新会话，防止提前 PasteSent 使 ReturnForPaste 失效。
        if result == Some(native::Completion::Submitted(native::Submission::Sent)) {
            (self.on_sent)();
        }
    }
}

// written 是实际写进剪贴板的内容：降级成文字时和 request.content（历史里的记录）不同。
fn perform(
    request: &Request,
    written: &Arc<Content>,
    selected: Match,
    clipboard: &clipboard::Service,
    stopping: &AtomicBool,
    on_sent: &dyn Fn(),
    report: &mut Value,
) -> Result<Outcome, String> {
    if cancelled(request, stopping) {
        return Err("request_cancelled".into());
    }
    let external_switch = request.external_switch;
    if request.mode == Mode::Copy {
        return copy(request, written, clipboard, report, "user_copy");
    }
    if !window::valid(&request.target) {
        return copy(request, written, clipboard, report, "invalid_target");
    }
    if external_switch {
        return copy(request, written, clipboard, report, "external_switch");
    }
    if let Some(reason) = request.copy_only_reason {
        return copy(request, written, clipboard, report, reason);
    }
    if !request
        .permit
        .as_ref()
        .is_some_and(|permit| permit.current(&request.target, window::foreground()))
    {
        return copy(
            request,
            written,
            clipboard,
            report,
            "selection_boundary_changed",
        );
    }
    let integrity = compare_integrity(&request.target);
    let allowed = integrity["status"] == "allowed";
    report["integrity"] = integrity;
    if !allowed {
        return copy(
            request,
            written,
            clipboard,
            report,
            "target_may_be_elevated",
        );
    }
    let (ready, confirmation) = preflight(request, stopping);
    report["preflight"] = confirmation;
    if cancelled(request, stopping) {
        return Err("request_cancelled".into());
    }
    if !ready {
        return copy(request, written, clipboard, report, "preflight_timeout");
    }
    let probe = written
        .text_value()
        .map(|text| uia::Probe::start(request.target.process, text));
    let before = probe
        .as_ref()
        .map(|probe| probe.baseline())
        .unwrap_or_default();
    report["layer3"] = json!(before);
    if cancelled(request, stopping) {
        return Err("request_cancelled".into());
    }
    // UIA 和打开剪贴板期间用户可能改变焦点，所以在写入与发键前分别复核。
    if !checks(request).all() {
        return copy(request, written, clipboard, report, "conditions_changed");
    }
    let observation = clipboard.write(
        record(request),
        written.clone(),
        request.target.process,
        true,
    )?;
    report["clipboard_written"] = json!(true);
    if cancelled(request, stopping) || !checks(request).all() {
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
    let layer3 = if let Some(probe) = probe {
        probe.sent(sent_at);
        probe.finish(before, sent_at)
    } else {
        before
    };
    let confirmed = layer3.conclusion == "confirmed";
    report["layer3"] = json!(layer3);
    while sent_at.elapsed() < Duration::from_secs(1)
        && !observation.lock().unwrap().observed()
        && !cancelled(request, stopping)
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
    callbacks: Callbacks<'_>,
    exports: Option<&Path>,
) -> Value {
    let mut report = json!({"paste_result":null,"reason":null,"clipboard_written":false,
        "record_id":request.record_id,"written_formats":[],
        "process_name":request.target.process_name,"window_class":request.target.window_class,
        "focus_class":request.target.focus_class,"focus_unknown":request.target.focus==0,
        "key_rule":null,"as_text":null,"actual_key":null,"preflight":null,"integrity":null,
        "send_input":0,"expected_events":0,"layer2":null,"layer3":uia::Evidence::default(),
        "selected_to_send_ms":null,"selected_to_result_ms":null});
    let selected = table.select(&request.target.process_name, &request.target.window_class);
    report["key_rule"] = json!(selected);
    // 只收文字的目标或 Ctrl+Enter：先转成文字。图片要编码 PNG，放在发键前确认之前做。
    let (written, conversion) = as_text::apply(
        &request.content,
        request.mode,
        selected.as_text,
        exports,
        hash::sha256,
    );
    report["as_text"] = conversion;
    // 转换后的 Arc 固定后才选择策略；不可按历史 kind 猜测，也不重复转换。
    let candidate = native::eligible(native::Eligibility {
        selection: request
            .permit
            .as_ref()
            .is_some_and(|permit| permit.search()),
        copy: request.mode == Mode::Copy,
        ordinary: !request.external_switch && request.copy_only_reason.is_none(),
        control_class: &request.target.focus_class,
        pure_unicode: pure_unicode(&written),
        builtin_default_ctrl_v: table.is_builtin_default_ctrl_v(selected),
    });
    let ordinary = candidate && compare_integrity(&request.target)["native_ordinary"] == true;
    let outcome = if ordinary {
        report["submission_method"] = json!("wm_paste");
        report["native_submission"] =
            json!({"attempted":false,"acknowledged":false,"status":"not_submitted"});
        let result = {
            let mut backend = Native {
                request: &request,
                written: &written,
                clipboard,
                stopping,
                restore: callbacks.restore,
                on_sent: callbacks.on_sent,
                synchronize: callbacks.synchronize,
                report: &mut report,
            };
            native::run(&mut backend)
        };
        result.map(|completion| match completion {
            native::Completion::CopiedOnly(reason) => {
                report["reason"] = json!(reason);
                Outcome::CopiedOnly
            }
            native::Completion::Submitted(native::Submission::Sent) => Outcome::Sent,
            native::Completion::Submitted(native::Submission::Unknown) => {
                Outcome::NativeSubmissionUnknown
            }
            native::Completion::Submitted(native::Submission::Refused) => {
                unreachable!("拒绝提交已转为只复制")
            }
        })
    } else {
        if let Some(focus) = (callbacks.restore)(RestorePolicy::LegacyBeforeSubmit) {
            report["restore"] = json!(focus);
        }
        perform(
            &request,
            &written,
            selected,
            clipboard,
            stopping,
            callbacks.on_sent,
            &mut report,
        )
    };
    match outcome {
        Ok(outcome) => report["paste_result"] = json!(outcome),
        Err(reason) => report["reason"] = json!(reason),
    }
    report["selected_to_result_ms"] = json!(request.started.elapsed().as_secs_f64() * 1000.0);
    if report["clipboard_written"] == true {
        let mut names: Vec<_> = written.formats.keys().cloned().collect();
        if written.formats.contains_key(crate::history::content::FILES) {
            names.push("Preferred DropEffect".into());
        }
        names.extend(
            crate::capture::MARKERS
                .iter()
                .map(|name| (*name).to_owned()),
        );
        report["written_formats"] = json!(names);
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_text_qualification_uses_the_actual_fixed_conversion_and_rejects_extra_formats() {
        use crate::history::content::{Data, HTML};
        use std::collections::BTreeMap;
        let text = Content::text("合成文字", hash::sha256).unwrap();
        assert!(pure_unicode(&text));
        let rich = Content::new(
            BTreeMap::from([
                (TEXT.into(), text.formats[TEXT].data.clone()),
                (
                    HTML.into(),
                    Data::Bytes(Arc::<[u8]>::from(&b"synthetic"[..])),
                ),
            ]),
            hash::sha256,
        )
        .unwrap();
        assert!(!pure_unicode(&rich));
        let (written, conversion) = as_text::apply(&rich, Mode::Text, None, None, hash::sha256);
        assert_eq!(conversion["conversion"], "plain_text");
        assert!(
            pure_unicode(&written),
            "资格依据转换后的实际格式而非历史 kind"
        );
        let malformed = Content::new(
            BTreeMap::from([(TEXT.into(), Data::Bytes(Arc::<[u8]>::from(&[1u8][..])))]),
            hash::sha256,
        )
        .unwrap();
        assert!(!pure_unicode(&malformed));
    }
    #[test]
    fn requires_all_four_preflight_conditions() {
        for mask in 0..32 {
            let conditions = Checks {
                foreground: mask & 1 != 0,
                focus: mask & 2 != 0,
                modifiers: mask & 4 != 0,
                hidden: mask & 8 != 0,
                permission: mask & 16 != 0,
            };
            assert_eq!(conditions.all(), mask == 31);
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
