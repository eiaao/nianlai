//! 检查节流、“每个新版本只通知一次”和界面显示条件（纯状态机，时间由调用方传入，便于测试）。

use super::feed::{self, Latest};
use serde::{Deserialize, Serialize};

pub const FIRST_DELAY_MS: u64 = 90_000;
pub const PREVIEW_FIRST_DELAY_MS: u64 = 4_000;
pub const CHECK_INTERVAL_MS: u64 = 24 * 60 * 60 * 1000;
/// 上次检查失败后的重试间隔，避免离线时每天只试一次又不至于频繁打扰。
pub const RETRY_INTERVAL_MS: u64 = 60 * 60 * 1000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckResult {
    /// 还没检查过。
    #[default]
    None,
    /// 已是最新。
    Latest,
    /// 发现新版本。
    New,
    Failed,
}

/// 会持久化的更新状态（存在设置文件的 `update` 分组里，除 `auto_check` 外由程序自己维护）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub last_check_ms: Option<u64>,
    pub last_result: CheckResult,
    pub latest: Option<Latest>,
    pub notified_version: Option<String>,
    pub skipped_version: Option<String>,
    pub banner_dismissed_version: Option<String>,
}

/// 现在是否该做一次后台检查。
pub fn check_due(
    auto_check: bool,
    now_ms: u64,
    started_ms: u64,
    first_delay_ms: u64,
    state: &State,
) -> bool {
    if !auto_check || now_ms.saturating_sub(started_ms) < first_delay_ms {
        return false;
    }
    let Some(last) = state.last_check_ms else {
        return true;
    };
    // 系统时间被往回拨时，上次检查时间会在“未来”；按到期处理，检查后会写回当前时间。
    if last > now_ms {
        return true;
    }
    let interval = if state.last_result == CheckResult::Failed {
        RETRY_INTERVAL_MS
    } else {
        CHECK_INTERVAL_MS
    };
    now_ms - last >= interval
}

impl State {
    /// 记录一次成功的检查结果。已知版本变化时，重置与旧版本相关的“已看过 / 横幅已关闭”标记不需要：
    /// 这些标记按版本号比较，换了版本自然失效。
    pub fn record_success(&mut self, now_ms: u64, current: &str, latest: Latest) {
        self.last_check_ms = Some(now_ms);
        if feed::is_newer(&latest.version, current) {
            self.last_result = CheckResult::New;
            self.latest = Some(latest);
        } else {
            self.last_result = CheckResult::Latest;
            self.latest = None;
        }
    }

    pub fn record_failure(&mut self, now_ms: u64) {
        self.last_check_ms = Some(now_ms);
        self.last_result = CheckResult::Failed;
    }

    /// 当前可用的新版本；缓存里的版本如果已经不比当前新（用户手动升级过），视为没有。
    pub fn available(&self, current: &str) -> Option<&Latest> {
        self.latest
            .as_ref()
            .filter(|latest| feed::is_newer(&latest.version, current))
    }

    fn is(slot: &Option<String>, version: &str) -> bool {
        slot.as_deref() == Some(version)
    }

    pub fn skipped(&self, version: &str) -> bool {
        Self::is(&self.skipped_version, version)
    }

    /// 设置里“关于”导航项的小绿点：有未跳过的新版本就一直显示，看过“关于”页也不消失。
    pub fn show_dot(&self, current: &str) -> bool {
        self.available(current)
            .is_some_and(|latest| !self.skipped(&latest.version))
    }

    /// 设置窗口顶部横幅：有未跳过的新版本，且这个版本的横幅没被关闭过。
    pub fn show_banner(&self, current: &str) -> bool {
        self.available(current).is_some_and(|latest| {
            !self.skipped(&latest.version)
                && !Self::is(&self.banner_dismissed_version, &latest.version)
        })
    }

