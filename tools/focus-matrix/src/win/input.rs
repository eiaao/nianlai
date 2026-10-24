use super::{hwnd, observe, safety::Control};
use crate::cli::Hotkey;
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::POINT,
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

pub const MARKER: usize = 0x4e4c464d;
pub struct Input {
    pub control: Arc<Control>,
    held: Vec<u16>,
    failures: u32,
    owned: BTreeMap<isize, u32>,
}
impl Input {
    pub fn new(control: Arc<Control>) -> Self {
        Self {
            control,
            held: vec![],
            failures: 0,
            owned: BTreeMap::new(),
        }
    }
    pub fn own(&mut self, raw: isize, pid: u32) {
        self.owned.insert(raw, pid);
    }
    pub fn check(&mut self, allowed: &[isize]) -> Result<(), String> {
        self.control.check()?;
        let mut fg = observe::foreground();
        // 窗口切换的瞬间前台可能短暂为空，等一会儿再判断；持续为空多半是锁屏。
        let deadline = Instant::now() + Duration::from_secs(1);
        while fg == 0 && Instant::now() < deadline {
            self.control.wait(20)?;
            fg = observe::foreground();
        }
        if fg == 0 {
            self.control.stop("前台持续为空，停止注入");
            return self.control.check();
        }
        if !allowed.contains(&fg)
            || !observe::visible(fg)
            || self.owned.get(&fg) != Some(&observe::pid(fg))
        {
            self.failures += 1;
            if self.failures >= 3 {
                self.control.stop("连续三次前台窗口不符合预期");
            }
            return Err(format!(
                "harness_error：前台不符合预期（类={}，进程={}）",
                observe::class(fg),
                observe::process_name(observe::pid(fg))
            ));
        }
        self.failures = 0;
        Ok(())
    }
    fn emit(&mut self, event: INPUT, allowed: &[isize]) -> Result<(), String> {
        self.check(allowed)?;
        if unsafe { SendInput(&[event], size_of::<INPUT>() as i32) } != 1 {
            self.control.stop("SendInput 返回 0，可能锁屏或 UIPI 拦截");
            return self.control.check();
        }
        Ok(())
    }
    pub fn chord(&mut self, chord: &Hotkey, allowed: &[isize]) -> Result<(), String> {
        self.check(allowed)?;
        self.held = chord.modifiers.clone();
        let mut events = chord
            .modifiers
            .iter()
            .map(|&vk| key(vk, 0, KEYBD_EVENT_FLAGS(0)))
            .collect::<Vec<_>>();
        events.push(key(chord.key, 0, KEYBD_EVENT_FLAGS(0)));
        events.push(key(chord.key, 0, KEYEVENTF_KEYUP));
        events.extend(
            chord
                .modifiers
                .iter()
                .rev()
                .map(|&vk| key(vk, 0, KEYEVENTF_KEYUP)),
        );
        // 整个组合键一次入队，避免 Enter 收起面板后，抬起事件被前台门禁截断。
        self.control.check()?;
        let sent = unsafe { SendInput(&events, size_of::<INPUT>() as i32) };
        if sent as usize != events.len() {
            self.control
                .stop("SendInput 未完整注入组合键，可能锁屏或 UIPI 拦截");
            self.release_modifiers();
            return self.control.check();
        }
        self.held.clear();
        self.control.check()
    }
    pub fn named(&mut self, chord: &str, allowed: &[isize]) -> Result<(), String> {
        self.chord(&Hotkey::parse(chord)?, allowed)
    }
    pub fn unicode(&mut self, text: &str, allowed: &[isize]) -> Result<(), String> {
        for unit in text.encode_utf16() {
            self.check(allowed)?;
            let events = [
                key(0, unit, KEYEVENTF_UNICODE),
                key(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
            ];
            if unsafe { SendInput(&events, size_of::<INPUT>() as i32) } != 2 {
                self.control.stop("SendInput 未完整注入 Unicode 字符");
                return self.control.check();
            }
        }
        Ok(())
    }
    pub fn first_letter(&mut self, ch: char, allowed: &[isize]) -> Result<(), String> {
        self.check(allowed)?;
        // VkKeyScanEx 使用目标线程的布局；工具线程的默认布局不代表用户当前输入语言。
        let thread = unsafe { GetWindowThreadProcessId(hwnd(observe::foreground()), None) };
        let layout = unsafe { GetKeyboardLayout(thread) };
        let code = unsafe { VkKeyScanExW(ch as u16, layout) };
        if code < 0 {
            return Err("搜索首字无法按当前键盘布局映射".into());
        }
        let mods = (code as u16) >> 8;
        if mods & !1 != 0 {
            return Err("搜索首字映射需要 Ctrl/Alt，不能直接触发搜索".into());
        }
        let modifiers = if mods & 1 != 0 { vec![0x10] } else { vec![] };
        self.chord(
            &Hotkey {
                modifiers,
                key: code as u16 & 0xff,
            },
            allowed,
        )
    }
    pub fn release_modifiers(&mut self) {
        for vk in self.held.drain(..).rev() {
            unsafe {
                let _ = SendInput(&[key(vk, 0, KEYEVENTF_KEYUP)], size_of::<INPUT>() as i32);
            }
        }
    }
    pub fn mouse(&mut self, point: POINT, click: bool, allowed: &[isize]) -> Result<(), String> {
        self.check(allowed)?;
        if click {
            let root = unsafe { GetAncestor(WindowFromPoint(point), GA_ROOT).0 as isize };
            if !allowed.contains(&root) || self.owned.get(&root) != Some(&observe::pid(root)) {
                return Err("控件坐标落在未认领窗口上，拒绝点击".into());
            }
        }
        let (x, y, w, h) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        if w <= 1 || h <= 1 {
            return Err("虚拟屏幕尺寸无效".into());
        }
        let event = |flags| INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: ((point.x - x) as i64 * 65535 / (w - 1) as i64) as i32,
                    dy: ((point.y - y) as i64 * 65535 / (h - 1) as i64) as i32,
                    dwFlags: flags,
                    dwExtraInfo: MARKER,
                    ..Default::default()
                },
            },
        };
        self.emit(
            event(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK),
            allowed,
        )?;
        if click {
            self.check(allowed)?;
            let root = unsafe { GetAncestor(WindowFromPoint(point), GA_ROOT).0 as isize };
            if !allowed.contains(&root) {
                return Err("点击前窗口遮挡发生变化".into());
            }
            let events = [event(MOUSEEVENTF_LEFTDOWN), event(MOUSEEVENTF_LEFTUP)];
            if unsafe { SendInput(&events, size_of::<INPUT>() as i32) } != 2 {
                self.control.stop("鼠标点击未完整注入");
                return self.control.check();
            }
        }
        Ok(())
    }
}
fn key(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    let extended = if matches!(vk, 0x21..=0x2e) {
        KEYEVENTF_EXTENDEDKEY
    } else {
        KEYBD_EVENT_FLAGS(0)
    };
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: flags | extended,
                dwExtraInfo: MARKER,
                ..Default::default()
            },
        },
    }
}
impl Drop for Input {
    fn drop(&mut self) {
        self.release_modifiers();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn distinct_injection_marker() {
        assert_ne!(MARKER, 0x4e4c4730);
        let i = key(0x31, 0, KEYEVENTF_KEYUP);
        unsafe {
            assert_eq!(i.Anonymous.ki.dwExtraInfo, MARKER);
            assert!(i.Anonymous.ki.dwFlags.contains(KEYEVENTF_KEYUP));
        }
    }
}
