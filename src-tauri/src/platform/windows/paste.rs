use super::{clipboard, hash, uia, window};
use crate::{
    history::content::{Content, Data, HTML, RTF, TEXT},
    paste::{
        as_text,
        capture_access::AccessGuard,
        generic,
        keys::{Key, Match, Table},
        native, native_access,
        native_edit::{self, NativeTransport},
        request::{
            Callbacks, Mode, Outcome, Request, RestorePolicy, conclude, generic_outcome,
            native_outcome,
        },
    },
    platform::{REPLAY_WAIT, ReplayKey, Target, key_state},
};
use serde_json::{Value, json};
use std::{
    mem::size_of,
    path::Path,
    sync::{
        Arc, Mutex,
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
    System::{
        DataExchange::{GetClipboardOwner, GetClipboardSequenceNumber, GetOpenClipboardWindow},
        Threading::{
            GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId, OpenProcess,
            OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    },
    UI::Input::KeyboardAndMouse::*,
    UI::WindowsAndMessaging::{
        GetWindowThreadProcessId, IsWindowVisible, SMTO_ABORTIFHUNG, SMTO_BLOCK, SMTO_ERRORONEXIT,
        SendMessageTimeoutW, WM_GETTEXTLENGTH, WM_PASTE,
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
    report["submission_method"] = json!("clipboard_only");
    report["clipboard_publication"] = json!("immediate");
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
// System EDIT messages (winuser.h); no private/async pointer message is used.
const EM_GETSEL: u32 = 0x00b0;
const EM_REPLACESEL: u32 = 0x00c2;
const EM_GETPASSWORDCHAR: u32 = 0x00d2;
const EM_GETLIMITTEXT: u32 = 0x00d5;

#[cfg(test)]
#[path = "native_message_tests.rs"]
mod native_message_tests;

#[cfg(test)]
#[path = "native_edit_isolation_tests.rs"]
mod native_edit_isolation_tests;

#[cfg(test)]
#[path = "current_station_native_clipboard_tests.rs"]
mod current_station_native_clipboard_tests;

#[cfg(test)]
pub(crate) fn submit_native_message(
    control: usize,
    expected_thread: u32,
    expected_process: u32,
    timeout_ms: u32,
) -> native::Delivery {
    submit_native_message_observed(
        control,
        expected_thread,
        expected_process,
        timeout_ms,
        |_| {},
    )
}

fn submit_native_message_observed(
    control: usize,
    expected_thread: u32,
    expected_process: u32,
    timeout_ms: u32,
    mut observe: impl FnMut(bool),
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
    observe(true);
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
    // 必须先取 SMTO 的错误，再查询 holder，防止元数据 API 覆盖证据。
    let error_code = (!acknowledged).then(|| unsafe { GetLastError().0 });
    observe(false);
    native::Delivery {
        attempted: true,
        acknowledged,
        error_code,
    }
}

fn submit_native_replacement(
    target: &Target,
    text: &native_edit::Text,
    timeout_ms: u32,
) -> native::Delivery {
    let mut process = 0;
    let thread =
        unsafe { GetWindowThreadProcessId(window::hwnd(target.focus), Some(&mut process)) };
    if thread == 0
        || thread != target.thread
        || process != target.process
        || thread == unsafe { GetCurrentThreadId() }
        || process == unsafe { GetCurrentProcessId() }
    {
        return native::Delivery {
            attempted: false,
            acknowledged: false,
            error_code: None,
        };
    }
    // Cross-process system-message marshalling owns the receiver copy through WndProc.
    // The fixed UTF-16 buffer stays alive through this call, including timeout/early Reply.
    // Same-process cross-thread pointer messages are explicitly excluded above.
    let acknowledged = unsafe {
        SetLastError(WIN32_ERROR(0));
        SendMessageTimeoutW(
            window::hwnd(target.focus),
            EM_REPLACESEL,
            WPARAM(1), // Allow undo; do not clear the user's undo buffer or alter the selection.
            LPARAM(text.terminated().as_ptr() as isize),
            SMTO_ABORTIFHUNG | SMTO_BLOCK | SMTO_ERRORONEXIT,
            timeout_ms,
            None,
        )
        .0 != 0
    };
    let error_code = (!acknowledged).then(|| unsafe { GetLastError().0 });
    // EM_REPLACESEL has no content-success return value. Zero can still complete later.
    native::Delivery {
        attempted: true,
        acknowledged,
        error_code,
    }
}

fn open_holder(target: u32) -> Value {
    let window = unsafe { GetOpenClipboardWindow() }.unwrap_or_default();
    let mut process = 0;
    if !window.is_invalid() {
        unsafe { GetWindowThreadProcessId(window, Some(&mut process)) };
    }
    json!({"window_observed":!window.is_invalid(),
        "classification":native::open_holder(!window.is_invalid(), process, unsafe { GetCurrentProcessId() }, target)})
}

fn native_text_formats(content: &Content) -> bool {
    content.formats.iter().all(|(name, format)| {
        matches!(name.as_str(), TEXT | HTML | RTF) && matches!(&format.data, Data::Bytes(_))
    }) && content.raw(TEXT).is_some_and(|bytes| {
        bytes.len() % 2 == 0
            && String::from_utf16(
                &bytes
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect::<Vec<_>>(),
            )
            .is_ok()
    })
}

pub(crate) fn native_candidate(
    request: &Request,
    written: &Content,
    builtin_default_ctrl_v: bool,
) -> bool {
    native::eligible(native::Eligibility {
        selection: request
            .permit
            .as_ref()
            .is_some_and(|permit| permit.search()),
        copy: request.mode == Mode::Copy,
        ordinary: !request.external_switch && request.copy_only_reason.is_none(),
        control_class: &request.target.focus_class,
        supported_text_formats: native_text_formats(written),
        builtin_default_ctrl_v,
    })
}

struct Native<'a> {
    access: Option<AccessGuard>,
    publication: Option<native_access::Publication>,
    publication_error: Option<&'static str>,
    clock: Instant,
    budget: Option<native_access::Budget>,
    query_budget: Option<native_edit::QueryBudget>,
    transport: Option<NativeTransport>,
    request: &'a Request,
    written: &'a Arc<Content>,
    clipboard: &'a clipboard::Service,
    stopping: &'a AtomicBool,
    restore: &'a dyn Fn(RestorePolicy) -> Option<crate::platform::FocusResult>,
    on_sent: &'a dyn Fn(),
    synchronize: &'a dyn Fn() -> bool,
    report: &'a mut Value,
}
impl Native<'_> {
    fn edit_style(&self) -> Result<native_edit::Style, &'static str> {
        window::native_edit_style(&self.request.target)
            .map(native_edit::Style::from_bits)
            .ok_or("invalid_target")
    }
    fn query_remaining(&self) -> Result<u32, &'static str> {
        self.query_budget
            .ok_or(native_edit::QUERY_FAILED)?
            .remaining_ms(self.clock.elapsed())
    }
    fn edit_query(&mut self, message: u32) -> Result<usize, &'static str> {
        self.query_budget
            .get_or_insert_with(|| native_edit::QueryBudget::new(self.clock.elapsed()));
        native::guarded(self)?;
        let timeout = self.query_remaining()?;
        let mut value = 0;
        // Scalar system messages only: no cross-process output pointer or target text body.
        let completed = unsafe {
            SetLastError(WIN32_ERROR(0));
            SendMessageTimeoutW(
                window::hwnd(self.request.target.focus),
                message,
                WPARAM(0),
                LPARAM(0),
                SMTO_ABORTIFHUNG | SMTO_BLOCK | SMTO_ERRORONEXIT,
                timeout,
                Some(&mut value),
            )
            .0 != 0
        };
        if cancelled(self.request, self.stopping) {
            return Err("request_cancelled");
        }
        self.query_remaining()?;
        if completed {
            Ok(value)
        } else {
            Err(native_edit::QUERY_FAILED)
        }
    }
    fn edit_snapshot(
        &mut self,
        style: native_edit::Style,
        length: usize,
        password: usize,
    ) -> Result<native_edit::Snapshot, &'static str> {
        let selection = self.edit_query(EM_GETSEL)?;
        let limit = self.edit_query(EM_GETLIMITTEXT)?;
        NativeTransport::validate_style(style, self.edit_style()?)?;
        native_edit::Snapshot::from_queries(style, length, selection, limit, password)
    }
    fn publication_stable(&mut self) -> Result<(), &'static str> {
        native::guarded(self)?;
        let matched = self.publication.is_some_and(|publication| {
            publication.matches(
                || unsafe { GetClipboardSequenceNumber() },
                || unsafe { GetClipboardOwner() }.unwrap_or_default().0 as usize,
            )
        });
        if cancelled(self.request, self.stopping) {
            return Err("request_cancelled");
        }
        if matched {
            Ok(())
        } else {
            Err(self.publication_error.unwrap_or(native_access::CHANGED))
        }
    }
    fn choose_transport(&mut self) -> Result<NativeTransport, &'static str> {
        let style = self.edit_style()?;
        let text = native_edit::prepare(self.written.raw(TEXT).ok_or("native_edit_invalid_text")?)?;
        let Some(text) = text else {
            return NativeTransport::wm_paste(style);
        };
        if !style.compatible(&text)
            || self.request.target.process == unsafe { GetCurrentProcessId() }
        {
            return NativeTransport::wm_paste(style);
        }
        let password = self.edit_query(EM_GETPASSWORDCHAR)?;
        NativeTransport::validate_style(style, self.edit_style()?)?;
        // Password masking without ES_PASSWORD is still a known old-WM compatibility case.
        if password > u16::MAX as usize {
            return Err(native_edit::QUERY_FAILED);
        }
        if password != 0 {
            return NativeTransport::wm_paste(style);
        }
        let length = self.edit_query(WM_GETTEXTLENGTH)?;
        if native_edit::legacy_length(length)? {
            NativeTransport::validate_style(style, self.edit_style()?)?;
            return NativeTransport::wm_paste(style);
        }
        let snapshot = self.edit_snapshot(style, length, password)?;
        NativeTransport::choose(text, snapshot)
    }
    fn submit_replacement(
        &mut self,
        text: native_edit::Text,
        snapshot: native_edit::Snapshot,
    ) -> Result<native::Submission, &'static str> {
        let style = self.edit_style()?;
        let password = self.edit_query(EM_GETPASSWORDCHAR)?;
        let length = self.edit_query(WM_GETTEXTLENGTH)?;
        if native_edit::legacy_length(length)? {
            return Err(native_edit::STATE_CHANGED);
        }
        let observed = self.edit_snapshot(style, length, password)?;
        NativeTransport::validate_snapshot(snapshot, observed)?;
        self.publication_stable()?;
        let timeout = self.query_remaining()?;
        let sent_at = Instant::now();
        let delivery = submit_native_replacement(&self.request.target, &text, timeout);
        self.report["native_publication"] =
            json!({"stage":"final","status":"matched","metadata_only":true});
        self.report["native_submission"] = json!({"attempted":delivery.attempted,"acknowledged":delivery.acknowledged,
            "status":if !delivery.attempted {"not_submitted"} else if delivery.acknowledged {"sent"} else {"unknown"},
            "timeout_ms":timeout,"error_code":delivery.error_code,"content_confirmed":false});
        if delivery.attempted {
            self.report["selected_to_send_ms"] =
                json!(sent_at.duration_since(self.request.started).as_secs_f64() * 1000.0);
        }
        Ok(delivery.submission())
    }
    fn report_availability(&mut self, stage: &'static str, result: Result<(), &'static str>) {
        if let Some(budget) = self.budget.as_ref() {
            self.report["native_availability"] = json!({
                "stage":stage,"status":result.err().unwrap_or("ready"),
                "probes":budget.probes(),"waits":budget.waits(),
                "elapsed_ms":budget.observed_elapsed().as_secs_f64()*1000.0,
                "budget_ms":native_access::BUDGET.as_millis() as u64,
            });
        }
    }
    fn available(&mut self, stage: &'static str) -> Result<(), &'static str> {
        let mut budget = self
            .budget
            .take()
            .unwrap_or_else(|| native_access::Budget::new(self.clock.elapsed()));
        let publication = self.publication;
        let result = budget.wait(self, publication).map_err(|reason| {
            if reason == native_access::CHANGED {
                self.publication_error.unwrap_or(reason)
            } else {
                reason
            }
        });
        self.budget = Some(budget);
        // 末次探针成功后不在唯一发送之前构造日志；失败或准备阶段可立即记录。
        if stage != "final" || result.is_err() {
            self.report_availability(stage, result);
        }
        result
    }
}
impl native_access::Environment for Native<'_> {
    fn now(&self) -> Duration {
        self.clock.elapsed()
    }
    fn pause(&mut self, duration: Duration) {
        thread::sleep(duration);
    }
    fn guard(&mut self) -> Result<(), &'static str> {
        native::guarded(self)
    }
    fn probe(&mut self, publication: native_access::Publication) -> native_access::Probe {
        clipboard::probe_native(publication)
    }
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
        let observation = self.clipboard.write(
            record(self.request),
            self.written.clone(),
            self.request.target.process,
            false,
        )?;
        self.report["clipboard_written"] = json!(true);
        if let Ok(observation) = observation.lock() {
            self.publication = observation.publication;
            self.publication_error = observation.publication_error;
        }
        Ok(())
    }
    fn prepare_submit(&mut self) -> Result<(), &'static str> {
        if self.transport.is_some() {
            return Err("native_transport_already_selected");
        }
        let transport = self.choose_transport()?;
        self.report["submission_method"] = json!(transport.method());
        let direct = matches!(transport, NativeTransport::ReplaceSel { .. });
        self.transport = Some(transport); // Frozen once, before any target mutation.
        if direct {
            self.publication_stable()?;
            self.report["native_publication"] =
                json!({"stage":"prepare","status":"matched","metadata_only":true});
            Ok(())
        } else {
            self.available("prepare")
        }
    }
    fn submit(&mut self) -> Result<native::Submission, &'static str> {
        match self
            .transport
            .take()
            .ok_or("native_transport_not_selected")?
        {
            NativeTransport::ReplaceSel { text, snapshot } => {
                return self.submit_replacement(text, snapshot);
            }
            NativeTransport::WmPaste { style } => {
                NativeTransport::validate_style(style, self.edit_style()?)?
            }
        }
        let mut holders = [Value::Null, Value::Null];
        // 诊断查询在末次实际探针之前；它不以 NULL 或持有者分类代替可用性判断。
        holders[0] = open_holder(self.request.target.process);
        self.available("final")?;
        if cancelled(self.request, self.stopping) {
            return Err("request_cancelled");
        }
        let sent_at = Instant::now();
        // 返回值只证明消息调用完成，绝不读取 WM_PASTE 的结果值推断内容成功。
        // 最后 Open/Close 与目标自行打开之间仍有跨进程 TOCTOU，不能宣称原子交付。
        let delivery = submit_native_message_observed(
            self.request.target.focus,
            self.request.target.thread,
            self.request.target.process,
            NATIVE_TIMEOUT_MS,
            |before| {
                if !before {
                    holders[1] = open_holder(self.request.target.process);
                }
            },
        );
        self.report_availability("final", Ok(()));
        self.report["native_submission"] = json!({"attempted":delivery.attempted,"acknowledged":delivery.acknowledged,
            "status":if !delivery.attempted {"not_submitted"} else if delivery.acknowledged {"sent"} else {"unknown"},
            "timeout_ms":NATIVE_TIMEOUT_MS,"error_code":delivery.error_code,
            "open_holder_before":holders[0],"open_holder_before_stage":"before_final_probe",
            "open_holder_after":holders[1]});
        if delivery.attempted {
            self.report["selected_to_send_ms"] =
                json!(sent_at.duration_since(self.request.started).as_secs_f64() * 1000.0);
        }
        Ok(delivery.submission())
    }
    fn finish(&mut self, result: Option<native::Completion>) {
        // UI 归还可跨线程等待；先关闭本次 reader 排斥区间。
        drop(self.access.take());
        if !matches!(result, Some(native::Completion::Submitted(_))) {
            if let Some(native::Completion::NotSubmitted(reason)) = result {
                self.report_availability("finished", Err(reason));
            }
            self.report["native_cleanup"] = json!({"skipped":"not_submitted"});
            return;
        }
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

struct Generic<'a> {
    access: Option<AccessGuard>,
    publication: Option<native_access::Publication>,
    publication_error: Option<&'static str>,
    clock: Instant,
    budget: Option<native_access::Budget>,
    request: &'a Request,
    written: &'a Arc<Content>,
    clipboard: &'a clipboard::Service,
    stopping: &'a AtomicBool,
    on_sent: &'a dyn Fn(),
    restore: &'a dyn Fn(RestorePolicy) -> Option<crate::platform::FocusResult>,
    synchronize: &'a dyn Fn() -> bool,
    report: &'a mut Value,
    inputs: Vec<INPUT>,
    observation: Option<Arc<Mutex<clipboard::Observation>>>,
    probe: Option<uia::Probe>,
    before: uia::Evidence,
    sent_at: Option<Instant>,
}

