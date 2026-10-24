//! G0 焦点诊断小工具。
//!
//! 实时显示前台窗口、其中有焦点的控件、插入符位置和进程权限，并用一个不抢焦点、
//! 鼠标可以穿透的红框，把焦点控件框出来。用来在 G0 里看清“面板呼出前后，焦点到底在哪”。
//!
//! 用法：
//!   focus-probe               持续监听：焦点变化时打印一行，并在屏幕上画框
//!   focus-probe --once        打印当前状态后退出
//!   focus-probe --json        每行输出一条 JSON，方便留存测试记录
//!   focus-probe --no-overlay  只打印，不画框
//!
//! 只读取窗口标题、类名和进程名，不读取任何输入框的内容。

#[cfg(not(windows))]
fn main() {
    eprintln!("focus-probe 目前只支持 Windows");
}

#[cfg(windows)]
fn main() {
    win::run();
}

#[cfg(windows)]
mod win {
    use std::cell::RefCell;
    use std::time::{SystemTime, UNIX_EPOCH};

    use windows::Win32::Foundation::{
        COLORREF, CloseHandle, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
    };
    use windows::Win32::Graphics::Gdi::{ClientToScreen, CreateSolidBrush};
    use windows::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::{
        GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::{PCWSTR, PWSTR, w};

    /// 某一时刻的焦点状态。窗口句柄存成整数，方便比较和打印。
    #[derive(Clone, Default, PartialEq)]
    struct Snapshot {
        fg: isize,
        fg_class: String,
        fg_title: String,
        pid: u32,
        process: String,
        elevated: Option<bool>,
        fg_noactivate: bool,
        fg_toolwindow: bool,
        focus: isize,
        focus_class: String,
        caret: Option<RECT>,
        focus_rect: Option<RECT>,
        menu_mode: bool,
    }

    #[derive(Default)]
    struct State {
        last: Snapshot,
        json: bool,
        start: u128,
        self_elevated: Option<bool>,
        bars: Option<[isize; 4]>,
    }

    thread_local! {
        static STATE: RefCell<State> = RefCell::new(State::default());
    }

    fn now_ms() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    }

    fn hwnd(raw: isize) -> HWND {
        HWND(raw as _)
    }

    fn wide(buf: &[u16], len: i32) -> String {
        String::from_utf16_lossy(&buf[..len.clamp(0, buf.len() as i32) as usize])
    }

    fn class_name(h: HWND) -> String {
        if h.0.is_null() {
            return String::new();
        }
        let mut buf = [0u16; 256];
        let n = unsafe { GetClassNameW(h, &mut buf) };
        wide(&buf, n)
    }

    fn window_title(h: HWND) -> String {
        let mut buf = [0u16; 512];
        let n = unsafe { GetWindowTextW(h, &mut buf) };
        wide(&buf, n).chars().take(60).collect()
    }

