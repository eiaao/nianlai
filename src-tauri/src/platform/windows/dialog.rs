use windows::{
    Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MessageBoxW,
    },
    core::HSTRING,
};

// 系统消息框，阻塞到用户关掉。守护者没有自己的窗口，只有这一种提示方式。
pub fn error(text: &str) {
    unsafe {
        let _ = MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from("拈来"),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
        );
    }
}
