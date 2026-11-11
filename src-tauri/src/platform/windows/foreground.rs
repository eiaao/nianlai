// 前台切换事件与“第一次外部切换”锁存。
use std::sync::atomic::{AtomicUsize, Ordering};
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Copy)]
pub(super) struct Event {
    pub kind: u32,
    pub window: usize,
    pub object: i32,
    pub child: i32,
    pub time: u32,
}
impl Event {
    pub(super) fn is_foreground(self) -> bool {
        self.kind == EVENT_SYSTEM_FOREGROUND && self.object == 0 && self.child == 0
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exact_foreground_window_event_is_expected() {
        let event = Event {
            kind: EVENT_SYSTEM_FOREGROUND,
            window: 20,
            object: 0,
            child: 0,
            time: 100,
        };
        assert!(event.is_foreground());
        assert!(
            !Event {
                kind: EVENT_OBJECT_FOCUS,
                ..event
            }
            .is_foreground()
        );
        assert!(
            !Event {
                object: -4,
                ..event
            }
            .is_foreground()
        );
        assert!(!Event { child: 1, ..event }.is_foreground());
    }

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
}
