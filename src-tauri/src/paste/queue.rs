//! 逐张粘贴的确定性状态机与剪贴板写入租约；本模块不调用桌面 API。
use super::outcome::{Outcome, PasteResult, Reason};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

/// 空闲超时：最后一次 Ctrl+V / 放完一张之后这么久没有动静就停止，热键钩子不再接管 Ctrl+V。
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// 一张从开始到回执的上限；超过视为卡住，停止整个队列。
pub const ITEM_TIMEOUT: Duration = Duration::from_secs(10);
/// 一起放下时相邻两张之间的间隔。
pub const NEXT_DELAY: Duration = Duration::from_millis(80);
/// 一次最多放多少张。
pub const MAX_ITEMS: usize = 32;

/// 入队前的检查：给用户看的中文提示，命令和面板共用。
pub fn validate_ids(ids: &[usize]) -> Result<(), &'static str> {
    if ids.is_empty() {
        Err("没有要粘贴的条目，请先选择。")
    } else if ids.len() > MAX_ITEMS {
        Err("一次最多粘贴 32 条，请减少后再试。")
    } else if ids.iter().collect::<std::collections::HashSet<_>>().len() != ids.len() {
        Err("所选条目有重复，请重新选择。")
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Stop {
    Done = 1,
    Cancelled,
    InstallFailed,
    Timeout,
    ItemTimeout,
    ClipboardChanged,
    WriteFailed,
    SubmissionUncertain,
    Elevated,
    WorkerFailed,
    Escape = 11,
}

impl Stop {
    fn from_code(code: usize) -> Option<Self> {
        Some(match code {
            1 => Self::Done,
            2 => Self::Cancelled,
            3 => Self::InstallFailed,
            4 => Self::Timeout,
            5 => Self::ItemTimeout,
            6 => Self::ClipboardChanged,
            7 => Self::WriteFailed,
            8 => Self::SubmissionUncertain,
            9 => Self::Elevated,
            10 => Self::WorkerFailed,
            11 => Self::Escape,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Cancelled => "cancelled",
            Self::InstallFailed => "hook_install_failed",
            Self::Timeout => "idle_timeout",
            Self::ItemTimeout => "item_timeout",
            Self::ClipboardChanged => "clipboard_changed",
            Self::WriteFailed => "write_failed",
            Self::SubmissionUncertain => "submission_uncertain",
            Self::Elevated => "target_may_be_elevated",
            Self::WorkerFailed => "worker_failed",
            Self::Escape => "escape",
        }
    }

    pub fn message(self) -> Option<&'static str> {
        match self {
            Self::Done | Self::Cancelled | Self::Escape => None,
            Self::InstallFailed => Some("逐张粘贴键盘监听安装失败，请重新呼出后重试。"),
            Self::Timeout => Some("两分钟没有粘贴，逐张粘贴已停止；请重新选择剩余条目。"),
            Self::ItemTimeout => Some("这条等待超过十秒，已停止；请先检查目标再重试。"),
            Self::ClipboardChanged => Some("检测到新的剪贴板内容，逐张粘贴已停止，新内容已保留。"),
            Self::WriteFailed => Some("剪贴板写入未完成，逐张粘贴已停止；请检查后重试。"),
            Self::SubmissionUncertain => {
                Some("这条的粘贴结果未确认，逐张粘贴已停止；请先检查目标。")
            }
            Self::Elevated => {
                Some("目标可能以管理员运行，已复制当前一条，逐张粘贴已停止；请手动 Ctrl+V。")
            }
            Self::WorkerFailed => Some("逐张粘贴工作线程启动失败，已停止；请重新呼出后重试。"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Installing,
    Ready,
    Busy(Duration),
    Stopped(Stop),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Demand {
    Start(usize),
    Buffered,
    Full,
    Inactive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finished {
    Ready,
    Next,
    Stopped(Stop),
    Stale,
}

/// 面板唯一的一份队列进度。时间由调用方从同一个 Guard 时钟传入，测试不需要 sleep。
pub struct State {
    pub ids: VecDeque<usize>,
    pub total: usize,
    pub automatic: bool,
    pub error: Option<&'static str>,
    /// 记录已不在、原文件都不在而跳过的张数（不算放下，也不卡住后面的）。
    pub skipped: usize,
    phase: Phase,
    pending: usize,
    next_at: Duration,
    last_activity: Duration,
}

impl State {
    pub fn new(ids: Vec<usize>, automatic: bool) -> Self {
        Self {
            total: ids.len(),
            ids: ids.into(),
            automatic,
            error: None,
            skipped: 0,
            phase: Phase::Installing,
            pending: 0,
            next_at: Duration::ZERO,
            last_activity: Duration::ZERO,
        }
    }

    pub fn installed(&mut self, success: bool, now: Duration) {
        if self.phase == Phase::Installing {
            self.last_activity = now;
            if success {
                self.phase = Phase::Ready;
            } else {
                self.stop(Stop::InstallFailed);
            }
        }
    }

    pub fn busy(&self) -> bool {
        matches!(self.phase, Phase::Busy(_))
    }

    pub fn stop(&mut self, reason: Stop) {
        if matches!(self.phase, Phase::Stopped(_)) {
            return;
        }
        self.phase = Phase::Stopped(reason);
        self.pending = 0;
        self.error = reason.message();
    }

    /// 空闲只在没有一张正在放时计时；正在放的一张由 ITEM_TIMEOUT 兜底。
    pub fn expired(&self, now: Duration) -> Option<Stop> {
        match self.phase {
            Phase::Stopped(reason) => Some(reason),
            Phase::Busy(since) if now.saturating_sub(since) >= ITEM_TIMEOUT => {
                Some(Stop::ItemTimeout)
            }
            Phase::Busy(_) => None,
            _ if now.saturating_sub(self.last_activity) >= IDLE_TIMEOUT => Some(Stop::Timeout),
            _ => None,
        }
    }

    pub fn due(&self, now: Duration) -> bool {
        self.automatic && self.phase == Phase::Ready && self.error.is_none() && now >= self.next_at
    }

    /// 提示出错后用户还没动作时，允许界面先显示一条说明（例如目标窗口没还回来）。
    pub fn notice(&mut self, message: &'static str) {
        if !matches!(self.phase, Phase::Stopped(_)) {
            self.error = Some(message);
        }
    }

    pub fn request(&mut self, now: Duration) -> Demand {
        if let Some(reason) = self.expired(now) {
            self.stop(reason);
            return Demand::Inactive;
        }
        match self.phase {
            Phase::Ready if !self.ids.is_empty() => {
                self.last_activity = now;
                self.phase = Phase::Busy(now);
                self.error = None;
                Demand::Start(self.ids[0])
            }
            Phase::Busy(_) if self.pending + 1 < self.ids.len() => {
                self.last_activity = now;
                self.pending += 1;
                self.error = Some("正在粘贴，已记下这次 Ctrl+V");
                Demand::Buffered
            }
            Phase::Busy(_) => {
                self.last_activity = now;
                self.error = Some("剩余条目都已记下，请等粘贴完");
                Demand::Full
            }
            _ => Demand::Inactive,
        }
    }

    pub fn finish(&mut self, id: usize, result: &Outcome, now: Duration) -> Finished {
        if !self.busy() || self.ids.front() != Some(&id) {
            return Finished::Stale;
        }
        if let Some(reason) = self.expired(now) {
            self.stop(reason);
            return Finished::Stopped(reason);
        }
        self.last_activity = now;
        if result.request_cancelled || result.reason == Some(Reason::RequestCancelled) {
            self.stop(Stop::Cancelled);
            return Finished::Stopped(Stop::Cancelled);
        }
        if result.clipboard_written
            && matches!(
                result.result,
                Some(PasteResult::Sent | PasteResult::Confirmed)
            )
        {
            self.ids.pop_front();
            self.error = None;
            if self.ids.is_empty() {
                self.stop(Stop::Done);
                return Finished::Stopped(Stop::Done);
            }
            self.phase = Phase::Ready;
            self.next_at = now + NEXT_DELAY;
            if self.pending > 0 {
                self.pending -= 1;
                Finished::Next
            } else {
                Finished::Ready
            }
        } else if result.clipboard_written || result.result == Some(PasteResult::CopiedOnly) {
            let reason = if result.reason == Some(Reason::TargetMayBeElevated) {
                Stop::Elevated
            } else {
                Stop::SubmissionUncertain
            };
            self.stop(reason);
            Finished::Stopped(reason)
        } else if matches!(
            result.reason,
            Some(Reason::RecordMissing | Reason::FilesMissing)
        ) {
            // 这张已经不能放了：跳过并上报，同一次 Ctrl+V 接着放下一张，不卡住整个队列。
            self.ids.pop_front();
            self.skipped += 1;
            self.error = Some(if result.reason == Some(Reason::FilesMissing) {
                "原文件不在了，已跳过这条"
            } else {
                "这条记录已不在历史里，已跳过这条"
            });
            if self.ids.is_empty() {
                let message = self.error;
                self.stop(Stop::Done);
                // 跳过后已无可粘贴：仍是正常结束，但保留说明，界面据此停留提示。
                self.error = message;
                return Finished::Stopped(Stop::Done);
            }
            self.phase = Phase::Ready;
            self.next_at = now;
            Finished::Next
        } else {
            // 没有写入、没有发送，可以明确重试同一张；被吞的后续 chord 全部作废。
            self.phase = Phase::Ready;
            self.pending = 0;
            self.error = Some(match result.reason {
                Some(Reason::InvalidTarget) => "请先点回要粘贴的位置，再按 Ctrl+V；Esc 取消",
                _ => "这条没有粘贴，按 Ctrl+V 重试；Esc 取消",
            });
            Finished::Ready
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub owner: usize,
    pub sequence: u32,
}

/// 剪贴板是否还是期望的那一份。序号 0 一律按外部变化处理。
/// - 起始基线可以没有持有者（空剪贴板、源程序已退出、OpenClipboard(NULL)）：只比对序号；
/// - 第一次自写之后期望值换成自己的发布（持有者非 0）：持有者和序号都要一致。
///
/// 超时由 State 的空闲计时负责（停止时经 Guard::stop 撤销），这里只比对剪贴板。
fn check_stamp(expected: Stamp, stamp: Stamp) -> Result<(), Stop> {
    let same = if expected.owner == 0 {
        stamp.sequence == expected.sequence
    } else {
        stamp == expected
    };
    if expected.sequence == 0 || stamp.sequence == 0 || !same {
        Err(Stop::ClipboardChanged)
    } else {
        Ok(())
    }
}

/// 每次队列写入都在剪贴板线程持有一把 Lease。OpenClipboard 成功后、EmptyClipboard 前
/// 必须 begin；发布快照成功后必须 published。其它线程可以用 stop 立即撤销取消标记，
/// 不需要等待 UI 通知，也不会在 watcher 回调里反向等待租约锁。
pub struct Guard {
    pub cancelled: Arc<AtomicBool>,
    expected: Mutex<Stamp>,
    stopped: AtomicUsize,
    writer: usize,
    started: Instant,
    // 最近一次活动（Guard 时钟的毫秒数）：给不经过主线程的键盘钩子做空闲兜底。
    activity: AtomicU64,
}

impl Guard {
    pub fn new(writer: usize, expected: Stamp) -> Result<Self, Stop> {
        if writer == 0 {
            return Err(Stop::ClipboardChanged);
        }
        check_stamp(expected, expected)?;
        Ok(Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            expected: Mutex::new(expected),
            stopped: AtomicUsize::new(0),
            writer,
            started: Instant::now(),
            activity: AtomicU64::new(0),
        })
    }

    pub fn writer(&self) -> usize {
        self.writer
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn touch(&self) {
        self.activity
            .store(self.elapsed().as_millis() as u64, Ordering::Release);
    }

    /// 距最近一次活动已过多久（钩子线程每 50ms 查一次，无锁）。
    pub fn idle(&self) -> Duration {
        self.elapsed()
            .saturating_sub(Duration::from_millis(self.activity.load(Ordering::Acquire)))
    }

    pub fn stop(&self, reason: Stop) {
        let _ =
            self.stopped
                .compare_exchange(0, reason as usize, Ordering::AcqRel, Ordering::Acquire);
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn stop_reason(&self) -> Option<Stop> {
        Stop::from_code(self.stopped.load(Ordering::Acquire))
    }

    pub fn inspect(&self, sample: impl FnOnce() -> Stamp) -> Option<Stop> {
        self.lease().check(sample()).err()
    }

    pub fn lease(&self) -> Lease<'_> {
        Lease {
            guard: self,
            expected: self
                .expected
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
            began: false,
            committed: false,
        }
    }
}

pub struct Lease<'a> {
    guard: &'a Guard,
    expected: MutexGuard<'a, Stamp>,
    began: bool,
    committed: bool,
}

impl Lease<'_> {
    pub fn check(&mut self, stamp: Stamp) -> Result<(), Stop> {
        if self.guard.cancelled.load(Ordering::Acquire) && self.guard.stop_reason().is_none() {
            self.guard.stop(Stop::Cancelled);
        }
        let result = if let Some(reason) = self.guard.stop_reason() {
            Err(reason)
        } else {
            check_stamp(*self.expected, stamp)
        };
        if let Err(reason) = result {
            self.guard.stop(reason);
        }
        result
    }

    /// 仅在成功 OpenClipboard 后、EmptyClipboard 紧前调用。租约锁持续到发布快照更新。
    pub fn begin(&mut self, stamp: Stamp) -> Result<(), Stop> {
        self.check(stamp)?;
        self.began = true;
        Ok(())
    }

    pub fn published(&mut self, stamp: Stamp) {
        if stamp.owner == self.guard.writer && stamp.sequence != 0 {
            *self.expected = stamp;
            self.committed = true;
        } else {
            self.guard.stop(Stop::ClipboardChanged);
        }
    }
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if self.began && !self.committed {
            self.guard.stop(Stop::WriteFailed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sent() -> Outcome {
        Outcome::from_report(Some(PasteResult::Sent), json!({"clipboard_written":true}))
    }

    #[test]
    fn install_failure_and_stop_release_busy_without_losing_items() {
        let mut state = State::new(vec![3, 2, 1], false);
        assert_eq!(state.request(Duration::ZERO), Demand::Inactive);
        state.installed(false, Duration::ZERO);
        assert!(!state.busy());
        assert!(state.error.is_some());
        assert_eq!(state.request(Duration::ZERO), Demand::Inactive);
        assert_eq!(state.ids, [3, 2, 1]);

        let mut state = State::new(vec![1], false);
        state.installed(true, Duration::ZERO);
        assert_eq!(state.request(Duration::ZERO), Demand::Start(1));
        state.stop(Stop::Cancelled);
        assert_eq!(state.finish(1, &sent(), Duration::ZERO), Finished::Stale);
        assert_eq!(state.ids, [1]);
    }

    #[test]
    fn busy_ctrl_v_is_buffered_once_per_chord_up_to_remaining_items() {
        let mut state = State::new(vec![1, 2, 3], false);
        state.installed(true, Duration::ZERO);
        assert_eq!(state.request(Duration::ZERO), Demand::Start(1));
        assert_eq!(state.request(Duration::ZERO), Demand::Buffered);
        assert_eq!(state.request(Duration::ZERO), Demand::Buffered);
        assert_eq!(state.request(Duration::ZERO), Demand::Full);
        for id in [1, 2] {
            assert_eq!(state.finish(id, &sent(), Duration::ZERO), Finished::Next);
            assert_eq!(state.request(Duration::ZERO), Demand::Start(id + 1));
        }
        assert_eq!(
            state.finish(3, &sent(), Duration::ZERO),
            Finished::Stopped(Stop::Done)
        );
        assert_eq!(state.request(Duration::ZERO), Demand::Inactive);
    }

    #[test]
    fn failure_retains_front_and_discards_pending_intents() {
        let mut state = State::new(vec![1, 2], true);
        state.installed(true, Duration::ZERO);
        state.request(Duration::ZERO);
        state.request(Duration::ZERO);
        assert_eq!(
            state.finish(
                1,
                &Outcome::not_written(Reason::ClipboardBusy),
                Duration::ZERO
            ),
            Finished::Ready
        );
        assert!(!state.busy());
        assert!(!state.due(Duration::from_secs(1)));
        assert_eq!(state.ids, [1, 2]);
        state.request(Duration::ZERO);
        assert_eq!(state.finish(1, &sent(), Duration::ZERO), Finished::Ready);
        assert!(!state.due(Duration::from_millis(79)));
        assert!(state.due(Duration::from_millis(80)));
    }

    #[test]
    fn elevated_copied_only_and_uncertain_submission_never_remove_the_front() {
        for (paste_result, reason, stop) in [
            (
                PasteResult::CopiedOnly,
                "target_may_be_elevated",
                Stop::Elevated,
            ),
            (
                PasteResult::CopiedOnly,
                "invalid_target",
                Stop::SubmissionUncertain,
            ),
            (
                PasteResult::GenericSubmissionUnknown,
                "send_failed",
                Stop::SubmissionUncertain,
            ),
        ] {
            let mut state = State::new(vec![1, 2], false);
            state.installed(true, Duration::ZERO);
            state.request(Duration::ZERO);
            let result = Outcome::from_report(
                Some(paste_result),
                json!({"clipboard_written":true,"reason":reason}),
            );
            assert_eq!(
                state.finish(1, &result, Duration::ZERO),
                Finished::Stopped(stop)
            );
            assert_eq!(state.ids, [1, 2]);
            assert!(!state.busy());
        }
    }

    #[test]
    fn idle_and_busy_deadlines_are_finite_and_late_results_cannot_advance() {
        let mut state = State::new(vec![1, 2], false);
        state.installed(true, Duration::ZERO);
        assert_eq!(state.expired(IDLE_TIMEOUT - Duration::from_millis(1)), None);
        assert_eq!(state.expired(IDLE_TIMEOUT), Some(Stop::Timeout));
        // 有动作就重新计时：放了一张之后再等两分钟才算空闲。
        let later = Duration::from_secs(100);
        assert_eq!(state.request(later), Demand::Start(1));
        assert_eq!(state.finish(1, &sent(), later), Finished::Ready);
        assert_eq!(
            state.expired(later + IDLE_TIMEOUT - Duration::from_millis(1)),
            None
        );
        assert_eq!(state.expired(later + IDLE_TIMEOUT), Some(Stop::Timeout));
        // 正在放的一张不按空闲算，按单张上限算。
        assert_eq!(state.request(later), Demand::Start(2));
        assert_eq!(
            state.expired(later + ITEM_TIMEOUT - Duration::from_millis(1)),
            None
        );
        assert_eq!(state.expired(later + ITEM_TIMEOUT), Some(Stop::ItemTimeout));
        assert_eq!(
            state.finish(2, &sent(), later + ITEM_TIMEOUT),
            Finished::Stopped(Stop::ItemTimeout)
        );
        assert_eq!(state.ids, [2]);
        assert_eq!(
            state.finish(2, &sent(), later + ITEM_TIMEOUT + Duration::from_secs(1)),
            Finished::Stale
        );
    }

    #[test]
    fn idle_timeout_stops_further_requests() {
        let mut state = State::new(vec![1], false);
        state.installed(true, Duration::from_secs(5));
        assert_eq!(
            state.request(Duration::from_secs(5) + IDLE_TIMEOUT),
            Demand::Inactive
        );
        assert_eq!(state.error, Stop::Timeout.message());
        assert_eq!(state.ids, [1], "停止不丢剩余条目");
    }

    // 记录已不在 / 原文件都不在：跳过这张并上报，同一次 Ctrl+V 接着放下一张。
    #[test]
    fn missing_records_are_skipped_and_the_queue_continues() {
        let mut state = State::new(vec![1, 2, 3], false);
        state.installed(true, Duration::ZERO);
        assert_eq!(state.request(Duration::ZERO), Demand::Start(1));
        assert_eq!(
            state.finish(
                1,
                &Outcome::not_written(Reason::RecordMissing),
                Duration::ZERO
            ),
            Finished::Next
        );
        assert_eq!(state.ids, [2, 3]);
        assert_eq!(state.skipped, 1);
        assert_eq!(state.error, Some("这条记录已不在历史里，已跳过这条"));
        assert_eq!(state.request(Duration::ZERO), Demand::Start(2));
        assert_eq!(state.finish(2, &sent(), Duration::ZERO), Finished::Ready);
        assert_eq!(state.request(Duration::ZERO), Demand::Start(3));
        assert_eq!(
            state.finish(
                3,
                &Outcome::not_written(Reason::FilesMissing),
                Duration::ZERO
            ),
            Finished::Stopped(Stop::Done),
            "最后一张也跳过时队列正常结束"
        );
        assert_eq!(state.skipped, 2);
    }

    #[test]
    fn own_window_target_is_retryable_with_a_hint() {
        let mut state = State::new(vec![1, 2], false);
        state.installed(true, Duration::ZERO);
        state.request(Duration::ZERO);
        assert_eq!(
            state.finish(
                1,
                &Outcome::not_written(Reason::InvalidTarget),
                Duration::ZERO
            ),
            Finished::Ready
        );
        assert_eq!(state.ids, [1, 2]);
        assert!(state.error.unwrap().contains("点回要粘贴的位置"));
        assert_eq!(state.request(Duration::ZERO), Demand::Start(1));
    }

    #[test]
    fn ids_are_validated_with_specific_messages() {
        assert_eq!(validate_ids(&[]), Err("没有要粘贴的条目，请先选择。"));
        let full: Vec<_> = (0..MAX_ITEMS).collect();
        assert_eq!(validate_ids(&full), Ok(()));
        let over: Vec<_> = (0..=MAX_ITEMS).collect();
        assert_eq!(
            validate_ids(&over),
            Err("一次最多粘贴 32 条，请减少后再试。")
        );
        assert_eq!(
            validate_ids(&[3, 4, 3]),
            Err("所选条目有重复，请重新选择。")
        );
    }

    #[test]
    fn guard_idle_follows_touch() {
        let guard = Guard::new(
            5,
            Stamp {
                owner: 7,
                sequence: 1,
            },
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert!(guard.idle() >= Duration::from_millis(20));
        guard.touch();
        assert!(guard.idle() < Duration::from_millis(20));
    }

    #[test]
    fn clipboard_owner_and_sequence_are_both_required_including_null_owner() {
        let stamp = Stamp {
            owner: 7,
            sequence: 10,
        };
        assert_eq!(check_stamp(stamp, stamp), Ok(()));
        for next in [
            Stamp {
                owner: 0,
                sequence: 10,
            },
            Stamp {
                owner: 0,
                sequence: 11,
            },
            Stamp {
                owner: 8,
                sequence: 10,
            },
            Stamp {
                owner: 7,
                sequence: 11,
            },
            Stamp {
                owner: 7,
                sequence: 0,
            },
        ] {
            assert_eq!(check_stamp(stamp, next), Err(Stop::ClipboardChanged));
        }
        assert!(Guard::new(0, stamp).is_err());
        assert!(
            Guard::new(
                5,
                Stamp {
                    sequence: 0,
                    ..stamp
                }
            )
            .is_err()
        );
    }

    #[test]
    fn own_publication_renews_stamp_but_user_copy_before_locked_write_cancels() {
        let original = Stamp {
            owner: 7,
            sequence: 10,
        };
        let own = Stamp {
            owner: 5,
            sequence: 11,
        };
        let guard = Guard::new(5, original).unwrap();
        {
            let mut lease = guard.lease();
            lease.begin(original).unwrap();
            lease.published(own);
        }
        assert_eq!(guard.inspect(|| own), None);
        let user = Stamp {
            owner: 0,
            sequence: 12,
        };
        assert_eq!(guard.lease().begin(user), Err(Stop::ClipboardChanged));
        assert!(guard.cancelled.load(Ordering::Acquire));
        assert_eq!(guard.stop_reason(), Some(Stop::ClipboardChanged));
    }

    #[test]
    fn failed_or_raced_publication_cancels_and_stopped_guard_cannot_write() {
        let original = Stamp {
            owner: 7,
            sequence: 10,
        };
        let guard = Guard::new(5, original).unwrap();
        {
            let mut lease = guard.lease();
            lease.begin(original).unwrap();
        }
        assert_eq!(guard.inspect(|| original), Some(Stop::WriteFailed));

        let guard = Guard::new(5, original).unwrap();
        {
            let mut lease = guard.lease();
            lease.begin(original).unwrap();
            lease.published(Stamp {
                owner: 8,
                sequence: 12,
            });
        }
        assert_eq!(guard.inspect(|| original), Some(Stop::ClipboardChanged));

        let guard = Guard::new(5, original).unwrap();
        guard.stop(Stop::Timeout);
        assert_eq!(guard.lease().begin(original), Err(Stop::Timeout));
    }

    // 问题 3：起始剪贴板没有持有者时队列照样能起，只比对序号；自写之后按持有者+序号比对。
    #[test]
    fn unowned_clipboard_baseline_compares_sequence_only_until_first_own_write() {
        let baseline = Stamp {
            owner: 0,
            sequence: 40,
        };
        let guard = Guard::new(5, baseline).expect("持有者为 0 也能起");
        assert_eq!(guard.inspect(|| baseline), None);
        assert_eq!(
            guard.inspect(|| Stamp {
                owner: 9,
                sequence: 41
            }),
            Some(Stop::ClipboardChanged),
            "序号变了就是外部变化"
        );
        let guard = Guard::new(5, baseline).unwrap();
        let own = Stamp {
            owner: 5,
            sequence: 41,
        };
        {
            let mut lease = guard.lease();
            lease.begin(baseline).unwrap();
            lease.published(own);
        }
        assert_eq!(guard.inspect(|| own), None);
        assert_eq!(
            guard.inspect(|| Stamp {
                owner: 0,
                sequence: 41
            }),
            Some(Stop::ClipboardChanged),
            "自写之后持有者也要一致"
        );
    }

    #[test]
    fn skipping_the_last_item_ends_normally_but_keeps_the_explanation() {
        let mut state = State::new(vec![1], false);
        state.installed(true, Duration::ZERO);
        state.request(Duration::ZERO);
        assert_eq!(
            state.finish(
                1,
                &Outcome::not_written(Reason::RecordMissing),
                Duration::ZERO
            ),
            Finished::Stopped(Stop::Done)
        );
        assert_eq!(state.error, Some("这条记录已不在历史里，已跳过这条"));
    }
}
