use super::observe;
use std::{
    cell::RefCell,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::{Console::*, LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
        UI::{Accessibility::*, WindowsAndMessaging::*},
    },
    core::BOOL,
};

pub struct Control {
    stopped: AtomicBool,
    reason: Mutex<Option<String>>,
    events: Mutex<Vec<isize>>,
    pub started: Instant,
    stop_file: PathBuf,
    limit: Duration,
    trial_deadline: Mutex<Option<Instant>>,
}
impl Control {
    pub fn stop(&self, reason: impl Into<String>) {
        let mut saved = self.reason.lock().unwrap_or_else(|e| e.into_inner());
        if !self.stopped.swap(true, Ordering::SeqCst) {
            *saved = Some(reason.into());
        }
    }
    pub fn check(&self) -> Result<(), String> {
        if self.stopped.load(Ordering::SeqCst) {
            Err(self.reason().unwrap_or_else(|| "已中止".into()))
        } else if self
            .trial_deadline
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some_and(|d| Instant::now() >= d)
        {
            Err("timeout：单次试验超过总时限".into())
        } else {
            Ok(())
        }
    }
    pub fn trial_deadline(&self, deadline: Option<Instant>) {
        *self
            .trial_deadline
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = deadline;
    }
    pub fn reason(&self) -> Option<String> {
        self.reason
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn mark(&self) -> usize {
        self.events.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
    pub fn sequence(&self, from: usize) -> Vec<isize> {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(from..)
            .unwrap_or(&[])
            .to_vec()
    }
    pub fn wait(&self, ms: u64) -> Result<(), String> {
        let until = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < until {
            self.check()?;
            thread::sleep(Duration::from_millis(10));
        }
        self.check()
    }
}
struct ThreadState {
    control: Arc<Control>,
    last: Option<(i32, i32)>,
    distance: i64,
}
thread_local! {static STATE:RefCell<Option<ThreadState>>=const{RefCell::new(None)};}
static CONSOLE: Mutex<Option<Arc<Control>>> = Mutex::new(None);
unsafe extern "system" fn console(kind: u32) -> BOOL {
    if matches!(
        kind,
        CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT | CTRL_SHUTDOWN_EVENT
    ) {
        if let Some(c) = CONSOLE.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            c.stop("控制台中止");
        }
        BOOL(1)
    } else {
        BOOL(0)
    }
}
unsafe extern "system" fn keyboard(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if code >= 0 {
            let event = &*(l.0 as *const KBDLLHOOKSTRUCT);
            if !event.flags.contains(LLKHF_INJECTED) {
                STATE.with(|s| {
                    if let Some(s) = s.borrow().as_ref() {
                        s.control.stop("检测到物理键盘输入");
                    }
                });
            }
        }
        CallNextHookEx(None, code, w, l)
    }
}
unsafe extern "system" fn mouse(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if code >= 0 {
            let event = &*(l.0 as *const MSLLHOOKSTRUCT);
            if event.flags & LLMHF_INJECTED == 0 {
                STATE.with(|s| {
                    if let Some(s) = s.borrow_mut().as_mut() {
                        if w.0 as u32 != WM_MOUSEMOVE {
                            s.control.stop("检测到物理鼠标按键或滚轮");
                        } else {
                            if let Some((x, y)) = s.last {
                                s.distance += (event.pt.x as i64 - x as i64).abs()
                                    + (event.pt.y as i64 - y as i64).abs();
                            }
                            s.last = Some((event.pt.x, event.pt.y));
                            if s.distance > 8 {
                                s.control.stop("物理鼠标累计移动超过 8 像素");
                            }
                        }
                    }
                });
            } else {
                STATE.with(|s| {
                    if let Some(s) = s.borrow_mut().as_mut() {
                        s.last = Some((event.pt.x, event.pt.y));
                    }
                });
            }
        }
        CallNextHookEx(None, code, w, l)
    }
}
unsafe extern "system" fn foreground(
    _: HWINEVENTHOOK,
    _: u32,
    h: HWND,
    _: i32,
    _: i32,
    _: u32,
    _: u32,
) {
    STATE.with(|s| {
        if let Some(s) = s.borrow().as_ref() {
            s.control
                .events
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(h.0 as isize);
        }
    });
}
pub struct Guard {
    pub control: Arc<Control>,
    thread: Option<JoinHandle<()>>,
    thread_id: u32,
}
impl Guard {
    pub fn install(stop_file: PathBuf, max_minutes: u64) -> Result<Self, String> {
        let control = Arc::new(Control {
            stopped: AtomicBool::new(false),
            reason: Mutex::new(None),
            events: Mutex::new(vec![]),
            started: Instant::now(),
            stop_file,
            limit: Duration::from_secs(max_minutes * 60),
            trial_deadline: Mutex::new(None),
        });
        let c = control.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("focus-matrix-safety".into())
            .spawn(move || unsafe {
                let result = (|| -> Result<_, String> {
                    let module = GetModuleHandleW(None).map_err(|e| e.to_string())?;
                    let keyboard =
                        SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), Some(module.into()), 0)
                            .map_err(|e| e.to_string())?;
                    let mouse =
                        match SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), Some(module.into()), 0) {
                            Ok(h) => h,
                            Err(e) => {
                                let _ = UnhookWindowsHookEx(keyboard);
                                return Err(e.to_string());
                            }
                        };
                    let event = SetWinEventHook(
                        EVENT_SYSTEM_FOREGROUND,
                        EVENT_SYSTEM_FOREGROUND,
                        None,
                        Some(foreground),
                        0,
                        0,
                        WINEVENT_OUTOFCONTEXT,
                    );
                    if event.0.is_null() {
                        let _ = UnhookWindowsHookEx(keyboard);
                        let _ = UnhookWindowsHookEx(mouse);
                        return Err("前台事件钩子安装失败".into());
                    }
                    Ok((keyboard, mouse, event))
                })();
                let Ok((keyboard, mouse, event)) = result else {
                    let _ = tx.send(Err(result.err().unwrap_or_default()));
                    return;
                };
                let mut msg = MSG::default();
                let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
                STATE.with(|s| {
                    *s.borrow_mut() = Some(ThreadState {
                        control: c.clone(),
                        last: None,
                        distance: 0,
                    })
                });
                let _ = tx.send(Ok(GetCurrentThreadId()));
                let mut checked = Instant::now();
                let mut empty_since: Option<Instant> = None;
                'outer: loop {
                    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        if msg.message == WM_QUIT {
                            break 'outer;
                        }
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                    if checked.elapsed() >= Duration::from_millis(50) {
                        checked = Instant::now();
                        if c.stop_file.exists() {
                            c.stop("检测到输出目录 STOP 文件");
                        }
                        if c.started.elapsed() > c.limit {
                            c.stop("超过 --max-minutes");
                        }
                        // 面板收起、气泡关闭的瞬间前台会短暂为空；持续 2 秒才按锁屏处理。
                        if observe::foreground() != 0 {
                            empty_since = None;
                        } else if empty_since.get_or_insert(checked).elapsed()
                            >= Duration::from_secs(2)
                        {
                            c.stop("前台窗口持续为空，可能锁屏");
                        }
                    }
                    // 低层钩子的回调要等本线程取消息才执行；有消息立即醒来，不给每个按键加延迟。
                    let _ = MsgWaitForMultipleObjects(None, false, 50, QS_ALLINPUT);
                }
                let _ = UnhookWindowsHookEx(keyboard);
                let _ = UnhookWindowsHookEx(mouse);
                let _ = UnhookWinEvent(event);
                STATE.with(|s| *s.borrow_mut() = None);
            })
            .map_err(|e| e.to_string())?;
        let thread_id = rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "中止监视线程启动超时")??;
        *CONSOLE.lock().unwrap_or_else(|e| e.into_inner()) = Some(control.clone());
        let guard = Self {
            control,
            thread: Some(thread),
            thread_id,
        };
        unsafe {
            SetConsoleCtrlHandler(Some(console), true).map_err(|e| e.to_string())?;
        }
        Ok(guard)
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        unsafe {
            let _ = SetConsoleCtrlHandler(Some(console), false);
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        *CONSOLE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}
