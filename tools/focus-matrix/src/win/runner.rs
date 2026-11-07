use super::{
    input::Input,
    observe,
    process::{Child, Job},
    safety::{Control, Guard},
    target::{Snapshot, Target},
    uia::Uia,
};
use crate::{
    cli::{Hotkey, Mode, Options},
    judge::{self, Class, Evidence, Trial, Verdict},
    logtail::{self, KnownHistory, Tail},
    preflight::{self, Preflight},
    protocol::{self, Digest},
    report::Summary,
    scenario::{Config, Kind, Scenario},
};
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use windows::Win32::{
    System::{Com::CoCreateGuid, Power::*},
    UI::HiDpi::*,
};

struct Awake;
impl Awake {
    fn new() -> Self {
        unsafe {
            SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED);
        }
        Self
    }
}
impl Drop for Awake {
    fn drop(&mut self) {
        unsafe {
            SetThreadExecutionState(ES_CONTINUOUS);
        }
    }
}
struct Temp {
    path: PathBuf,
    parent: PathBuf,
    keep: bool,
}
impl Drop for Temp {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        if let Ok(path) = self.path.canonicalize()
            && path.parent() == Some(self.parent.as_path())
            && path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("nianlai-focus-matrix-"))
            && let Err(e) = std::fs::remove_dir_all(&path)
        {
            eprintln!("临时目录清理失败（可手动删除 {}）：{e}", path.display());
        }
    }
}
fn token() -> Result<String, String> {
    unsafe {
        CoCreateGuid()
            .map(|g| format!("{g:?}").replace(['{', '}', '-'], ""))
            .map_err(|e| e.to_string())
    }
}
fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<(), String> {
    let mut f = File::create(path).map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(&mut f, value).map_err(|e| e.to_string())?;
    f.write_all(b"\n")
        .and_then(|_| f.sync_data())
        .map_err(|e| e.to_string())
}
fn result_error(s: &Scenario, mode: Mode, n: u32, class: Class, message: String) -> Trial {
    Trial {
        scenario: s.id.clone(),
        mode,
        trial: n,
        session: None,
        sample: None,
        action: String::new(),
        result: Verdict {
            class,
            flags: vec![class],
            explanation: message,
        },
        evidence: None,
        hotkey_ms: None,
        result_ms: None,
        details: json!({}),
    }
}
struct Engine {
    input: Input,
    uia: Uia,
    control: Arc<Control>,
    job: Job,
    tail: Tail,
    events: Vec<Value>,
    log: File,
    results_file: File,
    results: Vec<Trial>,
    known: KnownHistory,
    hotkey: Hotkey,
    panel: isize,
    nianlai: Option<Child>,
    config: Config,
    base: PathBuf,
    trial_deadline: Option<Instant>,
}
impl Engine {
    fn poll(&mut self) -> Result<(), String> {
        self.control.check()?;
        for (raw, value) in self.tail.poll()? {
            self.log
                .write_all(raw.as_bytes())
                .and_then(|_| self.log.flush())
                .map_err(|e| e.to_string())?;
            // 别的程序复制的内容会排到历史最前，快速取用的第 1 条就不再是已知的那条。
            if value["event"] == "capture"
                && let Some(record) = value["record_id"].as_u64()
            {
                self.known.written(record, None);
            }
            self.events.push(value);
        }
        Ok(())
    }
    fn tick(&mut self, ms: u64) -> Result<(), String> {
        self.control.check()?;
        if self.trial_deadline.is_some_and(|d| Instant::now() > d) {
            return Err("timeout：单次试验超过总时限".into());
        }
        self.poll()?;
        self.control.wait(ms)
    }
    fn event(
        &mut self,
        from: usize,
        name: &str,
        session: Option<u64>,
        ms: u64,
    ) -> Result<Value, String> {
        let end = Instant::now() + Duration::from_millis(ms);
        loop {
            self.tick(10)?;
            if let Some(v) = logtail::matching(&self.events, from, name, session) {
                return Ok(v.clone());
            }
            if Instant::now() > end {
                return Err(format!("timeout：等待 {name} 超时"));
            }
        }
    }
    fn save(&mut self, result: Trial) -> Result<(), String> {
        serde_json::to_writer(&mut self.results_file, &result).map_err(|e| e.to_string())?;
        self.results_file
            .write_all(b"\n")
            .and_then(|_| self.results_file.flush())
            .and_then(|_| self.results_file.sync_data())
            .map_err(|e| e.to_string())?;
        self.results.push(result);
        Ok(())
    }
    fn start_nianlai(&mut self, options: &Options) -> Result<(), String> {
        if observe::processes()?
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case("nianlai.exe"))
        {
            return Err("启动前复查发现已有 nianlai，拒绝真跑".into());
        }
        let data = self.base.join(preflight::NIANLAI_DATA);
        std::fs::create_dir(&data).map_err(|e| e.to_string())?;
        self.control.check()?;
        let exe = options.exe.canonicalize().map_err(|e| e.to_string())?;
        self.nianlai = Some(self.job.spawn(
            &exe,
            &[],
            &[
                ("NIANLAI_HOTKEY".into(), Some(options.hotkey.clone())),
                ("NIANLAI_G0_SAMPLES".into(), Some("1".into())),
                (
                    "NIANLAI_G0_DATA_DIR".into(),
                    Some(data.to_string_lossy().into()),
                ),
                ("NIANLAI_G0_AUTOCYCLE".into(), None),
                ("NIANLAI_MEMORY_TIER".into(), None),
            ],
        )?);
        let deadline = Instant::now() + Duration::from_millis(self.config.timeouts.startup_ms);
        loop {
            self.tick(20)?;
            if self.nianlai.as_ref().is_some_and(Child::exited) {
                return Err("样机启动后提前退出".into());
            }
            let registered = self
                .events
                .iter()
                .find(|v| v["event"] == "hotkey_registered" && v["purpose"] == "panel");
            let ready = self.events.iter().any(|v| v["event"] == "frontend_ready");
            if ready && let Some(registered) = registered {
                self.hotkey = if registered["fallback"] == true {
                    Hotkey::parse("Ctrl+Shift+F12")?
                } else {
                    Hotkey::parse(&options.hotkey)?
                };
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err("timeout：样机 frontend_ready/hotkey_registered 启动等待超时".into());
            }
        }
    }
    fn panel(&mut self) -> Result<isize, String> {
        let pid = self.nianlai.as_ref().ok_or("样机未启动")?.pid;
        if self.panel != 0 && observe::pid(self.panel) == pid && observe::visible(self.panel) {
            self.input.own(self.panel, pid);
            return Ok(self.panel);
        }
        self.panel = observe::windows()?
            .into_iter()
            .find(|w| w.pid == pid && observe::owned_title(w.raw) == "拈来")
            .map(|w| w.raw)
            .ok_or("未找到已启动样机的可见面板")?;
        self.input.own(self.panel, pid);
        Ok(self.panel)
    }
    fn dismiss(&mut self, target: &Target) -> Result<(), String> {
        self.control.check()?;
        if let Ok(panel) = self.panel()
            && observe::visible(panel)
        {
            self.input.named("Esc", &[target.active, panel])?;
            let deadline = Instant::now() + Duration::from_secs(2);
            while observe::visible(panel) {
                self.tick(20)?;
                if Instant::now() > deadline {
                    return Err("面板 Esc 后没有收起".into());
                }
            }
        }
        Ok(())
    }
    fn shown(&mut self, target: &Target, from: usize) -> Result<Value, String> {
        self.input.chord(&self.hotkey, &[target.active])?;
        let until = Instant::now() + Duration::from_millis(self.config.timeouts.shown_ms);
        let mut handled = from;
        loop {
            self.tick(15)?;
            if let Some(v) = self.events[from..].iter().find(|v| {
                v["event"] == "shown" && v["auto"] == false && v["session"].as_u64().is_some()
            }) {
                return Ok(v.clone());
            }
            if logtail::matching(&self.events, handled, "show_skipped", None).is_some() {
                handled = self.events.len();
                self.tick(150)?;
                self.input.chord(&self.hotkey, &[target.active])?;
            }
            if Instant::now() > until {
                return Err("timeout：等待新的 shown 超时".into());
            }
        }
    }
    fn attempt(&mut self, target: &mut Target, mode: Mode, n: u32) -> Trial {
        let s = target.scenario.clone();
        self.trial_deadline =
            Some(Instant::now() + Duration::from_millis(self.config.timeouts.trial_ms));
        self.control.trial_deadline(self.trial_deadline);
        let mut session = None;
        let result = self.trial(target, mode, n, &mut session);
        self.trial_deadline = None;
        self.control.trial_deadline(None);
        let mut row = match result {
            Ok(row) => row,
            Err(error) => {
                let class = if self.control.check().is_err() {
                    Class::Aborted
                } else if error.starts_with("timeout") {
                    Class::Timeout
                } else {
                    Class::HarnessError
                };
                let mut row = result_error(&s, mode, n, class, error);
                row.session = session;
                row
            }
        };
        if self.control.check().is_ok() {
            if let Err(e) = self.dismiss(target) {
                self.control.stop(format!("面板无法安全收起：{e}"));
            }
            // 粘贴事件超时后不能继续；否则下一次会借用上一轮的内容和日志。
            if row.result.class == Class::Timeout && session.is_some() {
                self.control.stop("试验超时，无法确认样机是否还在发键");
            }
        }
        if self.control.check().is_ok()
            && let Err(error) = target.cleanup_trial(&mut self.input, &self.uia)
        {
            row.details["cleanup_error"] = json!(error);
            if matches!(row.result.class, Class::Pass | Class::Degraded) {
                row.result = Verdict {
                    class: Class::HarnessError,
                    flags: vec![Class::HarnessError],
                    explanation: "试验清理未完成，不计入有效次数".into(),
                };
            }
        }
        if self.control.check().is_err() {
            self.input.release_modifiers();
            row.details["abort_reason"] = json!(self.control.reason());
            if !matches!(row.result.class, Class::Timeout | Class::Aborted) {
                row.result.flags.push(Class::Aborted);
                row.result.class = Class::Aborted;
                row.result.explanation =
                    self.control.reason().unwrap_or_else(|| "试验已中止".into());
            }
        }
        row
    }
    fn trial(
        &mut self,
        target: &mut Target,
        mode: Mode,
        n: u32,
        session_out: &mut Option<u64>,
    ) -> Result<Trial, String> {
        let s = target.scenario.clone();
        let escaped = n > 0 && (n - 1) % 4 == 3;
        let numeric = n == 0 || (n - 1).is_multiple_of(2);
        let query = &s.samples[n.saturating_sub(1) as usize % s.samples.len()];
        let mut notes = vec![];
        let (digit, sample) = if mode == Mode::Search {
            (s.search_digit, query.sample)
        } else {
            let requested = s.quick_digits[n.saturating_sub(1) as usize % s.quick_digits.len()];
            let chosen = if escaped {
                Some((1, query.sample))
            } else {
                self.known
                    .choose(if numeric { requested } else { 1 }, |id| {
                        s.supports_sample(id)
                    })
            };
            let (digit, id) = chosen.ok_or("已知历史深度不足，不能猜测快速取用条目")?;
            if digit != requested && numeric {
                notes.push(format!(
                    "已知顺序不足或示例不适用于本控件，数字 {requested} 回退到 {digit}"
                ));
            }
            (digit, id)
        };
        let before = target.prepare(&mut self.input, &self.uia)?;
        let before_text = before.text.clone().unwrap_or_default();
        let sample_text = crate::samples::all()
            .into_iter()
            .find(|s| s.id == sample)
            .ok_or("示例不存在")?
            .text;
        let expected = if escaped {
            before_text.clone()
        } else {
            protocol::expand(&s.expected, &before_text, &sample_text, &target.before_ext)?
        };
        let expected_digest = if escaped {
            before.digest
        } else {
            Digest::of(&expected)
        };
        let mark = self.control.mark();
        let from = self.events.len();
        let shown = self.shown(target, from)?;
        let session = shown["session"].as_u64().ok_or("shown 缺少会话号")?;
        *session_out = Some(session);
        let panel = self.panel()?;
        let monitors = json!({"target":observe::monitor_window(target.root.raw),"panel":observe::monitor_window(panel),"mouse":observe::monitor_mouse()});
        if mode == Mode::Search {
            let mut chars = query.query.chars();
            let first = chars.next().ok_or("空搜索词")?;
            self.input.first_letter(first, &[target.active])?;
            let ready = self.event(
                from,
                "search_ready",
                Some(session),
                self.config.timeouts.search_ms,
            )?;
            if ready["search_box_ready"] != true {
                return Err("search_ready 没有确认搜索框就绪".into());
            }
            self.input.unicode(chars.as_str(), &[panel])?;
            self.tick(100)?;
            match self
                .uia
                .find(panel, Some("搜索预览和文件名"), None, None)
                .and_then(|e| e.text.ok_or("搜索框没有 Value/Text".into()))
            {
                Ok(value) if value != query.query => {
                    self.dismiss(target)?;
                    return Err(
                        "搜索框文字不等于搜索词，可能仍在中文组合输入；没有发送选择键".into(),
                    );
                }
                Ok(_) => {}
                Err(error) => notes.push(format!("搜索框读不到，按设计继续：{error}")),
            }
        }
        let action = if escaped {
            "Esc".into()
        } else if numeric {
            if mode == Mode::Search {
                format!("Ctrl+{digit}")
            } else {
                digit.to_string()
            }
        } else {
            "Enter".into()
        };
        if !observe::visible(panel) {
            return Err("选择前面板已消失，不发送选择键".into());
        }
        let popup_alive_at_selection = observe::visible(target.active);
        self.input.named(
            &action,
            &[if mode == Mode::Search {
                panel
            } else {
                target.active
            }],
        )?;
        let paste = if escaped {
            self.event(
                from,
                "hide_source",
                Some(session),
                self.config.timeouts.shown_ms,
            )?;
            self.event(
                from,
                "focus_restored",
                Some(session),
                self.config.timeouts.shown_ms,
            )?;
            None
        } else {
            Some(self.event(from, "paste", Some(session), self.config.timeouts.paste_ms)?)
        };
        let deadline = Instant::now() + Duration::from_millis(self.config.timeouts.settle_ms);
        let after = loop {
            self.tick(25)?;
            let after = target.snapshot(&self.uia, Some(&before))?;
            // 留满观察窗口，防止第一段粘贴后才到达的第二段或回车被漏掉。
            if Instant::now() >= deadline {
                break after;
            }
        };
        // 试验期间别的程序复制了内容时，第 1 条已经换了，快速取用的期望不再确定。
        if mode == Mode::Quick
            && !escaped
            && self.events[from..]
                .iter()
                .any(|v| v["event"] == "capture" && v["record_id"].is_u64())
        {
            return Err("试验期间样机记录到其他程序的复制，快速取用的期望条目不确定".into());
        }
        let sequence = self.control.sequence(mark);
        let sequence_labels = std::iter::once(target.active)
            .chain(sequence.iter().copied())
            .map(|raw| {
                if raw == target.root.raw || raw == target.active {
                    "target".to_owned()
                } else if raw == panel {
                    "panel".to_owned()
                } else {
                    format!(
                        "other:{}:{}",
                        observe::class(raw),
                        observe::process_name(observe::pid(raw))
                    )
                }
            })
            .fold(Vec::<String>::new(), |mut v, s| {
                if v.last() != Some(&s) {
                    v.push(s);
                }
                v
            });
        let transition_ok = if mode == Mode::Quick {
            sequence_labels == ["target"]
        } else {
            sequence_labels == ["target", "panel", "target"]
        };
        let (elsewhere, leaked, submitted, protocol_focus, activation_ok) =
            compare_protocol(target, &before, &after, mode);
        let same_element = match (&before.element, &after.element) {
            (Some(a), Some(b)) => self.uia.same(a.id, b.id),
            _ => false,
        };
        let mut e = Evidence {
            expected: expected_digest,
            actual: after.digest,
            before: before.digest,
            elsewhere_changed: elsewhere,
            leaked_key: leaked,
            submitted: submitted || target.disk_changed(),
            focus_ok: after.fg == target.active
                && after.focus == before.focus
                && same_element
                && protocol_focus
                && after.alive,
            transition_ok: transition_ok && activation_ok,
            observed: after.observed,
            claim: paste
                .as_ref()
                .and_then(|p| p["paste_result"].as_str())
                .map(str::to_owned),
            reason: paste
                .as_ref()
                .and_then(|p| p["reason"].as_str())
                .map(str::to_owned),
            clipboard_written: paste
                .as_ref()
                .is_some_and(|p| p["clipboard_written"] == true),
            escaped,
            ..Default::default()
        };
        if s.target.kind == Kind::Bookmark && popup_alive_at_selection && !after.alive {
            e.submitted = true;
        }
        if s.target.kind == Kind::Explorer && mode == Mode::Quick && after.element.is_none() {
            e.submitted = true;
        }
        if let Some(actual) = &after.text
            && Digest::of(actual) == after.digest
            && !escaped
            && protocol::normalize(actual) != expected
            && e.actual == e.expected
        {
            e.leaked_key = true;
        }
        self.control.check()?;
        let actual_sample = crate::samples::all()
            .into_iter()
            .find(|other| {
                protocol::expand(&s.expected, &before_text, &other.text, &target.before_ext)
                    .is_ok_and(|text| {
                        Digest::of(&text) == after.digest
                            && after
                                .text
                                .as_ref()
                                .is_none_or(|actual| protocol::normalize(actual) == text)
                    })
            })
            .map(|s| s.id);
        if !escaped && actual_sample.is_some_and(|id| id != sample) {
            e.wrong_item = actual_sample;
        }
        if !escaped {
            let twice = protocol::expand(
                &s.expected,
                &before_text,
                &format!("{sample_text}{sample_text}"),
                &target.before_ext,
            )?;
            e.duplicate = Digest::of(&twice) == after.digest;
        }
        if let Some(p) = &paste
            && p["clipboard_written"] == true
            && let Some(record) = p["record_id"].as_u64()
        {
            self.known.written(record, actual_sample);
        }
        let verdict = judge::classify(&e);
        let details = json!({"notes":notes,"record_id":paste.as_ref().and_then(|p|p["record_id"].as_u64()),"actual_sample":actual_sample,
            "foreground_sequence":sequence_labels,"native_focus_before":before.focus,"native_focus_after":after.focus,"uia_same":same_element,
            "monitors":monitors,"expected_excerpt":if verdict.class!=Class::Pass{Some(expected.chars().take(200).collect::<String>())}else{None::<String>},
            "actual_excerpt":if verdict.class!=Class::Pass{actual_sample.and_then(|id|crate::samples::all().into_iter().find(|s|s.id==id).map(|s|s.text.chars().take(200).collect::<String>()))}else{None::<String>},
            "warmup":n==0});
        Ok(Trial {
            scenario: s.id,
            mode,
            trial: n,
            session: Some(session),
            sample: Some(sample),
            action,
            result: verdict,
            evidence: Some(e),
            hotkey_ms: shown["hotkey_to_show_ms"].as_f64(),
            result_ms: paste
                .as_ref()
                .and_then(|p| p["selected_to_result_ms"].as_f64()),
            details,
        })
    }
    fn warmup(&mut self, s: &Scenario) -> Result<(), String> {
        let defaults: Config =
            serde_json::from_str(crate::scenario::DEFAULT).map_err(|e| e.to_string())?;
        let mut helper = self
            .config
            .scenarios
            .iter()
            .find(|s| s.target.kind == Kind::Helper)
            .or_else(|| {
                defaults
                    .scenarios
                    .iter()
                    .find(|s| s.target.kind == Kind::Helper)
            })
            .cloned()
            .ok_or("快速预热需要场景文件中有一个 helper 条目")?;
        helper.id = format!("warmup-{}", s.id);
        helper.samples = s.samples.clone();
        helper.quick_digits = vec![1];
        let mut target = Target::launch(&helper, &self.base, &token()?, &self.job, &self.control)?;
        let row = self.attempt(&mut target, Mode::Search, 0);
        let ok = row.result.class == Class::Pass;
        self.save(row)?;
        if !ok {
            return Err("搜索预热未通过，快速取用不猜测历史顺序".into());
        }
        Ok(())
    }
    fn run_scenarios(&mut self, options: &Options, preflight: &Preflight) -> Result<(), String> {
        let scenarios = self
            .config
            .select(&options.only)?
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let last_ready = preflight
            .plan
            .iter()
            .rev()
            .find(|p| p.status == "ready")
            .map(|p| p.id.clone());
        for s in scenarios {
            self.control.check()?;
            let plan = preflight
                .plan
                .iter()
                .find(|p| p.id == s.id)
                .ok_or("计划缺失")?;
            if plan.status != "ready" {
                for &mode in &options.modes {
                    self.save(result_error(
                        &s,
                        mode,
                        1,
                        Class::Skipped,
                        plan.reason.clone(),
                    ))?;
                }
                continue;
            }
            eprintln!("开始场景 {}：{}", s.id, s.name);
            let mut target =
                match Target::launch(&s, &self.base, &token()?, &self.job, &self.control) {
                    Ok(target) => target,
                    Err(error) => {
                        for &mode in &options.modes {
                            self.save(result_error(
                                &s,
                                mode,
                                1,
                                Class::HarnessError,
                                error.clone(),
                            ))?;
                        }
                        continue;
                    }
                };
            if let Err(error) = target
                .focus(&mut self.input, &self.uia)
                .and_then(|_| target.position(&mut self.input))
            {
                for &mode in &options.modes {
                    self.save(result_error(
                        &s,
                        mode,
                        1,
                        Class::HarnessError,
                        error.clone(),
                    ))?;
                }
                continue;
            }
            for n in 1..=options.trials {
                // 即使 --modes 的参数顺序相反，也先搜索再快速，避免依赖样机初始历史。
                for mode in [Mode::Search, Mode::Quick]
                    .into_iter()
                    .filter(|m| options.modes.contains(m))
                {
                    self.control.check()?;
                    if mode == Mode::Quick
                        && (n - 1) % 4 != 3
                        && self.known.choose(1, |id| s.supports_sample(id)).is_none()
                        && let Err(error) = self.warmup(&s)
                    {
                        self.save(result_error(&s, mode, n, Class::HarnessError, error))?;
                        continue;
                    }
                    let row = self.attempt(&mut target, mode, n);
                    eprintln!(
                        "  {} {n}/{}：{:?}",
                        mode.label(),
                        options.trials,
                        row.result.class
                    );
                    self.save(row)?;
                }
            }
            if last_ready.as_ref() == Some(&s.id) && self.control.check().is_ok() {
                let fg = observe::foreground();
                if target.owns(fg)
                    || self
                        .nianlai
                        .as_ref()
                        .is_some_and(|p| observe::pid(fg) == p.pid)
                {
                    let _ = self.input.named("Ctrl+Alt+Shift+Q", &[fg]);
                }
            }
        }
        Ok(())
    }
    fn shutdown(&mut self) {
        self.input.release_modifiers();
        if let Some(child) = &self.nianlai {
            if !child.exited() && self.control.check().is_ok() {
                let fg = observe::foreground();
                // 场景目标已关闭时，只允许样机自己的前台接收退出组合键。
                if observe::pid(fg) == child.pid {
                    let _ = self.input.named("Ctrl+Alt+Shift+Q", &[fg]);
                }
            }
            if !child.wait(Duration::from_secs(2)) {
                let _ = child.kill();
            }
        }
        if let Ok(rows) = self.tail.poll() {
            for (raw, value) in rows {
                let _ = self.log.write_all(raw.as_bytes());
                self.events.push(value);
            }
        }
        let _ = self.log.flush();
    }
}
fn compare_protocol(
    target: &Target,
    before: &Snapshot,
    after: &Snapshot,
    mode: Mode,
) -> (bool, bool, bool, bool, bool) {
    let kind = target.scenario.target.kind;
    let mut elsewhere = false;
    let mut leaked = false;
    let mut submitted = false;
    let mut focus = true;
    let mut activation = true;
    if matches!(kind, Kind::Web | Kind::Address | Kind::Bookmark) {
        match (&before.title, &after.title) {
            (Some(a), Some(b)) => {
                for field in ["input", "textarea", "editable"] {
                    if (kind != Kind::Web || field != target.scenario.target.field)
                        && a.digest(field).ok() != b.digest(field).ok()
                    {
                        elsewhere = true;
                    }
                    for key in ["enter", "esc", "digit", "chars"] {
                        if a.number(&format!("{field}_{key}")).ok()
                            != b.number(&format!("{field}_{key}")).ok()
                        {
                            if matches!(key, "enter" | "esc") {
                                submitted = true;
                            } else {
                                leaked = true;
                            }
                        }
                    }
                }
                submitted |= a.number("loads").ok() != b.number("loads").ok()
                    || a.number("submit").ok() != b.number("submit").ok();
                if kind == Kind::Web {
                    focus = b.fields.get("active") == Some(&target.scenario.target.field)
                        && b.number("focus").ok() == Some(1);
                }
                if mode == Mode::Quick {
                    activation = a.number("blur").ok() == b.number("blur").ok();
                }
            }
            _ => {
                submitted = true;
                focus = false;
            }
        }
        if kind != Kind::Address {
            if before.address.is_some() && after.address.is_some() {
                elsewhere |= before.address != after.address;
            } else {
                focus = false;
            }
        }
        if kind == Kind::Bookmark && !after.alive && mode == Mode::Quick {
            submitted = true;
        }
    } else if kind == Kind::Helper {
        match (&before.title, &after.title) {
            (Some(a), Some(b)) => {
                submitted = a.number("enter").ok() != b.number("enter").ok()
                    || a.number("esc").ok() != b.number("esc").ok();
                leaked = a.number("digit").ok() != b.number("digit").ok()
                    || a.number("chars").ok() != b.number("chars").ok();
                focus = b.number("focus").ok() == Some(1);
                activation =
                    mode != Mode::Quick || a.number("deact").ok() == b.number("deact").ok();
            }
            _ => focus = false,
        }
    } else if kind == Kind::Explorer && after.element.is_none() {
        focus = false;
    }
    (elsewhere, leaked, submitted, focus, activation)
}

