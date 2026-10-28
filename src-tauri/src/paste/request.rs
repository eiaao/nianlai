use crate::{history::content::Content, platform::Target};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Instant};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Paste,
    Copy,
    // Ctrl+Enter：先把文件、图片、富文本转成文字，再照常粘贴。
    Text,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Copied,
    Sent,
    Confirmed,
    CopiedOnly,
    NativeSubmissionUnknown,
    NativeNotSubmitted,
    GenericSubmissionUnknown,
    GenericNotSubmitted,
}

// 通用提交已开始后，任何未发送/写入错误都终止旧请求，不能承诺仍可手动 Ctrl+V。
pub(crate) fn generic_outcome(
    result: Result<super::generic::Completion, String>,
    report: &mut serde_json::Value,
) -> Outcome {
    use super::generic::{Completion, Submission};
    match result {
        Ok(Completion::NotSubmitted(reason)) => {
            report["reason"] = serde_json::json!(reason);
            Outcome::GenericNotSubmitted
        }
        Err(reason) => {
            report["reason"] = serde_json::json!(reason);
            Outcome::GenericNotSubmitted
        }
        Ok(Completion::Submitted(Submission::Sent)) => Outcome::Sent,
        Ok(Completion::Submitted(Submission::Confirmed)) => Outcome::Confirmed,
        Ok(Completion::Submitted(Submission::Unknown)) => {
            report["reason"] = serde_json::json!("generic_submission_unknown");
            Outcome::GenericSubmissionUnknown
        }
        Ok(Completion::Submitted(Submission::Refused)) => unreachable!("拒绝发送已转为未提交"),
    }
}

// 只用于已选择原生策略的结果；锁前/写入错误也结束旧选择，不能重新开放已卸监听的面板。
pub(crate) fn native_outcome(
    result: Result<super::native::Completion, String>,
    report: &mut serde_json::Value,
) -> Outcome {
    use super::native::{Completion, Submission};
    match result {
        Ok(Completion::NotSubmitted(reason)) => {
            report["reason"] = serde_json::json!(reason);
            Outcome::NativeNotSubmitted
        }
        Err(reason) => {
            report["reason"] = serde_json::json!(reason);
            Outcome::NativeNotSubmitted
        }
        Ok(Completion::Submitted(Submission::Sent)) => Outcome::Sent,
        Ok(Completion::Submitted(Submission::Unknown)) => Outcome::NativeSubmissionUnknown,
        Ok(Completion::Submitted(Submission::Refused)) => {
            unreachable!("拒绝提交已转为未提交")
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestorePolicy {
    LegacyBeforeSubmit,
    GenericBeforeInput,
    NativeAfterSubmit { unknown: bool },
}

pub struct Callbacks<'a> {
    pub on_sent: &'a dyn Fn(),
    pub restore: &'a dyn Fn(RestorePolicy) -> Option<crate::platform::FocusResult>,
    pub synchronize: &'a dyn Fn() -> bool,
}

pub fn conclude(mode: Mode, sent: bool, confirmed: bool, degraded: bool) -> Outcome {
    if mode == Mode::Copy {
        Outcome::Copied
    } else if degraded || !sent {
        Outcome::CopiedOnly
    } else if confirmed {
        Outcome::Confirmed
    } else {
        Outcome::Sent
    }
}

pub struct Request {
    pub session: u64,
    pub target: Target,
    pub panel: usize,
    pub record_id: usize,
    pub content: Arc<Content>,
    pub mode: Mode,
    pub started: Instant,
    pub external_switch: bool,
    pub permit: Option<super::permit::Permit>,
    pub copy_only_reason: Option<&'static str>,
    pub cancelled: Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_content_evidence_confirms_a_successfully_sent_paste() {
        assert_eq!(conclude(Mode::Copy, false, false, false), Outcome::Copied);
        assert_eq!(conclude(Mode::Paste, true, false, false), Outcome::Sent);
        assert_eq!(conclude(Mode::Paste, true, true, false), Outcome::Confirmed);
        assert_eq!(
            conclude(Mode::Paste, false, true, false),
            Outcome::CopiedOnly
        );
        assert_eq!(conclude(Mode::Paste, true, true, true), Outcome::CopiedOnly);
        assert_eq!(conclude(Mode::Text, true, false, false), Outcome::Sent);
        assert_eq!(conclude(Mode::Text, true, true, false), Outcome::Confirmed);
    }
}
