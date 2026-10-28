// 当前站诊断必须由父任务在用户明确授权空档后单独启用；私有站失败不会调用本入口。
use super::{clipboard, hash, submit_native_message, window};
use crate::{
    history::{CopyEvent, History, content::Content},
    log::Logger,
    settings::Directories,
};
use serde_json::{Value, json};
use std::{
    fs,
    mem::size_of,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_NO_MORE_FILES, GetLastError, GlobalFree, HANDLE, HGLOBAL, HWND,
            SetLastError, WIN32_ERROR,
        },
        System::{
            DataExchange::*, Diagnostics::ToolHelp::*, Memory::*, StationsAndDesktops::*,
            Threading::*,
        },
        UI::WindowsAndMessaging::*,
    },
    core::w,
};

type Result<T> = std::result::Result<T, String>;
const OPT_IN: &str = "M0_CURRENT_STATION_TEST_CLIPBOARD_V1";
const OPT_IN_ENV: &str = "NIANLAI_CURRENT_STATION_CLIPBOARD_OPT_IN";
const OUTPUT_ENV: &str = "NIANLAI_CURRENT_STATION_CLIPBOARD_OUTPUT";
const HELPER_ENV: &str = "NIANLAI_CURRENT_STATION_CLIPBOARD_WINFORMS_EXE";
const SENTINEL: &str = "当前站自有控件诊断\r\nEdit sentinel 🦫 20261027";

