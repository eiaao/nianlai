// 本地日期（备份默认文件名用）。
use windows::Win32::System::SystemInformation::GetLocalTime;

/// 本地时区的（年，月，日）。
pub fn local_date() -> (u16, u8, u8) {
    let now = unsafe { GetLocalTime() };
    (now.wYear, now.wMonth as u8, now.wDay as u8)
}
