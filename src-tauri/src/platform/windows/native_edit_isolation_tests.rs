// 显式运行的真实 Edit 实验。只有专用子进程的私有 window station 可以访问剪贴板。
use super::{hash, submit_native_message, window};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fs,
    mem::size_of,
    os::windows::{ffi::OsStrExt, process::CommandExt},
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, GetLastError, GlobalFree, HANDLE, HGLOBAL, HWND, SetLastError,
            WAIT_OBJECT_0, WIN32_ERROR,
        },
        System::{
            DataExchange::{
                CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardOwner,
                GetClipboardSequenceNumber, GetOpenClipboardWindow, OpenClipboard,
                SetClipboardData,
            },
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock},
            StationsAndDesktops::*,
            Threading::*,
        },
        UI::WindowsAndMessaging::*,
    },
    core::{PCWSTR, PWSTR, w},
};

type Result<T> = std::result::Result<T, String>;
const TEST: &str = "platform::windows::paste::native_edit_isolation_tests::private_station_real_edit_receives_one_native_paste";
const PHASE: &str = "NIANLAI_NATIVE_EDIT_PHASE";
const OUTPUT: &str = "NIANLAI_NATIVE_EDIT_OUTPUT";
const STATION: &str = "NIANLAI_NATIVE_EDIT_STATION";
const DESKTOP: &str = "NIANLAI_NATIVE_EDIT_DESKTOP";
const SENTINEL: &str = "隔离原生粘贴\r\nEdit sentinel 20261027 🦫";

fn api<T>(result: windows::core::Result<T>) -> Result<T> {
    result.map_err(|error| error.to_string())
}
fn wide(value: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain([0]).collect()
}
fn save(directory: &Path, name: &str, value: &Value) -> Result<()> {
    fs::write(
        directory.join(name),
        serde_json::to_vec_pretty(value).unwrap(),
    )
    .map_err(|error| error.to_string())
}
fn digest(text: &str) -> Value {
    let hash = hash::sha256(text.as_bytes()).unwrap();
    json!({"length_utf16":text.encode_utf16().count(),
        "sha256_utf8":hash.iter().map(|byte| format!("{byte:02x}")).collect::<String>()})
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

#[derive(Clone)]
struct Scope {
    station: String,
    desktop: String,
}
impl Scope {
    fn verify(&self, thread: u32) -> Result<Value> {
        unsafe {
            let station = object_name(HANDLE(api(GetProcessWindowStation())?.0))?;
            let desktop = api(GetThreadDesktop(thread))?;
            let desktop_name = object_name(HANDLE(desktop.0))?;
            let mut input = -1i32;
            api(GetUserObjectInformationW(
                HANDLE(desktop.0),
                UOI_IO,
                Some(std::ptr::addr_of_mut!(input).cast()),
                size_of::<i32>() as u32,
                None,
            ))?;
            if !self.station.starts_with("NianlaiNativeProbe-")
                || !self.desktop.starts_with("NianlaiNativeProbe-Desk-")
                || station.eq_ignore_ascii_case("WinSta0")
                || station != self.station
                || desktop_name != self.desktop
                || input != 0
            {
                return Err(format!(
                    "隔离核验失败：station={station} desktop={desktop_name} input={input}"
                ));
            }
            Ok(
                json!({"station":station,"desktop":desktop_name,"thread":thread,
                "input_desktop":false,"private_station_verified":true}),
            )
        }
    }
}

// bootstrap 不访问剪贴板。worker 在创建进程时就绑定私有站，后续线程默认桌面也属于该站。
fn bootstrap(directory: &Path) -> Result<()> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        ^ (u128::from(std::process::id()) << 64);
    let scope = Scope {
        station: format!("NianlaiNativeProbe-{nonce:032x}"),
        desktop: format!("NianlaiNativeProbe-Desk-{nonce:032x}"),
    };
    unsafe {
        let station_name = wide(&scope.station);
        SetLastError(WIN32_ERROR(0));
        let station = api(CreateWindowStationW(
            PCWSTR(station_name.as_ptr()),
            0,
            (WINSTA_ENUMDESKTOPS
                | WINSTA_READATTRIBUTES
                | WINSTA_ACCESSCLIPBOARD
                | WINSTA_CREATEDESKTOP
                | WINSTA_WRITEATTRIBUTES
                | WINSTA_ACCESSGLOBALATOMS) as u32,
            None,
        ))
        .map_err(|error| format!("CreateWindowStationW: {error}"))?;
        if GetLastError().0 == 183 {
            return Err("私有 station 名称已存在，拒绝复用".into());
        }
        api(SetProcessWindowStation(station))
            .map_err(|error| format!("SetProcessWindowStation: {error}"))?;
        let desktop_name = wide(&scope.desktop);
        let desktop = api(CreateDesktopW(
            PCWSTR(desktop_name.as_ptr()),
            PCWSTR::null(),
            None,
            DESKTOP_CONTROL_FLAGS(0),
            DESKTOP_CREATEWINDOW.0
                | DESKTOP_READOBJECTS.0
                | DESKTOP_WRITEOBJECTS.0
                | DESKTOP_READ_CONTROL.0,
            None,
        ))
        .map_err(|error| format!("CreateDesktopW: {error}"))?;
        api(SetThreadDesktop(desktop)).map_err(|error| format!("SetThreadDesktop: {error}"))?;
        save(
            directory,
            "bootstrap-isolation.json",
            &scope
                .verify(GetCurrentThreadId())
                .map_err(|error| format!("Scope::verify: {error}"))?,
        )?;
        // 当前关联的 station/desktop 不切回 WinSta0 关闭；专用 bootstrap 退出由系统回收。
    }
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let command = format!(
        "\"{}\" --exact {TEST} --ignored --nocapture --test-threads=1",
        executable.display()
    );
    launch_private(
        &scope,
        &executable,
        &command,
        &[
            (PHASE, "worker"),
            (STATION, &scope.station),
            (DESKTOP, &scope.desktop),
        ],
    )
}

