//! 备份导出与导入（M2-4 / B07）的命令层：互斥、路径校验、日志、中文错误。
//! 备份文件的格式、检查和合并都在 `nianlai-store` 的 `backup` 模块里。
//!
//! 磁盘上的位置：备份文件由用户在系统对话框里选；导出和导入过程中的临时目录在
//! `<数据目录>\imports\`（导出用 `export-*`，导入检查用令牌命名），结束即删，下次启动也会清空；
//! 导出时的 `<目标>.pending` 暂存文件与目标在同一目录，失败时删除。
use super::History;
use crate::platform;
use nianlai_store::{BackupSummary, Inspection, Store};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

const MEMORY_MODE: &str = "历史库没有打开，备份与导入暂时不可用";
const BUSY_MESSAGE: &str = "正在进行另一项备份或导入，请等它结束再试";
const SUFFIX: &str = ".nianlai-backup";

static BUSY: AtomicBool = AtomicBool::new(false);

/// 同一时间只允许一项导出或导入；离开作用域释放。
struct Busy;
impl Busy {
    fn acquire() -> Result<Self, String> {
        BUSY.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self)
            .map_err(|_| BUSY_MESSAGE.to_owned())
    }
}
impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}

#[derive(Serialize)]
pub struct Summary {
    pub items: usize,
    pub favorites: usize,
    pub icons: usize,
    pub bytes: u64,
    pub default_name: String,
}

#[derive(Serialize)]
pub struct Exported {
    pub path: String,
    pub bytes: u64,
    pub items: usize,
    pub favorites: usize,
    pub icons: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Merge,
    Overwrite,
}

#[derive(Serialize)]
pub struct Imported {
    pub mode: Mode,
    pub added_items: usize,
    pub added_favorites: usize,
    /// 覆盖已登记，应用马上要退出并重启来完成替换。
    pub relaunch: bool,
    /// 重启是否由守护者自动完成；为假时（不是守护者拉起的）需要用户自己再打开。
    pub auto_restart: bool,
}

pub fn default_name() -> String {
    let (year, month, day) = platform::time::local_date();
    format!("拈来-{year:04}-{month:02}-{day:02}{SUFFIX}")
}

fn is_backup_name(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(&SUFFIX[1..]))
}

/// 要读取或定位的备份文件：必须是已存在的 `.nianlai-backup` 文件。
fn existing_backup(path: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(path);
    if !is_backup_name(&path) || !path.is_file() {
        return Err("找不到这个备份文件（需要是已存在的 .nianlai-backup 文件）".to_owned());
    }
    Ok(path)
}

pub fn reveal(path: &str) -> Result<(), String> {
    platform::dialog::reveal(&existing_backup(path)?)
}

impl History {
    fn backup_store(&self) -> Result<(Store, PathBuf), String> {
        let store = self.store().ok_or(MEMORY_MODE)?;
        let directory = store.directory().to_path_buf();
        Ok((store, directory))
    }

    pub fn backup_summary(&self) -> Result<Summary, String> {
        let (_, directory) = self.backup_store()?;
        let summary: BackupSummary = nianlai_store::backup_summary(&directory)
            .map_err(|_| "统计备份内容失败，请稍后再试".to_owned())?;
        Ok(Summary {
            items: summary.items,
            favorites: summary.favorites,
            icons: summary.icons,
            bytes: summary.bytes,
            default_name: default_name(),
        })
    }

