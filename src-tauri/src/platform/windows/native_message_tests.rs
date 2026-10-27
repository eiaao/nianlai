// 显式运行的原生 API 语义实验：仅创建本进程专用线程的 HWND_MESSAGE，完全不读取剪贴板或发送输入。
use super::{native, submit_native_message};
use std::{
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        System::{
            LibraryLoader::GetModuleHandleW,
            Threading::{GetCurrentProcessId, GetCurrentThreadId},
        },
        UI::WindowsAndMessaging::*,
    },
    core::PCWSTR,
};

#[derive(Clone, Copy)]
enum Mode {
    ReturnZero,
    Delay,
    ReplyEarly,
}
struct State {
    mode: Mode,
    calls: Arc<AtomicUsize>,
    entered: mpsc::Sender<()>,
    completed: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}
thread_local! { static STATE: RefCell<Option<State>> = const { RefCell::new(None) }; }
static NEXT: AtomicUsize = AtomicUsize::new(1);

unsafe extern "system" fn procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_PASTE {
        STATE.with(|slot| {
            if let Some(state) = slot.borrow().as_ref() {
                state.calls.fetch_add(1, Ordering::SeqCst);
                let _ = state.entered.send(());
                if matches!(state.mode, Mode::ReplyEarly) {
                    unsafe {
                        let _ = ReplyMessage(LRESULT(0));
                    }
                }
                if !matches!(state.mode, Mode::ReturnZero) {
                    let _ = state.release.recv_timeout(Duration::from_secs(2));
                }
                let _ = state.completed.send(());
            }
        });
        return LRESULT(0);
    }
    if message == WM_CLOSE {
        unsafe {
            PostQuitMessage(0);
        }
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

struct Fixture {
    control: usize,
    owner: u32,
    thread: Option<JoinHandle<Result<(), String>>>,
    calls: Arc<AtomicUsize>,
    entered: mpsc::Receiver<()>,
    completed: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
}
impl Fixture {
    fn start(mode: Mode) -> Self {
        let (ready, receive_ready) = mpsc::channel();
        let (entered, receive_entered) = mpsc::channel();
        let (completed, receive_completed) = mpsc::channel();
        let (release, receive_release) = mpsc::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let thread = thread::Builder::new()
            .name("native-message-fixture".into())
            .spawn(move || unsafe {
                let setup = (|| -> Result<_, String> {
                    let module = GetModuleHandleW(None).map_err(|error| error.to_string())?;
                    let instance = HINSTANCE(module.0);
                    let class: Vec<u16> = format!(
                        "NianlaiNativeFixture{}-{}",
                        GetCurrentProcessId(),
                        NEXT.fetch_add(1, Ordering::SeqCst)
                    )
                    .encode_utf16()
                    .chain([0])
                    .collect();
                    let descriptor = WNDCLASSW {
                        lpfnWndProc: Some(procedure),
                        hInstance: instance,
                        lpszClassName: PCWSTR(class.as_ptr()),
                        ..Default::default()
                    };
                    if RegisterClassW(&descriptor) == 0 {
                        return Err("注册隔离窗口类失败".into());
                    }
                    STATE.with(|slot| {
                        *slot.borrow_mut() = Some(State {
                            mode,
                            calls: count,
                            entered,
                            completed,
                            release: receive_release,
                        })
                    });
                    match CreateWindowExW(
                        WINDOW_EX_STYLE(0),
                        PCWSTR(class.as_ptr()),
                        PCWSTR::null(),
                        WINDOW_STYLE(0),
                        0,
                        0,
                        0,
                        0,
                        Some(HWND_MESSAGE),
                        None,
                        Some(instance),
                        None,
                    ) {
                        Ok(window) => Ok((window, class, instance)),
                        Err(error) => {
                            let _ = UnregisterClassW(PCWSTR(class.as_ptr()), Some(instance));
                            Err(error.to_string())
                        }
                    }
                })();
                let (window, class, instance) = match setup {
                    Ok(value) => value,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return Ok(());
                    }
                };
                if ready
                    .send(Ok((window.0 as usize, GetCurrentThreadId())))
                    .is_ok()
                {
                    let mut message = MSG::default();
                    while GetMessageW(&mut message, None, 0, 0).0 > 0 {
                        DispatchMessageW(&message);
                    }
                }
                // 唯一销毁点在创建线程；WM_CLOSE 只结束自己的消息循环，不提前释放 HWND。
                let destroyed = DestroyWindow(window).map_err(|error| error.to_string());
                let unregistered = UnregisterClassW(PCWSTR(class.as_ptr()), Some(instance))
                    .map_err(|error| error.to_string());
                STATE.with(|slot| *slot.borrow_mut() = None);
                destroyed.and(unregistered)
            })
            .unwrap();
        let (control, owner) = match receive_ready.recv().unwrap() {
            Ok(value) => value,
            Err(error) => {
                thread.join().unwrap().unwrap();
                panic!("{error}");
            }
        };
        Self {
            control,
            owner,
            thread: Some(thread),
            calls,
            entered: receive_entered,
            completed: receive_completed,
            release,
        }
    }
    fn submit(&self, timeout_ms: u32) -> native::Delivery {
        let mut pid = 0;
        let owner =
            unsafe { GetWindowThreadProcessId(super::window::hwnd(self.control), Some(&mut pid)) };
        assert_eq!(pid, unsafe { GetCurrentProcessId() });
        assert_eq!(owner, self.owner);
        assert_ne!(owner, unsafe { GetCurrentThreadId() });
        submit_native_message(self.control, owner, pid, timeout_ms)
    }
    fn wait_completed(&self) {
        self.entered.recv_timeout(Duration::from_secs(2)).unwrap();
        self.completed.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(self.calls.load(Ordering::SeqCst), 1);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = self.release.send(());
            // 地址来自本 fixture 的 ready 通道，且创建线程保证收到关闭前不销毁窗口。
            let _ = unsafe {
                PostMessageW(
                    Some(super::window::hwnd(self.control)),
                    WM_CLOSE,
                    WPARAM(0),
                    LPARAM(0),
                )
            };
            let result = thread.join();
            if !thread::panicking() {
                result.unwrap().unwrap();
            }
        }
    }
}

