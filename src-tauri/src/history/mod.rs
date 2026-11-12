pub mod backup;
pub mod content;
pub mod files;
pub mod preview;
pub mod snapshots;
pub mod storage;

use crate::log::{Logger, now_ms};
use content::{Content, Fingerprint, Hash};
pub use nianlai_store::{CopyEvent, Ingest, older};
use nianlai_store::{MemoryOptions, Metadata, Store};
use serde::Serialize;
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

type Changed = Arc<dyn Fn() + Send + Sync>;

/// 新复制的内容先留在内存里，等一小段时间再写盘（A11 临时内容）：
/// 文本扩展等工具借用剪贴板后会很快还原成原来的内容，期间被还原就整条丢弃，从未落盘。
struct Deferred {
    content: Arc<Content>,
    event: CopyEvent,
    partial: bool,
    // 这条之前剪贴板上的内容；剪贴板变回它，就说明这条是临时借用。
    previous: Option<Fingerprint>,
    generation: u64,
}

/// 采集线程拿到的结果：已写入、暂存待写、或判定为临时内容而丢弃。
pub enum Captured {
    Stored(Ingest),
    Deferred,
    Transient,
}

/// 回收站保留天数（B12）：删除满这么久的条目在后台定时检查时物理删除。
pub const TRASH_DAYS: u64 = 7;
const DAY_MS: u64 = 24 * 60 * 60 * 1000;
/// 本次运行里可以按 Ctrl+Z 依次撤销的删除条数。
const UNDO_DEPTH: usize = 50;

/// 回收站里的一条：列表项加上删除时间。
#[derive(Clone, Serialize)]
pub struct Trashed {
    #[serde(flatten)]
    pub item: Preview,
    pub deleted_ms: u64,
}

#[derive(Clone, Serialize)]
pub struct Preview {
    id: usize,
    kind: String,
    preview: String,
    file_names: Vec<String>,
    source: String,
    copied_ms: u64,
    copy_count: usize,
    partial: bool,
    // E01：收藏状态；favorite_position 是收藏顺序（收藏列表的排序键），永不因复制、搜索、粘贴变化。
    favorite: bool,
    favorite_position: Option<u64>,
    // A06：来源应用的显示名（FileDescription，缺省为文件名或进程名）与图标 PNG 的绝对路径。
    source_name: Option<String>,
    source_icon: Option<String>,
    // M2-6：收藏的标题与备注；不是收藏或没写时为空，不序列化。
    #[serde(skip_serializing_if = "String::is_empty")]
    title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    note: String,
    // M2-3：文件类条目的失效检查结论；文件都在（或不是文件条目）时不带。
    #[serde(skip_serializing_if = "Option::is_none")]
    file_state: Option<files::Mark>,
}
#[derive(Serialize)]
pub struct Snapshot {
    items: Vec<Preview>,
    total: usize,
    recording: bool,
    last_recorded_ms: Option<u64>,
    store_status: Option<&'static str>,
}
#[derive(Serialize)]
pub struct SearchReply {
    request_id: u64,
    items: Vec<Preview>,
}

// 面板控制器（panel.rs）持有唯一的 History，但字段不对外公开；Tauri 命令（lib.rs）
// 通过这里的进程注册表取当前实例。产品进程只有一个，测试不受影响。
// B10：粘贴旧条目后是否把它移到列表最前；启动和保存设置时更新。
static REUSE_TO_TOP: AtomicBool = AtomicBool::new(true);
pub fn set_reuse_to_top(value: bool) {
    REUSE_TO_TOP.store(value, Ordering::Release);
}
// 内存模式的库：同一套存储层，只是开在内存里；编号与磁盘库错开（见 `MemoryOptions::fallback`）。
fn memory_store(hash: Hash) -> Store {
    Store::open_memory(hash, MemoryOptions::fallback()).expect("内存库应当总能打开")
}
// 把存储层的错误码翻成给用户看的话。
fn label_error(error: nianlai_store::Error) -> String {
    match error {
        nianlai_store::Error::Invalid("reorder_duplicate") => "常用顺序里有重复的条目".into(),
        nianlai_store::Error::Invalid("reorder_mismatch") => {
            "常用顺序与当前常用不一致，请刷新后重试".into()
        }
        nianlai_store::Error::Invalid("not_favorite") => "只有常用的条目才能设置标题和备注".into(),
        other => user_message(&other),
    }
}

// 存储层错误翻成用户能看懂的话；原始错误由调用方照常写日志。
// SQLite 的扩展错误码低 8 位是主码：5 忙、6 表被锁、13 磁盘满、8 只读、3 无权限、23 授权拒绝、
// 11 损坏、26 不是数据库、14 打不开；扩展码 1032（只读：库文件被移走）单独说明。
pub(crate) fn user_message(error: &nianlai_store::Error) -> String {
    use nianlai_store::Error;
    use std::io::ErrorKind;
    match error {
        Error::Busy => "数据库正忙，请稍后重试".into(),
        Error::Sqlite(code) if matches!(code & 0xff, 5 | 6) => "数据库正忙，请稍后重试".into(),
        Error::Sqlite(code) if code & 0xff == 13 => "磁盘空间不足，请清理磁盘后重试".into(),
        Error::Io(ErrorKind::StorageFull) => "磁盘空间不足，请清理磁盘后重试".into(),
        // 1032 = SQLITE_READONLY_DBMOVED：打开着的库文件被移走或删除了，要放在“只读”之前。
        Error::Sqlite(1032) => "数据文件已被移动或删除，请重启拈来".into(),
        Error::Sqlite(code) if matches!(code & 0xff, 3 | 8 | 23) => {
            "数据文件是只读的或没有权限，请检查数据目录的权限".into()
        }
        Error::Io(ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem) => {
            "数据文件是只读的或没有权限，请检查数据目录的权限".into()
        }
        Error::Sqlite(code) if matches!(code & 0xff, 11 | 26) => {
            "历史库文件已损坏，可以从存档恢复".into()
        }
        Error::Sqlite(code) if code & 0xff == 14 => {
            "找不到或无法打开数据文件，可能已被移动、删除或被占用".into()
        }
        Error::Io(ErrorKind::NotFound) => "找不到数据文件，可能已被移动或删除".into(),
        Error::NewerSchema { .. } => "历史库来自更新版本的拈来，请先升级拈来".into(),
        Error::Sqlite(code) => format!("操作失败，请稍后重试（Sqlite {code}）"),
        Error::Io(kind) => format!("操作失败，请稍后重试（Io {kind:?}）"),
        Error::Invalid(reason) => format!("操作失败，请稍后重试（Invalid {reason}）"),
        Error::Closed => "操作失败，请稍后重试（Closed）".into(),
    }
}

