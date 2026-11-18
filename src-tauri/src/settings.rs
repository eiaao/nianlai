// 测试钩子保留原名，优先级高于设置文件：
// NIANLAI_G0_DATA_DIR：隔离历史、导出及测试日志。
// NIANLAI_HOTKEY：矩阵工具指定呼出键。
// NIANLAI_G0_AUTOCYCLE、NIANLAI_G0_SAMPLES：自动循环和示例数据，仍由原模块读取。
// NIANLAI_G0_RESUSPEND_MIN：覆盖 memory.resuspend_minutes，支持小数分钟及 0（关闭）。
// 呼出键保存后即时重新注册；其余运行配置仍按各自的生效策略读取。

use crate::{log::Logger, shortcut};
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
// 设置同时经 TypeScript number 使用，超出安全整数会让样本 ID 和计数失真。
const MAX_SAFE_JSON_INT: u64 = 9_007_199_254_740_991;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
type Unknown = BTreeMap<String, Value>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub hotkey: String,
    pub theme: String,
    pub data_dir: Option<PathBuf>,
    pub memory: Memory,
    pub log: Log,
    pub capture: Capture,
    pub snapshots: Snapshots,
    pub interaction: Interaction,
    pub history: History,
    pub guide: Guide,
    pub update: UpdateSettings,
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
            log: Log::default(),
            capture: Capture::default(),
            snapshots: Snapshots::default(),
            interaction: Interaction::default(),
            history: History::default(),
            guide: Guide::default(),
            update: UpdateSettings::default(),
            unknown: Unknown::new(),
        }
    }
}

/// 新版本检测（见 update 模块）。`auto_check` 是用户开关；其余是程序自己维护的状态：
/// 最近一次检查、已知的最新版本，以及每个版本的“已通知 / 已跳过 / 已查看 / 横幅已关闭”标记。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UpdateSettings {
    pub auto_check: bool,
    pub last_check_ms: Option<u64>,
    pub last_result: crate::update::policy::CheckResult,
    pub latest: Option<crate::update::feed::Latest>,
    pub notified_version: Option<String>,
    pub skipped_version: Option<String>,
    pub seen_version: Option<String>,
    pub banner_dismissed_version: Option<String>,
    #[serde(flatten)]
    unknown: Unknown,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self {
            auto_check: true,
            last_check_ms: None,
            last_result: Default::default(),
            latest: None,
            notified_version: None,
            skipped_version: None,
            seen_version: None,
            banner_dismissed_version: None,
            unknown: Unknown::new(),
        }
    }
}

impl UpdateSettings {
    /// 程序维护的那部分状态，交给更新模块。
    pub fn state(&self) -> crate::update::policy::State {
        crate::update::policy::State {
            last_check_ms: self.last_check_ms,
            last_result: self.last_result,
            latest: self.latest.clone(),
            notified_version: self.notified_version.clone(),
            skipped_version: self.skipped_version.clone(),
            seen_version: self.seen_version.clone(),
            banner_dismissed_version: self.banner_dismissed_version.clone(),
        }
    }
}

/// 默认排除的密码管理器（A07）。按进程名比较，不区分大小写。
#[cfg(windows)]
pub const DEFAULT_EXCLUDED_APPS: [&str; 6] = [
    "KeePass.exe",
    "KeePassXC.exe",
    "Bitwarden.exe",
    "1Password.exe",
    "Enpass.exe",
    "RoboForm.exe",
];
#[cfg(target_os = "macos")]
pub const DEFAULT_EXCLUDED_APPS: [&str; 5] = [
    "1Password",
    "Bitwarden",
    "KeePassXC",
    "com.apple.keychainaccess",
    "com.apple.Passwords",
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

/// 自动快照（B18）占用的空间上限，单位 MB。份数固定为 7。
pub const SNAPSHOT_MAX_MB: u64 = 1024;
const SNAPSHOT_MB_RANGE: std::ops::RangeInclusive<u64> = 64..=102_400;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshots {
    pub max_mb: u64,
    #[serde(flatten)]
    unknown: Unknown,
}

impl Default for Snapshots {
    fn default() -> Self {
        Self {
            max_mb: SNAPSHOT_MAX_MB,
            unknown: Unknown::new(),
        }
    }
}

/// 历史（B10）：reuse_to_top 为真时，粘贴旧条目会把它移到列表最前；为假则保持原位。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct History {
    pub reuse_to_top: bool,
    #[serde(flatten)]
    unknown: Unknown,
}

