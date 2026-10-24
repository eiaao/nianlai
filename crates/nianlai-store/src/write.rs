use crate::{
    content::{Content, Data, Fingerprint, Hash, TEXT},
    *,
};
use rusqlite::{OptionalExtension, params};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    path::PathBuf,
    sync::{Arc, Mutex, RwLock, mpsc},
    thread::{self, JoinHandle},
    time::Duration,
};

type Job = Box<dyn FnOnce(&mut Writer) + Send>;
struct Inner {
    sender: mpsc::Sender<Option<Job>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    cache: Arc<RwLock<Cache>>,
    stats: Arc<Mutex<Stats>>,
    directory: PathBuf,
    hash: Hash,
    report: OpenReport,
}
#[derive(Clone)]
pub struct Store(Arc<Inner>);
#[derive(Default)]
struct Stats {
    writes: usize,
    failures: usize,
    micros: BTreeMap<u64, usize>,
}
struct Writer {
    connection: Connection,
    hash: Hash,
    cache: Arc<RwLock<Cache>>,
    stats: Arc<Mutex<Stats>>,
    last: Option<(usize, CopyEvent)>,
    echoes: Vec<(Fingerprint, Instant)>,
    fail_after_formats: bool,
    sort_order: i64,
    _lock: File,
}

pub(crate) fn lock(directory: &Path) -> Result<File> {
    lock_named(directory, "history.lock", false)
}
pub(crate) fn lock_named(directory: &Path, name: &str, shared: bool) -> Result<File> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(name))?;
    if shared {
        lock.lock_shared()?;
    } else {
        lock.try_lock().map_err(|_| Error::Busy)?;
    }
    Ok(lock)
}

