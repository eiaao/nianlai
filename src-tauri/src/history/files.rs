//! 文件类条目的失效检查（M2-3）。
//!
//! 文件类条目只存路径，原文件被移走、改名或删除后条目就“失效”了。这里只查元数据（不读内容），
//! 结果放在内存里：面板呼出时查列表里看得到的文件条目，后台每天查一次全部。
//! 网络位置可能长时间无响应，单独开线程限时，超时记为“未确认”；同一次检查里同一个网络根目录
//! 超时一次后，其余路径直接记为未确认，不再一个个等。
use super::{
    DAY_MS, Shared,
    content::{Content, FILES},
};
use nianlai_store::{FileList, content::Data, now_ms};
use serde::Serialize;
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    fs, io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

/// 网络位置单个路径的等待上限。
pub const NETWORK_TIMEOUT: Duration = Duration::from_millis(1500);
/// 全量检查的间隔：后台每小时醒一次，距上次全量满一天才查。
pub const FULL_INTERVAL_MS: u64 = DAY_MS;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathState {
    Present,
    Missing,
    Unknown,
}

/// 一条文件条目的检查结论；文件都在时不记。
/// state：missing = 全部不在；partial = 部分不在（其余还能用）；unknown = 有网络位置没确认、其余都在。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Mark {
    pub state: &'static str,
    pub missing: usize,
    pub unknown: usize,
    pub total: usize,
    pub checked_ms: u64,
}

impl Mark {
    fn from(states: &[PathState], checked_ms: u64) -> Option<Self> {
        let missing = states.iter().filter(|s| **s == PathState::Missing).count();
        let unknown = states.iter().filter(|s| **s == PathState::Unknown).count();
        let total = states.len();
        let state = if total > 0 && missing == total {
            "missing"
        } else if missing > 0 {
            "partial"
        } else if unknown > 0 {
            "unknown"
        } else {
            return None;
        };
        Some(Self {
            state,
            missing,
            unknown,
            total,
            checked_ms,
        })
    }
    /// 定论：同样的检查结果不因检查时间不同而算作变化。
    fn same(&self, other: &Self) -> bool {
        (self.state, self.missing, self.unknown, self.total)
            == (other.state, other.missing, other.unknown, other.total)
    }
}

/// 预览里逐个列出的路径。
#[derive(Clone, Debug, Serialize)]
pub struct PathCheck {
    pub path: String,
    pub state: PathState,
}

#[derive(Default)]
pub struct Marks {
    marks: Mutex<HashMap<usize, Mark>>,
    last_full_ms: AtomicU64,
    checking: AtomicBool,
}

impl Marks {
    pub fn get(&self, id: usize) -> Option<Mark> {
        self.marks.lock().unwrap().get(&id).cloned()
    }
    pub fn last_full_ms(&self) -> Option<u64> {
        Some(self.last_full_ms.load(Ordering::Acquire)).filter(|ms| *ms > 0)
    }
    /// 全部文件都不在的条目（“清理失效条目”只清这些；部分失效的其余文件还能用）。
    pub fn missing(&self) -> Vec<usize> {
        let mut ids: Vec<usize> = self
            .marks
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, mark)| mark.state == "missing")
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();
        ids
    }
    pub fn counts(&self) -> (usize, usize, usize) {
        let marks = self.marks.lock().unwrap();
        let count = |state| marks.values().filter(|m| m.state == state).count();
        (count("missing"), count("partial"), count("unknown"))
    }
    /// 进了回收站的条目不再算失效；恢复后下次检查再标。
    pub fn forget(&self, ids: &[usize]) {
        self.update(ids.iter().map(|id| (*id, None)).collect(), false);
    }
    /// 记下这些条目的结论；返回是否有变化。`all` 为 true 表示这是全量结果，名单外的记录一并清掉。
    fn update(&self, results: Vec<(usize, Option<Mark>)>, all: bool) -> bool {
        let mut marks = self.marks.lock().unwrap();
        let mut changed = false;
        let checked: HashSet<usize> = results.iter().map(|(id, _)| *id).collect();
        if all {
            let before = marks.len();
            marks.retain(|id, _| checked.contains(id));
            changed |= marks.len() != before;
        }
        for (id, mark) in results {
            match mark {
                Some(mark) => {
                    if marks.get(&id).is_none_or(|old| !old.same(&mark)) {
                        changed = true;
                    }
                    marks.insert(id, mark);
                }
                None => changed |= marks.remove(&id).is_some(),
            }
        }
        changed
    }
}

