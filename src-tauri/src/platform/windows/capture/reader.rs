use super::super::{formats, hash, window};
use super::{Notice, Shared, notice};
use crate::{
    capture::blocking_marker,
    history::{
        CopyEvent,
        content::{Content, DIB, DIBV5, Data, FILES, TEXT},
    },
    platform::Result,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::atomic::Ordering,
    thread,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{HGLOBAL, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        System::{
            DataExchange::*,
            LibraryLoader::GetModuleHandleW,
            Memory::{GlobalLock, GlobalSize, GlobalUnlock},
        },
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, HWND_MESSAGE, MSG,
            PM_REMOVE, PeekMessageW, RegisterClassW, TranslateMessage, WINDOW_EX_STYLE,
            WINDOW_STYLE, WNDCLASSW,
        },
    },
    core::w,
};

unsafe extern "system" fn procedure(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, message, wp, lp) }
}

/// 读取线程用自己的仅消息窗口打开剪贴板。
/// 实测：用 OpenClipboard(NULL) 打开后，另一个同样不指定窗口的程序也能“打开”成功，
/// 把打开权抢走，我们列到第一个格式就报 ERROR_CLIPBOARD_NOT_OPEN（这台机器上几乎每次复制都会）。
fn reader_window(shared: &Shared) -> Option<HWND> {
    let result = (|| -> Option<HWND> {
        let instance = HINSTANCE(unsafe { GetModuleHandleW(None) }.ok()?.0);
        let class = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            hInstance: instance,
            lpszClassName: w!("NianlaiG0CaptureReader"),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&class) } == 0 {
            return None;
        }
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class.lpszClassName,
                w!(""),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(instance),
                None,
            )
        }
        .ok()
    })();
    shared
        .logger
        .event("capture_reader_window", json!({"success":result.is_some()}));
    result
}

// 线程平时阻塞在条件变量上，不处理消息；每次读取前把可能积下的消息清掉。
fn drain() {
    let mut message = MSG::default();
    unsafe {
        while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

struct Open;
impl Drop for Open {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}
struct Locked(HGLOBAL);
impl Drop for Locked {
    fn drop(&mut self) {
        unsafe {
            let _ = GlobalUnlock(self.0);
        }
    }
}

fn with_data<T>(id: u32, read: impl FnOnce(&[u8]) -> Result<T>) -> Result<T> {
    let handle = unsafe { GetClipboardData(id) }.map_err(|_| "read_failed")?;
    let global = HGLOBAL(handle.0);
    let size = unsafe { GlobalSize(global) };
    if size == 0 || size > isize::MAX as usize {
        return Err("read_failed".into());
    }
    let pointer = unsafe { GlobalLock(global) };
    if pointer.is_null() {
        return Err("read_failed".into());
    }
    let _locked = Locked(global);
    read(unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), size) })
}

struct Attempt {
    notice: Notice,
    all: Vec<(u32, String)>,
    saved: Vec<Value>,
    missing: Vec<Value>,
    data: BTreeMap<String, Data>,
    retries: usize,
    wait_ms: f64,
    partial: bool,
    ole_bitmap: bool,
    reason: Option<&'static str>,
    marker: Option<&'static str>,
    retry_latest: bool,
    enumerate_error: Option<Value>,
}

impl Attempt {
    fn new(notice: Notice) -> Self {
        Self {
            notice,
            all: Vec::new(),
            saved: Vec::new(),
            missing: Vec::new(),
            data: BTreeMap::new(),
            retries: 0,
            wait_ms: 0.0,
            partial: false,
            ole_bitmap: false,
            reason: None,
            marker: None,
            retry_latest: false,
            enumerate_error: None,
        }
    }
}

fn read(shared: &Shared, owner: Option<HWND>, mut attempt: Attempt) -> Attempt {
    // 实测：刚打开剪贴板、列出第一个格式后，可能报 ERROR_CLIPBOARD_NOT_OPEN（1418），
    // 第一次复制正常，之后几乎每次都这样。关闭后稍等再完整读一遍，并记下当时谁占着剪贴板。
    let mut lost = Vec::new();
    for delay in [15, 30, 60, 0] {
        attempt = read_once(shared, owner, attempt);
        let Some(error) = attempt.enumerate_error.take() else {
            break;
        };
        lost.push(error);
        if delay == 0 || shared.stopping.load(Ordering::Acquire) {
            break;
        }
        thread::sleep(Duration::from_millis(delay));
        attempt = Attempt::new(attempt.notice);
    }
    if !lost.is_empty() {
        attempt.enumerate_error = Some(json!(lost));
    }
    attempt
}

fn holder() -> Value {
    let window = unsafe { GetOpenClipboardWindow() }.unwrap_or_default();
    let process = super::process(window);
    json!({"open_window":!window.is_invalid(),"process":(process != 0).then(|| window::process_name(process))})
}