// 全部可变状态聚在 Inner 里；对外只有 History（内部是 Arc<Inner>，可 Clone），各处拿到的是同一份。
pub struct Inner {
    application_guard: Mutex<Option<std::fs::File>>,
    directory: OnceLock<PathBuf>,
    // 永远有一个库：启动时先是内存库，打开磁盘库成功后换成磁盘库；
    // 打不开或完整性检查失败时留在（或换回）内存库。区分两者只看 `Store::is_memory`。
    store: RwLock<Store>,
    // 完整性检查失败而被换下的磁盘库：不再使用，但也不关闭。
    // SQLite 关闭最后一个连接时会把 WAL 检查点写回 history.db、删掉 -wal 并释放 history.lock，
    // 被判损坏的库不能因此被改动；所以像旧版本那样一直持有到进程退出。
    retired: Mutex<Vec<Store>>,
    status: Mutex<Option<&'static str>>,
    recording: AtomicBool,
    last_write: Mutex<Option<Instant>>,
    refresh: Mutex<Option<crate::platform::apps::Refresher>>,
    // 待删除（撤销窗口内）的那一条；同一时间只挂一条，新的删除会先提交上一条。
    // 本次运行里删除过的条目，最近的在最后；Ctrl+Z 跨呼出依次恢复。
    undo_stack: Mutex<Vec<usize>>,
    deferred: Mutex<Option<Deferred>>,
    generation: AtomicU64,
    // 最近一次出现在剪贴板上的内容（含我们自己写回的）。
    last_seen: Mutex<Option<Fingerprint>>,
    changed: Mutex<Option<Changed>>,
    files: files::Marks,
    // 预览图缓存：见 preview.rs。
    images: Mutex<preview::ImageCache>,
    hash: Hash,
    logger: Logger,
}

#[derive(Clone)]
pub struct History(Arc<Inner>);
impl std::ops::Deref for History {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}
impl History {
    pub fn new(hash: Hash, logger: Logger) -> Self {
        Self(Arc::new(Inner {
            application_guard: Mutex::new(None),
            directory: OnceLock::new(),
            store: RwLock::new(memory_store(hash)),
            retired: Mutex::new(Vec::new()),
            status: Mutex::new(Some("正在加载历史")),
            recording: AtomicBool::new(false),
            last_write: Mutex::new(None),
            refresh: Mutex::new(None),
            undo_stack: Mutex::new(Vec::new()),
            deferred: Mutex::new(None),
            generation: AtomicU64::new(0),
            last_seen: Mutex::new(None),
            changed: Mutex::new(None),
            files: files::Marks::default(),
            images: Mutex::default(),
            hash,
            logger,
        }))
    }
    /// 采集线程的入口：按设置决定立即写入，还是先暂存再写（A11）。
    pub fn capture(
        &self,
        content: Arc<Content>,
        event: CopyEvent,
        partial: bool,
        now: Instant,
    ) -> Result<Captured, String> {
        let policy = crate::capture::rules::gate().policy();
        let window = policy
            .transient
            .then(|| Duration::from_millis(policy.transient_ms));
        self.capture_with(content, event, partial, now, window)
    }
    /// 暂存内容写入后用来通知面板刷新。
    pub fn set_changed(&self, changed: Changed) {
        *self.changed.lock().unwrap() = Some(changed);
    }
    pub fn set_recording(&self, value: bool) {
        self.recording.store(value, Ordering::Release);
    }
    pub fn last_write(&self) -> Option<Instant> {
        *self.last_write.lock().unwrap()
    }
    pub fn maintenance(&self, changed: Arc<dyn Fn() + Send + Sync>) {
        let Some(store) = self.store() else {
            return;
        };
        let history = self.clone();
        let logger = self.logger.clone();
        if std::thread::Builder::new().name("history-check".into()).spawn(move || {
            let started=Instant::now();
            let result=store.reader().and_then(|r|r.quick_check());
            logger.event("store_check",json!({"success":result.is_ok(),"elapsed_ms":started.elapsed().as_secs_f64()*1000.0,"error_code":result.as_ref().err().map(ToString::to_string)}));
            if result.is_err() {
                history.fall_back_to_memory("历史库异常，未落盘");
                changed();return;
            }
            // 启动时先检查一次，之后每小时检查是否到了每日快照的时间（程序常常连开好几天）。
            loop {
                let Some(directory)=history.store_directory() else {return};
                snapshots::automatic(&directory, &logger);
                history.purge_expired();
                if history.check_files_daily() {
                    history.notify_changed();
                }
                std::thread::sleep(snapshots::CHECK_INTERVAL);
            }
        }).is_err() {self.logger.event("store_check_failed",json!({"error_code":"thread_start"}));}
    }
    // 图片降级成路径时 PNG 存在这里；库没打开（内存模式）也照样用数据目录。
    pub fn exports(&self) -> Option<PathBuf> {
        self.exports_path()
    }
    // 采集线程在内容保存成功后调用：把来源 exe 交给后台刷新显示名和图标（A06）。
    pub fn refresh_source_app(&self, exe_path: &str) {
        if let Some(refresher) = self.refresh.lock().unwrap().as_ref() {
            refresher.refresh(exe_path);
        }
    }
}

