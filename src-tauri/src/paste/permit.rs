use super::request::Mode;
use crate::platform::Target;
use serde::Deserialize;
use std::sync::{Arc, Mutex};

pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

// revision 必须由前端在 DOM 选择事件中同步固定，后端从不替它刷新起点。
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct Selection {
    pub revision: u64,
    pub nonce: u64,
}

#[derive(Clone, PartialEq, Eq)]
struct Identity {
    window: usize,
    focus: usize,
    thread: u32,
    process: u32,
    focus_class: String,
}
impl From<&Target> for Identity {
    fn from(target: &Target) -> Self {
        Self {
            window: target.window,
            focus: target.focus,
            thread: target.thread,
            process: target.process,
            focus_class: target.focus_class.clone(),
        }
    }
}

// 只保证独立原生编辑 HWND 的身份；共享浏览器宿主不能证明标签或输入对象仍相同。
pub fn reliable_control(target: &Target) -> bool {
    let class = target.focus_class.to_ascii_lowercase();
    target.window != 0
        && target.focus != 0
        && target.thread != 0
        && target.process != 0
        && (matches!(
            class.as_str(),
            "edit" | "richedit" | "richedit20a" | "richedit20w" | "richedit50w" | "richedit50a"
        ) || class.starts_with("windowsforms10.edit."))
}

struct Return {
    armed_at: u32,
    observed: bool,
}
#[derive(Default)]
struct State {
    alive: bool,
    revision: u64,
    first_external: usize,
    last_event: Option<u32>,
    returning: Option<Return>,
    quick_uncertain: bool,
    target_lost: bool,
}

pub struct Monitor {
    panel: usize,
    target: usize,
    state: Mutex<State>,
}
impl Monitor {
    pub fn new(panel: usize, target: usize) -> Self {
        Self {
            panel,
            target,
            state: Mutex::new(State::default()),
        }
    }
    pub fn started(&self) {
        self.state.lock().unwrap().alive = true;
    }
    pub fn stopped(&self) {
        let mut state = self.state.lock().unwrap();
        state.alive = false;
        state.revision = state.revision.saturating_add(1);
    }
    pub fn revision(&self) -> u64 {
        self.state.lock().unwrap().revision
    }
    #[cfg(test)]
    pub fn first_external(&self) -> usize {
        self.state.lock().unwrap().first_external
    }
    pub fn target_destroyed(&self) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.target_lost = true;
        state.revision = state.revision.saturating_add(1);
        state.revision
    }
    // 非预期回调无法证明前台归还，撤销现有许可；不与前台事件共用时间高水位。
    pub fn uncertain(&self) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.revision = state.revision.saturating_add(1);
        state.quick_uncertain = true;
        state.revision
    }

    // 既保留首次外部锁存，又对每次后续事件计数；目标窗口也只有武装归还的一次转换能豁免。
    pub fn observe(&self, window: usize, time: u32, current: usize) -> Option<u64> {
        let mut state = self.state.lock().unwrap();
        let before = state.revision;
        let out_of_order = state.last_event.is_some_and(|last| !ordered(time, last));
        if !out_of_order {
            state.last_event = Some(time);
        }
        let stale_return = state
            .returning
            .as_ref()
            .is_some_and(|ret| time == ret.armed_at || !ordered(time, ret.armed_at));
        if window == 0 || current != window || out_of_order || stale_return {
            // 迟到事件撤销已存在许可，不冒充监听线程死亡；下一次主动选择可使用已通知的新 revision。
            state.revision = state.revision.saturating_add(1);
            state.quick_uncertain = true;
        } else {
            let expected =
                window == self.target && state.returning.as_ref().is_some_and(|ret| !ret.observed);
            if expected {
                state.returning.as_mut().unwrap().observed = true;
            } else if window != self.panel
                || state.returning.as_ref().is_some_and(|ret| ret.observed)
            {
                state.revision = state.revision.saturating_add(1);
            }
        }
        if window != 0 && window != self.panel && window != self.target && state.first_external == 0
        {
            state.first_external = window;
        }
        (before != state.revision).then_some(state.revision)
    }
}
fn ordered(time: u32, previous: u32) -> bool {
    time.wrapping_sub(previous) <= i32::MAX as u32
}