/// 检查一组路径。网络位置限时，同一次检查里同一个网络根目录超时过就不再等。
pub fn probe(paths: &[String]) -> Vec<PathState> {
    let mut dead = HashSet::new();
    paths
        .iter()
        .map(|path| {
            if !crate::platform::disk::is_remote(path) {
                return local(path);
            }
            let root = root(path);
            if dead.contains(&root) {
                return PathState::Unknown;
            }
            let (sender, receiver) = mpsc::sync_channel(1);
            let owned = path.clone();
            let started = std::thread::Builder::new()
                .name("file-probe".into())
                .spawn(move || {
                    let _ = sender.send(local(&owned));
                })
                .is_ok();
            match started.then(|| receiver.recv_timeout(NETWORK_TIMEOUT)) {
                Some(Ok(state)) => state,
                _ => {
                    dead.insert(root);
                    PathState::Unknown
                }
            }
        })
        .collect()
}

fn local(path: &str) -> PathState {
    match fs::symlink_metadata(path) {
        Ok(_) => PathState::Present,
        Err(error) if error.kind() == io::ErrorKind::NotFound => PathState::Missing,
        Err(_) => PathState::Unknown,
    }
}

/// 网络根目录：\\server\share 或盘符。
fn root(path: &str) -> String {
    let normalized = path.replace('/', "\\");
    if let Some(rest) = normalized.strip_prefix("\\\\") {
        let mut parts = rest.splitn(3, '\\');
        let server = parts.next().unwrap_or_default();
        let share = parts.next().unwrap_or_default();
        return format!("\\\\{server}\\{share}").to_lowercase();
    }
    normalized
        .chars()
        .take(2)
        .collect::<String>()
        .to_lowercase()
}

pub(super) fn memory_lists(memory: &super::memory::History) -> Vec<FileList> {
    memory
        .records()
        .filter_map(
            |record| match record.content.formats.get(FILES).map(|f| &f.data) {
                Some(Data::Files(paths)) => Some(FileList {
                    id: record.id,
                    favorite: record.favorite,
                    paths: paths.to_vec(),
                }),
                _ => None,
            },
        )
        .collect()
}

/// 粘贴文件条目前的检查结果。
pub enum Usable {
    /// 不是文件条目，或文件都在（未确认的也照常粘贴）。
    All,
    /// 部分文件不在：只粘贴还在的，第二项是跳过的个数。
    Partial(Arc<Content>, usize),
    /// 全部不在：不粘贴。
    Nothing,
}

/// 粘贴前现查一遍路径（M2-3）。部分不在时重新组一份只含还在的文件的内容：只留文件列表和
/// “首选放置效果”，其余描述文件的格式（Shell IDList 等）里还有已不在的文件，一并去掉。
pub fn usable(content: &Arc<Content>, hash: nianlai_store::content::Hash) -> Usable {
    let Some(Data::Files(paths)) = content.formats.get(FILES).map(|f| &f.data) else {
        return Usable::All;
    };
    let states = probe(paths);
    let kept: Vec<String> = paths
        .iter()
        .zip(&states)
        .filter(|(_, state)| **state != PathState::Missing)
        .map(|(path, _)| path.clone())
        .collect();
    if kept.len() == paths.len() {
        return Usable::All;
    }
    if kept.is_empty() {
        return Usable::Nothing;
    }
    let skipped = paths.len() - kept.len();
    let mut formats = std::collections::BTreeMap::new();
    formats.insert(FILES.to_owned(), Data::Files(kept.into()));
    if let Some(effect) = content.formats.get("Preferred DropEffect") {
        formats.insert("Preferred DropEffect".to_owned(), effect.data.clone());
    }
    match Content::new(formats, hash) {
        Ok(filtered) => Usable::Partial(filtered, skipped),
        Err(_) => Usable::All,
    }
}

impl super::History {
    pub fn usable_files(&self, content: &Arc<Content>) -> Usable {
        usable(content, self.0.hash)
    }

    /// 面板呼出后在后台检查列表里看得到的文件条目（最近的和收藏的）；结论有变化就通知面板刷新。
    pub fn check_visible_files(&self) {
        let shared = self.0.clone();
        let _ = std::thread::Builder::new()
            .name("file-check".into())
            .spawn(move || {
                let mut ids: Vec<usize> = shared
                    .snapshot()
                    .items
                    .into_iter()
                    .chain(shared.favorites().unwrap_or_default())
                    .filter(|item| item.kind == "文件")
                    .map(|item| item.id)
                    .collect();
                ids.sort_unstable();
                ids.dedup();
                if !ids.is_empty() && shared.check_files(Some(&ids), "visible") {
                    shared.notify_changed();
                }
            });
    }
}

impl Shared {
    fn file_lists(&self) -> Result<Vec<FileList>, String> {
        if let Some(store) = self.store() {
            store
                .reader()
                .and_then(|r| r.file_lists())
                .map_err(|e| e.to_string())
        } else {
            Ok(memory_lists(&self.memory.lock().unwrap()))
        }
    }

