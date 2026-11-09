//! 存储管理（M2-3 / B05）：占用、垃圾清理、压缩。
//!
//! 不做自动清理；这里的清理都由用户在设置“数据与备份”页手动发起。条目类的清理（失效条目、截图）
//! 进回收站，收藏的不动；临时导出的 PNG 和恢复时留下的旧库副本是文件，直接删除。
use super::{DAY_MS, Shared};
use nianlai_store::{KindUsage, now_ms};
use serde::Serialize;
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

/// 临时导出的 PNG 刚生成时目标程序可能还在读；只删这么久以前的。
const EXPORT_GRACE: Duration = Duration::from_secs(10 * 60);
/// 手动挑选截图的缩略图长边像素。
const THUMBNAIL_SIZE: u32 = 240;
const MEMORY_MODE: &str = "历史库没有打开，存储管理暂时不可用";

#[derive(Clone, Copy, Default, Serialize)]
pub struct Files {
    pub count: usize,
    pub bytes: u64,
}

#[derive(Serialize)]
pub struct FileCheck {
    pub missing: usize,
    pub partial: usize,
    pub unknown: usize,
    pub checked_ms: Option<u64>,
}

#[derive(Serialize)]
pub struct ImageCounts {
    pub all: Files,
    pub older_30: Files,
    pub older_90: Files,
}

#[derive(Serialize)]
pub struct Overview {
    /// history.db 连同 -wal / -shm 的大小。
    pub database_bytes: u64,
    pub kinds: Vec<KindUsage>,
    pub trashed: Files,
    pub reclaimable_bytes: u64,
    pub snapshots: Files,
    pub exports: Files,
    /// 恢复快照时留下的旧库副本（history.db.before-restore-*）。
    pub leftovers: Files,
    pub free_bytes: Option<u64>,
    pub files: FileCheck,
    pub images: ImageCounts,
}

#[derive(Serialize)]
pub struct ImageItem {
    pub id: usize,
    pub bytes: usize,
    pub copied_ms: u64,
    pub favorite: bool,
}

fn file_size(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |m| m.len())
}

/// 目录下满足条件的文件（不进子目录）。
fn files_in(directory: &Path, keep: impl Fn(&str) -> bool) -> Vec<(PathBuf, fs::Metadata)> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_name().to_str().is_some_and(&keep))
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            metadata.is_file().then(|| (entry.path(), metadata))
        })
        .collect()
}

fn total(files: &[(PathBuf, fs::Metadata)]) -> Files {
    Files {
        count: files.len(),
        bytes: files.iter().map(|(_, m)| m.len()).sum(),
    }
}

/// history.db 连同 -wal / -shm 的大小。
fn database_bytes(directory: &Path) -> u64 {
    ["history.db", "history.db-wal", "history.db-shm"]
        .iter()
        .map(|name| file_size(&directory.join(name)))
        .sum()
}

fn leftovers(directory: &Path) -> Vec<(PathBuf, fs::Metadata)> {
    files_in(directory, |name| {
        name.starts_with("history.db.before-restore-")
    })
}

fn exports(directory: &Path) -> Vec<(PathBuf, fs::Metadata)> {
    files_in(&directory.join("exports"), |name| {
        name.starts_with("nianlai-") && (name.ends_with(".png") || name.ends_with(".partial"))
    })
}

impl Shared {
    fn storage(&self) -> Result<(nianlai_store::Store, PathBuf), String> {
        let store = self.store().ok_or(MEMORY_MODE)?;
        let directory = store.directory().to_path_buf();
        Ok((store, directory))
    }

    pub fn storage_overview(&self) -> Result<Overview, String> {
        let (store, directory) = self.storage()?;
        let reader = store.reader().map_err(|e| e.to_string())?;
        let usage = reader.usage().map_err(|e| e.to_string())?;
        let images = |before: Option<u64>| -> Result<Files, String> {
            let items = reader.images(before).map_err(|e| e.to_string())?;
            let free: Vec<_> = items.iter().filter(|item| !item.favorite).collect();
            Ok(Files {
                count: free.len(),
                bytes: free.iter().map(|item| item.bytes as u64).sum(),
            })
        };
        let now = now_ms();
        let images = ImageCounts {
            all: images(None)?,
            older_30: images(Some(now.saturating_sub(30 * DAY_MS)))?,
            older_90: images(Some(now.saturating_sub(90 * DAY_MS)))?,
        };
        let database_bytes = database_bytes(&directory);
        let (missing, partial, unknown) = self.files.counts();
        Ok(Overview {
            database_bytes,
            kinds: usage.kinds,
            trashed: Files {
                count: usage.trashed_count,
                bytes: usage.trashed_bytes,
            },
            reclaimable_bytes: usage.reclaimable_bytes,
            snapshots: total(&files_in(&directory.join("snapshots"), |name| {
                name.ends_with(".db")
            })),
            exports: total(&exports(&directory)),
            leftovers: total(&leftovers(&directory)),
            free_bytes: crate::platform::disk::free_bytes(&directory),
            files: FileCheck {
                missing,
                partial,
                unknown,
                checked_ms: self.files.last_full_ms(),
            },
            images,
        })
    }