impl Default for History {
    fn default() -> Self {
        Self {
            reuse_to_top: true,
            unknown: Unknown::new(),
        }
    }
}

// 首次启动只保存流程进度；运行时的面板 session 和待保存错误由前端状态机单独持有。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Guide {
    pub version: u8,
    pub phase: GuidePhase,
    pub baseline_ms: Option<u64>,
    pub baseline_total: u64,
    pub sample_id: Option<u64>,
    pub cards_dealt: bool,
    pub dismissed: Vec<GuideCardId>,
    #[serde(flatten)]
    unknown: Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuidePhase {
    AwaitCopy,
    AwaitShow,
    AwaitPaste,
    Completed,
    Skipped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GuideCardId {
    T1,
    T2,
    T3,
    T4,
}

impl Default for Guide {
    fn default() -> Self {
        Self {
            version: 1,
            phase: GuidePhase::AwaitCopy,
            baseline_ms: None,
            baseline_total: 0,
            sample_id: None,
            cards_dealt: false,
            dismissed: Vec::new(),
            unknown: Unknown::new(),
        }
    }
}

/// 交互（D12、G09）：面板呼出位置，以及鼠标单击 / 中键点击条目时做什么。
/// placement：caret 输入光标旁（找不到光标退回鼠标旁）/ mouse 鼠标旁 / center 屏幕中央。
/// click、middle_click：paste 粘贴 / copy 只复制 / text 纯文本粘贴 / none 不做额外操作。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Interaction {
    pub placement: String,
    pub click: String,
    pub middle_click: String,
    #[serde(flatten)]
    unknown: Unknown,
}

impl Default for Interaction {
    fn default() -> Self {
        Self {
            placement: "caret".into(),
            click: "paste".into(),
            middle_click: "text".into(),
            unknown: Unknown::new(),
        }
    }
}

