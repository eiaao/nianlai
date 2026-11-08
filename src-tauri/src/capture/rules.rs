//! 采集规则链（PRD 6.1）：写入前依次检查暂停（A09）→ 排除应用（A07）→ 敏感内容（A08）
//! → 类型开关和大小上限（A10）；临时内容（A11）在历史层按行为识别（history::Deferred）。
//! 被跳过的内容不落盘，日志只记原因。

use super::sensitive;
use crate::{
    history::content::{DIB, DIBV5, Data, FILES, HTML, PNG, RTF, TEXT},
    settings::Capture,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, LazyLock, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};

const KIB: usize = 1024;

#[derive(Debug, PartialEq, Eq)]
pub struct Skip {
    pub reason: &'static str,
    pub rule: Option<&'static str>,
}

impl Skip {
    fn new(reason: &'static str) -> Self {
        Self { reason, rule: None }
    }
}

type Listener = Arc<dyn Fn() + Send + Sync>;

pub struct Gate {
    // 暂停只在内存里，重启后自动恢复记录，不会忘了开回来。
    paused: AtomicBool,
    ignore_next: AtomicBool,
    policy: RwLock<Arc<Capture>>,
    // 暂停状态变化时通知托盘和面板。
    listeners: Mutex<Vec<Listener>>,
}

static GATE: LazyLock<Gate> = LazyLock::new(Gate::default);

pub fn gate() -> &'static Gate {
    &GATE
}

impl Default for Gate {
    fn default() -> Self {
        Self {
            paused: AtomicBool::new(false),
            ignore_next: AtomicBool::new(false),
            policy: RwLock::new(Arc::new(Capture::default())),
            listeners: Mutex::new(Vec::new()),
        }
    }
}

impl Gate {
    pub fn apply(&self, capture: &Capture) {
        *self.policy.write().unwrap() = Arc::new(capture.clone());
    }

    pub fn policy(&self) -> Arc<Capture> {
        self.policy.read().unwrap().clone()
    }

    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }

    pub fn ignoring_next(&self) -> bool {
        self.ignore_next.load(Ordering::Acquire)
    }

    pub fn set_paused(&self, paused: bool) {
        if self.paused.swap(paused, Ordering::AcqRel) != paused {
            self.notify();
        }
    }

    pub fn set_ignore_next(&self, ignore: bool) {
        if self.ignore_next.swap(ignore, Ordering::AcqRel) != ignore {
            self.notify();
        }
    }

    pub fn subscribe(&self, listener: Listener) {
        self.listeners.lock().unwrap().push(listener);
    }

    fn notify(&self) {
        let listeners = self.listeners.lock().unwrap().clone();
        for listener in listeners {
            listener();
        }
    }

    /// 读取剪贴板内容之前：暂停和排除应用直接跳过，正文不进内存。
    /// `source` 是复制时的前台进程，`owner` 是剪贴板所有者进程，任一在名单里都算。
    pub fn before_read(&self, source: &str, owner: &str) -> Option<&'static str> {
        if self.paused() {
            return Some("paused");
        }
        let policy = self.policy();
        let excluded = |name: &str| {
            !name.is_empty()
                && policy
                    .excluded_apps
                    .iter()
                    .any(|app| app.eq_ignore_ascii_case(name))
        };
        (excluded(source) || excluded(owner)).then_some("excluded_app")
    }

    /// 读到内容之后、写入之前：忽略下一次 → 敏感内容 → 类型开关和大小上限。
    /// 关掉的类型只去掉对应格式；全部去掉时才跳过整条。
    pub fn after_read(&self, data: &mut BTreeMap<String, Data>) -> Result<(), Skip> {
        // 只有真正读到内容才消耗“忽略下一次”，清空剪贴板之类的变化不算。
        if self.ignore_next.swap(false, Ordering::AcqRel) {
            self.notify();
            return Err(Skip::new("ignore_next"));
        }
        let policy = self.policy();
        check(&policy, data)
    }
}

