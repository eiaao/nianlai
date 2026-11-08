// 测试钩子保留原名，优先级高于设置文件：
// NIANLAI_G0_DATA_DIR：隔离历史、导出及测试日志。
// NIANLAI_HOTKEY：矩阵工具指定呼出键。
// NIANLAI_G0_AUTOCYCLE、NIANLAI_G0_SAMPLES：自动循环和示例数据，仍由原模块读取。
// NIANLAI_G0_RESUSPEND_MIN：覆盖 memory.resuspend_minutes，支持小数分钟及 0（关闭）。
// 呼出键保存后即时重新注册；其余运行配置仍按各自的生效策略读取。

use crate::{hotkey, log::Logger};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tauri_plugin_global_shortcut::Shortcut;

const DEFAULT_HOTKEY: &str = "Ctrl+`";
const MIB: u64 = 1024 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
type Unknown = BTreeMap<String, Value>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub hotkey: String,
    pub theme: String,
    pub data_dir: Option<PathBuf>,
    pub memory: Memory,
    pub experiments: Experiments,
    pub log: Log,
    pub capture: Capture,
    #[serde(flatten)]
    unknown: Unknown,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: DEFAULT_HOTKEY.into(),
            theme: "system".into(),
            data_dir: None,
            memory: Memory::default(),
            experiments: Experiments::default(),
            log: Log::default(),
            capture: Capture::default(),
            unknown: Unknown::new(),
        }
    }
}

/// 默认排除的密码管理器（A07）。按进程名比较，不区分大小写。
pub const DEFAULT_EXCLUDED_APPS: [&str; 6] = [
    "KeePass.exe",
    "KeePassXC.exe",
    "Bitwarden.exe",
    "1Password.exe",
    "Enpass.exe",
    "RoboForm.exe",
];
pub const MAX_TEXT_KB: u64 = 2048;
const MAX_EXCLUDED_APPS: usize = 200;

/// 采集规则（PRD 6.1，A07–A11）。暂停和“忽略下一次”只在内存里，不在这里。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Capture {
    pub excluded_apps: Vec<String>,
    pub sensitive_off: Vec<String>,
    pub text: bool,
    pub rich_text: bool,
    pub images: bool,
    pub files: bool,
    pub max_text_kb: u64,
    pub transient: bool,
    pub transient_ms: u64,
    #[serde(flatten)]
    unknown: Unknown,
}

impl Default for Capture {
    fn default() -> Self {
        Self {
            excluded_apps: DEFAULT_EXCLUDED_APPS.map(String::from).to_vec(),
            sensitive_off: Vec::new(),
            text: true,
            rich_text: true,
            images: true,
            files: true,
            max_text_kb: MAX_TEXT_KB,
            transient: true,
            transient_ms: 1000,
            unknown: Unknown::new(),
        }
    }
}

fn valid_app(value: &Value) -> bool {
    // 只存进程名（如 KeePass.exe），不收路径；前后空格会让比较永远对不上，直接视为无效。
    value.as_str().is_some_and(|name| {
        !name.is_empty()
            && name.len() <= 260
            && name.trim() == name
            && !name.contains(['\\', '/', '\0'])
    })
}