#[test]
#[ignore = "父代理审查后显式运行：仅本进程 HWND_MESSAGE，不访问剪贴板或输入设备"]
fn native_message_zero_lresult_is_acknowledged_without_content_confirmation() {
    let fixture = Fixture::start(Mode::ReturnZero);
    let delivery = fixture.submit(1000);
    assert!(delivery.attempted && delivery.acknowledged);
    assert_eq!(delivery.submission(), native::Submission::Sent);
    fixture.wait_completed();
}

#[test]
#[ignore = "父代理审查后显式运行：仅本进程 HWND_MESSAGE，不访问剪贴板或输入设备"]
fn native_message_timeout_after_entry_can_still_complete_once() {
    let fixture = Fixture::start(Mode::Delay);
    let delivery = fixture.submit(100);
    assert!(delivery.attempted && !delivery.acknowledged);
    assert_eq!(delivery.submission(), native::Submission::Unknown);
    fixture
        .entered
        .try_recv()
        .expect("超时时目标已进入自己的 WndProc");
    assert!(fixture.completed.try_recv().is_err());
    fixture.release.send(()).unwrap();
    fixture
        .completed
        .recv_timeout(Duration::from_secs(2))
        .unwrap();
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
}

#[test]
#[ignore = "父代理审查后显式运行：仅本进程 HWND_MESSAGE，不访问剪贴板或输入设备"]
fn native_message_early_reply_is_only_acknowledgement() {
    let fixture = Fixture::start(Mode::ReplyEarly);
    let delivery = fixture.submit(1000);
    assert!(delivery.attempted && delivery.acknowledged);
    assert!(
        fixture.completed.try_recv().is_err(),
        "提前回执不能视为处理完成"
    );
    assert_eq!(delivery.submission(), native::Submission::Sent);
    fixture.release.send(()).unwrap();
    fixture.wait_completed();
}