fn launch_private(
    scope: &Scope,
    executable: &Path,
    command: &str,
    overrides: &[(&str, &str)],
) -> Result<()> {
    let mut variables: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(key, _)| {
            !overrides
                .iter()
                .any(|(name, _)| key.to_string_lossy().eq_ignore_ascii_case(name))
        })
        .collect();
    variables.extend(
        overrides
            .iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value))),
    );
    variables.sort_by_key(|(key, _)| key.to_string_lossy().to_ascii_uppercase());
    let mut environment = Vec::new();
    for (key, value) in variables {
        environment.extend(key.encode_wide());
        environment.push(b'=' as u16);
        environment.extend(value.encode_wide());
        environment.push(0);
    }
    environment.push(0);
    let executable = wide(executable);
    let mut command = wide(command);
    let mut desktop = wide(format!("{}\\{}", scope.station, scope.desktop));
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        lpDesktop: PWSTR(desktop.as_mut_ptr()),
        ..Default::default()
    };
    let mut process = PROCESS_INFORMATION::default();
    unsafe {
        api(CreateProcessW(
            PCWSTR(executable.as_ptr()),
            Some(PWSTR(command.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            Some(environment.as_ptr().cast()),
            PCWSTR::null(),
            &startup,
            &mut process,
        ))?;
        let _ = CloseHandle(process.hThread);
        if WaitForSingleObject(process.hProcess, 20000) != WAIT_OBJECT_0 {
            // 仅使用本函数刚创建的进程句柄，不按 PID 终止其他进程。
            let _ = TerminateProcess(process.hProcess, 124);
            let _ = WaitForSingleObject(process.hProcess, 2000);
            let _ = CloseHandle(process.hProcess);
            return Err("私有 worker 超过20秒已终止".into());
        }
        let mut code = 0;
        let result = api(GetExitCodeProcess(process.hProcess, &mut code));
        let _ = CloseHandle(process.hProcess);
        result?;
        if code != 0 {
            return Err(format!("私有 worker 退出码 {code}，详见 worker-error.json"));
        }
    }
    Ok(())
}

struct Edit {
    control: usize,
    thread_id: u32,
    thread: Option<thread::JoinHandle<Result<()>>>,
}
impl Edit {
    fn start(scope: Scope) -> Result<Self> {
        let (ready, receive) = mpsc::channel();
        let worker = thread::spawn(move || -> Result<()> {
            unsafe {
                let thread_id = GetCurrentThreadId();
                let result: Result<(HWND, HWND)> = (|| {
                    scope.verify(thread_id)?;
                    // 使用系统 EDIT 类，不拦截或自行实现 WM_PASTE。
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
                    let control = api(CreateWindowExW(
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
                    ))?;
                    Ok((parent, control))
                })();
                let (parent, control) = match result {
                    Ok(value) => value,
                    Err(error) => {
                        let _ = ready.send(Err(error.clone()));
                        return Err(error);
                    }
                };
                if ready.send(Ok((control.0 as usize, thread_id))).is_ok() {
                    let mut message = MSG::default();
                    while GetMessageW(&mut message, None, 0, 0).0 > 0 {
                        DispatchMessageW(&message);
                    }
                }
                api(DestroyWindow(parent))
            }
        });
        let (control, thread_id) = receive
            .recv_timeout(Duration::from_secs(3))
            .map_err(|error| error.to_string())??;
        Ok(Self {
            control,
            thread_id,
            thread: Some(worker),
        })
    }
    fn text(&self) -> Result<String> {
        let mut value = [0u16; 1024];
        let length = unsafe { GetWindowTextW(window::hwnd(self.control), &mut value) };
        String::from_utf16(&value[..length.max(0) as usize]).map_err(|error| error.to_string())
    }
}
impl Drop for Edit {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            unsafe {
                let _ = PostThreadMessageW(
                    self.thread_id,
                    WM_QUIT,
                    Default::default(),
                    Default::default(),
                );
            }
            let _ = thread.join();
        }
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

fn minimal_write(scope: &Scope, owner: HWND) -> Result<Value> {
    scope.verify(unsafe { GetCurrentThreadId() })?;
    let data: Vec<_> = SENTINEL.encode_utf16().chain([0]).collect();
    unsafe {
        let memory = Memory(api(GlobalAlloc(GMEM_MOVEABLE, data.len() * 2))?);
        let pointer = GlobalLock(memory.0);
        if pointer.is_null() {
            return Err("GlobalLock失败".into());
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), pointer.cast(), data.len());
        let _ = GlobalUnlock(memory.0);
        api(OpenClipboard(Some(owner)))?;
        let opened = Open(true);
        api(EmptyClipboard())?;
        api(SetClipboardData(13, Some(HANDLE(memory.0.0))))?;
        std::mem::forget(memory);
        opened.close()?;
    }
    Ok(json!({"writer":"minimal_win32_eager","close_succeeded":true}))
}