fn valid_click(value: &Value) -> bool {
    matches!(value.as_str(), Some("paste" | "copy" | "text" | "none"))
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
    shortcut::parse(value)
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
    field(
        section(object, "snapshots", &mut issues),
        "max_mb",
        "snapshots.max_mb",
        json!(SNAPSHOT_MAX_MB),
        |v| v.as_u64().is_some_and(|n| SNAPSHOT_MB_RANGE.contains(&n)),
        &mut issues,
    );
    let interaction = section(object, "interaction", &mut issues);
    field(
        interaction,
        "placement",
        "interaction.placement",
        json!("caret"),
        |v| matches!(v.as_str(), Some("caret" | "mouse" | "center")),
        &mut issues,
    );
    field(
        interaction,
        "click",
        "interaction.click",
        json!("paste"),
        valid_click,
        &mut issues,
    );
    field(
        interaction,
        "middle_click",
        "interaction.middle_click",
        json!("text"),
        valid_click,
        &mut issues,
    );
    field(
        section(object, "history", &mut issues),
        "reuse_to_top",
        "history.reuse_to_top",
        json!(true),
        Value::is_boolean,
        &mut issues,
    );
    let update = section(object, "update", &mut issues);
    field(
        update,
        "auto_check",
        "update.auto_check",
        json!(true),
        Value::is_boolean,
        &mut issues,
    );
    field(
        update,
        "last_check_ms",
        "update.last_check_ms",
        Value::Null,
        |v| v.is_null() || v.as_u64().is_some_and(|n| n <= MAX_SAFE_JSON_INT),
        &mut issues,
    );
    field(
        update,
        "last_result",
        "update.last_result",
        json!("none"),
        |v| matches!(v.as_str(), Some("none" | "latest" | "new" | "failed")),
        &mut issues,
    );
    for name in [
        "notified_version",
        "skipped_version",
        "seen_version",
        "banner_dismissed_version",
    ] {
        field(
            update,
            name,
            &format!("update.{name}"),
            Value::Null,
            |v| v.is_null() || v.as_str().is_some_and(|s| s.len() <= 64),
            &mut issues,
        );
    }
    // 缓存的最新版本必须通过与版本文件相同的校验（地址、sha256、版本号）；不合格就丢弃，等下次检查重取。
    let latest = update.get("latest").cloned().unwrap_or(Value::Null);
    let latest = serde_json::from_value::<crate::update::feed::Latest>(latest.clone())
        .ok()
        .and_then(|l| l.sanitize(crate::update::feed::PACKAGE_EXT));
    match latest {
        Some(latest) => {
            update.insert("latest".into(), serde_json::to_value(latest).unwrap());
        }
        None => {
            if update.get("latest").is_some_and(|v| !v.is_null()) {
                issues.push(Issue::new("update.latest", "缓存的新版本信息无效，已丢弃"));
            }
            update.insert("latest".into(), Value::Null);
        }
    }
    let guide = section(object, "guide", &mut issues);
    field(
        guide,
        "version",
        "guide.version",
        json!(1),
        |v| v.as_u64() == Some(1),
        &mut issues,
    );
    field(
        guide,
        "phase",
        "guide.phase",
        json!("await_copy"),
        |v| {
            matches!(
                v.as_str(),
                Some("await_copy" | "await_show" | "await_paste" | "completed" | "skipped")
            )
        },
        &mut issues,
    );
    field(
        guide,
        "baseline_ms",
        "guide.baseline_ms",
        Value::Null,
        |v| v.is_null() || v.as_u64().is_some_and(|n| n <= MAX_SAFE_JSON_INT),
        &mut issues,
    );
    field(
        guide,
        "baseline_total",
        "guide.baseline_total",
        json!(0),
        |v| v.as_u64().is_some_and(|n| n <= MAX_SAFE_JSON_INT),
        &mut issues,
    );
    field(
        guide,
        "sample_id",
        "guide.sample_id",
        Value::Null,
        |v| v.is_null() || v.as_u64().is_some_and(|n| n > 0 && n <= MAX_SAFE_JSON_INT),
        &mut issues,
    );
    field(
        guide,
        "cards_dealt",
        "guide.cards_dealt",
        json!(false),
        Value::is_boolean,
        &mut issues,
    );
    match guide.get("dismissed").cloned() {
        None => {
            guide.insert("dismissed".into(), json!([]));
        }
        Some(Value::Array(items)) => {
            let mut dismissed = Vec::new();
            let mut invalid = false;
            for item in items {
                let id = match item.as_str() {
                    Some("t1") => "t1",
                    Some("t2") => "t2",
                    Some("t3") => "t3",
                    Some("t4") => "t4",
                    _ => {
                        invalid = true;
                        continue;
                    }
                };
                if !dismissed.contains(&id) {
                    dismissed.push(id);
                }
            }
            guide.insert("dismissed".into(), json!(dismissed));
            if invalid {
                issues.push(Issue::new(
                    "guide.dismissed",
                    "含有不认识的新手牌 ID，已忽略",
                ));
            }
        }
        Some(_) => {
            issues.push(Issue::new("guide.dismissed", "字段值无效，已使用默认值"));
            guide.insert("dismissed".into(), json!([]));
        }
    }
    let waiting_for_sample = matches!(
        guide.get("phase").and_then(Value::as_str),
        Some("await_show" | "await_paste")
    );
    if waiting_for_sample && guide.get("sample_id").is_some_and(Value::is_null) {
        // 等待真实样本的两拍不能在没有 ID 时恢复；退回复制，不让迟到结果误完成。
        guide.insert("phase".into(), json!("await_copy"));
        issues.push(Issue::new(
            "guide.phase",
            "等待中的样本 ID 无效，已回到复制步骤",
        ));
    }
    let terminal = matches!(
        guide.get("phase").and_then(Value::as_str),
        Some("completed" | "skipped")
    );
    if !terminal && guide.get("cards_dealt").and_then(Value::as_bool) == Some(true) {
        // phase 校验和缺失样本回退之后统一收尾：只有终态可以显式重发新手牌。
        guide.insert("cards_dealt".into(), json!(false));
        issues.push(Issue::new(
            "guide.cards_dealt",
            "进行中的引导不能保留已发牌标记，已关闭",
        ));
    }
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
            Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(value) => {
                    // 必须在默认值填充之前看原始 JSON：旧文件缺 guide 与新安装不是一回事。
                    let had_guide = value.get("guide").is_some();
                    let (mut settings, issues) = validate(value);
                    if !had_guide {
                        settings.guide.phase = GuidePhase::Completed;
                    }
                    (settings, issues)
                }
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
            parse_hotkey(value.as_str().ok_or("呼出键必须是字符串")?)?;
        }
        let mut current = self.settings.lock().unwrap();
        let mut value = serde_json::to_value(&*current).unwrap();
        merge(&mut value, update);
        let (settings, issues) = validate(value);
        atomic_write(&self.path, &settings).map_err(|_| "没保存上，还是原来的设置")?;
        *current = settings.clone();
        Ok((settings, issues))
    }
}

