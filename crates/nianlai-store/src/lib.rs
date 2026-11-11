mod backup;
pub mod content;
pub mod hash;
mod migrations;
mod schema;
mod search;
mod snapshot;
mod write;

pub use backup::{
    EXTENSION as BACKUP_EXTENSION, ExportReport, Inspection, MergePreview, MergeReport,
    OverwritePreview, Step as InspectionStep, Summary as BackupSummary, apply_overwrite,
    clean_stale as clean_stale_imports, discard as discard_import, export as export_backup,
    inspect as inspect_backup, prepare_overwrite, summary as backup_summary,
};
pub use migrations::VERSION;
pub use search::{Search, SearchMode};
pub use snapshot::{
    Reason, Retention, SNAPSHOT_KEEP, SnapshotInfo, SnapshotReport, check, list_snapshots, restore,
    snapshot, snapshot_due, snapshot_file, snapshot_reason, snapshot_with, validate_snapshot,
};
pub use write::{MemoryOptions, Store, clean_note, clean_title};

use content::{Fingerprint, Hash};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const CACHE_LIMIT: usize = 300;
pub type Result<T> = std::result::Result<T, Error>;

pub fn application_guard(directory: &Path) -> Result<std::fs::File> {
    std::fs::create_dir_all(directory)?;
    // 内存降级时也保留应用的共享锁；恢复工具需要排他锁。
    write::lock_named(directory, "application.lock", true)
}

// 错误只暴露分类和错误码；SQLite 的原始错误可能含 SQL、内容或完整路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Sqlite(i32),
    Io(std::io::ErrorKind),
    // 库由更新版本的拈来创建：found 是库里的 user_version，supported 是本程序能打开的最高版本。
    NewerSchema { found: u32, supported: u32 },
    Invalid(&'static str),
    Busy,
    Closed,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error.sqlite_error().map_or(-1, |e| e.extended_code))
    }
}
impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.kind())
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn older(sequence: u32, latest: u32) -> bool {
    (sequence.wrapping_sub(latest) as i32) < 0
}

#[derive(Clone)]
pub struct CopyEvent {
    pub time_ms: u64,
    pub sequence: u32,
    pub source_process: String,
    // 来源进程的可执行文件路径；权限不够或进程已退出时为空串，此时来源应用只记进程名。
    pub source_path: String,
    pub owner_process: String,
    pub owner_pid: u32,
    pub observed: Instant,
}

