use crate::{
    log::Logger,
    panel::{Action, dispatch},
    platform::Result,
};
use serde_json::json;
use std::{
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread,
    time::{Duration, Instant},
};
use tauri::AppHandle;

const MAX_STEPS: usize = 1000;
const CANCEL_POLL: Duration = Duration::from_millis(100);

pub struct Step {
    hidden: Duration,
    visible: Duration,
}

pub struct Plan {
    steps: Vec<Step>,
    total: Duration,
}

impl Plan {
    fn parse(value: &str) -> Result<Self> {
        let mut steps = Vec::new();
        let mut total = Duration::ZERO;
        for (index, step) in value.split(',').enumerate() {
            if index == MAX_STEPS {
                return Err("最多允许 1000 步".into());
            }
            let (hidden, visible) = step
                .trim()
                .split_once(':')
                .ok_or_else(|| format!("第 {} 步必须是 隐藏秒数:显示秒数", index + 1))?;
            let duration = |text: &str, visible: bool| -> Result<Duration> {
                let seconds = text
                    .trim()
                    .parse::<f64>()
                    .map_err(|_| format!("第 {} 步的秒数不是有效数字", index + 1))?;
                if !seconds.is_finite() || seconds < 0.0 || (visible && seconds == 0.0) {
                    return Err(format!(
                        "第 {} 步要求隐藏秒数 ≥ 0，显示秒数 > 0，且必须为有限数",
                        index + 1
                    ));
                }
                let value = Duration::try_from_secs_f64(seconds)
                    .map_err(|_| format!("第 {} 步超出计时范围", index + 1))?;
                Ok(if visible {
                    value.max(Duration::from_nanos(1))
                } else {
                    value
                })
            };
            let hidden = duration(hidden, false)?;
            let visible = duration(visible, true)?;
            total = total
                .checked_add(hidden)
                .and_then(|sum| sum.checked_add(visible))
                .ok_or("总时长超出计时范围")?;
            steps.push(Step { hidden, visible });
        }
        if Instant::now().checked_add(total).is_none() {
            return Err("总时长超出计时范围".into());
        }
        Ok(Self { steps, total })
    }

    pub fn from_env(logger: &Logger) -> Option<Self> {
        let result = match std::env::var("NIANLAI_G0_AUTOCYCLE") {
            Ok(value) => Self::parse(&value),
            Err(std::env::VarError::NotPresent) => return None,
            Err(std::env::VarError::NotUnicode(_)) => Err("环境变量不是有效 Unicode".into()),
        };
        match result {
            Ok(plan) => {
                logger.event(
                    "autocycle_enabled",
                    json!({
                        "steps":plan.steps.len(), "total_seconds":plan.total.as_secs_f64(),
                    }),
                );
                Some(plan)
            }
            Err(reason) => {
                logger.event("autocycle_invalid", json!({"reason":reason}));
                None
            }
        }
    }
}

fn receive<T>(receiver: &Receiver<Result<T>>, cancel: &Receiver<()>, logger: &Logger) -> Option<T> {
    loop {
        if !matches!(cancel.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            return None;
        }
        match receiver.recv_timeout(CANCEL_POLL) {
            Ok(Ok(value)) => return Some(value),
            Ok(Err(error)) => {
                logger.event("autocycle_aborted", json!({"reason":error}));
                return None;
            }
            Err(RecvTimeoutError::Disconnected) => {
                logger.event(
                    "autocycle_aborted",
                    json!({"reason":"回执未送达，请求被忽略或执行失败"}),
                );
                return None;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

fn wait(since: Instant, duration: Duration, cancel: &Receiver<()>) -> bool {
    matches!(
        cancel.recv_timeout(duration.saturating_sub(since.elapsed())),
        Err(RecvTimeoutError::Timeout)
    )
}

pub fn spawn(
    app: AppHandle,
    logger: Logger,
    plan: Plan,
    initial_hidden: Receiver<Result<Instant>>,
) -> Result<Sender<()>> {
    let (stop, cancel) = mpsc::channel();
    thread::Builder::new()
        .name("panel-autocycle".into())
        .spawn(move || {
            // 回执来自隐藏/挂起完成处，避免把异步挂起的耗时算进计划等待时间。
            let Some(mut hidden_since) = receive(&initial_hidden, &cancel, &logger) else {
                return;
            };
            let count = plan.steps.len();
            for (index, step) in plan.steps.into_iter().enumerate() {
                if !wait(hidden_since, step.hidden, &cancel) {
                    return;
                }
                let cycle = index + 1;
                let (reply, shown) = mpsc::channel();
                if dispatch(
                    &app,
                    Action::AutoShow {
                        cycle,
                        hidden_since,
                        triggered: Instant::now(),
                        reply,
                    },
                )
                .is_err()
                {
                    return;
                }
                let Some((session, shown_at)) = receive(&shown, &cancel, &logger) else {
                    return;
                };
                if !wait(shown_at, step.visible, &cancel) {
                    return;
                }
                let (reply, hidden) = mpsc::channel();
                if dispatch(
                    &app,
                    Action::AutoHide {
                        cycle,
                        session,
                        reply,
                    },
                )
                .is_err()
                {
                    return;
                }
                let Some(at) = receive(&hidden, &cancel, &logger) else {
                    return;
                };
                hidden_since = at;
            }
            let _ = dispatch(&app, Action::AutoDone { cycles: count });
        })
        .map_err(|e| e.to_string())?;
    Ok(stop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_zero_hidden_and_fractional_durations() {
        let plan = Plan::parse(" 0:0.25, 60.5:3 ").unwrap();
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].hidden, Duration::ZERO);
        assert_eq!(plan.steps[0].visible, Duration::from_millis(250));
        assert_eq!(plan.total, Duration::from_millis(63750));
    }

    #[test]
    fn rejects_invalid_steps_and_unrepresentable_durations() {
        for input in [
            "", " ", "1", ":1", "1:", "1:2:3", "1:2,", "1:2,,3:4", "-1:2", "1:0", "1:-0.1",
            "NaN:1", "1:inf", "1e100:1", "hello:1",
        ] {
            assert!(Plan::parse(input).is_err(), "{input}");
        }
    }

    #[test]
    fn enforces_the_step_limit() {
        assert!(Plan::parse(&vec!["0:1"; 1000].join(",")).is_ok());
        assert!(Plan::parse(&vec!["0:1"; 1001].join(",")).is_err());
    }

    #[test]
    fn keeps_a_positive_visible_duration() {
        assert_eq!(
            Plan::parse("0:0.0000000001").unwrap().steps[0].visible,
            Duration::from_nanos(1)
        );
    }

    #[test]
    fn cancellation_interrupts_a_long_wait() {
        let (stop, cancel) = mpsc::channel();
        stop.send(()).unwrap();
        assert!(!wait(Instant::now(), Duration::from_secs(3600), &cancel));
    }
}
