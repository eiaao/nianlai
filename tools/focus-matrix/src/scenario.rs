use crate::{
    cli::{Hotkey, Mode},
    protocol, samples,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const DEFAULT: &str = include_str!("../scenarios/default.json");
pub const PAGE: &str = include_str!("../assets/page.html");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub timeouts: Timeouts,
    pub scenarios: Vec<Scenario>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timeouts {
    pub startup_ms: u64,
    pub shown_ms: u64,
    pub search_ms: u64,
    pub paste_ms: u64,
    pub settle_ms: u64,
    pub trial_ms: u64,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Requirement {
    Required,
    BestEffort,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requirements {
    pub quick: Requirement,
    pub search: Requirement,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub sample: usize,
    pub query: String,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Helper,
    File,
    Web,
    Address,
    Bookmark,
    Explorer,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Control {
    pub class: Option<String>,
    pub control_type: Option<i32>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub kind: Kind,
    pub program: Option<String>,
    pub args: Vec<String>,
    pub process: String,
    pub window_class: String,
    pub control: Control,
    #[serde(default)]
    pub address_control: Option<Control>,
    pub field: String,
    pub exclusive: bool,
    pub single_line: bool,
    pub fullscreen: bool,
    pub second_monitor: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    Focus,
    Clear,
    Key { chord: String },
    Wait { ms: u64 },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub id: String,
    pub name: String,
    pub row: u8,
    pub enabled: bool,
    pub note: String,
    pub requirements: Requirements,
    pub target: Target,
    pub prepare: Vec<Step>,
    pub cleanup: Vec<Step>,
    pub expected: String,
    pub samples: Vec<Query>,
    pub quick_digits: Vec<u8>,
    pub search_digit: u8,
}
impl Scenario {
    pub fn requirement(&self, mode: Mode) -> Requirement {
        if self.target.kind == Kind::Helper {
            return Requirement::Required;
        }
        match mode {
            Mode::Quick => self.requirements.quick,
            Mode::Search => self.requirements.search,
        }
    }
    pub fn supports_sample(&self, id: usize) -> bool {
        self.samples.iter().any(|q| q.sample == id)
    }
}

pub fn search_matches(query: &str, tokenized: bool) -> Vec<usize> {
    let query = query.to_lowercase();
    samples::all()
        .iter()
        .filter(|sample| {
            let text = sample
                .text
                .chars()
                .take(300)
                .collect::<String>()
                .to_lowercase();
            if tokenized {
                query.split_whitespace().all(|word| text.contains(word))
            } else {
                text.contains(&query)
            }
        })
        .map(|s| s.id)
        .collect()
}
pub fn filename_ok(name: &str) -> bool {
    if name.is_empty()
        || name.encode_utf16().count() > 120
        || name.ends_with([' ', '.'])
        || name
            .chars()
            .any(|c| c.is_control() || "\\/:*?\"<>|".contains(c))
    {
        return false;
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    ![
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ]
    .contains(&stem.as_str())
}
impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 || self.scenarios.is_empty() || self.scenarios.len() > 100 {
            return Err("场景版本须为 1，数量 1–100".into());
        }
        let t = &self.timeouts;
        if !(1000..=120000).contains(&t.startup_ms)
            || !(500..=10000).contains(&t.shown_ms)
            || !(500..=10000).contains(&t.search_ms)
            || !(2000..=15000).contains(&t.paste_ms)
            || !(100..=3000).contains(&t.settle_ms)
            || !(10000..=60000).contains(&t.trial_ms)
            || t.trial_ms < t.shown_ms + t.search_ms + t.paste_ms + t.settle_ms
        {
            return Err("超时范围或单次总预算不合理".into());
        }
        let all = samples::all();
        let mut ids = BTreeSet::new();
        for s in &self.scenarios {
            let fail = |m: &str| format!("场景 {}：{m}", s.id);
            if s.id.is_empty()
                || !s
                    .id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
                || !ids.insert(&s.id)
            {
                return Err(fail("ID 无效或重复"));
            }
            if !(1..=8).contains(&s.row) || s.name.is_empty() {
                return Err(fail("行号或中文名无效"));
            }
            if s.samples.is_empty() || s.samples.len() > 50 {
                return Err(fail("samples 不能为空或过多"));
            }
            for q in &s.samples {
                let text = &all
                    .iter()
                    .find(|v| v.id == q.sample)
                    .ok_or_else(|| fail("示例不存在"))?
                    .text;
                if !q
                    .query
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_lowercase)
                    || q.query.chars().count() > 100
                    || q.query.chars().any(char::is_control)
                {
                    return Err(fail(
                        "搜索词首字符须是小写 ASCII 字母，且长度不超过 100、无控制字符",
                    ));
                }
                if search_matches(&q.query, false) != [q.sample]
                    || search_matches(&q.query, true) != [q.sample]
                {
                    return Err(fail("搜索词在两种搜索语义下必须唯一命中指定示例"));
                }
                if s.target.single_line && text.contains(['\r', '\n']) {
                    return Err(fail("单行控件不能使用多行示例"));
                }
                if s.target.kind == Kind::Explorer && !filename_ok(text) {
                    return Err(fail("重命名示例不是合法的适中文件名"));
                }
            }
            if s.quick_digits.is_empty()
                || s.quick_digits.iter().any(|n| !(1..=9).contains(n))
                || !(1..=9).contains(&s.search_digit)
            {
                return Err(fail("数字范围为 1–9"));
            }
            if s.search_digit != 1 {
                return Err(fail("唯一搜索结果只允许 Ctrl+1"));
            }
            protocol::expand(&s.expected, "", "", "").map_err(|e| fail(&e))?;
            if !s.expected.contains("{pasted}") {
                return Err(fail("期望模板必须包含 {pasted}"));
            }
            if s.prepare.len() + s.cleanup.len() > 30 {
                return Err(fail("步骤过多"));
            }
            let wait_total: u64 = s
                .prepare
                .iter()
                .chain(&s.cleanup)
                .filter_map(|step| match step {
                    Step::Wait { ms } => Some(*ms),
                    _ => None,
                })
                .sum();
            if wait_total > t.trial_ms - t.shown_ms - t.search_ms - t.paste_ms - t.settle_ms {
                return Err(fail("步骤等待总量超出试验剩余预算"));
            }
            if s.target.process.eq_ignore_ascii_case("explorer.exe")
                && s.target.kind != Kind::Explorer
            {
                return Err(fail("explorer.exe 只能用不终止进程的 explorer 目标类型"));
            }
            for step in s.prepare.iter().chain(&s.cleanup) {
                match step {
                    Step::Key { chord } => {
                        Hotkey::parse(chord).map_err(|e| fail(&e))?;
                        if ![
                            "Ctrl+A", "Delete", "Ctrl+L", "Ctrl+D", "F2", "Esc", "Ctrl+S", "End",
                            "Home",
                        ]
                        .contains(&chord.as_str())
                        {
                            return Err(fail("步骤按键不在允许集合内"));
                        }
                    }
                    Step::Wait { ms } if *ms > 3000 => {
                        return Err(fail("单步等待不能超过 3000 ms"));
                    }
                    _ => {}
                }
            }
            if s.target.kind != Kind::Helper
                && (s.target.program.as_ref().is_none_or(String::is_empty)
                    || s.target.process.is_empty()
                    || s.target.window_class.is_empty())
            {
                return Err(fail("目标程序、进程名与窗口类名必填"));
            }
            if s.target.kind == Kind::Helper
                && (s.requirements.quick != Requirement::Required
                    || s.requirements.search != Requirement::Required)
            {
                return Err(fail("基准窗口必须通过"));
            }
            if matches!(s.target.kind, Kind::Web | Kind::Address | Kind::Bookmark) {
                if s.target
                    .address_control
                    .as_ref()
                    .is_some_and(|c| c.class.as_ref().is_none_or(String::is_empty))
                {
                    return Err(fail("浏览器地址栏匹配必须指定类名"));
                }
                if !["input", "textarea", "editable"].contains(&s.target.field.as_str()) {
                    return Err(fail("网页字段必须为 input/textarea/editable"));
                }
                for required in [
                    "--user-data-dir={profile}",
                    "--host-resolver-rules=MAP * ~NOTFOUND",
                    "--proxy-server=http://127.0.0.1:9",
                    "--no-first-run",
                    "--no-default-browser-check",
                    "--disable-sync",
                    "--disable-extensions",
                    "--disable-background-networking",
                    "--force-renderer-accessibility",
                ] {
                    if !s.target.args.iter().any(|a| a == required) {
                        return Err(fail(&format!("浏览器缺少隔离参数：{required}")));
                    }
                    let flag = required.split('=').next().unwrap_or(required);
                    if s.target
                        .args
                        .iter()
                        .filter(|arg| arg.split('=').next() == Some(flag))
                        .count()
                        != 1
                    {
                        return Err(fail("浏览器隔离参数重复，可能覆盖本次独立目录或断网配置"));
                    }
                }
                if s.target
                    .args
                    .iter()
                    .filter(|a| a.as_str() == "{url}")
                    .count()
                    != 1
                {
                    return Err(fail("浏览器必须且只能打开本次 {url}"));
                }
                if s.target
                    .args
                    .iter()
                    .any(|a| !a.starts_with("--") && a != "{url}")
                {
                    return Err(fail("浏览器不允许额外的位置参数"));
                }
                if s.target.fullscreen && !s.target.args.iter().any(|a| a == "--start-fullscreen") {
                    return Err(fail("全屏场景缺少 --start-fullscreen"));
                }
            }
            for arg in &s.target.args {
                let mut tail = arg.as_str();
                while let Some((_, rest)) = tail.split_once('{') {
                    let (name, next) = rest
                        .split_once('}')
                        .ok_or_else(|| fail("命令行占位符未闭合"))?;
                    if !["file", "url", "profile", "token", "dir"].contains(&name) {
                        return Err(fail("未知命令行占位符"));
                    }
                    tail = next;
                }
                if arg.contains('\0') {
                    return Err(fail("命令行含 NUL"));
                }
            }
        }
        Ok(())
    }
    pub fn select<'a>(&'a self, only: &[String]) -> Result<Vec<&'a Scenario>, String> {
        for id in only {
            if !self.scenarios.iter().any(|s| &s.id == id) {
                return Err(format!("未知场景 ID：{id}"));
            }
        }
        let selected: Vec<_> = self
            .scenarios
            .iter()
            .filter(|s| {
                if only.is_empty() {
                    s.enabled
                } else {
                    only.contains(&s.id)
                }
            })
            .collect();
        if selected.is_empty() {
            return Err("没有选中的场景".into());
        }
        Ok(selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn defaults() -> Config {
        serde_json::from_str(DEFAULT).unwrap()
    }
    #[test]
    fn default_valid() {
        defaults().validate().unwrap();
    }
    #[test]
    fn both_search_semantics() {
        assert_eq!(search_matches("sample item 11。", false), [11]);
        assert_eq!(search_matches("sample item 11。", true), [11]);
        assert!(search_matches("item sample", false).is_empty());
        assert!(search_matches("item sample", true).len() > 1);
        let mut c = defaults();
        c.scenarios[0].samples[0].query = "hello".into();
        assert!(c.validate().is_err());
    }
    #[test]
    fn validation_rejects() {
        let mut c = defaults();
        c.scenarios[1].id = c.scenarios[0].id.clone();
        assert!(c.validate().is_err());
        let mut c = defaults();
        c.scenarios[0].samples[0].sample = 99;
        assert!(c.validate().is_err());
        let mut c = defaults();
        c.scenarios[0].target.single_line = true;
        assert!(c.validate().is_err());
        let mut c = defaults();
        c.scenarios[0].quick_digits = vec![0];
        assert!(c.validate().is_err());
        let mut c = defaults();
        c.scenarios[0].samples[0].query = "World!".into();
        assert!(c.validate().is_err());
        let mut c = defaults();
        c.scenarios[0].target.kind = Kind::Explorer;
        assert!(c.validate().is_err());
    }
    #[test]
    fn filenames() {
        for v in ["a/b", "a:b", "a\nb", "CON", "COM1.txt", "x.", "x ", ""] {
            assert!(!filename_ok(v), "{v}");
        }
        assert!(filename_ok(&samples::all()[10].text));
    }
    #[test]
    fn browser_isolation_cannot_be_overridden() {
        let mut c = defaults();
        c.scenarios
            .iter_mut()
            .find(|s| s.target.kind == Kind::Web)
            .unwrap()
            .target
            .args
            .push("--user-data-dir=C:/user-profile".into());
        assert!(c.validate().is_err());
        let mut c = defaults();
        c.scenarios
            .iter_mut()
            .find(|s| s.target.kind == Kind::Web)
            .unwrap()
            .target
            .args
            .push("https://example.com".into());
        assert!(c.validate().is_err());
    }
}
