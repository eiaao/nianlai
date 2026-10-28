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
    selection_nonce: u64,
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
        let mut state = self.monitor.state.lock().unwrap();
        if !state.alive
            || state.target_lost
            || selection.revision != state.revision
            || foreground != self.monitor.panel
        {
            return Err("selection_boundary_changed");
        }
        state.selection_nonce = selection.nonce;
        Ok(Permit {
            session,
            record,
            mode,
            target: Identity::from(&self.target),
            source: Source::Selection(selection.revision),
            nonce: selection.nonce,
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
            nonce: 0,
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
    nonce: u64,
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
    // 原生提交在面板仍为前台时完成，不能借用要求目标前台的发键许可。
    pub fn can_submit_native(
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
            || mode == Mode::Copy
            || Identity::from(target) != self.target
            || !super::native::standard_edit(&target.focus_class)
            || !reliable_control(target)
            || foreground != self.monitor.panel
            || !self.search()
        {
            return false;
        }
        let state = self.monitor.state.lock().unwrap();
        self.valid_state(&state) && state.returning.is_none() && self.nonce == state.selection_nonce
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
    fn native_permission_binds_selection_nonce_identity_and_the_unarmed_panel() {
        let (monitor, mut session) = state();
        let permit = select(&mut session, 0, 1).unwrap();
        assert!(permit.can_submit_native(7, 9, Mode::Paste, &target(), 10));
        assert!(!permit.can_submit_native(8, 9, Mode::Paste, &target(), 10));
        assert!(!permit.can_submit_native(7, 8, Mode::Paste, &target(), 10));
        assert!(!permit.can_submit_native(7, 9, Mode::Text, &target(), 10));
        assert!(!permit.can_submit_native(7, 9, Mode::Paste, &target(), 20));
        let mut replaced = target();
        replaced.focus = 22;
        assert!(!permit.can_submit_native(7, 9, Mode::Paste, &replaced, 10));
        assert!(
            !session
                .quick(9, Mode::Paste)
                .can_submit_native(7, 9, Mode::Paste, &target(), 10)
        );
        let newer = select(&mut session, monitor.revision(), 2).unwrap();
        assert!(
            !permit.can_submit_native(7, 9, Mode::Paste, &target(), 10),
            "旧选择 nonce 不能复用"
        );
        assert!(newer.can_submit_native(7, 9, Mode::Paste, &target(), 10));
        assert!(newer.arm_return(10, 100));
        assert!(!newer.can_submit_native(7, 9, Mode::Paste, &target(), 10));
    }

    // 真实 Monitor/Permit、原生提交 runner 和 direct-only 归还组合重放；只替换平台副作用。
    struct NativeReplay {
        monitor: Arc<Monitor>,
        permit: Permit,
        target: Target,
        current: usize,
        operations: Vec<&'static str>,
        change_during_write: Option<&'static str>,
        change_after_submit: Option<&'static str>,
        input_desktop: Option<bool>,
        identity_valid: bool,
        cancelled: bool,
        after_submit_switch: bool,
        submission: crate::paste::native::Submission,
        attempted: usize,
        queued_switch: bool,
        synchronized: bool,
        ordinary: bool,
        written: Arc<crate::history::content::Content>,
        published: Option<Arc<crate::history::content::Content>>,
        change_before_submit: Option<&'static str>,
        generic_preflight_started: bool,
        finished: usize,
    }
    impl NativeReplay {
        fn new() -> Self {
            let (monitor, mut session) = state();
            monitor.observe(30, 100, 10); // 现场选择前的非当前候选事件。
            let permit = select(&mut session, monitor.revision(), 1).unwrap();
            Self {
                monitor,
                permit,
                target: target(),
                current: 10,
                operations: Vec::new(),
                change_during_write: None,
                change_after_submit: None,
                input_desktop: Some(true),
                identity_valid: true,
                cancelled: false,
                after_submit_switch: false,
                submission: crate::paste::native::Submission::Sent,
                attempted: 0,
                queued_switch: false,
                synchronized: true,
                ordinary: true,
                written: crate::history::content::Content::text(
                    "synthetic replay",
                    nianlai_store::hash::sha256,
                )
                .unwrap(),
                published: None,
                change_before_submit: None,
                generic_preflight_started: false,
                finished: 0,
            }
        }
        fn change(&mut self, kind: &str) {
            match kind {
                "switch" => {
                    self.queued_switch = true;
                }
                "destroy" => {
                    self.monitor.target_destroyed();
                }
                "identity" => {
                    self.identity_valid = false;
                    self.target.focus = 22;
                }
                "stop" => {
                    self.monitor.stopped();
                    self.cancelled = true;
                }
                "cancel" => {
                    self.cancelled = true;
                    self.permit.revoke();
                }
                "desktop_inactive" => self.input_desktop = Some(false),
                "desktop_unknown" => self.input_desktop = None,
                "noncurrent" => {
                    self.monitor.observe(30, 150, self.current);
                }
                "ordinary_unknown" => self.ordinary = false,
                "fence" => self.synchronized = false,
                _ => panic!("未知回放变化"),
            }
        }
        fn cleanup(&mut self) {
            if self.permit.arm_return(self.current, 200) {
                crate::platform::focus_restore::run_direct(&mut ReplayFocus(self), |_, _| {});
            }
            self.operations.extend(["hide", "blur"]);
            // 恢复/隐藏后再次产生现场 seq4；不豁免，也不能重写既有提交结果。
            self.monitor.observe(30, 202, self.current);
        }

        fn generic_boundary(&mut self) -> Result<(), &'static str> {
            let synchronized = crate::paste::native::Backend::synchronize(self);
            if self.cancelled {
                return Err("request_cancelled");
            }
            if !synchronized {
                return Err("selection_boundary_changed");
            }
            if !crate::paste::native::input_desktop_ready(self.input_desktop, || {
                self.permit.revoke()
            }) {
                return Err("generic_input_desktop_unavailable");
            }
            if !self.identity_valid
                || !self
                    .permit
                    .check(7, 9, Mode::Paste, &self.target, self.current)
            {
                return Err("selection_boundary_changed");
            }
            if !self.ordinary {
                return Err("target_may_be_elevated");
            }
            Ok(())
        }

        #[cfg(windows)]
        fn candidate(&self, builtin: bool) -> bool {
            let request = crate::paste::request::Request {
                session: 7,
                target: self.target.clone(),
                panel: 10,
                record_id: 9,
                content: self.written.clone(),
                mode: Mode::Paste,
                started: std::time::Instant::now(),
                external_switch: false,
                permit: Some(self.permit.clone()),
                copy_only_reason: None,
                cancelled: Arc::new(std::sync::atomic::AtomicBool::new(self.cancelled)),
            };
            crate::platform::paste::native_candidate(&request, &self.written, builtin)
        }
    }
    struct ReplayFocus<'a>(&'a mut NativeReplay);
    impl crate::platform::focus_restore::Backend for ReplayFocus<'_> {
        type Attachment = ();
        fn snapshot(&mut self) -> crate::platform::focus_restore::Snapshot {
            let replay = &self.0;
            crate::platform::focus_restore::Snapshot {
                focus: crate::platform::FocusResult {
                    foreground: crate::platform::ForegroundResult {
                        path: "unchanged",
                        success: replay.current == 20,
                    },
                    focus_restored: replay.current == 20,
                    focus_unknown: false,
                },
                target_valid: replay.identity_valid,
                original_valid: replay.identity_valid,
                permitted: crate::paste::native::input_desktop_ready(replay.input_desktop, || {
                    replay.permit.revoke();
                }) && (replay.permit.current(&replay.target, replay.current)
                    || replay.permit.return_pending(&replay.target, replay.current)),
            }
        }
        fn activate(&mut self) -> crate::platform::ForegroundResult {
            self.0.operations.push("activate");
            self.0.current = 20;
            self.0.monitor.observe(20, 201, 20);
            crate::platform::ForegroundResult {
                path: "direct_only",
                success: true,
            }
        }
        fn attach(&mut self) {
            panic!("原生提交后的归还不得连接输入队列");
        }
        fn set_focus(&mut self) {
            panic!("原生提交后的归还不得 SetFocus");
        }
    }
    impl crate::paste::native::Backend for NativeReplay {
        fn synchronize(&mut self) -> bool {
            self.operations.push("fence");
            if !self.synchronized {
                self.permit.revoke();
                return false;
            }
            if self.queued_switch {
                self.monitor.observe(40, 150, 40);
                self.monitor.observe(10, 151, 10);
                self.queued_switch = false;
            }
            true
        }
        fn check(&mut self) -> Result<(), &'static str> {
            self.operations.push("check");
            let input_desktop =
                crate::paste::native::input_desktop_ready(self.input_desktop, || {
                    self.permit.revoke()
                });
            crate::paste::native::Conditions {
                cancelled: self.cancelled,
                input_desktop,
                target_valid: self.identity_valid,
                permitted: self.permit.can_submit_native(
                    7,
                    9,
                    Mode::Paste,
                    &self.target,
                    self.current,
                ),
                modifiers_released: true,
                ordinary: self.ordinary,
            }
            .check()
        }
        fn write(&mut self) -> Result<(), String> {
            self.operations.push("write");
            self.published = Some(self.written.clone());
            if let Some(change) = self.change_during_write {
                self.change(change);
            }
            Ok(())
        }
        fn prepare_submit(&mut self) -> Result<(), &'static str> {
            self.operations.push("prepare");
            if let Some(change) = self.change_before_submit {
                self.change(change);
            }
            Ok(())
        }
        fn submit(&mut self) -> Result<crate::paste::native::Submission, &'static str> {
            self.operations.push("submit");
            self.attempted += 1;
            if let Some(change) = self.change_after_submit {
                self.change(change);
            }
            if self.after_submit_switch {
                self.monitor.observe(40, 160, 40);
                self.current = 40;
                self.cancelled = true;
            }
            Ok(self.submission)
        }
        fn finish(&mut self, result: Option<crate::paste::native::Completion>) {
            self.finished += 1;
            if matches!(result, Some(crate::paste::native::Completion::Submitted(_))) {
                self.cleanup();
            }
        }
    }

    // 同一真实许可状态配合生产 generic runner；平台副作用仍由上述 replay 注入。
    impl crate::paste::generic::Backend for NativeReplay {
        fn coordinate(&mut self) -> Result<(), String> {
            self.operations.push("coordinate");
            Ok(())
        }
        fn check_before_publish(&mut self) -> Result<(), &'static str> {
            self.generic_boundary()
        }
        fn publish(&mut self) -> Result<(), String> {
            crate::paste::native::Backend::write(self)
        }
        fn restore(&mut self) -> Result<(), &'static str> {
            self.operations.push("restore");
            self.cleanup();
            Ok(())
        }
        fn prepare_submit(&mut self) -> Result<(), &'static str> {
            self.operations.push("prepare_boundary");
            self.generic_boundary()?;
            self.generic_preflight_started = true;
            self.operations.push("preflight");
            Ok(())
        }
        fn available(&mut self) -> Result<(), &'static str> {
            self.operations.push("available");
            self.generic_boundary()
        }
        fn send(&mut self) -> Result<crate::paste::generic::Submission, &'static str> {
            self.generic_boundary()?;
            if !self
                .permit
                .can_send(7, 9, Mode::Paste, &self.target, self.current)
            {
                return Err("selection_boundary_changed");
            }
            self.operations.push("send");
            self.attempted += 1;
            Ok(crate::paste::generic::Submission::Sent)
        }
        fn observe(&mut self) -> bool {
            false
        }
        fn finish(&mut self, _: Option<crate::paste::generic::Completion>) {
            self.finished += 1;
            self.operations.push("generic_finish");
        }
    }

    #[cfg(windows)]
    fn multiformat_replay_content(companions: &[&str]) -> Arc<crate::history::content::Content> {
        use crate::history::content::{Content, Data, TEXT};
        let text = Content::text("synthetic 中文 replay", nianlai_store::hash::sha256).unwrap();
        let mut formats =
            std::collections::BTreeMap::from([(TEXT.into(), text.formats[TEXT].data.clone())]);
        for name in companions {
            formats.insert((*name).into(), Data::Bytes(Arc::from(name.as_bytes())));
        }
        Content::new(formats, nianlai_store::hash::sha256).unwrap()
    }

    #[test]
    #[cfg(windows)]
    fn native_multiformat_i1_replay_submits_before_restore_while_generic_refuses_at_prepare_boundary()
     {
        use crate::{
            history::content::{HTML, RTF},
            paste::{as_text, generic, native},
        };
        for companions in [vec![HTML], vec![RTF], vec![HTML, RTF]] {
            let content = multiformat_replay_content(&companions);
            let (written, conversion) = as_text::apply(
                &content,
                Mode::Paste,
                None,
                None,
                nianlai_store::hash::sha256,
            );
            assert!(conversion.is_null());
            assert!(Arc::ptr_eq(&content, &written));

            let mut generic = NativeReplay::new();
            generic.written = written.clone();
            assert_eq!(
                generic.permit.current_revision(),
                1,
                "选择前非当前事件已经由新选择固定"
            );
            assert!(
                !generic.candidate(false),
                "正式Ctrl+V覆盖规则仍选择通用路径"
            );
            assert_eq!(
                generic::run(&mut generic).unwrap(),
                generic::Completion::NotSubmitted("selection_boundary_changed")
            );
            assert_eq!(
                (generic.attempted, generic.finished, generic.current),
                (0, 1, 20)
            );
            assert!(generic.operations.contains(&"prepare_boundary"));
            assert!(!generic.generic_preflight_started);
            assert!(!generic.operations.contains(&"available"));
            assert!(!generic.operations.contains(&"send"));

            let mut native = NativeReplay::new();
            native.written = written.clone();
            assert!(native.candidate(true));
            assert_eq!(
                native::run(&mut native).unwrap(),
                native::Completion::Submitted(native::Submission::Sent)
            );
            let position = |operation| {
                native
                    .operations
                    .iter()
                    .position(|name| *name == operation)
                    .unwrap()
            };
            assert!(position("submit") < position("activate"));
            assert!(position("activate") < position("hide"));
            assert_eq!((native.attempted, native.finished), (1, 1));
            assert!(
                !native.permit.current(&native.target, native.current),
                "同一归还后非当前事件仍撤销许可，不豁免IME"
            );
            for published in [generic.published.unwrap(), native.published.unwrap()] {
                assert!(Arc::ptr_eq(&published, &written));
                assert_eq!(published.formats.len(), content.formats.len());
                for (name, format) in &content.formats {
                    assert!(published.formats[name].data == format.data);
                }
            }
        }
    }

    #[test]
    #[cfg(windows)]
    fn native_multiformat_pre_submit_noncurrent_switch_return_and_other_revocations_never_submit() {
        use crate::history::content::{HTML, RTF};
        for companions in [vec![HTML], vec![RTF], vec![HTML, RTF]] {
            for change in [
                "noncurrent",
                "switch",
                "cancel",
                "identity",
                "destroy",
                "desktop_unknown",
                "ordinary_unknown",
                "fence",
            ] {
                for stage in ["before", "write", "prepare"] {
                    let mut replay = NativeReplay::new();
                    replay.written = multiformat_replay_content(&companions);
                    assert!(replay.candidate(true));
                    match stage {
                        "before" => replay.change(change),
                        "write" => replay.change_during_write = Some(change),
                        "prepare" => replay.change_before_submit = Some(change),
                        _ => unreachable!(),
                    }
                    let _ = crate::paste::native::run(&mut replay);
                    assert_eq!(
                        (replay.attempted, replay.finished),
                        (0, 1),
                        "{change}/{stage}"
                    );
                    assert!(!replay.operations.contains(&"activate"));
                    assert!(!replay.operations.contains(&"send"));
                }
            }
        }
    }

    #[test]
    #[cfg(windows)]
    fn native_multiformat_post_submit_noncurrent_cancel_and_unknown_keep_one_submission() {
        use crate::{
            history::content::{HTML, RTF},
            paste::native::{self, Completion, Submission},
        };
        for companions in [vec![HTML], vec![RTF], vec![HTML, RTF]] {
            for submission in [Submission::Sent, Submission::Unknown] {
                for change in ["noncurrent", "cancel", "switch", "desktop_unknown"] {
                    let mut replay = NativeReplay::new();
                    replay.written = multiformat_replay_content(&companions);
                    replay.submission = submission;
                    if change == "switch" {
                        replay.after_submit_switch = true;
                    } else {
                        replay.change_after_submit = Some(change);
                    }
                    assert_eq!(
                        native::run(&mut replay).unwrap(),
                        Completion::Submitted(submission)
                    );
                    assert_eq!((replay.attempted, replay.finished), (1, 1));
                    assert!(!replay.operations.contains(&"activate"));
                    assert!(!replay.operations.contains(&"send"));
                }
            }
        }
    }

    #[test]
    fn production_native_flow_replays_c1_and_preserves_submission_before_cleanup_events() {
        use crate::paste::native::{Completion, Submission, run};
        let mut legacy = NativeReplay::new();
        legacy.cleanup();
        assert!(
            !legacy.permit.current(&legacy.target, legacy.current),
            "既有先归还/隐藏顺序的首个 perform 检查失败"
        );
        assert_eq!(legacy.attempted, 0);
        let mut replay = NativeReplay::new();
        assert_eq!(
            run(&mut replay).unwrap(),
            Completion::Submitted(Submission::Sent)
        );
        assert_eq!(
            replay.operations,
            [
                "fence", "check", "write", "fence", "check", "prepare", "fence", "check", "submit",
                "activate", "hide", "blur"
            ]
        );
        assert_eq!(replay.attempted, 1);
        assert!(
            !replay.permit.current(&replay.target, replay.current),
            "seq4 仍撤销许可，已提交事实保持"
        );
    }

    #[test]
    fn production_native_flow_refuses_read_and_write_phase_revocation() {
        for change in ["switch", "destroy", "identity", "stop", "cancel"] {
            for during_write in [false, true] {
                let mut replay = NativeReplay::new();
                if during_write {
                    replay.change_during_write = Some(change);
                } else {
                    replay.change(change);
                }
                let _ = crate::paste::native::run(&mut replay);
                assert_eq!(replay.attempted, 0, "{change}, write={during_write}");
                assert!(
                    !replay.operations.contains(&"activate"),
                    "撤销后不抢回原窗口"
                );
            }
        }
    }

    #[test]
    fn production_native_flow_refuses_inactive_or_unknown_desktop_at_either_guard() {
        for change in ["desktop_inactive", "desktop_unknown"] {
            for during_write in [false, true] {
                let mut replay = NativeReplay::new();
                if during_write {
                    replay.change_during_write = Some(change);
                } else {
                    replay.change(change);
                }
                assert_eq!(
                    crate::paste::native::run(&mut replay).unwrap(),
                    crate::paste::native::Completion::NotSubmitted(
                        "native_input_desktop_unavailable"
                    )
                );
                assert_eq!(replay.attempted, 0, "{change}, write={during_write}");
                assert!(!replay.operations.contains(&"activate"));
            }
        }
    }

    #[test]
    fn input_desktop_recovery_cannot_reuse_a_revoked_selection() {
        use crate::paste::native::Backend;
        for status in [Some(false), None] {
            let mut replay = NativeReplay::new();
            replay.input_desktop = status;
            assert_eq!(replay.check(), Err("native_input_desktop_unavailable"));
            replay.input_desktop = Some(true);
            assert_eq!(replay.check(), Err("selection_boundary_changed"));
            assert_eq!(
                crate::paste::native::run(&mut replay).unwrap(),
                crate::paste::native::Completion::NotSubmitted("selection_boundary_changed")
            );
            assert_eq!(replay.attempted, 0);
            assert!(!replay.operations.contains(&"activate"));
        }
    }

    #[test]
    fn submitted_result_survives_desktop_loss_without_cleanup_stealing_foreground() {
        use crate::paste::native::{Completion, Submission, run};
        for change in ["desktop_inactive", "desktop_unknown"] {
            for submission in [Submission::Sent, Submission::Unknown] {
                let mut replay = NativeReplay::new();
                replay.submission = submission;
                replay.change_after_submit = Some(change);
                assert_eq!(run(&mut replay).unwrap(), Completion::Submitted(submission));
                assert_eq!(replay.attempted, 1);
                assert_eq!(replay.current, 10);
                assert!(!replay.operations.contains(&"activate"));
            }
        }
    }

    #[test]
    fn submitted_ack_and_unknown_survive_later_cancel_without_stealing_foreground() {
        use crate::paste::native::{Completion, Submission, run};
        for submission in [Submission::Sent, Submission::Unknown] {
            let mut replay = NativeReplay::new();
            replay.submission = submission;
            replay.after_submit_switch = true;
            assert_eq!(run(&mut replay).unwrap(), Completion::Submitted(submission));
            assert_eq!(replay.attempted, 1);
            assert_eq!(replay.current, 40);
            assert!(!replay.operations.contains(&"activate"));
        }
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