fn api<T>(value: windows::core::Result<T>) -> Result<T> {
    value.map_err(|error| error.to_string())
}
fn opt_in(value: Option<&str>) -> Result<()> {
    if value == Some(OPT_IN) {
        Ok(())
    } else {
        Err("缺少本轮用户授权后的固定opt-in；不创建窗口或访问剪贴板".into())
    }
}
fn save(directory: &Path, name: &str, value: &Value) -> Result<()> {
    fs::write(
        directory.join(name),
        serde_json::to_vec_pretty(value).unwrap(),
    )
    .map_err(|error| error.to_string())
}
fn digest(text: &str) -> Value {
    json!({"length_utf16":text.encode_utf16().count(),"sha256_utf8":hex(&hash::sha256(text.as_bytes()).unwrap())})
}
fn checked_text_length(length: i32, last_error: u32) -> Result<usize> {
    if length < 0 || (length == 0 && last_error != 0) {
        Err(format!("GetWindowTextW失败：{last_error}"))
    } else {
        Ok(length as usize)
    }
}
fn with_watchdog(action: impl FnOnce() -> Result<()>) -> Result<()> {
    // 在Guard、窗口及剪贴板操作前启动；不依赖GUI消息循环或日志线程。
    let (finished, deadline) = mpsc::channel();
    let watchdog = thread::spawn(move || {
        if matches!(
            deadline.recv_timeout(Duration::from_secs(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ) {
            std::process::exit(124);
        }
    });
    let result = action();
    let _ = finished.send(());
    let _ = watchdog.join();
    result
}
fn hex(value: &[u8]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn object_name(handle: HANDLE) -> Result<String> {
    let mut name = [0u16; 256];
    unsafe {
        api(GetUserObjectInformationW(
            handle,
            UOI_NAME,
            Some(name.as_mut_ptr().cast()),
            size_of_val(&name) as u32,
            None,
        ))?;
    }
    String::from_utf16(
        &name[..name
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(name.len())],
    )
    .map_err(|error| error.to_string())
}
fn no_product() -> Result<()> {
    unsafe {
        let processes = api(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0))?;
        let result = (|| {
            let mut entry = PROCESSENTRY32W {
                dwSize: size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            api(Process32FirstW(processes, &mut entry))?;
            loop {
                let name = String::from_utf16_lossy(
                    &entry.szExeFile[..entry
                        .szExeFile
                        .iter()
                        .position(|unit| *unit == 0)
                        .unwrap_or(entry.szExeFile.len())],
                );
                if name.eq_ignore_ascii_case("nianlai.exe") {
                    return Err("存在nianlai.exe；仅拒绝，不关闭任何日常进程".into());
                }
                match Process32NextW(processes, &mut entry) {
                    Ok(()) => {}
                    Err(error) if error.code() == ERROR_NO_MORE_FILES.to_hresult() => break,
                    Err(error) => return Err(error.to_string()),
                }
            }
            Ok(())
        })();
        let _ = CloseHandle(processes);
        result
    }
}

#[derive(Clone)]
struct Guard {
    desktop: String,
}
impl Guard {
    fn enter() -> Result<Self> {
        opt_in(std::env::var(OPT_IN_ENV).ok().as_deref())?;
        no_product()?;
        let desktop =
            unsafe { object_name(HANDLE(api(GetThreadDesktop(GetCurrentThreadId()))?.0))? };
        let guard = Self { desktop };
        guard.verify_thread(unsafe { GetCurrentThreadId() })?;
        Ok(guard)
    }
    fn verify_thread(&self, thread: u32) -> Result<()> {
        opt_in(std::env::var(OPT_IN_ENV).ok().as_deref())?;
        no_product()?;
        self.verify_thread_live(thread)
    }
    fn verify_thread_live(&self, thread: u32) -> Result<()> {
        opt_in(std::env::var(OPT_IN_ENV).ok().as_deref())?;
        unsafe {
            if object_name(HANDLE(api(GetProcessWindowStation())?.0))? != "WinSta0" {
                return Err("当前站不是WinSta0；不切换或回退".into());
            }
            let desktop = api(GetThreadDesktop(thread))?;
            let name = object_name(HANDLE(desktop.0))?;
            let mut receiving = 0i32;
            api(GetUserObjectInformationW(
                HANDLE(desktop.0),
                UOI_IO,
                Some(std::ptr::addr_of_mut!(receiving).cast()),
                size_of::<i32>() as u32,
                None,
            ))?;
            if name != self.desktop || receiving == 0 {
                return Err("输入桌面失活/变化；停止后续诊断".into());
            }
        }
        Ok(())
    }
    fn own(&self, hwnd: HWND, process: u32, thread: u32) -> Result<()> {
        self.verify_thread(unsafe { GetCurrentThreadId() })?;
        Self::identity(hwnd, process, thread)?;
        self.verify_thread(thread)
    }
    fn own_live(&self, hwnd: HWND, process: u32, thread: u32) -> Result<()> {
        self.verify_thread_live(unsafe { GetCurrentThreadId() })?;
        Self::identity(hwnd, process, thread)?;
        self.verify_thread_live(thread)
    }
    fn identity(hwnd: HWND, process: u32, thread: u32) -> Result<()> {
        let mut actual = 0;
        if !unsafe { IsWindow(Some(hwnd)) }.as_bool()
            || unsafe { GetWindowThreadProcessId(hwnd, Some(&mut actual)) } != thread
            || actual != process
        {
            return Err("自有窗口PID/TID/存活身份已变化".into());
        }
        Ok(())
    }
}

struct Open(bool);
impl Open {
    fn close(mut self) -> Result<()> {
        unsafe {
            api(CloseClipboard())?;
        }
        self.0 = false;
        Ok(())
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
struct Memory(HGLOBAL);
impl Drop for Memory {
    fn drop(&mut self) {
        unsafe {
            let _ = GlobalFree(Some(self.0));
        }
    }
}

struct Owner {
    hwnd: usize,
    thread: u32,
}
impl Owner {
    fn new(guard: &Guard) -> Result<Self> {
        guard.verify_thread(unsafe { GetCurrentThreadId() })?;
        let hwnd = unsafe {
            api(CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!(""),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                None,
                None,
            ))?
        };
        Ok(Self {
            hwnd: hwnd.0 as usize,
            thread: unsafe { GetCurrentThreadId() },
        })
    }
    fn verify(&self, guard: &Guard) -> Result<HWND> {
        let hwnd = window::hwnd(self.hwnd);
        guard.own(hwnd, unsafe { GetCurrentProcessId() }, self.thread)?;
        Ok(hwnd)
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(window::hwnd(self.hwnd));
        }
    }
}

#[derive(Clone, Copy)]
struct Lease {
    owner: usize,
    sequence: u32,
}
impl Lease {
    fn capture(owner: usize) -> Result<Self> {
        unsafe {
            if GetClipboardOwner().ok() != Some(window::hwnd(owner)) {
                return Err("写入后的owner不是本例自有窗口".into());
            }
            let sequence = GetClipboardSequenceNumber();
            if sequence == 0 {
                return Err("剪贴板序号不可用".into());
            }
            Ok(Self { owner, sequence })
        }
    }
    fn verify(&self) -> Result<()> {
        if unsafe { GetClipboardOwner() }.ok() != Some(window::hwnd(self.owner))
            || unsafe { GetClipboardSequenceNumber() } != self.sequence
        {
            return Err("剪贴板owner/sequence被外部改变；不读取、不继续覆盖".into());
        }
        Ok(())
    }
}

fn read_own_sample(guard: &Guard, reader: &Owner, lease: Lease) -> Result<Value> {
    let owner = reader.verify(guard)?;
    unsafe {
        api(OpenClipboard(Some(owner)))?;
        let open = Open(true);
        // 在锁内确认是本次写入，绝不读取原用户内容或已被外部替换的内容。
        lease.verify()?;
        if GetOpenClipboardWindow().ok() != Some(owner) {
            return Err("自有reader的非NULL打开owner不符".into());
        }
        let handle = api(GetClipboardData(13))?;
        let memory = HGLOBAL(handle.0);
        let size = GlobalSize(memory);
        if size == 0 || size > 4096 || !size.is_multiple_of(2) {
            return Err("本例Unicode块长度异常".into());
        }
        let pointer = GlobalLock(memory);
        if pointer.is_null() {
            return Err("本例Unicode块不可锁定".into());
        }
        let result: Result<Value> = (|| {
            let units = std::slice::from_raw_parts(pointer.cast::<u16>(), size / 2);
            let length = units
                .iter()
                .position(|unit| *unit == 0)
                .ok_or("本例文字缺少NUL")?;
            let text = String::from_utf16(&units[..length]).map_err(|error| error.to_string())?;
            Ok(digest(&text))
        })();
        let _ = GlobalUnlock(memory);
        let mut value = result?;
        open.close()?;
        value["sequence"] = json!(lease.sequence);
        value["known_owner_verified_under_lock"] = json!(true);
        value["close_succeeded"] = json!(true);
        if value["length_utf16"] != digest(SENTINEL)["length_utf16"]
            || value["sha256_utf8"] != digest(SENTINEL)["sha256_utf8"]
        {
            return Err("本例哨兵读回不匹配".into());
        }
        Ok(value)
    }
}

fn minimal_write(guard: &Guard, owner: &Owner, previous: Lease) -> Result<Lease> {
    let owner = owner.verify(guard)?;
    let units: Vec<_> = SENTINEL.encode_utf16().chain([0]).collect();
    unsafe {
        let memory = Memory(api(GlobalAlloc(GMEM_MOVEABLE, units.len() * 2))?);
        let pointer = GlobalLock(memory.0);
        if pointer.is_null() {
            return Err("GlobalLock失败".into());
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), pointer.cast(), units.len());
        let _ = GlobalUnlock(memory.0);
        api(OpenClipboard(Some(owner)))?;
        let open = Open(true);
        previous.verify()?;
        api(EmptyClipboard())?;
        api(SetClipboardData(13, Some(HANDLE(memory.0.0))))?;
        std::mem::forget(memory);
        open.close()?;
        Lease::capture(owner.0 as usize)
    }
}

struct Edit {
    control: usize,
    thread: u32,
    join: Option<thread::JoinHandle<Result<()>>>,
}
impl Edit {
    fn new(guard: Guard) -> Result<Self> {
        Self::new_with_setup(guard, |_| Ok(()))
    }
    fn new_with_setup(
        guard: Guard,
        setup: impl FnOnce(HWND) -> Result<()> + Send + 'static,
    ) -> Result<Self> {
        let (ready, receive) = mpsc::channel();
        let join = thread::spawn(move || -> Result<()> {
            let thread = unsafe { GetCurrentThreadId() };
            let created: Result<(HWND, HWND)> = (|| unsafe {
                guard.verify_thread(thread)?;
                let parent = api(CreateWindowExW(
                    WS_EX_NOACTIVATE,
                    w!("STATIC"),
                    w!(""),
                    WS_OVERLAPPED,
                    0,
                    0,
                    500,
                    200,
                    None,
                    None,
                    None,
                    None,
                ))?;
                let control = match api(CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("EDIT"),
                    w!(""),
                    WS_CHILD | WS_VISIBLE | WINDOW_STYLE(ES_MULTILINE as u32),
                    0,
                    0,
                    480,
                    180,
                    Some(parent),
                    None,
                    None,
                    None,
                )) {
                    Ok(control) => control,
                    Err(error) => {
                        let _ = DestroyWindow(parent);
                        return Err(error);
                    }
                };
                if let Err(error) = setup(control) {
                    let _ = DestroyWindow(parent);
                    return Err(error);
                }
                Ok((parent, control))
            })();
            let (parent, control) = match created {
                Ok(value) => value,
                Err(error) => {
                    let _ = ready.send(Err(error.clone()));
                    return Err(error);
                }
            };
            if ready.send(Ok((control.0 as usize, thread))).is_ok() {
                unsafe {
                    let mut message = MSG::default();
                    while GetMessageW(&mut message, None, 0, 0).0 > 0 {
                        DispatchMessageW(&message);
                    }
                }
            }
            unsafe { api(DestroyWindow(parent)) }
        });
        let (control, thread) = receive
            .recv_timeout(Duration::from_secs(3))
            .map_err(|error| error.to_string())??;
        Ok(Self {
            control,
            thread,
            join: Some(join),
        })
    }
    fn snapshot(&self, guard: &Guard) -> Result<Value> {
        guard.own(
            window::hwnd(self.control),
            unsafe { GetCurrentProcessId() },
            self.thread,
        )?;
        let mut value = [0u16; 1024];
        unsafe {
            SetLastError(WIN32_ERROR(0));
        }
        let length = unsafe { GetWindowTextW(window::hwnd(self.control), &mut value) };
        let length = checked_text_length(length, unsafe { GetLastError().0 })?;
        Ok(digest(
            &String::from_utf16(&value[..length]).map_err(|error| error.to_string())?,
        ))
    }
}
impl Drop for Edit {
    fn drop(&mut self) {
        if let Some(join) = self.join.take() {
            unsafe {
                let _ = PostThreadMessageW(
                    self.thread,
                    WM_QUIT,
                    Default::default(),
                    Default::default(),
                );
            }
            let _ = join.join();
        }
    }
}

fn raw_case(guard: &Guard, directory: &Path, name: &str, lease: Lease, held: bool) -> Result<bool> {
    let edit = Edit::new(guard.clone())?;
    let before = edit.snapshot(guard)?;
    if before["length_utf16"] != 0 {
        return Err("新建自有Edit初始不为空".into());
    }
    guard.own(
        window::hwnd(edit.control),
        unsafe { GetCurrentProcessId() },
        edit.thread,
    )?;
    lease.verify()?;
    let delivery = submit_native_message(
        edit.control,
        edit.thread,
        unsafe { GetCurrentProcessId() },
        200,
    );
    let after = edit.snapshot(guard)?;
    let expected = if held { digest("") } else { digest(SENTINEL) };
    save(
        directory,
        &format!("{name}.json"),
        &json!({"case":name,"before":before,"after":after,"expected":expected,
        "attempted":delivery.attempted,"acknowledged":delivery.acknowledged,"error_code":delivery.error_code,
        "wm_paste_attempts":1,"input_events_sent":0,"focus_requested":false,"target_hidden":true,
        "clipboard_held_by_other_thread":held,"content_matches":after==expected}),
    )?;
    Ok(delivery.attempted && delivery.acknowledged && after == expected)
}

fn held_case(guard: &Guard, directory: &Path, reader: &Owner, lease: Lease) -> Result<bool> {
    let (opened, receive) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let context = guard.clone();
    let holder = thread::spawn(move || -> Result<()> {
        let owner = Owner::new(&context)?;
        let hwnd = owner.verify(&context)?;
        unsafe {
            api(OpenClipboard(Some(hwnd)))?;
        }
        let open = Open(true);
        lease.verify()?;
        let _ = opened.send((owner.hwnd, owner.thread));
        let result = wait
            .recv_timeout(Duration::from_secs(3))
            .map_err(|error| error.to_string());
        open.close()?;
        result
    });
    let (holder_window, holder_thread) = match receive.recv_timeout(Duration::from_secs(3)) {
        Ok(value) => value,
        Err(error) => {
            let _ = release.send(());
            let _ = holder.join();
            return Err(error.to_string());
        }
    };
    let result = (|| {
        let sender = reader.verify(guard)?;
        if holder_window == reader.hwnd || holder_thread == unsafe { GetCurrentThreadId() } {
            return Err("竞争holder与sender没有独立HWND/线程".into());
        }
        let error = match unsafe { OpenClipboard(Some(sender)) } {
            Ok(()) => {
                Open(true).close()?;
                return Err("独立holder持锁时sender仍能OpenClipboard，不能当确定竞争对照".into());
            }
            Err(error) => error.code().0,
        };
        save(
            directory,
            "competition-lock.json",
            &json!({"holder_has_independent_hwnd":true,"holder_thread":holder_thread,"sender_open_failed":true,"sender_open_hresult":error,"lock_handshake_received":true,"not_a_live_root_cause_conclusion":true}),
        )?;
        raw_case(guard, directory, "raw-edit-clipboard-held", lease, true)
    })();
    let _ = release.send(());
    let closed = holder.join().map_err(|_| "自有holder线程异常")?;
    closed?;
    // 释放后不再投递，避免把诊断变成隐含重试。
    result
}

struct Production(clipboard::Service);
impl Drop for Production {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}
fn production_write(guard: &Guard, directory: &Path) -> Result<(Production, Lease)> {
    let (service, lease, _) = production_write_observed(guard, directory)?;
    Ok((service, lease))
}

fn publication_evidence(
    trace: Option<crate::paste::native_publication_trace::PublicationTrace>,
    publication_accepted: bool,
    publication_error: Option<&str>,
) -> Value {
    json!({
        "schema":1,"evidence_collected":trace.as_ref().is_some_and(|trace| trace.complete()),
        "publication_accepted":publication_accepted,"publication_error":publication_error,
        "trace":trace,"trace_build":"cfg_test","same_publish_call":true,
        "locked_sequence_used_for_decision":false,"clipboard_body_read_by_trace":false,
    })
}

fn production_write_observed(
    guard: &Guard,
    directory: &Path,
) -> Result<(
    Production,
    Lease,
    Arc<std::sync::Mutex<clipboard::Observation>>,
)> {
    let directories = Directories {
        settings_file: directory.join("unused-settings.json"),
        data: directory.join("history"),
        logs: directory.join("logs"),
        data_source: "authorized_diagnostic",
        test_data_dir: true,
    };
    fs::create_dir_all(&directories.logs).map_err(|error| error.to_string())?;
    let logger = Logger::with_settings(&directories, 5).map_err(|error| error.to_string())?;
    if logger.directories.logs != directories.logs {
        return Err("日志目录出现fallback；拒绝诊断".into());
    }
    let history = Arc::new(History::new(hash::sha256, logger.clone()));
    let content = Content::text(SENTINEL, hash::sha256)?;
    let ingested = history.ingest(
        content.clone(),
        CopyEvent {
            time_ms: crate::log::now_ms(),
            sequence: 1,
            source_process: "controlled_diagnostic".into(),
            owner_process: "controlled_diagnostic".into(),
            owner_pid: unsafe { GetCurrentProcessId() },
            observed: Instant::now(),
        },
        false,
        Instant::now(),
    )?;
    let id = ingested.record_id.ok_or("内存测试记录创建失败")?;
    guard.verify_thread(unsafe { GetCurrentThreadId() })?;
    let service = Production(clipboard::Service::start(logger, history)?);
    let owner = service.0.owner();
    let mut process = 0;
    let thread = unsafe { GetWindowThreadProcessId(window::hwnd(owner), Some(&mut process)) };
    guard.own(
        window::hwnd(owner),
        unsafe { GetCurrentProcessId() },
        thread,
    )?;
    if process != unsafe { GetCurrentProcessId() } {
        return Err("生产writer owner不属于诊断进程".into());
    }
    let observation = service.0.write(
        clipboard::Record {
            id,
            fingerprint: content.fingerprint,
        },
        content,
        unsafe { GetCurrentProcessId() },
        false,
    )?;
    // 发布拒绝或后续 lease 校验失败也要保留同次闭包值；此处 OS 锁已关闭。
    let (trace, publication_accepted, publication_error) = {
        let observed = observation
            .lock()
            .map_err(|_| "生产writer observation已poison")?;
        (
            observed.publication_trace,
            observed.publication.is_some(),
            observed.publication_error,
        )
    };
    save(
        directory,
        "production-publication.json",
        &publication_evidence(trace, publication_accepted, publication_error),
    )?;
    let lease = Lease::capture(owner)?;
    save(
        directory,
        "production-writer.json",
        &json!({"writer":"production_clipboard_service","delayed":false,"owner_process_verified":true,"owner_thread":thread,"desktop":guard.desktop,"sequence":lease.sequence,"history_mode":"isolated_memory"}),
    )?;
    Ok((service, lease, observation))
}

#[path = "current_station_native_availability_tests.rs"]
mod availability;
struct Receiver {
    child: Child,
    directory: PathBuf,
}
impl Receiver {
    fn stop(&mut self) -> Result<i32> {
        let _ = fs::write(self.directory.join("STOP"), b"");
        let started = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().map_err(|error| error.to_string())? {
                return Ok(status.code().unwrap_or(-1));
            }
            if started.elapsed() > Duration::from_secs(3) {
                let _ = self.child.kill();
                let _ = self.child.wait();
                return Err("仅终止自有helper：正常STOP超时".into());
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Receiver {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
fn read_json(path: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(path).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())
}
fn winforms_case(guard: &Guard, directory: &Path, executable: &Path, lease: Lease) -> Result<bool> {
    guard.verify_thread(unsafe { GetCurrentThreadId() })?;
    lease.verify()?;
    let output = directory.join("winforms-receiver");
    let child = Command::new(executable)
        .args([
            "--scope",
            "authorized-interactive",
            "--opt-in",
            OPT_IN,
            "--expected-station",
            "WinSta0",
            "--expected-desktop",
            &guard.desktop,
            "--output",
        ])
        .arg(&output)
        .args(["--timeout-ms", "15000"])
        .creation_flags(CREATE_NO_WINDOW.0)
        .spawn()
        .map_err(|error| error.to_string())?;
    let mut receiver = Receiver {
        child,
        directory: output,
    };
    let started = Instant::now();
    let ready = loop {
        guard.verify_thread(unsafe { GetCurrentThreadId() })?;
        if receiver.directory.join("failure.json").exists() {
            return Err("WinForms helper准备失败".into());
        }
        if let Ok(value) = read_json(&receiver.directory.join("ready.json")) {
            break value;
        }
        if started.elapsed() > Duration::from_secs(3) {
            return Err("WinForms helper ready超时".into());
        }
        thread::sleep(Duration::from_millis(20));
    };
    if ready["pid"] != receiver.child.id()
        || ready["station"] != "WinSta0"
        || ready["desktop"] != guard.desktop
        || ready["message_loop_running"] != true
    {
        return Err("WinForms ready身份/消息循环不符".into());
    }
    let control = ready["edit"].as_u64().ok_or("helper edit缺失")? as usize;
    let thread = ready["thread"].as_u64().ok_or("helper thread缺失")? as u32;
    guard.own(window::hwnd(control), receiver.child.id(), thread)?;
    let before = read_json(&receiver.directory.join("state.json"))?;
    if before["run_id"] != ready["run_id"]
        || before["pid"] != receiver.child.id()
        || before["length_utf16"] != 0
        || before["sha256_utf8"] != digest("")["sha256_utf8"]
        || before["paste_calls"] != 0
    {
        return Err("WinForms接收器初始状态不为空/已收消息".into());
    }
    lease.verify()?;
    let delivery = submit_native_message(control, thread, receiver.child.id(), 200);
    let started = Instant::now();
    let after = loop {
        guard.verify_thread(unsafe { GetCurrentThreadId() })?;
        if let Ok(value) = read_json(&receiver.directory.join("state.json"))
            && value["wm_paste_completed"] == true
        {
            break value;
        }
        if started.elapsed() > Duration::from_secs(2) {
            return Err("WinForms单次WM_PASTE未报告完成，无重试".into());
        }
        thread::sleep(Duration::from_millis(20));
    };
    let expected = digest(SENTINEL);
    let immediate_matches = after["run_id"] == ready["run_id"]
        && after["pid"] == receiver.child.id()
        && after["paste_calls"] == 1
        && after["length_utf16"] == expected["length_utf16"]
        && after["sha256_utf8"] == expected["sha256_utf8"];
    let exit = receiver.stop()?;
    let final_state = read_json(&receiver.directory.join("state.json"))?;
    let final_matches = final_state["run_id"] == ready["run_id"]
        && final_state["pid"] == receiver.child.id()
        && final_state["paste_calls"] == 1
        && final_state["digest_origin"] == "ui_final_current_text"
        && final_state["final_current_length_utf16"] == expected["length_utf16"]
        && final_state["final_current_sha256_utf8"] == expected["sha256_utf8"]
        && !receiver.directory.join("failure.json").exists();
    save(
        directory,
        "winforms-production-writer.json",
        &json!({"before":before,"immediate_after":after,"final_current":final_state,"expected":expected,"immediate_content_matches":immediate_matches,"final_content_matches":final_matches,"attempted":delivery.attempted,"acknowledged":delivery.acknowledged,"wm_paste_attempts":1,"helper_exit_code":exit,"receiver_executable_sha256":hex(&hash::sha256(&fs::read(executable).map_err(|error|error.to_string())?).unwrap()),"focus_requested":false,"target_hidden":true}),
    )?;
    Ok(delivery.attempted
        && delivery.acknowledged
        && immediate_matches
        && final_matches
        && exit == 0)
}

fn diagnostic(guard: &Guard, directory: &Path, helper: &Path) -> Result<()> {
    let reader = Owner::new(guard)?;
    let (_service, production) = production_write(guard, directory)?;
    save(
        directory,
        "production-clipboard-read.json",
        &read_own_sample(guard, &reader, production)?,
    )?;
    let production_ok = raw_case(
        guard,
        directory,
        "raw-edit-production-writer",
        production,
        false,
    )?;
    let winforms_ok = winforms_case(guard, directory, helper, production)?;
    let minimal = minimal_write(guard, &reader, production)?;
    save(
        directory,
        "minimal-clipboard-read.json",
        &read_own_sample(guard, &reader, minimal)?,
    )?;
    let minimal_ok = raw_case(guard, directory, "raw-edit-minimal-writer", minimal, false)?;
    let held_ok = held_case(guard, directory, &reader, minimal)?;
    lease_final(guard, minimal)?;
    save(
        directory,
        "case-summary.json",
        &json!({"raw_production_content_matches":production_ok,"winforms_production_content_matches":winforms_ok,"raw_minimal_content_matches":minimal_ok,"held_control_matches_empty":held_ok,"no_retry":true,"competition_does_not_establish_live_root_cause":true}),
    )?;
    if production_ok && winforms_ok && minimal_ok && held_ok {
        Ok(())
    } else {
        Err("诊断组存在内容/回执不符；全部保留，无重试".into())
    }
}
fn lease_final(guard: &Guard, lease: Lease) -> Result<()> {
    guard.verify_thread(unsafe { GetCurrentThreadId() })?;
    lease.verify()
}

#[test]
fn current_station_opt_in_requires_exact_explicit_token() {
    for value in [
        None,
        Some(""),
        Some("1"),
        Some("true"),
        Some("M0_CURRENT_STATION_TEST_CLIPBOARD_V1 "),
    ] {
        assert!(opt_in(value).is_err());
    }
    assert!(opt_in(Some(OPT_IN)).is_ok());
}

#[test]
fn current_station_zero_text_length_requires_successful_read() {
    assert_eq!(checked_text_length(0, 0), Ok(0));
    assert!(checked_text_length(0, 5).is_err());
    assert!(checked_text_length(-1, 0).is_err());
    assert_eq!(checked_text_length(12, 0), Ok(12));
}

#[test]
fn publication_evidence_distinguishes_collection_from_acceptance() {
    use crate::paste::{native_access, native_publication_trace::Recorder};
    for owner in [7, 8] {
        let recorder = Recorder::new(|| Duration::ZERO);
        let result = native_access::publish(
            7,
            || {
                recorder.baseline(0);
                recorder.close(true);
                true
            },
            || {
                recorder.sequence(42);
                42
            },
            || {
                recorder.owner(owner, 7);
                owner
            },
        );
        let value = publication_evidence(Some(recorder.finish()), result.is_ok(), result.err());
        assert_eq!(value["evidence_collected"], true);
        assert_eq!(value["publication_accepted"], owner == 7);
        assert_eq!(value["trace"]["locked_sequence"]["value"], 0);
        assert_eq!(value["locked_sequence_used_for_decision"], false);
        assert_eq!(value["clipboard_body_read_by_trace"], false);
    }
    let absent = publication_evidence(None, false, None);
    assert_eq!(absent["evidence_collected"], false);
    assert!(absent["trace"].is_null());
}

#[test]
#[ignore = "只在用户明确授权本轮空档、无nianlai.exe且设置固定opt-in后显式运行；会写固定测试剪贴板"]
fn authorized_current_station_real_controls_clipboard_diagnostic() {
    with_watchdog(|| {
    let guard = Guard::enter()?;
    let base = PathBuf::from(std::env::var_os(OUTPUT_ENV).ok_or("必须提供独立证据目录")?);
    let helper = PathBuf::from(std::env::var_os(HELPER_ENV).ok_or("必须提供独立WinForms helper")?);
    if !helper.is_file() { return Err("WinForms helper不存在，不创建控件".into()); }
    let directory = base.join(format!(
        "current-run-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&directory).map_err(|error|error.to_string())?;
    let executable = std::env::current_exe().map_err(|error|error.to_string())?;
    save(&directory,"source.json",&json!({"schema":1,"execution_mode":"authorized_interactive_diagnostic","authorization_record":"父端仅在用户明确本轮空档授权后设置固定opt-in；授权原文和时间由父端外部证据记录","executable":executable,"executable_sha256":hex(&hash::sha256(&fs::read(&executable).map_err(|error|error.to_string())?).unwrap()),"desktop":guard.desktop,"station":"WinSta0","watchdog_ms":30000,"original_clipboard_read":false,"foreground_or_input_operations":false,"limitations":["隐藏自有目标，不证明生产可见控件资格","不含真实用户搜索/IME/物理B4"]}))?;
    let result = diagnostic(&guard, &directory, &helper);
    save(&directory,"result.json",&json!({"passed":result.is_ok(),"error":result.as_ref().err(),"no_retry":true,"original_clipboard_read":false}))?;
    println!(
        "authorized_clipboard_diagnostic_evidence={}",
        directory.display()
    );
    result
    }).unwrap();
}
