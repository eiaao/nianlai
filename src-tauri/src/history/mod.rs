pub mod content;
mod memory;

use crate::log::{Logger, now_ms};
use content::{Content, Fingerprint, Hash};
pub use nianlai_store::{CopyEvent, Ingest, older};
use nianlai_store::{Metadata, Store};
use serde::Serialize;
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// 面板里按 Delete 后可撤销的时长。期间条目从列表和搜索里拿掉，到期才真正删除。
pub const UNDO_WINDOW: Duration = Duration::from_secs(4);

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
static ACTIVE: Mutex<Vec<std::sync::Weak<Shared>>> = Mutex::new(Vec::new());
pub fn active() -> Option<Arc<Shared>> {
    let mut entries = ACTIVE.lock().unwrap();
    entries.retain(|weak| weak.upgrade().is_some());
    entries.last().and_then(|weak| weak.upgrade())
}

// 全部可变状态聚在 Shared 里，命令层（active()）与采集线程（Arc<History>）访问同一份。
pub struct Shared {
    application_guard: Mutex<Option<std::fs::File>>,
    directory: OnceLock<PathBuf>,
    store: RwLock<Option<Store>>,
    disabled: AtomicBool,
    memory: Mutex<memory::History>,
    status: Mutex<Option<&'static str>>,
    recording: AtomicBool,
    last_write: Mutex<Option<Instant>>,
    refresh: Mutex<Option<crate::platform::apps::Refresher>>,
    // 待删除（撤销窗口内）的那一条；同一时间只挂一条，新的删除会先提交上一条。
    pending_delete: Mutex<Option<(usize, Instant)>>,
    hash: Hash,
    logger: Logger,
}