fn read_once(shared: &Shared, owner: Option<HWND>, mut attempt: Attempt) -> Attempt {
    let waits = [10, 20, 40, 80, 160, 320];
    let started = Instant::now();
    loop {
        if shared.stopping.load(Ordering::Acquire) {
            attempt.reason = Some("stopping");
            return attempt;
        }
        if unsafe { OpenClipboard(owner) }.is_ok() {
            break;
        }
        if attempt.retries == waits.len() {
            attempt.wait_ms = started.elapsed().as_secs_f64() * 1000.0;
            attempt.reason = Some("open_failed");
            return attempt;
        }
        // WSLg 常在数毫秒后抢先打开；每次重试再打开，不占着剪贴板等待。
        let until = Instant::now() + Duration::from_millis(waits[attempt.retries]);
        attempt.retries += 1;
        while Instant::now() < until && !shared.stopping.load(Ordering::Acquire) {
            thread::sleep(
                Duration::from_millis(10).min(until.saturating_duration_since(Instant::now())),
            );
        }
    }
    attempt.wait_ms = started.elapsed().as_secs_f64() * 1000.0;
    let open = Open;
    let sequence = unsafe { GetClipboardSequenceNumber() };
    if attempt.notice.sequence != sequence {
        attempt.notice = shared
            .inbox
            .matching(sequence)
            .unwrap_or_else(|| notice("self_check"));
        attempt.notice.sequence = sequence;
    }
    let owner = unsafe { GetClipboardOwner() }.unwrap_or_default();
    // 必须在枚举和 GetClipboardData 之前判断，不能让采集成为延迟渲染的首个读取方。
    if owner.0 as usize == shared.own_owner {
        attempt.reason = Some("own_write");
    } else {
        attempt.notice.owner = super::process(owner);
        match formats::enumerate() {
            Err((code, count)) => {
                attempt.reason = Some("enumerate_failed");
                attempt.enumerate_error = Some(json!({"code":code,"after":count,
                    "since_open_ms":started.elapsed().as_secs_f64()*1000.0-attempt.wait_ms,"holder":holder()}));
            }
            Ok(all) => {
                attempt.all = all;
                attempt.marker = blocking_marker(
                    attempt.all.iter().map(|(_, name)| name.as_str()),
                    |marker| {
                        let id = attempt.all.iter().find(|(_, name)| name == marker)?.0;
                        let started = Instant::now();
                        let result = with_data(id, |bytes| {
                            Ok(u32::from_le_bytes(
                                bytes
                                    .get(..4)
                                    .ok_or("read_failed")?
                                    .try_into()
                                    .map_err(|_| "read_failed")?,
                            ))
                        })
                        .ok();
                        if started.elapsed() > Duration::from_secs(1) {
                            shared.logger.event("capture_slow", json!({"format":marker,"read_ms":started.elapsed().as_secs_f64()*1000.0}));
                        }
                        result
                    },
                );
                if attempt.marker.is_some() {
                    attempt.reason = Some("marker");
                } else {
                    read_formats(shared, &mut attempt);
                }
            }
        }
    }
    drop(open);
    if unsafe { GetClipboardSequenceNumber() } != sequence
        && !shared.stopping.load(Ordering::Acquire)
    {
        attempt.data.clear();
        attempt.reason = Some("sequence_changed");
        attempt.retry_latest = true;
    }
    attempt
}

fn read_formats(shared: &Shared, attempt: &mut Attempt) {
    let has = |name: &str| attempt.all.iter().any(|(_, available)| available == name);
    attempt.ole_bitmap = has("Object Descriptor")
        && [
            TEXT,
            "CF_TEXT",
            "CF_OEMTEXT",
            "HTML Format",
            "Rich Text Format",
        ]
        .iter()
        .any(|name| has(name));
    let v5 = has(DIBV5);
    let mut empty = false;
    for (id, name) in &attempt.all {
        let Some(limit) = formats::limit(name) else {
            continue;
        };
        if shared.stopping.load(Ordering::Acquire) {
            attempt.reason = Some("stopping");
            attempt.data.clear();
            return;
        }
        if (name == DIB || name == DIBV5) && attempt.ole_bitmap {
            attempt
                .missing
                .push(json!({"name":name,"reason":"ole_bitmap"}));
            continue;
        }
        if name == DIB && v5 {
            continue;
        }
        let started = Instant::now();
        let mut fallback = false;
        let result = with_data(*id, |bytes| {
            if name == FILES {
                return formats::parse_dropfiles(bytes)
                    .map(|paths| Data::Files(paths.into()))
                    .ok_or("read_failed".into());
            }
            let length = formats::effective_length(name, bytes).unwrap_or_else(|| {
                fallback = true;
                bytes.len()
            });
            if length > limit {
                return Err("too_large".into());
            }
            // 密码管理器到时清空剪贴板时，常写一段空文本（Bitwarden 退出时还不带标记），不当成一次复制。
            if length == 0 {
                return Err("empty".into());
            }
            Ok(Data::Bytes(bytes[..length].into()))
        });
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        if elapsed > 1000.0 {
            shared.logger.event(
                "capture_slow",
                json!({"format":formats::log_name(name),"read_ms":elapsed}),
            );
        }
        match result {
            Ok(data) => {
                attempt.saved.push(json!({"name":formats::log_name(name),"bytes":data.size(),"read_ms":elapsed,"global_size_fallback":fallback}));
                attempt.data.insert(name.clone(), data);
            }
            Err(reason) => {
                if reason == "empty" {
                    empty = true;
                } else {
                    attempt.partial = true;
                }
                attempt.missing.push(json!({"name":formats::log_name(name),"reason":reason,"read_ms":elapsed,"global_size_fallback":fallback}));
            }
        }
    }
    if attempt.data.is_empty() {
        attempt.reason = Some(if empty {
            "empty"
        } else {
            "no_supported_formats"
        });
    }
}

