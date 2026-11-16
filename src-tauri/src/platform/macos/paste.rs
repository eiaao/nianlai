use super::clipboard;
use crate::{
    history::content::Content,
    paste::{
        keys::Table,
        outcome::{Outcome, PasteResult},
        request::{Callbacks, Mode, Request},
    },
    platform::{ReplayKey, Target, key_state},
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

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
// macOS 没有 UIPI，这里不需要。
pub fn compare_integrity(_: &Target) -> Value {
    json!({"target_higher":false})
}
pub fn execute(
    request: Request,
    _: &Table,
    clipboard: &clipboard::Service,
    stopping: &AtomicBool,
    _: Callbacks<'_>,
    _: Option<&Path>,
) -> Outcome {
    if stopping.load(Ordering::Acquire) || request.cancelled.load(Ordering::Acquire) {
        return Outcome::not_written("request_cancelled".into());
    }
    let record = clipboard::Record {
        id: request.record_id,
        fingerprint: request.content.fingerprint,
    };
    let result = match request.clipboard_guard {
        Some(guard) => clipboard.write_guarded(
            record,
            request.content,
            request.target.process,
            false,
            guard,
        ),
        None => clipboard.write(record, request.content, request.target.process, false),
    };
    match result {
        Err(error) => Outcome::not_written(error.as_str().into()),
        Ok(_) => Outcome::from_report(
            Some(if request.mode == Mode::Copy {
                PasteResult::Copied
            } else {
                PasteResult::CopiedOnly
            }),
            json!({"clipboard_written":true,"reason":"macos_paste_pending"}),
        ),
    }
}
