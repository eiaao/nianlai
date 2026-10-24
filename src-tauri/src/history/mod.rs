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
    time::Instant,
};

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
}
impl From<Metadata> for Preview {
    fn from(item: Metadata) -> Self {
        Self {
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
        }
    }
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

pub struct History {
    application_guard: Mutex<Option<std::fs::File>>,
    directory: OnceLock<PathBuf>,
    store: RwLock<Option<Store>>,
    disabled: AtomicBool,
    memory: Mutex<memory::History>,
    status: Mutex<Option<&'static str>>,
    recording: AtomicBool,
    last_write: Mutex<Option<Instant>>,
    hash: Hash,
    logger: Logger,
}
impl History {
    pub fn new(hash: Hash, logger: Logger) -> Self {
        let mut memory = memory::History::new(hash);
        memory.isolate_fallback_ids();
        Self {
            application_guard: Mutex::new(None),
            directory: OnceLock::new(),
            store: RwLock::new(None),
            disabled: AtomicBool::new(false),
            memory: Mutex::new(memory),
            status: Mutex::new(Some("正在加载历史")),
            recording: AtomicBool::new(false),
            last_write: Mutex::new(None),
            hash,
            logger,
        }
    }
    fn store(&self) -> Option<Store> {
        if self.disabled.load(Ordering::Acquire) {
            None
        } else {
            self.store.read().unwrap().clone()
        }
    }
    pub fn initialize(&self) {
        let specified = std::env::var_os("NIANLAI_G0_DATA_DIR");
        let location = if specified.is_some() {
            "环境变量指定"
        } else {
            "默认位置"
        };
        let directory = specified.map(PathBuf::from).or_else(|| {
            std::env::var_os("LOCALAPPDATA").map(|base| PathBuf::from(base).join("Nianlai/g0/data"))
        });
        if let Some(path) = &directory {
            let _ = self.directory.set(path.clone());
        }
        let opened = directory
            .ok_or(nianlai_store::Error::Invalid("data_directory_missing"))
            .and_then(|path| {
                *self.application_guard.lock().unwrap() =
                    Some(nianlai_store::application_guard(&path)?);
                Store::open(&path, self.hash)
            });
        match opened {
            Ok(store) => {
                let mut report = serde_json::to_value(store.report()).unwrap();
                report["location"] = json!(location);
                self.logger.event("store_opened", report);
                *self.status.lock().unwrap() = None;
                *self.store.write().unwrap() = Some(store);
            }
            Err(error) => {
                *self.status.lock().unwrap() =
                    Some(if matches!(error, nianlai_store::Error::NewerVersion(_)) {
                        "数据版本较新，未能打开"
                    } else {
                        "历史库异常，未落盘"
                    });
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
    pub fn maintenance(self: &Arc<Self>, changed: Arc<dyn Fn() + Send + Sync>) {
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
                *history.status.lock().unwrap()=Some("历史库异常，未落盘");
                    history.disabled.store(true, Ordering::Release);
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
        }).is_err() {self.logger.event("store_check_failed",json!({"error_code":"thread_start"}));}
    }
    // 图片降级成路径时 PNG 存在这里；库没打开（内存模式）也照样用数据目录。
    pub fn exports(&self) -> Option<PathBuf> {
        self.directory
            .get()
            .map(|directory| directory.join("exports"))
    }
    pub fn snapshot(&self) -> Snapshot {
        let mut snapshot = if let Some(store) = self.store() {
            let cache = store.cache();
            Snapshot {
                items: cache.items.into_iter().map(Preview::from).collect(),
                total: cache.total,
                recording: false,
                last_recorded_ms: cache.last_recorded_ms,
                store_status: None,
            }
        } else {
            self.memory.lock().unwrap().snapshot()
        };
        snapshot.recording = self.recording.load(Ordering::Acquire);
        snapshot.store_status = *self.status.lock().unwrap();
        snapshot
    }
    pub fn search(&self, query: &str, request_id: u64) -> Result<SearchReply, String> {
        let started = Instant::now();
        let (result, mode) = if let Some(store) = self.store() {
            match store.reader().and_then(|r| r.search(query)) {
                Ok(result) => (
                    Ok(result.items.into_iter().map(Preview::from).collect()),
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
        result.map(|items| SearchReply { request_id, items })
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
    pub fn set_recording(&self, value: bool) {
        self.recording.store(value, Ordering::Release);
    }
    pub fn last_write(&self) -> Option<Instant> {
        *self.last_write.lock().unwrap()
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