fn read_clipboard(scope: &Scope, owner: HWND) -> Result<Value> {
    scope.verify(unsafe { GetCurrentThreadId() })?;
    unsafe {
        api(OpenClipboard(Some(owner)))?;
        let opened = Open(true);
        if GetOpenClipboardWindow().ok() != Some(owner) {
            return Err("非NULL私有reader owner不匹配".into());
        }
        let data = api(GetClipboardData(13))?;
        let memory = HGLOBAL(data.0);
        let size = GlobalSize(memory);
        let pointer = GlobalLock(memory);
        if pointer.is_null() || size > 4096 {
            return Err("私有剪贴板数据不可读/超界".into());
        }
        let units = std::slice::from_raw_parts(pointer.cast::<u16>(), size / 2);
        let length = units
            .iter()
            .position(|unit| *unit == 0)
            .ok_or("文字缺少NUL")?;
        let text = String::from_utf16(&units[..length]).map_err(|error| error.to_string())?;
        let _ = GlobalUnlock(memory);
        opened.close()?;
        let mut result = digest(&text);
        result["sequence"] = json!(GetClipboardSequenceNumber());
        result["closed_after_read"] = json!(GetOpenClipboardWindow().is_err());
        result["open_owner_verified"] = json!(true);
        result["close_succeeded"] = json!(true);
        result["owner_known"] = json!(GetClipboardOwner().is_ok());
        Ok(result)
    }
}

