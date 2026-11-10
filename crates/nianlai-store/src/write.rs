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
                    let mut connection = writable(&db)?;
                    let migrated_from = schema::initialize(&mut connection, &path)?;
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
                        total: counts.contents - counts.trashed,
                        last_recorded_ms,
                        latest_sequence: None,
                    };
                    let report = OpenReport {
                        version: VERSION,
                        migrated_from,
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
    // 收藏 / 取消收藏（E01）。收藏放最后，顺序一经分配永不改变；取消后再收藏会排到最后。
    pub fn set_favorite(&self, id: usize, favorite: bool) -> Result<Option<Metadata>> {
        self.call(move |w| {
            let tx = w.connection.transaction()?;
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM contents WHERE id=?1)",
                [id as i64],
                |r| r.get(0),
            )?;
            if !exists {
                return Ok(None);
            }
            if favorite {
                let now = now_ms() as i64;
                tx.execute(
                    "INSERT INTO favorites(content_id,position,title,note,created_ms,updated_ms) \
                     VALUES(?1,(SELECT coalesce(max(position),0)+1 FROM favorites),'','',?2,?2) \
                     ON CONFLICT(content_id) DO NOTHING",
                    params![id as i64, now],
                )?;
            } else {
                tx.execute("DELETE FROM favorites WHERE content_id=?1", [id as i64])?;
            }
            let item = schema::metadata(&tx, id)?;
            tx.commit()?;
            update_cached_item(&mut w.cache.write().unwrap(), &item);
            Ok(Some(item))
        })
    }
    // 重排收藏（E03）：ids 是当前未在回收站的全部收藏的新顺序。
    // 只在这些收藏现有的 position 槽位里置换，回收站里的收藏保持原位。
    pub fn reorder_favorites(&self, ids: &[usize]) -> Result<Vec<Metadata>> {
        let ids = ids.to_vec();
        self.call(move |w| {
            let tx = w.connection.transaction()?;
            let live: Vec<(i64, i64)> = tx
                .prepare(
                    "SELECT fv.content_id,fv.position FROM favorites fv \
                     WHERE NOT EXISTS(SELECT 1 FROM trash t WHERE t.content_id=fv.content_id) \
                     ORDER BY fv.position",
                )?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<std::result::Result<_, _>>()?;
            let mut seen = std::collections::BTreeSet::new();
            if !ids.iter().all(|id| seen.insert(*id)) {
                return Err(Error::Invalid("reorder_duplicate"));
            }
            let mut expected: Vec<i64> = live.iter().map(|(id, _)| *id).collect();
            expected.sort_unstable();
            let given: Vec<i64> = seen.iter().map(|id| *id as i64).collect();
            if expected != given {
                return Err(Error::Invalid("reorder_mismatch"));
            }
            // position 有 UNIQUE 约束：先全部挪到负数，再写最终值。
            for (id, position) in &live {
                tx.execute(
                    "UPDATE favorites SET position=?2 WHERE content_id=?1",
                    params![id, -position],
                )?;
            }
            for (id, (_, position)) in ids.iter().zip(&live) {
                tx.execute(
                    "UPDATE favorites SET position=?2 WHERE content_id=?1",
                    params![*id as i64, position],
                )?;
            }
            let items = schema::favorites(&tx)?;
            tx.commit()?;
            let mut cache = w.cache.write().unwrap();
            for item in &items {
                update_cached_item(&mut cache, item);
            }
            Ok(items)
        })
    }
    // 修改收藏的标题与备注（E04）；不是收藏则报错。
    pub fn set_favorite_label(&self, id: usize, title: &str, note: &str) -> Result<Metadata> {
        let title = clean_title(title);
        let note = clean_note(note);
        self.call(move |w| {
            let tx = w.connection.transaction()?;
            if tx.execute(
                "UPDATE favorites SET title=?2,note=?3,updated_ms=?4 WHERE content_id=?1",
                params![id as i64, title, note, now_ms() as i64],
            )? == 0
            {
                return Err(Error::Invalid("not_favorite"));
            }
            let item = schema::metadata(&tx, id)?;
            tx.commit()?;
            update_cached_item(&mut w.cache.write().unwrap(), &item);
            Ok(item)
        })
    }
    // 删除一条历史（PRD 6.7 Delete）：放进回收站（B12），不物理删除；收藏的内容也可以（B13 由界面确认）。
    // 返回 false 表示内容不存在或已在回收站。
    pub fn delete(&self, id: usize) -> Result<bool> {
        self.call(move |w| {
            let tx = w.connection.transaction()?;
            let moved = tx.execute(
                "INSERT INTO trash(content_id,deleted_ms) SELECT id,?2 FROM contents WHERE id=?1 \
                 ON CONFLICT(content_id) DO NOTHING",
                params![id as i64, now_ms() as i64],
            )? > 0;
            tx.commit()?;
            if moved {
                if w.last.as_ref().is_some_and(|(last, _)| *last == id) {
                    w.last = None;
                }
                let mut cache = w.cache.write().unwrap();
                cache.items.retain(|item| item.id != id);
                cache.total = cache.total.saturating_sub(1);
            }
            Ok(moved)
        })
    }
    // 垃圾清理（M2-3）：一次把多条移进回收站，收藏的、已在回收站或不存在的跳过。返回实际移进去的。
    pub fn trash_many(&self, ids: Vec<usize>) -> Result<Vec<usize>> {
        self.call(move |w| {
            let tx = w.connection.transaction()?;
            let mut moved = Vec::new();
            let now = now_ms() as i64;
            for id in ids {
                if tx.execute(
                    "INSERT INTO trash(content_id,deleted_ms) SELECT c.id,?2 FROM contents c \
                     WHERE c.id=?1 AND NOT EXISTS(SELECT 1 FROM favorites WHERE content_id=c.id) \
                     ON CONFLICT(content_id) DO NOTHING",
                    params![id as i64, now],
                )? > 0
                {
                    moved.push(id);
                }
            }
            tx.commit()?;
            if !moved.is_empty() {
                if w.last
                    .as_ref()
                    .is_some_and(|(last, _)| moved.contains(last))
                {
                    w.last = None;
                }
                let mut cache = w.cache.write().unwrap();
                cache.items.retain(|item| !moved.contains(&item.id));
                cache.total = cache.total.saturating_sub(moved.len());
            }
            Ok(moved)
        })
    }
    // 压缩（M2-3）：把删除后空出来的页还给磁盘。VACUUM 期间写入线程忙，采集排队等它结束。
    pub fn compact(&self) -> Result<()> {
        self.call(|w| {
            w.connection
                .execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")?;
            Ok(())
        })
    }
    // 从回收站恢复：回到原来的位置（排序时间不变）。不在回收站里返回 None。
    pub fn restore(&self, id: usize) -> Result<Option<Metadata>> {
        self.call(move |w| {
            let tx = w.connection.transaction()?;
            if tx.execute("DELETE FROM trash WHERE content_id=?1", [id as i64])? == 0 {
                return Ok(None);
            }
            let item = schema::metadata(&tx, id)?;
            tx.commit()?;
            update_cache(&mut w.cache.write().unwrap(), item.clone(), &[], 1);
            Ok(Some(item))
        })
    }
    // 物理删除回收站里的内容：`ids` 为 None 时按删除时间早于 `before_ms` 的全部清掉。
    // 收藏行先删（外键不级联）；formats、copy_events、trash 随内容级联，搜索索引由触发器同步。
    pub fn purge(&self, ids: Option<Vec<usize>>, before_ms: u64) -> Result<usize> {
        self.call(move |w| {
            let tx = w.connection.transaction()?;
            let targets: Vec<i64> = match ids {
                Some(ids) => {
                    let mut found = Vec::new();
                    for id in ids {
                        if schema::trashed(&tx, id)? {
                            found.push(id as i64);
                        }
                    }
                    found
                }
                None => tx
                    .prepare("SELECT content_id FROM trash WHERE deleted_ms<?1")?
                    .query_map([before_ms.min(i64::MAX as u64) as i64], |r| r.get(0))?
                    .collect::<std::result::Result<_, _>>()?,
            };
            for id in &targets {
                tx.execute("DELETE FROM favorites WHERE content_id=?1", [id])?;
                tx.execute("DELETE FROM contents WHERE id=?1", [id])?;
            }
            tx.commit()?;
            if w.last
                .as_ref()
                .is_some_and(|(last, _)| targets.contains(&(*last as i64)))
            {
                w.last = None;
            }
            Ok(targets.len())
        })
    }
    // 合并备份（M2-4）：把已检查通过的临时目录里的备份并进现役库，一个事务，随后整体重载缓存。
    pub fn import_merge(&self, token: &str) -> Result<MergeReport> {
        let staging =
            backup::staging(&self.0.directory, token).ok_or(Error::Invalid("import_missing"))?;
        let icons = self.0.directory.join("icons");
        self.call(move |w| {
            std::fs::create_dir_all(&icons)?;
            let (report, sort_order) = backup::merge_into(
                &mut w.connection,
                &staging.join("history.db"),
                &staging.join("icons"),
                &icons,
                w.sort_order,
            )?;
            w.sort_order = sort_order;
            if report.added_items > 0 || report.added_favorites > 0 {
                let counts = schema::counts(&w.connection)?;
                let items = schema::recent(&w.connection)?;
                let last_recorded_ms = w
                    .connection
                    .query_row("SELECT max(time_ms) FROM copy_events", [], |r| {
                        r.get::<_, Option<i64>>(0)
                    })?
                    .map(|v| v as u64);
                let mut cache = w.cache.write().unwrap();
                cache.items = items;
                cache.total = counts.contents - counts.trashed;
                cache.last_recorded_ms = last_recorded_ms;
                drop(cache);
                w.last = None;
            }
            Ok(report)
        })
    }
    // 后台刷新来源应用（A06）：更新显示名和图标缓存引用。图标文件由调用方先写好。
    pub fn refresh_source_app(
        &self,
        app_id: i64,
        display_name: String,
        icon_key: String,
        icon_file: String,
    ) -> Result<()> {
        self.call(move |w| {
            let tx = w.connection.transaction()?;
            tx.execute(
                "UPDATE source_apps SET display_name=?2,icon_key=?3,icon_file=?4,updated_ms=?5 \
                 WHERE id=?1",
                params![app_id, display_name, icon_key, icon_file, now_ms() as i64],
            )?;
            tx.commit()?;
            // 来源应用信息变了，最近列表缓存整体重载（图标刷新不频繁，重载开销可忽略）。
            w.cache.write().unwrap().items = schema::recent(&w.connection)?;
            Ok(())
        })
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
            + item.source_name.as_deref().map_or(0, str::len)
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

const TITLE_LIMIT: usize = 200;
const NOTE_LIMIT: usize = 2000;
// 标题：换行折成空格、去首尾空白，最多 200 个字符。
pub fn clean_title(title: &str) -> String {
    let folded: String = title
        .chars()
        .map(|c| if matches!(c, '\r' | '\n') { ' ' } else { c })
        .collect();
    folded.trim().chars().take(TITLE_LIMIT).collect()
}
// 备注：保留内部换行、去首尾空白，最多 2000 个字符。
pub fn clean_note(note: &str) -> String {
    note.trim().chars().take(NOTE_LIMIT).collect()
}

// 收藏状态变化只改缓存里这一项的字段，顺序和其余条目不动（愉悦标准：可预期）。
fn update_cached_item(cache: &mut Cache, item: &Metadata) {
    if let Some(cached) = cache.items.iter_mut().find(|cached| cached.id == item.id) {
        cached.favorite = item.favorite;
        cached.favorite_position = item.favorite_position;
        cached.title.clone_from(&item.title);
        cached.note.clone_from(&item.note);
    }
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
        // 来源应用行只做一次点查，找不到才插入；版本信息和图标由采集线程之外的后台刷新补上。
        let source_app_id = upsert_source_app(&tx, &event)?;
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
            let mut keep = existing;
            // 回收站里的行不计入列表总数：合并前活着几行，合并后只剩一行活的。
            let mut live_before = usize::from(!schema::trashed(&tx, existing)?);
            outcome = if same {
                "merged_same_observation"
            } else {
                "merged_late_formats"
            };
            if let Some(other) = duplicate.filter(|other| *other != existing) {
                // 补齐后与另一条内容同指纹，必须并成一行；保留被收藏的那行（B03），收藏随保留行迁移。
                let keeper = if schema::deletable(&tx, existing)? && !schema::deletable(&tx, other)?
                {
                    other
                } else {
                    existing
                };
                let dropped = if keeper == existing { other } else { existing };
                live_before += usize::from(!schema::trashed(&tx, other)?);
                tx.execute(
                    "UPDATE copy_events SET content_id=?1 WHERE content_id=?2",
                    params![keeper as i64, dropped as i64],
                )?;
                // 两行都被收藏时二合一：position 撞车的收藏行（保留行先占了位置）直接并入。
                tx.execute(
                    "UPDATE OR IGNORE favorites SET content_id=?1 WHERE content_id=?2",
                    params![keeper as i64, dropped as i64],
                )?;
                tx.execute(
                    "DELETE FROM favorites WHERE content_id=?1",
                    [dropped as i64],
                )?;
                tx.execute("DELETE FROM contents WHERE id=?1", [dropped as i64])?;
                deleted.push(dropped);
                keep = keeper;
            }
            tx.execute("DELETE FROM trash WHERE content_id=?1", [keep as i64])?;
            delta += 1 - live_before as isize;
            let text = search_text(&merged)?;
            tx.execute("UPDATE contents SET fingerprint=?2,kind=?3,preview=?4,file_names=?5,bytes=?6,partial=?7,sort_ms=?8,search_text=?9 WHERE id=?1",
                params![keep as i64,merged.fingerprint.as_slice(),merged.kind,merged.preview,serde_json::to_string(&merged.file_names).unwrap(),merged.bytes as i64,partial,event.time_ms as i64,text])?;
            tx.execute("DELETE FROM formats WHERE content_id=?1", [keep as i64])?;
            insert_formats(&tx, keep, &merged)?;
            id = keep;
        } else if let Some(existing) = duplicate {
            id = existing;
            outcome = "duplicate";
            // 回收站里的内容又被复制：它又是活的，回到列表。
            if tx.execute("DELETE FROM trash WHERE content_id=?1", [id as i64])? > 0 {
                delta += 1;
            }
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
            tx.execute("INSERT INTO copy_events(content_id,time_ms,source_process,owner_process,owner_pid,sequence,source_app_id) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![id as i64,event.time_ms as i64,event.source_process,event.owner_process,event.owner_pid,event.sequence,(source_app_id != 0).then_some(source_app_id)])?;
            Some(tx.last_insert_rowid())
        } else {
            None
        };
        tx.execute("UPDATE contents SET copy_count=(SELECT count(*) FROM copy_events WHERE content_id=?1),source=(SELECT source_process FROM copy_events WHERE content_id=?1 ORDER BY time_ms DESC,id DESC LIMIT 1),source_app_id=(SELECT source_app_id FROM copy_events WHERE content_id=?1 ORDER BY time_ms DESC,id DESC LIMIT 1) WHERE id=?1",[id as i64])?;
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

// 来源应用显示名的退回值（A06）：有路径用不带扩展名的文件名，否则用进程名。
// FileDescription 由后台刷新覆盖，这里只保证列表立即有东西可显示。
fn fallback_display_name(process_name: &str, exe_path: &str) -> String {
    if !exe_path.is_empty()
        && let Some(stem) = std::path::Path::new(exe_path)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .filter(|stem| !stem.is_empty())
    {
        return stem;
    }
    process_name.to_owned()
}

// 事件落库时找到（或建出）来源应用行；进程名拿不到就不建（v1 以来 source_process 可为空）。
fn upsert_source_app(tx: &Connection, event: &CopyEvent) -> Result<i64> {
    if event.source_process.is_empty() {
        return Ok(0);
    }
    let existing: Option<i64> = tx
        .query_row(
            "SELECT id FROM source_apps WHERE process_name=?1 AND exe_path=?2",
            params![event.source_process, event.source_path],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return Ok(id);
    }
    tx.execute(
        "INSERT INTO source_apps(process_name,exe_path,display_name,icon_file,icon_key,updated_ms) \
         VALUES(?1,?2,?3,'','',?4)",
        params![
            event.source_process,
            event.source_path,
            fallback_display_name(&event.source_process, &event.source_path),
            event.time_ms as i64
        ],
    )?;
    Ok(tx.last_insert_rowid())
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
