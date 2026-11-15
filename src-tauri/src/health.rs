//! 托盘健康状态：类型化的存储/记录健康 + 纯函数托盘选择器。
//!
//! 与现有 `HistorySnapshot.recording`、`store_status` 并列：那两处照旧驱动面板横幅，
//! 这里的状态只供托盘、设置状态行和首次引导使用（v11 实现方案第 2 节）。

use crate::history::History;
use serde::Serialize;

/// 存储库的健康状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreHealth {
    /// 正在打开数据、还没有结论。
    Loading,
    /// 磁盘库已打开。
    Ready,
    /// 打不开、完整性检查失败或已退回内存模式。
    Failed,
}

/// 记录（剪贴板监视）的健康状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureHealth {
    /// 记录线程尚未就绪。
    Starting,
    /// 正在监听剪贴板。
    Recording,
    /// 重挂失败、消息循环意外结束或读取线程失效。
    Failed,
    /// 正常退出过程中。
    Stopping,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct HealthSnapshot {
    pub store: StoreHealth,
    pub capture: CaptureHealth,
    pub recorded_today: usize,
}

/// PNG 尺寸（托盘资源测试用）。
#[cfg(test)]
pub(crate) fn png_dimensions(data: &[u8]) -> (u32, u32) {
    (
        u32::from_be_bytes(data[16..20].try_into().unwrap()),
        u32::from_be_bytes(data[20..24].try_into().unwrap()),
    )
}

/// 托盘三态；点击故障态时打开的设置页（存储故障 → 数据与备份，记录故障 → 记录与隐私）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayState {
    Recording,
    Paused,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrayDecision {
    pub state: TrayState,
    /// 故障态对应的设置页；非故障态为 None（左键仍走"打开拈来"）。
    pub settings_page: Option<&'static str>,
}

/// 出问题 > 已暂停 > 记录中；暂停不遮蔽故障。启动中（Starting）和退出中（Stopping）不算故障。
pub fn select_tray(health: &HealthSnapshot, paused: bool) -> TrayDecision {
    if health.capture == CaptureHealth::Stopping {
        return TrayDecision {
            state: if paused {
                TrayState::Paused
            } else {
                TrayState::Recording
            },
            settings_page: None,
        };
    }
    let store_failed = health.store == StoreHealth::Failed;
    if health.capture == CaptureHealth::Failed || store_failed {
        return TrayDecision {
            state: TrayState::Error,
            settings_page: Some(if store_failed { "data" } else { "privacy" }),
        };
    }
    if paused {
        return TrayDecision {
            state: TrayState::Paused,
            settings_page: None,
        };
    }
    TrayDecision {
        state: TrayState::Recording,
        settings_page: None,
    }
}

impl History {
    /// 托盘、设置状态行用的健康快照。`recorded_today` 是本机自然日内成功写入的复制次数。
    pub fn health(&self) -> HealthSnapshot {
        self.health_at(crate::platform::time::local_day_start_ms())
    }
    /// 测试可注入固定日起点，避免构造事件与读取健康之间跨过午夜。
    pub(crate) fn health_at(&self, day_start: u64) -> HealthSnapshot {
        // 只读计数失败（例如写入线程正忙）不代表存储坏了：不改 StoreHealth，沿用当天上次读到的数。
        // 存储真故障由写入路径判定，那里才会把状态置为 Failed。
        let recorded_today = match self.db().copied_since(day_start) {
            Ok(count) => {
                *self.recorded_today_cache.lock().unwrap() = Some((day_start, count));
                count
            }
            Err(error) => {
                self.log_health_error(&error);
                match *self.recorded_today_cache.lock().unwrap() {
                    Some((start, count)) if start == day_start => count,
                    _ => 0,
                }
            }
        };
        HealthSnapshot {
            store: self.store_health(),
            capture: self.capture_health(),
            recorded_today,
        }
    }

