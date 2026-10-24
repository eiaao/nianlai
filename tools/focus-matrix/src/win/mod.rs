mod helper;
mod input;
mod observe;
mod process;
mod runner;
mod safety;
mod target;
mod uia;

pub use observe::environment;
pub use runner::run;
pub fn helper(args: &[String]) -> Result<(), String> {
    helper::run(args)
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
pub fn hwnd(raw: isize) -> windows::Win32::Foundation::HWND {
    windows::Win32::Foundation::HWND(raw as _)
}