pub use crate::directories::Directories;

/// 保存设置的顺序：校验并原子写盘 → 含呼出键时换键 → 广播。
/// 校验或写盘失败直接返回，不换键也不广播（原设置保留）；换键失败不回滚，由 `apply_hotkey` 自己记录状态。
pub fn save(
    store: &Store,
    update: Value,
    apply_hotkey: impl FnOnce(&Settings),
    changed: impl FnOnce(&Settings),
) -> Result<(Settings, Vec<Issue>), String> {
    let update_hotkey = update.get("hotkey").is_some();
    let (saved, issues) = store.set(update)?;
    if update_hotkey {
        apply_hotkey(&saved);
    }
    changed(&saved);
    Ok((saved, issues))
}

/// 按设置文件打开日志；单实例通知可能早于主启动流程，托盘走这个无参入口。
pub fn open_logger() -> io::Result<Logger> {
    let path = settings_path();
    let (store, issues) = Store::load(path.clone());
    let resolved = Resolved::new(store.get(), path);
    let logger = Logger::with_settings(&resolved.directories, resolved.settings.log.max_total_mb)?;
    for issue in issues.iter().chain(&resolved.issues) {
        issue.record(&logger);
    }
    Ok(logger)
}

#[cfg(windows)]
pub fn settings_path() -> PathBuf {
    std::env::var_os("APPDATA")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Nianlai/settings.json")
}

#[cfg(target_os = "macos")]
fn mac_library() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Library")
}
#[cfg(target_os = "macos")]
pub fn settings_path() -> PathBuf {
    mac_library().join("Application Support/Nianlai/settings.json")
}

pub struct Resolved {
    pub settings: Settings,
    pub directories: Directories,
    pub issues: Vec<Issue>,
    // 环境变量给出了有效的呼出键并已覆盖设置；无效值不算，沿用设置文件里的键。
    pub hotkey_overridden: bool,
}