pub fn run(options: Options, config: Config, source: String, preflight: Preflight) -> u8 {
    eprintln!("{}", preflight.chinese());
    eprintln!(
        "真跑将接管键盘鼠标，打开计划目标和必要的原生预热窗口；样机粘贴会覆盖原剪贴板。请离开电脑。动鼠标最可靠；也可 Ctrl+C 或在输出目录创建 STOP。"
    );
    let started = crate::now_ms();
    let mut results = vec![];
    let mut abort = None;
    let mut temporary = None;
    let mut output_created = false;
    let work = (|| -> Result<(), String> {
        if options.out.exists() {
            return Err("输出目录已存在；请选择新的 --out，避免覆盖既有结果".into());
        }
        std::fs::create_dir_all(&options.out).map_err(|e| e.to_string())?;
        output_created = true;
        write_json(
            &options.out.join("run.json"),
            &json!({"started_ms":started,"ended_ms":null,"options":options,"preflight":preflight,"scenarios":source,"abort_reason":null}),
        )?;
        let guard = Guard::install(options.out.join("STOP"), options.max_minutes)?;
        let _awake = Awake::new();
        unsafe {
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
                .map_err(|e| e.to_string())?;
        }
        for n in (1..=options.countdown).rev() {
            eprintln!("倒计时 {n} 秒");
            guard.control.wait(1000)?;
        }
        guard.control.check()?;
        let temp_parent = std::env::temp_dir();
        let parent = temp_parent.canonicalize().map_err(|e| e.to_string())?;
        let path = temp_parent.join(format!(
            "nianlai-focus-matrix-{}-{}-{}",
            started,
            std::process::id(),
            token()?
        ));
        std::fs::create_dir(&path).map_err(|e| e.to_string())?;
        temporary = Some(path.clone());
        let _temp = Temp {
            path: path.clone(),
            parent,
            keep: options.keep_temp,
        };
        let file = |name: &str| {
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(options.out.join(name))
                .map_err(|e| e.to_string())
        };
        let mut engine = Engine {
            input: Input::new(guard.control.clone()),
            uia: Uia::new(guard.control.clone())?,
            control: guard.control.clone(),
            job: Job::new()?,
            // 数据目录在启动样机时才建；日志还不存在时从头读，读到的都是本次运行的事件。
            tail: Tail::at_end(&preflight::nianlai_log(&path)).map_err(|e| e.to_string())?,
            events: vec![],
            log: file("nianlai-log.jsonl")?,
            results_file: file("results.jsonl")?,
            results: vec![],
            known: KnownHistory::default(),
            hotkey: Hotkey::parse(&options.hotkey)?,
            panel: 0,
            nianlai: None,
            config: config.clone(),
            base: path,
            trial_deadline: None,
        };
        let result = engine
            .start_nianlai(&options)
            .and_then(|_| engine.run_scenarios(&options, &preflight));
        if let Err(error) = &result {
            engine.control.stop(error.clone());
            engine.input.release_modifiers();
            if let Some(s) = config.select(&options.only)?.first() {
                let _ = engine.save(result_error(
                    s,
                    options.modes[0],
                    0,
                    Class::Aborted,
                    error.clone(),
                ));
            }
        }
        engine.shutdown();
        results = std::mem::take(&mut engine.results);
        result
    })();
    if let Err(error) = work {
        eprintln!("运行停止：{error}");
        abort = Some(error);
    }
    if !output_created {
        return 1;
    }
    let selected = config.select(&options.only).unwrap_or_default();
    let summary = Summary::build(&selected, &options.modes, options.trials, &results);
    let final_write = (|| -> Result<(), String> {
        if !options.out.is_dir() {
            return Err("输出目录未建立".into());
        }
        write_json(&options.out.join("summary.json"), &summary)?;
        std::fs::write(options.out.join("summary.md"), summary.markdown())
            .map_err(|e| e.to_string())?;
        write_json(
            &options.out.join("run.json"),
            &json!({"started_ms":started,"ended_ms":crate::now_ms(),"options":options,"preflight":preflight,"scenarios":source,"temp":temporary,"abort_reason":abort,"real_input_unvalidated_build":true}),
        )?;
        for name in ["results.jsonl", "nianlai-log.jsonl"] {
            if !options.out.join(name).exists() {
                File::create(options.out.join(name)).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    })();
    if let Err(error) = final_write {
        eprintln!("保存汇总失败：{error}");
        return 1;
    }
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&summary).unwrap_or_default()
        );
    } else {
        println!("{}", summary.markdown());
    }
    u8::from(abort.is_some() || !summary.success)
}
