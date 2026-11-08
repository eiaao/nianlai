//! B18 自动快照与恢复：每天一份、版本变化时强制一份，保留 7 份，总大小受设置上限约束。
//! 恢复要换掉正在使用的库，所以只登记“下次启动时恢复哪一份”，再让应用重启；
//! 启动时在打开库之前完成替换，替换前先把当前库另拍一份快照，可以从同一个列表恢复回来。

use crate::log::Logger;
use nianlai_store::{Reason, Retention, SNAPSHOT_KEEP, SnapshotInfo};
use serde::Serialize;
use serde_json::json;
use std::{
    fs,
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

const PENDING_FILE: &str = "restore-pending";
const MIB: u64 = 1024 * 1024;
/// 运行期间多久检查一次是否该拍每日快照。
pub const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);

static MAX_MB: AtomicU64 = AtomicU64::new(crate::settings::SNAPSHOT_MAX_MB);
static LAST_RESTORE: Mutex<Option<Restored>> = Mutex::new(None);

/// 本次启动时执行的恢复结果，设置页据此提示。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Restored {
    pub name: String,
    pub error: Option<String>,
}

pub fn set_limit(max_mb: u64) {
    MAX_MB.store(max_mb, Ordering::Release);
}

fn retention() -> Retention {
    Retention {
        keep: SNAPSHOT_KEEP,
        max_bytes: MAX_MB.load(Ordering::Acquire).saturating_mul(MIB),
    }
}

fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// 到时间或版本变了就拍一份；由后台线程定时调用。
pub fn automatic(directory: &Path, logger: &Logger) {
    let result = nianlai_store::snapshot_reason(directory, version()).and_then(|reason| {
        reason
            .map(|reason| {
                nianlai_store::snapshot_with(directory, reason, Some(retention()), Some(version()))
                    .map(|report| (reason, report))
            })
            .transpose()
    });
    match result {
        Ok(Some((reason, report))) => logger.event(
            "snapshot",
            json!({"reason":reason,"elapsed_ms":report.elapsed_ms,"bytes":report.bytes,"retained":report.retained,"deleted":report.deleted}),
        ),
        Ok(None) => {}
        Err(error) => logger.event("snapshot_failed", json!({"error_code":error.to_string()})),
    }
}

pub fn manual(directory: &Path, logger: &Logger) -> Result<(), String> {
    let report = nianlai_store::snapshot_with(
        directory,
        Reason::Manual,
        Some(retention()),
        Some(version()),
    )
    .map_err(|error| {
        logger.event("snapshot_failed", json!({"error_code":error.to_string()}));
        match error {
            nianlai_store::Error::Busy => "正在生成另一份快照，请稍后再试".to_owned(),
            _ => "快照没有生成成功，现有快照不受影响".to_owned(),
        }
    })?;
    logger.event(
        "snapshot",
        json!({"reason":Reason::Manual,"elapsed_ms":report.elapsed_ms,"bytes":report.bytes,"retained":report.retained,"deleted":report.deleted}),
    );
    Ok(())
}

pub fn list(directory: &Path) -> Result<Vec<SnapshotInfo>, String> {
    nianlai_store::list_snapshots(directory).map_err(|_| "快照列表读取失败".to_owned())
}

pub fn last_restore() -> Option<Restored> {
    LAST_RESTORE.lock().unwrap().clone()
}

/// 先完整校验快照，通过后才登记；校验失败不改动任何东西。
pub fn schedule_restore(directory: &Path, name: &str, logger: &Logger) -> Result<(), String> {
    let file = nianlai_store::snapshot_file(directory, name)
        .ok_or_else(|| "找不到这份快照，可能已被轮换删除".to_owned())?;
    if let Err(error) = nianlai_store::validate_snapshot(&file) {
        logger.event(
            "snapshot_restore_rejected",
            json!({"name":name,"error_code":error.to_string()}),
        );
        return Err("这份快照校验没有通过，不能用来恢复".to_owned());
    }
    fs::write(directory.join("snapshots").join(PENDING_FILE), name)
        .map_err(|_| "恢复没有登记成功，请重试".to_owned())?;
    logger.event("snapshot_restore_scheduled", json!({"name":name}));
    Ok(())
}