impl Generic<'_> {
    fn synchronize(&self) -> Result<(), &'static str> {
        if cancelled(self.request, self.stopping) {
            return Err("request_cancelled");
        }
        if !(self.synchronize)() {
            if let Some(permit) = self.request.permit.as_ref() {
                permit.revoke();
            }
            return Err("selection_boundary_changed");
        }
        Ok(())
    }

    fn boundary(&mut self) -> Result<(), &'static str> {
        self.synchronize()?;
        let integrity = compare_integrity(&self.request.target);
        let allowed = integrity["status"] == "allowed";
        self.report["integrity"] = integrity;
        let input_desktop = native::input_desktop_ready(
            window::thread_input_desktop_receiving_input().ok(),
            || {
                if let Some(permit) = self.request.permit.as_ref() {
                    permit.revoke();
                }
            },
        );
        if cancelled(self.request, self.stopping) {
            Err("request_cancelled")
        } else if !input_desktop {
            Err("generic_input_desktop_unavailable")
        } else if !window::valid(&self.request.target) {
            Err("invalid_target")
        } else if !self.request.permit.as_ref().is_some_and(|permit| {
            permit.check(
                self.request.session,
                self.request.record_id,
                self.request.mode,
                &self.request.target,
                window::foreground(),
            )
        }) {
            Err("selection_boundary_changed")
        } else if !allowed {
            Err("target_may_be_elevated")
        } else {
            Ok(())
        }
    }

    fn guard_submission(&mut self) -> Result<(), &'static str> {
        self.boundary()?;
        self.final_input_guard()
    }

    fn final_input_guard(&self) -> Result<(), &'static str> {
        let input_desktop = native::input_desktop_ready(
            window::thread_input_desktop_receiving_input().ok(),
            || {
                if let Some(permit) = self.request.permit.as_ref() {
                    permit.revoke();
                }
            },
        );
        let current = checks(self.request);
        if cancelled(self.request, self.stopping) {
            Err("request_cancelled")
        } else if !input_desktop {
            Err("generic_input_desktop_unavailable")
        } else if !current.permission {
            Err("selection_boundary_changed")
        } else if !current.foreground || !current.focus || !current.hidden {
            Err("conditions_changed")
        } else if !current.modifiers {
            Err("generic_modifiers_held")
        } else {
            Ok(())
        }
    }

    fn acquire(&mut self, stage: &'static str) -> Result<(), &'static str> {
        let started = Instant::now();
        let access = self
            .clipboard
            .capture_access()
            .native(native_access::BUDGET, || {
                cancelled(self.request, self.stopping)
            });
        self.report["generic_coordination"][stage] = json!({
            "outcome":access.as_ref().map_or_else(|error| error.outcome(), |_| "acquired"),
            "wait_ms":started.elapsed().as_secs_f64()*1000.0,
            "budget_ms":native_access::BUDGET.as_millis() as u64,
        });
        self.access = Some(access.map_err(|error| error.reason())?);
        Ok(())
    }

    fn report_availability(&mut self, result: Result<(), &'static str>) {
        if let Some(budget) = self.budget.as_ref() {
            self.report["generic_availability"] = json!({
                "status":result.err().unwrap_or("ready"),
                "probes":budget.probes(),"waits":budget.waits(),
                "elapsed_ms":budget.observed_elapsed().as_secs_f64()*1000.0,
                "budget_ms":native_access::BUDGET.as_millis() as u64,
            });
        }
    }
}

