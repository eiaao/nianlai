pub mod cli;
pub mod judge;
pub mod logtail;
pub mod preflight;
pub mod protocol;
pub mod report;
#[path = "../../../src-tauri/src/samples.rs"]
pub mod samples;
pub mod scenario;
#[cfg(windows)]
pub mod win;

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