    fn log_health_error(&self, error: &nianlai_store::Error) {
        self.logger.event(
            "recorded_today_failed",
            serde_json::json!({ "error_code": error.to_string() }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::content::Content;
    use crate::log::Logger;
    use crate::settings::{Directories, tests::Directory};
    use nianlai_store::CopyEvent;
    use std::time::Instant;

    fn history(directory: &Directory) -> History {
        let directories = Directories {
            settings_file: directory.0.join("settings.json"),
            data: directory.0.join("data"),
            logs: directory.0.join("logs"),
            data_source: "设置文件指定",
            test_data_dir: false,
        };
        let logger = Logger::with_settings(&directories, 20).unwrap();
        History::new(nianlai_store::hash::sha256, logger)
    }

    fn save(history: &History, text: &str, sequence: u32, time_ms: u64) {
        let now = Instant::now();
        history
            .ingest(
                Content::text(text, nianlai_store::hash::sha256).unwrap(),
                CopyEvent {
                    time_ms,
                    sequence,
                    source_process: "health-test.exe".into(),
                    source_path: String::new(),
                    owner_process: "health-test.exe".into(),
                    owner_pid: 1,
                    observed: now,
                },
                false,
                now,
            )
            .unwrap();
    }

    fn snapshot(store: StoreHealth, capture: CaptureHealth) -> HealthSnapshot {
        HealthSnapshot {
            store,
            capture,
            recorded_today: 0,
        }
    }

    #[test]
    fn task12_fix_guide_health_uses_typed_snake_case_snapshot() {
        let value =
            serde_json::to_value(snapshot(StoreHealth::Failed, CaptureHealth::Starting)).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"store":"failed","capture":"starting","recorded_today":0})
        );
    }

    #[test]
    fn task12_fix_guide_health_reads_real_failure_when_legacy_status_is_null() {
        let directory = Directory::new();
        let history = history(&directory);
        history.initialize();
        assert!(history.note_store_error(&nianlai_store::Error::Sqlite(13)));
        assert_eq!(
            serde_json::to_value(history.snapshot()).unwrap()["store_status"],
            serde_json::Value::Null
        );
        assert_eq!(history.health().store, StoreHealth::Failed);
        history.set_recording(false);
        assert_eq!(history.health().capture, CaptureHealth::Failed);
    }

    // ---------- 选择器：第 2 节表格逐行 ----------

    #[test]
    fn startup_and_shutdown_never_report_failure() {
        assert_eq!(
            select_tray(
                &snapshot(StoreHealth::Loading, CaptureHealth::Starting),
                false
            ),
            TrayDecision {
                state: TrayState::Recording,
                settings_page: None
            }
        );
        assert_eq!(
            select_tray(
                &snapshot(StoreHealth::Ready, CaptureHealth::Starting),
                false
            ),
            TrayDecision {
                state: TrayState::Recording,
                settings_page: None
            }
        );
        assert_eq!(
            select_tray(&snapshot(StoreHealth::Ready, CaptureHealth::Stopping), true),
            TrayDecision {
                state: TrayState::Paused,
                settings_page: None
            }
        );
        assert_eq!(
            select_tray(
                &snapshot(StoreHealth::Loading, CaptureHealth::Stopping),
                false
            ),
            TrayDecision {
                state: TrayState::Recording,
                settings_page: None
            }
        );
    }

    #[test]
    fn task2_fix_stopping_suppresses_store_failure() {
        for (paused, state) in [(false, TrayState::Recording), (true, TrayState::Paused)] {
            assert_eq!(
                select_tray(
                    &snapshot(StoreHealth::Failed, CaptureHealth::Stopping),
                    paused
                ),
                TrayDecision {
                    state,
                    settings_page: None
                }
            );
        }
    }

    #[test]
    fn normal_running_and_user_pause() {
        assert_eq!(
            select_tray(
                &snapshot(StoreHealth::Ready, CaptureHealth::Recording),
                false
            ),
            TrayDecision {
                state: TrayState::Recording,
                settings_page: None
            }
        );
        // 用户暂停不是故障：暂停本身退一步。
        assert_eq!(
            select_tray(
                &snapshot(StoreHealth::Ready, CaptureHealth::Recording),
                true
            ),
            TrayDecision {
                state: TrayState::Paused,
                settings_page: None
            }
        );
    }

    #[test]
    fn store_and_capture_failures_report_error_with_target_pages() {
        assert_eq!(
            select_tray(
                &snapshot(StoreHealth::Failed, CaptureHealth::Recording),
                false
            ),
            TrayDecision {
                state: TrayState::Error,
                settings_page: Some("data")
            }
        );
        assert_eq!(
            select_tray(&snapshot(StoreHealth::Ready, CaptureHealth::Failed), false),
            TrayDecision {
                state: TrayState::Error,
                settings_page: Some("privacy")
            }
        );
        // 存储故障优先决定点击页；故障优先于暂停。
        assert_eq!(
            select_tray(&snapshot(StoreHealth::Failed, CaptureHealth::Failed), true),
            TrayDecision {
                state: TrayState::Error,
                settings_page: Some("data")
            }
        );
        assert_eq!(
            select_tray(&snapshot(StoreHealth::Ready, CaptureHealth::Failed), true),
            TrayDecision {
                state: TrayState::Error,
                settings_page: Some("privacy")
            }
        );
    }