impl native_access::Environment for Generic<'_> {
    fn now(&self) -> Duration {
        self.clock.elapsed()
    }
    fn pause(&mut self, duration: Duration) {
        thread::sleep(duration);
    }
    fn guard(&mut self) -> Result<(), &'static str> {
        self.guard_submission()
    }
    fn probe(&mut self, publication: native_access::Publication) -> native_access::Probe {
        clipboard::probe_native(publication)
    }
}

impl generic::Backend for Generic<'_> {
    fn coordinate(&mut self) -> Result<(), String> {
        self.acquire("publish").map_err(str::to_owned)
    }
    fn check_before_publish(&mut self) -> Result<(), &'static str> {
        self.boundary()
    }
    fn publish(&mut self) -> Result<(), String> {
        let observation = self.clipboard.write(
            record(self.request),
            self.written.clone(),
            self.request.target.process,
            false,
        )?;
        self.report["clipboard_written"] = json!(true);
        {
            let observed = observation
                .lock()
                .map_err(|_| "clipboard_observation_failed")?;
            self.publication = observed.publication;
            self.publication_error = observed.publication_error;
        }
        self.observation = Some(observation);
        Ok(())
    }
    fn restore(&mut self) -> Result<(), &'static str> {
        // 跨主线程等待回执前先释放本进程 reader 协调；归还后重获，再核对原 stamp。
        drop(self.access.take());
        let focus = (self.restore)(RestorePolicy::GenericBeforeInput);
        if let Some(focus) = focus {
            self.report["restore"] = json!(focus);
            Ok(())
        } else {
            Err("generic_restore_unavailable")
        }
    }
    fn prepare_submit(&mut self) -> Result<(), &'static str> {
        self.boundary()?;
        let (ready, confirmation) = preflight(self.request, self.stopping);
        self.report["preflight"] = confirmation;
        if cancelled(self.request, self.stopping) {
            return Err("request_cancelled");
        }
        if !ready {
            return Err("preflight_timeout");
        }
        // UIA 的基线读取依赖当前目标焦点，只能在归还后开始。
        self.probe = self
            .written
            .text_value()
            .map(|text| uia::Probe::start(self.request.target.process, text));
        self.before = self
            .probe
            .as_ref()
            .map(|probe| probe.baseline())
            .unwrap_or_default();
        self.report["layer3"] = json!(self.before);
        Ok(())
    }
    fn available(&mut self) -> Result<(), &'static str> {
        self.acquire("submit")?;
        // 协调获取、UI 归还、既有松键等待各自计时，不消耗此 200ms 探针预算。
        let mut budget = native_access::Budget::new(self.clock.elapsed());
        let publication = self.publication;
        let result = budget.wait(self, publication).map_err(|reason| {
            generic::availability_reason(if reason == native_access::CHANGED {
                self.publication_error.unwrap_or(reason)
            } else {
                reason
            })
        });
        self.budget = Some(budget);
        // 成功时日志延至唯一一次 SendInput 后，缩短最终探针与发键间隔。
        if result.is_err() {
            self.report_availability(result);
        }
        result
    }
    fn send(&mut self) -> Result<generic::Submission, &'static str> {
        // 探针返回后仅做轻量输入复核；不在此再等待 UI、查 token 或构造日志。
        self.final_input_guard()?;
        let sent_at = Instant::now();
        self.sent_at = Some(sent_at);
        if cancelled(self.request, self.stopping) {
            return Err("request_cancelled");
        }
        let sent = send(&self.inputs);
        drop(self.access.take());
        self.report_availability(Ok(()));
        self.report["send_input"] = json!(sent);
        self.report["expected_events"] = json!(self.inputs.len());
        self.report["selected_to_send_ms"] =
            json!(sent_at.duration_since(self.request.started).as_secs_f64() * 1000.0);
        if sent != self.inputs.len() as u32 {
            // 只补齐实际成功按下且尚未抬起的键，不能再发整套快捷键。
            let cleanup = cleanup_events(&self.inputs, sent);
            if !cleanup.is_empty() {
                self.report["cleanup_send_input"] = json!(send(&cleanup));
            }
            return Ok(generic::Submission::from_input_count(
                sent,
                self.inputs.len(),
            ));
        }
        (self.on_sent)();
        Ok(generic::Submission::Sent)
    }
    fn observe(&mut self) -> bool {
        let sent_at = self.sent_at.expect("发送后收集证据");
        let before = std::mem::take(&mut self.before);
        let layer3 = if let Some(probe) = self.probe.take() {
            probe.sent(sent_at);
            probe.finish(before, sent_at)
        } else {
            before
        };
        let confirmed = layer3.conclusion == "confirmed";
        self.report["layer3"] = json!(layer3);
        confirmed
    }
    fn finish(&mut self, _: Option<generic::Completion>) {
        drop(self.access.take());
        if let Some(observation) = self.observation.as_ref()
            && let Ok(observation) = observation.lock()
        {
            // 即时发布不能观测首读者，不等待一秒，也不把零观测记为 timeout。
            self.report["layer2"] = observation.report(self.sent_at.unwrap_or_else(Instant::now));
        }
    }
}