impl History {
    pub fn store_directory(&self) -> Option<PathBuf> {
        self.store().map(|store| store.directory().to_path_buf())
    }
    /// 磁盘库；内存模式返回 None。快照、备份、存储统计、图标缓存等只在磁盘库上有意义的功能
    /// 都用它判断“能不能做”，其余功能走 `db()`。
    pub(crate) fn store(&self) -> Option<Store> {
        Some(self.db()).filter(|store| !store.is_memory())
    }
    /// 当前的库（磁盘或内存）。
    pub(crate) fn db(&self) -> Store {
        self.store.read().unwrap().clone()
    }
    /// 完整性检查失败：换成一个全新的内存库。旧库留在磁盘上不动：旧 Store 收进 `retired`，
    /// 不再使用也不 drop（drop 会让 SQLite 做检查点、改动 history.db 和 -wal）。
    fn fall_back_to_memory(&self, status: &'static str) {
        let old = std::mem::replace(&mut *self.store.write().unwrap(), memory_store(self.hash));
        if !old.is_memory() {
            self.retired.lock().unwrap().push(old);
        }
        self.images.lock().unwrap().clear();
        *self.status.lock().unwrap() = Some(status);
    }
    pub fn initialize(&self) {
        let location = self.logger.directories.data_source;
        let directory = self.logger.directories.data.clone();
        let _ = self.directory.set(directory.clone());
        // 上次登记的快照恢复要在打开库之前完成。
        snapshots::apply_pending(&directory, &self.logger);
        // 导入备份遗留的临时目录（覆盖要用的那份已在上一步用完）。
        nianlai_store::clean_stale_imports(&directory);
        let opened = (|| {
            *self.application_guard.lock().unwrap() =
                Some(nianlai_store::application_guard(&directory)?);
            Store::open(&directory, self.hash)
        })();
        match opened {
            Ok(store) => {
                let mut report = serde_json::to_value(store.report()).unwrap();
                report["location"] = json!(location);
                self.logger.event("store_opened", report);
                *self.status.lock().unwrap() = None;
                *self.last_seen.lock().unwrap() = store
                    .cache()
                    .items
                    .iter()
                    .max_by_key(|item| item.copied_ms)
                    .map(|item| item.fingerprint);
                // 来源应用的显示名与图标在采集线程之外刷新（A06）：门槛 P95 ≤ 500 ms 不受影响。
                let refresher = crate::platform::apps::start(self.clone(), self.logger.clone());
                *self.refresh.lock().unwrap() = Some(refresher);
                *self.store.write().unwrap() = store;
                self.images.lock().unwrap().clear();
            }
            Err(error) => {
                *self.status.lock().unwrap() = Some(
                    if matches!(error, nianlai_store::Error::NewerSchema { .. }) {
                        "这个数据目录由更新版本的拈来创建，请升级后再打开"
                    } else {
                        "历史库异常，未落盘"
                    },
                );
                self.logger.event(
                    "store_open_failed",
                    json!({"location":location,"error_code":error.to_string()}),
                );
            }
        }
        if std::env::var("NIANLAI_G0_SAMPLES").as_deref() == Ok("1") {
            let store = self.db();
            let result = crate::samples::all()
                .into_iter()
                .rev()
                .try_for_each(|sample| {
                    let content = Content::text(&sample.text, self.hash)?;
                    store
                        .seed(
                            content,
                            CopyEvent {
                                time_ms: now_ms(),
                                sequence: 0,
                                source_process: "示例".into(),
                                source_path: String::new(),
                                owner_process: "示例".into(),
                                owner_pid: 0,
                                observed: Instant::now(),
                            },
                        )
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                });
            if let Err(error) = result {
                self.logger
                    .event("store_seed_failed", json!({"error_code":error}));
            }
        }
    }
    // 图片降级成路径时 PNG 存在这里；库没打开（内存模式）也照样用数据目录。
    pub fn exports_path(&self) -> Option<PathBuf> {
        self.directory
            .get()
            .map(|directory| directory.join("exports"))
    }
    // 图标缓存目录（A06）：<数据目录>\icons。
    pub fn icons(&self) -> Option<PathBuf> {
        self.directory
            .get()
            .map(|directory| directory.join("icons"))
    }
    // 图标文件的绝对路径：icon_file 是 icons 目录内的相对名。
    fn icon_path(&self, icon_file: Option<&str>) -> Option<String> {
        let file = icon_file.filter(|file| !file.is_empty())?;
        let directory = self.directory.get()?;
        directory
            .join("icons")
            .join(file)
            .to_str()
            .map(str::to_owned)
    }
    fn preview(&self, item: Metadata) -> Preview {
        Preview {
            id: item.id,
            kind: item.kind,
            preview: item.preview,
            file_names: item.file_names,
            source: if item.source.is_empty() {
                "来源未知".into()
            } else {
                item.source
            },
            copied_ms: item.copied_ms,
            copy_count: item.copy_count,
            partial: item.partial,
            favorite: item.favorite,
            favorite_position: item.favorite_position,
            source_name: item.source_name,
            source_icon: self.icon_path(item.icon_file.as_deref()),
            title: item.title,
            note: item.note,
            file_state: None,
        }
    }
    /// 给列表项带上文件失效结论。
    fn annotate(&self, items: &mut [Preview]) {
        for item in items {
            item.file_state = self.files.get(item.id);
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        let cache = self.db().cache();
        let mut snapshot = Snapshot {
            items: cache
                .items
                .into_iter()
                .map(|item| self.preview(item))
                .collect(),
            total: cache.total,
            recording: false,
            last_recorded_ms: cache.last_recorded_ms,
            store_status: None,
        };
        self.annotate(&mut snapshot.items);
        snapshot.recording = self.recording.load(Ordering::Acquire);
        snapshot.store_status = self.status.lock().unwrap().or_else(|| {
            crate::capture::rules::gate()
                .paused()
                .then_some("已暂停记录")
        });
        snapshot
    }
    pub fn search(&self, query: &str, request_id: u64) -> Result<SearchReply, String> {
        let started = Instant::now();
        let store = self.db();
        // 内存库用同一套搜索，日志里仍标 memory_substring，便于从日志区分两种模式。
        let memory = store.is_memory();
        let mut raw_error = None;
        let (result, mode) = match store.reader().and_then(|r| r.search(query)) {
            Ok(result) => (
                Ok(result
                    .items
                    .into_iter()
                    .map(|item| self.preview(item))
                    .collect()),
                if memory {
                    json!("memory_substring")
                } else {
                    json!(result.mode)
                },
            ),
            Err(error) => {
                raw_error = Some(error.to_string());
                (Err(user_message(&error)), json!("failed"))
            }
        };
        self.logger.event("search",json!({"words":query.split_whitespace().count(),"characters":query.chars().count(),"mode":mode,
            "results":result.as_ref().map(Vec::len).ok(),"elapsed_ms":started.elapsed().as_secs_f64()*1000.0,"error_code":raw_error}));
        result.map(|mut items: Vec<Preview>| {
            self.annotate(&mut items);
            SearchReply { request_id, items }
        })
    }
    pub fn ingest(
        &self,
        content: Arc<Content>,
        event: CopyEvent,
        partial: bool,
        now: Instant,
    ) -> Result<Ingest, String> {
        // 错误只进日志（采集线程），不给用户看，保持原始文本。
        let result = self
            .db()
            .ingest(content, event, partial, now)
            .map_err(|e| e.to_string())?;
        // 合并到已有 id：它的格式被重写了，缩略图缓存里的旧图要作废。
        if result.merged()
            && let Some(id) = result.record_id
        {
            self.images.lock().unwrap().remove(id);
        }
        Ok(result)
    }
    /// 面板隐藏时调用：释放读连接池里的空闲连接和缩略图缓存（下次用到再按需重建）。
    pub fn release_idle(&self) {
        self.db().release_idle_readers();
        self.images.lock().unwrap().clear();
    }
    fn capture_with(
        &self,
        content: Arc<Content>,
        event: CopyEvent,
        partial: bool,
        now: Instant,
        window: Option<Duration>,
    ) -> Result<Captured, String> {
        let fingerprint = content.fingerprint;
        // 整个过程持锁：暂存内容的写入和新内容的处理按复制顺序进行。
        let mut deferred = self.deferred.lock().unwrap();
        if let Some(pending) = deferred.take() {
            if pending.previous == Some(fingerprint) && pending.content.fingerprint != fingerprint {
                // 剪贴板变回了借用前的内容：暂存的那条丢弃，还原回来的也不算一次新复制。
                *self.last_seen.lock().unwrap() = Some(fingerprint);
                self.logger.event(
                    "capture_transient",
                    json!({"held_ms":pending.event.observed.elapsed().as_secs_f64()*1000.0,"kind":pending.content.kind}),
                );
                return Ok(Captured::Transient);
            }
            self.commit_deferred(pending);
        }
        let previous = self.last_seen.lock().unwrap().replace(fingerprint);
        let Some(window) = window else {
            drop(deferred);
            return self
                .ingest(content, event, partial, now)
                .map(Captured::Stored);
        };
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let shared = self.clone();
        let timer = std::thread::Builder::new()
            .name("history-deferred".into())
            .spawn(move || {
                std::thread::sleep(window);
                let mut deferred = shared.deferred.lock().unwrap();
                if deferred
                    .as_ref()
                    .is_some_and(|pending| pending.generation == generation)
                {
                    let pending = deferred.take().unwrap();
                    shared.commit_deferred(pending);
                }
            });
        let pending = Deferred {
            content,
            event,
            partial,
            previous,
            generation,
        };
        if timer.is_err() {
            // 起不了计时线程就直接写，不能让内容一直留在内存里。
            drop(deferred);
            let Deferred {
                content,
                event,
                partial,
                ..
            } = pending;
            return self
                .ingest(content, event, partial, now)
                .map(Captured::Stored);
        }
        *deferred = Some(pending);
        Ok(Captured::Deferred)
    }
    pub fn flush_deferred(&self) {
        let mut deferred = self.deferred.lock().unwrap();
        if let Some(pending) = deferred.take() {
            self.commit_deferred(pending);
        }
    }
    // 调用方持有 deferred 锁。
    fn commit_deferred(&self, pending: Deferred) {
        let source_path = pending.event.source_path.clone();
        let held_ms = pending.event.observed.elapsed().as_secs_f64() * 1000.0;
        let result = self.ingest(
            pending.content,
            pending.event,
            pending.partial,
            Instant::now(),
        );
        self.logger.event("capture_commit", json!({
            "held_ms":held_ms,"outcome":result.as_ref().ok().map(|r| r.outcome),"reason":result.as_ref().ok().and_then(|r| r.reason),
            "record_id":result.as_ref().ok().and_then(|r| r.record_id),"kind":result.as_ref().ok().and_then(|r| r.kind.clone()),
            "copy_count":result.as_ref().ok().and_then(|r| r.copy_count),"write_ms":result.as_ref().ok().map(|r| r.write_ms),
            "error_code":result.as_ref().err()
        }));
        if let Ok(Ingest {
            record_id: Some(_), ..
        }) = result
        {
            if !source_path.is_empty()
                && let Some(refresher) = self.refresh.lock().unwrap().as_ref()
            {
                refresher.refresh(&source_path);
            }
            let changed = self.changed.lock().unwrap().clone();
            if let Some(changed) = changed {
                changed();
            }
        }
    }
    // 仅在采集、搜索或粘贴工作线程调用；界面用 snapshot，绝不读 BLOB。
    pub fn get(&self, id: usize) -> Result<Option<Arc<Content>>, String> {
        self.load(id).map_err(|e| user_message(&e))
    }
    // 同 `get`，但保留原始错误，给只写日志的调用方。
    pub(crate) fn load(&self, id: usize) -> nianlai_store::Result<Option<Arc<Content>>> {
        self.db().reader().and_then(|r| r.content(id))
    }
    pub fn before_write(
        &self,
        id: usize,
        fingerprint: Fingerprint,
        written: Fingerprint,
        now: Instant,
    ) -> bool {
        let found = self
            .db()
            .before_write(id, fingerprint, written, now)
            .unwrap_or(false);
        if found {
            *self.last_write.lock().unwrap() = Some(now);
            *self.last_seen.lock().unwrap() = Some(written);
        }
        found
    }
    pub fn reuse(&self, id: usize) {
        if !REUSE_TO_TOP.load(Ordering::Acquire) {
            return;
        }
        if let Err(error) = self.db().reuse(id, now_ms()) {
            self.logger.event(
                "store_reuse_failed",
                json!({"error_code":error.to_string()}),
            );
        }
    }
    pub fn latest_sequence(&self) -> Option<u32> {
        self.db().latest_sequence()
    }
    pub fn skipped_sequence(&self, sequence: u32) {
        if let Err(error) = self.db().skipped_sequence(sequence) {
            self.logger
                .event("store_skip_failed", json!({"error_code":error.to_string()}));
        }
    }
    /// 面板 Delete：移进回收站（B12）。收藏的也可以，确认由界面负责（B13）。
    pub fn delete(&self, id: usize) -> Result<bool, String> {
        let result = self.db().delete(id);
        if let Ok(true) = result {
            let mut stack = self.undo_stack.lock().unwrap();
            stack.retain(|old| *old != id);
            stack.push(id);
            if stack.len() > UNDO_DEPTH {
                stack.remove(0);
            }
        }
        self.logger.event(
            "history_deleted",
            json!({"trashed":result.as_ref().ok(),"error_code":result.as_ref().err().map(ToString::to_string)}),
        );
        result.map_err(|e| user_message(&e))
    }
    /// Ctrl+Z：恢复本次运行里最近删除、还在回收站的一条；跨呼出有效。返回恢复的条目。
    pub fn undo_delete(&self) -> Result<Option<Preview>, String> {
        loop {
            let Some(id) = self.undo_stack.lock().unwrap().pop() else {
                self.logger
                    .event("history_delete_undo", json!({"success":false}));
                return Ok(None);
            };
            // 已经从回收站恢复、清掉或又被复制回来的跳过，继续找上一条。
            if let Some(item) = self.restore_trashed(id)? {
                self.logger
                    .event("history_delete_undo", json!({"success":true}));
                return Ok(Some(item));
            }
        }
    }
    /// 回收站里恢复指定条目；不在回收站返回 None。
    pub fn restore(&self, id: usize) -> Result<Option<Preview>, String> {
        let restored = self.restore_trashed(id)?;
        self.undo_stack.lock().unwrap().retain(|old| *old != id);
        self.logger
            .event("trash_restored", json!({"success":restored.is_some()}));
        Ok(restored)
    }
    fn restore_trashed(&self, id: usize) -> Result<Option<Preview>, String> {
        self.db()
            .restore(id)
            .map(|item| item.map(|item| self.preview(item)))
            .map_err(|e| user_message(&e))
    }
    pub fn trash(&self) -> Result<Vec<Trashed>, String> {
        let items: Vec<_> = self
            .db()
            .reader()
            .and_then(|r| r.trash())
            .map_err(|e| user_message(&e))?
            .into_iter()
            .map(|(item, deleted_ms)| (self.preview(item), deleted_ms))
            .collect();
        Ok(items
            .into_iter()
            .map(|(item, deleted_ms)| Trashed { item, deleted_ms })
            .collect())
    }
    /// 物理删除：`ids` 为 None 时清空整个回收站。
    pub fn purge(&self, ids: Option<Vec<usize>>) -> Result<usize, String> {
        self.purge_before(ids, u64::MAX, "manual")
    }
    /// 后台定时调用：清掉删除满 TRASH_DAYS 天的。
    pub fn purge_expired(&self) {
        let _ = self.purge_before(
            None,
            now_ms().saturating_sub(TRASH_DAYS * DAY_MS),
            "expired",
        );
    }
    fn purge_before(
        &self,
        ids: Option<Vec<usize>>,
        before_ms: u64,
        reason: &str,
    ) -> Result<usize, String> {
        let result = self.db().purge(ids, before_ms);
        self.images.lock().unwrap().clear();
        if result.as_ref().is_ok_and(|count| *count > 0) || result.is_err() {
            self.logger.event(
                "trash_purged",
                json!({"reason":reason,"count":result.as_ref().ok(),"error_code":result.as_ref().err().map(ToString::to_string)}),
            );
        }
        result.map_err(|e| user_message(&e))
    }
    pub fn favorites(&self) -> Result<Vec<Preview>, String> {
        let mut items: Vec<Preview> = self
            .db()
            .reader()
            .and_then(|r| r.favorites())
            .map(|items| items.into_iter().map(|item| self.preview(item)).collect())
            .map_err(|e| user_message(&e))?;
        self.annotate(&mut items);
        Ok(items)
    }
    pub fn set_favorite(&self, id: usize, favorite: bool) -> Result<Option<Preview>, String> {
        self.db()
            .set_favorite(id, favorite)
            .map(|item| item.map(|item| self.preview(item)))
            .map_err(|e| user_message(&e))
    }
    /// 重排收藏：ids 是界面展示的全部收藏的新顺序。返回新的收藏列表。
    pub fn reorder_favorites(&self, ids: &[usize]) -> Result<Vec<Preview>, String> {
        let mut items: Vec<Preview> = self
            .db()
            .reorder_favorites(ids)
            .map(|items| items.into_iter().map(|item| self.preview(item)).collect())
            .map_err(label_error)?;
        self.annotate(&mut items);
        Ok(items)
    }
    /// 修改收藏的标题与备注；不是收藏则报错。
    pub fn set_favorite_label(
        &self,
        id: usize,
        title: &str,
        note: &str,
    ) -> Result<Preview, String> {
        self.db()
            .set_favorite_label(id, title, note)
            .map(|item| self.preview(item))
            .map_err(label_error)
    }
    pub fn summary(&self) {
        let stats = self.store().as_ref().map(Store::stats).unwrap_or_default();
        self.logger.event("store_summary", json!(stats));
    }
}

#[cfg(test)]
mod gate_tests;
#[cfg(all(test, windows))]
mod tests;

#[cfg(test)]
mod startup_tests {
    use super::*;
    use crate::settings::{Directories, tests::Directory};
    use std::fs;

