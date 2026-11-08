use super::{Preview, Snapshot, content};

use content::{Content, Fingerprint, Hash};
use serde::Serialize;
use std::{
    collections::VecDeque,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

const MAX_RECORDS: usize = 300;
const MAX_BYTES: usize = 256 * 1024 * 1024;
const ECHO_LIFETIME: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct CopyEvent {
    pub time_ms: u64,
    pub sequence: u32,
    pub source_process: String,
    pub source_path: String,
    pub owner_process: String,
    pub owner_pid: u32,
    pub observed: Instant,
}

pub struct Record {
    pub id: usize,
    pub content: Arc<Content>,
    pub events: Vec<CopyEvent>,
    pub observation: CopyEvent,
    pub updated_ms: u64,
    pub partial: bool,
    // 收藏（E01）：顺序一经分配不再变化；被收藏的记录不参与容量淘汰（B03）。
    pub favorite: bool,
    pub favorite_position: Option<u64>,
    observed: bool,
}

#[derive(Serialize)]
pub struct Ingest {
    pub outcome: &'static str,
    pub reason: Option<&'static str>,
    pub record_id: Option<usize>,
    pub kind: Option<&'static str>,
    pub copy_count: Option<usize>,
}

impl Ingest {
    fn skipped(outcome: &'static str, reason: Option<&'static str>) -> Self {
        Self {
            outcome,
            reason,
            record_id: None,
            kind: None,
            copy_count: None,
        }
    }
}

pub struct History {
    records: VecDeque<Record>,
    hash: Hash,
    next_id: usize,
    latest_sequence: Option<u32>,
    last_observation_id: Option<usize>,
    echoes: Vec<(Fingerprint, Instant)>,
    pub last_write: Option<Instant>,
    pub recording: bool,
    last_recorded_ms: Option<u64>,
    max_records: usize,
    max_bytes: usize,
    next_favorite_position: u64,
    // 回收站（B12）：删除的记录连同删除时间移到这里；内存模式退出即丢，和历史本身一样。
    trash: Vec<(Record, u64)>,
}

// DWORD 序列号会回绕；半个序列空间内按有符号差判断新旧。
pub fn older(sequence: u32, latest: u32) -> bool {
    (sequence.wrapping_sub(latest) as i32) < 0
}

// 来源应用显示名的退回值：路径文件名去扩展名，无路径时用进程名。
// 内存模式不做后台刷新（图标与 FileDescription 需要磁盘库），显示到这一层为止。
fn source_name(event: &CopyEvent) -> String {
    if !event.source_path.is_empty()
        && let Some(stem) = Path::new(&event.source_path)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .filter(|stem| !stem.is_empty())
    {
        return stem;
    }
    event.source_process.clone()
}

impl History {
    pub fn isolate_fallback_ids(&mut self) {
        // 与磁盘记录分开编号，检查失败切换内存模式时，旧的界面请求不能误取新内容。
        self.next_id = (1u64 << 52).min(usize::MAX as u64 / 2) as usize;
    }
    pub fn new(hash: Hash) -> Self {
        Self {
            records: VecDeque::new(),
            hash,
            next_id: 1,
            latest_sequence: None,
            last_observation_id: None,
            echoes: Vec::new(),
            last_write: None,
            recording: false,
            last_recorded_ms: None,
            max_records: MAX_RECORDS,
            max_bytes: MAX_BYTES,
            next_favorite_position: 1,
            trash: Vec::new(),
        }
    }

    pub fn get(&self, id: usize) -> Option<Arc<Content>> {
        self.records
            .iter()
            .find(|record| record.id == id)
            .map(|record| record.content.clone())
    }

    pub fn before_write(
        &mut self,
        id: usize,
        fingerprint: Fingerprint,
        written: Fingerprint,
        now: Instant,
    ) -> bool {
        if self
            .get(id)
            .is_none_or(|content| content.fingerprint != fingerprint)
        {
            return false;
        }
        self.echoes
            .retain(|(_, at)| now.saturating_duration_since(*at) < ECHO_LIFETIME);
        self.echoes.push((written, now));
        self.last_write = Some(now);
        true
    }

    pub fn reuse(&mut self, id: usize) {
        if let Some(index) = self.records.iter().position(|record| record.id == id) {
            let record = self.records.remove(index).unwrap();
            self.records.push_front(record);
        }
    }

    pub fn search(&self, query: &str) -> Vec<Preview> {
        let words: Vec<_> = query
            .split_whitespace()
            .map(str::to_ascii_lowercase)
            .collect();
        let ids: Vec<_> = self
            .records
            .iter()
            .filter(|r| {
                let text = format!(
                    "{}\n{}",
                    r.content.text_value().unwrap_or_default(),
                    r.content.file_names.join("\n")
                )
                .to_ascii_lowercase();
                words.iter().all(|word| text.contains(word))
            })
            .map(|r| r.id)
            .collect();
        self.snapshot()
            .items
            .into_iter()
            .filter(|item| ids.contains(&item.id))
            .collect()
    }

    pub fn latest_sequence(&self) -> Option<u32> {
        self.latest_sequence
    }

    pub fn skipped_sequence(&mut self, sequence: u32) {
        if self
            .latest_sequence
            .is_none_or(|latest| !older(sequence, latest))
        {
            if self.latest_sequence != Some(sequence) {
                self.last_observation_id = None;
            }
            self.latest_sequence = Some(sequence);
        }
    }

    // E01：收藏 / 取消收藏；返回更新后的项（内容不存在为 None）。
    pub fn set_favorite(&mut self, id: usize, favorite: bool) -> Option<Preview> {
        let record = self.records.iter_mut().find(|record| record.id == id)?;
        if favorite && !record.favorite {
            record.favorite = true;
            record.favorite_position = Some(self.next_favorite_position);
            self.next_favorite_position += 1;
        } else if !favorite {
            record.favorite = false;
            record.favorite_position = None;
        }
        self.snapshot().items.into_iter().find(|item| item.id == id)
    }

    // 删除一条历史；收藏的不删（B03）。
    /// 删除：移进回收站。收藏的也可以（界面先确认），恢复后仍是收藏。
    pub fn delete(&mut self, id: usize, now_ms: u64) -> bool {
        let Some(index) = self.records.iter().position(|record| record.id == id) else {
            return false;
        };
        let record = self.records.remove(index).unwrap();
        self.trash.push((record, now_ms));
        if self.last_observation_id == Some(id) {
            self.last_observation_id = None;
        }
        true
    }

    /// 从回收站恢复，按原来的时间放回列表。
    pub fn restore(&mut self, id: usize) -> Option<Preview> {
        let index = self.trash.iter().position(|(record, _)| record.id == id)?;
        let (record, _) = self.trash.remove(index);
        let at = self
            .records
            .iter()
            .position(|other| other.updated_ms < record.updated_ms)
            .unwrap_or(self.records.len());
        let preview = preview(&record);
        self.records.insert(at, record);
        Some(preview)
    }

    /// 物理删除回收站里的记录：`ids` 为 None 时清掉删除时间早于 `before_ms` 的。
    pub fn purge(&mut self, ids: Option<&[usize]>, before_ms: u64) -> usize {
        let before = self.trash.len();
        self.trash.retain(|(record, at)| match ids {
            Some(ids) => !ids.contains(&record.id),
            None => *at >= before_ms,
        });
        before - self.trash.len()
    }

    pub fn trash(&self) -> Vec<(Preview, u64)> {
        let mut items: Vec<_> = self
            .trash
            .iter()
            .map(|(record, at)| (preview(record), *at))
            .collect();
        items.sort_by_key(|(item, at)| std::cmp::Reverse((*at, item.id)));
        items
    }

    // 收藏列表按收藏顺序返回（E01）。
    pub fn favorites(&self) -> Vec<Preview> {
        let mut items: Vec<Preview> = self
            .snapshot()
            .items
            .into_iter()
            .filter(|item| item.favorite)
            .collect();
        items.sort_by_key(|item| item.favorite_position);
        items
    }

    pub fn ingest(
        &mut self,
        content: Arc<Content>,
        event: CopyEvent,
        partial: bool,
        now: Instant,
    ) -> Result<Ingest, String> {
        if self
            .latest_sequence
            .is_some_and(|latest| older(event.sequence, latest))
        {
            return Ok(Ingest::skipped("stale", None));
        }
        self.latest_sequence = Some(event.sequence);
        if content.bytes > self.max_bytes {
            self.last_observation_id = None;
            return Ok(Ingest::skipped("skipped", Some("capacity")));
        }
        self.echoes
            .retain(|(_, at)| now.saturating_duration_since(*at) < ECHO_LIFETIME);
        if self
            .echoes
            .iter()
            .any(|(hash, _)| *hash == content.fingerprint)
        {
            self.last_observation_id = None;
            return Ok(Ingest::skipped("skipped", Some("own_echo")));
        }
        // 回收站里的内容又被复制：先放回列表，下面按重复内容处理。
        if let Some(index) = self
            .trash
            .iter()
            .position(|(record, _)| record.content.fingerprint == content.fingerprint)
        {
            let (record, _) = self.trash.remove(index);
            self.records.push_front(record);
        }
        let same = self
            .records
            .iter()
            .position(|record| record.observed && record.observation.sequence == event.sequence);
        let late = self.last_observation_id.and_then(|id| {
            self.records.iter().position(|record| {
                record.id == id
                    && event.owner_pid != 0
                    && record.observation.owner_pid == event.owner_pid
                    && record.observation.owner_process == event.owner_process
                    && event.observed >= record.observation.observed
                    && event.observed.duration_since(record.observation.observed)
                        <= Duration::from_secs(1)
                    && content.expanded_from(&record.content)
            })
        });
        let duplicate = self
            .records
            .iter()
            .position(|record| record.content.fingerprint == content.fingerprint);
        let outcome;
        let mut record = if let Some(index) = same.or(late) {
            // 合并时先完成哈希，再取出旧记录；计算失败不能丢掉原记录。
            let merged = self.records[index].content.merge(&content, self.hash)?;
            if merged.bytes > self.max_bytes {
                self.last_observation_id = None;
                return Ok(Ingest::skipped("skipped", Some("capacity")));
            }
            let conflicting = content.formats.iter().any(|(name, new)| {
                self.records[index]
                    .content
                    .formats
                    .get(name)
                    .is_some_and(|old| old.data != new.data)
            });
            let mut record = self.records.remove(index).unwrap();
            record.content = merged;
            record.partial = partial || conflicting;
            outcome = if same.is_some() {
                "merged_same_observation"
            } else {
                "merged_late_formats"
            };
            record
        } else if let Some(index) = duplicate {
            let mut record = self.records.remove(index).unwrap();
            record.events.push(event.clone());
            record.partial = partial;
            outcome = "duplicate";
            record
        } else {
            let id = self.next_id;
            self.next_id += 1;
            outcome = "saved";
            Record {
                id,
                content,
                events: vec![event.clone()],
                observation: event.clone(),
                updated_ms: event.time_ms,
                partial,
                favorite: false,
                favorite_position: None,
                observed: true,
            }
        };
        record.observation = event.clone();
        record.observed = true;
        record.updated_ms = event.time_ms;
        // 补齐后可能与另一条完整内容相同，仍只保留一份内容，复制事件全部归并；
        // 被收藏的那条不能被并掉（B03）。
        if same.is_some() || late.is_some() {
            while let Some(index) = self.records.iter().position(|other| {
                !other.favorite && other.content.fingerprint == record.content.fingerprint
            }) {
                record
                    .events
                    .extend(self.records.remove(index).unwrap().events);
            }
            record.events.sort_by_key(|event| event.time_ms);
        }
        let result = Ingest {
            outcome,
            reason: None,
            record_id: Some(record.id),
            kind: Some(record.content.kind),
            copy_count: Some(record.events.len()),
        };
        self.last_observation_id = Some(record.id);
        self.last_recorded_ms = Some(event.time_ms);
        self.records.push_front(record);
        self.evict();
        Ok(result)
    }

    // 容量淘汰只动未收藏的记录；收藏不随历史过期（B03/E01）。
    fn evict(&mut self) {
        let mut bytes: usize = self.records.iter().map(|record| record.content.bytes).sum();
        while self.records.len() > self.max_records || bytes > self.max_bytes {
            let Some(index) = self.records.iter().rposition(|record| !record.favorite) else {
                break;
            };
            let record = self.records.remove(index).unwrap();
            bytes -= record.content.bytes;
            if self.last_observation_id == Some(record.id) {
                self.last_observation_id = None;
            }
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            items: self.records.iter().take(300).map(preview).collect(),
            total: self.records.len(),
            recording: self.recording,
            last_recorded_ms: self.last_recorded_ms,
            store_status: None,
        }
    }

    pub fn seed_samples(&mut self) -> Result<(), String> {
        for sample in crate::samples::all().into_iter().rev() {
            let event = CopyEvent {
                time_ms: crate::log::now_ms(),
                sequence: 0,
                source_process: "示例".into(),
                source_path: String::new(),
                owner_process: "示例".into(),
                owner_pid: 0,
                observed: Instant::now(),
            };
            self.records.push_front(Record {
                id: self.next_id,
                content: Content::text(&sample.text, self.hash)?,
                events: vec![event.clone()],
                observation: event.clone(),
                updated_ms: event.time_ms,
                partial: false,
                favorite: false,
                favorite_position: None,
                observed: false,
            });
            self.next_id += 1;
        }
        Ok(())
    }
}

fn preview(record: &Record) -> Preview {
    let event = record.events.last().unwrap();
    Preview {
        id: record.id,
        kind: record.content.kind.into(),
        preview: record.content.preview.clone(),
        file_names: record.content.file_names.clone(),
        source: if event.source_process.is_empty() {
            "来源未知".into()
        } else {
            event.source_process.clone()
        },
        copied_ms: record.updated_ms,
        copy_count: record.events.len(),
        partial: record.partial,
        favorite: record.favorite,
        favorite_position: record.favorite_position,
        source_name: (!event.source_process.is_empty() || !event.source_path.is_empty())
            .then(|| source_name(event)),
        // 内存模式不做图标提取（无磁盘库可写缓存），图标字段留空。
        source_icon: None,
    }
}

#[cfg(all(test, windows))]
#[path = "tests.rs"]
mod tests;
