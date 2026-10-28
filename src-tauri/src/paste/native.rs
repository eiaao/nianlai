// 定向原生提交的资格与一次性流程。平台操作通过 Backend 注入，回归不访问桌面或剪贴板。
#[derive(Clone, Copy)]
pub struct Eligibility<'a> {
    pub selection: bool,
    pub copy: bool,
    pub ordinary: bool,
    pub control_class: &'a str,
    // 合法 Unicode TEXT，且仅伴随已物化的 HTML / RTF。
    pub supported_text_formats: bool,
    pub builtin_default_ctrl_v: bool,
}

pub fn standard_edit(class: &str) -> bool {
    let class = class.to_ascii_lowercase();
    class == "edit" || class.starts_with("windowsforms10.edit.")
}

pub fn eligible(value: Eligibility<'_>) -> bool {
    value.selection
        && !value.copy
        && value.ordinary
        && standard_edit(value.control_class)
        && value.supported_text_formats
        && value.builtin_default_ctrl_v
}

pub fn ordinary_tokens(
    own: Option<u32>,
    own_elevated: Option<bool>,
    target: Option<u32>,
    target_elevated: Option<bool>,
) -> bool {
    own == Some(0x2000)
        && target == Some(0x2000)
        && own_elevated == Some(false)
        && target_elevated == Some(false)
}

pub fn input_desktop_ready(receiving_input: Option<bool>, revoke: impl FnOnce()) -> bool {
    if receiving_input == Some(true) {
        true
    } else {
        // 旧桌面的前台/按键状态不能证明当前仍可输入；恢复桌面也不能复用旧许可。
        revoke();
        false
    }
}

