use crate::settings::Directories;
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct Logger {
    sender: mpsc::Sender<Message>,
    // 策略固定为隐藏时挂起；保留原日志字段及平台调用接口。
    pub tier: u8,
    pub directories: Arc<Directories>,
}

enum Message {
    Event(Value),
    Flush(mpsc::SyncSender<()>),
}

pub const COMPATIBILITY_FILE: &str = "panel-log.jsonl";

// 应用与守护者共用文件名，日期取事件的 UTC 时间。
pub fn daily_file_name(millis: u64) -> String {
    format!("panel-{}.jsonl", day_stamp(millis))
}

// 指定目录不可写时共用同一种退路，按进程号隔离。
pub fn fallback_directory() -> PathBuf {
    std::env::temp_dir()
        .join("NianlaiLogs")
        .join(std::process::id().to_string())
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// 使用与 time_ms 一致的 UTC 日期，避免夏令时或时区变动让轮转顺序倒退。
fn day_stamp(millis: u64) -> String {
    let days = (millis / 86_400_000) as i64 + 719_468;
    let era = days / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = month_index + if month_index < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!("{year:04}{month:02}{day:02}")
}

fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

struct DailyWriter {
    directory: PathBuf,
    max_bytes: u64,
    file_name: String,
    file: Option<File>,
    compatibility: bool,
    compatibility_file: Option<File>,
}

impl DailyWriter {
    fn new(directory: PathBuf, max_bytes: u64, compatibility: bool, time: u64) -> io::Result<Self> {
        fs::create_dir_all(&directory)?;
        let mut writer = Self {
            directory,
            max_bytes,
            file_name: String::new(),
            file: None,
            compatibility,
            compatibility_file: None,
        };
        writer.open_day(time)?;
        writer.prune()?;
        Ok(writer)
    }

    fn current_path(&self) -> PathBuf {
        self.directory.join(&self.file_name)
    }

    fn compatibility_path(&self) -> PathBuf {
        self.directory.join(COMPATIBILITY_FILE)
    }

    fn open_day(&mut self, time: u64) -> io::Result<()> {
        let file_name = daily_file_name(time);
        if self.file_name != file_name {
            self.file.take();
            self.file_name = file_name;
        }
        if self.file.is_none() {
            self.file = Some(open_append(&self.current_path())?);
        }
        // 工具按偏移连续读取此文件，跨天时也不能切换或清空。
        if self.compatibility && self.compatibility_file.is_none() {
            self.compatibility_file = Some(open_append(&self.compatibility_path())?);
        }
        Ok(())
    }

    fn write(&mut self, value: &Value) -> io::Result<()> {
        self.open_day(value["time_ms"].as_u64().unwrap_or_else(now_ms))?;
        let line = format!("{value}\n");
        self.file.as_mut().unwrap().write_all(line.as_bytes())?;
        if let Some(file) = &mut self.compatibility_file {
            file.write_all(line.as_bytes())?;
        }
        self.flush()?;
        self.prune()
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(file) = &mut self.file {
            file.flush()?;
        }
        if let Some(file) = &mut self.compatibility_file {
            file.flush()?;
        }
        Ok(())
    }

    fn prune(&mut self) -> io::Result<()> {
        let mut files = Vec::new();
        let mut total = 0_u64;
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            // 不跟随链接，也不删除同目录下的历史库、导出或其他文件。
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let path = entry.path();
            // Windows 的目录枚举信息可能尚未反映打开文件的写入量；活动文件以句柄为准。
            let metadata = if path == self.current_path()
                && let Some(file) = &self.file
            {
                file.metadata()?
            } else if path == self.compatibility_path()
                && let Some(file) = &self.compatibility_file
            {
                file.metadata()?
            } else {
                entry.metadata()?
            };
            let date = if name == COMPATIBILITY_FILE {
                day_stamp(
                    metadata
                        .modified()?
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                )
            } else if let Some(date) = name
                .strip_prefix("panel-")
                .and_then(|name| name.strip_suffix(".jsonl"))
                .filter(|date| date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()))
            {
                date.to_owned()
            } else {
                continue;
            };
            total = total.saturating_add(metadata.len());
            files.push((date, name.to_owned(), path, metadata.len()));
        }
        files.sort_by(|left, right| (&left.0, &left.1).cmp(&(&right.0, &right.1)));
        for (_, _, path, bytes) in files {
            if total <= self.max_bytes {
                break;
            }
            // 当前单日文件超限也必须释放；下一条事件会重新创建完整 JSONL 文件。
            if path == self.current_path() {
                self.file.take();
            }
            if path == self.compatibility_path() {
                self.compatibility_file.take();
            }
            fs::remove_file(path)?;
            total = total.saturating_sub(bytes);
        }
        Ok(())
    }
}

