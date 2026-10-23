use super::window;
use crate::{log::Logger, platform::Result};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{
            GetLastError, GlobalFree, HANDLE, HGLOBAL, HINSTANCE, HWND, LPARAM, LRESULT,
            SetLastError, WIN32_ERROR, WPARAM,
        },
        System::{
            DataExchange::*,
            LibraryLoader::GetModuleHandleW,
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
            Ole::CF_UNICODETEXT,
        },
        UI::WindowsAndMessaging::*,
    },
    core::w,
};

const COMMAND: u32 = WM_APP + 10;

pub struct Read {
    at: Instant,
    process_name: String,
    known: bool,
    is_target: bool,
}

#[derive(Default)]
pub struct Observation {
    first: Option<Read>,
    destroyed: bool,
    render_failed: bool,
}

impl Observation {
    pub fn observed(&self) -> bool {
        self.first.is_some()
    }

    pub fn report(&self, sent: Instant) -> Value {
        let delta = self.first.as_ref().map(|read| {
            if read.at >= sent {
                read.at.duration_since(sent).as_secs_f64() * 1000.0
            } else {
                -(sent.duration_since(read.at).as_secs_f64() * 1000.0)
            }
        });
        let status = match &self.first {
            None => "timeout",
            Some(read)
                if read.at > sent && read.at.duration_since(sent) > Duration::from_secs(1) =>
            {
                "timeout"
            }
            Some(read) if !read.known => "unknown_reader_first",
            Some(read) if !read.is_target => "other_process_first",
            Some(read) if read.at < sent => "before_send",
            Some(_) if self.render_failed => "render_failed",
            Some(_) => "target_read",
        };
        json!({"status":status,"first_reader":self.first.as_ref().map(|read| &read.process_name),
            "is_target":self.first.as_ref().and_then(|read| read.known.then_some(read.is_target)), "relative_to_send_ms":delta,
            "timed_out":status=="timeout", "first_read_only":true,
            "ownership_lost":self.destroyed,"render_failed":self.render_failed})
    }
}

