//! 一次粘贴的结果：平台层返回，面板据此决定下一步，同时序列化成日志行 `paste` 和前端事件 `panel:paste-result`。
//! 判断用的字段（结论、原因、是否写入、是否被取消、是否被动提示）是有类型的；
//! 只给日志看的证据留在 `evidence` 里，序列化时和它们平铺在同一层。

use serde::Serialize;
use serde_json::{Map, Value};

/// 日志和前端里的 `paste_result` 取值。
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PasteResult {
    Copied,
    Sent,
    Confirmed,
    CopiedOnly,
    NativeSubmissionUnknown,
    NativeNotSubmitted,
    GenericSubmissionUnknown,
    GenericNotSubmitted,
}

macro_rules! reasons {
    ($($variant:ident => $text:literal,)*) => {
        /// 日志和前端里的 `reason` 取值。底层偶尔会冒出没列在这里的字符串，原样保留在 `Other` 里。
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub enum Reason {
            $($variant,)*
            Other(String),
        }

        impl Reason {
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $text,)*
                    Self::Other(text) => text,
                }
            }
        }

        impl From<&str> for Reason {
            fn from(text: &str) -> Self {
                match text {
                    $($text => Self::$variant,)*
                    other => Self::Other(other.to_owned()),
                }
            }
        }

        #[cfg(test)]
        pub(crate) const KNOWN_REASONS: &[(Reason, &str)] = &[$((Reason::$variant, $text),)*];
    };
}

reasons! {
    RequestCancelled => "request_cancelled",
    UserCopy => "user_copy",
    InvalidTarget => "invalid_target",
    ExternalSwitch => "external_switch",
    TargetControlUnverified => "target_control_unverified",
    SelectionBoundaryChanged => "selection_boundary_changed",
    TargetMayBeElevated => "target_may_be_elevated",
    ConditionsChanged => "conditions_changed",
    PreflightTimeout => "preflight_timeout",
    SendFailed => "send_failed",
    RecordMissing => "record_missing",
    FilesMissing => "files_missing",
    StoreReadFailed => "store_read_failed",
    ClipboardCaptureBusy => "clipboard_capture_busy",
    ClipboardBusy => "clipboard_busy",
    ClipboardWriteFailed => "clipboard_write_failed",
    ClipboardAllocateFailed => "clipboard_allocate_failed",
    ClipboardLockFailed => "clipboard_lock_failed",
    ClipboardRenderFailed => "clipboard_render_failed",
    ClipboardRegisterFormatFailed => "clipboard_register_format_failed",
    ClipboardEmptyFailed => "clipboard_empty_failed",
    ClipboardMarkerFailed => "clipboard_marker_failed",
    ClipboardFormatFailed => "clipboard_format_failed",
    ClipboardModuleFailed => "clipboard_module_failed",
    ClipboardClassFailed => "clipboard_class_failed",
    ClipboardWindowFailed => "clipboard_window_failed",
    ClipboardThreadFailed => "clipboard_thread_failed",
    ClipboardStartFailed => "clipboard_start_failed",
    ClipboardStopped => "clipboard_stopped",
    ClipboardPostFailed => "clipboard_post_failed",
    NativeClipboardBusy => "native_clipboard_busy",
    NativeClipboardChanged => "native_clipboard_changed",
    NativeClipboardCloseFailed => "native_clipboard_close_failed",
    NativeInputDesktopUnavailable => "native_input_desktop_unavailable",
    NativeModifiersHeld => "native_modifiers_held",
    NativeTargetUnavailable => "native_target_unavailable",
    NativeSubmissionUnknown => "native_submission_unknown",
    NativeEditInvalidText => "native_edit_invalid_text",
    NativeEditQueryFailed => "native_edit_query_failed",
    NativeEditQueryTimeout => "native_edit_query_timeout",
    NativeEditReadOnly => "native_edit_read_only",
    NativeEditStateChanged => "native_edit_state_changed",
    NativeTransportAlreadySelected => "native_transport_already_selected",
    NativeTransportNotSelected => "native_transport_not_selected",
    GenericClipboardBusy => "generic_clipboard_busy",
    GenericClipboardChanged => "generic_clipboard_changed",
    GenericClipboardCloseFailed => "generic_clipboard_close_failed",
    GenericInputDesktopUnavailable => "generic_input_desktop_unavailable",
    GenericModifiersHeld => "generic_modifiers_held",
    GenericRestoreUnavailable => "generic_restore_unavailable",
    GenericSubmissionUnknown => "generic_submission_unknown",
}

impl Serialize for Reason {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    /// `None` 序列化为 null：线程出错或读库失败时没有结论。
    pub result: Option<PasteResult>,
    pub reason: Option<Reason>,
    pub clipboard_written: bool,
    pub request_cancelled: bool,
    /// 已复制、需要被动提示用户手动粘贴（只在为真时才出现在日志里）。
    pub passive_hint: bool,
    /// 只给日志和前端看的证据，不参与判断。
    pub evidence: Map<String, Value>,
    // 读库失败这类结果从来不带 paste_result 键；为了日志逐字段不变，序列化时也不带。
    omit_result: bool,
}