impl Resolved {
    pub fn new(settings: Settings, settings_file: PathBuf) -> Self {
        #[cfg(windows)]
        let local = std::env::var_os("LOCALAPPDATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        #[cfg(target_os = "macos")]
        let local = mac_library();
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
        let mut hotkey_overridden = false;
        if let Some(value) = hotkey {
            match value.to_str().filter(|v| parse_hotkey(v).is_ok()) {
                Some(value) => {
                    settings.hotkey = value.into();
                    hotkey_overridden = true;
                }
                None => {
                    issues.push(Issue::new(
                        "NIANLAI_HOTKEY",
                        "测试快捷键无效，已沿用设置里的快捷键",
                    ));
                }
            }
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
        #[cfg(windows)]
        let default_data = local.join("Nianlai/data");
        #[cfg(target_os = "macos")]
        let default_data = local.join("Application Support/Nianlai");
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
            #[cfg(windows)]
            {
                local.join("Nianlai/logs")
            }
            #[cfg(target_os = "macos")]
            {
                local.join("Logs/Nianlai")
            }
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
            hotkey_overridden,
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
    fn save_orders_write_then_hotkey_then_broadcast_and_skips_both_on_failure() {
        let directory = Directory::new();
        let (store, _) = Store::load(directory.0.join("settings.json"));
        let calls = std::cell::RefCell::new(Vec::new());
        let run = |update| {
            save(
                &store,
                update,
                |_| calls.borrow_mut().push("hotkey"),
                |_| calls.borrow_mut().push("broadcast"),
            )
        };
        run(json!({"theme":"dark"})).unwrap();
        assert_eq!(*calls.borrow(), ["broadcast"]);
        calls.borrow_mut().clear();
        run(json!({"hotkey":"Alt+V"})).unwrap();
        assert_eq!(*calls.borrow(), ["hotkey", "broadcast"]);
        calls.borrow_mut().clear();
        assert!(run(json!({"hotkey":"Win+L"})).is_err());
        assert!(calls.borrow().is_empty());
        assert_eq!(store.get().hotkey, "Alt+V");
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
        assert_eq!(defaults["log"]["max_total_mb"], 20);
        assert!(parse_hotkey(&store.get().hotkey).is_ok());
        let (updated, issues) = store
            .set(
                json!({"hotkey":"Alt+V","theme":"dark","data_dir":directory.0,
                "memory":{"resuspend_minutes":0.5},
                "log":{"max_total_mb":3}}),
            )
            .unwrap();
        assert!(issues.is_empty());
        let (loaded, issues) = Store::load(path);
        assert!(issues.is_empty());
        assert_eq!(loaded.get(), updated);
        assert_eq!(updated.memory.resuspend_minutes, 0.5);
    }

    #[test]
    fn guide_file_presence_controls_fresh_default_and_legacy_migration() {
        let directory = Directory::new();
        let path = directory.0.join("settings.json");
        let (fresh, issues) = Store::load(path.clone());
        assert!(issues.is_empty());
        assert_eq!(fresh.get().guide, Guide::default());
        assert_eq!(
            serde_json::to_value(fresh.get().guide).unwrap(),
            json!({"version":1,"phase":"await_copy","baseline_ms":null,
                "baseline_total":0,"sample_id":null,"cards_dealt":false,"dismissed":[]})
        );

        let old =
            serde_json::to_string_pretty(&json!({"theme":"dark","future":{"keep":true}})).unwrap();
        fs::write(&path, &old).unwrap();
        let (legacy, issues) = Store::load(path.clone());
        assert!(issues.is_empty());
        assert_eq!(legacy.get().guide.phase, GuidePhase::Completed);
        assert!(!legacy.get().guide.cards_dealt);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            old,
            "只读迁移不能改写旧文件"
        );
        let (reloaded, _) = Store::load(path);
        assert_eq!(reloaded.get().guide.phase, GuidePhase::Completed);
    }

    #[test]
    fn guide_missing_invalid_and_out_of_range_fields_fall_back_safely() {
        let directory = Directory::new();
        let path = directory.0.join("settings.json");
        fs::write(&path, json!({"guide":{"phase":"completed"}}).to_string()).unwrap();
        let (store, issues) = Store::load(path.clone());
        assert!(issues.is_empty());
        assert_eq!(store.get().guide.phase, GuidePhase::Completed);
        assert_eq!(store.get().guide.version, 1);
        assert_eq!(store.get().guide.baseline_ms, None);
        assert_eq!(store.get().guide.baseline_total, 0);
        assert_eq!(store.get().guide.sample_id, None);
        assert!(!store.get().guide.cards_dealt);
        assert!(store.get().guide.dismissed.is_empty());

        fs::write(
            &path,
            json!({"guide":{
                "version":"new","phase":"done","baseline_ms":-1,"baseline_total":-2,
                "sample_id":0,"cards_dealt":"yes",
                "dismissed":["t2","t1","t2","bad",1,"t4","t1"]
            }})
            .to_string(),
        )
        .unwrap();
        let (store, issues) = Store::load(path.clone());
        assert_eq!(issues.len(), 7);
        let guide = &store.get().guide;
        assert_eq!(guide.version, 1);
        assert_eq!(guide.phase, GuidePhase::AwaitCopy);
        assert_eq!(guide.baseline_ms, None);
        assert_eq!(guide.baseline_total, 0);
        assert_eq!(guide.sample_id, None);
        assert!(!guide.cards_dealt);
        assert_eq!(
            guide.dismissed,
            [GuideCardId::T2, GuideCardId::T1, GuideCardId::T4]
        );

        for (field, value) in [
            ("baseline_ms", json!(u64::MAX)),
            ("baseline_total", json!(u64::MAX)),
            ("sample_id", json!(u64::MAX)),
            ("baseline_ms", json!(1.5)),
            ("baseline_total", json!("2")),
            ("sample_id", json!(-3)),
        ] {
            let mut guide = json!({"phase":"await_paste","sample_id":7});
            guide[field] = value;
            fs::write(&path, json!({"guide":guide}).to_string()).unwrap();
            let (store, _) = Store::load(path.clone());
            assert!(
                store
                    .get()
                    .guide
                    .baseline_ms
                    .is_none_or(|n| n <= 9_007_199_254_740_991)
            );
            assert!(store.get().guide.baseline_total <= 9_007_199_254_740_991);
            if field == "sample_id" {
                assert_eq!(store.get().guide.phase, GuidePhase::AwaitCopy);
                assert_eq!(store.get().guide.sample_id, None);
            } else {
                assert_eq!(store.get().guide.sample_id, Some(7));
            }
        }
    }

    #[test]
    fn task14_nonterminal_guide_cannot_keep_cards_dealt() {
        let directory = Directory::new();
        let path = directory.0.join("settings.json");
        for (phase, sample_id) in [
            ("await_copy", Value::Null),
            ("await_show", json!(7)),
            ("await_paste", json!(7)),
        ] {
            fs::write(
                &path,
                json!({"guide":{
                    "phase":phase,"sample_id":sample_id,"cards_dealt":true,
                    "dismissed":["t2"],"future":{"keep":true}
                }})
                .to_string(),
            )
            .unwrap();
            let (store, issues) = Store::load(path.clone());
            assert!(!store.get().guide.cards_dealt, "{phase} 不得保留已发牌");
            assert!(
                issues
                    .iter()
                    .any(|issue| issue.field == "guide.cards_dealt")
            );
            assert_eq!(store.get().guide.dismissed, [GuideCardId::T2]);
            assert_eq!(
                serde_json::to_value(&store.get().guide).unwrap()["future"],
                json!({"keep":true})
            );
        }

        for phase in ["completed", "skipped"] {
            fs::write(
                &path,
                json!({"guide":{"phase":phase,"cards_dealt":true}}).to_string(),
            )
            .unwrap();
            let (store, issues) = Store::load(path.clone());
            assert!(store.get().guide.cards_dealt, "{phase} 的显式重发必须保留");
            assert!(issues.is_empty());
        }

        fs::write(
            &path,
            json!({"guide":{"phase":"done","cards_dealt":true}}).to_string(),
        )
        .unwrap();
        let (store, issues) = Store::load(path.clone());
        assert_eq!(store.get().guide.phase, GuidePhase::AwaitCopy);
        assert!(!store.get().guide.cards_dealt);
        assert!(issues.iter().any(|issue| issue.field == "guide.phase"));
        assert!(
            issues
                .iter()
                .any(|issue| issue.field == "guide.cards_dealt")
        );
    }

    #[test]
    fn guide_unknown_fields_survive_load_save_and_reload() {
        let directory = Directory::new();
        let path = directory.0.join("settings.json");
        let future = json!({"nested":[1,null,{"中文":"保留"}]});
        fs::write(
            &path,
            json!({"future":future,"guide":{"phase":"await_show","sample_id":9,"future":future}})
                .to_string(),
        )
        .unwrap();
        let (store, issues) = Store::load(path.clone());
        assert!(issues.is_empty());
        store.set(json!({"guide":{"cards_dealt":false}})).unwrap();
        let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["future"], future);
        assert_eq!(saved["guide"]["future"], future);
        assert_eq!(saved["guide"]["phase"], "await_show");
        let (reloaded, issues) = Store::load(path);
        assert!(issues.is_empty());
        assert_eq!(reloaded.get(), store.get());
    }

    #[test]
    fn guide_about_and_delete_updates_are_partial_and_preserve_unrelated_fields() {
        let directory = Directory::new();
        let path = directory.0.join("settings.json");
        fs::write(
            &path,
            json!({"guide":{"phase":"skipped","cards_dealt":false,"dismissed":["t2"]}}).to_string(),
        )
        .unwrap();
        let (store, issues) = Store::load(path.clone());
        assert!(issues.is_empty());
        store
            .set(json!({"guide":{"cards_dealt":true,"dismissed":[]}}))
            .unwrap();
        assert_eq!(store.get().guide.phase, GuidePhase::Skipped);
        assert!(store.get().guide.cards_dealt);
        assert!(store.get().guide.dismissed.is_empty());

        store
            .set(json!({"guide":{
                "version":1,"phase":"await_copy","baseline_ms":null,"baseline_total":0,
                "sample_id":null,"cards_dealt":false
            }}))
            .unwrap();
        assert_eq!(store.get().guide.phase, GuidePhase::AwaitCopy);
        assert!(!store.get().guide.cards_dealt);
        assert!(store.get().guide.dismissed.is_empty());

        store
            .set(json!({"guide":{"phase":"completed","cards_dealt":true,"dismissed":["t1"]}}))
            .unwrap();
        store
            .set(json!({"guide":{"dismissed":["t1","t3"]}}))
            .unwrap();
        let guide = &store.get().guide;
        assert_eq!(guide.phase, GuidePhase::Completed);
        assert!(guide.cards_dealt);
        assert_eq!(guide.dismissed, [GuideCardId::T1, GuideCardId::T3]);
        let (reloaded, issues) = Store::load(path);
        assert!(issues.is_empty());
        assert_eq!(reloaded.get(), store.get());
    }

    #[test]
    fn retired_experiments_section_is_read_without_issues_and_kept_as_unknown() {
        let directory = Directory::new();
        let path = directory.0.join("settings.json");
        let old = json!({"searchbox_focus":true});
        fs::write(&path, json!({"theme":"dark","experiments":old}).to_string()).unwrap();
        let (store, issues) = Store::load(path.clone());
        assert!(issues.is_empty());
        assert_eq!(store.get().theme, "dark");
        store.set(json!({"theme":"light"})).unwrap();
        let value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["experiments"], old);
    }