    pub fn export_backup(&self, path: &str) -> Result<Exported, String> {
        let _busy = Busy::acquire()?;
        let (_, directory) = self.backup_store()?;
        let mut destination = PathBuf::from(path);
        if destination.as_os_str().is_empty() || destination.is_dir() {
            return Err("保存位置无效".to_owned());
        }
        if !is_backup_name(&destination) {
            let mut name = destination.into_os_string();
            name.push(SUFFIX);
            destination = PathBuf::from(name);
        }
        if destination
            .parent()
            .is_some_and(|p| !p.as_os_str().is_empty() && !p.is_dir())
        {
            return Err("保存位置的文件夹不存在".to_owned());
        }
        // 导出要先在数据目录写一份临时库，空间不够就不开始。
        let estimate = nianlai_store::backup_summary(&directory)
            .map(|s| s.bytes)
            .unwrap_or(0);
        let needed = estimate + estimate / 10 + 16 * 1024 * 1024;
        if platform::disk::free_bytes(&directory).is_some_and(|free| free < needed) {
            return Err(format!(
                "数据所在磁盘的剩余空间不够：导出需要约 {} MB",
                needed.div_ceil(1024 * 1024)
            ));
        }
        let started = std::time::Instant::now();
        let result =
            nianlai_store::export_backup(&directory, &destination, env!("CARGO_PKG_VERSION"));
        self.logger.event(
            "backup_exported",
            json!({"elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
                "items":result.as_ref().ok().map(|r|r.items),
                "bytes":result.as_ref().ok().map(|r|r.bytes),
                "error_code":result.as_ref().err().map(ToString::to_string)}),
        );
        let report = result.map_err(|_| "导出没有成功，现有数据不受影响".to_owned())?;
        Ok(Exported {
            path: destination.to_string_lossy().into_owned(),
            bytes: report.bytes,
            items: report.items,
            favorites: report.favorites,
            icons: report.icons,
        })
    }

    pub fn inspect_backup(&self, path: &str) -> Result<Inspection, String> {
        let _busy = Busy::acquire()?;
        let (_, directory) = self.backup_store()?;
        let file = existing_backup(path)?;
        let started = std::time::Instant::now();
        let inspection = nianlai_store::inspect_backup(&directory, &file);
        self.logger.event(
            "backup_inspected",
            json!({"ok":inspection.ok,"elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
                "failed_step":inspection.steps.iter().find(|s|!s.ok).map(|s|s.id),
                "upgraded_from":inspection.upgraded_from}),
        );
        Ok(inspection)
    }

    pub fn import_backup(&self, token: &str, mode: Mode) -> Result<Imported, String> {
        self.images.lock().unwrap().clear();
        let _busy = Busy::acquire()?;
        let (store, directory) = self.backup_store()?;
        let result = match mode {
            Mode::Merge => store
                .import_merge(token)
                .map(|report| Imported {
                    mode,
                    added_items: report.added_items,
                    added_favorites: report.added_favorites,
                    relaunch: false,
                    auto_restart: false,
                })
                .map_err(|error| match error {
                    nianlai_store::Error::Invalid("import_missing") => {
                        "这份备份的检查结果已失效，请重新选择文件".to_owned()
                    }
                    _ => "合并没有成功，现有数据不受影响".to_owned(),
                }),
            Mode::Overwrite => super::snapshots::schedule_import(&directory, token, &self.logger)
                .map(|()| Imported {
                    mode,
                    added_items: 0,
                    added_favorites: 0,
                    relaunch: true,
                    auto_restart: crate::recovery::is_child(),
                }),
        };
        self.logger.event(
            "backup_imported",
            json!({"mode":mode,"added_items":result.as_ref().ok().map(|r|r.added_items),
                "added_favorites":result.as_ref().ok().map(|r|r.added_favorites),
                "error":result.as_ref().err()}),
        );
        let imported = result?;
        if mode == Mode::Merge {
            // 合并完成，临时目录不再需要；覆盖要留到重启后用。
            nianlai_store::discard_import(&directory, token);
            self.notify_changed();
        }
        Ok(imported)
    }

    pub fn discard_import(&self, token: &str) {
        // 正在导出 / 导入时不动临时目录；已登记覆盖的临时目录要留到重启。
        let Ok(_busy) = Busy::acquire() else {
            return;
        };
        let Some(directory) = self.store_directory() else {
            return;
        };
        if !super::snapshots::import_pending(&directory, token) {
            nianlai_store::discard_import(&directory, token);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_name_has_local_date_and_suffix_and_only_one_job_runs_at_a_time() {
        let name = default_name();
        assert!(name.starts_with("拈来-") && name.ends_with(".nianlai-backup"));
        assert_eq!(
            name.chars().count(),
            "拈来-2026-01-02.nianlai-backup".chars().count()
        );
        let first = Busy::acquire().unwrap();
        assert_eq!(Busy::acquire().err().as_deref(), Some(BUSY_MESSAGE));
        drop(first);
        assert!(Busy::acquire().is_ok());
        assert!(existing_backup("nope.nianlai-backup").is_err());
        assert!(existing_backup("Cargo.toml").is_err());
    }
}
