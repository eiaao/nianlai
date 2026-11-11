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
/// 登记文件内容以此开头表示导入备份覆盖，后面是临时目录令牌；快照文件名都以 history- 开头，不会混淆。
const IMPORT_PREFIX: &str = "import:";

/// 登记文件里的内容：恢复哪份快照，或应用哪个导入令牌。
#[derive(Debug, PartialEq, Eq)]
enum Pending {
    Snapshot(String),
    Import(String),
}

impl Pending {
    fn parse(marker: &str) -> Self {
        let marker = marker.trim();
        match marker.strip_prefix(IMPORT_PREFIX) {
            Some(token) => Self::Import(token.to_owned()),
            None => Self::Snapshot(marker.to_owned()),
        }
    }

    /// 写进登记文件的文本，也是 `Restored.name`。
    fn marker(&self) -> String {
        match self {
            Self::Snapshot(name) => name.clone(),
            Self::Import(token) => format!("{IMPORT_PREFIX}{token}"),
        }
    }
}
const MIB: u64 = 1024 * 1024;
/// 运行期间多久检查一次是否该拍每日快照。
pub const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);

static MAX_MB: AtomicU64 = AtomicU64::new(crate::settings::SNAPSHOT_MAX_MB);
static LAST_RESTORE: Mutex<Option<Restored>> = Mutex::new(None);

/// 本次启动时执行的恢复结果，设置页据此提示。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Restored {
    /// 快照文件名；导入备份覆盖时是被导入的备份令牌（`import:<令牌>`）。
    pub name: String,
    /// "snapshot" 或 "import"（导入备份并覆盖，M2-4）。
    pub kind: &'static str,
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
    fs::write(
        directory.join("snapshots").join(PENDING_FILE),
        Pending::Snapshot(name.to_owned()).marker(),
    )
    .map_err(|_| "恢复没有登记成功，请重试".to_owned())?;
    logger.event("snapshot_restore_scheduled", json!({"name":name}));
    Ok(())
}

/// 登记“导入备份并覆盖”（M2-4）：临时目录里的库先完整校验，通过才登记；校验失败不改动任何东西。
/// 重启后在 `apply_pending` 里和快照恢复走同一条路：先另拍“恢复前”快照，再换库、换图标。
pub fn schedule_import(directory: &Path, token: &str, logger: &Logger) -> Result<(), String> {
    if let Err(error) = nianlai_store::prepare_overwrite(directory, token) {
        logger.event(
            "backup_import_rejected",
            json!({"error_code":error.to_string()}),
        );
        return Err("这份备份校验没有通过，不能用来覆盖".to_owned());
    }
    let folder = directory.join("snapshots");
    fs::create_dir_all(&folder).map_err(|_| "覆盖没有登记成功，请重试".to_owned())?;
    fs::write(
        folder.join(PENDING_FILE),
        Pending::Import(token.to_owned()).marker(),
    )
    .map_err(|_| "覆盖没有登记成功，请重试".to_owned())?;
    logger.event("backup_import_scheduled", json!({}));
    Ok(())
}

/// 这个令牌的临时目录是否正被登记的覆盖占用（此时不能清理）。
pub fn import_pending(directory: &Path, token: &str) -> bool {
    fs::read_to_string(directory.join("snapshots").join(PENDING_FILE))
        .is_ok_and(|marker| Pending::parse(&marker) == Pending::Import(token.to_owned()))
}

/// 启动时、打开库之前调用：有登记的恢复就执行。登记文件无论成败都先删掉，避免反复重试。
pub fn apply_pending(directory: &Path, logger: &Logger) {
    let marker = directory.join("snapshots").join(PENDING_FILE);
    let Ok(text) = fs::read_to_string(&marker) else {
        return;
    };
    let _ = fs::remove_file(&marker);
    let pending = Pending::parse(&text);
    let result = (|| -> Result<(), String> {
        match &pending {
            Pending::Snapshot(name) => {
                // 先确认要恢复的东西在，再动当前库。
                let file =
                    nianlai_store::snapshot_file(directory, name).ok_or("snapshot_missing")?;
                before_restore(directory)?;
                nianlai_store::restore(directory, &file).map(drop)
            }
            Pending::Import(token) => {
                before_restore(directory)?;
                nianlai_store::apply_overwrite(directory, token)
            }
        }
        .map_err(|error| error.to_string())
    })();
    let (kind, applied, failed) = match pending {
        Pending::Snapshot(_) => ("snapshot", "snapshot_restored", "snapshot_restore_failed"),
        Pending::Import(_) => ("import", "backup_import_applied", "backup_import_failed"),
    };
    logger.event(
        if result.is_ok() { applied } else { failed },
        json!({"name":match &pending {
            Pending::Snapshot(name) => name.as_str(),
            Pending::Import(_) => "",
        },"error_code":result.as_ref().err()}),
    );
    // 覆盖用过的临时目录（无论成败）这里就清掉。
    if let Pending::Import(token) = &pending {
        nianlai_store::discard_import(directory, token);
    }
    *LAST_RESTORE.lock().unwrap() = Some(Restored {
        name: pending.marker(),
        kind,
        error: result.err(),
    });
}