    #[test]
    fn snapshot_limit_is_validated() {
        let (settings, issues) = validate(json!({}));
        assert!(issues.is_empty());
        assert_eq!(settings.snapshots.max_mb, SNAPSHOT_MAX_MB);
        let (settings, issues) = validate(json!({"snapshots":{"max_mb":2048}}));
        assert!(issues.is_empty());
        assert_eq!(settings.snapshots.max_mb, 2048);
        for bad in [json!(0), json!(63), json!(102_401), json!("1G")] {
            let (settings, issues) = validate(json!({"snapshots":{"max_mb":bad}}));
            assert_eq!(issues.len(), 1);
            assert_eq!(settings.snapshots.max_mb, SNAPSHOT_MAX_MB);
        }
    }

    #[test]
    fn interaction_defaults_and_validation() {
        let (settings, issues) = validate(json!({}));
        assert!(issues.is_empty());
        assert_eq!(settings.interaction, Interaction::default());
        assert_eq!(settings.interaction.placement, "caret");
        assert_eq!(settings.interaction.click, "paste");
        assert_eq!(settings.interaction.middle_click, "text");
        let (settings, issues) = validate(
            json!({"interaction":{"placement":"center","click":"none","middle_click":"copy"}}),
        );
        assert!(issues.is_empty());
        assert_eq!(settings.interaction.placement, "center");
        assert_eq!(settings.interaction.click, "none");
        assert_eq!(settings.interaction.middle_click, "copy");
        for bad in [json!("left"), json!(1), json!(null)] {
            let (settings, issues) = validate(
                json!({"interaction":{"placement":bad.clone(),"click":bad.clone(),"middle_click":"x"}}),
            );
            assert_eq!(issues.len(), 3);
            assert_eq!(settings.interaction, Interaction::default());
        }
        // 只改一项时，其余字段保持原值（合并更新）。
        let (settings, _) = validate(json!({"interaction":{"click":"copy"}}));
        assert_eq!(settings.interaction.middle_click, "text");
    }

