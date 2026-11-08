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
    // v2 起：收藏必须指向存在的内容；contents / copy_events 的来源应用引用必须指向存在的行。
    let invalid:bool=c.query_row("SELECT
        EXISTS(SELECT 1 FROM contents c WHERE NOT EXISTS(SELECT 1 FROM formats f WHERE f.content_id=c.id)
            OR NOT EXISTS(SELECT 1 FROM copy_events e WHERE e.content_id=c.id)
            OR c.copy_count!=(SELECT count(*) FROM copy_events e WHERE e.content_id=c.id)
            OR c.bytes!=(SELECT sum(bytes) FROM formats f WHERE f.content_id=c.id))
        OR EXISTS(SELECT 1 FROM formats f LEFT JOIN contents c ON c.id=f.content_id WHERE c.id IS NULL OR (f.encoding=0 AND f.bytes!=length(f.data)))
        OR EXISTS(SELECT 1 FROM copy_events e LEFT JOIN contents c ON c.id=e.content_id WHERE c.id IS NULL)
        OR EXISTS(SELECT 1 FROM contents c LEFT JOIN search_index_docsize s ON s.id=c.id WHERE s.id IS NULL)
        OR EXISTS(SELECT 1 FROM search_index_docsize s LEFT JOIN contents c ON c.id=s.id WHERE c.id IS NULL)
        OR EXISTS(SELECT 1 FROM favorites fv LEFT JOIN contents c ON c.id=fv.content_id WHERE c.id IS NULL)
        OR EXISTS(SELECT 1 FROM copy_events e LEFT JOIN source_apps a ON a.id=e.source_app_id WHERE e.source_app_id IS NOT NULL AND a.id IS NULL)
        OR EXISTS(SELECT 1 FROM contents c LEFT JOIN source_apps a ON a.id=c.source_app_id WHERE c.source_app_id IS NOT NULL AND a.id IS NULL)",[],|r|r.get(0))?;
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

/// 默认保留的快照份数（B18）。
pub const SNAPSHOT_KEEP: usize = 7;
const VERSION_FILE: &str = "version";

/// 快照为什么生成；写在文件名末尾，列表里据此显示来历。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Daily,
    Upgrade,
    Manual,
    BeforeRestore,
}
impl Reason {
    fn tag(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Upgrade => "upgrade",
            Self::Manual => "manual",
            Self::BeforeRestore => "before_restore",
        }
    }
    fn parse(name: &str) -> Option<Self> {
        let tag = name.strip_suffix(".db")?.rsplit_once('-')?.1;
        [
            Self::Daily,
            Self::Upgrade,
            Self::Manual,
            Self::BeforeRestore,
        ]
        .into_iter()
        .find(|reason| reason.tag() == tag)
    }
}