impl Logger {
    // 单实例通知可能早于主启动流程；保留托盘原有的无参入口。
    pub fn new() -> io::Result<Self> {
        let path = crate::settings::settings_path();
        let (store, issues) = crate::settings::Store::load(path.clone());
        let resolved = crate::settings::Resolved::new(store.get(), path);
        let logger =
            Self::with_settings(&resolved.directories, resolved.settings.log.max_total_mb)?;
        for issue in issues.iter().chain(&resolved.issues) {
            issue.record(&logger);
        }
        Ok(logger)
    }

    pub fn with_settings(directories: &Directories, max_total_mb: u64) -> io::Result<Self> {
        let max_bytes = max_total_mb
            .checked_mul(1024 * 1024)
            .filter(|bytes| *bytes > 0)
            .ok_or_else(|| io::Error::other("日志总量上限无效"))?;
        let mut directories = directories.clone();
        let open =
            |directory| DailyWriter::new(directory, max_bytes, directories.test_data_dir, now_ms());
        // 指定目录不可写时仍允许内存模式启动，回退目录不使用 G0 数据。
        let (mut writer, fallback) = match open(directories.logs.clone()) {
            Ok(writer) => (writer, false),
            Err(_) => {
                let directory = fallback_directory();
                let writer = open(directory.clone())?;
                directories.logs = directory;
                (writer, true)
            }
        };
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("panel-log".into())
            .spawn(move || {
                while let Ok(message) = receiver.recv() {
                    match message {
                        Message::Event(value) => {
                            if writer.write(&value).is_err() {
                                eprintln!("拈来日志写入或轮转失败");
                            }
                        }
                        Message::Flush(done) => {
                            let _ = writer.flush();
                            let _ = done.send(());
                        }
                    }
                }
            })?;
        let logger = Self {
            sender,
            tier: 2,
            directories: Arc::new(directories),
        };
        if fallback {
            logger.event(
                "log_directory_fallback",
                json!({"reason":"日志目录不可写，已使用临时目录"}),
            );
        }
        Ok(logger)
    }

    pub fn event(&self, event: &str, fields: Value) {
        let mut row = json!({
            "time_ms": now_ms(), "event": event, "session": null,
            "hotkey_to_show_ms": null, "show_to_paint_ms": null,
            "process_name": null, "focus_class": null,
            "foreground_path": null, "foreground_ok": null, "focus_restored": null, "focus_unknown": null,
            "replayed_keys": null, "dropped_keys": null, "wait_ms": null, "keyboard_hook": null, "mouse_hook": null,
            "memory_tier": self.tier, "suspend_result": null, "paste_result": null
        });
        if let (Some(row), Some(fields)) = (row.as_object_mut(), fields.as_object()) {
            row.extend(fields.clone());
        }
        let _ = self.sender.send(Message::Event(row));
    }