    fn directories(directory: &Directory) -> Directories {
        Directories {
            settings_file: directory.0.join("settings.json"),
            data: directory.0.join("configured-data"),
            logs: directory.0.join("logs"),
            data_source: "设置文件指定",
            test_data_dir: false,
        }
    }

    #[test]
    fn initializes_store_and_exports_in_startup_data_directory() {
        let directory = Directory::new();
        let directories = directories(&directory);
        let logger = Logger::with_settings(&directories, 20).unwrap();
        let history = History::new(nianlai_store::hash::sha256, logger);
        history.initialize();
        assert_eq!(history.store().unwrap().directory(), directories.data);
        assert!(directories.data.join("history.db").is_file());
        assert_eq!(history.exports(), Some(directories.data.join("exports")));
        assert_eq!(history.icons(), Some(directories.data.join("icons")));
        assert_eq!(history.snapshot().store_status, None);
        assert!(!directories.settings_file.exists());
    }

    #[test]
    fn stored_and_memory_search_logs_never_include_content_or_queries() {
        const CONTENT: &str = "正文隐私哨兵-9c41e2，包含查询隐私词-8e5b72";
        const QUERY: &str = "查询隐私词-8e5b72";
        for persistent in [false, true] {
            let directory = Directory::new();
            let directories = directories(&directory);
            let logger = Logger::with_settings(&directories, 20).unwrap();
            let history = History::new(nianlai_store::hash::sha256, logger.clone());
            if persistent {
                history.initialize();
                assert!(history.store().is_some());
            }
            let now = Instant::now();
            let saved = history
                .ingest(
                    Content::text(CONTENT, nianlai_store::hash::sha256).unwrap(),
                    CopyEvent {
                        time_ms: now_ms(),
                        sequence: 1,
                        source_process: "privacy-test.exe".into(),
                        source_path: String::new(),
                        owner_process: "privacy-test.exe".into(),
                        owner_pid: 1,
                        observed: now,
                    },
                    false,
                    now,
                )
                .unwrap();
            let result = history.search(QUERY, 1).unwrap();
            assert_eq!(result.items.len(), 1);
            assert_eq!(result.items[0].id, saved.record_id.unwrap());
            assert_eq!(result.items[0].preview, CONTENT);
            assert!(
                history
                    .search("零结果查询哨兵-3fd610", 2)
                    .unwrap()
                    .items
                    .is_empty()
            );
            history.summary();
            logger.flush();
            let text = fs::read_dir(&directories.logs)
                .unwrap()
                .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
                .collect::<String>();
            for private in [CONTENT, QUERY, "零结果查询哨兵-3fd610"] {
                assert!(!text.contains(private));
            }
            let rows: Vec<serde_json::Value> = text
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(
                rows.iter().filter(|row| row["event"] == "search").count(),
                2
            );
            for row in rows {
                for field in [
                    "text",
                    "title",
                    "window_title",
                    "clipboard_text",
                    "query",
                    "preview",
                    "content",
                ] {
                    assert!(row.get(field).is_none(), "日志不应包含字段 {field}");
                }
            }
        }
    }