/// 轮换规则：份数和总大小任一超出就从最旧的删起，但至少留下最新的一份。
#[derive(Clone, Copy, Debug)]
pub struct Retention {
    pub keep: usize,
    pub max_bytes: u64,
}
impl Default for Retention {
    fn default() -> Self {
        Self {
            keep: SNAPSHOT_KEEP,
            max_bytes: u64::MAX,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SnapshotInfo {
    pub name: String,
    pub bytes: u64,
    pub created_ms: u64,
    pub reason: Option<Reason>,
}

fn snapshot_directory(directory: &Path) -> PathBuf {
    directory.join("snapshots")
}
fn snapshots(directory: &Path) -> Result<Vec<PathBuf>> {
    let directory = snapshot_directory(directory);
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
    // 文件名以时间戳开头，按名字排序就是按时间从旧到新。
    files.sort();
    Ok(files)
}

/// 列出快照，最新的在前。
pub fn list_snapshots(directory: &Path) -> Result<Vec<SnapshotInfo>> {
    let mut list = Vec::new();
    for file in snapshots(directory)?.into_iter().rev() {
        let metadata = fs::metadata(&file)?;
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        list.push(SnapshotInfo {
            reason: Reason::parse(&name),
            bytes: metadata.len(),
            created_ms: metadata
                .modified()?
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64),
            name,
        });
    }
    Ok(list)
}

/// 按名字找快照文件；只接受快照目录里真实存在的文件名，不接受路径。
pub fn snapshot_file(directory: &Path, name: &str) -> Option<PathBuf> {
    snapshots(directory)
        .ok()?
        .into_iter()
        .find(|file| file.file_name().is_some_and(|n| n == name))
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

/// 现在该不该拍快照：版本号和上次拍快照时不同 → 升级快照；距上一份超过一天 → 每日快照。
pub fn snapshot_reason(directory: &Path, version: &str) -> Result<Option<Reason>> {
    let marked = fs::read_to_string(snapshot_directory(directory).join(VERSION_FILE)).ok();
    if marked.as_deref().map(str::trim) != Some(version) && !snapshots(directory)?.is_empty() {
        return Ok(Some(Reason::Upgrade));
    }
    Ok(snapshot_due(directory)?.then_some(Reason::Daily))
}

pub fn snapshot(directory: &Path) -> Result<SnapshotReport> {
    snapshot_with(directory, Reason::Manual, Some(Retention::default()), None)
}

/// 拍一份快照。`retention` 为 None 时不轮换（恢复前的那一份，避免把要恢复的目标轮换掉）；
/// `version` 给出时记下当前版本，用于判断下次是否升级。
pub fn snapshot_with(
    directory: &Path,
    reason: Reason,
    retention: Option<Retention>,
    version: Option<&str>,
) -> Result<SnapshotReport> {
    let started = Instant::now();
    let folder = snapshot_directory(directory);
    fs::create_dir_all(&folder)?;
    // 两个快照工具同时运行时不互相轮换正在生成的文件。
    let _lock = write::lock(&folder)?;
    // 上次拍到一半被打断留下的暂存文件。
    for entry in fs::read_dir(&folder)? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "pending") {
            let _ = fs::remove_file(path);
        }
    }
    let file = folder.join(format!("history-{}-{}.db", stamp(), reason.tag()));
    let c = schema::readonly(&directory.join("history.db"))?;
    copy_database(&c, &file)?;
    drop(c);
    if let Some(version) = version {
        fs::write(folder.join(VERSION_FILE), version)?;
    }
    let files = snapshots(directory)?;
    let mut sizes = files
        .iter()
        .map(|p| fs::metadata(p).map(|m| m.len()))
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut deleted = 0;
    if let Some(retention) = retention {
        let mut total: u64 = sizes.iter().sum();
        for (index, old) in files.iter().enumerate() {
            let remaining = files.len() - deleted;
            if remaining <= 1 || (remaining <= retention.keep && total <= retention.max_bytes) {
                break;
            }
            if *old == file {
                continue;
            }
            fs::remove_file(old)?;
            total -= sizes[index];
            sizes[index] = 0;
            deleted += 1;
        }
    }
    Ok(SnapshotReport {
        bytes: fs::metadata(&file)?.len(),
        file,
        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        retained: files.len() - deleted,
        deleted,
    })
}

// 把连接上的库完整复制到 destination：VACUUM INTO 给出一个一致的读快照（WAL 里已提交的内容也在），
// 先写到 .pending 再 fsync、改名，所以目标位置要么没有文件、要么是完整文件。迁移前备份也走这里。
pub(crate) fn copy_database(c: &Connection, destination: &Path) -> Result<()> {
    let stage = destination.with_extension("pending");
    let result = (|| -> Result<()> {
        c.execute(
            "VACUUM INTO ?1",
            [stage.to_str().ok_or(Error::Invalid("path_encoding"))?],
        )?;
        fs::OpenOptions::new()
            .write(true)
            .open(&stage)?
            .sync_all()?;
        fs::rename(&stage, destination)?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&stage);
        return Err(error);
    }
    Ok(())
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