    /// 进程名和是否以管理员身份运行。拿不到令牌时返回 None：
    /// 普通权限的程序通常读不了管理员进程的令牌，这本身就是一个信号。
    fn process_info(pid: u32) -> (String, Option<bool>) {
        unsafe {
            let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return ("（无法打开进程）".into(), None);
            };
            let mut buf = [0u16; 1024];
            let mut size = buf.len() as u32;
            let name = if QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut size,
            )
            .is_ok()
            {
                let full = String::from_utf16_lossy(&buf[..size as usize]);
                full.rsplit('\\').next().unwrap_or(&full).to_string()
            } else {
                "（未知进程）".into()
            };
            let mut token = HANDLE::default();
            let elevated = if OpenProcessToken(process, TOKEN_QUERY, &mut token).is_ok() {
                let mut info = TOKEN_ELEVATION::default();
                let mut returned = 0u32;
                let ok = GetTokenInformation(
                    token,
                    TokenElevation,
                    Some(&mut info as *mut TOKEN_ELEVATION as *mut _),
                    size_of::<TOKEN_ELEVATION>() as u32,
                    &mut returned,
                )
                .is_ok();
                let _ = CloseHandle(token);
                ok.then_some(info.TokenIsElevated != 0)
            } else {
                None
            };
            let _ = CloseHandle(process);
            (name, elevated)
        }
    }

    fn capture() -> Snapshot {
        unsafe {
            let fg = GetForegroundWindow();
            let mut snap = Snapshot {
                fg: fg.0 as isize,
                ..Default::default()
            };
            if fg.0.is_null() {
                return snap;
            }
            let mut pid = 0u32;
            let thread = GetWindowThreadProcessId(fg, Some(&mut pid));
            snap.pid = pid;
            snap.fg_class = class_name(fg);
            snap.fg_title = window_title(fg);
            let ex_style = GetWindowLongPtrW(fg, GWL_EXSTYLE) as u32;
            snap.fg_noactivate = ex_style & WS_EX_NOACTIVATE.0 != 0;
            snap.fg_toolwindow = ex_style & WS_EX_TOOLWINDOW.0 != 0;
            (snap.process, snap.elevated) = process_info(pid);

            // GetGUIThreadInfo 不需要 AttachThreadInput 就能读到别的线程的焦点控件和插入符。
            let mut gui = GUITHREADINFO {
                cbSize: size_of::<GUITHREADINFO>() as u32,
                ..Default::default()
            };
            if GetGUIThreadInfo(thread, &mut gui).is_ok() {
                snap.focus = gui.hwndFocus.0 as isize;
                snap.focus_class = class_name(gui.hwndFocus);
                snap.menu_mode = gui.flags.0 & (GUI_INMENUMODE.0 | GUI_POPUPMENUMODE.0) != 0;
                if !gui.hwndCaret.0.is_null() {
                    let mut top_left = POINT {
                        x: gui.rcCaret.left,
                        y: gui.rcCaret.top,
                    };
                    let mut bottom_right = POINT {
                        x: gui.rcCaret.right,
                        y: gui.rcCaret.bottom,
                    };
                    let _ = ClientToScreen(gui.hwndCaret, &mut top_left);
                    let _ = ClientToScreen(gui.hwndCaret, &mut bottom_right);
                    snap.caret = Some(RECT {
                        left: top_left.x,
                        top: top_left.y,
                        right: bottom_right.x,
                        bottom: bottom_right.y,
                    });
                }
                let target = if gui.hwndFocus.0.is_null() {
                    fg
                } else {
                    gui.hwndFocus
                };
                let mut rect = RECT::default();
                if GetWindowRect(target, &mut rect).is_ok() {
                    snap.focus_rect = Some(rect);
                }
            }
            snap
        }
    }

    fn json_str(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }

    fn print(s: &Snapshot, json: bool, start: u128, self_elevated: Option<bool>) {
        let elapsed = now_ms().saturating_sub(start) as f64 / 1000.0;
        if json {
            let caret = s.caret.map_or("null".to_string(), |r| {
                format!("[{},{},{},{}]", r.left, r.top, r.right, r.bottom)
            });
            let elevated = s.elevated.map_or("null".to_string(), |e| e.to_string());
            println!(
                "{{\"t\":{elapsed:.3},\"fg\":{},\"fg_class\":{},\"fg_title\":{},\"pid\":{},\"process\":{},\"elevated\":{elevated},\"fg_noactivate\":{},\"fg_toolwindow\":{},\"focus\":{},\"focus_class\":{},\"caret\":{caret},\"menu_mode\":{}}}",
                s.fg,
                json_str(&s.fg_class),
                json_str(&s.fg_title),
                s.pid,
                json_str(&s.process),
                s.fg_noactivate,
                s.fg_toolwindow,
                s.focus,
                json_str(&s.focus_class),
                s.menu_mode,
            );
            return;
        }
        if s.fg == 0 {
            println!("[{elapsed:>8.3}s] 没有前台窗口");
            return;
        }
        let rights = match (s.elevated, self_elevated) {
            (Some(true), Some(false)) => "管理员（普通权限发的按键会被系统拦下）",
            (Some(true), _) => "管理员",
            (Some(false), _) => "普通",
            (None, _) => "读不到（可能是管理员）",
        };
        let focus = if s.focus_class.is_empty() {
            "（无）"
        } else {
            s.focus_class.as_str()
        };
        let caret = s.caret.map_or("无插入符".to_string(), |r| {
            format!("插入符 ({}, {})", r.left, r.top)
        });
        let mut flags = Vec::new();
        if s.fg_noactivate {
            flags.push("不激活窗口");
        }
        if s.fg_toolwindow {
            flags.push("工具窗口");
        }
        if s.menu_mode {
            flags.push("菜单打开中");
        }
        let flags = if flags.is_empty() {
            String::new()
        } else {
            format!(" | {}", flags.join("、"))
        };
        println!(
            "[{elapsed:>8.3}s] 前台 {}（{}）“{}” | 焦点控件 {focus} | {caret} | 权限 {rights}{flags}",
            s.process, s.fg_class, s.fg_title
        );
    }

    unsafe extern "system" fn bar_proc(
        h: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // 框线被点到也不激活自己，保证不打扰被观察的窗口。
        if msg == WM_MOUSEACTIVATE {
            return LRESULT(MA_NOACTIVATE as isize);
        }
        unsafe { DefWindowProcW(h, msg, wparam, lparam) }
    }

    /// 四条细窗口拼成一个红框：置顶、不激活、不进任务栏、鼠标穿透。
    fn create_bars() -> windows::core::Result<[isize; 4]> {
        unsafe {
            let module = GetModuleHandleW(PCWSTR::null())?;
            let class = w!("NianlaiFocusProbeBar");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(bar_proc),
                hInstance: module.into(),
                lpszClassName: class,
                hbrBackground: CreateSolidBrush(COLORREF(0x0000_00FF)),
                ..Default::default()
            };
            RegisterClassW(&wc);
            let mut bars = [0isize; 4];
            for bar in bars.iter_mut() {
                let h = CreateWindowExW(
                    WS_EX_TOPMOST
                        | WS_EX_TOOLWINDOW
                        | WS_EX_NOACTIVATE
                        | WS_EX_LAYERED
                        | WS_EX_TRANSPARENT,
                    class,
                    w!(""),
                    WS_POPUP,
                    0,
                    0,
                    0,
                    0,
                    None,
                    None,
                    Some(module.into()),
                    None,
                )?;
                SetLayeredWindowAttributes(h, COLORREF(0), 220, LWA_ALPHA)?;
                *bar = h.0 as isize;
            }
            Ok(bars)
        }
    }

    fn place_bars(bars: &[isize; 4], rect: Option<RECT>) {
        unsafe {
            let Some(r) = rect else {
                for &bar in bars {
                    let _ = ShowWindow(hwnd(bar), SW_HIDE);
                }
                return;
            };
            let t = 3;
            let width = r.right - r.left;
            let height = r.bottom - r.top;
            let edges = [
                (r.left - t, r.top - t, width + 2 * t, t),
                (r.left - t, r.bottom, width + 2 * t, t),
                (r.left - t, r.top, t, height),
                (r.right, r.top, t, height),
            ];
            for (&bar, (x, y, w, h)) in bars.iter().zip(edges) {
                let _ = SetWindowPos(
                    hwnd(bar),
                    Some(HWND_TOPMOST),
                    x,
                    y,
                    w.max(1),
                    h.max(1),
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
        }
    }

    fn refresh() {
        let snap = capture();
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            let last = &state.last;
            let changed = snap.fg != last.fg
                || snap.focus != last.focus
                || snap.pid != last.pid
                || snap.menu_mode != last.menu_mode;
            if changed {
                print(&snap, state.json, state.start, state.self_elevated);
            }
            if let Some(bars) = state.bars {
                place_bars(&bars, snap.focus_rect);
            }
            state.last = snap;
        });
    }

    unsafe extern "system" fn on_win_event(
        _: HWINEVENTHOOK,
        _: u32,
        _: HWND,
        _: i32,
        _: i32,
        _: u32,
        _: u32,
    ) {
        refresh();
    }

    unsafe extern "system" fn on_timer(_: HWND, _: u32, _: usize, _: u32) {
        refresh();
    }

    pub fn run() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let has = |flag: &str| args.iter().any(|a| a == flag);
        let json = has("--json");
        unsafe {
            // 按显示器的实际像素取坐标，红框才能在缩放屏上对齐。
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
        let self_elevated = process_info(unsafe { GetCurrentProcessId() }).1;
        if has("--once") {
            print(&capture(), json, now_ms(), self_elevated);
            return;
        }
        let bars = if has("--no-overlay") {
            None
        } else {
            match create_bars() {
                Ok(bars) => Some(bars),
                Err(err) => {
                    eprintln!("画框窗口创建失败，只打印不画框：{err}");
                    None
                }
            }
        };
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            state.json = json;
            state.start = now_ms();
            state.self_elevated = self_elevated;
            state.bars = bars;
        });
        if !json {
            eprintln!("focus-probe：焦点变化时打印一行，红框标出焦点控件。按 Ctrl+C 退出。");
        }
        unsafe {
            // 事件只在焦点变化时触发；定时器补上插入符和窗口移动带来的变化。
            let _foreground = SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(on_win_event),
                0,
                0,
                WINEVENT_OUTOFCONTEXT,
            );
            let _focus = SetWinEventHook(
                EVENT_OBJECT_FOCUS,
                EVENT_OBJECT_FOCUS,
                None,
                Some(on_win_event),
                0,
                0,
                WINEVENT_OUTOFCONTEXT,
            );
            SetTimer(None, 1, 250, Some(on_timer));
            refresh();
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}