pub(super) fn run(shared: &Shared) {
    let owner = reader_window(shared);
    while let Some(notification) = shared.inbox.next(&shared.stopping) {
        drain();
        let started = Instant::now();
        let mut attempt = read(shared, owner, Attempt::new(notification));
        let read_ms = started.elapsed().as_secs_f64() * 1000.0;
        let source = window::process_name(attempt.notice.source);
        let owner = window::process_name(attempt.notice.owner);
        let mut outcome = "skipped";
        let mut record = json!({"record_id":null,"kind":null,"copy_count":null});
        let mut changed = false;
        let mut write_ms = None;
        let mut transaction_ok = None;
        let mut store_error: Option<String> = None;
        if shared.stopping.load(Ordering::Acquire) {
            attempt.reason = Some("stopping");
        }
        if attempt.reason.is_none() {
            match Content::new(std::mem::take(&mut attempt.data), hash::sha256) {
                Ok(content) => {
                    let event = CopyEvent {
                        time_ms: attempt.notice.time_ms,
                        sequence: attempt.notice.sequence,
                        source_process: source.clone(),
                        owner_process: owner.clone(),
                        owner_pid: attempt.notice.owner,
                        observed: attempt.notice.at,
                    };
                    let write_started = Instant::now();
                    let result = if shared.stopping.load(Ordering::Acquire) {
                        None
                    } else {
                        Some(
                            shared
                                .history
                                .ingest(content, event, attempt.partial, Instant::now()),
                        )
                    };
                    match result {
                        Some(Ok(result)) => {
                            write_ms = Some(result.write_ms);
                            transaction_ok = Some(result.transaction_ok);
                            outcome = result.outcome;
                            attempt.reason = result.reason;
                            changed = result.record_id.is_some();
                            record = serde_json::to_value(result).unwrap_or_default();
                        }
                        Some(Err(error)) => {
                            write_ms = Some(write_started.elapsed().as_secs_f64() * 1000.0);
                            transaction_ok = Some(false);
                            store_error = Some(error);
                            attempt.reason = Some("store_failed");
                        }
                        None => attempt.reason = Some("stopping"),
                    }
                }
                Err(_) => attempt.reason = Some("hash_failed"),
            }
        }
        if attempt.reason.is_some() && !attempt.retry_latest && store_error.is_none() {
            shared.history.skipped_sequence(attempt.notice.sequence);
        }
        let latency = attempt.notice.at.elapsed().as_secs_f64() * 1000.0;
        {
            let mut counts = shared.counters.lock().unwrap();
            counts.reads += 1;
            *counts.outcomes.entry(outcome).or_default() += 1;
            if let Some(reason) = attempt.reason {
                *counts.skip_reasons.entry(reason).or_default() += 1;
            }
            if changed {
                *counts.latencies.entry(latency.ceil() as u64).or_default() += 1;
            }
        }
        shared.logger.event("capture", json!({
            "sequence":attempt.notice.sequence,"trigger":attempt.notice.trigger,"outcome":outcome,"reason":attempt.reason,"marker":attempt.marker,
            "enumerate_error":attempt.enumerate_error,"write_ms":write_ms,"transaction_ok":transaction_ok,"error_code":store_error,
            "source_process":source,"owner_process":owner,"same_process":attempt.notice.source!=0 && attempt.notice.source==attempt.notice.owner,
            "formats":attempt.all.iter().take(60).map(|(_, name)| formats::log_name(name)).collect::<Vec<_>>(),"format_count":attempt.all.len(),
            "saved_formats":attempt.saved,"missing_formats":attempt.missing,"ole_bitmap_skipped":attempt.ole_bitmap,
            "open_retries":attempt.retries,"open_wait_ms":attempt.wait_ms,"read_ms":read_ms,"capture_ms":started.elapsed().as_secs_f64()*1000.0,
            "notification_to_history_ms":if changed { Some(latency) } else { None },"record_id":record["record_id"],"kind":record["kind"],"copy_count":record["copy_count"]
        }));
        if changed {
            (shared.changed)();
        }
        if attempt.retry_latest {
            shared.inbox.push(notice("self_check"));
        }
    }
    if let Some(hwnd) = owner {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supported_limits_allow_four_k_dib_but_limit_compressed_images() {
        assert_eq!(
            formats::limit(crate::history::content::PNG),
            Some(20 * formats::MIB)
        );
        assert_eq!(formats::limit(DIBV5), Some(64 * formats::MIB));
        assert_eq!(formats::limit(TEXT), Some(2 * formats::MIB));
    }
}
