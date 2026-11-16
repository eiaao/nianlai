// M3 接入 macOS 粘贴后移除共享粘贴模块的临时 dead_code 许可。
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub mod as_text;
#[cfg(any(windows, test))]
pub mod capture_access;
#[cfg(any(windows, test))]
pub mod evidence;
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub mod generic;
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub mod keys;
#[cfg(any(windows, test))]
pub mod native;
// generic 的错误映射仍共享这些常量；其余是 Windows 剪贴板探针。
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub mod native_access;
#[cfg(any(windows, test))]
pub mod native_edit;
#[cfg(test)]
pub mod native_publication_trace;
pub mod outcome;
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub mod permit;
pub(crate) mod queue;
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub mod request;