/// 启动时、打开库之前调用：有登记的恢复就执行。登记文件无论成败都先删掉，避免反复重试。
pub fn apply_pending(directory: &Path, logger: &Logger) {
    let marker = directory.join("snapshots").join(PENDING_FILE);
    let Ok(name) = fs::read_to_string(&marker) else {
        return;
    };
    let _ = fs::remove_file(&marker);
    let name = name.trim().to_owned();
    let result = (|| -> Result<(), String> {
        let file = nianlai_store::snapshot_file(directory, &name).ok_or("snapshot_missing")?;
        // 当前库先另拍一份，不参与轮换，避免把要恢复的那份删掉。
        if directory.join("history.db").exists() {
            nianlai_store::snapshot_with(directory, Reason::BeforeRestore, None, None)
                .map_err(|error| format!("before_restore:{error}"))?;
        }
        nianlai_store::restore(directory, &file).map_err(|error| error.to_string())?;
        Ok(())
    })();
    logger.event(
        if result.is_ok() {
            "snapshot_restored"
        } else {
            "snapshot_restore_failed"
        },
        json!({"name":name,"error_code":result.as_ref().err()}),
    );
    *LAST_RESTORE.lock().unwrap() = Some(Restored {
        name,
        error: result.err(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Directories, tests::Directory};

    fn logger(directory: &Directory) -> Logger {
        Logger::with_settings(
            &Directories {
                settings_file: directory.0.join("settings.json"),
                data: directory.0.join("data"),
                logs: directory.0.join("logs"),
                data_source: "设置文件指定",
                test_data_dir: false,
            },
            20,
        )
        .unwrap()
    }

    #[test]
    fn restore_is_validated_then_applied_at_startup_with_a_before_restore_snapshot() {
        let directory = Directory::new();
        let logger = logger(&directory);
        let data = directory.0.join("data");
        // 无登记时什么都不做。
        apply_pending(&data, &logger);
        fs::create_dir_all(data.join("snapshots")).unwrap();
        // 登记了不存在的快照：清掉登记，库不动。
        fs::write(data.join("snapshots").join(PENDING_FILE), "history-x.db").unwrap();
        apply_pending(&data, &logger);
        assert!(!data.join("snapshots").join(PENDING_FILE).exists());
        assert_eq!(
            last_restore().unwrap().error.as_deref(),
            Some("snapshot_missing")
        );
        assert!(schedule_restore(&data, "../history.db", &logger).is_err());

        let store = nianlai_store::Store::open(&data, nianlai_store::hash::sha256).unwrap();
        drop(store);
        manual(&data, &logger).unwrap();
        let target = list(&data).unwrap()[0].name.clone();
        // 损坏的快照登记不上。
        let broken = data
            .join("snapshots")
            .join("history-20200101-000000-000-1-0-manual.db");
        fs::write(&broken, b"broken").unwrap();
        assert!(
            schedule_restore(&data, "history-20200101-000000-000-1-0-manual.db", &logger).is_err()
        );
        assert!(!data.join("snapshots").join(PENDING_FILE).exists());
        fs::remove_file(broken).unwrap();

        schedule_restore(&data, &target, &logger).unwrap();
        apply_pending(&data, &logger);
        assert_eq!(
            last_restore(),
            Some(Restored {
                name: target.clone(),
                error: None
            })
        );
        let reasons: Vec<_> = list(&data).unwrap().iter().map(|s| s.reason).collect();
        assert_eq!(reasons, [Some(Reason::BeforeRestore), Some(Reason::Manual)]);
        nianlai_store::check(&data).unwrap();
    }
}