    #[test]
    fn newer_schema_in_startup_directory_preserves_database_and_reports_upgrade() {
        let directory = Directory::new();
        let directories = directories(&directory);
        let mut database =
            include_bytes!("../../../crates/nianlai-store/tests/fixtures/v1.db").to_vec();
        // SQLite 文件头的 user_version 位于第 60 字节；夹具无 WAL，直接构造更新版本的库。
        database[60..64].copy_from_slice(&(nianlai_store::VERSION + 1).to_be_bytes());
        fs::create_dir(&directories.data).unwrap();
        let database_file = directories.data.join("history.db");
        fs::write(&database_file, &database).unwrap();
        let logger = Logger::with_settings(&directories, 20).unwrap();
        let history = History::new(nianlai_store::hash::sha256, logger);
        history.initialize();
        assert!(history.store().is_none());
        assert_eq!(
            history.snapshot().store_status,
            Some("这个数据目录由更新版本的拈来创建，请升级后再打开")
        );
        assert_eq!(history.exports(), Some(directories.data.join("exports")));
        assert_eq!(fs::read(database_file).unwrap(), database);
        assert!(!directories.data.join("backups").exists());
    }

    // 命令层（lib.rs）拿到的就是这个 History 句柄，磁盘与内存模式行为一致（M1-S）。
    #[test]
    fn history_serves_favorites_for_stored_and_memory_modes() {
        for persistent in [false, true] {
            let directory = Directory::new();
            let directories = directories(&directory);
            let logger = Logger::with_settings(&directories, 20).unwrap();
            let history = History::new(nianlai_store::hash::sha256, logger);
            if persistent {
                history.initialize();
            }
            let shared = &history;
            let now = Instant::now();
            let saved = shared
                .ingest(
                    Content::text("收藏注册表", nianlai_store::hash::sha256).unwrap(),
                    CopyEvent {
                        time_ms: now_ms(),
                        sequence: 1,
                        source_process: "registry-test.exe".into(),
                        source_path: r"C:\Tools\registry-test.exe".into(),
                        owner_process: "registry-test.exe".into(),
                        owner_pid: 1,
                        observed: now,
                    },
                    false,
                    now,
                )
                .unwrap();
            let id = saved.record_id.unwrap();
            assert!(shared.set_favorite(id, true).unwrap().unwrap().favorite);
            let favorites = shared.favorites().unwrap();
            assert_eq!(favorites.len(), 1);
            assert_eq!(favorites[0].id, id);
            assert_eq!(favorites[0].favorite_position, Some(1));
            assert_eq!(favorites[0].source_name.as_deref(), Some("registry-test"));
            // 图标要等后台线程刷新（测试不等待），此处为空。
            assert_eq!(favorites[0].source_icon, None);
            assert!(!shared.set_favorite(id, false).unwrap().unwrap().favorite);
            assert!(shared.favorites().unwrap().is_empty());
            // 列表快照同样带收藏字段。
            let listed = shared.snapshot();
            assert!(listed.items.iter().all(|item| !item.favorite));
        }
    }

