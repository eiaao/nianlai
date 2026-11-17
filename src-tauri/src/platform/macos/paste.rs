use super::{ax, clipboard, window};
use crate::{
    history::content::Content,
    paste::{
        as_text::{self, Separator},
        generic::{self, Backend, Completion, Submission},
        keys::Table,
        outcome::Outcome,
        queue::{Stamp, Stop},
        request::{self, Callbacks, MODIFIER_GRACE, Mode, Request, RestorePolicy},
    },
    platform::{ReplayKey, hash::sha256, key_state},
};
use core_graphics::{
    event::{CGEvent, CGEventFlags, CGEventTapLocation},
    event_source::{CGEventSource, CGEventSourceStateID},
};
use objc2::MainThreadMarker;
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tauri::AppHandle;

unsafe extern "C" {
    fn CGEventSourceFlagsState(state: CGEventSourceStateID) -> CGEventFlags;
}

// Carbon 按当前布局的 Command 层找 V；Dvorak-QWERTY 的快捷键层也由系统处理。
#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISCopyCurrentKeyboardLayoutInputSource() -> *const std::ffi::c_void;
    fn TISGetInputSourceProperty(
        source: *const std::ffi::c_void,
        key: *const std::ffi::c_void,
    ) -> *const std::ffi::c_void;
    static kTISPropertyUnicodeKeyLayoutData: *const std::ffi::c_void;
    fn LMGetKbdType() -> u8;
    fn UCKeyTranslate(
        layout: *const u8,
        key: u16,
        action: u16,
        modifiers: u32,
        keyboard: u32,
        options: u32,
        dead: *mut u32,
        capacity: std::ffi::c_ulong,
        length: *mut std::ffi::c_ulong,
        output: *mut u16,
    ) -> i32;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDataGetBytePtr(data: *const std::ffi::c_void) -> *const u8;
    fn CFRelease(value: *const std::ffi::c_void);
}
fn paste_key(app: &AppHandle) -> u16 {
    if let Some(main) = MainThreadMarker::new() {
        return command_paste_key(main);
    }
    // Carbon 只能在主线程读取布局；读取或调度失败都回退物理 V，迟到结果丢弃。
    let (reply, receiver) = mpsc::sync_channel(1);
    if app
        .run_on_main_thread(move || {
            let key = MainThreadMarker::new().map_or(9, command_paste_key);
            let _ = reply.send(key);
        })
        .is_err()
    {
        return 9;
    }
    receiver
        .recv_timeout(crate::platform::REPLAY_WAIT)
        .unwrap_or(9)
}

fn command_paste_key(_: MainThreadMarker) -> u16 {
    unsafe {
        let source = TISCopyCurrentKeyboardLayoutInputSource();
        if source.is_null() {
            return 9;
        }
        let data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
        let layout = (!data.is_null()).then(|| CFDataGetBytePtr(data));
        let key = layout
            .filter(|layout| !layout.is_null())
            .and_then(|layout| {
                (0..128).find(|key| {
                    let mut dead = 0;
                    let mut length = 0;
                    let mut output = [0; 4];
                    UCKeyTranslate(
                        layout,
                        *key,
                        0,
                        1,
                        u32::from(LMGetKbdType()),
                        1,
                        &mut dead,
                        4,
                        &mut length,
                        output.as_mut_ptr(),
                    ) == 0
                        && length == 1
                        && matches!(output[0], 0x56 | 0x76)
                })
            });
        CFRelease(source);
        key.unwrap_or(9)
    }
}

fn modifiers_released() -> bool {
    let flags = unsafe { CGEventSourceFlagsState(CGEventSourceStateID::HIDSystemState) };
    !flags.intersects(
        CGEventFlags::CGEventFlagCommand
            | CGEventFlags::CGEventFlagControl
            | CGEventFlags::CGEventFlagAlternate
            | CGEventFlags::CGEventFlagShift
            | CGEventFlags::CGEventFlagSecondaryFn,
    )
}