struct Memory(HGLOBAL);
impl Memory {
    fn new(bytes: &[u8]) -> Result<Self> {
        unsafe {
            let memory = Self(
                GlobalAlloc(GMEM_MOVEABLE, bytes.len()).map_err(|_| "clipboard_allocate_failed")?,
            );
            let pointer = GlobalLock(memory.0);
            if pointer.is_null() {
                return Err("clipboard_lock_failed".into());
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer.cast(), bytes.len());
            let _ = GlobalUnlock(memory.0);
            Ok(memory)
        }
    }
    fn transfer(self, format: u32) -> std::result::Result<(), Self> {
        if unsafe { SetClipboardData(format, Some(HANDLE(self.0.0))) }.is_err() {
            return Err(self);
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
struct Open;
impl Drop for Open {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

struct Pending {
    text: Option<Memory>,
    target: u32,
    observation: Arc<Mutex<Observation>>,
}
thread_local! { static PENDING: RefCell<Option<Pending>> = const { RefCell::new(None) }; }

fn render(record_reader: bool) {
    // SetClipboardData 可能触发同步消息，调用期间不持有 RefCell 的借用。
    let pending = PENDING.with(|state| state.borrow_mut().take());
    if let Some(mut pending) = pending {
        let mut reader_process = None;
        if record_reader && !pending.observation.lock().unwrap().observed() {
            let at = Instant::now();
            let mut process = 0;
            if let Ok(reader) = unsafe { GetOpenClipboardWindow() } {
                unsafe {
                    GetWindowThreadProcessId(reader, Some(&mut process));
                }
            }
            pending.observation.lock().unwrap().first = Some(Read {
                at,
                process_name: String::new(),
                known: process != 0,
                is_target: process != 0 && process == pending.target,
            });
            reader_process = Some(process);
        }
        if let Some(text) = pending.text.take()
            && let Err(text) = text.transfer(CF_UNICODETEXT.0 as u32)
        {
            pending.text = Some(text);
            pending.observation.lock().unwrap().render_failed = true;
        }
        // 先交付正文再解析进程名，避免进程查询拖住目标的 GetClipboardData。
        if let Some(process) = reader_process {
            let name = window::process_name(process);
            if let Some(first) = pending.observation.lock().unwrap().first.as_mut() {
                first.process_name = name;
            }
        }
        PENDING.with(|state| *state.borrow_mut() = Some(pending));
    }
}

fn render_all(owner: HWND) -> bool {
    if !PENDING.with(|state| {
        state
            .borrow()
            .as_ref()
            .is_some_and(|pending| pending.text.is_some())
    }) {
        return true;
    }
    let started = Instant::now();
    loop {
        if unsafe { OpenClipboard(Some(owner)) }.is_ok() {
            break;
        }
        if started.elapsed() >= Duration::from_millis(200) {
            return false;
        }
        thread::sleep(Duration::from_millis(5));
    }
    let _open = Open;
    if unsafe { GetClipboardOwner() }.ok() == Some(owner) {
        render(false);
    }
    !PENDING.with(|state| {
        state
            .borrow()
            .as_ref()
            .is_some_and(|pending| pending.text.is_some())
    })
}

unsafe extern "system" fn procedure(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match message {
        WM_RENDERFORMAT if wp.0 as u32 == CF_UNICODETEXT.0 as u32 => {
            render(true);
            LRESULT(0)
        }
        WM_RENDERALLFORMATS => {
            let _ = render_all(hwnd);
            LRESULT(0)
        }
        WM_DESTROYCLIPBOARD => {
            PENDING.with(|state| {
                if let Some(pending) = state.borrow_mut().take() {
                    pending.observation.lock().unwrap().destroyed = true;
                }
            });
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wp, lp) },
    }
}

fn write(
    owner: HWND,
    text: &str,
    target: u32,
    delayed: bool,
    observation: Arc<Mutex<Observation>>,
) -> Result<()> {
    let bytes: Vec<u8> = text
        .encode_utf16()
        .chain(Some(0))
        .flat_map(u16::to_ne_bytes)
        .collect();
    let text = Memory::new(&bytes)?;
    let names = [
        w!("Clipboard Viewer Ignore"),
        w!("ExcludeClipboardContentFromMonitorProcessing"),
        w!("CanIncludeInClipboardHistory"),
        w!("CanUploadToCloudClipboard"),
    ];
    let mut markers = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let format = unsafe { RegisterClipboardFormatW(*name) };
        if format == 0 {
            return Err("clipboard_register_format_failed".into());
        }
        markers.push((
            format,
            Memory::new(&(if index < 2 { 1u32 } else { 0u32 }).to_ne_bytes())?,
        ));
    }
    let started = Instant::now();
    loop {
        if unsafe { OpenClipboard(Some(owner)) }.is_ok() {
            break;
        }
        if started.elapsed() >= Duration::from_millis(200) {
            return Err("clipboard_busy".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    let _open = Open;
    unsafe {
        EmptyClipboard().map_err(|_| "clipboard_empty_failed")?;
    }
    for (format, marker) in markers {
        marker
            .transfer(format)
            .map_err(|_| "clipboard_marker_failed")?;
    }
    if delayed {
        unsafe {
            // 延迟渲染成功时也返回 NULL；windows-rs 的非空句柄包装会将它当成错误。
            SetLastError(WIN32_ERROR(0));
            let _ = SetClipboardData(CF_UNICODETEXT.0 as u32, None);
            if GetLastError().0 != 0 {
                return Err("clipboard_delay_failed".into());
            }
        }
        PENDING.with(|state| {
            *state.borrow_mut() = Some(Pending {
                text: Some(text),
                target,
                observation,
            })
        });
    } else {
        text.transfer(CF_UNICODETEXT.0 as u32)
            .map_err(|_| "clipboard_text_failed")?;
    }
    Ok(())
}

enum Command {
    Write {
        text: String,
        target: u32,
        delayed: bool,
        observation: Arc<Mutex<Observation>>,
        reply: mpsc::SyncSender<Result<()>>,
    },
    Materialize(mpsc::SyncSender<bool>),
    Stop,
}
struct Inner {
    owner: usize,
    sender: mpsc::Sender<Command>,
    thread: Mutex<Option<JoinHandle<()>>>,
}
#[derive(Clone)]
pub struct Service(Arc<Inner>);

impl Service {
    pub fn start(logger: Logger) -> Result<Self> {
        let (sender, commands) = mpsc::channel();
        let (ready, receiver) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("paste-clipboard".into())
            .spawn(move || {
                let result = (|| -> Result<HWND> {
                    unsafe {
                        let instance = HINSTANCE(
                            GetModuleHandleW(None)
                                .map_err(|_| "clipboard_module_failed")?
                                .0,
                        );
                        let class = WNDCLASSW {
                            lpfnWndProc: Some(procedure),
                            hInstance: instance,
                            lpszClassName: w!("NianlaiG0Clipboard"),
                            ..Default::default()
                        };
                        if RegisterClassW(&class) == 0 {
                            return Err("clipboard_class_failed".into());
                        }
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
                        .map_err(|_| "clipboard_window_failed".into())
                    }
                })();
                let owner = match result {
                    Ok(owner) => owner,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                logger.event("clipboard_thread_started", json!({"message_only":true}));
                if ready.send(Ok(owner.0 as usize)).is_err() {
                    unsafe {
                        let _ = DestroyWindow(owner);
                    }
                    return;
                }
                let mut message = MSG::default();
                'messages: loop {
                    let result = unsafe { GetMessageW(&mut message, None, 0, 0) }.0;
                    if result <= 0 {
                        break;
                    }
                    if message.message == COMMAND {
                        while let Ok(command) = commands.try_recv() {
                            match command {
                                Command::Write {
                                    text,
                                    target,
                                    delayed,
                                    observation,
                                    reply,
                                } => {
                                    let _ = reply.send(write(
                                        owner,
                                        &text,
                                        target,
                                        delayed,
                                        observation,
                                    ));
                                }
                                Command::Materialize(reply) => {
                                    let _ = reply.send(render_all(owner));
                                }
                                Command::Stop => {
                                    logger.event(
                                        "clipboard_shutdown_render",
                                        json!({"success":render_all(owner)}),
                                    );
                                    break 'messages;
                                }
                            }
                        }
                    } else {
                        unsafe {
                            let _ = TranslateMessage(&message);
                            DispatchMessageW(&message);
                        }
                    }
                }
                unsafe {
                    let _ = DestroyWindow(owner);
                }
                PENDING.with(|state| state.borrow_mut().take());
                logger.event("clipboard_thread_stopped", json!({}));
            })
            .map_err(|_| "clipboard_thread_failed")?;
        let owner = receiver.recv().map_err(|_| "clipboard_start_failed")??;
        Ok(Self(Arc::new(Inner {
            owner,
            sender,
            thread: Mutex::new(Some(thread)),
        })))
    }

    fn command(&self, command: Command) -> Result<()> {
        if self.0.thread.lock().unwrap().is_none() {
            return Err("clipboard_stopped".into());
        }
        self.0
            .sender
            .send(command)
            .map_err(|_| "clipboard_stopped")?;
        unsafe {
            PostMessageW(
                Some(window::hwnd(self.0.owner)),
                COMMAND,
                WPARAM(0),
                LPARAM(0),
            )
        }
        .map_err(|_| "clipboard_post_failed".into())
    }

    pub fn write(&self, text: &str, target: u32, delayed: bool) -> Result<Arc<Mutex<Observation>>> {
        let observation = Arc::new(Mutex::new(Observation::default()));
        let (reply, receiver) = mpsc::sync_channel(1);
        self.command(Command::Write {
            text: text.to_owned(),
            target,
            delayed,
            observation: observation.clone(),
            reply,
        })?;
        receiver.recv().map_err(|_| "clipboard_write_failed")??;
        Ok(observation)
    }

    pub fn materialize(&self) -> bool {
        let (reply, receiver) = mpsc::sync_channel(1);
        if self.command(Command::Materialize(reply)).is_ok() {
            return receiver.recv().unwrap_or(false);
        }
        false
    }

    pub fn shutdown(&self) {
        self.0.shutdown();
    }
}
impl Inner {
    fn shutdown(&self) {
        let thread = self.thread.lock().unwrap().take();
        if let Some(thread) = thread {
            let _ = self.sender.send(Command::Stop);
            unsafe {
                let _ = PostMessageW(
                    Some(window::hwnd(self.owner)),
                    COMMAND,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
            let _ = thread.join();
        }
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_only_the_first_read_inside_the_evidence_window() {
        let sent = Instant::now();
        let observation = |at, is_target| Observation {
            first: Some(Read {
                at,
                process_name: "example.exe".into(),
                known: true,
                is_target,
            }),
            ..Default::default()
        };
        assert_eq!(Observation::default().report(sent)["status"], "timeout");
        let early = observation(sent - Duration::from_millis(10), true).report(sent);
        assert_eq!(early["status"], "before_send");
        assert_eq!(early["relative_to_send_ms"], -10.0);
        assert_eq!(
            observation(sent + Duration::from_millis(10), true).report(sent)["status"],
            "target_read"
        );
        assert_eq!(
            observation(sent + Duration::from_millis(10), false).report(sent)["status"],
            "other_process_first"
        );
        let mut unknown = observation(sent + Duration::from_millis(10), false);
        unknown.first.as_mut().unwrap().known = false;
        let unknown = unknown.report(sent);
        assert_eq!(unknown["status"], "unknown_reader_first");
        assert!(unknown["is_target"].is_null());
        for is_target in [true, false] {
            assert_eq!(
                observation(sent + Duration::from_millis(1001), is_target).report(sent)["status"],
                "timeout"
            );
        }
    }
}