    // M2-6：重排、标题备注（含搜索）与 reuse_to_top 开关，存储模式和内存模式行为一致。
    #[test]
    fn reorder_labels_search_and_reuse_switch_work_in_both_modes() {
        for persistent in [false, true] {
            let directory = Directory::new();
            let directories = directories(&directory);
            let logger = Logger::with_settings(&directories, 20).unwrap();
            let history = History::new(nianlai_store::hash::sha256, logger);
            if persistent {
                history.initialize();
            }
            let shared = &history;
            let now = Instant::now();
            let mut ids = Vec::new();
            for sequence in 1..=3u32 {
                let saved = shared
                    .ingest(
                        Content::text(&format!("整理内容{sequence}"), nianlai_store::hash::sha256)
                            .unwrap(),
                        CopyEvent {
                            time_ms: now_ms() + sequence as u64,
                            sequence,
                            source_process: "organize.exe".into(),
                            source_path: String::new(),
                            owner_process: "organize.exe".into(),
                            owner_pid: 1,
                            observed: now,
                        },
                        false,
                        now,
                    )
                    .unwrap();
                ids.push(saved.record_id.unwrap());
            }
            let (a, b, c) = (ids[0], ids[1], ids[2]);
            for id in [a, b, c] {
                shared.set_favorite(id, true).unwrap().unwrap();
            }
            let order = |shared: &History| {
                shared
                    .favorites()
                    .unwrap()
                    .into_iter()
                    .map(|item| item.id)
                    .collect::<Vec<_>>()
            };
            assert_eq!(order(shared), vec![a, b, c]);
            // 重排：返回新列表并持久生效。
            let reordered = shared.reorder_favorites(&[c, a, b]).unwrap();
            assert_eq!(
                reordered.iter().map(|i| i.id).collect::<Vec<_>>(),
                vec![c, a, b]
            );
            assert_eq!(order(shared), vec![c, a, b]);
            // 错误：重复、非收藏、缺项；不改变顺序，并给中文提示。
            for bad in [vec![c, c, b], vec![c, a, 9999], vec![c, a]] {
                let error = shared.reorder_favorites(&bad).err().unwrap();
                assert!(error.contains("常用"), "{error}");
                assert_eq!(order(shared), vec![c, a, b]);
            }
            // 标题备注：清理、上限、非收藏报错。
            let labeled = shared
                .set_favorite_label(a, "  发票\n清单 ", "\n备注 xyzzy\n第二行\n")
                .unwrap();
            assert_eq!(labeled.title, "发票 清单");
            assert_eq!(labeled.note, "备注 xyzzy\n第二行");
            let long = shared
                .set_favorite_label(b, &"t".repeat(300), &"n".repeat(3000))
                .unwrap();
            assert_eq!(long.title.chars().count(), 200);
            assert_eq!(long.note.chars().count(), 2000);
            shared.set_favorite_label(b, "", "").unwrap();
            shared.set_favorite(c, false).unwrap().unwrap();
            assert!(shared.set_favorite_label(c, "x", "y").is_err());
            assert!(order(shared).iter().all(|id| *id != c));
            // 列表项与收藏列表带标题备注；序列化时空串不输出。
            let favorites = shared.favorites().unwrap();
            let item = favorites.iter().find(|i| i.id == a).unwrap();
            assert_eq!(item.title, "发票 清单");
            let json = serde_json::to_value(&favorites[..]).unwrap();
            let plain = json
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["id"] == b)
                .unwrap();
            assert!(plain.get("title").is_none() && plain.get("note").is_none());
            // 搜索覆盖标题与备注。
            let found = |shared: &History, query: &str| {
                shared
                    .search(query, 1)
                    .unwrap()
                    .items
                    .into_iter()
                    .map(|i| i.id)
                    .collect::<Vec<_>>()
            };
            assert_eq!(found(shared, "XYZZY"), vec![a]);
            assert_eq!(found(shared, "发票"), vec![a]);
            // reuse_to_top：关闭时粘贴旧条目不挪位置，打开时挪到最前。
            let newest = |shared: &History| shared.snapshot().items[0].id;
            assert_eq!(newest(shared), c);
            set_reuse_to_top(false);
            shared.reuse(a);
            let kept = newest(shared);
            set_reuse_to_top(true);
            assert_eq!(kept, c);
            shared.reuse(a);
            assert_eq!(newest(shared), a);
        }
    }

