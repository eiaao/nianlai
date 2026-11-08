pub mod rules;
pub mod sensitive;

pub const MARKERS: [&str; 4] = [
    "Clipboard Viewer Ignore",
    "ExcludeClipboardContentFromMonitorProcessing",
    "CanIncludeInClipboardHistory",
    "CanUploadToCloudClipboard",
];

pub fn blocking_marker<'a>(
    names: impl IntoIterator<Item = &'a str>,
    mut read: impl FnMut(&str) -> Option<u32>,
) -> Option<&'static str> {
    let names: Vec<_> = names.into_iter().collect();
    for (index, marker) in MARKERS.iter().enumerate() {
        if names.contains(marker) && (index < 2 || read(marker).is_none_or(|value| value == 0)) {
            return Some(marker);
        }
    }
    None
}

pub struct SelfCheck {
    last_notified: u32,
    suspicious: bool,
}

impl SelfCheck {
    pub fn new(sequence: u32) -> Self {
        Self {
            last_notified: sequence,
            suspicious: false,
        }
    }
    pub fn notified(&mut self, sequence: u32) {
        self.last_notified = sequence;
        self.suspicious = false;
    }
    pub fn check(&mut self, sequence: u32, processed: Option<u32>) -> bool {
        // 读取线程处理过的序列号也算已知：来源程序在我们读取时才补交延迟渲染的数据，
        // 如果这会让序列号变化，却不会再有通知。
        if sequence == self.last_notified || processed == Some(sequence) {
            self.suspicious = false;
            return false;
        }
        let failed = self.suspicious;
        self.suspicious = true;
        failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_markers_never_read_and_dwords_fail_closed() {
        for marker in &MARKERS[..2] {
            assert_eq!(
                blocking_marker([*marker], |_| panic!("存在型标记无需读取")),
                Some(*marker)
            );
        }
        for marker in &MARKERS[2..] {
            for value in [Some(0), None] {
                assert_eq!(blocking_marker([*marker], |_| value), Some(*marker));
            }
            for value in [1, 2, u32::MAX] {
                assert_eq!(blocking_marker([*marker], |_| Some(value)), None);
            }
        }
        assert_eq!(
            blocking_marker(["CF_UNICODETEXT"], |_| panic!("无标记")),
            None
        );
        assert_eq!(
            blocking_marker(MARKERS, |_| panic!("先处理存在型标记")),
            Some(MARKERS[0])
        );
    }

    #[test]
    fn watchdog_needs_two_misses_and_a_notification_resets_suspicion() {
        let mut check = SelfCheck::new(1);
        assert!(!check.check(1, None));
        assert!(!check.check(2, None));
        check.notified(2);
        assert!(!check.check(3, None));
        assert!(check.check(4, None));
        check.notified(4);
        assert!(!check.check(4, None));
        // 读取线程已经处理过的序列号不算失效，也会清掉可疑状态。
        assert!(!check.check(5, None));
        assert!(!check.check(5, Some(5)));
        assert!(!check.check(6, Some(5)));
        assert!(check.check(6, Some(5)));
    }
}