impl Store {
    pub fn open(directory: &Path, hash: Hash) -> Result<Self> {
        let directory = directory.to_path_buf();
        let path = directory.clone();
        let cache = Arc::new(RwLock::new(Cache::default()));
        let shared = cache.clone();
        let stats = Arc::new(Mutex::new(Stats::default()));
        let shared_stats = stats.clone();
        let (sender, receiver) = mpsc::channel::<Option<Job>>();
        let (ready, opened) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("history-writer".into())
            .spawn(move || {
                let initialized = (|| -> Result<(Writer, OpenReport)> {
                    let started = Instant::now();
                    let db = path.join("history.db");
                    // 先只读探测版本；拒绝新版时不设置 journal_mode，不触发迁移。
                    if db.exists() {
                        let probe =
                            Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
                        schema::version(&probe)?;
                    }
                    std::fs::create_dir_all(&path)?;
                    let lock = lock(&path)?;
                    let connection = writable(&db)?;
                    schema::initialize(&connection)?;
                    let counts = schema::counts(&connection)?;
                    let sort_order = connection.query_row(
                        "SELECT coalesce(max(sort_order),0) FROM contents",
                        [],
                        |r| r.get(0),
                    )?;
                    let open_ms = started.elapsed().as_secs_f64() * 1000.0;
                    let load = Instant::now();
                    let items = schema::recent(&connection)?;
                    let last_recorded_ms = connection
                        .query_row("SELECT max(time_ms) FROM copy_events", [], |r| {
                            r.get::<_, Option<i64>>(0)
                        })?
                        .map(|v| v as u64);
                    *shared.write().unwrap() = Cache {
                        items,
                        total: counts.contents,
                        last_recorded_ms,
                        latest_sequence: None,
                    };
                    let report = OpenReport {
                        version: VERSION,
                        open_ms,
                        load_ms: load.elapsed().as_secs_f64() * 1000.0,
                        counts,
                    };
                    Ok((
                        Writer {
                            connection,
                            hash,
                            cache: shared,
                            stats: shared_stats,
                            last: None,
                            echoes: Vec::new(),
                            fail_after_formats: false,
                            sort_order,
                            _lock: lock,
                        },
                        report,
                    ))
                })();
                match initialized {
                    Ok((mut writer, report)) => {
                        if ready.send(Ok(report)).is_err() {
                            return;
                        }
                        while let Ok(Some(job)) = receiver.recv() {
                            job(&mut writer);
                        }
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error));
                    }
                }
            })?;
        let report = match opened.recv().map_err(|_| Error::Closed).and_then(|r| r) {
            Ok(report) => report,
            Err(error) => {
                let _ = thread.join();
                return Err(error);
            }
        };
        Ok(Self(Arc::new(Inner {
            sender,
            thread: Mutex::new(Some(thread)),
            cache,
            stats,
            directory,
            hash,
            report,
        })))
    }
    fn call<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Writer) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (reply, result) = mpsc::sync_channel(1);
        self.0
            .sender
            .send(Some(Box::new(move |writer| {
                let _ = reply.send(job(writer));
            })))
            .map_err(|_| Error::Closed)?;
        result.recv().map_err(|_| Error::Closed)?
    }
    pub fn latest_sequence(&self) -> Option<u32> {
        self.0.cache.read().unwrap().latest_sequence
    }
    pub fn cache(&self) -> Cache {
        self.0.cache.read().unwrap().clone()
    }
    pub fn report(&self) -> &OpenReport {
        &self.0.report
    }
    pub fn directory(&self) -> &Path {
        &self.0.directory
    }
    pub fn reader(&self) -> Result<Reader> {
        Reader::open(&self.0.directory, self.0.hash)
    }
    pub fn ingest(
        &self,
        content: Arc<Content>,
        event: CopyEvent,
        partial: bool,
        now: Instant,
    ) -> Result<Ingest> {
        self.call(move |w| w.ingest(content, event, partial, now))
    }
    pub fn seed(&self, content: Arc<Content>, event: CopyEvent) -> Result<Ingest> {
        self.call(move |w| {
            let last = w.last.take();
            let sequence = w.cache.write().unwrap().latest_sequence.take();
            let result = w.ingest(content, event, false, Instant::now());
            w.last = last;
            w.cache.write().unwrap().latest_sequence = sequence;
            result
        })
    }
    pub fn skipped_sequence(&self, sequence: u32) -> Result<()> {
        self.call(move |w| {
            let mut cache = w.cache.write().unwrap();
            if cache
                .latest_sequence
                .is_none_or(|latest| !older(sequence, latest))
            {
                if cache.latest_sequence != Some(sequence) {
                    w.last = None;
                }
                cache.latest_sequence = Some(sequence);
            }
            Ok(())
        })
    }
    // 校验的是记录本身的指纹；登记回声的是实际写进剪贴板的内容，降级成文字时两者不同。
    pub fn before_write(
        &self,
        id: usize,
        fingerprint: Fingerprint,
        written: Fingerprint,
        now: Instant,
    ) -> Result<bool> {
        self.call(move |w| {
            let exists = w.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM contents WHERE id=?1 AND fingerprint=?2)",
                params![id as i64, fingerprint.as_slice()],
                |r| r.get(0),
            )?;
            if exists {
                w.echoes
                    .retain(|(_, at)| now.saturating_duration_since(*at) < Duration::from_secs(5));
                w.echoes.push((written, now));
            }
            Ok(exists)
        })
    }
    pub fn reuse(&self, id: usize, time_ms: u64) -> Result<()> {
        self.call(move |w| {
            let started = Instant::now();
            let result = (|| -> Result<()> {
                let tx = w.connection.transaction()?;
                if tx.execute(
                    "UPDATE contents SET sort_ms=?2,sort_order=?3 WHERE id=?1",
                    params![id as i64, time_ms as i64, w.sort_order + 1],
                )? == 0
                {
                    return Err(Error::Invalid("record_missing"));
                }
                let item = schema::metadata(&tx, id)?;
                tx.commit()?;
                w.sort_order += 1;
                update_cache(&mut w.cache.write().unwrap(), item, &[], 0);
                Ok(())
            })();
            let mut stats = w.stats.lock().unwrap();
            if result.is_ok() {
                stats.writes += 1;
                *stats
                    .micros
                    .entry(started.elapsed().as_micros() as u64)
                    .or_default() += 1;
            } else {
                stats.failures += 1;
            }
            result
        })
    }
    pub fn stats(&self) -> WriteStats {
        let stats = self.0.stats.lock().unwrap();
        let threshold = (stats.writes * 95).div_ceil(100);
        let mut seen = 0;
        let p95 = stats
            .micros
            .iter()
            .find_map(|(us, count)| {
                seen += count;
                (seen >= threshold).then_some(*us as f64 / 1000.0)
            })
            .unwrap_or(0.0);
        WriteStats {
            writes: stats.writes,
            failures: stats.failures,
            p95_ms: p95,
        }
    }
    #[cfg(test)]
    pub(crate) fn inject_after_formats(&self) -> Result<()> {
        self.call(|w| {
            w.fail_after_formats = true;
            Ok(())
        })
    }
}
impl Drop for Inner {
    fn drop(&mut self) {
        let _ = self.sender.send(None);
        if let Some(thread) = self.thread.lock().unwrap().take() {
            let _ = thread.join();
        }
    }
}

