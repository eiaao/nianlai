use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::PathBuf,
    sync::mpsc,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct Logger {
    sender: mpsc::Sender<Message>,
    pub tier: u8,
}
enum Message {
    Event(Value),
    Flush(mpsc::SyncSender<()>),
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl Logger {
    pub fn new() -> io::Result<Self> {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let base = std::env::var_os("NIANLAI_G0_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| base.join("Nianlai").join("g0"));
        let open = |base: &std::path::Path| -> io::Result<File> {
            std::fs::create_dir_all(base)?;
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(base.join("panel-log.jsonl"))
        };
        // 指定数据目录不可写时，仍要让内存模式启动并留下不含路径的诊断。
        let mut file = open(&base).or_else(|_| {
            open(
                &std::env::temp_dir()
                    .join("NianlaiG0Logs")
                    .join(std::process::id().to_string()),
            )
        })?;
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("panel-log".into())
            .spawn(move || {
                while let Ok(message) = receiver.recv() {
                    match message {
                        Message::Event(value) => {
                            if let Err(error) =
                                writeln!(file, "{value}").and_then(|()| file.flush())
                            {
                                eprintln!("G0 日志写入失败: {error}");
                            }
                        }
                        Message::Flush(done) => {
                            let _ = file.flush();
                            let _ = done.send(());
                        }
                    }
                }
            })?;
        Ok(Self {
            sender,
            tier: if std::env::var("NIANLAI_MEMORY_TIER").as_deref() == Ok("1") {
                1
            } else {
                2
            },
        })
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
