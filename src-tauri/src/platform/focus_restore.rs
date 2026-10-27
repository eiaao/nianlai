// 恢复流程与 Win32 调用分开，回归直接驱动生产流程而不访问桌面。
use super::{FocusResult, ForegroundResult};

#[derive(Clone, Copy)]
pub(crate) struct Snapshot {
    pub focus: FocusResult,
    pub target_valid: bool,
    pub original_valid: bool,
    pub permitted: bool,
}
impl Snapshot {
    fn may_focus(self) -> bool {
        self.target_valid
            && self.original_valid
            && self.permitted
            && self.focus.foreground.success
            && !self.focus.focus_unknown
    }
}

pub(crate) trait Backend {
    type Attachment;
    fn snapshot(&mut self) -> Snapshot;
    fn activate(&mut self) -> ForegroundResult;
    fn attach(&mut self) -> Self::Attachment;
    fn set_focus(&mut self);
}

pub(crate) fn run(
    backend: &mut impl Backend,
    mut observe: impl FnMut(&'static str, Snapshot),
) -> FocusResult {
    let before = backend.snapshot();
    observe("before_activate", before);
    if !before.target_valid || !before.permitted {
        return before.focus;
    }
    let activation = backend.activate();
    let mut snapshot = backend.snapshot();
    observe("after_activate", snapshot);
    if activation.success && snapshot.may_focus() && !snapshot.focus.focus_restored {
        let attached = backend.attach();
        // 连接输入队列也可能促成目标自己的焦点恢复，或与用户切走同时发生。
        // 第二次快照仍安全且焦点确实缺失，才执行原有单次 SetFocus fallback。
        snapshot = backend.snapshot();
        observe("after_attach", snapshot);
        if snapshot.may_focus() && !snapshot.focus.focus_restored {
            backend.set_focus();
        }
        drop(attached);
        snapshot = backend.snapshot();
        observe("after_fallback", snapshot);
    }
    snapshot.focus.foreground.path = activation.path;
    snapshot.focus
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::VecDeque, rc::Rc};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Operation {
        Activate,
        Attach,
        SetFocus,
        Detach,
    }
    struct Attachment(Rc<RefCell<Vec<Operation>>>);
    impl Drop for Attachment {
        fn drop(&mut self) {
            self.0.borrow_mut().push(Operation::Detach);
        }
    }
    struct Script {
        snapshots: VecDeque<Snapshot>,
        operations: Rc<RefCell<Vec<Operation>>>,
        activation_succeeds: bool,
    }
    impl Script {
        fn new(snapshots: impl IntoIterator<Item = Snapshot>) -> Self {
            Self {
                snapshots: snapshots.into_iter().collect(),
                operations: Default::default(),
                activation_succeeds: true,
            }
        }
        fn restore(&mut self) -> FocusResult {
            let mut phases = Vec::new();
            let result = run(self, |phase, _| phases.push(phase));
            assert!(phases.len() <= 4, "单次归还只记录固定数量的阶段");
            assert!(self.snapshots.is_empty(), "必须执行完整的平台脚本");
            result
        }
        fn assert_operations(&self, expected: &[Operation]) {
            assert_eq!(*self.operations.borrow(), expected);
        }
    }
    impl Backend for Script {
        type Attachment = Attachment;
        fn snapshot(&mut self) -> Snapshot {
            self.snapshots.pop_front().expect("非预期的平台快照")
        }
        fn activate(&mut self) -> ForegroundResult {
            self.operations.borrow_mut().push(Operation::Activate);
            ForegroundResult {
                path: "scripted_activation",
                success: self.activation_succeeds,
            }
        }
        fn attach(&mut self) -> Attachment {
            self.operations.borrow_mut().push(Operation::Attach);
            Attachment(self.operations.clone())
        }
        fn set_focus(&mut self) {
            self.operations.borrow_mut().push(Operation::SetFocus);
        }
    }
    fn snapshot(foreground: bool, restored: bool) -> Snapshot {
        Snapshot {
            focus: FocusResult {
                foreground: ForegroundResult {
                    path: "unchanged",
                    success: foreground,
                },
                focus_restored: restored,
                focus_unknown: false,
            },
            target_valid: true,
            original_valid: true,
            permitted: true,
        }
    }
    fn panel() -> Snapshot {
        snapshot(false, false)
    }
    fn pending() -> Snapshot {
        snapshot(true, false)
    }
    fn restored() -> Snapshot {
        snapshot(true, true)
    }

    #[test]
    fn activation_that_restores_original_focus_has_no_extra_side_effects() {
        let mut script = Script::new([panel(), restored()]);
        let result = script.restore();
        assert!(result.foreground.success && result.focus_restored);
        assert_eq!(result.foreground.path, "scripted_activation");
        script.assert_operations(&[Operation::Activate]);
    }

    #[test]
    fn missing_focus_uses_one_guarded_fallback_and_detaches_before_confirmation() {
        let mut script = Script::new([panel(), pending(), pending(), restored()]);
        assert!(script.restore().focus_restored);
        script.assert_operations(&[
            Operation::Activate,
            Operation::Attach,
            Operation::SetFocus,
            Operation::Detach,
        ]);
    }

    #[test]
    fn invalid_target_or_revoked_permission_never_activates() {
        for invalid_target in [true, false] {
            let mut before = panel();
            before.target_valid = !invalid_target;
            before.permitted = invalid_target;
            let mut script = Script::new([before]);
            assert!(!script.restore().foreground.success);
            script.assert_operations(&[]);
        }
    }

    #[test]
    fn failed_activation_never_attaches_or_sets_focus() {
        let mut script = Script::new([panel(), panel()]);
        script.activation_succeeds = false;
        assert!(!script.restore().foreground.success);
        script.assert_operations(&[Operation::Activate]);
    }

    #[test]
    fn foreground_identity_and_permission_are_rechecked_after_activation() {
        for state in [
            snapshot(false, false),
            Snapshot {
                original_valid: false,
                ..pending()
            },
            Snapshot {
                permitted: false,
                ..pending()
            },
        ] {
            let mut script = Script::new([panel(), state]);
            script.restore();
            script.assert_operations(&[Operation::Activate]);
        }
    }

    #[test]
    fn focus_restored_during_attachment_does_not_get_set_again() {
        let mut script = Script::new([panel(), pending(), restored(), restored()]);
        assert!(script.restore().focus_restored);
        script.assert_operations(&[Operation::Activate, Operation::Attach, Operation::Detach]);
    }

    #[test]
    fn switching_away_or_revoking_during_attachment_never_sets_focus() {
        for changed in [
            snapshot(false, false),
            Snapshot {
                original_valid: false,
                ..pending()
            },
            // 前台已返回，但监听器保留了期间切走的撤销结果。
            Snapshot {
                permitted: false,
                ..pending()
            },
        ] {
            let mut script = Script::new([panel(), pending(), changed, changed]);
            script.restore();
            script.assert_operations(&[Operation::Activate, Operation::Attach, Operation::Detach]);
        }
    }

    #[test]
    fn unknown_original_control_still_restores_foreground_without_setting_focus() {
        let unknown = Snapshot {
            original_valid: false,
            focus: FocusResult {
                focus_unknown: true,
                ..restored().focus
            },
            ..restored()
        };
        let mut script = Script::new([
            Snapshot {
                original_valid: false,
                ..panel()
            },
            unknown,
        ]);
        assert!(script.restore().foreground.success);
        script.assert_operations(&[Operation::Activate]);
    }
}