pub struct Session {
    session: u64,
    target: Target,
    monitor: Arc<Monitor>,
    last_nonce: u64,
}
impl Session {
    pub fn new(session: u64, target: Target, monitor: Arc<Monitor>) -> Self {
        Self {
            session,
            target,
            monitor,
            last_nonce: 0,
        }
    }
    pub fn select(
        &mut self,
        session: u64,
        record: usize,
        mode: Mode,
        selection: Selection,
        foreground: usize,
    ) -> Result<Permit, &'static str> {
        if session != self.session {
            return Err("stale_session");
        }
        if selection.nonce == 0
            || selection.nonce > MAX_SAFE_INTEGER
            || selection.nonce <= self.last_nonce
        {
            return Err("selection_nonce_invalid");
        }
        self.last_nonce = selection.nonce;
        let state = self.monitor.state.lock().unwrap();
        if !state.alive
            || state.target_lost
            || selection.revision != state.revision
            || foreground != self.monitor.panel
        {
            return Err("selection_boundary_changed");
        }
        Ok(Permit {
            session,
            record,
            mode,
            target: Identity::from(&self.target),
            source: Source::Selection(selection.revision),
            monitor: self.monitor.clone(),
        })
    }
    pub fn quick(&self, record: usize, mode: Mode) -> Permit {
        Permit {
            session: self.session,
            record,
            mode,
            target: Identity::from(&self.target),
            source: Source::Quick,
            monitor: self.monitor.clone(),
        }
    }
}

#[derive(Clone, Copy)]
enum Source {
    Quick,
    Selection(u64),
}
#[derive(Clone)]
pub struct Permit {
    session: u64,
    record: usize,
    mode: Mode,
    target: Identity,
    source: Source,
    monitor: Arc<Monitor>,
}
impl Permit {
    pub fn current_revision(&self) -> u64 {
        self.monitor.revision()
    }
    pub fn revoke(&self) {
        self.monitor.uncertain();
    }
    fn valid_state(&self, state: &State) -> bool {
        state.alive
            && !state.target_lost
            && match self.source {
                Source::Quick => state.first_external == 0 && !state.quick_uncertain,
                Source::Selection(revision) => revision == state.revision,
            }
    }
    pub fn check(
        &self,
        session: u64,
        record: usize,
        mode: Mode,
        target: &Target,
        foreground: usize,
    ) -> bool {
        if session != self.session
            || record != self.record
            || mode != self.mode
            || Identity::from(target) != self.target
        {
            return false;
        }
        let state = self.monitor.state.lock().unwrap();
        self.valid_state(&state)
            && if state.returning.is_some() {
                foreground == self.monitor.target
            } else {
                foreground == self.monitor.panel
                    || (matches!(self.source, Source::Quick) && foreground == self.monitor.target)
            }
    }
    pub fn arm_return(&self, foreground: usize, now: u32) -> bool {
        let mut state = self.monitor.state.lock().unwrap();
        if !self.valid_state(&state) || state.returning.is_some() {
            return false;
        }
        if foreground == self.monitor.target && matches!(self.source, Source::Quick) {
            return true;
        }
        if foreground != self.monitor.panel {
            return false;
        }
        state.returning = Some(Return {
            armed_at: now,
            observed: false,
        });
        true
    }
    pub fn can_send(
        &self,
        session: u64,
        record: usize,
        mode: Mode,
        target: &Target,
        foreground: usize,
    ) -> bool {
        if session != self.session
            || record != self.record
            || mode != self.mode
            || Identity::from(target) != self.target
            || foreground != self.monitor.target
        {
            return false;
        }
        if self.search() && !reliable_control(target) {
            return false;
        }
        let state = self.monitor.state.lock().unwrap();
        // 搜索发键前必须收到已武装的归还事件；还在等待异步 WinEvent 时不能抢先发送。
        self.valid_state(&state)
            && (matches!(self.source, Source::Quick)
                || state.returning.as_ref().is_some_and(|ret| ret.observed))
    }
    pub fn search(&self) -> bool {
        matches!(self.source, Source::Selection(_))
    }
    pub fn current(&self, target: &Target, foreground: usize) -> bool {
        self.check(self.session, self.record, self.mode, target, foreground)
    }
    pub fn return_pending(&self, target: &Target, foreground: usize) -> bool {
        if Identity::from(target) != self.target || foreground != self.monitor.panel {
            return false;
        }
        let state = self.monitor.state.lock().unwrap();
        self.valid_state(&state) && state.returning.as_ref().is_some_and(|ret| !ret.observed)
    }
}