#[derive(Clone, Copy)]
pub struct Conditions {
    pub cancelled: bool,
    pub input_desktop: bool,
    pub target_valid: bool,
    pub permitted: bool,
    pub modifiers_released: bool,
    pub ordinary: bool,
}
impl Conditions {
    pub fn check(self) -> Result<(), &'static str> {
        if self.cancelled {
            Err("request_cancelled")
        } else if !self.input_desktop {
            Err("native_input_desktop_unavailable")
        } else if !self.target_valid {
            Err("invalid_target")
        } else if !self.permitted {
            Err("selection_boundary_changed")
        } else if !self.modifiers_released {
            Err("native_modifiers_held")
        } else if !self.ordinary {
            Err("target_may_be_elevated")
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Submission {
    Sent,
    Unknown,
    Refused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Delivery {
    pub attempted: bool,
    pub acknowledged: bool,
    pub error_code: Option<u32>,
}
impl Delivery {
    pub fn submission(self) -> Submission {
        if !self.attempted {
            Submission::Refused
        } else if self.acknowledged {
            Submission::Sent
        } else {
            Submission::Unknown
        }
    }
}

pub fn open_holder(window_observed: bool, process: u32, own: u32, target: u32) -> &'static str {
    if !window_observed {
        // NULL 也可能来自 OpenClipboard(NULL)；不能把它记录成剪贴板空闲。
        "unknown_null_or_unavailable"
    } else if process == 0 {
        "unknown_process"
    } else if process == own {
        "self_process"
    } else if process == target {
        "target_process"
    } else {
        "other_process"
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Completion {
    NotSubmitted(&'static str),
    Submitted(Submission),
}

pub trait Backend {
    fn synchronize(&mut self) -> bool;
    fn check(&mut self) -> Result<(), &'static str>;
    fn write(&mut self) -> Result<(), String>;
    fn prepare_submit(&mut self) -> Result<(), &'static str>;
    fn submit(&mut self) -> Result<Submission, &'static str>;
    // 无论是否成功都只清理一次；提交后的清理不得改变已提交/未知的事实。
    fn finish(&mut self, result: Option<Completion>);
}

pub fn run(backend: &mut impl Backend) -> Result<Completion, String> {
    let result = (|| {
        let before = guarded(backend);
        if before == Err("request_cancelled") {
            return Err("request_cancelled".into());
        }
        backend.write()?;
        if let Err(reason) = before {
            return Ok(Completion::NotSubmitted(reason));
        }
        if let Err(reason) = guarded(backend)
            .and_then(|()| backend.prepare_submit())
            .and_then(|()| guarded(backend))
        {
            return Ok(Completion::NotSubmitted(reason));
        }
        match backend.submit() {
            Err(reason) => Ok(Completion::NotSubmitted(reason)),
            Ok(Submission::Refused) => Ok(Completion::NotSubmitted("native_target_unavailable")),
            Ok(submission) => Ok(Completion::Submitted(submission)),
        }
    })();
    backend.finish(result.as_ref().ok().copied());
    result
}

pub(crate) fn guarded(backend: &mut impl Backend) -> Result<(), &'static str> {
    let synchronized = backend.synchronize();
    let check = backend.check();
    if check == Err("request_cancelled") || synchronized {
        check
    } else {
        Err("selection_boundary_changed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn eligibility() -> Eligibility<'static> {
        Eligibility {
            selection: true,
            copy: false,
            ordinary: true,
            control_class: "Edit",
            supported_text_formats: true,
            builtin_default_ctrl_v: true,
        }
    }
    #[test]
    fn qualification_excludes_every_unsupported_strategy() {
        assert!(eligible(eligibility()));
        assert!(eligible(Eligibility {
            control_class: "WindowsForms10.EDIT.app.0.test",
            ..eligibility()
        }));
        for value in [
            Eligibility {
                selection: false,
                ..eligibility()
            },
            Eligibility {
                copy: true,
                ..eligibility()
            },
            Eligibility {
                ordinary: false,
                ..eligibility()
            },
            Eligibility {
                supported_text_formats: false,
                ..eligibility()
            },
            Eligibility {
                builtin_default_ctrl_v: false,
                ..eligibility()
            },
            Eligibility {
                control_class: "RichEdit20W",
                ..eligibility()
            },
            Eligibility {
                control_class: "Chrome_RenderWidgetHostHWND",
                ..eligibility()
            },
            Eligibility {
                control_class: "",
                ..eligibility()
            },
        ] {
            assert!(!eligible(value));
        }
    }
    #[test]
    fn ordinary_permission_requires_both_medium_and_explicitly_unelevated() {
        assert!(ordinary_tokens(
            Some(0x2000),
            Some(false),
            Some(0x2000),
            Some(false)
        ));
        for (own, elevated, target, target_elevated) in [
            (Some(0x3000), Some(true), Some(0x3000), Some(true)),
            (Some(0x2000), None, Some(0x2000), Some(false)),
            (Some(0x2000), Some(false), None, Some(false)),
            (Some(0x2000), Some(false), Some(0x2000), None),
        ] {
            assert!(!ordinary_tokens(own, elevated, target, target_elevated));
        }
    }
    #[test]
    fn each_runtime_guard_refuses_before_any_native_submission() {
        let ready = Conditions {
            cancelled: false,
            input_desktop: true,
            target_valid: true,
            permitted: true,
            modifiers_released: true,
            ordinary: true,
        };
        assert_eq!(ready.check(), Ok(()));
        for (conditions, reason) in [
            (
                Conditions {
                    cancelled: true,
                    ..ready
                },
                "request_cancelled",
            ),
            (
                Conditions {
                    input_desktop: false,
                    ..ready
                },
                "native_input_desktop_unavailable",
            ),
            (
                Conditions {
                    target_valid: false,
                    ..ready
                },
                "invalid_target",
            ),
            (
                Conditions {
                    permitted: false,
                    ..ready
                },
                "selection_boundary_changed",
            ),
            (
                Conditions {
                    modifiers_released: false,
                    ..ready
                },
                "native_modifiers_held",
            ),
            (
                Conditions {
                    ordinary: false,
                    ..ready
                },
                "target_may_be_elevated",
            ),
        ] {
            assert_eq!(conditions.check(), Err(reason));
        }
    }

    #[test]
    fn input_desktop_requires_known_true_and_revokes_on_false_or_query_failure() {
        for status in [Some(true), Some(false), None] {
            let mut revoked = false;
            assert_eq!(
                input_desktop_ready(status, || revoked = true),
                status == Some(true)
            );
            assert_eq!(revoked, status != Some(true));
        }
    }

    struct Script {
        checks: VecDeque<Result<(), &'static str>>,
        operations: Vec<&'static str>,
        submission: Submission,
        writes_fail: bool,
        synchronized: bool,
        committed: usize,
        finished: Option<Completion>,
    }
    impl Script {
        fn new(checks: impl IntoIterator<Item = Result<(), &'static str>>) -> Self {
            Self {
                checks: checks.into_iter().collect(),
                operations: Vec::new(),
                submission: Submission::Sent,
                writes_fail: false,
                synchronized: true,
                committed: 0,
                finished: None,
            }
        }
    }
    impl Backend for Script {
        fn synchronize(&mut self) -> bool {
            self.operations.push("fence");
            self.synchronized
        }
        fn check(&mut self) -> Result<(), &'static str> {
            self.operations.push("check");
            self.checks.pop_front().expect("非预期的资格复核")
        }
        fn write(&mut self) -> Result<(), String> {
            self.operations.push("write");
            if self.writes_fail {
                Err("clipboard_write_failed".into())
            } else {
                Ok(())
            }
        }
        fn prepare_submit(&mut self) -> Result<(), &'static str> {
            self.operations.push("prepare");
            Ok(())
        }
        fn submit(&mut self) -> Result<Submission, &'static str> {
            self.operations.push("submit");
            self.committed += 1;
            Ok(self.submission)
        }
        fn finish(&mut self, result: Option<Completion>) {
            self.operations.push("release");
            if matches!(result, Some(Completion::Submitted(_))) {
                self.operations.extend(["activate", "hide", "blur"]);
            }
            self.finished = result;
        }
    }
    #[test]
    fn submission_finishes_before_activation_hiding_or_blurring() {
        let mut script = Script::new([Ok(()), Ok(()), Ok(())]);
        assert_eq!(
            run(&mut script).unwrap(),
            Completion::Submitted(Submission::Sent)
        );
        assert_eq!(
            script.operations,
            [
                "fence", "check", "write", "fence", "check", "prepare", "fence", "check", "submit",
                "release", "activate", "hide", "blur"
            ]
        );
        assert_eq!(script.committed, 1);
    }
    #[test]
    fn revocation_before_or_during_write_never_submits() {
        for checks in [
            vec![Err("selection_boundary_changed")],
            vec![Ok(()), Err("selection_boundary_changed")],
        ] {
            let mut script = Script::new(checks);
            assert_eq!(
                run(&mut script).unwrap(),
                Completion::NotSubmitted("selection_boundary_changed")
            );
            assert_eq!(script.committed, 0);
        }
    }
    #[test]
    fn cancellation_and_write_failure_do_not_submit_and_still_finish_once() {
        for checks in [
            vec![Err("request_cancelled")],
            vec![Ok(()), Err("request_cancelled")],
        ] {
            let mut script = Script::new(checks);
            let before_write = script.checks.len() == 1;
            if before_write {
                assert_eq!(run(&mut script).unwrap_err(), "request_cancelled");
            } else {
                assert_eq!(
                    run(&mut script).unwrap(),
                    Completion::NotSubmitted("request_cancelled")
                );
            }
            assert_eq!(script.committed, 0);
            assert_eq!(
                script
                    .operations
                    .iter()
                    .filter(|&&op| op == "release")
                    .count(),
                1
            );
            assert!(!script.operations.contains(&"activate"));
            assert_eq!(script.operations.contains(&"write"), !before_write);
        }
        let mut script = Script::new([Ok(())]);
        script.writes_fail = true;
        assert_eq!(run(&mut script).unwrap_err(), "clipboard_write_failed");
        assert_eq!(script.committed, 0);
    }
    #[test]
    fn timeout_can_have_committed_and_must_never_retry_or_change_to_copied_only() {
        let mut script = Script::new([Ok(()), Ok(()), Ok(())]);
        script.submission = Submission::Unknown;
        let expected = Completion::Submitted(Submission::Unknown);
        assert_eq!(run(&mut script).unwrap(), expected);
        assert_eq!(script.finished, Some(expected));
        assert_eq!(
            script.committed, 1,
            "即使目标已处理而调用超时，也只有一次提交"
        );
        assert_eq!(
            script
                .operations
                .iter()
                .filter(|&&op| op == "submit")
                .count(),
            1
        );
    }

    #[test]
    fn failed_fence_and_refused_delivery_are_never_reported_as_sent() {
        let mut script = Script::new([Ok(())]);
        script.synchronized = false;
        assert_eq!(
            run(&mut script).unwrap(),
            Completion::NotSubmitted("selection_boundary_changed")
        );
        assert_eq!(script.committed, 0);
        let refused = Delivery {
            attempted: false,
            acknowledged: false,
            error_code: None,
        };
        assert_eq!(refused.submission(), Submission::Refused);
        let unknown = Delivery {
            attempted: true,
            acknowledged: false,
            error_code: Some(1460),
        };
        assert_eq!(unknown.submission(), Submission::Unknown);
        let acknowledged = Delivery {
            attempted: true,
            acknowledged: true,
            error_code: None,
        };
        assert_eq!(acknowledged.submission(), Submission::Sent);
    }

    #[test]
    fn post_write_guard_failure_is_not_a_manual_paste_promise() {
        let mut script = Script::new([Ok(()), Err("selection_boundary_changed")]);
        let result = run(&mut script).unwrap();
        assert_eq!(
            result,
            Completion::NotSubmitted("selection_boundary_changed")
        );
        assert_eq!(
            script
                .operations
                .iter()
                .filter(|&&op| op == "write")
                .count(),
            1
        );
        assert_eq!(script.committed, 0);
    }

    #[test]
    fn cancellation_after_write_keeps_the_written_but_unsubmitted_fact() {
        let mut script = Script::new([Ok(()), Err("request_cancelled")]);
        assert_eq!(
            run(&mut script).unwrap(),
            Completion::NotSubmitted("request_cancelled")
        );
        assert_eq!(
            script
                .operations
                .iter()
                .filter(|&&op| op == "write")
                .count(),
            1
        );
        assert_eq!(script.committed, 0);
    }

    #[test]
    fn final_identity_refusal_is_not_a_manual_paste_promise() {
        let mut script = Script::new([Ok(()), Ok(()), Ok(())]);
        script.submission = Submission::Refused;
        assert_eq!(
            run(&mut script).unwrap(),
            Completion::NotSubmitted("native_target_unavailable")
        );
    }
}