pub struct History(Arc<Shared>);
impl Clone for History {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl History {
    pub fn new(hash: Hash, logger: Logger) -> Self {
        let mut memory = memory::History::new(hash);
        memory.isolate_fallback_ids();
        let shared = Arc::new(Shared {
            application_guard: Mutex::new(None),
            directory: OnceLock::new(),
            store: RwLock::new(None),
            disabled: AtomicBool::new(false),
            memory: Mutex::new(memory),
            status: Mutex::new(Some("正在加载历史")),
            recording: AtomicBool::new(false),
            last_write: Mutex::new(None),
            refresh: Mutex::new(None),
            pending_delete: Mutex::new(None),
            hash,
            logger,
        });
        ACTIVE.lock().unwrap().push(Arc::downgrade(&shared));
        Self(shared)
    }
    pub fn initialize(&self) {
        self.0.initialize(&self.0.clone());
    }
    pub fn snapshot(&self) -> Snapshot {
        self.0.snapshot()
    }
    pub fn search(&self, query: &str, request_id: u64) -> Result<SearchReply, String> {
        self.0.search(query, request_id)
    }
    pub fn ingest(
        &self,
        content: Arc<Content>,
        event: CopyEvent,
        partial: bool,
        now: Instant,
    ) -> Result<Ingest, String> {
        self.0.ingest(content, event, partial, now)
    }
    pub fn get(&self, id: usize) -> Result<Option<Arc<Content>>, String> {
        self.0.get(id)
    }
    pub fn before_write(
        &self,
        id: usize,
        fingerprint: Fingerprint,
        written: Fingerprint,
        now: Instant,
    ) -> bool {
        self.0.before_write(id, fingerprint, written, now)
    }
    pub fn reuse(&self, id: usize) {
        self.0.reuse(id);
    }
    pub fn latest_sequence(&self) -> Option<u32> {
        self.0.latest_sequence()
    }
    pub fn skipped_sequence(&self, sequence: u32) {
        self.0.skipped_sequence(sequence);
    }
    pub fn set_recording(&self, value: bool) {
        self.0.recording.store(value, Ordering::Release);
    }
    pub fn last_write(&self) -> Option<Instant> {
        *self.0.last_write.lock().unwrap()
    }
    pub fn summary(&self) {
        self.0.summary();
    }
    pub fn flush_delete(&self) {
        self.0.flush_delete();
    }
    pub fn maintenance(self: &Arc<Self>, changed: Arc<dyn Fn() + Send + Sync>) {
        let Some(store) = self.0.store() else {
            return;
        };
        let history = Arc::new(Self(self.0.clone()));
        let logger = self.0.logger.clone();
        if std::thread::Builder::new().name("history-check".into()).spawn(move || {
            let started=Instant::now();
            let result=store.reader().and_then(|r|r.quick_check());
            logger.event("store_check",json!({"success":result.is_ok(),"elapsed_ms":started.elapsed().as_secs_f64()*1000.0,"error_code":result.as_ref().err().map(ToString::to_string)}));
            if result.is_err() {
                *history.0.status.lock().unwrap()=Some("历史库异常，未落盘");
                    history.0.disabled.store(true, Ordering::Release);
                changed();return;
            }
            let result=nianlai_store::snapshot_due(store.directory()).and_then(|due| {
                if due {nianlai_store::snapshot(store.directory()).map(Some)}else{Ok(None)}
            });
            match result {
                Ok(Some(report))=>logger.event("snapshot",json!({"elapsed_ms":report.elapsed_ms,"bytes":report.bytes,"retained":report.retained,"deleted":report.deleted})),
                Ok(None)=>{},
                Err(error)=>logger.event("snapshot_failed",json!({"error_code":error.to_string()})),
            }
        }).is_err() {self.0.logger.event("store_check_failed",json!({"error_code":"thread_start"}));}
    }
    // 图片降级成路径时 PNG 存在这里；库没打开（内存模式）也照样用数据目录。
    pub fn exports(&self) -> Option<PathBuf> {
        self.0.exports_path()
    }
    // 采集线程在内容保存成功后调用：把来源 exe 交给后台刷新显示名和图标（A06）。
    pub fn refresh_source_app(&self, exe_path: &str) {
        if let Some(refresher) = self.0.refresh.lock().unwrap().as_ref() {
            refresher.refresh(exe_path);
        }
    }
}

impl Shared {
    pub(crate) fn store(&self) -> Option<Store> {
        if self.disabled.load(Ordering::Acquire) {
            None
        } else {
            self.store.read().unwrap().clone()
        }
    }
    pub fn initialize(&self, this: &Arc<Shared>) {
        let location = self.logger.directories.data_source;
        let directory = self.logger.directories.data.clone();
        let _ = self.directory.set(directory.clone());
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
                // 来源应用的显示名与图标在采集线程之外刷新（A06）：门槛 P95 ≤ 500 ms 不受影响。
                let refresher = crate::platform::apps::start(this.clone(), self.logger.clone());
                *self.refresh.lock().unwrap() = Some(refresher);
                *self.store.write().unwrap() = Some(store);
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
            let result = if let Some(store) = self.store() {
                crate::samples::all()
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
                    })
            } else {
                self.memory.lock().unwrap().seed_samples()
            };
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
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        let mut snapshot = if let Some(store) = self.store() {
            let cache = store.cache();
            Snapshot {
                items: cache
                    .items
                    .into_iter()
                    .map(|item| self.preview(item))
                    .collect(),
                total: cache.total,
                recording: false,
                last_recorded_ms: cache.last_recorded_ms,
                store_status: None,
            }
        } else {
            self.memory.lock().unwrap().snapshot()
        };
        if let Some((id, _)) = *self.pending_delete.lock().unwrap() {
            let before = snapshot.items.len();
            snapshot.items.retain(|item| item.id != id);
            if snapshot.items.len() < before {
                snapshot.total = snapshot.total.saturating_sub(1);
            }
        }
        snapshot.recording = self.recording.load(Ordering::Acquire);
        snapshot.store_status = *self.status.lock().unwrap();
        snapshot
    }
    pub fn search(&self, query: &str, request_id: u64) -> Result<SearchReply, String> {
        let started = Instant::now();
        let (result, mode) = if let Some(store) = self.store() {
            match store.reader().and_then(|r| r.search(query)) {
                Ok(result) => (
                    Ok(result
                        .items
                        .into_iter()
                        .map(|item| self.preview(item))
                        .collect()),
                    json!(result.mode),
                ),
                Err(error) => (Err(error.to_string()), json!("failed")),
            }
        } else {
            (
                Ok(self.memory.lock().unwrap().search(query)),
                json!("memory_substring"),
            )
        };
        self.logger.event("search",json!({"words":query.split_whitespace().count(),"characters":query.chars().count(),"mode":mode,
            "results":result.as_ref().map(Vec::len).ok(),"elapsed_ms":started.elapsed().as_secs_f64()*1000.0,"error_code":result.as_ref().err()}));
        let pending = self.pending_delete.lock().unwrap().map(|(id, _)| id);
        result.map(|mut items: Vec<Preview>| {
            items.retain(|item| Some(item.id) != pending);
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
        let result = if let Some(store) = self.store() {
            store
                .ingest(content, event, partial, now)
                .map_err(|e| e.to_string())
        } else {
            let event = memory::CopyEvent {
                time_ms: event.time_ms,
                sequence: event.sequence,
                source_process: event.source_process,
                source_path: event.source_path,
                owner_process: event.owner_process,
                owner_pid: event.owner_pid,
                observed: event.observed,
            };
            self.memory
                .lock()
                .unwrap()
                .ingest(content, event, partial, now)
                .map(|r| Ingest {
                    outcome: r.outcome,
                    reason: r.reason,
                    record_id: r.record_id,
                    event_id: None,
                    kind: r.kind.map(str::to_owned),
                    copy_count: r.copy_count,
                    write_ms: 0.0,
                    transaction_ok: false,
                })
        };
        // 撤销窗口内又复制了同样的内容：这条又是活的，取消挂起的删除。
        if let Ok(Ingest {
            record_id: Some(id),
            ..
        }) = &result
        {
            let mut pending = self.pending_delete.lock().unwrap();
            if pending.is_some_and(|(pending, _)| pending == *id) {
                *pending = None;
            }
        }
        result
    }
    // 仅在采集、搜索或粘贴工作线程调用；界面用 snapshot，绝不读 BLOB。
    pub fn get(&self, id: usize) -> Result<Option<Arc<Content>>, String> {
        if let Some(store) = self.store() {
            store
                .reader()
                .and_then(|r| r.content(id))
                .map_err(|e| e.to_string())
        } else {
            Ok(self.memory.lock().unwrap().get(id))
        }
    }
    pub fn before_write(
        &self,
        id: usize,
        fingerprint: Fingerprint,
        written: Fingerprint,
        now: Instant,
    ) -> bool {
        let found = if let Some(store) = self.store() {
            store
                .before_write(id, fingerprint, written, now)
                .unwrap_or(false)
        } else {
            self.memory
                .lock()
                .unwrap()
                .before_write(id, fingerprint, written, now)
        };
        if found {
            *self.last_write.lock().unwrap() = Some(now);
        }
        found
    }
    pub fn reuse(&self, id: usize) {
        if let Some(store) = self.store() {
            if let Err(error) = store.reuse(id, now_ms()) {
                self.logger.event(
                    "store_reuse_failed",
                    json!({"error_code":error.to_string()}),
                );
            }
        } else {
            self.memory.lock().unwrap().reuse(id);
        }
    }
    pub fn latest_sequence(&self) -> Option<u32> {
        if let Some(store) = self.store() {
            store.latest_sequence()
        } else {
            self.memory.lock().unwrap().latest_sequence()
        }
    }
    pub fn skipped_sequence(&self, sequence: u32) {
        if let Some(store) = self.store() {
            if let Err(error) = store.skipped_sequence(sequence) {
                self.logger
                    .event("store_skip_failed", json!({"error_code":error.to_string()}));
            }
        } else {
            self.memory.lock().unwrap().skipped_sequence(sequence);
        }
    }
    /// 面板 Delete：先挂起，UNDO_WINDOW 后才真正删除。收藏的内容不删（B03），返回 false。
    pub fn delete(self: &Arc<Self>, id: usize) -> Result<bool, String> {
        let deletable = if let Some(store) = self.store() {
            store
                .reader()
                .and_then(|r| r.deletable(id))
                .map_err(|e| e.to_string())?
        } else {
            self.memory
                .lock()
                .unwrap()
                .favorites()
                .iter()
                .all(|item| item.id != id)
        };
        if !deletable {
            return Ok(false);
        }
        let at = Instant::now();
        let previous = self.pending_delete.lock().unwrap().replace((id, at));
        if let Some((previous, _)) = previous.filter(|(previous, _)| *previous != id) {
            self.commit_delete(previous);
        }
        let this = self.clone();
        let spawned = std::thread::Builder::new()
            .name("history-delete".into())
            .spawn(move || {
                std::thread::sleep(UNDO_WINDOW);
                let due = {
                    let mut pending = this.pending_delete.lock().unwrap();
                    let due = *pending == Some((id, at));
                    if due {
                        *pending = None;
                    }
                    due
                };
                if due {
                    this.commit_delete(id);
                }
            });
        if spawned.is_err() {
            // 起不了计时线程就不给撤销机会，直接删，不能让条目一直挂着。
            *self.pending_delete.lock().unwrap() = None;
            self.commit_delete(id);
        }
        self.logger.event(
            "history_delete_pending",
            json!({"undo_ms":UNDO_WINDOW.as_millis() as u64}),
        );
        Ok(true)
    }
    /// 撤销窗口内恢复；已经提交或不是挂起的那条时返回 false。
    pub fn undo_delete(&self, id: usize) -> bool {
        let mut pending = self.pending_delete.lock().unwrap();
        let undone = pending.is_some_and(|(pending, _)| pending == id);
        if undone {
            *pending = None;
        }
        drop(pending);
        self.logger
            .event("history_delete_undo", json!({"success":undone}));
        undone
    }
    /// 退出前提交挂起的删除，撤销窗口不跨进程。
    pub fn flush_delete(&self) {
        let pending = self.pending_delete.lock().unwrap().take();
        if let Some((id, _)) = pending {
            self.commit_delete(id);
        }
    }
    fn commit_delete(&self, id: usize) {
        let result = if let Some(store) = self.store() {
            store.delete(id).map_err(|e| e.to_string())
        } else {
            Ok(self.memory.lock().unwrap().delete(id))
        };
        self.logger.event(
            "history_deleted",
            json!({"removed":result.as_ref().ok(),"error_code":result.as_ref().err()}),
        );
    }
    pub fn favorites(&self) -> Result<Vec<Preview>, String> {
        if let Some(store) = self.store() {
            store
                .reader()
                .and_then(|r| r.favorites())
                .map(|items| items.into_iter().map(|item| self.preview(item)).collect())
                .map_err(|e| e.to_string())
        } else {
            Ok(self.memory.lock().unwrap().favorites())
        }
    }
    pub fn set_favorite(&self, id: usize, favorite: bool) -> Result<Option<Preview>, String> {
        if let Some(store) = self.store() {
            store
                .set_favorite(id, favorite)
                .map(|item| item.map(|item| self.preview(item)))
                .map_err(|e| e.to_string())
        } else {
            Ok(self.memory.lock().unwrap().set_favorite(id, favorite))
        }
    }
    pub fn summary(&self) {
        let stats = self
            .store
            .read()
            .unwrap()
            .as_ref()
            .map(Store::stats)
            .unwrap_or_default();
        self.logger.event("store_summary", json!(stats));
    }
}

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
        assert_eq!(history.0.store().unwrap().directory(), directories.data);
        assert!(directories.data.join("history.db").is_file());
        assert_eq!(history.exports(), Some(directories.data.join("exports")));
        assert_eq!(history.0.icons(), Some(directories.data.join("icons")));
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
                assert!(history.0.store().is_some());
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
        assert!(history.0.store().is_none());
        assert_eq!(
            history.snapshot().store_status,
            Some("这个数据目录由更新版本的拈来创建，请升级后再打开")
        );
        assert_eq!(history.exports(), Some(directories.data.join("exports")));
        assert_eq!(fs::read(database_file).unwrap(), database);
        assert!(!directories.data.join("backups").exists());
    }

    // 命令层（lib.rs）通过注册表取到活动的历史服务，磁盘与内存模式行为一致（M1-S）。
    // 注册表按“最后注册”取实例，并行测试会互相插队，所以功能断言走自己的 Shared，
    // 注册表本身只断言拿得到某个活动实例。
    #[test]
    fn active_registry_serves_favorites_for_stored_and_memory_modes() {
        for persistent in [false, true] {
            let directory = Directory::new();
            let directories = directories(&directory);
            let logger = Logger::with_settings(&directories, 20).unwrap();
            let history = History::new(nianlai_store::hash::sha256, logger);
            if persistent {
                history.initialize();
            }
            assert!(active().is_some());
            let shared = &history.0;
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

    // 面板 Delete：撤销窗口内条目从快照和搜索里消失、可撤销；收藏拒删；再次复制取消挂起；退出前提交。
    #[test]
    fn pending_delete_hides_undoes_and_commits_for_stored_and_memory_modes() {
        for persistent in [false, true] {
            let directory = Directory::new();
            let directories = directories(&directory);
            let logger = Logger::with_settings(&directories, 20).unwrap();
            let history = History::new(nianlai_store::hash::sha256, logger);
            if persistent {
                history.initialize();
            }
            let shared = &history.0;
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
            let ids = |shared: &Arc<Shared>| {
                shared
                    .snapshot()
                    .items
                    .iter()
                    .map(|item| item.id)
                    .collect::<Vec<_>>()
            };
            shared.set_favorite(kept, true).unwrap().unwrap();
            assert!(!shared.delete(kept).unwrap(), "收藏的不删");
            assert!(shared.delete(removed).unwrap());
            assert!(!ids(shared).contains(&removed));
            assert_eq!(shared.snapshot().total, 1);
            assert!(
                shared
                    .search("删除注册表", 1)
                    .unwrap()
                    .items
                    .iter()
                    .all(|item| item.id != removed)
            );
            assert!(shared.undo_delete(removed));
            assert!(!shared.undo_delete(removed));
            assert!(ids(shared).contains(&removed));
            // 撤销窗口内再次复制同样内容：取消挂起，不会被到期提交删掉。
            assert!(shared.delete(removed).unwrap());
            assert_eq!(save("删除注册表乙", 3), removed);
            assert!(ids(shared).contains(&removed));
            shared.flush_delete();
            assert!(shared.get(removed).unwrap().is_some());
            // 退出前提交挂起的删除。
            assert!(shared.delete(removed).unwrap());
            shared.flush_delete();
            assert!(shared.get(removed).unwrap().is_none());
            assert!(!shared.undo_delete(removed));
            assert!(shared.get(kept).unwrap().is_some());
        }
    }
}
