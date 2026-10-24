use crate::{cli::Mode, protocol::Digest, scenario::Requirement};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    Pass,
    Degraded,
    WrongPaste,
    WrongSubmit,
    FocusLost,
    ClaimMismatch,
    Timeout,
    HarnessError,
    Skipped,
    Aborted,
}
impl Class {
    pub fn valid(self) -> bool {
        // 中止的那次没做完，和工具自身的问题一样不计入有效次数，格子显示“未完成”而不是“不通过”。
        !matches!(self, Self::HarnessError | Self::Skipped | Self::Aborted)
    }
}
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub expected: Digest,
    pub actual: Digest,
    pub before: Digest,
    pub elsewhere_changed: bool,
    pub leaked_key: bool,
    pub submitted: bool,
    pub focus_ok: bool,
    pub transition_ok: bool,
    pub observed: bool,
    pub claim: Option<String>,
    pub reason: Option<String>,
    pub clipboard_written: bool,
    pub escaped: bool,
    pub wrong_item: Option<usize>,
    pub duplicate: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub class: Class,
    pub flags: Vec<Class>,
    pub explanation: String,
}
pub fn classify(e: &Evidence) -> Verdict {
    let mut flags = vec![];
    let changed = e.actual != e.before || e.elsewhere_changed;
    let pasted = e.actual == e.expected && e.observed;
    if e.submitted {
        flags.push(Class::WrongSubmit);
    }
    if e.elsewhere_changed
        || e.leaked_key
        || e.wrong_item.is_some()
        || e.duplicate
        || (e.observed && e.actual != e.expected && changed)
        || (e.escaped && changed)
    {
        flags.push(Class::WrongPaste);
    }
    if (matches!(e.claim.as_deref(), Some("sent" | "confirmed")) && !pasted)
        || (matches!(e.claim.as_deref(), Some("copied" | "copied_only")) && changed)
    {
        flags.push(Class::ClaimMismatch);
    }
    let degraded = e.claim.as_deref() == Some("copied_only")
        && e.clipboard_written
        && e.reason.as_ref().is_some_and(|s| !s.is_empty())
        && !changed
        && e.observed;
    if !degraded && (!e.focus_ok || !e.transition_ok) {
        flags.push(Class::FocusLost);
    }
    let class = flags.first().copied().unwrap_or({
        if !e.observed {
            Class::HarnessError
        } else if degraded {
            Class::Degraded
        } else if (e.escaped && !changed)
            || (pasted && matches!(e.claim.as_deref(), Some("sent" | "confirmed")))
        {
            Class::Pass
        } else {
            Class::ClaimMismatch
        }
    });
    if flags.is_empty() && !matches!(class, Class::Pass | Class::Degraded) {
        flags.push(class);
    }
    let explanation = match class {
        Class::Pass => "内容、焦点和提交证据一致",
        Class::Degraded => "样机正当降级为仅复制，所有可观测目标均未改变",
        Class::WrongPaste => "发现粘错条目、重复粘贴、漏键或旁路内容变化",
        Class::WrongSubmit => "发现提交键泄漏、页面跳转、弹窗退出或文件名改变",
        Class::FocusLost => "原输入焦点或前台切换序列不符合模式要求",
        Class::ClaimMismatch => "样机粘贴结论与实际内容不一致",
        Class::HarnessError => "工具未取得充分的内容证据",
        _ => "试验未完成",
    }
    .into();
    Verdict {
        class,
        flags,
        explanation,
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trial {
    pub scenario: String,
    pub mode: Mode,
    pub trial: u32,
    pub session: Option<u64>,
    pub sample: Option<usize>,
    pub action: String,
    pub result: Verdict,
    pub evidence: Option<Evidence>,
    pub hotkey_ms: Option<f64>,
    pub result_ms: Option<f64>,
    pub details: serde_json::Value,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Cell {
    Pass,
    Fail,
    Incomplete,
    Manual,
}
impl Cell {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pass => "通过",
            Self::Fail => "不通过",
            Self::Incomplete => "未完成",
            Self::Manual => "手工",
        }
    }
}
pub fn cell(requirement: Requirement, n: u32, results: &[&Trial]) -> Cell {
    // 看到过误粘、误提交或结论不符就不通过，哪怕那次随后被中止、不计入有效次数。
    if results.iter().any(|r| {
        r.result.flags.iter().any(|f| {
            matches!(
                f,
                Class::WrongPaste | Class::WrongSubmit | Class::ClaimMismatch
            )
        })
    }) {
        return Cell::Fail;
    }
    let valid: Vec<_> = results.iter().filter(|r| r.result.class.valid()).collect();
    if valid.len() < n as usize {
        return Cell::Incomplete;
    }
    if valid.iter().all(|r| {
        r.result.flags.is_empty()
            && (r.result.class == Class::Pass
                || (requirement == Requirement::BestEffort && r.result.class == Class::Degraded))
    }) {
        Cell::Pass
    } else {
        Cell::Fail
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn good() -> Evidence {
        Evidence {
            before: Digest::of(""),
            expected: Digest::of("hello"),
            actual: Digest::of("hello"),
            focus_ok: true,
            transition_ok: true,
            observed: true,
            claim: Some("confirmed".into()),
            ..Default::default()
        }
    }
    #[test]
    fn classifications_and_cross_checks() {
        assert_eq!(classify(&good()).class, Class::Pass);
        let mut e = good();
        e.actual = e.before;
        assert_eq!(classify(&e).class, Class::ClaimMismatch);
        e.claim = Some("copied_only".into());
        e.clipboard_written = true;
        e.reason = Some("invalid_target".into());
        e.focus_ok = false;
        assert_eq!(classify(&e).class, Class::Degraded);
        e.actual = e.expected;
        let r = classify(&e);
        assert!(r.flags.contains(&Class::ClaimMismatch));
        e.submitted = true;
        e.leaked_key = true;
        let r = classify(&e);
        assert_eq!(r.class, Class::WrongSubmit);
        assert!(r.flags.contains(&Class::WrongPaste));
        let mut e = good();
        e.transition_ok = false;
        assert_eq!(classify(&e).class, Class::FocusLost);
    }
    pub fn row(class: Class) -> Trial {
        Trial {
            scenario: "a".into(),
            mode: Mode::Quick,
            trial: 1,
            session: None,
            sample: None,
            action: "Enter".into(),
            result: Verdict {
                class,
                flags: vec![],
                explanation: "测试".into(),
            },
            evidence: None,
            hotkey_ms: Some(10.0),
            result_ms: Some(20.0),
            details: serde_json::Value::Null,
        }
    }
    #[test]
    fn counts_and_requirements() {
        let pass = row(Class::Pass);
        let deg = row(Class::Degraded);
        let harness = row(Class::HarnessError);
        assert_eq!(
            cell(Requirement::Required, 2, &[&pass, &harness]),
            Cell::Incomplete
        );
        assert_eq!(cell(Requirement::Required, 2, &[&pass, &deg]), Cell::Fail);
        assert_eq!(cell(Requirement::BestEffort, 2, &[&pass, &deg]), Cell::Pass);
        assert_eq!(
            cell(Requirement::BestEffort, 2, &[&pass, &row(Class::FocusLost)]),
            Cell::Fail
        );
        assert_eq!(
            cell(Requirement::Required, 2, &[&pass, &row(Class::Aborted)]),
            Cell::Incomplete
        );
        let mut aborted = row(Class::Aborted);
        aborted.result.flags = vec![Class::WrongPaste, Class::Aborted];
        assert_eq!(
            cell(Requirement::Required, 1, &[&pass, &aborted]),
            Cell::Fail
        );
    }
}