    /// 把条目移进回收站（收藏的跳过），并记日志。返回移进去的条数。
    fn trash_for_cleanup(&self, ids: Vec<usize>, target: &str) -> Result<usize, String> {
        let (store, _) = self.storage()?;
        let result = store.trash_many(ids).map_err(|e| e.to_string());
        if let Ok(moved) = &result {
            // 进了回收站就不再算失效条目；恢复后下次检查再标。
            self.files.forget(moved);
            if !moved.is_empty() {
                self.notify_changed();
            }
        }
        self.logger.event(
            "storage_cleaned",
            json!({"target":target,"count":result.as_ref().ok().map(Vec::len),"error_code":result.as_ref().err()}),
        );
        result.map(|moved| moved.len())
    }

    /// 清理失效条目：只清全部文件都不在的（部分失效的其余文件还能用，未确认的不动）。
    pub fn clean_missing(&self) -> Result<usize, String> {
        self.trash_for_cleanup(self.files.missing(), "missing_files")
    }

    /// 清理截图类图片：`older_days` 为 None 时清全部（收藏的不动）。
    pub fn clean_images(&self, older_days: Option<u64>) -> Result<usize, String> {
        let (store, _) = self.storage()?;
        let before = older_days.map(|days| now_ms().saturating_sub(days * DAY_MS));
        let ids = store
            .reader()
            .and_then(|r| r.images(before))
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|item| !item.favorite)
            .map(|item| item.id)
            .collect();
        self.trash_for_cleanup(ids, "images")
    }

    /// 手动挑选的条目移进回收站。
    pub fn trash_items(&self, ids: Vec<usize>) -> Result<usize, String> {
        self.trash_for_cleanup(ids, "picked")
    }

    /// 手动挑选截图的列表，按大小从大到小。
    pub fn images(&self) -> Result<Vec<ImageItem>, String> {
        let (store, _) = self.storage()?;
        Ok(store
            .reader()
            .and_then(|r| r.images(None))
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|item| ImageItem {
                id: item.id,
                bytes: item.bytes,
                copied_ms: item.copied_ms,
                favorite: item.favorite,
            })
            .collect())
    }

    /// 缩略图（PNG 的 data URL），现场从库里的图片缩小，不落盘。
    pub fn thumbnail(&self, id: usize) -> Result<Option<String>, String> {
        let Some(content) = self.get(id)? else {
            return Ok(None);
        };
        let png = crate::platform::image::thumbnail(&content, THUMBNAIL_SIZE)?;
        Ok(Some(format!("data:image/png;base64,{}", base64(&png))))
    }

    /// 删除临时导出的 PNG（只删 10 分钟以前的，刚生成的目标程序可能还在读）。
    pub fn clean_exports(&self) -> Result<Files, String> {
        let directory = self
            .directory
            .get()
            .cloned()
            .ok_or(MEMORY_MODE.to_owned())?;
        let old: Vec<_> = exports(&directory)
            .into_iter()
            .filter(|(_, metadata)| {
                metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.elapsed().ok())
                    .is_some_and(|age| age >= EXPORT_GRACE)
            })
            .collect();
        Ok(self.remove_files(old, "exports"))
    }

    /// 删除恢复快照时留下的旧库副本。
    pub fn clean_leftovers(&self) -> Result<Files, String> {
        let (_, directory) = self.storage()?;
        Ok(self.remove_files(leftovers(&directory), "leftovers"))
    }

    fn remove_files(&self, files: Vec<(PathBuf, fs::Metadata)>, target: &str) -> Files {
        let mut removed = Files::default();
        let mut failed = 0;
        for (path, metadata) in files {
            if fs::remove_file(&path).is_ok() {
                removed.count += 1;
                removed.bytes += metadata.len();
            } else {
                failed += 1;
            }
        }
        self.logger.event(
            "storage_cleaned",
            json!({"target":target,"count":removed.count,"bytes":removed.bytes,"failed":failed}),
        );
        removed
    }

    /// 压缩历史库。VACUUM 要写一份完整的临时副本，先确认剩余空间够用。返回压缩前后省下的字节数。
    pub fn compact(&self) -> Result<u64, String> {
        let (store, directory) = self.storage()?;
        // 连同 -wal 一起算：压缩会把 WAL 并回主库，只看主库会以为变大了。
        let before = database_bytes(&directory);
        let needed = before + before / 10 + 16 * 1024 * 1024;
        if crate::platform::disk::free_bytes(&directory).is_some_and(|free| free < needed) {
            return Err(format!(
                "磁盘剩余空间不够：压缩需要约 {} MB",
                needed.div_ceil(1024 * 1024)
            ));
        }
        let started = std::time::Instant::now();
        let result = store.compact().map_err(|e| e.to_string());
        let after = database_bytes(&directory);
        self.logger.event(
            "storage_compacted",
            json!({"before_bytes":before,"after_bytes":after,"elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
                "error_code":result.as_ref().err()}),
        );
        result.map(|_| before.saturating_sub(after))
    }
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_standard_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
