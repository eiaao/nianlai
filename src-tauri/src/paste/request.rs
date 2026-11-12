pub use super::outcome::PasteResult;
use crate::{history::content::Content, platform::Target};
use serde::Deserialize;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

/// 普通粘贴的修饰键宽限：发键前确认最长等 300ms，期间修饰键仍按着才补发抬起（原行为）。
pub const MODIFIER_GRACE: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Paste,
    Copy,
    // Ctrl+Enter：先把文件、图片、富文本转成文字，再照常粘贴。
    Text,
}

// 通用提交已开始后，任何未发送/写入错误都终止旧请求，不能承诺仍可手动 Ctrl+V。
pub(crate) fn generic_outcome(
    result: Result<super::generic::Completion, String>,
    report: &mut serde_json::Value,
) -> PasteResult {
    use super::generic::{Completion, Submission};
    match result {
        Ok(Completion::NotSubmitted(reason)) => {
            report["reason"] = serde_json::json!(reason);
            PasteResult::GenericNotSubmitted
        }
        Err(reason) => {
            report["reason"] = serde_json::json!(reason);
            PasteResult::GenericNotSubmitted
        }
        Ok(Completion::Submitted(Submission::Sent)) => PasteResult::Sent,
        Ok(Completion::Submitted(Submission::Confirmed)) => PasteResult::Confirmed,
        Ok(Completion::Submitted(Submission::Unknown)) => {
            report["reason"] = serde_json::json!("generic_submission_unknown");
            PasteResult::GenericSubmissionUnknown
        }
        Ok(Completion::Submitted(Submission::Refused)) => unreachable!("拒绝发送已转为未提交"),
    }
}

// 只用于已选择原生策略的结果；锁前/写入错误也结束旧选择，不能重新开放已卸监听的面板。
pub(crate) fn native_outcome(
    result: Result<super::native::Completion, String>,
    report: &mut serde_json::Value,
) -> PasteResult {
    use super::native::{Completion, Submission};
    match result {
        Ok(Completion::NotSubmitted(reason)) => {
            report["reason"] = serde_json::json!(reason);
            PasteResult::NativeNotSubmitted
        }
        Err(reason) => {
            report["reason"] = serde_json::json!(reason);
            PasteResult::NativeNotSubmitted
        }
        Ok(Completion::Submitted(Submission::Sent)) => PasteResult::Sent,
        Ok(Completion::Submitted(Submission::Unknown)) => PasteResult::NativeSubmissionUnknown,
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

pub fn conclude(mode: Mode, sent: bool, confirmed: bool, degraded: bool) -> PasteResult {
    if mode == Mode::Copy {
        PasteResult::Copied
    } else if degraded || !sent {
        PasteResult::CopiedOnly
    } else if confirmed {
        PasteResult::Confirmed
    } else {
        PasteResult::Sent
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
    // 一叠一起放下（automatic 队列）且这一张不是最后一条时置 true：实际写入的文本尾部
    // 追加分隔（as_text::append_separator；单行编辑框用空格，其余换行）。指纹按实际写入登记回声。
    pub text_separator: bool,
    /// 发键前确认里，其余条件都满足、只剩修饰键按着时，等多久才补发修饰键抬起。
    /// 普通粘贴固定 `MODIFIER_GRACE`（与原来的 300ms 确认期一致）；逐张放是用户按住 Ctrl
    /// 连按 V 的明确操作，传 0 立即补抬起，不再白等。
    pub modifier_grace: Duration,
    /// 逐张放由剪贴板线程据此取得写租约；普通单条粘贴为 None，行为保持不变。
    pub clipboard_guard: Option<Arc<super::queue::Guard>>,
    pub cancelled: Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_content_evidence_confirms_a_successfully_sent_paste() {
        assert_eq!(
            conclude(Mode::Copy, false, false, false),
            PasteResult::Copied
        );
        assert_eq!(conclude(Mode::Paste, true, false, false), PasteResult::Sent);
        assert_eq!(
            conclude(Mode::Paste, true, true, false),
            PasteResult::Confirmed
        );
        assert_eq!(
            conclude(Mode::Paste, false, true, false),
            PasteResult::CopiedOnly
        );
        assert_eq!(
            conclude(Mode::Paste, true, true, true),
            PasteResult::CopiedOnly
        );
        assert_eq!(conclude(Mode::Text, true, false, false), PasteResult::Sent);
        assert_eq!(
            conclude(Mode::Text, true, true, false),
            PasteResult::Confirmed
        );
    }
}