impl Outcome {
    /// 平台层的结果：`report` 里取出判断用的字段，其余都当证据。
    pub fn from_report(result: Option<PasteResult>, report: Value) -> Self {
        let Value::Object(mut evidence) = report else {
            unreachable!("粘贴报告总是对象")
        };
        evidence.remove("paste_result");
        let reason = match evidence.remove("reason") {
            Some(Value::String(text)) => Some(Reason::from(text.as_str())),
            _ => None,
        };
        let clipboard_written = evidence.remove("clipboard_written") == Some(Value::Bool(true));
        let passive_hint = evidence.remove("passive_hint") == Some(Value::Bool(true));
        Self {
            result,
            reason,
            clipboard_written,
            request_cancelled: false,
            passive_hint,
            evidence,
            omit_result: false,
        }
    }

    /// 没走到粘贴就结束（记录不在、文件不在、读库失败）：没写剪贴板，也没有 paste_result。
    pub fn not_written(reason: Reason) -> Self {
        Self {
            result: None,
            reason: Some(reason),
            clipboard_written: false,
            request_cancelled: false,
            passive_hint: false,
            evidence: Map::new(),
            omit_result: true,
        }
    }

    /// 日志行和前端载荷的 JSON。
    pub fn to_value(&self) -> Value {
        let mut map = self.evidence.clone();
        if !self.omit_result {
            map.insert("paste_result".into(), to_json(&self.result));
        }
        map.insert("reason".into(), to_json(&self.reason));
        map.insert(
            "clipboard_written".into(),
            Value::Bool(self.clipboard_written),
        );
        map.insert(
            "request_cancelled".into(),
            Value::Bool(self.request_cancelled),
        );
        if self.passive_hint {
            map.insert("passive_hint".into(), Value::Bool(true));
        }
        Value::Object(map)
    }
}

fn to_json<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("枚举序列化不会失败")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn paste_result_strings_are_the_logged_and_frontend_strings() {
        for (variant, text) in [
            (PasteResult::Copied, "copied"),
            (PasteResult::Sent, "sent"),
            (PasteResult::Confirmed, "confirmed"),
            (PasteResult::CopiedOnly, "copied_only"),
            (
                PasteResult::NativeSubmissionUnknown,
                "native_submission_unknown",
            ),
            (PasteResult::NativeNotSubmitted, "native_not_submitted"),
            (
                PasteResult::GenericSubmissionUnknown,
                "generic_submission_unknown",
            ),
            (PasteResult::GenericNotSubmitted, "generic_not_submitted"),
        ] {
            assert_eq!(serde_json::to_value(variant).unwrap(), json!(text));
        }
    }

    #[test]
    fn every_reason_variant_serializes_to_its_original_string() {
        for (variant, text) in KNOWN_REASONS {
            assert_eq!(to_json(variant), json!(text), "{text}");
            assert_eq!(&Reason::from(*text), variant, "{text}");
        }
        let mut seen = std::collections::HashSet::new();
        assert!(KNOWN_REASONS.iter().all(|(_, text)| seen.insert(*text)));
        // 没列出的字符串原样保留。
        let other = Reason::from("some_future_reason");
        assert_eq!(other, Reason::Other("some_future_reason".into()));
        assert_eq!(to_json(&other), json!("some_future_reason"));
    }

    #[test]
    fn outcome_json_keeps_the_original_report_shape() {
        let report = json!({"paste_result":null,"reason":"x_unlisted","clipboard_written":true,
            "passive_hint":true,"integrity":{"status":"allowed"}});
        let mut outcome = Outcome::from_report(Some(PasteResult::CopiedOnly), report);
        outcome.reason = Some(Reason::TargetMayBeElevated);
        outcome.request_cancelled = true;
        outcome.evidence.insert("store_read_ms".into(), json!(1.5));
        assert_eq!(
            outcome.to_value(),
            json!({"paste_result":"copied_only","reason":"target_may_be_elevated",
                "clipboard_written":true,"request_cancelled":true,"passive_hint":true,
                "integrity":{"status":"allowed"},"store_read_ms":1.5})
        );
        // 执行出错时 paste_result 和 reason 都是 null 键，passive_hint 不出现。
        let none = Outcome::from_report(
            None,
            json!({"paste_result":null,"reason":null,"clipboard_written":false}),
        );
        assert_eq!(
            none.to_value(),
            json!({"paste_result":null,"reason":null,"clipboard_written":false,"request_cancelled":false})
        );
        // 读库失败：没有 paste_result 键。
        let mut missing = Outcome::not_written(Reason::StoreReadFailed);
        missing.evidence.insert("error_code".into(), json!("e"));
        assert_eq!(
            missing.to_value(),
            json!({"reason":"store_read_failed","clipboard_written":false,
                "request_cancelled":false,"error_code":"e"})
        );
    }
}