    pub fn flush(&self) {
        let (sender, receiver) = mpsc::sync_channel(0);
        if self.sender.send(Message::Flush(sender)).is_ok() {
            let _ = receiver.recv();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::tests::Directory;

    #[test]
    fn utc_dates_cover_epoch_leap_day_and_year_boundary() {
        assert_eq!(day_stamp(0), "19700101");
        assert_eq!(day_stamp(951_782_400_000), "20000229");
        assert_eq!(day_stamp(1_704_067_199_999), "20231231");
        assert_eq!(day_stamp(1_704_067_200_000), "20240101");
        assert_eq!(day_stamp(1_709_164_800_000), "20240229");
    }

    #[test]
    fn rotates_daily_and_keeps_compatibility_stream_across_midnight_and_restart() {
        let directory = Directory::new();
        let first = json!({"time_ms":0,"event":"first"});
        let second = json!({"time_ms":86_400_000,"event":"second"});
        let mut writer = DailyWriter::new(directory.0.clone(), 4096, true, 0).unwrap();
        writer.write(&first).unwrap();
        writer.write(&second).unwrap();
        drop(writer);
        let mut writer = DailyWriter::new(directory.0.clone(), 4096, true, 86_400_000).unwrap();
        writer.write(&second).unwrap();
        drop(writer);
        assert_eq!(
            fs::read_to_string(directory.0.join("panel-19700101.jsonl")).unwrap(),
            format!("{first}\n")
        );
        assert_eq!(
            fs::read_to_string(directory.0.join("panel-19700102.jsonl")).unwrap(),
            format!("{second}\n{second}\n")
        );
        assert_eq!(
            fs::read_to_string(directory.0.join("panel-log.jsonl")).unwrap(),
            format!("{first}\n{second}\n{second}\n")
        );
    }

    #[test]
    fn startup_prunes_oldest_dates_and_protects_other_files() {
        let directory = Directory::new();
        for day in ["20240103", "20240101", "20240102"] {
            fs::write(directory.0.join(format!("panel-{day}.jsonl")), b"1234").unwrap();
        }
        fs::write(directory.0.join("history.sqlite"), b"12345678901234567890").unwrap();
        fs::write(directory.0.join("other.jsonl"), b"12345678901234567890").unwrap();
        fs::create_dir(directory.0.join("panel-20230101.jsonl")).unwrap();
        let writer = DailyWriter::new(directory.0.clone(), 8, false, 1_704_326_400_000).unwrap();
        assert!(!directory.0.join("panel-20240101.jsonl").exists());
        assert!(directory.0.join("panel-20240102.jsonl").exists());
        assert!(directory.0.join("panel-20240103.jsonl").exists());
        assert!(directory.0.join("history.sqlite").exists());
        assert!(directory.0.join("other.jsonl").exists());
        assert!(directory.0.join("panel-20230101.jsonl").is_dir());
        assert!(!directory.0.join("panel-log.jsonl").exists());
        drop(writer);
    }

    #[test]
    fn enforces_limit_after_writes_even_when_current_file_alone_is_too_large() {
        let directory = Directory::new();
        let mut writer = DailyWriter::new(directory.0.clone(), 10, false, 0).unwrap();
        writer
            .write(&json!({"time_ms":0,"event":"超过单文件限额"}))
            .unwrap();
        assert!(!directory.0.join("panel-19700101.jsonl").exists());
        writer.max_bytes = 1024;
        writer
            .write(&json!({"time_ms":0,"event":"重新创建"}))
            .unwrap();
        let text = fs::read_to_string(directory.0.join("panel-19700101.jsonl")).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap()["event"],
            "重新创建"
        );
    }

    #[test]
    fn compatibility_copy_counts_towards_total_limit() {
        let directory = Directory::new();
        let value = json!({"time_ms":0,"event":"test"});
        let bytes = value.to_string().len() as u64 + 1;
        let mut writer = DailyWriter::new(directory.0.clone(), bytes, true, 0).unwrap();
        writer.write(&value).unwrap();
        drop(writer);
        let total: u64 = fs::read_dir(&directory.0)
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().len())
            .sum();
        assert_eq!(total, bytes);
        assert_eq!(
            fs::read_to_string(directory.0.join("panel-log.jsonl")).unwrap(),
            format!("{value}\n")
        );
    }

    #[test]
    fn logger_flush_preserves_schema_and_fixed_suspend_tier() {
        let directory = Directory::new();
        let directories = Directories {
            settings_file: directory.0.join("settings.json"),
            data: directory.0.join("data"),
            logs: directory.0.join("logs"),
            data_source: "默认位置",
            test_data_dir: false,
        };
        let logger = Logger::with_settings(&directories, 20).unwrap();
        logger.event("验证", json!({"session":7}));
        logger.flush();
        let path = fs::read_dir(&directories.logs)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(value["event"], "验证");
        assert_eq!(value["session"], 7);
        assert_eq!(value["memory_tier"], 2);
        for field in [
            "time_ms",
            "hotkey_to_show_ms",
            "show_to_paint_ms",
            "process_name",
            "focus_class",
            "foreground_path",
            "foreground_ok",
            "focus_restored",
            "focus_unknown",
            "replayed_keys",
            "dropped_keys",
            "wait_ms",
            "keyboard_hook",
            "mouse_hook",
            "suspend_result",
            "paste_result",
        ] {
            assert!(value.get(field).is_some(), "{field}");
        }
        assert!(!directories.logs.join("panel-log.jsonl").exists());
        assert!(!directories.data.exists());
    }
}