fn cleanup_events(inputs: &[INPUT], sent: u32) -> Vec<INPUT> {
    let mut held_keys = Vec::new();
    for event in inputs.iter().take(sent as usize) {
        let key = unsafe { event.Anonymous.ki };
        if key.dwFlags.contains(KEYEVENTF_KEYUP) {
            held_keys.retain(|&(vk, _)| vk != key.wVk.0);
        } else {
            held_keys.push((key.wVk.0, key.dwFlags));
        }
    }
    held_keys
        .into_iter()
        .rev()
        .map(|(vk, flags)| input(vk, 0, flags | KEYEVENTF_KEYUP))
        .collect()
}

// written 是实际写进剪贴板的内容：降级成文字时和 request.content（历史里的记录）不同。
fn perform(
    request: &Request,
    written: &Arc<Content>,
    selected: Match,
    clipboard: &clipboard::Service,
    stopping: &AtomicBool,
    callbacks: Callbacks<'_>,
    report: &mut Value,
) -> Result<Outcome, String> {
    if cancelled(request, stopping) {
        return Err("request_cancelled".into());
    }
    let copy_only = |report: &mut Value, reason| {
        if let Some(focus) = (callbacks.restore)(RestorePolicy::LegacyBeforeSubmit) {
            report["restore"] = json!(focus);
        }
        copy(request, written, clipboard, report, reason)
    };
    let external_switch = request.external_switch;
    if request.mode == Mode::Copy {
        return copy_only(report, "user_copy");
    }
    if !window::valid(&request.target) {
        return copy_only(report, "invalid_target");
    }
    if external_switch {
        return copy_only(report, "external_switch");
    }
    if let Some(reason) = request.copy_only_reason {
        return copy_only(report, reason);
    }
    if !request.permit.as_ref().is_some_and(|permit| {
        permit.check(
            request.session,
            request.record_id,
            request.mode,
            &request.target,
            window::foreground(),
        )
    }) {
        return copy_only(report, "selection_boundary_changed");
    }
    let integrity = compare_integrity(&request.target);
    let allowed = integrity["status"] == "allowed";
    report["integrity"] = integrity;
    if !allowed {
        return copy_only(report, "target_may_be_elevated");
    }
    let result = {
        let mut backend = Generic {
            access: None,
            publication: None,
            publication_error: None,
            clock: Instant::now(),
            budget: None,
            request,
            written,
            clipboard,
            stopping,
            on_sent: callbacks.on_sent,
            restore: callbacks.restore,
            synchronize: callbacks.synchronize,
            report,
            inputs: sequence(selected.key),
            observation: None,
            probe: None,
            before: uia::Evidence::default(),
            sent_at: None,
        };
        generic::run(&mut backend)
    };
    if report["send_input"].as_u64().is_some_and(|sent| sent > 0) {
        report["actual_key"] = json!(selected.key);
    }
    Ok(generic_outcome(result, report))
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
        "submission_method":"send_input","clipboard_publication":"immediate",
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
    let candidate = native_candidate(
        &request,
        &written,
        table.is_builtin_default_ctrl_v(selected),
    );
    let ordinary = candidate && compare_integrity(&request.target)["native_ordinary"] == true;
    let outcome = if ordinary {
        report["submission_method"] = json!("native_pending");
        report["native_submission"] =
            json!({"attempted":false,"acknowledged":false,"status":"not_submitted"});
        let access_started = Instant::now();
        let access = clipboard
            .capture_access()
            .native(Duration::from_millis(u64::from(NATIVE_TIMEOUT_MS)), || {
                cancelled(&request, stopping)
            });
        report["native_coordination"] = json!({
            "outcome":access.as_ref().map_or_else(|error| error.outcome(), |_| "acquired"),
            "wait_ms":access_started.elapsed().as_secs_f64()*1000.0,"budget_ms":NATIVE_TIMEOUT_MS,
        });
        let result = access
            .map_err(|error| error.reason().to_owned())
            .and_then(|access| {
                let mut backend = Native {
                    access: Some(access),
                    publication: None,
                    publication_error: None,
                    clock: Instant::now(),
                    budget: None,
                    query_budget: None,
                    transport: None,
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
            });
        Ok(native_outcome(result, &mut report))
    } else {
        perform(
            &request,
            &written,
            selected,
            clipboard,
            stopping,
            callbacks,
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

    fn native_multiformat_request(content: Arc<Content>) -> Request {
        native_multiformat_request_mode(content, Mode::Paste)
    }

    fn native_multiformat_request_mode(content: Arc<Content>, mode: Mode) -> Request {
        use crate::paste::permit::{Monitor, Selection, Session};
        let target = Target {
            window: 20,
            focus: 21,
            thread: 2,
            process: 3,
            focus_class: "Edit".into(),
            ..Target::default()
        };
        let monitor = Arc::new(Monitor::new(10, 20));
        monitor.started();
        let mut session = Session::new(7, target.clone(), monitor);
        let permit = session
            .select(
                7,
                9,
                mode,
                Selection {
                    revision: 0,
                    nonce: 1,
                },
                10,
            )
            .unwrap();
        Request {
            session: 7,
            target,
            panel: 10,
            record_id: 9,
            content,
            mode,
            started: Instant::now(),
            external_switch: false,
            permit: Some(permit),
            copy_only_reason: None,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn native_multiformat_candidate_accepts_materialized_html_and_rtf_with_unicode_text() {
        use crate::history::content::{Data, HTML, RTF};
        use std::collections::BTreeMap;
        let text = Content::text("合成多格式文字", hash::sha256).unwrap();
        for companions in [vec![HTML], vec![RTF], vec![HTML, RTF]] {
            let mut formats = BTreeMap::from([(TEXT.into(), text.formats[TEXT].data.clone())]);
            for name in companions {
                formats.insert(
                    name.into(),
                    Data::Bytes(Arc::from(&b"synthetic-companion\0"[..])),
                );
            }
            let content = Content::new(formats, hash::sha256).unwrap();
            let request = native_multiformat_request(content.clone());
            assert!(
                native_candidate(&request, &content, true),
                "普通标准Edit的合法文字及HTML/RTF伴随格式应在初始资格阶段进入native"
            );
        }
    }

    #[test]
    fn native_multiformat_candidate_rejects_unsupported_or_unmaterialized_formats_and_invalid_text()
    {
        use crate::history::content::{FILES, PNG};
        use std::collections::BTreeMap;
        let text = Content::text("合成文字", hash::sha256).unwrap();
        let bytes = |value: &'static [u8]| Data::Bytes(Arc::from(value));
        let paired = |name: &str, data: Data| {
            BTreeMap::from([
                (TEXT.into(), text.formats[TEXT].data.clone()),
                (name.into(), data),
            ])
        };
        for formats in [
            BTreeMap::new(),
            BTreeMap::from([(HTML.into(), bytes(b"synthetic-html"))]),
            BTreeMap::from([(RTF.into(), bytes(b"synthetic-rtf"))]),
            paired(PNG, bytes(b"synthetic-png")),
            paired(
                FILES,
                Data::Files(vec!["C:\\synthetic.txt".to_owned()].into()),
            ),
            paired("Unknown Format", bytes(b"synthetic-unknown")),
            paired(
                HTML,
                Data::Files(vec!["C:\\not-materialized-html".to_owned()].into()),
            ),
            paired(
                RTF,
                Data::Files(vec!["C:\\not-materialized-rtf".to_owned()].into()),
            ),
            BTreeMap::from([(TEXT.into(), bytes(&[1]))]),
            BTreeMap::from([(TEXT.into(), bytes(&[0x00, 0xd8]))]),
            BTreeMap::from([(TEXT.into(), bytes(&[0, 0, 0x00, 0xd8]))]),
        ] {
            let content = Content::new(formats, hash::sha256).unwrap();
            let request = native_multiformat_request(content.clone());
            assert!(!native_candidate(&request, &content, true));
        }
    }

    #[test]
    fn native_multiformat_candidate_keeps_target_selection_and_rule_limits() {
        use std::collections::BTreeMap;
        let text = Content::text("合成文字", hash::sha256).unwrap();
        let content = Content::new(
            BTreeMap::from([
                (TEXT.into(), text.formats[TEXT].data.clone()),
                (HTML.into(), Data::Bytes(Arc::from(&b"synthetic-html"[..]))),
            ]),
            hash::sha256,
        )
        .unwrap();
        assert!(!native_candidate(
            &native_multiformat_request(content.clone()),
            &content,
            false
        ));
        for excluded in [
            "copy",
            "external",
            "copy_only",
            "quick",
            "RichEdit20W",
            "Chrome_RenderWidgetHostHWND",
        ] {
            let mut request = native_multiformat_request(content.clone());
            match excluded {
                "copy" => request.mode = Mode::Copy,
                "external" => request.external_switch = true,
                "copy_only" => request.copy_only_reason = Some("target_control_unverified"),
                "quick" => {
                    use crate::paste::permit::{Monitor, Session};
                    let monitor = Arc::new(Monitor::new(10, 20));
                    monitor.started();
                    request.permit = Some(
                        Session::new(7, request.target.clone(), monitor).quick(9, Mode::Paste),
                    );
                }
                class => request.target.focus_class = class.into(),
            }
            assert!(!native_candidate(&request, &content, true), "{excluded}");
        }
    }
    #[test]
    fn generic_partial_input_cleanup_only_releases_successful_held_keys() {
        for (key, prefixes) in [
            (
                Key::CtrlV,
                vec![vec![], vec![0x11], vec![0x56, 0x11], vec![0x11], vec![]],
            ),
            (
                Key::CtrlShiftV,
                vec![
                    vec![],
                    vec![0x11],
                    vec![0x10, 0x11],
                    vec![0x56, 0x10, 0x11],
                    vec![0x10, 0x11],
                    vec![0x11],
                    vec![],
                ],
            ),
            (
                Key::ShiftInsert,
                vec![vec![], vec![0x10], vec![0x2D, 0x10], vec![0x10], vec![]],
            ),
        ] {
            let inputs = sequence(key);
            for (sent, expected) in prefixes.into_iter().enumerate() {
                let cleanup = cleanup_events(&inputs, sent as u32);
                let actual: Vec<_> = cleanup
                    .iter()
                    .map(|event| unsafe { event.Anonymous.ki.wVk.0 })
                    .collect();
                assert_eq!(actual, expected);
                for event in cleanup {
                    let key = unsafe { event.Anonymous.ki };
                    assert!(key.dwFlags.contains(KEYEVENTF_KEYUP));
                    assert_eq!(key.dwExtraInfo, INPUT_MARKER);
                    assert_eq!(
                        key.dwFlags.contains(KEYEVENTF_EXTENDEDKEY),
                        key.wVk.0 == 0x2D
                    );
                }
            }
        }
    }
    #[test]
    fn native_text_qualification_uses_the_actual_fixed_conversion() {
        use crate::history::content::{Data, HTML};
        use std::collections::BTreeMap;
        let text = Content::text("合成文字", hash::sha256).unwrap();
        assert!(native_text_formats(&text));
        let rich = Content::new(
            BTreeMap::from([
                (TEXT.into(), text.formats[TEXT].data.clone()),
                (
                    HTML.into(),
                    Data::Bytes(Arc::<[u8]>::from(&b"synthetic"[..])),
                ),
                (
                    "Unknown Format".into(),
                    Data::Bytes(Arc::from(&b"synthetic-extra"[..])),
                ),
            ]),
            hash::sha256,
        )
        .unwrap();
        assert!(!native_text_formats(&rich));
        let (written, conversion) = as_text::apply(&rich, Mode::Text, None, None, hash::sha256);
        assert_eq!(conversion["conversion"], "plain_text");
        assert!(
            native_text_formats(&written),
            "资格依据转换后的实际格式而非历史 kind"
        );
        let request = native_multiformat_request_mode(rich, Mode::Text);
        assert!(
            native_candidate(&request, &written, true),
            "不能改按request.content原格式分流"
        );
        let malformed = Content::new(
            BTreeMap::from([(TEXT.into(), Data::Bytes(Arc::<[u8]>::from(&[1u8][..])))]),
            hash::sha256,
        )
        .unwrap();
        assert!(!native_text_formats(&malformed));
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