#[derive(Clone, Debug, Serialize)]
pub struct FormatMeta {
    pub name: String,
    pub fingerprint: Fingerprint,
    pub bytes: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct Metadata {
    pub id: usize,
    pub fingerprint: Fingerprint,
    pub kind: String,
    pub preview: String,
    pub file_names: Vec<String>,
    pub bytes: usize,
    pub partial: bool,
    pub copied_ms: u64,
    pub source: String,
    pub copy_count: usize,
    // 收藏状态（E01）：favorite 为真时 favorite_position 是收藏顺序，永不因复制、搜索或粘贴变化。
    pub favorite: bool,
    pub favorite_position: Option<u64>,
    // 来源应用（A06）：display_name 与图标文件名（icons 目录内的相对名，调用方拼绝对路径）。
    pub source_name: Option<String>,
    pub icon_file: Option<String>,
    // 收藏的标题与备注（E03/E04）；不是收藏或没写时为空串。
    pub title: String,
    pub note: String,
    #[serde(skip)]
    pub sort_order: i64,
    #[serde(skip)]
    pub formats: Vec<FormatMeta>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SourceApp {
    pub id: i64,
    pub process_name: String,
    pub exe_path: String,
    pub display_name: String,
    pub icon_file: String,
    pub icon_key: String,
}

#[derive(Clone, Default, Debug, Serialize)]
pub struct Counts {
    pub contents: usize,
    pub formats: usize,
    pub events: usize,
    pub indexed: usize,
    pub favorites: usize,
    pub source_apps: usize,
    // 回收站里的条数（v3 起）；contents 含这些，列表总数要减掉。
    pub trashed: usize,
}
/// 某一类内容的条数和字节数（存储管理，B05）。只算不在回收站里的。
#[derive(Clone, Debug, Serialize)]
pub struct KindUsage {
    pub kind: String,
    pub count: usize,
    pub bytes: u64,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Usage {
    pub kinds: Vec<KindUsage>,
    pub trashed_count: usize,
    pub trashed_bytes: u64,
    /// 数据库里已经空出来、要压缩（VACUUM）才还给磁盘的字节数。
    pub reclaimable_bytes: u64,
}
/// 文件类条目的路径（B04 失效检查用）。
#[derive(Clone, Debug)]
pub struct FileList {
    pub id: usize,
    pub favorite: bool,
    pub paths: Vec<String>,
}
#[derive(Clone, Default)]
pub struct Cache {
    pub items: Vec<Metadata>,
    pub total: usize,
    pub last_recorded_ms: Option<u64>,
    pub latest_sequence: Option<u32>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Ingest {
    pub outcome: &'static str,
    pub reason: Option<&'static str>,
    pub record_id: Option<usize>,
    pub event_id: Option<i64>,
    pub kind: Option<String>,
    pub copy_count: Option<usize>,
    pub write_ms: f64,
    pub transaction_ok: bool,
}
impl Ingest {
    /// 这次写入合并进了已有的那一条（`record_id` 是它的 id，格式被重写过）。
    pub fn merged(&self) -> bool {
        matches!(
            self.outcome,
            "merged_same_observation" | "merged_late_formats"
        )
    }
    fn skipped(outcome: &'static str, reason: Option<&'static str>) -> Self {
        Self {
            outcome,
            reason,
            record_id: None,
            event_id: None,
            kind: None,
            copy_count: None,
            write_ms: 0.0,
            transaction_ok: false,
        }
    }
}
#[derive(Clone, Default, Debug, Serialize)]
pub struct OpenReport {
    pub version: u32,
    // 这次打开时从哪个版本迁移上来的；没有迁移为 None。
    pub migrated_from: Option<u32>,
    pub open_ms: f64,
    pub load_ms: f64,
    pub counts: Counts,
}
#[derive(Clone, Default, Debug, Serialize)]
pub struct WriteStats {
    pub writes: usize,
    pub failures: usize,
    pub p95_ms: f64,
}

enum Source {
    // 池里借出的读连接；Reader drop 时还回池里。
    File(Option<Lease>),
    // 内存库的连接只在写入线程里；读也交给那条连接做，读写看到的是同一份数据。
    Memory(Store),
}
/// 借出的读连接，记着借出时池的代数；还回时代数变了（池被清空过）就直接关掉。
struct Lease {
    connection: Option<Connection>,
    generation: u64,
    pool: Arc<Pool>,
}
/// 磁盘库的读连接池。
/// - 上限 `POOL_MAX`：面板一次呼出通常并发 1~2 个读（列表 + 搜索/预览），4 个已够，再多只是占内存。
/// - 空闲超过 `POOL_IDLE` 的连接在下次存取池时关掉（不开后台线程）；连接带着 SQLite 页缓存，闲置久了该还给系统。
///   调用方知道自己空闲下来的时刻（如面板隐藏）时，用 `release_idle` 立即关掉池里的空闲连接。
/// - `clear` 用于压缩、换库等文件被整体改写的时刻：池里的连接全关，借出未还的也不再收回（代数不符）。
pub(crate) struct Pool {
    directory: PathBuf,
    idle: Mutex<Vec<(Connection, Instant)>>,
    generation: AtomicU64,
}
const POOL_MAX: usize = 4;
const POOL_IDLE: Duration = Duration::from_secs(45);
impl Pool {
    pub(crate) fn new(directory: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            directory,
            idle: Mutex::new(Vec::new()),
            generation: AtomicU64::new(0),
        })
    }
    fn take(self: &Arc<Self>) -> Result<Lease> {
        let generation = self.generation.load(Ordering::Acquire);
        let reused = {
            let mut idle = self.idle.lock().unwrap();
            idle.retain(|(_, since)| since.elapsed() < POOL_IDLE);
            idle.pop().map(|(connection, _)| connection)
        };
        let connection = match reused {
            Some(connection) => connection,
            None => schema::readonly(&self.directory.join("history.db"))?,
        };
        Ok(Lease {
            connection: Some(connection),
            generation,
            pool: self.clone(),
        })
    }
    /// 关掉池里所有空闲连接；借出的连接照常归还（不动代数）。
    pub(crate) fn release_idle(&self) {
        self.idle.lock().unwrap().clear();
    }
    pub(crate) fn clear(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.idle.lock().unwrap().clear();
    }
    #[cfg(test)]
    pub(crate) fn idle(&self) -> usize {
        self.idle.lock().unwrap().len()
    }
    #[cfg(test)]
    pub(crate) fn age_idle(&self, by: Duration) {
        for (_, since) in self.idle.lock().unwrap().iter_mut() {
            *since -= by;
        }
    }
}
impl Lease {
    fn connection(&self) -> &Connection {
        self.connection.as_ref().expect("连接只在 drop 时取走")
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let Some(connection) = self.connection.take() else {
            return;
        };
        // 还有没结束的事务就不要放回去：复用的连接不能带着过期的读快照。
        if !connection.is_autocommit() {
            return;
        }
        let pool = &self.pool;
        let mut idle = pool.idle.lock().unwrap();
        // 代数在持锁时再查一次，和 clear 互斥。
        if self.generation != pool.generation.load(Ordering::Acquire) {
            return;
        }
        idle.retain(|(_, since)| since.elapsed() < POOL_IDLE);
        if idle.len() < POOL_MAX {
            idle.push((connection, Instant::now()));
        }
    }
}
pub struct Reader {
    source: Source,
    hash: Hash,
}
impl Reader {
    /// 单独打开一个读者（不属于任何 Store 的池，drop 时连接即关闭）。
    pub fn open(directory: &Path, hash: Hash) -> Result<Self> {
        Self::pooled(&Pool::new(directory.to_path_buf()), hash)
    }
    pub(crate) fn pooled(pool: &Arc<Pool>, hash: Hash) -> Result<Self> {
        Ok(Self {
            source: Source::File(Some(pool.take()?)),
            hash,
        })
    }
    pub(crate) fn memory(store: Store, hash: Hash) -> Self {
        Self {
            source: Source::Memory(store),
            hash,
        }
    }
    #[cfg(test)]
    pub(crate) fn connection(&self) -> &Connection {
        match &self.source {
            Source::File(lease) => lease.as_ref().unwrap().connection(),
            Source::Memory(_) => panic!("内存库的连接在写入线程里"),
        }
    }
    fn run<T: Send + 'static>(
        &self,
        job: impl FnOnce(&Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        match &self.source {
            Source::File(lease) => job(lease.as_ref().unwrap().connection()),
            Source::Memory(store) => store.call(move |writer| job(&writer.connection)),
        }
    }
    pub fn recent(&self) -> Result<Vec<Metadata>> {
        self.run(|c| {
            let tx = c.unchecked_transaction()?;
            let items = schema::recent(&tx)?;
            tx.commit()?;
            Ok(items)
        })
    }
    pub fn counts(&self) -> Result<Counts> {
        self.run(|c| {
            let tx = c.unchecked_transaction()?;
            let counts = schema::counts(&tx)?;
            tx.commit()?;
            Ok(counts)
        })
    }
    // 收藏列表（E01）：按收藏顺序返回，不按复制时间。
    pub fn favorites(&self) -> Result<Vec<Metadata>> {
        self.run(|c| {
            let tx = c.unchecked_transaction()?;
            let items = schema::favorites(&tx)?;
            tx.commit()?;
            Ok(items)
        })
    }
    pub fn source_app_by_path(&self, exe_path: &str) -> Result<Option<SourceApp>> {
        let exe_path = exe_path.to_owned();
        self.run(move |c| {
            let tx = c.unchecked_transaction()?;
            let app = schema::source_app_by_path(&tx, &exe_path)?;
            tx.commit()?;
            Ok(app)
        })
    }
    /// 回收站（B12）：最近删除的在前，带删除时间。
    pub fn trash(&self) -> Result<Vec<(Metadata, u64)>> {
        self.run(|c| {
            let tx = c.unchecked_transaction()?;
            let items = schema::trash(&tx)?;
            tx.commit()?;
            Ok(items)
        })
    }
    /// 不在回收站里的文件类条目和它们的路径；`ids` 给定时只取这些条目。
    pub fn file_lists(&self, ids: Option<&[usize]>) -> Result<Vec<FileList>> {
        let ids = ids.map(<[usize]>::to_vec);
        self.run(move |c| {
            let tx = c.unchecked_transaction()?;
            let lists = schema::file_lists(&tx, ids.as_deref())?;
            tx.commit()?;
            Ok(lists)
        })
    }
    pub fn usage(&self) -> Result<Usage> {
        self.run(|c| {
            let tx = c.unchecked_transaction()?;
            let usage = schema::usage(&tx)?;
            tx.commit()?;
            Ok(usage)
        })
    }
    /// 存在库里的图片（没有源文件的截图等），`before_ms` 只取复制时间早于它的；按大小从大到小。
    pub fn images(&self, before_ms: Option<u64>) -> Result<Vec<Metadata>> {
        self.run(move |c| {
            let tx = c.unchecked_transaction()?;
            let items = schema::images(&tx, before_ms)?;
            tx.commit()?;
            Ok(items)
        })
    }
    pub fn content(&self, id: usize) -> Result<Option<std::sync::Arc<content::Content>>> {
        let hash = self.hash;
        self.run(move |c| {
            let tx = c.unchecked_transaction()?;
            let content = schema::content(&tx, id, hash)?;
            tx.commit()?;
            Ok(content)
        })
    }
    pub fn search(&self, query: &str) -> Result<Search> {
        let query = query.to_owned();
        self.run(move |c| search::search(c, &query))
    }
    pub fn event_exists(&self, id: i64) -> Result<bool> {
        self.run(move |c| {
            Ok(c.query_row(
                "SELECT EXISTS(SELECT 1 FROM copy_events WHERE id=?1)",
                [id],
                |r| r.get(0),
            )?)
        })
    }
    pub fn quick_check(&self) -> Result<()> {
        self.run(|c| schema::pragma_check(c, "quick_check"))
    }
}

pub(crate) fn writable(path: &Path) -> Result<Connection> {
    let c = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )?;
    c.busy_timeout(std::time::Duration::from_secs(5))?;
    c.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;")?;
    Ok(c)
}

#[cfg(test)]
mod backup_tests;
#[cfg(test)]
mod gates_tests;
#[cfg(test)]
mod perf_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod upgrade_tests;