/// 当前库先另拍一份，不参与轮换，避免把要恢复的那份删掉。
fn before_restore(directory: &Path) -> Result<(), String> {
    if directory.join("history.db").exists() {
        nianlai_store::snapshot_with(directory, Reason::BeforeRestore, None, None)
            .map_err(|error| format!("before_restore:{error}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Directories, tests::Directory};

    // 恢复结果记在进程级静态里，两条恢复测试不能同时跑。
    static SERIAL: Mutex<()> = Mutex::new(());

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
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
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
                kind: "snapshot",
                error: None
            })
        );
        let reasons: Vec<_> = list(&data).unwrap().iter().map(|s| s.reason).collect();
        assert_eq!(reasons, [Some(Reason::BeforeRestore), Some(Reason::Manual)]);
        nianlai_store::check(&data).unwrap();
    }

    #[test]
    fn import_overwrite_is_validated_then_applied_at_startup_and_cleans_up() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let directory = Directory::new();
        let logger = logger(&directory);
        let backups = Directory::new();
        // 源库：一条内容 + 一个图标，导出成备份。
        let source = backups.0.join("source");
        fs::create_dir_all(source.join("icons")).unwrap();
        let hash = nianlai_store::hash::sha256;
        let store = nianlai_store::Store::open(&source, hash).unwrap();
        store
            .seed(
                nianlai_store::content::Content::text("来自备份", hash).unwrap(),
                nianlai_store::CopyEvent {
                    time_ms: 5,
                    sequence: 1,
                    source_process: "a.exe".into(),
                    source_path: String::new(),
                    owner_process: "a.exe".into(),
                    owner_pid: 1,
                    observed: std::time::Instant::now(),
                },
            )
            .unwrap();
        let file = backups.0.join("b.nianlai-backup");
        nianlai_store::export_backup(&source, &file, "t").unwrap();
        drop(store);

        // 目标数据目录：另有一条本地内容和一个旧图标。
        let data = directory.0.join("data");
        {
            let local = nianlai_store::Store::open(&data, hash).unwrap();
            local
                .seed(
                    nianlai_store::content::Content::text("本地", hash).unwrap(),
                    nianlai_store::CopyEvent {
                        time_ms: 9,
                        sequence: 1,
                        source_process: "b.exe".into(),
                        source_path: String::new(),
                        owner_process: "b.exe".into(),
                        owner_pid: 1,
                        observed: std::time::Instant::now(),
                    },
                )
                .unwrap();
        }
        fs::create_dir_all(data.join("icons")).unwrap();
        fs::write(data.join("icons").join("old.png"), b"old").unwrap();
        let inspection = nianlai_store::inspect_backup(&data, &file);
        assert!(inspection.ok);
        // 假令牌登记不上；真令牌登记后占用临时目录。
        assert!(schedule_import(&data, "nope", &logger).is_err());
        assert!(!import_pending(&data, &inspection.token));
        schedule_import(&data, &inspection.token, &logger).unwrap();
        assert!(import_pending(&data, &inspection.token));
        apply_pending(&data, &logger);
        assert_eq!(
            last_restore(),
            Some(Restored {
                name: format!("import:{}", inspection.token),
                kind: "import",
                error: None
            })
        );
        assert!(!data.join("snapshots").join(PENDING_FILE).exists());
        assert!(!data.join("imports").join(&inspection.token).exists());
        assert!(!data.join("icons").join("old.png").exists());
        let reasons: Vec<_> = list(&data).unwrap().iter().map(|s| s.reason).collect();
        assert_eq!(reasons, [Some(Reason::BeforeRestore)]);
        let store = nianlai_store::Store::open(&data, hash).unwrap();
        let items = store.cache().items;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].preview, "来自备份");
        drop(store);

        // 登记后临时目录被删：应用时失败，库不动，登记已清掉。
        let inspection = nianlai_store::inspect_backup(&data, &file);
        schedule_import(&data, &inspection.token, &logger).unwrap();
        nianlai_store::discard_import(&data, &inspection.token);
        apply_pending(&data, &logger);
        assert_eq!(last_restore().unwrap().kind, "import");
        assert!(last_restore().unwrap().error.is_some());
        nianlai_store::check(&data).unwrap();
    }
}