    #[test]
    fn history_defaults_and_validation() {
        let (settings, issues) = validate(json!({}));
        assert!(issues.is_empty());
        assert!(settings.history.reuse_to_top);
        let (settings, issues) = validate(json!({"history":{"reuse_to_top":false}}));
        assert!(issues.is_empty());
        assert!(!settings.history.reuse_to_top);
        for bad in [json!("no"), json!(0), json!(null)] {
            let (settings, issues) = validate(json!({"history":{"reuse_to_top":bad}}));
            assert_eq!(issues.len(), 1);
            assert!(settings.history.reuse_to_top);
        }
    }

    #[cfg(windows)]
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
            "memory":{"resuspend_minutes":-1},
            "log":{"max_total_mb":0}
        }));
        assert_eq!(issues.len(), 4);
        assert_eq!(settings.hotkey, DEFAULT_HOTKEY);
        assert_eq!(settings.theme, "dark");
        assert!(settings.data_dir.is_none());
        assert_eq!(settings.memory.resuspend_minutes, 0.0);
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
        let (settings, issues) = validate(json!({"theme":[],"memory":false,"log":null}));
        assert_eq!(settings, Settings::default());
        assert_eq!(issues.len(), 3);
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
            "log":{"future":unknown}})
            .to_string(),
        )
        .unwrap();
        let (store, issues) = Store::load(path.clone());
        assert!(issues.is_empty());
        store
            .set(json!({"theme":"light","memory":{"resuspend_minutes":15},
            "log":{"max_total_mb":2}}))
            .unwrap();
        let value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["future"], unknown);
        for section in ["memory", "log"] {
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
                "log":{"max_total_mb":private}})
            .to_string(),
        )
        .unwrap();
        let (store, mut issues) = Store::load(path.clone());
        assert_eq!(
            store.get(),
            Settings {
                guide: Guide {
                    phase: GuidePhase::Completed,
                    ..Guide::default()
                },
                ..Settings::default()
            }
        );
        assert_eq!(issues.len(), 5);
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
        assert_eq!(rows.len(), 6);
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
        assert_eq!(
            store.set(json!({"theme":"dark"})).err().unwrap(),
            "没保存上，还是原来的设置"
        );
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

    #[cfg(windows)]
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
        assert!(resolved.hotkey_overridden);
        assert!(!resolve(None, None, None).hotkey_overridden);
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
            // 无效的环境变量不算覆盖：沿用设置文件里保存的键。
            assert_eq!(invalid.settings.hotkey, "Alt+V");
            assert!(!invalid.hotkey_overridden);
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

    #[test]
    fn update_settings_default_validate_and_keep_unknown_fields() {
        let (settings, issues) = validate(json!({}));
        assert!(issues.is_empty());
        assert!(settings.update.auto_check);
        assert_eq!(settings.update.latest, None);
        let sha = "a".repeat(64);
        let url = format!(
            "{}v0.2.0/nianlai-0.2.0-windows-x64-portable.zip",
            crate::update::feed::DOWNLOAD_PREFIX
        );
        let (settings, issues) = validate(json!({"update":{
            "auto_check": "yes", "last_result": "bogus", "skipped_version": 5,
            "latest": {"version":"0.2.0","url":url,"sha256":sha},
            "future_field": 1,
        }}));
        assert!(settings.update.auto_check);
        assert_eq!(settings.update.skipped_version, None);
        assert_eq!(settings.update.last_result, Default::default());
        #[cfg(windows)]
        assert_eq!(settings.update.latest.unwrap().version, "0.2.0");
        assert_eq!(issues.len(), 3 + usize::from(cfg!(not(windows))));
        let (settings, issues) = validate(
            json!({"update":{"latest":{"version":"0.2.0","url":"https://evil.com/a.zip","sha256":sha}}}),
        );
        assert_eq!(settings.update.latest, None);
        assert_eq!(issues.len(), 1);
        assert!(serde_json::to_value(&settings).unwrap()["update"].is_object());
    }
}