fn string_list(value: &Value, max: usize, valid: impl Fn(&Value) -> bool) -> bool {
    value
        .as_array()
        .is_some_and(|items| items.len() <= max && items.iter().all(valid))
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Memory {
    pub resuspend_minutes: f64,
    #[serde(flatten)]
    unknown: Unknown,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Experiments {
    pub searchbox_focus: bool,
    #[serde(flatten)]
    unknown: Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Log {
    pub max_total_mb: u64,
    #[serde(flatten)]
    unknown: Unknown,
}

impl Default for Log {
    fn default() -> Self {
        Self {
            max_total_mb: 20,
            unknown: Unknown::new(),
        }
    }
}

pub struct Issue {
    field: String,
    reason: &'static str,
}

impl Issue {
    fn new(field: &str, reason: &'static str) -> Self {
        Self {
            field: field.into(),
            reason,
        }
    }

    pub fn record(&self, logger: &Logger) {
        // 不记录原值或解析器错误，避免任意输入进入日志。
        logger.event(
            "settings_invalid",
            json!({"field":self.field,"reason":self.reason}),
        );
    }
}

pub fn parse_hotkey(value: &str) -> Result<Shortcut, String> {
    hotkey::parse(value)
}

fn valid_path(value: &str) -> bool {
    !value.contains('\0') && Path::new(value).is_absolute()
}

fn valid_minutes(minutes: f64) -> bool {
    if !minutes.is_finite() || minutes < 0.0 {
        return false;
    }
    if minutes == 0.0 {
        return true;
    }
    Duration::try_from_secs_f64(minutes * 60.0)
        .ok()
        .is_some_and(|duration| {
            !duration.is_zero() && Instant::now().checked_add(duration).is_some()
        })
}

fn field(
    object: &mut Map<String, Value>,
    name: &str,
    path: &str,
    default: Value,
    valid: impl FnOnce(&Value) -> bool,
    issues: &mut Vec<Issue>,
) {
    match object.get(name) {
        Some(value) if valid(value) => return,
        Some(_) => issues.push(Issue::new(path, "字段值无效，已使用默认值")),
        None => {}
    }
    object.insert(name.into(), default);
}

fn section<'a>(
    object: &'a mut Map<String, Value>,
    name: &str,
    issues: &mut Vec<Issue>,
) -> &'a mut Map<String, Value> {
    if !object.get(name).is_some_and(Value::is_object) {
        if object.contains_key(name) {
            issues.push(Issue::new(name, "设置分组必须是对象，已使用默认值"));
        }
        object.insert(name.into(), json!({}));
    }
    object.get_mut(name).unwrap().as_object_mut().unwrap()
}

fn validate(mut value: Value) -> (Settings, Vec<Issue>) {
    let mut issues = Vec::new();
    if !value.is_object() {
        issues.push(Issue::new("settings", "设置必须是对象，已使用默认值"));
        value = json!({});
    }
    let object = value.as_object_mut().unwrap();
    field(
        object,
        "hotkey",
        "hotkey",
        json!(DEFAULT_HOTKEY),
        |v| v.as_str().is_some_and(|s| parse_hotkey(s).is_ok()),
        &mut issues,
    );
    field(
        object,
        "theme",
        "theme",
        json!("system"),
        |v| matches!(v.as_str(), Some("system" | "light" | "dark")),
        &mut issues,
    );
    field(
        object,
        "data_dir",
        "data_dir",
        Value::Null,
        |v| v.is_null() || v.as_str().is_some_and(valid_path),
        &mut issues,
    );
    field(
        section(object, "memory", &mut issues),
        "resuspend_minutes",
        "memory.resuspend_minutes",
        json!(0.0),
        |v| v.as_f64().is_some_and(valid_minutes),
        &mut issues,
    );
    field(
        section(object, "experiments", &mut issues),
        "searchbox_focus",
        "experiments.searchbox_focus",
        json!(false),
        Value::is_boolean,
        &mut issues,
    );
    field(
        section(object, "log", &mut issues),
        "max_total_mb",
        "log.max_total_mb",
        json!(20),
        |v| v.as_u64().is_some_and(|n| n > 0 && n <= u64::MAX / MIB),
        &mut issues,
    );
    let capture = section(object, "capture", &mut issues);
    field(
        capture,
        "excluded_apps",
        "capture.excluded_apps",
        json!(DEFAULT_EXCLUDED_APPS),
        |v| string_list(v, MAX_EXCLUDED_APPS, valid_app),
        &mut issues,
    );
    field(
        capture,
        "sensitive_off",
        "capture.sensitive_off",
        json!([]),
        |v| {
            string_list(v, 64, |id| {
                id.as_str()
                    .is_some_and(|id| !id.is_empty() && id.len() <= 64)
            })
        },
        &mut issues,
    );
    for name in ["text", "rich_text", "images", "files", "transient"] {
        field(
            capture,
            name,
            &format!("capture.{name}"),
            json!(true),
            Value::is_boolean,
            &mut issues,
        );
    }
    field(
        capture,
        "max_text_kb",
        "capture.max_text_kb",
        json!(MAX_TEXT_KB),
        |v| v.as_u64().is_some_and(|n| (1..=MAX_TEXT_KB).contains(&n)),
        &mut issues,
    );
    field(
        capture,
        "transient_ms",
        "capture.transient_ms",
        json!(1000),
        |v| v.as_u64().is_some_and(|n| (200..=5000).contains(&n)),
        &mut issues,
    );
    // 已逐项规范所有已知字段；未知字段由各层 flatten 原样接收。
    (serde_json::from_value(value).unwrap(), issues)
}

fn merge(base: &mut Value, update: Value) {
    if let (Some(base), Value::Object(update)) = (base.as_object_mut(), &update) {
        for (key, value) in update {
            merge(
                base.entry(key.clone()).or_insert(Value::Null),
                value.clone(),
            );
        }
    } else {
        *base = update;
    }
}

fn atomic_write(path: &Path, settings: &Settings) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("设置文件缺少父目录"))?;
    fs::create_dir_all(parent)?;
    let (temporary, mut file) = loop {
        let temporary = parent.join(format!(
            ".settings-{}-{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => break (temporary, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    let result = (|| {
        serde_json::to_writer_pretty(&mut file, settings)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        // 同目录改名替换；不先删除旧文件，失败时旧设置仍可读。
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

pub struct Store {
    path: PathBuf,
    settings: Mutex<Settings>,
}

impl Store {
    pub fn load(path: PathBuf) -> (Self, Vec<Issue>) {
        let (settings, issues) = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(value) => validate(value),
                Err(_) => (
                    Settings::default(),
                    vec![Issue::new(
                        "settings",
                        "设置文件不是有效 JSON，已使用默认值",
                    )],
                ),
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let settings = Settings::default();
                let issues = if atomic_write(&path, &settings).is_err() {
                    vec![Issue::new(
                        "settings",
                        "默认设置文件创建失败，本次使用默认值",
                    )]
                } else {
                    Vec::new()
                };
                (settings, issues)
            }
            Err(_) => (
                Settings::default(),
                vec![Issue::new("settings", "设置文件读取失败，本次使用默认值")],
            ),
        };
        (
            Self {
                path,
                settings: Mutex::new(settings),
            },
            issues,
        )
    }

    pub fn get(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub fn set(&self, update: Value) -> Result<(Settings, Vec<Issue>), String> {
        if !update.is_object() {
            return Err("设置必须是 JSON 对象".into());
        }
        if let Some(value) = update.get("hotkey") {
            // 加载旧文件仍逐字段降级；主动保存非法组合则拒绝整个更新，不能静默改回默认键。
            parse_hotkey(value.as_str().ok_or("呼出快捷键必须是字符串")?)?;
        }
        let mut current = self.settings.lock().unwrap();
        let mut value = serde_json::to_value(&*current).unwrap();
        merge(&mut value, update);
        let (settings, issues) = validate(value);
        atomic_write(&self.path, &settings).map_err(|_| "设置文件写入失败，原设置已保留")?;
        *current = settings.clone();
        Ok((settings, issues))
    }
}

#[derive(Clone)]
pub struct Directories {
    pub settings_file: PathBuf,
    pub data: PathBuf,
    pub logs: PathBuf,
    pub data_source: &'static str,
    pub test_data_dir: bool,
}

pub fn settings_path() -> PathBuf {
    std::env::var_os("APPDATA")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Nianlai/settings.json")
}

pub struct Resolved {
    pub settings: Settings,
    pub directories: Directories,
    pub issues: Vec<Issue>,
}

impl Resolved {
    pub fn new(settings: Settings, settings_file: PathBuf) -> Self {
        let local = std::env::var_os("LOCALAPPDATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        Self::resolve(
            settings,
            settings_file,
            &local,
            std::env::var_os("NIANLAI_G0_DATA_DIR"),
            std::env::var_os("NIANLAI_HOTKEY"),
            std::env::var_os("NIANLAI_G0_RESUSPEND_MIN"),
        )
    }

    fn resolve(
        mut settings: Settings,
        settings_file: PathBuf,
        local: &Path,
        data: Option<OsString>,
        hotkey: Option<OsString>,
        resuspend: Option<OsString>,
    ) -> Self {
        let mut issues = Vec::new();
        if let Some(value) = hotkey {
            settings.hotkey = match value.to_str().filter(|v| parse_hotkey(v).is_ok()) {
                Some(value) => value.into(),
                None => {
                    issues.push(Issue::new("NIANLAI_HOTKEY", "测试快捷键无效，已使用默认值"));
                    DEFAULT_HOTKEY.into()
                }
            };
        }
        if let Some(value) = resuspend {
            settings.memory.resuspend_minutes = match value
                .to_str()
                .and_then(|v| v.trim().parse::<f64>().ok())
                .filter(|v| valid_minutes(*v))
            {
                Some(value) => value,
                None => {
                    issues.push(Issue::new(
                        "NIANLAI_G0_RESUSPEND_MIN",
                        "测试重挂起间隔无效，已关闭重挂起",
                    ));
                    0.0
                }
            };
        }
        let default_data = local.join("Nianlai/data");
        let (data, data_source, test_data_dir) = match data {
            Some(value) if value.to_str().is_some_and(valid_path) => {
                (PathBuf::from(value), "环境变量指定", true)
            }
            Some(_) => {
                issues.push(Issue::new(
                    "NIANLAI_G0_DATA_DIR",
                    "测试数据目录无效，已使用默认目录",
                ));
                (default_data, "默认位置", false)
            }
            None => match &settings.data_dir {
                Some(value) => (value.clone(), "设置文件指定", false),
                None => (default_data, "默认位置", false),
            },
        };
        let logs = if test_data_dir {
            data.clone()
        } else {
            local.join("Nianlai/logs")
        };
        Self {
            settings,
            directories: Directories {
                settings_file,
                data,
                logs,
                data_source,
                test_data_dir,
            },
            issues,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub struct Directory(pub PathBuf);

    impl Directory {
        pub fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "nianlai-settings-test-{}-{}-{}",
                std::process::id(),
                crate::log::now_ms(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn creates_defaults_then_reads_and_updates_settings() {
        let directory = Directory::new();
        let path = directory.0.join("Nianlai/settings.json");
        let (store, issues) = Store::load(path.clone());
        assert!(issues.is_empty());
        assert_eq!(store.get(), Settings::default());
        let defaults = serde_json::to_value(store.get()).unwrap();
        assert_eq!(defaults["hotkey"], "Ctrl+`");
        assert_eq!(defaults["theme"], "system");
        assert!(defaults["data_dir"].is_null());
        assert_eq!(defaults["memory"]["resuspend_minutes"].as_f64(), Some(0.0));
        assert_eq!(defaults["experiments"]["searchbox_focus"], false);
        assert_eq!(defaults["log"]["max_total_mb"], 20);
        assert!(parse_hotkey(&store.get().hotkey).is_ok());
        let (updated, issues) = store
            .set(
                json!({"hotkey":"Alt+V","theme":"dark","data_dir":directory.0,
                "memory":{"resuspend_minutes":0.5},"experiments":{"searchbox_focus":true},
                "log":{"max_total_mb":3}}),
            )
            .unwrap();
        assert!(issues.is_empty());
        let (loaded, issues) = Store::load(path);
        assert!(issues.is_empty());
        assert_eq!(loaded.get(), updated);
        assert_eq!(updated.memory.resuspend_minutes, 0.5);
        assert!(updated.experiments.searchbox_focus);
    }

    #[test]
    fn capture_rules_validate_each_field() {
        let (settings, issues) = validate(json!({}));
        assert!(issues.is_empty());
        assert_eq!(settings.capture, Capture::default());
        assert!(
            settings
                .capture
                .excluded_apps
                .iter()
                .any(|app| app == "KeePass.exe")
        );
        let (settings, issues) = validate(json!({"capture":{
            "excluded_apps":["Code.exe"], "sensitive_off":["jwt"], "rich_text":false,
            "max_text_kb":64, "transient":false, "transient_ms":1500
        }}));
        assert!(issues.is_empty());
        assert_eq!(settings.capture.excluded_apps, ["Code.exe"]);
        assert_eq!(settings.capture.sensitive_off, ["jwt"]);
        assert!(!settings.capture.rich_text && settings.capture.text);
        assert_eq!(
            (settings.capture.max_text_kb, settings.capture.transient_ms),
            (64, 1500)
        );
        for apps in [
            json!(["C:\\Tools\\a.exe"]),
            json!([" a.exe"]),
            json!([""]),
            json!("a.exe"),
            json!([1]),
        ] {
            let (settings, issues) = validate(json!({"capture":{"excluded_apps":apps}}));
            assert_eq!(
                settings.capture.excluded_apps,
                Capture::default().excluded_apps
            );
            assert_eq!(issues.len(), 1);
        }
        let (settings, issues) =
            validate(json!({"capture":{"max_text_kb":0,"transient_ms":100,"images":"no"}}));
        assert_eq!(issues.len(), 3);
        assert_eq!(settings.capture, Capture::default());
    }

    #[test]
    fn invalid_fields_fall_back_individually_without_losing_valid_fields() {
        let (settings, issues) = validate(json!({
            "hotkey":"Ctrl+Alt+Shift+Q", "theme":"dark", "data_dir":"relative",
            "memory":{"resuspend_minutes":-1}, "experiments":{"searchbox_focus":"1"},
            "log":{"max_total_mb":0}
        }));
        assert_eq!(issues.len(), 5);
        assert_eq!(settings.hotkey, DEFAULT_HOTKEY);
        assert_eq!(settings.theme, "dark");
        assert!(settings.data_dir.is_none());
        assert_eq!(settings.memory.resuspend_minutes, 0.0);
        assert!(!settings.experiments.searchbox_focus);
        assert_eq!(settings.log.max_total_mb, 20);
        for value in [Value::Null, json!([]), json!("正文不应进入诊断")] {
            let (settings, issues) = validate(value);
            assert_eq!(settings, Settings::default());
            assert_eq!(issues.len(), 1);
        }
        for value in [
            json!(null),
            json!(true),
            json!("20"),
            json!(-1),
            json!(1.5),
            json!(u64::MAX),
        ] {
            let (settings, issues) = validate(json!({"log":{"max_total_mb":value}}));
            assert_eq!(settings.log.max_total_mb, 20);
            assert_eq!(issues.len(), 1);
        }
        for value in [json!(-1), json!("15"), json!(1e300), json!(1e-300)] {
            let (settings, issues) = validate(json!({"memory":{"resuspend_minutes":value}}));
            assert_eq!(settings.memory.resuspend_minutes, 0.0);
            assert_eq!(issues.len(), 1);
        }
        let (settings, issues) =
            validate(json!({"theme":[],"memory":false,"experiments":[],"log":null}));
        assert_eq!(settings, Settings::default());
        assert_eq!(issues.len(), 4);
        assert!(parse_hotkey("Ctrl+Alt+Shift+L").is_err());
        assert!(parse_hotkey("正文不应进入诊断").is_err());
    }

    #[test]
    fn unknown_fields_survive_load_partial_update_and_atomic_replacement() {
        let directory = Directory::new();
        let path = directory.0.join("settings.json");
        let unknown = json!({"nested":[1,null,{"中文":"保留"}]});
        fs::write(
            &path,
            json!({"future":unknown,"memory":{"future":unknown},
            "experiments":{"future":unknown},"log":{"future":unknown}})
            .to_string(),
        )
        .unwrap();
        let (store, issues) = Store::load(path.clone());
        assert!(issues.is_empty());
        store
            .set(json!({"theme":"light","memory":{"resuspend_minutes":15},
            "experiments":{"searchbox_focus":true},"log":{"max_total_mb":2}}))
            .unwrap();
        let value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["future"], unknown);
        for section in ["memory", "experiments", "log"] {
            assert_eq!(value[section]["future"], unknown);
        }
        assert_eq!(value["theme"], "light");
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
        let (reloaded, issues) = Store::load(path);
        assert!(issues.is_empty());
        assert_eq!(reloaded.get(), store.get());
    }

    #[test]
    fn malformed_or_unreadable_file_uses_defaults_and_reports_without_input() {
        let directory = Directory::new();
        let path = directory.0.join("settings.json");
        fs::write(&path, b"{broken").unwrap();
        let (store, issues) = Store::load(path.clone());
        assert_eq!(store.get(), Settings::default());
        assert_eq!(issues.len(), 1);
        assert!(!issues[0].reason.contains("broken"));
        assert_eq!(fs::read(&path).unwrap(), b"{broken");
        let (_, issues) = Store::load(directory.0.clone());
        assert_eq!(issues.len(), 1);
        let (_, issues) = Store::load(path.join("cannot-create.json"));
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn invalid_settings_diagnostics_never_write_input_values_to_logs() {
        let directory = Directory::new();
        let path = directory.0.join("settings.json");
        let private = "设置原值隐私哨兵-7da493";
        fs::write(
            &path,
            json!({"hotkey":private,"theme":private,"data_dir":private,
                "memory":{"resuspend_minutes":private},
                "experiments":{"searchbox_focus":private},"log":{"max_total_mb":private}})
            .to_string(),
        )
        .unwrap();
        let (store, mut issues) = Store::load(path.clone());
        assert_eq!(store.get(), Settings::default());
        assert_eq!(issues.len(), 6);
        // JSON 解析器的错误也不能把损坏文件的输入带进诊断。
        fs::write(&path, format!("{{{private}")).unwrap();
        issues.extend(Store::load(path.clone()).1);
        let directories = Directories {
            settings_file: path,
            data: directory.0.join("data"),
            logs: directory.0.join("logs"),
            data_source: "默认位置",
            test_data_dir: true,
        };
        let logger = Logger::with_settings(&directories, 20).unwrap();
        for issue in issues {
            issue.record(&logger);
        }
        logger.flush();
        let text =
            fs::read_to_string(directories.logs.join(crate::log::COMPATIBILITY_FILE)).unwrap();
        assert!(!text.contains(private));
        let rows: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.len(), 7);
        assert!(rows.iter().all(|row| row["event"] == "settings_invalid"));
        assert!(rows.iter().all(|row| row["field"].is_string()));
    }

    #[test]
    fn failed_atomic_replacement_keeps_previous_file_and_memory() {
        let directory = Directory::new();
        let path = directory.0.join("settings.json");
        let (store, _) = Store::load(path.clone());
        let original = fs::read(&path).unwrap();
        assert!(store.set(json!(null)).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        fs::write(path.join("sentinel"), b"old").unwrap();
        assert!(store.set(json!({"theme":"dark"})).is_err());
        assert_eq!(store.get(), Settings::default());
        assert_eq!(fs::read(path.join("sentinel")).unwrap(), b"old");
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    }

    #[test]
    fn concurrent_reader_only_observes_complete_old_or_new_settings() {
        use std::sync::{Barrier, atomic::AtomicBool};

        let directory = Directory::new();
        let path = directory.0.join("settings.json");
        let (store, _) = Store::load(path.clone());
        let barrier = Barrier::new(2);
        let done = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let reader = scope.spawn(|| {
                let mut reads = 0;
                barrier.wait();
                while !done.load(Ordering::Acquire) || reads == 0 {
                    let value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                    assert!(matches!(
                        value["theme"].as_str(),
                        Some("system" | "light" | "dark")
                    ));
                    assert_eq!(value["log"]["max_total_mb"], 20);
                    reads += 1;
                }
                reads
            });
            barrier.wait();
            for index in 0..20 {
                store
                    .set(json!({"theme":if index % 2 == 0 {"light"} else {"dark"}}))
                    .unwrap();
            }
            done.store(true, Ordering::Release);
            assert!(reader.join().unwrap() > 0);
        });
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    }

    #[test]
    fn test_overrides_win_without_changing_persisted_settings() {
        let directory = Directory::new();
        let settings = validate(
            json!({"hotkey":"Alt+V","data_dir":directory.0.join("custom"),
            "memory":{"resuspend_minutes":15}}),
        )
        .0;
        let resolve = |data, hotkey, minutes| {
            Resolved::resolve(
                settings.clone(),
                directory.0.join("settings.json"),
                &directory.0,
                data,
                hotkey,
                minutes,
            )
        };
        let resolved = resolve(None, None, None);
        assert_eq!(resolved.directories.data, directory.0.join("custom"));
        assert_eq!(resolved.directories.logs, directory.0.join("Nianlai/logs"));
        assert_eq!(resolved.settings.memory.resuspend_minutes, 15.0);
        let resolved = resolve(
            Some(directory.0.join("test").into_os_string()),
            Some("Ctrl+Shift+F11".into()),
            Some("0.5".into()),
        );
        assert!(resolved.issues.is_empty());
        assert!(resolved.directories.test_data_dir);
        assert_eq!(resolved.directories.logs, directory.0.join("test"));
        assert_eq!(resolved.directories.data, resolved.directories.logs);
        assert_eq!(resolved.settings.hotkey, "Ctrl+Shift+F11");
        assert_eq!(resolved.settings.memory.resuspend_minutes, 0.5);
        assert_eq!(settings.hotkey, "Alt+V");
        assert_eq!(settings.memory.resuspend_minutes, 15.0);
        assert_eq!(
            resolve(None, None, Some("0".into()))
                .settings
                .memory
                .resuspend_minutes,
            0.0
        );
        for value in ["NaN", "inf", "-1", "正文不应进入诊断"] {
            let invalid = resolve(
                Some("relative".into()),
                Some(value.into()),
                Some(value.into()),
            );
            assert_eq!(invalid.issues.len(), 3);
            assert_eq!(invalid.settings.hotkey, DEFAULT_HOTKEY);
            assert_eq!(invalid.settings.memory.resuspend_minutes, 0.0);
            assert_eq!(invalid.directories.data, directory.0.join("Nianlai/data"));
        }
        let defaults = Resolved::resolve(
            Settings::default(),
            directory.0.join("settings.json"),
            &directory.0,
            None,
            None,
            None,
        );
        assert_eq!(defaults.directories.data, directory.0.join("Nianlai/data"));
        assert!(!defaults.directories.data.exists());
        assert!(!defaults.directories.test_data_dir);
    }
}
