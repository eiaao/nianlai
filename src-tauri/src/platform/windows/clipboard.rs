use super::{formats, window};
use crate::{
    history::{
        History,
        content::{Content, Data, FILES, Fingerprint, HTML, RTF, TEXT},
    },
    log::Logger,
    paste::{capture_access::CaptureAccess, native_access},
    platform::Result,
};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::BTreeMap,
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
    immediate: bool,
    first: Option<Read>,
    destroyed: bool,
    render_failed: bool,
    delay_failed: bool,
    pub publication: Option<native_access::Publication>,
    pub publication_error: Option<&'static str>,
    #[cfg(test)]
    pub publication_trace: Option<crate::paste::native_publication_trace::PublicationTrace>,
}

impl Observation {
    pub fn observed(&self) -> bool {
        self.first.is_some()
    }

    pub fn report(&self, sent: Instant) -> Value {
        if self.immediate {
            return json!({"status":"not_observable","observable":false,
                "first_reader":null,"is_target":null,"relative_to_send_ms":null,
                "timed_out":false,"first_read_only":false,
                "ownership_lost":self.destroyed,"render_failed":self.render_failed,
                "delay_failed":self.delay_failed});
        }
        let delta = self.first.as_ref().map(|read| {
            if read.at >= sent {
                read.at.duration_since(sent).as_secs_f64() * 1000.0
            } else {
                -(sent.duration_since(read.at).as_secs_f64() * 1000.0)
            }
        });
        let status = match &self.first {
            _ if self.delay_failed => "delay_failed",
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
        json!({"status":status,"observable":true,"first_reader":self.first.as_ref().map(|read| &read.process_name),
            "is_target":self.first.as_ref().and_then(|read| read.known.then_some(read.is_target)), "relative_to_send_ms":delta,
            "timed_out":status=="timeout", "first_read_only":true,
            "ownership_lost":self.destroyed,"render_failed":self.render_failed,"delay_failed":self.delay_failed})
    }
}

struct Memory(HGLOBAL);
impl Memory {
    fn new(bytes: &[u8]) -> Result<Self> {
        Self::padded(bytes, 0)
    }
    fn padded(bytes: &[u8], padding: usize) -> Result<Self> {
        unsafe {
            let memory = Self(
                GlobalAlloc(GMEM_MOVEABLE, (bytes.len() + padding).max(1))
                    .map_err(|_| "clipboard_allocate_failed")?,
            );
            let pointer = GlobalLock(memory.0);
            if pointer.is_null() {
                return Err("clipboard_lock_failed".into());
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer.cast(), bytes.len());
            std::ptr::write_bytes(pointer.cast::<u8>().add(bytes.len()), 0, padding);
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
struct Open(bool);
impl Open {
    fn close(mut self) -> bool {
        if unsafe { CloseClipboard() }.is_ok() {
            self.0 = false;
            true
        } else {
            // 首次失败即不能提交；Drop 仅尽力释放，不能把第二次关闭当成许可。
            false
        }
    }
}
impl Drop for Open {
    fn drop(&mut self) {
        if self.0 {
            unsafe {
                let _ = CloseClipboard();
            }
        }
    }
}

#[derive(Default)]
struct NativeProbe {
    open: Option<Open>,
}
impl native_access::Clipboard for NativeProbe {
    fn open(&mut self) -> bool {
        if unsafe { OpenClipboard(None) }.is_err() {
            return false;
        }
        self.open = Some(Open(true));
        true
    }
    fn sequence(&mut self) -> u32 {
        unsafe { GetClipboardSequenceNumber() }
    }
    fn owner(&mut self) -> usize {
        unsafe { GetClipboardOwner() }.unwrap_or_default().0 as usize
    }
    fn close(&mut self) -> bool {
        self.open.take().is_some_and(Open::close)
    }
}

// 与生产路径共用的实际探针入口；显式桌面诊断也必须经过同一 Open/stamp/Close 顺序。
pub(crate) fn probe_native(publication: native_access::Publication) -> native_access::Probe {
    native_access::probe(&mut NativeProbe::default(), publication)
}

struct Pending {
    formats: BTreeMap<u32, (String, Data)>,
    target: u32,
    observation: Arc<Mutex<Observation>>,
}
thread_local! { static PENDING: RefCell<Option<Pending>> = const { RefCell::new(None) }; }

fn memory(name: &str, data: &Data) -> Result<Memory> {
    match data {
        Data::Bytes(bytes) => Memory::padded(
            bytes,
            match name {
                TEXT => 2,
                HTML | RTF => 1,
                _ => 0,
            },
        ),
        Data::Files(paths) => Memory::new(&formats::build_dropfiles(paths)?),
    }
}

fn render(format: Option<u32>, record_reader: bool) {
    // SetClipboardData 可能触发同步消息，调用期间不持有 RefCell 的借用。
    let pending = PENDING.with(|state| state.borrow_mut().take());
    if let Some(mut pending) = pending {
        let mut reader_process = None;
        if record_reader
            && format.is_some_and(|id| pending.formats.contains_key(&id))
            && !pending.observation.lock().unwrap().observed()
        {
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
        let ids: Vec<_> = pending
            .formats
            .keys()
            .copied()
            .filter(|id| format.is_none_or(|requested| requested == *id))
            .collect();
        for id in ids {
            let (name, data) = &pending.formats[&id];
            if memory(name, data)
                .and_then(|memory| {
                    memory
                        .transfer(id)
                        .map_err(|_| "clipboard_render_failed".into())
                })
                .is_ok()
            {
                pending.formats.remove(&id);
            } else {
                pending.observation.lock().unwrap().render_failed = true;
            }
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
            .is_some_and(|pending| !pending.formats.is_empty())
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
    let _open = Open(true);
    if unsafe { GetClipboardOwner() }.ok() == Some(owner) {
        render(None, false);
    }
    !PENDING.with(|state| {
        state
            .borrow()
            .as_ref()
            .is_some_and(|pending| !pending.formats.is_empty())
    })
}

unsafe extern "system" fn procedure(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match message {
        WM_RENDERFORMAT => {
            render(Some(wp.0 as u32), true);
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

// 历史里的那条记录。写入前要确认它还在、指纹没变；实际写入的内容降级成文字时和它不同。
#[derive(Clone, Copy)]
pub struct Record {
    pub id: usize,
    pub fingerprint: Fingerprint,
}

fn write(
    owner: HWND,
    record: Record,
    content: &Content,
    history: &History,
    target: u32,
    delayed: bool,
    observation: Arc<Mutex<Observation>>,
) -> Result<()> {
    observation.lock().unwrap().immediate = !delayed;
    let mut pending = BTreeMap::new();
    for (name, format) in &content.formats {
        pending.insert(
            formats::register(name)?,
            (name.clone(), format.data.clone()),
        );
    }
    if content.formats.contains_key(FILES) {
        pending.insert(
            formats::register("Preferred DropEffect")?,
            (
                "Preferred DropEffect".into(),
                Data::Bytes(1u32.to_le_bytes().to_vec().into()),
            ),
        );
    }
    let immediate = if delayed {
        Vec::new()
    } else {
        pending
            .iter()
            .map(|(id, (name, data))| Ok((*id, memory(name, data)?)))
            .collect::<Result<Vec<_>>>()?
    };
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
    // 登记回声要经过存储的写线程，它可能正在写一张大图；放在打开剪贴板之前，
    // 免得占着剪贴板等数据库，挡住其他程序。
    if !history.before_write(
        record.id,
        record.fingerprint,
        content.fingerprint,
        Instant::now(),
    ) {
        return Err("record_missing".into());
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
    let open = Open(true);
    unsafe {
        EmptyClipboard().map_err(|_| "clipboard_empty_failed")?;
    }
    for (format, marker) in markers {
        marker
            .transfer(format)
            .map_err(|_| "clipboard_marker_failed")?;
    }
    if delayed {
        let ids: Vec<_> = pending.keys().copied().collect();
        let evidence = observation.clone();
        PENDING.with(|state| {
            *state.borrow_mut() = Some(Pending {
                formats: pending,
                target,
                observation: observation.clone(),
            })
        });
        for id in ids {
            unsafe {
                // 延迟渲染成功时也返回 NULL；windows-rs 的非空句柄包装会将它当成错误。
                SetLastError(WIN32_ERROR(0));
                let _ = SetClipboardData(id, None);
                if GetLastError().0 == 0 {
                    continue;
                }
            }
            // 剪贴板已经清空，不能一边报“写入失败”一边留下这条内容：改为当场写入全部格式，
            // 第 2 层证据如实记为不可用。
            evidence.lock().unwrap().delay_failed = true;
            render(None, false);
            if PENDING.with(|state| {
                state
                    .borrow()
                    .as_ref()
                    .is_some_and(|pending| !pending.formats.is_empty())
            }) {
                return Err("clipboard_render_failed".into());
            }
            break;
        }
    } else {
        for (id, memory) in immediate {
            memory.transfer(id).map_err(|_| "clipboard_format_failed")?;
        }
    }
    // 成功写入的事实和“可以安全提交”分开：关闭或快照失败仍然返回写入成功。
    // Close 成功后、可能等待存储线程的 reuse 前采样；不假设 Close 前后 sequence 相等。
    #[cfg(test)]
    let trace_started = Instant::now();
    #[cfg(test)]
    let trace = crate::paste::native_publication_trace::Recorder::new(|| trace_started.elapsed());
    let publication = native_access::publish(
        owner.0 as usize,
        || {
            // 仅测试构建额外观察一次锁内序号，不参与发布判断。
            #[cfg(test)]
            trace.baseline(unsafe { GetClipboardSequenceNumber() });
            let closed = open.close();
            #[cfg(test)]
            trace.close(closed);
            closed
        },
        || {
            let sequence = unsafe { GetClipboardSequenceNumber() };
            #[cfg(test)]
            trace.sequence(sequence);
            sequence
        },
        || {
            let current = unsafe { GetClipboardOwner() }.unwrap_or_default().0 as usize;
            #[cfg(test)]
            trace.owner(current, owner.0 as usize);
            current
        },
    );
    #[cfg(test)]
    let trace = trace.finish();
    {
        let mut observed = observation.lock().unwrap();
        #[cfg(test)]
        {
            observed.publication_trace = Some(trace);
        }
        match publication {
            Ok(publication) => observed.publication = Some(publication),
            Err(reason) => observed.publication_error = Some(reason),
        }
    }
    history.reuse(record.id);
    Ok(())
}

enum Command {
    Write {
        record: Record,
        content: Arc<Content>,
        target: u32,
        delayed: bool,
        observation: Arc<Mutex<Observation>>,
        reply: mpsc::SyncSender<Result<()>>,
    },
    Stop,
}
struct Inner {
    owner: usize,
    capture_access: Arc<CaptureAccess>,
    sender: mpsc::Sender<Command>,
    thread: Mutex<Option<JoinHandle<()>>>,
}
#[derive(Clone)]
pub struct Service(Arc<Inner>);

impl Service {
    pub fn start(logger: Logger, history: Arc<History>) -> Result<Self> {
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
                                    record,
                                    content,
                                    target,
                                    delayed,
                                    observation,
                                    reply,
                                } => {
                                    let _ = reply.send(write(
                                        owner,
                                        record,
                                        &content,
                                        &history,
                                        target,
                                        delayed,
                                        observation,
                                    ));
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
            capture_access: Arc::new(CaptureAccess::default()),
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

    pub fn owner(&self) -> usize {
        self.0.owner
    }

    pub fn capture_access(&self) -> Arc<CaptureAccess> {
        self.0.capture_access.clone()
    }

    pub fn write(
        &self,
        record: Record,
        content: Arc<Content>,
        target: u32,
        delayed: bool,
    ) -> Result<Arc<Mutex<Observation>>> {
        let observation = Arc::new(Mutex::new(Observation::default()));
        let (reply, receiver) = mpsc::sync_channel(1);
        self.command(Command::Write {
            record,
            content,
            target,
            delayed,
            observation: observation.clone(),
            reply,
        })?;
        receiver.recv().map_err(|_| "clipboard_write_failed")??;
        Ok(observation)
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
    fn immediate_publication_is_not_observable_even_without_a_reader() {
        let observation = Observation {
            immediate: true,
            ..Default::default()
        };
        for elapsed in [Duration::ZERO, Duration::from_secs(2)] {
            let report = observation.report(Instant::now() - elapsed);
            assert_eq!(report["status"], "not_observable");
            assert_eq!(report["observable"], false);
            assert_eq!(report["timed_out"], false);
            assert!(report["first_reader"].is_null());
            assert!(report["is_target"].is_null());
            assert!(report["relative_to_send_ms"].is_null());
        }
    }

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