    /// 托盘菜单“有新版本”：只要有可用新版本且没被跳过。
    pub fn show_tray_item(&self, current: &str) -> bool {
        self.available(current)
            .is_some_and(|latest| !self.skipped(&latest.version))
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn notify_decision(&self, current: &str, panel_visible: bool) -> Notify {
        let Some(latest) = self.available(current) else {
            return Notify::No;
        };
        if self.skipped(&latest.version) || Self::is(&self.notified_version, &latest.version) {
            return Notify::No;
        }
        if panel_visible {
            Notify::Wait
        } else {
            Notify::Now
        }
    }
}

/// 系统通知只在 Windows 上发（macOS 只用菜单栏提示）。
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notify {
    No,
    /// 面板正在显示：等它关闭后再发，避免打断粘贴。
    Wait,
    Now,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn latest(version: &str) -> Latest {
        Latest {
            version: version.into(),
            date: String::new(),
            url: format!("{}v{version}/a.zip", feed::DOWNLOAD_PREFIX),
            sha256: "0".repeat(64),
            notes: Vec::new(),
        }
    }
    const HOUR: u64 = 60 * 60 * 1000;

    #[test]
    fn first_check_waits_for_the_startup_delay() {
        let state = State::default();
        assert!(!check_due(true, 89_999, 0, FIRST_DELAY_MS, &state));
        assert!(check_due(true, 90_000, 0, FIRST_DELAY_MS, &state));
        assert!(check_due(true, 4_000, 0, PREVIEW_FIRST_DELAY_MS, &state));
    }

    #[test]
    fn auto_check_off_never_runs_in_background() {
        assert!(!check_due(
            false,
            u64::MAX / 2,
            0,
            FIRST_DELAY_MS,
            &State::default()
        ));
    }

    #[test]
    fn successful_checks_repeat_every_24_hours() {
        let mut state = State::default();
        state.record_success(1_000_000, "0.1.1", latest("0.1.1"));
        let start = 0;
        assert!(!check_due(
            true,
            1_000_000 + 24 * HOUR - 1,
            start,
            0,
            &state
        ));
        assert!(check_due(true, 1_000_000 + 24 * HOUR, start, 0, &state));
    }

    #[test]
    fn failed_checks_retry_after_an_hour() {
        let mut state = State::default();
        state.record_failure(5 * HOUR);
        assert!(!check_due(true, 6 * HOUR - 1, 0, 0, &state));
        assert!(check_due(true, 6 * HOUR, 0, 0, &state));
    }

    #[test]
    fn clock_moved_backwards_counts_as_due() {
        let mut state = State::default();
        state.record_success(10 * HOUR, "0.1.1", latest("0.1.1"));
        assert!(check_due(true, HOUR, 0, 0, &state));
    }

    #[test]
    fn success_only_keeps_versions_newer_than_current() {
        let mut state = State::default();
        state.record_success(1, "0.1.1", latest("0.1.2"));
        assert_eq!(state.last_result, CheckResult::New);
        assert!(state.available("0.1.1").is_some());
        // 用户手动升级后，缓存里的旧“新版本”不再算数。
        assert!(state.available("0.1.2").is_none());
        state.record_success(2, "0.1.1", latest("0.1.1"));
        assert_eq!(state.last_result, CheckResult::Latest);
        assert!(state.latest.is_none());
    }

    #[test]
    fn failure_keeps_the_known_latest_version() {
        let mut state = State::default();
        state.record_success(1, "0.1.1", latest("0.1.2"));
        state.record_failure(2);
        assert_eq!(state.last_result, CheckResult::Failed);
        assert!(state.available("0.1.1").is_some());
    }

    #[test]
    fn each_version_notifies_once() {
        let mut state = State::default();
        assert_eq!(state.notify_decision("0.1.1", false), Notify::No);
        state.record_success(1, "0.1.1", latest("0.1.2"));
        assert_eq!(state.notify_decision("0.1.1", true), Notify::Wait);
        assert_eq!(state.notify_decision("0.1.1", false), Notify::Now);
        state.notified_version = Some("0.1.2".into());
        assert_eq!(state.notify_decision("0.1.1", false), Notify::No);
        // 更新的版本出现后再通知一次。
        state.record_success(2, "0.1.1", latest("0.1.3"));
        assert_eq!(state.notify_decision("0.1.1", false), Notify::Now);
    }

    #[test]
    fn skipped_version_is_silent_but_a_newer_one_is_not() {
        let mut state = State::default();
        state.record_success(1, "0.1.1", latest("0.1.2"));
        state.skipped_version = Some("0.1.2".into());
        assert_eq!(state.notify_decision("0.1.1", false), Notify::No);
        assert!(!state.show_dot("0.1.1"));
        assert!(!state.show_banner("0.1.1"));
        assert!(!state.show_tray_item("0.1.1"));
        state.record_success(2, "0.1.1", latest("0.1.3"));
        assert!(state.show_dot("0.1.1"));
        assert!(state.show_banner("0.1.1"));
        assert!(state.show_tray_item("0.1.1"));
    }

    #[test]
    fn dot_banner_and_tray_item_are_independent() {
        let mut state = State::default();
        state.record_success(1, "0.1.1", latest("0.1.2"));
        assert!(
            state.show_dot("0.1.1") && state.show_banner("0.1.1") && state.show_tray_item("0.1.1")
        );
        // 关掉横幅：横幅不再出现，绿点和托盘项还在。
        state.banner_dismissed_version = Some("0.1.2".into());
        assert!(
            !state.show_banner("0.1.1") && state.show_dot("0.1.1") && state.show_tray_item("0.1.1")
        );
    }
}