// 搜索许可的反例测试先于实现落地；测试只操作状态，不访问窗口或发送按键。
#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> Target {
        Target {
            window: 20,
            focus: 21,
            thread: 2,
            process: 3,
            focus_class: "Edit".into(),
            ..Target::default()
        }
    }
    fn state() -> (Arc<Monitor>, Session) {
        let monitor = Arc::new(Monitor::new(10, 20));
        monitor.started();
        let session = Session::new(7, target(), monitor.clone());
        (monitor, session)
    }
    fn select(session: &mut Session, revision: u64, nonce: u64) -> Result<Permit, &'static str> {
        session.select(7, 9, Mode::Paste, Selection { revision, nonce }, 10)
    }

    #[test]
    fn a_new_selection_after_historical_external_events_keeps_the_old_latch() {
        let (monitor, mut session) = state();
        monitor.observe(30, 100, 30);
        monitor.observe(10, 101, 10);
        assert_eq!(monitor.first_external(), 30);
        let permit = select(&mut session, 1, 1).unwrap();
        assert!(permit.check(7, 9, Mode::Paste, &target(), 10));
        assert_eq!(monitor.first_external(), 30);
    }

    #[test]
    fn every_later_external_event_invalidates_even_after_an_ime_latch() {
        let (monitor, mut session) = state();
        monitor.observe(30, 100, 30);
        monitor.observe(10, 101, 10);
        let permit = select(&mut session, 1, 1).unwrap();
        monitor.observe(40, 102, 40);
        monitor.observe(10, 103, 10);
        assert_eq!(monitor.revision(), 2);
        assert_eq!(monitor.first_external(), 30);
        assert!(!permit.check(7, 9, Mode::Paste, &target(), 10));
        assert!(
            select(&mut session, 1, 2).is_err(),
            "迟到 IPC 不能重取新起点"
        );
    }

    #[test]
    fn selection_to_original_target_and_back_before_restore_is_cancelled() {
        let (monitor, mut session) = state();
        let permit = select(&mut session, 0, 1).unwrap();
        monitor.observe(20, 100, 20);
        monitor.observe(10, 101, 10);
        assert!(!permit.check(7, 9, Mode::Paste, &target(), 10));
    }

    #[test]
    fn nonce_session_record_mode_and_original_target_are_bound_once() {
        let (_, mut session) = state();
        let permit = select(&mut session, 0, 1).unwrap();
        assert!(select(&mut session, 0, 1).is_err());
        assert!(select(&mut session, 0, 0).is_err());
        assert!(select(&mut session, 0, MAX_SAFE_INTEGER + 1).is_err());
        assert!(!permit.check(8, 9, Mode::Paste, &target(), 10));
        assert!(!permit.check(7, 8, Mode::Paste, &target(), 10));
        assert!(!permit.check(7, 9, Mode::Text, &target(), 10));
        let mut changed = target();
        changed.focus = 22;
        assert!(!permit.check(7, 9, Mode::Paste, &changed, 10));
        assert!(
            session
                .select(
                    8,
                    9,
                    Mode::Paste,
                    Selection {
                        revision: 0,
                        nonce: 2
                    },
                    10
                )
                .is_err()
        );
        assert!(
            session
                .select(
                    7,
                    9,
                    Mode::Paste,
                    Selection {
                        revision: 0,
                        nonce: 3
                    },
                    20
                )
                .is_err()
        );
    }

    #[test]
    fn only_one_armed_panel_to_target_transition_is_exempt() {
        let (monitor, mut session) = state();
        let permit = select(&mut session, 0, 1).unwrap();
        assert!(permit.arm_return(10, 100));
        assert!(
            !permit.can_send(7, 9, Mode::Paste, &target(), 20),
            "未收到归还事件不能抢先发键"
        );
        monitor.observe(20, 101, 20);
        assert!(permit.check(7, 9, Mode::Paste, &target(), 20));
        monitor.observe(10, 102, 10);
        monitor.observe(20, 103, 20);
        assert!(!permit.check(7, 9, Mode::Paste, &target(), 20));
    }

    #[test]
    fn a_noncurrent_candidate_after_return_still_revokes_the_new_selection() {
        let (monitor, mut session) = state();
        // 历史候选事件由新主动选择覆盖，归还后的未知事件不能借此豁免。
        monitor.observe(30, 100, 10);
        let permit = select(&mut session, monitor.revision(), 1).unwrap();
        assert!(permit.arm_return(10, 200));
        monitor.observe(20, 201, 20);
        assert!(permit.can_send(7, 9, Mode::Paste, &target(), 20));
        monitor.observe(30, 202, 20);
        assert!(!permit.can_send(7, 9, Mode::Paste, &target(), 20));
    }

    #[test]
    fn a_real_switch_away_and_back_after_return_cannot_regain_permission() {
        let (monitor, mut session) = state();
        let permit = select(&mut session, 0, 1).unwrap();
        assert!(permit.arm_return(10, 100));
        monitor.observe(20, 101, 20);
        monitor.observe(40, 102, 40);
        monitor.observe(20, 103, 20);
        assert!(!permit.can_send(7, 9, Mode::Paste, &target(), 20));
    }

    #[test]
    fn an_unrelated_callback_cannot_confirm_an_armed_return_or_poison_event_time() {
        let (monitor, mut session) = state();
        let permit = select(&mut session, 0, 1).unwrap();
        assert!(permit.arm_return(10, 100));
        assert_eq!(monitor.uncertain(), 1);
        assert!(!permit.can_send(7, 9, Mode::Paste, &target(), 20));
        monitor.observe(20, 101, 20);
        assert!(!permit.can_send(7, 9, Mode::Paste, &target(), 20));
        assert_eq!(monitor.state.lock().unwrap().last_event, Some(101));
        assert!(
            !session
                .quick(9, Mode::Paste)
                .can_send(7, 9, Mode::Paste, &target(), 20)
        );
    }

    #[test]
    fn same_tick_target_event_has_no_proven_order_after_the_arm() {
        let (monitor, mut session) = state();
        let permit = select(&mut session, 0, 1).unwrap();
        assert!(permit.arm_return(10, 100));
        monitor.observe(20, 100, 20);
        assert!(!permit.can_send(7, 9, Mode::Paste, &target(), 20));
    }

    #[test]
    fn a_target_event_during_the_pre_return_wait_cannot_survive_an_aborted_restore() {
        let (monitor, mut session) = state();
        let permit = select(&mut session, 0, 1).unwrap();
        assert!(permit.arm_return(10, 100));
        monitor.observe(20, 101, 20);
        assert!(!permit.return_pending(&target(), 20));
        permit.revoke();
        assert!(!permit.can_send(7, 9, Mode::Paste, &target(), 20));
        let (monitor, session) = state();
        let quick = session.quick(9, Mode::Paste);
        quick.revoke();
        assert!(!quick.can_send(7, 9, Mode::Paste, &target(), 20));
        assert_eq!(
            monitor.first_external(),
            0,
            "主动撤销不清除或伪造旧外部锁存"
        );
    }

    #[test]
    fn delayed_or_out_of_order_events_and_monitor_shutdown_are_conservative() {
        let (monitor, mut session) = state();
        let permit = select(&mut session, 0, 1).unwrap();
        assert!(permit.arm_return(10, 100));
        monitor.observe(20, 99, 20);
        assert!(!permit.check(7, 9, Mode::Paste, &target(), 20));
        let (monitor, mut session) = state();
        monitor.observe(10, 100, 10);
        let permit = select(&mut session, 0, 1).unwrap();
        monitor.observe(10, 99, 10);
        assert!(!permit.check(7, 9, Mode::Paste, &target(), 10));
        assert!(select(&mut session, monitor.revision(), 2).is_ok());
        let (monitor, mut session) = state();
        let permit = select(&mut session, 0, 1).unwrap();
        monitor.stopped();
        assert!(!permit.check(7, 9, Mode::Paste, &target(), 10));
    }

    #[test]
    fn delayed_candidate_events_before_a_fresh_selection_do_not_kill_the_monitor() {
        let (monitor, mut session) = state();
        monitor.observe(30, 100, 10);
        monitor.observe(10, 102, 10);
        monitor.observe(30, 101, 10);
        let permit = select(&mut session, monitor.revision(), 1).unwrap();
        assert!(permit.check(7, 9, Mode::Paste, &target(), 10));
        monitor.observe(40, 103, 10);
        assert!(!permit.check(7, 9, Mode::Paste, &target(), 10));
        assert!(select(&mut session, monitor.revision(), 2).is_ok());
    }

    #[test]
    fn a_late_external_event_still_advances_the_event_time_high_water_mark() {
        let (monitor, mut session) = state();
        monitor.observe(30, 200, 10);
        let permit = select(&mut session, monitor.revision(), 1).unwrap();
        monitor.observe(10, 150, 10);
        assert!(!permit.check(7, 9, Mode::Paste, &target(), 10));
        assert!(select(&mut session, monitor.revision(), 2).is_ok());
    }

    #[test]
    fn destroyed_original_control_and_uncertain_quick_events_cannot_be_reused() {
        let (monitor, mut session) = state();
        let quick = session.quick(9, Mode::Paste);
        monitor.observe(0, 100, 0);
        assert!(!quick.check(7, 9, Mode::Paste, &target(), 20));
        assert!(select(&mut session, monitor.revision(), 1).is_ok());
        monitor.target_destroyed();
        assert!(select(&mut session, monitor.revision(), 2).is_err());
    }

    #[test]
    fn native_edit_identity_is_required_only_for_new_search_permissions() {
        for class in ["Chrome_RenderWidgetHostHWND", "WebView", "", "Scintilla"] {
            let (monitor, mut session) = state();
            session.target.focus_class = class.into();
            let target = session.target.clone();
            let permit = select(&mut session, 0, 1).unwrap();
            assert!(permit.arm_return(10, 100));
            monitor.observe(20, 101, 20);
            assert!(!permit.can_send(7, 9, Mode::Paste, &target, 20), "{class}");
        }
        let (_, mut session) = state();
        session.target.focus = 0;
        assert!(!reliable_control(&session.target));
        for class in [
            "Edit",
            "RichEdit20W",
            "RICHEDIT50W",
            "WindowsForms10.EDIT.app.0.34f5582_r6_ad1",
        ] {
            let (_, mut session) = state();
            session.target.focus_class = class.into();
            assert!(select(&mut session, 0, 1).is_ok(), "{class}");
        }
    }
}
