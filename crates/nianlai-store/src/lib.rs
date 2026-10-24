pub mod content;
pub mod hash;
mod migrations;
mod schema;
mod search;
mod snapshot;
mod write;

pub use migrations::VERSION;
pub use search::{Search, SearchMode};
pub use snapshot::{SnapshotReport, check, restore, snapshot, snapshot_due, validate_snapshot};
pub use write::Store;

use content::{Fingerprint, Hash};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::{
    path::Path,
    time::{Instant, SystemTime, UNIX_EPOCH},
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
    #[serde(skip)]
    pub sort_order: i64,
    #[serde(skip)]
    pub formats: Vec<FormatMeta>,
}

#[derive(Clone, Default, Debug, Serialize)]
pub struct Counts {
    pub contents: usize,
    pub formats: usize,
    pub events: usize,
    pub indexed: usize,
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

pub struct Reader {
    connection: Connection,
    hash: Hash,
}
impl Reader {
    pub fn open(directory: &Path, hash: Hash) -> Result<Self> {
        let connection = schema::readonly(&directory.join("history.db"))?;
        Ok(Self { connection, hash })
    }
    pub fn recent(&self) -> Result<Vec<Metadata>> {
        let tx = self.connection.unchecked_transaction()?;
        let items = schema::recent(&tx)?;
        tx.commit()?;
        Ok(items)
    }
    pub fn counts(&self) -> Result<Counts> {
        let tx = self.connection.unchecked_transaction()?;
        let counts = schema::counts(&tx)?;
        tx.commit()?;
        Ok(counts)
    }
    pub fn content(&self, id: usize) -> Result<Option<std::sync::Arc<content::Content>>> {
        let tx = self.connection.unchecked_transaction()?;
        let content = schema::content(&tx, id, self.hash)?;
        tx.commit()?;
        Ok(content)
    }
    pub fn search(&self, query: &str) -> Result<Search> {
        search::search(&self.connection, query)
    }
    pub fn event_exists(&self, id: i64) -> Result<bool> {
        Ok(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM copy_events WHERE id=?1)",
            [id],
            |r| r.get(0),
        )?)
    }
    pub fn quick_check(&self) -> Result<()> {
        schema::pragma_check(&self.connection, "quick_check")
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
mod tests;
#[cfg(test)]
mod upgrade_tests;