    /// 检查文件条目：`ids` 为 None 时检查全部（并记下全量检查时间）。返回结论是否有变化。
    /// 同一时间只跑一次检查；正在检查时直接返回 false。
    pub fn check_files(&self, ids: Option<&[usize]>, reason: &str) -> bool {
        if self.files.checking.swap(true, Ordering::AcqRel) {
            return false;
        }
        let started = Instant::now();
        let result = self.file_lists().map(|lists| {
            let lists: Vec<FileList> = match ids {
                Some(ids) => lists.into_iter().filter(|l| ids.contains(&l.id)).collect(),
                None => lists,
            };
            let now = now_ms();
            let results: Vec<(usize, Option<Mark>)> = lists
                .iter()
                .map(|list| (list.id, Mark::from(&probe(&list.paths), now)))
                .collect();
            let checked = results.len();
            let changed = self.files.update(results, ids.is_none());
            if ids.is_none() {
                self.files.last_full_ms.store(now, Ordering::Release);
            }
            (checked, changed)
        });
        self.files.checking.store(false, Ordering::Release);
        let (missing, partial, unknown) = self.files.counts();
        self.logger.event(
            "files_checked",
            json!({"reason":reason,"checked":result.as_ref().ok().map(|r| r.0),"missing":missing,
                "partial":partial,"unknown":unknown,"elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
                "error_code":result.as_ref().err()}),
        );
        result.is_ok_and(|(_, changed)| changed)
    }

    /// 后台定时：距上次全量检查满一天才查。
    pub fn check_files_daily(&self) -> bool {
        let due = self
            .files
            .last_full_ms()
            .is_none_or(|last| now_ms().saturating_sub(last) >= FULL_INTERVAL_MS);
        due && self.check_files(None, "daily")
    }

    /// 预览：逐个路径现查，顺便更新这一条的结论。不是文件条目返回 None。
    pub fn file_paths(&self, id: usize) -> Result<Option<Vec<PathCheck>>, String> {
        let Some(list) = self.file_lists()?.into_iter().find(|l| l.id == id) else {
            return Ok(None);
        };
        let states = probe(&list.paths);
        let changed = self
            .files
            .update(vec![(id, Mark::from(&states, now_ms()))], false);
        if changed {
            self.notify_changed();
        }
        Ok(Some(
            list.paths
                .into_iter()
                .zip(states)
                .map(|(path, state)| PathCheck { path, state })
                .collect(),
        ))
    }

    pub fn notify_changed(&self) {
        let changed = self.changed.lock().unwrap().clone();
        if let Some(changed) = changed {
            changed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary() -> std::path::PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "nianlai-files-test-{}-{}-{}",
            std::process::id(),
            now_ms(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn probe_and_marks_follow_files_being_moved_or_deleted() {
        let directory = temporary();
        let kept = directory.join("在.txt");
        let gone = directory.join("不在.txt");
        fs::write(&kept, "a").unwrap();
        fs::write(&gone, "b").unwrap();
        let paths = vec![
            kept.to_string_lossy().into_owned(),
            gone.to_string_lossy().into_owned(),
        ];
        assert_eq!(probe(&paths), [PathState::Present, PathState::Present]);
        assert_eq!(Mark::from(&probe(&paths), 1), None);

        fs::remove_file(&gone).unwrap();
        let mark = Mark::from(&probe(&paths), 1).unwrap();
        assert_eq!((mark.state, mark.missing, mark.total), ("partial", 1, 2));
        // 改名也算不在了。
        fs::rename(&kept, directory.join("改名.txt")).unwrap();
        let mark = Mark::from(&probe(&paths), 1).unwrap();
        assert_eq!((mark.state, mark.missing), ("missing", 2));
        // 目录也算文件条目里的一项。
        assert_eq!(
            probe(&[directory.to_string_lossy().into_owned()]),
            [PathState::Present]
        );
        fs::remove_dir_all(&directory).unwrap();

        let unknown = Mark::from(&[PathState::Present, PathState::Unknown], 1).unwrap();
        assert_eq!(unknown.state, "unknown");
        let both = Mark::from(&[PathState::Missing, PathState::Unknown], 1).unwrap();
        assert_eq!(both.state, "partial");
    }

    #[test]
    fn marks_update_reports_changes_and_full_checks_drop_stale_entries() {
        let marks = Marks::default();
        let missing = |at| Mark::from(&[PathState::Missing], at);
        assert!(marks.update(vec![(1, missing(1)), (2, missing(1))], false));
        // 只是检查时间不同，不算变化。
        assert!(!marks.update(vec![(1, missing(2))], false));
        assert_eq!(marks.missing(), [1, 2]);
        // 全量检查：2 已不在名单里（进了回收站），一并清掉。
        assert!(marks.update(vec![(1, missing(3))], true));
        assert_eq!(marks.missing(), [1]);
        assert!(marks.update(vec![(1, None)], false));
        assert!(marks.missing().is_empty());
        assert_eq!(root("\\\\NAS\\Share\\a\\b.txt"), "\\\\nas\\share");
        assert_eq!(root("D:\\a.txt"), "d:");
    }
}