    // 面板 Delete：撤销窗口内条目从快照和搜索里消失、可撤销；收藏拒删；再次复制取消挂起；退出前提交。
    #[test]
    fn delete_goes_to_trash_and_undo_restore_purge_work_in_stored_and_memory_modes() {
        for persistent in [false, true] {
            let directory = Directory::new();
            let directories = directories(&directory);
            let logger = Logger::with_settings(&directories, 20).unwrap();
            let history = History::new(nianlai_store::hash::sha256, logger);
            if persistent {
                history.initialize();
            }
            let shared = &history;
            let now = Instant::now();
            let save = |text: &str, sequence: u32| {
                shared
                    .ingest(
                        Content::text(text, nianlai_store::hash::sha256).unwrap(),
                        CopyEvent {
                            time_ms: now_ms(),
                            sequence,
                            source_process: "delete-test.exe".into(),
                            source_path: String::new(),
                            owner_process: "delete-test.exe".into(),
                            owner_pid: 1,
                            observed: now,
                        },
                        false,
                        now,
                    )
                    .unwrap()
                    .record_id
                    .unwrap()
            };
            let kept = save("删除注册表甲", 1);
            let removed = save("删除注册表乙", 2);
            let ids = |shared: &History| {
                shared
                    .snapshot()
                    .items
                    .iter()
                    .map(|item| item.id)
                    .collect::<Vec<_>>()
            };
            let third = save("删除注册表丙", 3);
            shared.set_favorite(kept, true).unwrap().unwrap();
            let in_trash = |shared: &History| {
                shared
                    .trash()
                    .unwrap()
                    .iter()
                    .map(|t| t.item.id)
                    .collect::<Vec<_>>()
            };
            // 收藏的也能删（界面确认），进回收站；列表、搜索、收藏都看不到。
            assert!(shared.delete(kept).unwrap());
            assert!(shared.delete(removed).unwrap());
            assert!(!shared.delete(removed).unwrap());
            assert_eq!(ids(shared), [third]);
            assert_eq!(shared.snapshot().total, 1);
            assert!(shared.favorites().unwrap().is_empty());
            assert!(
                shared
                    .search("删除注册表", 1)
                    .unwrap()
                    .items
                    .iter()
                    .all(|item| item.id == third)
            );
            assert_eq!(in_trash(shared), [removed, kept]);
            // Ctrl+Z 依次撤销最近的删除，与呼出无关；收藏恢复后仍是收藏。
            assert_eq!(shared.undo_delete().unwrap().unwrap().id, removed);
            let restored = shared.undo_delete().unwrap().unwrap();
            assert!(restored.id == kept && restored.favorite);
            assert!(shared.undo_delete().unwrap().is_none());
            assert_eq!(ids(shared), [third, removed, kept]);
            // 回收站里的内容再被复制：回到列表，撤销栈里的这条跳过。
            assert!(shared.delete(removed).unwrap());
            assert!(shared.delete(third).unwrap());
            assert_eq!(save("删除注册表乙", 4), removed);
            assert_eq!(in_trash(shared), [third]);
            assert_eq!(shared.undo_delete().unwrap().unwrap().id, third);
            assert!(shared.undo_delete().unwrap().is_none());
            // 从回收站恢复指定条目、物理删除指定条目、清空、过期清理。
            assert!(shared.delete(third).unwrap());
            assert!(shared.delete(removed).unwrap());
            assert_eq!(shared.restore(third).unwrap().unwrap().id, third);
            assert!(shared.restore(third).unwrap().is_none());
            assert_eq!(shared.purge(Some(vec![removed, third])).unwrap(), 1);
            assert!(shared.get(removed).unwrap().is_none());
            assert!(shared.delete(third).unwrap());
            shared.purge_expired();
            assert_eq!(in_trash(shared), [third], "未满保留期");
            assert_eq!(shared.purge(None).unwrap(), 1);
            assert!(in_trash(shared).is_empty());
            assert!(shared.undo_delete().unwrap().is_none());
            assert!(shared.get(kept).unwrap().is_some());
        }
    }

    #[test]
    fn full_text_comes_in_chunks_and_images_render_with_original_size() {
        let directory = Directory::new();
        let directories = directories(&directory);
        let logger = Logger::with_settings(&directories, 20).unwrap();
        let history = History::new(nianlai_store::hash::sha256, logger);
        history.initialize();
        let shared = &history;
        let now = Instant::now();
        let save = |content: Arc<Content>, sequence: u32| {
            shared
                .ingest(
                    content,
                    CopyEvent {
                        time_ms: now_ms(),
                        sequence,
                        source_process: "preview-test.exe".into(),
                        source_path: String::new(),
                        owner_process: "preview-test.exe".into(),
                        owner_pid: 1,
                        observed: now,
                    },
                    false,
                    now,
                )
                .unwrap()
                .record_id
                .unwrap()
        };
        // 3,750 字的长文（含 Emoji，按字符计）：完整预览先给前 2,000 字，再给全部。
        let long: String = "长文😀".repeat(1250);
        let text = save(
            Content::text(&long, nianlai_store::hash::sha256).unwrap(),
            1,
        );
        let first = shared.full_text(text, Some(2000)).unwrap().unwrap();
        assert_eq!((first.shown, first.total), (2000, 3750));
        assert_eq!(first.text.chars().count(), 2000);
        let all = shared.full_text(text, None).unwrap().unwrap();
        assert_eq!((all.shown, all.total), (3750, 3750));
        assert_eq!(all.text, long);
        assert!(
            shared.image(text, Some(100)).unwrap().is_none(),
            "文字不是图片"
        );

        // 300×100 的 24 位图：缩略图也带回原图尺寸。
        let mut dib = vec![0u8; 40];
        dib[..4].copy_from_slice(&40u32.to_le_bytes());
        dib[4..8].copy_from_slice(&300i32.to_le_bytes());
        dib[8..12].copy_from_slice(&100i32.to_le_bytes());
        dib[12..14].copy_from_slice(&1u16.to_le_bytes());
        dib[14..16].copy_from_slice(&24u16.to_le_bytes());
        dib.extend(std::iter::repeat_n(120u8, 900 * 100));
        let picture = save(
            Content::new(
                std::collections::BTreeMap::from([(
                    content::DIB.to_owned(),
                    content::Data::Bytes(dib.into()),
                )]),
                nianlai_store::hash::sha256,
            )
            .unwrap(),
            2,
        );
        let thumb = shared.image(picture, Some(120)).unwrap().unwrap();
        assert_eq!((thumb.width, thumb.height), (300, 100));
        assert!(thumb.url.starts_with("data:image/png;base64,"));
        let original = shared.image(picture, None).unwrap().unwrap();
        assert!(original.url.len() > thumb.url.len(), "原尺寸比缩略图大");
        assert!(
            shared.full_text(picture, Some(10)).unwrap().is_none(),
            "图片没有文字"
        );
        assert!(shared.image(9999, None).unwrap().is_none());
    }