// macOS 没有 Windows 原生编辑控件直投，这里不需要。
#[allow(dead_code)]
pub fn native_candidate(_: &Request, _: &Content, _: bool) -> bool {
    false
}
// macOS 没有 Alt 菜单状态，这里不需要。
pub fn cancel_alt_menu() -> u32 {
    0
}
// macOS 没有吞首键，这里不需要。
pub fn replay(_: &[ReplayKey], _: usize, _: Instant) -> usize {
    0
}
pub fn keys_released() -> bool {
    true
}
pub fn key_state_counts() -> key_state::Counts {
    key_state::Counts::default()
}
pub fn execute(
    app: &AppHandle,
    request: Request,
    table: &Table,
    clipboard: &clipboard::Service,
    stopping: &AtomicBool,
    callbacks: Callbacks<'_>,
    exports: Option<&Path>,
) -> Outcome {
    let selected = table.select(&request.target.process_name, &request.target.window_class);
    let (mut written, conversion) = as_text::apply(
        &request.content,
        request.mode,
        selected.as_text,
        exports,
        sha256,
    );
    if request.text_separator {
        written = as_text::append_separator(&written, Separator::Newline, sha256);
    }
    let mut backend = Paste {
        app,
        request: &request,
        written,
        clipboard,
        stopping,
        callbacks,
        publication: None,
        report: json!({"clipboard_written":false,"as_text":conversion,"key_rule":selected,"actual_key":null,
            "separator":request.text_separator.then_some(Separator::Newline.name())}),
    };
    let copy_reason = if request.mode == Mode::Copy {
        Some("user_copy")
    } else if request.target.process == std::process::id() {
        Some("target_is_self")
    } else if !ax::accessibility_status() {
        Some("mac_accessibility_required")
    } else {
        request.copy_only_reason
    };
    if let Some(reason) = copy_reason {
        if let Err(error) = backend
            .check_before_publish()
            .map_err(str::to_owned)
            .and_then(|()| backend.publish())
        {
            return Outcome::not_written(error.as_str().into());
        }
        let passive = matches!(reason, "mac_accessibility_required" | "target_is_self");
        backend.report["restore"] = json!((backend.callbacks.restore)(if passive {
            RestorePolicy::CopiedOnlyHint
        } else {
            RestorePolicy::LegacyBeforeSubmit
        }));
        backend.report["reason"] = json!(reason);
        backend.report["passive_hint"] = json!(passive);
        if reason == "target_is_self" {
            backend.report["hint_message"] = json!("已复制。点一下要贴的地方，再按 Ctrl+V");
        }
        backend.finish(None);
        return Outcome::from_report(
            Some(request::conclude(request.mode, false, false, true)),
            backend.report,
        );
    }
    let result = generic::run(&mut backend);
    let result = request::generic_outcome(result, &mut backend.report);
    Outcome::from_report(Some(result), backend.report)
}

struct Paste<'a> {
    app: &'a AppHandle,
    request: &'a Request,
    written: Arc<Content>,
    clipboard: &'a clipboard::Service,
    stopping: &'a AtomicBool,
    callbacks: Callbacks<'a>,
    publication: Option<Stamp>,
    report: Value,
}

