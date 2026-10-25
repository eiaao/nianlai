// 前台切换诊断：只补充证据，不把输入法、工具窗口或已返回面板的切换视为安全。
use super::{ime_association, window};
use crate::log::Logger;
use serde_json::{Value, json};
use std::{
    cell::Cell,
    ffi::OsStr,
    sync::atomic::{AtomicUsize, Ordering},
};
use windows::Win32::{System::SystemInformation::GetTickCount, UI::WindowsAndMessaging::*};

const TRACE_LIMIT: usize = 32;

// 保留原策略：第一次外部切换一直保留到本次钩子结束，返回面板不清除。
pub(super) fn record_first_external(
    switched: &AtomicUsize,
    foreground: usize,
    panel: usize,
    target: usize,
) -> bool {
    foreground != 0
        && foreground != panel
        && foreground != target
        && switched
            .compare_exchange(0, foreground, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
}

fn enabled(value: Option<&OsStr>) -> bool {
    value == Some(OsStr::new("1"))
}

#[derive(Debug, PartialEq)]
enum Slot {
    Event(usize),
    Truncated,
    Ignore,
}

#[derive(Default)]
struct Budget(Cell<usize>);
impl Budget {
    fn next(&self) -> Slot {
        let count = self.0.get();
        if count > TRACE_LIMIT {
            return Slot::Ignore;
        }
        self.0.set(count + 1);
        if count == TRACE_LIMIT {
            Slot::Truncated
        } else {
            Slot::Event(count + 1)
        }
    }
}

pub(super) struct Trace {
    logger: Logger,
    session: u64,
    panel: usize,
    target: usize,
    budget: Budget,
    association: ime_association::Session,
}

impl Trace {
    pub(super) fn from_env(
        logger: &Logger,
        session: u64,
        panel: usize,
        target: usize,
    ) -> Option<Self> {
        if !enabled(std::env::var_os("NIANLAI_G0_FOREGROUND_TRACE").as_deref()) {
            return None;
        }
        logger.event(
            "foreground_trace_started",
            json!({"session":session,"limit":TRACE_LIMIT}),
        );
        Some(Self {
            logger: logger.clone(),
            session,
            panel,
            target,
            budget: Budget::default(),
            association: ime_association::Session::new(logger, session, panel, target),
        })
    }

    pub(super) fn record(
        &self,
        event_window: usize,
        event_thread: u32,
        event_time: u32,
        latched: bool,
    ) {
        match self.budget.next() {
            Slot::Event(sequence) => {
                // 只投递元数据任务；UIA/IMM 查询由单一后台线程执行，不进入钩子回调。
                self.association
                    .record(sequence, event_window, event_thread, event_time);
                // WinEvent 时间使用开机毫秒计数，不能当成 UTC；wrapping_sub 处理 32 位回绕。
                let delay = unsafe { GetTickCount() }.wrapping_sub(event_time);
                let current = window::foreground();
                self.logger.event(
                    "foreground_trace",
                    json!({"session":self.session,"sequence":sequence,
                        "win_event_time_ms":event_time,"delivery_delay_ms":delay,
                        "event_thread":event_thread,"first_external_latched":latched,
                        "event_window":self.snapshot(event_window),
                        "current_window":self.snapshot(current),
                        "event_is_current":event_window == current}),
                );
            }
            Slot::Truncated => self.logger.event(
                "foreground_trace_truncated",
                json!({"session":self.session,"limit":TRACE_LIMIT}),
            ),
            Slot::Ignore => {}
        }
    }

    fn role(&self, raw: usize) -> &'static str {
        if raw == 0 {
            "none"
        } else if raw == self.panel {
            "panel"
        } else if raw == self.target {
            "target"
        } else {
            "other"
        }
    }

    fn snapshot(&self, raw: usize) -> Value {
        // 只取元数据；不读窗口标题、控件文本、剪贴板或按键，不查询进程路径。
        unsafe {
            let handle = window::hwnd(raw);
            let valid = IsWindow(Some(handle)).as_bool();
            if !valid {
                return json!({"role":self.role(raw),"valid":false});
            }
            let mut pid = 0;
            let thread = GetWindowThreadProcessId(handle, Some(&mut pid));
            let owner = GetWindow(handle, GW_OWNER).unwrap_or_default().0 as usize;
            let root_owner = GetAncestor(handle, GA_ROOTOWNER).0 as usize;
            let style = GetWindowLongPtrW(handle, GWL_EXSTYLE);
            let mut info = GUITHREADINFO {
                cbSize: size_of::<GUITHREADINFO>() as u32,
                ..Default::default()
            };
            let focus_known = thread != 0 && GetGUIThreadInfo(thread, &mut info).is_ok();
            json!({"role":self.role(raw),"valid":true,"class":window::class_name(raw),
                "pid":pid,"thread":thread,"visible":IsWindowVisible(handle).as_bool(),
                "owner_role":self.role(owner),"owner_class":window::class_name(owner),
                "root_owner_role":self.role(root_owner),"root_owner_class":window::class_name(root_owner),
                "no_activate":style & WS_EX_NOACTIVATE.0 as isize != 0,
                "tool_window":style & WS_EX_TOOLWINDOW.0 as isize != 0,
                "focus_known":focus_known,
                "focus_role":if focus_known {Some(self.role(info.hwndFocus.0 as usize))} else {None},
                "focus_class":if focus_known {Some(window::class_name(info.hwndFocus.0 as usize))} else {None}})
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_target_and_empty_events_do_not_mark_a_switch() {
        let switched = AtomicUsize::new(0);
        for foreground in [0, 10, 20, 10] {
            assert!(!record_first_external(&switched, foreground, 10, 20));
        }
        assert_eq!(switched.load(Ordering::Acquire), 0);
    }

    #[test]
    fn returning_from_an_ime_window_does_not_erase_the_first_switch() {
        let switched = AtomicUsize::new(0);
        // 复现旧日志反映的规则：输入法临时窗口 30 与其他外部窗口同样锁存。
        assert!(record_first_external(&switched, 30, 10, 20));
        for foreground in [10, 20, 0, 40] {
            assert!(!record_first_external(&switched, foreground, 10, 20));
        }
        assert_eq!(switched.load(Ordering::Acquire), 30);
    }

    #[test]
    fn an_ime_event_cannot_replace_an_earlier_real_external_switch() {
        let switched = AtomicUsize::new(0);
        assert!(record_first_external(&switched, 40, 10, 20));
        assert!(!record_first_external(&switched, 30, 10, 20));
        assert_eq!(switched.load(Ordering::Acquire), 40);
    }

    #[test]
    fn diagnostics_require_the_exact_opt_in_value() {
        assert!(!enabled(None));
        for value in ["", "0", "true", " 1", "1 "] {
            assert!(!enabled(Some(OsStr::new(value))));
        }
        assert!(enabled(Some(OsStr::new("1"))));
    }

    #[test]
    fn diagnostic_limit_does_not_disable_switch_detection() {
        let budget = Budget::default();
        let switched = AtomicUsize::new(0);
        for sequence in 1..=TRACE_LIMIT {
            assert_eq!(budget.next(), Slot::Event(sequence));
        }
        assert_eq!(budget.next(), Slot::Truncated);
        for _ in 0..100 {
            assert_eq!(budget.next(), Slot::Ignore);
        }
        assert!(record_first_external(&switched, 40, 10, 20));
        assert_eq!(switched.load(Ordering::Acquire), 40);
        assert_eq!(Budget::default().next(), Slot::Event(1));
    }
}