fn check(policy: &Capture, data: &mut BTreeMap<String, Data>) -> Result<(), Skip> {
    if let Some(Data::Bytes(bytes)) = data.get(TEXT) {
        let text = utf16(bytes);
        if let Some(rule) = sensitive::detect(&text, &policy.sensitive_off) {
            return Err(Skip {
                reason: "sensitive",
                rule: Some(rule),
            });
        }
    }
    let before = data.len();
    data.retain(|name, _| match name.as_str() {
        TEXT => policy.text,
        HTML | RTF => policy.text && policy.rich_text,
        PNG | DIB | DIBV5 => policy.images,
        FILES => policy.files,
        _ => true,
    });
    if data.is_empty() && before > 0 {
        return Err(Skip::new("type_disabled"));
    }
    let limit = policy.max_text_kb as usize * KIB;
    if [TEXT, HTML, RTF]
        .iter()
        .any(|name| data.get(*name).is_some_and(|value| value.size() > limit))
    {
        return Err(Skip::new("too_large"));
    }
    Ok(())
}

fn utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|unit| *unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> Data {
        Data::Bytes(
            value
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>()
                .into(),
        )
    }

    fn formats(entries: &[(&str, Data)]) -> BTreeMap<String, Data> {
        entries
            .iter()
            .map(|(name, data)| ((*name).to_owned(), data.clone()))
            .collect()
    }

    fn png() -> Data {
        Data::Bytes(vec![1u8; 16].into())
    }

    #[test]
    fn pause_and_excluded_apps_skip_before_reading() {
        let gate = Gate::default();
        assert_eq!(gate.before_read("notepad.exe", "notepad.exe"), None);
        assert_eq!(
            gate.before_read("KeePass.exe", "notepad.exe"),
            Some("excluded_app")
        );
        assert_eq!(
            gate.before_read("explorer.exe", "keepassxc.EXE"),
            Some("excluded_app")
        );
        assert_eq!(gate.before_read("", ""), None);
        gate.set_paused(true);
        assert_eq!(gate.before_read("notepad.exe", ""), Some("paused"));
        gate.set_paused(false);
        let mut capture = Capture::default();
        capture.excluded_apps = vec!["Code.exe".into()];
        gate.apply(&capture);
        assert_eq!(gate.before_read("KeePass.exe", ""), None);
        assert_eq!(gate.before_read("code.exe", ""), Some("excluded_app"));
    }

    #[test]
    fn ignore_next_is_consumed_only_by_real_content() {
        let gate = Gate::default();
        let notified = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = notified.clone();
        gate.subscribe(Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        }));
        gate.set_ignore_next(true);
        let mut data = formats(&[(TEXT, text("hello"))]);
        assert_eq!(gate.after_read(&mut data), Err(Skip::new("ignore_next")));
        assert!(!gate.ignoring_next());
        assert_eq!(gate.after_read(&mut data), Ok(()));
        assert_eq!(notified.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn sensitive_text_is_skipped_unless_rule_disabled() {
        let mut capture = Capture::default();
        let mut data = formats(&[(TEXT, text("id AKIAIOSFODNN7EXAMPLE"))]);
        assert_eq!(
            check(&capture, &mut data),
            Err(Skip {
                reason: "sensitive",
                rule: Some("cloud_access_key")
            })
        );
        capture.sensitive_off = vec!["cloud_access_key".into()];
        assert_eq!(check(&capture, &mut data), Ok(()));
    }

    #[test]
    fn disabled_types_drop_only_their_formats() {
        let mut capture = Capture::default();
        capture.rich_text = false;
        let mut data = formats(&[(TEXT, text("a")), (HTML, text("<b>a</b>")), (PNG, png())]);
        assert_eq!(check(&capture, &mut data), Ok(()));
        assert_eq!(data.keys().collect::<Vec<_>>(), [TEXT, PNG]);

        capture.images = false;
        let mut data = formats(&[(PNG, png()), (DIB, png())]);
        assert_eq!(check(&capture, &mut data), Err(Skip::new("type_disabled")));

        // 关掉纯文本时富文本也一起去掉，否则粘贴时没有纯文本可降级。
        let mut capture = Capture::default();
        capture.text = false;
        let mut data = formats(&[
            (TEXT, text("a")),
            (RTF, text("{\\rtf a}")),
            (FILES, Data::Files(vec!["C:\\a".into()].into())),
        ]);
        assert_eq!(check(&capture, &mut data), Ok(()));
        assert_eq!(data.keys().collect::<Vec<_>>(), [FILES]);
    }

    #[test]
    fn text_over_limit_is_skipped() {
        let mut capture = Capture::default();
        capture.max_text_kb = 1;
        let mut data = formats(&[(TEXT, text(&"x".repeat(600)))]);
        assert_eq!(check(&capture, &mut data), Err(Skip::new("too_large")));
        let mut data = formats(&[(TEXT, text(&"x".repeat(400)))]);
        assert_eq!(check(&capture, &mut data), Ok(()));
    }
}