impl Backend for Paste<'_> {
    fn coordinate(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn check_before_publish(&mut self) -> Result<(), &'static str> {
        let request = self.request;
        if self.stopping.load(Ordering::Acquire) || request.cancelled.load(Ordering::Acquire) {
            return Err("request_cancelled");
        }
        if !(self.callbacks.synchronize)() {
            return Err("selection_boundary_changed");
        }
        let foreground = window::foreground();
        if foreground != request.target.window {
            if let Some(guard) = request.clipboard_guard.as_ref() {
                guard.stop(Stop::Cancelled);
            }
            return Err("external_switch");
        }
        if !window::valid(&request.target) {
            return Err("invalid_target");
        }
        if request.external_switch {
            return Err("external_switch");
        }
        if !request.permit.as_ref().is_some_and(|permit| {
            permit.check(
                request.session,
                request.record_id,
                request.mode,
                &request.target,
                foreground,
            )
        }) {
            return Err("selection_boundary_changed");
        }
        Ok(())
    }
    fn publish(&mut self) -> Result<(), String> {
        let request = self.request;
        let record = clipboard::Record {
            id: request.record_id,
            fingerprint: request.content.fingerprint,
        };
        let observation = match request.clipboard_guard.as_ref() {
            Some(guard) => self.clipboard.write_guarded(
                record,
                self.written.clone(),
                request.target.process,
                false,
                guard.clone(),
            ),
            None => {
                self.clipboard
                    .write(record, self.written.clone(), request.target.process, false)
            }
        }?;
        self.publication = Some(observation.lock().unwrap().stamp);
        self.report["clipboard_written"] = json!(true);
        Ok(())
    }
    fn restore(&mut self) -> Result<(), &'static str> {
        let focus = (self.callbacks.restore)(RestorePolicy::GenericBeforeInput);
        self.report["restore"] = json!(focus);
        if focus.is_some_and(|focus| focus.foreground.success) {
            Ok(())
        } else {
            Err("generic_restore_unavailable")
        }
    }
    fn prepare_submit(&mut self) -> Result<(), &'static str> {
        let started = Instant::now();
        loop {
            self.check_before_publish()?;
            if modifiers_released() {
                return Ok(());
            }
            if started.elapsed() >= self.request.modifier_grace.min(MODIFIER_GRACE) {
                return Err("generic_modifiers_held");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn available(&mut self) -> Result<(), &'static str> {
        if !ax::accessibility_status() {
            return Err("mac_accessibility_required");
        }
        let stamp = self.clipboard.stamp();
        if let Some(guard) = self.request.clipboard_guard.as_ref()
            && let Some(reason) = guard.inspect(|| stamp)
        {
            return Err(reason.name());
        }
        if self.publication != Some(stamp) {
            return Err("generic_clipboard_changed");
        }
        Ok(())
    }
    fn send(&mut self) -> Result<Submission, &'static str> {
        let source =
            CGEventSource::new(CGEventSourceStateID::HIDSystemState).map_err(|()| "send_failed")?;
        let key = paste_key(self.app);
        let down =
            CGEvent::new_keyboard_event(source.clone(), key, true).map_err(|()| "send_failed")?;
        let up = CGEvent::new_keyboard_event(source, key, false).map_err(|()| "send_failed")?;
        down.set_flags(CGEventFlags::CGEventFlagCommand);
        up.set_flags(CGEventFlags::CGEventFlagCommand);
        self.check_before_publish()?;
        self.available()?;
        let request = self.request;
        if !request.permit.as_ref().is_some_and(|permit| {
            permit.can_send(
                request.session,
                request.record_id,
                request.mode,
                &request.target,
                window::foreground(),
            )
        }) {
            return Err("selection_boundary_changed");
        }
        if self.stopping.load(Ordering::Acquire) || request.cancelled.load(Ordering::Acquire) {
            return Err("request_cancelled");
        }
        if !modifiers_released() {
            return Err("generic_modifiers_held");
        }
        // CGEventPost 没有送达回执，发布一次只记 sent。
        down.post(CGEventTapLocation::HID);
        up.post(CGEventTapLocation::HID);
        self.report["actual_key"] = json!("Cmd+V");
        (self.callbacks.on_sent)();
        Ok(Submission::Sent)
    }
    fn observe(&mut self) -> bool {
        false
    }
    fn finish(&mut self, _: Option<Completion>) {
        self.report["selected_to_result_ms"] =
            json!(self.request.started.elapsed().as_secs_f64() * 1000.0);
    }
}