fn worker(scope: &Scope, directory: &Path) -> Result<()> {
    save(
        directory,
        "worker-isolation.json",
        &scope.verify(unsafe { GetCurrentThreadId() })?,
    )?;
    let edit = Edit::start(scope.clone())?;
    scope.verify(edit.thread_id)?;
    if !unsafe { IsWindowUnicode(window::hwnd(edit.control)) }.as_bool() {
        return Err("系统EDIT不是Unicode".into());
    }
    let before = digest(&edit.text()?);
    unsafe {
        let owner = api(CreateWindowExW(
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
        ))?;
        let writer = minimal_write(scope, owner)?;
        let clipboard = read_clipboard(scope, owner)?;
        let expected = digest(SENTINEL);
        if clipboard["length_utf16"] != expected["length_utf16"]
            || clipboard["sha256_utf8"] != expected["sha256_utf8"]
        {
            return Err("写后独立读取与固定哨兵不符".into());
        }
        // 每例恰好一次生产 wrapper；不调用 foreground/SetFocus/AttachThreadInput。
        let delivery =
            submit_native_message(edit.control, edit.thread_id, GetCurrentProcessId(), 200);
        let after = digest(&edit.text()?);
        save(
            directory,
            "raw-edit-minimal-writer.json",
            &json!({"schema":1,"case":"raw_edit_minimal_eager",
            "scope":scope.verify(GetCurrentThreadId())?,"sender_pid":GetCurrentProcessId(),
            "receiver_same_process":true,"receiver_thread":edit.thread_id,"real_class":"EDIT",
            "control_unicode":true,"focus_requested":false,"input_events_sent":0,
            "wm_paste_attempts":1,"attempted":delivery.attempted,"acknowledged":delivery.acknowledged,
                "error_code":delivery.error_code,"writer":writer,"expected":expected,"before":before,"clipboard_after_write":clipboard,
            "after":after,"content_matches":after==expected,
            "limitation":"未运行输入桌面guard；尚未覆盖生产writer/跨进程WinForms/活跃IME"}),
        )?;
        api(DestroyWindow(owner))?;
        if !delivery.attempted || !delivery.acknowledged || after != expected {
            return Err("真实EDIT未得到预期文字，保留证据".into());
        }
    }
    Ok(())
}

#[test]
#[ignore = "显式隔离实验：独立子进程私有window station真实EDIT，只访问私有剪贴板"]
fn private_station_real_edit_receives_one_native_paste() {
    let phase = std::env::var(PHASE).unwrap_or_default();
    if phase == "bootstrap" || phase == "worker" {
        let directory = PathBuf::from(std::env::var_os(OUTPUT).expect("实验目录缺失"));
        let result = if phase == "bootstrap" {
            bootstrap(&directory)
        } else {
            // 即使bootstrap异常退出，worker也会在任何UI/剪贴板操作前装好的独立期限内结束自身。
            let (finished, deadline) = mpsc::channel();
            let watchdog = thread::spawn(move || {
                if matches!(
                    deadline.recv_timeout(Duration::from_secs(10)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    std::process::exit(124);
                }
            });
            let scope = Scope {
                station: std::env::var(STATION).expect("station缺失"),
                desktop: std::env::var(DESKTOP).expect("desktop缺失"),
            };
            let result = worker(&scope, &directory);
            let _ = finished.send(());
            let _ = watchdog.join();
            result
        };
        if let Err(error) = &result {
            let _ = save(
                &directory,
                &format!("{phase}-error.json"),
                &json!({"error":error,"no_fallback":true}),
            );
        }
        result.unwrap();
        return;
    }
    let base = PathBuf::from(std::env::var_os(OUTPUT).expect("显式运行必须提供实验目录"));
    let directory = base.join(format!(
        "run-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&directory).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            TEST,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PHASE, "bootstrap")
        .env(OUTPUT, &directory)
        .creation_flags(CREATE_NO_WINDOW.0)
        .spawn()
        .unwrap();
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            println!("private_edit_evidence={}", directory.display());
            assert!(status.success(), "隔离实验失败，保留错误证据");
            break;
        }
        if started.elapsed() > Duration::from_secs(25) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("专用bootstrap超时");
        }
        thread::sleep(Duration::from_millis(20));
    }
}
