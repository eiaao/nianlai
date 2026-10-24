use super::{hwnd, wide};
use crate::protocol::Digest;
use std::cell::RefCell;
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{HiDpi::*, Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
    },
    core::{PCWSTR, w},
};

#[derive(Default)]
struct State {
    token: String,
    edit: isize,
    enter: u64,
    esc: u64,
    digit: u64,
    chars: u64,
    deact: u64,
}
thread_local! {static STATE:RefCell<State>=RefCell::new(State::default());}
pub const CLEAR: u32 = WM_APP + 33;
unsafe extern "system" fn edit_proc(
    h: HWND,
    msg: u32,
    w: WPARAM,
    l: LPARAM,
    _: usize,
    _: usize,
) -> LRESULT {
    unsafe {
        if msg == WM_KEYDOWN {
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                match w.0 as u16 {
                    0x0d => s.enter += 1,
                    0x1b => s.esc += 1,
                    _ => {}
                }
            });
        }
        if msg == WM_CHAR && GetKeyState(0x11) >= 0 && GetKeyState(0x12) >= 0 {
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                if (0x30..=0x39).contains(&w.0) {
                    s.digit += 1;
                } else if w.0 >= 0x20 {
                    s.chars += 1;
                }
            });
        }
        DefSubclassProc(h, msg, w, l)
    }
}
fn update(root: HWND) {
    unsafe {
        let (edit, token, counts) = STATE.with(|s| {
            let s = s.borrow();
            (
                s.edit,
                s.token.clone(),
                [s.enter, s.esc, s.digit, s.chars, s.deact],
            )
        });
        let n = GetWindowTextLengthW(hwnd(edit)).clamp(0, 200000) as usize;
        let mut text = vec![0u16; n + 1];
        let len = GetWindowTextW(hwnd(edit), &mut text);
        let d = Digest::of(&String::from_utf16_lossy(&text[..len.max(0) as usize]));
        let title = wide(&format!(
            "NLFM|t={token}|len={}|h={:08x}|enter={}|esc={}|digit={}|chars={}|deact={}|focus={}|END",
            d.len,
            d.hash,
            counts[0],
            counts[1],
            counts[2],
            counts[3],
            counts[4],
            u8::from(GetFocus().0 as isize == edit)
        ));
        let _ = SetWindowTextW(root, PCWSTR(title.as_ptr()));
    }
}
unsafe extern "system" fn proc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_SETFOCUS => {
                let edit = STATE.with(|s| s.borrow().edit);
                if edit != 0 {
                    let _ = SetFocus(Some(hwnd(edit)));
                }
                LRESULT(0)
            }
            WM_ACTIVATE => {
                if w.0 & 0xffff == WA_INACTIVE as usize {
                    STATE.with(|s| s.borrow_mut().deact += 1);
                }
                DefWindowProcW(h, msg, w, l)
            }
            WM_SIZE => {
                let edit = STATE.with(|s| s.borrow().edit);
                if edit != 0 {
                    let _ = MoveWindow(
                        hwnd(edit),
                        20,
                        20,
                        (l.0 & 0xffff) as i32 - 40,
                        ((l.0 >> 16) & 0xffff) as i32 - 40,
                        true,
                    );
                }
                LRESULT(0)
            }
            WM_TIMER => {
                update(h);
                LRESULT(0)
            }
            CLEAR => {
                let edit = STATE.with(|s| {
                    let mut s = s.borrow_mut();
                    s.enter = 0;
                    s.esc = 0;
                    s.digit = 0;
                    s.chars = 0;
                    s.deact = 0;
                    s.edit
                });
                let _ = SetWindowTextW(hwnd(edit), w!(""));
                update(h);
                LRESULT(1)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(h, msg, w, l),
        }
    }
}
pub fn run(args: &[String]) -> Result<(), String> {
    if args.len() != 2
        || args[0] != "--token"
        || std::env::var("NLFM_HELPER_TOKEN").ok().as_deref() != Some(args[1].as_str())
        || args[1].is_empty()
        || !args[1]
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-')
    {
        return Err("helper-window 只接受工具内部生成的标记和子进程环境".into());
    }
    STATE.with(|s| s.borrow_mut().token = args[1].clone());
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let module = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class = w!("NianlaiFocusMatrix");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: module.into(),
            lpszClassName: class,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err("测试窗口类注册失败".into());
        }
        let root = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("NLFM"),
            WS_OVERLAPPEDWINDOW,
            160,
            160,
            820,
            520,
            None,
            None,
            Some(module.into()),
            None,
        )
        .map_err(|e| e.to_string())?;
        let edit = CreateWindowExW(
            WS_EX_CLIENTEDGE,
            w!("EDIT"),
            w!(""),
            WS_CHILD
                | WS_VISIBLE
                | WS_TABSTOP
                | WS_VSCROLL
                | WINDOW_STYLE((ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN) as u32),
            20,
            20,
            760,
            420,
            Some(root),
            None,
            Some(module.into()),
            None,
        )
        .map_err(|e| e.to_string())?;
        if !SetWindowSubclass(edit, Some(edit_proc), 1, 0).as_bool() {
            return Err("EDIT 子类化失败".into());
        }
        STATE.with(|s| s.borrow_mut().edit = edit.0 as isize);
        SetTimer(Some(root), 1, 40, None);
        let _ = ShowWindow(root, SW_SHOW);
        let _ = SetFocus(Some(edit));
        update(root);
        let mut msg = MSG::default();
        loop {
            let result = GetMessageW(&mut msg, None, 0, 0).0;
            if result == 0 {
                break;
            }
            if result < 0 {
                return Err("测试窗口消息循环失败".into());
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}
