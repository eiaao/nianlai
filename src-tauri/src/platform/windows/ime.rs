// 跨进程只读查询输入法的中英文状态：给焦点窗口的默认输入法窗口发 WM_IME_CONTROL。
// ImmGetContext 和 TSF 的 compartment 都只能读本线程；面板的焦点窗口在 msedgewebview2.exe 里，对我们也算跨进程。
// 只在面板状态机线程或短命探针线程里调用，绝不能放进钩子回调（钩子要在 LowLevelHooksTimeout 内返回）。
// 只读：这里没有也不该有任何 IMC_SET*。
use super::window::{self, hwnd};
use serde_json::{Value, json};
use std::mem::size_of;
use windows::Win32::{
    Foundation::{GetLastError, HWND, LPARAM, WPARAM},
    UI::{
        Input::{Ime::ImmGetDefaultIMEWnd, KeyboardAndMouse::GetKeyboardLayout},
        WindowsAndMessaging::{
            GUITHREADINFO, GetGUIThreadInfo, GetWindowThreadProcessId, IsWindow, SMTO_ABORTIFHUNG,
            SendMessageTimeoutW, WM_IME_CONTROL,
        },
    },
};

// WM_IME_CONTROL 的子命令；windows crate 没有导出这两个常量。
const IMC_GETCONVERSIONMODE: usize = 1;
const IMC_GETOPENSTATUS: usize = 5;
// 转换模式最低位是 IME_CMODE_NATIVE：1 为中文。小狼毫的开关状态恒为开，只能看这一位。
const CMODE_NATIVE: usize = 1;
// 每条消息最多等这么久；目标线程挂死时 SMTO_ABORTIFHUNG 会立即返回。
const TIMEOUT_MS: u32 = 50;

#[derive(Default)]
pub struct State {
    pub open: Option<usize>,
    pub conversion: Option<usize>,
    pub layout: usize,
    pub ime_window: bool,
    pub process_name: String,
    pub window_class: String,
    pub error: Option<String>,
}

impl State {
    // open、conversion 记原值：各输入法的解读不同，不在这里下结论。native 只是转换模式最低位的便捷读法。
    pub fn to_json(&self) -> Value {
        json!({
            "open": self.open,
            "conversion": self.conversion,
            "native": self.conversion.map(native),
            "layout": layout_hex(self.layout),
            "ime_window": self.ime_window,
            "process_name": self.process_name,
            "window_class": self.window_class,
            "error": self.error,
        })
    }
}

fn native(conversion: usize) -> bool {
    conversion & CMODE_NATIVE != 0
}

// 低 16 位是语言标识，中文输入法为 0x0804；固定 8 位十六进制便于肉眼对照。
fn layout_hex(layout: usize) -> String {
    format!("{layout:#010x}")
}

// 失败时返回 GetLastError 的码，1460 是超时。
fn control(ime: HWND, command: usize) -> Result<usize, u32> {
    let mut result = 0usize;
    let sent = unsafe {
        SendMessageTimeoutW(
            ime,
            WM_IME_CONTROL,
            WPARAM(command),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            TIMEOUT_MS,
            Some(&mut result),
        )
    };
    if sent.0 == 0 {
        Err(unsafe { GetLastError() }.0)
    } else {
        Ok(result)
    }
}

// 查 window 所在线程的输入法状态；window 通常是焦点控件。
pub fn probe(window: usize) -> State {
    let mut state = State::default();
    let handle = hwnd(window);
    if window == 0 || !unsafe { IsWindow(Some(handle)) }.as_bool() {
        state.error = Some("window_invalid".into());
        return state;
    }
    let thread = unsafe { GetWindowThreadProcessId(handle, None) };
    state.layout = unsafe { GetKeyboardLayout(thread) }.0 as usize;
    state.process_name = window::process_of(window);
    state.window_class = window::class_name(window);
    let ime = unsafe { ImmGetDefaultIMEWnd(handle) };
    if ime.is_invalid() {
        state.error = Some("ime_window_missing".into());
        return state;
    }
    state.ime_window = true;
    let mut errors = Vec::new();
    match control(ime, IMC_GETOPENSTATUS) {
        Ok(open) => state.open = Some(open),
        Err(code) => errors.push(format!("open:{code}")),
    }
    // 第一问就失败说明对方没在泵消息，第二问不再等，总耗时不超过一次超时。
    if errors.is_empty() {
        match control(ime, IMC_GETCONVERSIONMODE) {
            Ok(conversion) => state.conversion = Some(conversion),
            Err(code) => errors.push(format!("conversion:{code}")),
        }
    } else {
        errors.push("conversion:skipped".into());
    }
    state.error = (!errors.is_empty()).then(|| errors.join(";"));
    state
}

pub fn query(window: usize) -> Value {
    probe(window).to_json()
}

// 当时前台线程的焦点窗口。GetFocus 跨线程拿不到；GetGUIThreadInfo 传 0 取的就是前台线程。
pub fn focused_window() -> usize {
    let mut info = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetGUIThreadInfo(0, &mut info) }.is_err() {
        return window::foreground();
    }
    [info.hwndFocus.0 as usize, info.hwndActive.0 as usize]
        .into_iter()
        .find(|&handle| handle != 0)
        .unwrap_or_else(window::foreground)
}

pub fn query_focused() -> Value {
    query(focused_window())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_is_the_lowest_conversion_bit() {
        assert!(native(1));
        // 中文加全角、符号等其他位。
        assert!(native(0x0401));
        assert!(!native(0));
        assert!(!native(8));
    }

    #[test]
    fn layout_prints_eight_hex_digits() {
        assert_eq!(layout_hex(0x0804_0804), "0x08040804");
        assert_eq!(layout_hex(0x0409_0409), "0x04090409");
        assert_eq!(layout_hex(0), "0x00000000");
    }

    #[test]
    fn json_keeps_raw_values_and_derives_native() {
        let state = State {
            open: Some(1),
            conversion: Some(1025),
            layout: 0x0804_0804,
            ime_window: true,
            process_name: "msedgewebview2.exe".into(),
            window_class: "Chrome_RenderWidgetHostHWND".into(),
            error: None,
        };
        assert_eq!(
            state.to_json(),
            json!({
                "open": 1, "conversion": 1025, "native": true, "layout": "0x08040804",
                "ime_window": true, "process_name": "msedgewebview2.exe",
                "window_class": "Chrome_RenderWidgetHostHWND", "error": null,
            })
        );
    }

    #[test]
    fn json_marks_failures_without_guessing() {
        let state = State {
            ime_window: true,
            error: Some("open:1460;conversion:skipped".into()),
            ..State::default()
        };
        let value = state.to_json();
        assert_eq!(value["open"], Value::Null);
        assert_eq!(value["conversion"], Value::Null);
        assert_eq!(value["native"], Value::Null);
        assert_eq!(value["ime_window"], true);
        assert_eq!(value["error"], "open:1460;conversion:skipped");
    }

    #[test]
    fn zero_handle_is_rejected_before_any_call() {
        let state = probe(0);
        assert_eq!(state.error.as_deref(), Some("window_invalid"));
        assert!(!state.ime_window);
        assert_eq!(state.to_json()["layout"], "0x00000000");
    }
}
