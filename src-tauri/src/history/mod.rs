pub mod content;

use content::{Content, Fingerprint, Hash};
use serde::Serialize;
use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

const MAX_RECORDS: usize = 1000;
const MAX_BYTES: usize = 256 * 1024 * 1024;
const ECHO_LIFETIME: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct CopyEvent {
    pub time_ms: u64,
    pub sequence: u32,
    pub source_process: String,
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
    observed: bool,
}

#[derive(Serialize)]
pub struct Preview {
    id: usize,
    kind: &'static str,
    preview: String,
    file_names: Vec<String>,
    source: String,
    copied_ms: u64,
    copy_count: usize,
    partial: bool,
}

#[derive(Serialize)]
pub struct Snapshot {
    items: Vec<Preview>,
    total: usize,
    recording: bool,
    last_recorded_ms: Option<u64>,
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
}

// DWORD 序列号会回绕；半个序列空间内按有符号差判断新旧。
pub fn older(sequence: u32, latest: u32) -> bool {
    (sequence.wrapping_sub(latest) as i32) < 0
}

impl History {
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
        }
    }

    pub fn get(&self, id: usize) -> Option<Arc<Content>> {
        self.records
            .iter()
            .find(|record| record.id == id)
            .map(|record| record.content.clone())
    }

    pub fn before_write(&mut self, id: usize, fingerprint: Fingerprint, now: Instant) -> bool {
        if self.get(id).is_none() {
            return false;
        }
        self.echoes
            .retain(|(_, at)| now.saturating_duration_since(*at) < ECHO_LIFETIME);
        self.echoes.push((fingerprint, now));
        self.last_write = Some(now);
        true
    }

    pub fn reuse(&mut self, id: usize) {
        if let Some(index) = self.records.iter().position(|record| record.id == id) {
            let record = self.records.remove(index).unwrap();
            self.records.push_front(record);
        }
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
                observed: true,
            }
        };
        record.observation = event.clone();
        record.observed = true;
        record.updated_ms = event.time_ms;
        // 补齐后可能与另一条完整内容相同，仍只保留一份内容，复制事件全部归并。
        if same.is_some() || late.is_some() {
            while let Some(index) = self
                .records
                .iter()
                .position(|other| other.content.fingerprint == record.content.fingerprint)
            {
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

    fn evict(&mut self) {
        let mut bytes: usize = self.records.iter().map(|record| record.content.bytes).sum();
        while self.records.len() > self.max_records || bytes > self.max_bytes {
            if let Some(record) = self.records.pop_back() {
                bytes -= record.content.bytes;
                if self.last_observation_id == Some(record.id) {
                    self.last_observation_id = None;
                }
            } else {
                break;
            }
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            items: self
                .records
                .iter()
                .take(300)
                .map(|record| {
                    let event = record.events.last().unwrap();
                    Preview {
                        id: record.id,
                        kind: record.content.kind,
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
                    }
                })
                .collect(),
            total: self.records.len(),
            recording: self.recording,
            last_recorded_ms: self.last_recorded_ms,
        }
    }

    pub fn seed_samples(&mut self) -> Result<(), String> {
        for sample in crate::samples::all().into_iter().rev() {
            let event = CopyEvent {
                time_ms: crate::log::now_ms(),
                sequence: 0,
                source_process: "示例".into(),
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
                observed: false,
            });
            self.next_id += 1;
        }
        Ok(())
    }
}

#[cfg(all(test, windows))]
mod tests;
