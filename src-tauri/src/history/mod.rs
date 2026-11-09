pub mod content;
pub mod files;
mod memory;
pub mod snapshots;
pub mod storage;

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
    // 本次运行里删除过的条目，最近的在最后；Ctrl+Z 跨呼出依次恢复。
    undo_stack: Mutex<Vec<usize>>,
    deferred: Mutex<Option<Deferred>>,
    generation: AtomicU64,
    // 最近一次出现在剪贴板上的内容（含我们自己写回的）。
    last_seen: Mutex<Option<Fingerprint>>,
    changed: Mutex<Option<Changed>>,
    files: files::Marks,
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
            undo_stack: Mutex::new(Vec::new()),
            deferred: Mutex::new(None),
            generation: AtomicU64::new(0),
            last_seen: Mutex::new(None),
            changed: Mutex::new(None),
            files: files::Marks::default(),
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
    // 采集走 capture()；测试直接写入。
    #[cfg(test)]
    pub fn ingest(
        &self,
        content: Arc<Content>,
        event: CopyEvent,
        partial: bool,
        now: Instant,
    ) -> Result<Ingest, String> {
        self.0.ingest(content, event, partial, now)
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
        Shared::capture(&self.0, content, event, partial, now, window)
    }
    /// 立刻写入暂存的内容：呼出面板前、退出前调用，保证面板里能看到刚复制的内容。
    pub fn flush_deferred(&self) {
        self.0.flush_deferred();
    }
    /// 暂存内容写入后用来通知面板刷新。
    pub fn set_changed(&self, changed: Changed) {
        *self.0.changed.lock().unwrap() = Some(changed);
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
            // 启动时先检查一次，之后每小时检查是否到了每日快照的时间（程序常常连开好几天）。
            loop {
                let Some(directory)=history.store_directory() else {return};
                snapshots::automatic(&directory, &logger);
                history.0.purge_expired();
                if history.0.check_files_daily() {
                    history.0.notify_changed();
                }
                std::thread::sleep(snapshots::CHECK_INTERVAL);
            }
        }).is_err() {self.0.logger.event("store_check_failed",json!({"error_code":"thread_start"}));}
    }
    /// 库已打开时的数据目录；内存模式（库没打开）返回 None。
    pub fn store_directory(&self) -> Option<PathBuf> {
        self.0.store_directory()
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
    pub fn store_directory(&self) -> Option<PathBuf> {
        self.store().map(|store| store.directory().to_path_buf())
    }
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
        // 上次登记的快照恢复要在打开库之前完成。
        snapshots::apply_pending(&directory, &self.logger);
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
            file_state: None,
        }
    }
    /// 给列表项带上文件失效结论（两种模式共用，内存模式的列表项不经过 preview）。
    fn annotate(&self, items: &mut [Preview]) {
        for item in items {
            item.file_state = self.files.get(item.id);
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
        if let Some(store) = self.store() {
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
        }
    }
    fn capture(
        this: &Arc<Self>,
        content: Arc<Content>,
        event: CopyEvent,
        partial: bool,
        now: Instant,
        window: Option<Duration>,
    ) -> Result<Captured, String> {
        let fingerprint = content.fingerprint;
        // 整个过程持锁：暂存内容的写入和新内容的处理按复制顺序进行。
        let mut deferred = this.deferred.lock().unwrap();
        if let Some(pending) = deferred.take() {
            if pending.previous == Some(fingerprint) && pending.content.fingerprint != fingerprint {
                // 剪贴板变回了借用前的内容：暂存的那条丢弃，还原回来的也不算一次新复制。
                *this.last_seen.lock().unwrap() = Some(fingerprint);
                this.logger.event(
                    "capture_transient",
                    json!({"held_ms":pending.event.observed.elapsed().as_secs_f64()*1000.0,"kind":pending.content.kind}),
                );
                return Ok(Captured::Transient);
            }
            this.commit_deferred(pending);
        }
        let previous = this.last_seen.lock().unwrap().replace(fingerprint);
        let Some(window) = window else {
            drop(deferred);
            return this
                .ingest(content, event, partial, now)
                .map(Captured::Stored);
        };
        let generation = this.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let shared = this.clone();
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
            return this
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
            *self.last_seen.lock().unwrap() = Some(written);
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
    /// 面板 Delete：移进回收站（B12）。收藏的也可以，确认由界面负责（B13）。
    pub fn delete(&self, id: usize) -> Result<bool, String> {
        let result = if let Some(store) = self.store() {
            store.delete(id).map_err(|e| e.to_string())
        } else {
            Ok(self.memory.lock().unwrap().delete(id, now_ms()))
        };
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
            json!({"trashed":result.as_ref().ok(),"error_code":result.as_ref().err()}),
        );
        result
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
        if let Some(store) = self.store() {
            store
                .restore(id)
                .map(|item| item.map(|item| self.preview(item)))
                .map_err(|e| e.to_string())
        } else {
            Ok(self.memory.lock().unwrap().restore(id))
        }
    }
    pub fn trash(&self) -> Result<Vec<Trashed>, String> {
        let items = if let Some(store) = self.store() {
            store
                .reader()
                .and_then(|r| r.trash())
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|(item, deleted_ms)| (self.preview(item), deleted_ms))
                .collect()
        } else {
            self.memory.lock().unwrap().trash()
        };
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
        let result = if let Some(store) = self.store() {
            store
                .purge(ids.clone(), before_ms)
                .map_err(|e| e.to_string())
        } else {
            Ok(self.memory.lock().unwrap().purge(ids.as_deref(), before_ms))
        };
        if result.as_ref().is_ok_and(|count| *count > 0) || result.is_err() {
            self.logger.event(
                "trash_purged",
                json!({"reason":reason,"count":result.as_ref().ok(),"error_code":result.as_ref().err()}),
            );
        }
        result
    }
    pub fn favorites(&self) -> Result<Vec<Preview>, String> {
        let mut items: Vec<Preview> = if let Some(store) = self.store() {
            store
                .reader()
                .and_then(|r| r.favorites())
                .map(|items| items.into_iter().map(|item| self.preview(item)).collect())
                .map_err(|e| e.to_string())?
        } else {
            self.memory.lock().unwrap().favorites()
        };
        self.annotate(&mut items);
        Ok(items)
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
    fn delete_goes_to_trash_and_undo_restore_purge_work_in_stored_and_memory_modes() {
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
            let third = save("删除注册表丙", 3);
            shared.set_favorite(kept, true).unwrap().unwrap();
            let in_trash = |shared: &Arc<Shared>| {
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
    fn missing_files_are_marked_cleaned_into_trash_and_skipped_when_pasting() {
        let directory = Directory::new();
        let directories = directories(&directory);
        let logger = Logger::with_settings(&directories, 20).unwrap();
        let history = History::new(nianlai_store::hash::sha256, logger);
        history.initialize();
        let shared = &history.0;
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
        assert!(history.0.store().is_some());
        history
    }

    fn capture(history: &History, text: &str, sequence: u32, window: Option<Duration>) -> Captured {
        let now = Instant::now();
        Shared::capture(
            &history.0,
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
