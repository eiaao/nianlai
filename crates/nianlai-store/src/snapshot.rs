use crate::*;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

static UNIQUE: AtomicU64 = AtomicU64::new(0);
pub(crate) fn stamp() -> String {
    let millis = now_ms();
    let mut days = millis / 86_400_000;
    let mut year = 1970;
    let leap = |year: u64| {
        year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
    };
    while days >= if leap(year) { 366 } else { 365 } {
        days -= if leap(year) { 366 } else { 365 };
        year += 1;
    }
    let months = [
        31,
        if leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 0;
    while days >= months[month] {
        days -= months[month];
        month += 1;
    }
    let seconds = millis / 1000 % 86400;
    format!(
        "{year:04}{:02}{:02}-{:02}{:02}{:02}-{:03}-{}-{}",
        month + 1,
        days + 1,
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60,
        millis % 1000,
        std::process::id(),
        UNIQUE.fetch_add(1, Ordering::Relaxed)
    )
}
struct Temporary(PathBuf);
impl Temporary {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!("nianlai-store-check-{}", stamp()));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn relational_check(c: &Connection) -> Result<Counts> {
    if schema::version(c)? != VERSION {
        return Err(Error::Invalid("schema_version"));
    }
    schema::pragma_check(c, "integrity_check")?;
    schema::structure(c)?;
    let invalid:bool=c.query_row("SELECT
        EXISTS(SELECT 1 FROM contents c WHERE NOT EXISTS(SELECT 1 FROM formats f WHERE f.content_id=c.id)
            OR NOT EXISTS(SELECT 1 FROM copy_events e WHERE e.content_id=c.id)
            OR c.copy_count!=(SELECT count(*) FROM copy_events e WHERE e.content_id=c.id)
            OR c.bytes!=(SELECT sum(bytes) FROM formats f WHERE f.content_id=c.id))
        OR EXISTS(SELECT 1 FROM formats f LEFT JOIN contents c ON c.id=f.content_id WHERE c.id IS NULL OR (f.encoding=0 AND f.bytes!=length(f.data)))
        OR EXISTS(SELECT 1 FROM copy_events e LEFT JOIN contents c ON c.id=e.content_id WHERE c.id IS NULL)
        OR EXISTS(SELECT 1 FROM contents c LEFT JOIN search_index_docsize s ON s.id=c.id WHERE s.id IS NULL)
        OR EXISTS(SELECT 1 FROM search_index_docsize s LEFT JOIN contents c ON c.id=s.id WHERE c.id IS NULL)",[],|r|r.get(0))?;
    if invalid {
        return Err(Error::Invalid("relational_integrity"));
    }
    schema::counts(c)
}
fn checked_copy(source: &Path) -> Result<Counts> {
    let temporary = Temporary::new()?;
    let candidate = temporary.0.join("candidate.db");
    let c = schema::readonly(source)?;
    schema::pragma_check(&c, "integrity_check")?;
    // VACUUM INTO 从只读连接取得一个完整的 WAL 读快照，不修改源库。
    c.execute(
        "VACUUM INTO ?1",
        [candidate.to_str().ok_or(Error::Invalid("path_encoding"))?],
    )?;
    drop(c);
    let c = writable(&candidate)?;
    let counts = relational_check(&c)?;
    // FTS 的校验命令是 INSERT；只在临时副本执行，rank=1 同时比对外部正文。
    c.execute(
        "INSERT INTO search_index(search_index,rank) VALUES('integrity-check',1)",
        [],
    )?;
    Ok(counts)
}
pub fn check(directory: &Path) -> Result<Counts> {
    checked_copy(&directory.join("history.db"))
}
pub fn validate_snapshot(path: &Path) -> Result<Counts> {
    let mut wal = path.as_os_str().to_os_string();
    wal.push("-wal");
    if fs::metadata(PathBuf::from(wal)).is_ok_and(|m| m.len() > 0) {
        return Err(Error::Invalid("snapshot_has_wal"));
    }
    let c = schema::readonly(path)?;
    relational_check(&c)?;
    drop(c);
    checked_copy(path)
}

#[derive(Debug, Serialize)]
pub struct SnapshotReport {
    pub file: PathBuf,
    pub elapsed_ms: f64,
    pub bytes: u64,
    pub retained: usize,
    pub deleted: usize,
}
fn snapshots(directory: &Path) -> Result<Vec<PathBuf>> {
    let directory = directory.join("snapshots");
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.file_type()?.is_file() && name.starts_with("history-") && name.ends_with(".db") {
            files.push(entry.path());
        }
    }
    files.sort();
    Ok(files)
}
pub fn snapshot_due(directory: &Path) -> Result<bool> {
    let files = snapshots(directory)?;
    let latest = files
        .iter()
        .map(|p| fs::metadata(p).and_then(|m| m.modified()))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .max();
    Ok(latest.is_none_or(|at| {
        SystemTime::now().duration_since(at).unwrap_or_default() > Duration::from_secs(86400)
    }))
}
pub fn snapshot(directory: &Path) -> Result<SnapshotReport> {
    let started = Instant::now();
    fs::create_dir_all(directory.join("snapshots"))?;
    // 两个快照工具同时运行时不互相轮换正在生成的文件。
    let _lock = write::lock(&directory.join("snapshots"))?;
    let file = directory
        .join("snapshots")
        .join(format!("history-{}.db", stamp()));
    let stage = file.with_extension("pending");
    let result = (|| -> Result<()> {
        let c = schema::readonly(&directory.join("history.db"))?;
        c.execute(
            "VACUUM INTO ?1",
            [stage.to_str().ok_or(Error::Invalid("path_encoding"))?],
        )?;
        fs::OpenOptions::new()
            .write(true)
            .open(&stage)?
            .sync_all()?;
        fs::rename(&stage, &file)?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&stage);
        return Err(error);
    }
    let files = snapshots(directory)?;
    let deleted = files.len().saturating_sub(7);
    for old in files.iter().take(deleted) {
        fs::remove_file(old)?;
    }
    Ok(SnapshotReport {
        bytes: fs::metadata(&file)?.len(),
        file,
        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        retained: files.len() - deleted,
        deleted,
    })
}

pub fn restore(directory: &Path, source: &Path) -> Result<PathBuf> {
    // 先校验外部候选；失败时不创建或打开现有库。
    validate_snapshot(source)?;
    fs::create_dir_all(directory)?;
    let _application_lock = write::lock_named(directory, "application.lock", false)?;
    let _lock = write::lock(directory)?;
    let suffix = stamp();
    let db = directory.join("history.db");
    let stage = directory.join(format!("history.db.restore-stage-{suffix}"));
    let backup = directory.join(format!("history.db.before-restore-{suffix}"));
    let result = (|| -> Result<()> {
        fs::copy(source, &stage)?;
        fs::OpenOptions::new()
            .write(true)
            .open(&stage)?
            .sync_all()?;
        // 校验实际放到位的字节，避免源文件在第一次校验以后被换掉。
        validate_snapshot(&stage)?;
        let mut moved = Vec::new();
        let swap = (|| -> Result<()> {
            for sidecar in ["", "-wal", "-shm"] {
                let from = directory.join(format!("history.db{sidecar}"));
                let to = directory.join(format!("history.db.before-restore-{suffix}{sidecar}"));
                if from.exists() {
                    fs::rename(&from, &to)?;
                    moved.push((from, to));
                }
            }
            fs::rename(&stage, &db)?;
            Ok(())
        })();
        if let Err(error) = swap {
            for (from, to) in moved.into_iter().rev() {
                if fs::rename(to, from).is_err() {
                    return Err(Error::Invalid("restore_rollback_failed_backups_retained"));
                }
            }
            return Err(error);
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&stage);
    }
    result?;
    Ok(backup)
}