    // ---------- 健康状态迁移 ----------

    #[test]
    fn set_recording_marks_recording_and_failure_transitions() {
        let directory = Directory::new();
        let history = history(&directory);
        assert_eq!(history.capture_health(), CaptureHealth::Starting);
        history.set_recording(true);
        assert_eq!(history.capture_health(), CaptureHealth::Recording);
        // 重挂失败 / 消息循环意外结束：Failed。
        history.set_recording(false);
        assert_eq!(history.capture_health(), CaptureHealth::Failed);
        // 重挂成功自动撤下故障。
        history.set_recording(true);
        assert_eq!(history.capture_health(), CaptureHealth::Recording);
    }

    #[test]
    fn stopping_is_never_overwritten_into_failure() {
        let directory = Directory::new();
        let history = history(&directory);
        history.set_recording(true);
        history.begin_capture_shutdown();
        assert_eq!(history.capture_health(), CaptureHealth::Stopping);
        // 线程末尾的 set_recording(false) 不得把 Stopping 变成故障。
        history.set_recording(false);
        assert_eq!(history.capture_health(), CaptureHealth::Stopping);
    }

    #[test]
    fn task2_health_capture_start_rearm_and_reader_failures_are_distinct_from_recovery() {
        let directory = Directory::new();
        let history = history(&directory);
        history.set_recording(false); // Service::start 返回错误。
        assert_eq!(
            select_tray(&history.health(), false).state,
            TrayState::Error
        );
        history.set_recording(true);
        assert_eq!(history.capture_health(), CaptureHealth::Recording);
        history.set_recording(false); // 异常重挂失败。
        assert_eq!(history.capture_health(), CaptureHealth::Failed);
        history.set_recording(true); // 重挂成功，原补查行为由 listener 保留。
        assert_eq!(history.capture_health(), CaptureHealth::Recording);
        history.set_recording(false); // 读取线程失效。
        assert_eq!(history.capture_health(), CaptureHealth::Failed);
        let snapshot = serde_json::to_value(history.snapshot()).unwrap();
        assert_eq!(snapshot["recording"], false);
    }

    #[test]
    fn task2_health_concurrent_shutdown_cannot_be_overwritten_by_thread_exit() {
        let directory = Directory::new();
        let history = history(&directory);
        for _ in 0..64 {
            history.set_capture_health(CaptureHealth::Starting);
            history.set_recording(true);
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            std::thread::scope(|scope| {
                let worker_history = history.clone();
                let worker_barrier = barrier.clone();
                scope.spawn(move || {
                    worker_barrier.wait();
                    worker_history.set_recording(false);
                });
                barrier.wait();
                history.begin_capture_shutdown();
            });
            assert_eq!(history.capture_health(), CaptureHealth::Stopping);
        }
    }

    // ---------- “今天记下 n 条” ----------

    #[test]
    fn recorded_today_counts_today_copies_including_repeats_and_survives_deletes() {
        let directory = Directory::new();
        let history = history(&directory);
        history.initialize();
        let day_start = crate::platform::time::local_day_start_ms();
        let yesterday = day_start.saturating_sub(24 * 60 * 60 * 1000);
        save(&history, "昨天的复制", 1, yesterday);
        save(&history, "今天第一次", 2, day_start);
        save(&history, "今天第一次", 3, day_start + 1000); // 同一内容再复制仍计一次。
        save(&history, "今天第二次", 4, day_start + 2000);
        let ids: Vec<_> = history
            .snapshot()
            .items
            .iter()
            .map(|item| item.id)
            .collect::<Vec<_>>();
        for id in ids {
            if history.get(id).unwrap().is_some() {
                let _ = history.delete(id);
            }
        }
        let health = history.health_at(day_start);
        assert_eq!(health.recorded_today, 3, "删除不倒扣当天已发生的复制");
        assert_eq!(health.store, StoreHealth::Ready);
    }

    #[test]
    fn recorded_today_excludes_skipped_content() {
        let directory = Directory::new();
        let history = history(&directory);
        history.initialize();
        let day_start = crate::platform::time::local_day_start_ms();
        // ingest 之外没有“被挡内容”入口（挡在采集层），这里证明计数只看真实写入的 copy_events。
        save(&history, "真实写入", 1, day_start);
        let health = history.health_at(day_start);
        assert_eq!(health.recorded_today, 1);
    }
}