    #[test]
    fn missing_files_are_marked_cleaned_into_trash_and_skipped_when_pasting() {
        let directory = Directory::new();
        let directories = directories(&directory);
        let logger = Logger::with_settings(&directories, 20).unwrap();
        let history = History::new(nianlai_store::hash::sha256, logger);
        history.initialize();
        let shared = &history;
        let files = directory.0.join("files");
        fs::create_dir_all(&files).unwrap();
        let (kept, gone) = (files.join("在.txt"), files.join("不在.txt"));
        fs::write(&kept, "a").unwrap();
        fs::write(&gone, "b").unwrap();
        let now = Instant::now();
        let save = |paths: Vec<String>, sequence: u32| {
            let content = Content::new(
                std::collections::BTreeMap::from([(
                    content::FILES.to_owned(),
                    content::Data::Files(paths.into()),
                )]),
                nianlai_store::hash::sha256,
            )
            .unwrap();
            shared
                .ingest(
                    content,
                    CopyEvent {
                        time_ms: now_ms(),
                        sequence,
                        source_process: "explorer.exe".into(),
                        source_path: String::new(),
                        owner_process: "explorer.exe".into(),
                        owner_pid: 1,
                        observed: now,
                    },
                    false,
                    now,
                )
                .unwrap()
                .record_id
                .unwrap()
        };
        let path = |p: &std::path::Path| p.to_string_lossy().into_owned();
        let both = save(vec![path(&kept), path(&gone)], 1);
        let only_gone = save(vec![path(&gone)], 2);
        assert!(!shared.check_files(None, "test"), "文件都在，没有变化");
        fs::remove_file(&gone).unwrap();
        assert!(shared.check_files(None, "test"));
        let state = |id| {
            shared
                .snapshot()
                .items
                .into_iter()
                .find(|item| item.id == id)
                .and_then(|item| item.file_state)
                .map(|mark| mark.state)
        };
        assert_eq!(state(both), Some("partial"));
        assert_eq!(state(only_gone), Some("missing"));
        let overview = shared.storage_overview().unwrap();
        assert_eq!((overview.files.missing, overview.files.partial), (1, 1));
        assert!(overview.files.checked_ms.is_some());

        // 粘贴：部分不在只留还在的，全部不在就不粘贴。
        match history.usable_files(&shared.get(both).unwrap().unwrap()) {
            files::Usable::Partial(filtered, skipped) => {
                assert_eq!(skipped, 1);
                assert!(matches!(&filtered.formats[content::FILES].data,
                    content::Data::Files(paths) if paths.to_vec() == [path(&kept)]));
            }
            _ => panic!("应该只粘贴还在的文件"),
        }
        assert!(matches!(
            history.usable_files(&shared.get(only_gone).unwrap().unwrap()),
            files::Usable::Nothing
        ));

        // 清理只清全部不在的，进回收站后不再算失效；恢复回来下次检查再标。
        assert_eq!(shared.clean_missing().unwrap(), 1);
        assert_eq!(state(both), Some("partial"));
        let overview = shared.storage_overview().unwrap();
        assert_eq!((overview.files.missing, overview.trashed.count), (0, 1));
        shared.restore(only_gone).unwrap().unwrap();
        assert!(shared.check_files(None, "test"));
        assert_eq!(state(only_gone), Some("missing"));
        let paths = shared.file_paths(both).unwrap().unwrap();
        assert_eq!(
            paths.iter().map(|p| p.state).collect::<Vec<_>>(),
            [files::PathState::Present, files::PathState::Missing]
        );
        assert!(shared.file_paths(9999).unwrap().is_none());
    }
}

#[cfg(test)]
mod deferred_tests {
    use super::*;
    use crate::settings::{Directories, tests::Directory};
    use std::sync::atomic::AtomicUsize;

    const WINDOW: Duration = Duration::from_millis(200);

    fn history(directory: &Directory) -> History {
        let directories = Directories {
            settings_file: directory.0.join("settings.json"),
            data: directory.0.join("data"),
            logs: directory.0.join("logs"),
            data_source: "设置文件指定",
            test_data_dir: false,
        };
        let logger = Logger::with_settings(&directories, 20).unwrap();
        let history = History::new(nianlai_store::hash::sha256, logger);
        history.initialize();
        assert!(history.store().is_some());
        history
    }

    fn capture(history: &History, text: &str, sequence: u32, window: Option<Duration>) -> Captured {
        let now = Instant::now();
        History::capture_with(
            history,
            Content::text(text, nianlai_store::hash::sha256).unwrap(),
            CopyEvent {
                time_ms: now_ms() + sequence as u64,
                sequence,
                source_process: "expander.exe".into(),
                source_path: String::new(),
                owner_process: "expander.exe".into(),
                owner_pid: 1,
                observed: now,
            },
            false,
            now,
            window,
        )
        .unwrap()
    }

    fn previews(history: &History) -> Vec<(String, usize)> {
        history
            .snapshot()
            .items
            .into_iter()
            .map(|item| (item.preview, item.copy_count))
            .collect()
    }

    #[test]
    fn restored_clipboard_drops_the_borrowed_content_without_writing_it() {
        let directory = Directory::new();
        let history = history(&directory);
        assert!(matches!(
            capture(&history, "原来的内容", 1, None),
            Captured::Stored(_)
        ));
        assert!(matches!(
            capture(&history, "展开的地址-7f3a", 2, Some(WINDOW)),
            Captured::Deferred
        ));
        assert!(matches!(
            capture(&history, "原来的内容", 3, Some(WINDOW)),
            Captured::Transient
        ));
        std::thread::sleep(WINDOW * 2);
        history.flush_deferred();
        assert_eq!(previews(&history), [("原来的内容".to_owned(), 1)]);
        assert!(history.search("展开的地址", 1).unwrap().items.is_empty());
    }

    #[test]
    fn content_that_stays_is_written_after_the_window() {
        let directory = Directory::new();
        let history = history(&directory);
        let notified = Arc::new(AtomicUsize::new(0));
        let counter = notified.clone();
        history.set_changed(Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        }));
        assert!(matches!(
            capture(&history, "留下的内容", 1, Some(WINDOW)),
            Captured::Deferred
        ));
        assert!(previews(&history).is_empty());
        std::thread::sleep(WINDOW * 3);
        assert_eq!(previews(&history), [("留下的内容".to_owned(), 1)]);
        assert_eq!(notified.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_new_copy_or_flush_writes_the_pending_one_in_order() {
        let directory = Directory::new();
        let history = history(&directory);
        capture(&history, "第一条", 1, Some(WINDOW));
        capture(&history, "第二条", 2, Some(WINDOW));
        // 新复制到来时先写入上一条；呼出面板时立刻写入暂存的这条。
        assert_eq!(previews(&history), [("第一条".to_owned(), 1)]);
        history.flush_deferred();
        assert_eq!(
            previews(&history),
            [("第二条".to_owned(), 1), ("第一条".to_owned(), 1)]
        );
        // 计时线程到期时已经没有暂存内容，不会重复写入。
        std::thread::sleep(WINDOW * 2);
        assert_eq!(previews(&history).len(), 2);
    }

    #[test]
    fn copying_the_same_content_again_is_not_transient() {
        let directory = Directory::new();
        let history = history(&directory);
        capture(&history, "A", 1, None);
        capture(&history, "B", 2, Some(WINDOW));
        assert!(matches!(
            capture(&history, "B", 3, Some(WINDOW)),
            Captured::Deferred
        ));
        history.flush_deferred();
        assert_eq!(
            previews(&history),
            [("B".to_owned(), 2), ("A".to_owned(), 1)]
        );
    }
}
