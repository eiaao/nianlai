use crate::{
    log::Logger,
    panel::{Action, dispatch},
    platform::{Result, webview},
};
use serde_json::json;
use std::{
    sync::{Arc, mpsc, mpsc::RecvTimeoutError},
    thread,
    time::Duration,
};
use tauri::AppHandle;

const CHECK: Duration = Duration::from_secs(10);
const SETTLE: Duration = Duration::from_secs(60);

// settings::valid_minutes 已保证有限、非负，且非 0 时换算出的时长有效；0 表示关闭。
fn interval(minutes: f64) -> Option<Duration> {
    if minutes == 0.0 {
        return None;
    }
    Duration::try_from_secs_f64(minutes * 60.0).ok()
}

pub fn from_settings(settings: &crate::settings::Memory, logger: &Logger) -> Option<Duration> {
    let interval = interval(settings.resuspend_minutes)?;
    logger.event(
        "resuspend_enabled",
        json!({"interval_minutes": interval.as_secs_f64() / 60.0}),
    );
    Some(interval)
}

pub fn spawn(
    app: AppHandle,
    logger: Logger,
    interval: Duration,
    state: Arc<webview::MemoryState>,
) -> Result<mpsc::Sender<()>> {
    let (stop, cancel) = mpsc::channel();
    thread::Builder::new()
        .name("webview-resuspend".into())
        .spawn(move || {
            loop {
                // 发送停止信号或发送端被丢弃都退出线程。
                if !matches!(cancel.recv_timeout(CHECK), Err(RecvTimeoutError::Timeout)) {
                    return;
                }
                // 不到期就不打扰 UI 线程。
                if !webview::due(&state, interval) {
                    continue;
                }
                let (reply, done) = mpsc::channel();
                if dispatch(&app, Action::Resuspend { interval, reply }).is_err() {
                    return;
                }
                // 回执被丢弃说明状态机判定跳过（面板正显示或正在粘贴）。
                let Ok((session, generation)) = done.recv() else {
                    continue;
                };
                // 挂起后约 23 秒内存才压缩完，等 60 秒采样能看到压缩后的数字。
                if !matches!(cancel.recv_timeout(SETTLE), Err(RecvTimeoutError::Timeout)) {
                    return;
                }
                logger.event("resuspend_settled", json!({
                    "session":session, "generation":generation, "settle_seconds":SETTLE.as_secs(),
                    "undisturbed": webview::generation(&state) == generation,
                    "memory": crate::platform::memory::sample(),
                }));
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(stop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_whole_and_fractional_minutes_and_zero_means_off() {
        assert_eq!(interval(15.0), Some(Duration::from_secs(15 * 60)));
        assert_eq!(interval(0.5), Some(Duration::from_secs(30)));
        assert_eq!(interval(0.0), None);
        assert_eq!(interval(-0.0), None);
    }
}