fn update_cache(cache: &mut Cache, item: Metadata, deleted: &[usize], delta: isize) {
    cache.total = cache.total.saturating_add_signed(delta);
    cache
        .items
        .retain(|old| old.id != item.id && !deleted.contains(&old.id));
    cache.items.push(item);
    cache
        .items
        .sort_by_key(|item| std::cmp::Reverse((item.copied_ms, item.sort_order)));
    cache.items.truncate(CACHE_LIMIT);
    // 只计算元数据的占用，原始图片和文件列表 BLOB 不进入缓存。
    let mut bytes = 0usize;
    cache.items.retain(|item| {
        bytes += item.preview.len()
            + item.source.len()
            + item.file_names.iter().map(String::len).sum::<usize>()
            + item
                .formats
                .iter()
                .map(|f| f.name.len() + 64)
                .sum::<usize>()
            + 256;
        bytes <= 256 * 1024 * 1024
    });
}

impl Writer {
    fn ingest(
        &mut self,
        content: Arc<Content>,
        event: CopyEvent,
        partial: bool,
        now: Instant,
    ) -> Result<Ingest> {
        let started = Instant::now();
        let result = self.transaction(content, event, partial, now);
        let mut stats = self.stats.lock().unwrap();
        match result {
            Ok(mut result) => {
                result.write_ms = started.elapsed().as_secs_f64() * 1000.0;
                if result.transaction_ok {
                    stats.writes += 1;
                    *stats
                        .micros
                        .entry(started.elapsed().as_micros() as u64)
                        .or_default() += 1;
                }
                Ok(result)
            }
            Err(error) => {
                stats.failures += 1;
                Err(error)
            }
        }
    }
    fn transaction(
        &mut self,
        content: Arc<Content>,
        event: CopyEvent,
        partial: bool,
        now: Instant,
    ) -> Result<Ingest> {
        if self
            .cache
            .read()
            .unwrap()
            .latest_sequence
            .is_some_and(|latest| older(event.sequence, latest))
        {
            return Ok(Ingest::skipped("stale", None));
        }
        if self.echoes.iter().any(|(fingerprint, at)| {
            *fingerprint == content.fingerprint
                && now.saturating_duration_since(*at) < Duration::from_secs(5)
        }) {
            self.cache.write().unwrap().latest_sequence = Some(event.sequence);
            self.last = None;
            return Ok(Ingest::skipped("skipped", Some("own_echo")));
        }
        if content.formats.is_empty() {
            return Err(Error::Invalid("empty_content"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut same = false;
        let mut merged = content.clone();
        let mut partial = partial;
        let mut merge_id = None;
        if let Some((id, previous)) = &self.last {
            same = previous.sequence == event.sequence;
            let candidate = event.owner_pid != 0
                && event.owner_pid == previous.owner_pid
                && event.owner_process == previous.owner_process
                && event.observed >= previous.observed
                && event.observed.duration_since(previous.observed) <= Duration::from_secs(1);
            if (same || candidate)
                && let Some(old) = schema::content(&tx, *id, self.hash)?
            {
                let late = !same && content.expanded_from(&old);
                if same || late {
                    partial |= content.formats.iter().any(|(name, new)| {
                        old.formats
                            .get(name)
                            .is_some_and(|old| old.data != new.data)
                    });
                    merged = old
                        .merge(&content, self.hash)
                        .map_err(|_| Error::Invalid("hash_failed"))?;
                    merge_id = Some(*id);
                }
            }
        }
        let duplicate: Option<usize> = tx
            .query_row(
                "SELECT id FROM contents WHERE fingerprint=?1",
                [merged.fingerprint.as_slice()],
                |r| schema::number(r, 0),
            )
            .optional()?;
        let mut deleted = Vec::new();
        let outcome;
        let id;
        let mut delta = 0;
        if let Some(existing) = merge_id {
            id = existing;
            outcome = if same {
                "merged_same_observation"
            } else {
                "merged_late_formats"
            };
            if let Some(other) = duplicate.filter(|other| *other != id) {
                tx.execute(
                    "UPDATE copy_events SET content_id=?1 WHERE content_id=?2",
                    params![id as i64, other as i64],
                )?;
                tx.execute("DELETE FROM contents WHERE id=?1", [other as i64])?;
                deleted.push(other);
                delta -= 1;
            }
            let text = search_text(&merged)?;
            tx.execute("UPDATE contents SET fingerprint=?2,kind=?3,preview=?4,file_names=?5,bytes=?6,partial=?7,sort_ms=?8,search_text=?9 WHERE id=?1",
                params![id as i64,merged.fingerprint.as_slice(),merged.kind,merged.preview,serde_json::to_string(&merged.file_names).unwrap(),merged.bytes as i64,partial,event.time_ms as i64,text])?;
            tx.execute("DELETE FROM formats WHERE content_id=?1", [id as i64])?;
            insert_formats(&tx, id, &merged)?;
        } else if let Some(existing) = duplicate {
            id = existing;
            outcome = "duplicate";
            tx.execute(
                "UPDATE contents SET partial=?2,sort_ms=?3 WHERE id=?1",
                params![id as i64, partial, event.time_ms as i64],
            )?;
        } else {
            outcome = "saved";
            delta += 1;
            tx.execute("INSERT INTO contents(fingerprint,kind,preview,file_names,bytes,partial,sort_ms,source,copy_count,search_text) VALUES(?1,?2,?3,?4,?5,?6,?7,'',0,?8)",
                params![merged.fingerprint.as_slice(),merged.kind,merged.preview,serde_json::to_string(&merged.file_names).unwrap(),merged.bytes as i64,partial,event.time_ms as i64,search_text(&merged)?])?;
            id = tx.last_insert_rowid() as usize;
            insert_formats(&tx, id, &merged)?;
        }
        tx.execute(
            "UPDATE contents SET sort_order=?2 WHERE id=?1",
            params![id as i64, self.sort_order + 1],
        )?;
        if std::mem::take(&mut self.fail_after_formats) {
            return Err(Error::Invalid("injected_after_formats"));
        }
        let event_id = if merge_id.is_none() {
            tx.execute("INSERT INTO copy_events(content_id,time_ms,source_process,owner_process,owner_pid,sequence) VALUES(?1,?2,?3,?4,?5,?6)",
                params![id as i64,event.time_ms as i64,event.source_process,event.owner_process,event.owner_pid,event.sequence])?;
            Some(tx.last_insert_rowid())
        } else {
            None
        };
        tx.execute("UPDATE contents SET copy_count=(SELECT count(*) FROM copy_events WHERE content_id=?1),source=(SELECT source_process FROM copy_events WHERE content_id=?1 ORDER BY time_ms DESC,id DESC LIMIT 1) WHERE id=?1",[id as i64])?;
        let item = schema::metadata(&tx, id)?;
        // 合并删掉缓存内条目时，补回第 301 条，避免最近列表出现缺口。
        let refill = if deleted.is_empty() {
            None
        } else {
            Some(schema::recent(&tx)?)
        };
        tx.commit()?;
        self.sort_order += 1;
        let result = Ingest {
            outcome,
            reason: None,
            record_id: Some(id),
            event_id,
            kind: Some(item.kind.clone()),
            copy_count: Some(item.copy_count),
            write_ms: 0.0,
            transaction_ok: true,
        };
        let mut cache = self.cache.write().unwrap();
        update_cache(&mut cache, item, &deleted, delta);
        if let Some(items) = refill {
            cache.items = items;
        }
        cache.last_recorded_ms = Some(event.time_ms);
        cache.latest_sequence = Some(event.sequence);
        self.last = Some((id, event));
        Ok(result)
    }
}

fn search_text(content: &Content) -> Result<String> {
    if content
        .raw(TEXT)
        .is_some_and(|bytes| bytes.len() > 2 * 1024 * 1024)
    {
        return Err(Error::Invalid("text_too_large"));
    }
    let mut text = content.text_value().unwrap_or_default();
    for name in &content.file_names {
        text.push('\n');
        text.push_str(name);
    }
    Ok(text.to_ascii_lowercase())
}
fn insert_formats(tx: &Connection, id: usize, content: &Content) -> Result<()> {
    let mut statement=tx.prepare_cached("INSERT INTO formats(content_id,name,fingerprint,bytes,encoding,data) VALUES(?1,?2,?3,?4,?5,?6)")?;
    for (name, format) in &content.formats {
        let encoded;
        let (encoding, bytes) = match &format.data {
            Data::Bytes(bytes) => (0, bytes.as_ref()),
            Data::Files(paths) => {
                encoded = serde_json::to_vec(paths.as_ref()).unwrap();
                (1, encoded.as_slice())
            }
        };
        statement.execute(params![
            id as i64,
            name,
            format.fingerprint.as_slice(),
            format.data.size() as i64,
            encoding,
            bytes
        ])?;
    }
    Ok(())
}
