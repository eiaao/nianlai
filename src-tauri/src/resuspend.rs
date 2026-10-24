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

fn parse(value: &str) -> Result<Duration> {
    let minutes = value
        .trim()
        .parse::<f64>()
        .map_err(|_| format!("分钟数“{value}”不是有效数字"))?;
    if !minutes.is_finite() || minutes <= 0.0 {
        return Err(format!("分钟数“{value}”必须大于 0 且为有限数"));
    }
    Duration::try_from_secs_f64(minutes * 60.0)
        .map_err(|_| format!("分钟数“{value}”换算出的时长超出计时范围"))
}

pub fn from_env(logger: &Logger) -> Option<Duration> {
    let value = match std::env::var("NIANLAI_G0_RESUSPEND_MIN") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => return None,
        Err(std::env::VarError::NotUnicode(_)) => {
            logger.event(
                "resuspend_invalid",
                json!({"reason": "环境变量不是有效 Unicode"}),
            );
            return None;
        }
    };
    match parse(&value) {
        Ok(_) if logger.tier == 1 => {
            logger.event("resuspend_ignored", json!({"reason":"memory_tier_1"}));
            None
        }
        Ok(interval) => {
            logger.event(
                "resuspend_enabled",
                json!({"interval_minutes": interval.as_secs_f64() / 60.0}),
            );
            Some(interval)
        }
        Err(reason) => {
            logger.event("resuspend_invalid", json!({"reason":reason}));
            None
        }
    }
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
    fn parses_whole_trimmed_and_fractional_minutes() {
        assert_eq!(parse("15").unwrap(), Duration::from_secs(15 * 60));
        assert_eq!(parse(" 30 ").unwrap(), Duration::from_secs(30 * 60));
        assert_eq!(parse("0.5").unwrap(), Duration::from_secs(30));
    }

    #[test]
    fn rejects_invalid_minutes() {
        for input in ["", "0", "-1", "NaN", "inf", "abc", "1e300"] {
            assert!(parse(input).is_err(), "{input}");
        }
    }
}
