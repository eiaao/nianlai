use crate::paste::evidence::{increased, normalize_newlines, occurrences};
use serde::Serialize;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use windows::{
    Win32::{System::Com::*, UI::Accessibility::*},
    core::{BSTR, Interface},
};

const BEFORE_LIMIT: Duration = Duration::from_millis(150);
const AFTER_LIMIT: Duration = Duration::from_millis(600);
const MAX_TEXT: usize = 200_000;
static PROBE_BUSY: AtomicBool = AtomicBool::new(false);

struct Active;
impl Drop for Active {
    fn drop(&mut self) {
        PROBE_BUSY.store(false, Ordering::Release);
    }
}

#[derive(Clone, Serialize)]
pub struct Evidence {
    pub interface: &'static str,
    pub control_type: Option<i32>,
    pub class_name: String,
    pub before_ms: f64,
    pub delay_ms: f64,
    pub confirmation_ms: Option<f64>,
    pub conclusion: &'static str,
    pub error_code: Option<i32>,
}
impl Default for Evidence {
    fn default() -> Self {
        Self {
            interface: "unavailable",
            control_type: None,
            class_name: String::new(),
            before_ms: 0.0,
            delay_ms: 0.0,
            confirmation_ms: None,
            conclusion: "unavailable",
            error_code: None,
        }
    }
}

struct Com;
impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

enum Reader {
    Value(IUIAutomationValuePattern),
    Text(IUIAutomationTextPattern),
}
impl Reader {
    unsafe fn text(&self) -> windows::core::Result<Option<String>> {
        let value: BSTR = match self {
            Self::Value(value) => unsafe { value.CurrentValue()? },
            Self::Text(text) => unsafe { text.DocumentRange()?.GetText(MAX_TEXT as i32)? },
        };
        // ValuePattern 也设上限，避免大文档参与比对；绝不把长度放进证据结构。
        Ok(if value.len() >= MAX_TEXT {
            None
        } else {
            Some(value.to_string())
        })
    }
}

pub struct Probe {
    before: mpsc::Receiver<Evidence>,
    after: mpsc::Receiver<Evidence>,
    sent: mpsc::Sender<Instant>,
    cancelled: Arc<AtomicBool>,
    started: Instant,
}

impl Probe {
    pub fn start(process: u32, pasted: String) -> Self {
        let started = Instant::now();
        let (before, receive_before) = mpsc::sync_channel(1);
        let (after, receive_after) = mpsc::sync_channel(1);
        let (sent, receive_sent) = mpsc::channel::<Instant>();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel = cancelled.clone();
        let probe = Self {
            before: receive_before,
            after: receive_after,
            sent,
            cancelled,
            started,
        };
        if PROBE_BUSY
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            let _ = before.send(Evidence::default());
            return probe;
        }
        let active = Active;
        // COM 的 300ms 事务不能保证在 150ms 内返回，所以证据工作独立于发键编排。
        // 所有接口和读到的文字都留在这个 MTA 线程，主线程不碰无障碍接口。
        let _ = thread::Builder::new()
            .name("paste-uia".into())
            .spawn(move || {
                let _active = active;
                let mut evidence = Evidence::default();
                let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
                let _com = initialized.is_ok().then(|| Com);
                let baseline = (|| -> windows::core::Result<Option<(Reader, usize, String)>> {
                    unsafe {
                        initialized.ok()?;
                        let automation: IUIAutomation =
                            CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)?;
                        let options = automation.cast::<IUIAutomation2>()?;
                        options.SetConnectionTimeout(300)?;
                        options.SetTransactionTimeout(300)?;
                        let element = automation.GetFocusedElement()?;
                        if element.CurrentProcessId()? as u32 != process
                            || element.CurrentIsPassword()?.as_bool()
                        {
                            return Ok(None);
                        }
                        evidence.control_type =
                            element.CurrentControlType().ok().map(|kind| kind.0);
                        evidence.class_name = element
                            .CurrentClassName()
                            .map(|class| class.to_string())
                            .unwrap_or_default();
                        let reader = if let Ok(value) = element
                            .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                        {
                            evidence.interface = "Value";
                            Reader::Value(value)
                        } else if let Ok(text) = element
                            .GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
                        {
                            evidence.interface = "Text";
                            Reader::Text(text)
                        } else {
                            return Ok(None);
                        };
                        let Some(text) = reader.text()? else {
                            return Ok(None);
                        };
                        let pasted = normalize_newlines(&pasted);
                        let count = occurrences(&normalize_newlines(&text), &pasted);
                        drop(text);
                        Ok(Some((reader, count, pasted)))
                    }
                })();
                evidence.before_ms = started.elapsed().as_secs_f64() * 1000.0;
                let (reader, before_count, pasted) = match baseline {
                    Ok(Some(baseline)) => baseline,
                    Ok(None) => {
                        evidence.interface = "unavailable";
                        let _ = before.send(evidence);
                        return;
                    }
                    Err(error) => {
                        evidence.error_code = Some(error.code().0);
                        evidence.interface = "unavailable";
                        let _ = before.send(evidence);
                        return;
                    }
                };
                if cancel.load(Ordering::Acquire) || started.elapsed() >= BEFORE_LIMIT {
                    evidence.conclusion = "before_timeout";
                    let _ = before.send(evidence);
                    return;
                }
                evidence.conclusion = "pending";
                if before.send(evidence.clone()).is_err() {
                    return;
                }
                let Ok(sent) = receive_sent.recv_timeout(Duration::from_secs(2)) else {
                    return;
                };
                evidence.conclusion = "timeout";
                loop {
                    if cancel.load(Ordering::Acquire) || sent.elapsed() >= AFTER_LIMIT {
                        break;
                    }
                    thread::sleep(
                        Duration::from_millis(25).min(AFTER_LIMIT.saturating_sub(sent.elapsed())),
                    );
                    if sent.elapsed() >= AFTER_LIMIT {
                        break;
                    }
                    match unsafe { reader.text() } {
                        Ok(Some(text)) => {
                            let confirmed = increased(before_count, &text, &pasted);
                            drop(text);
                            if confirmed && sent.elapsed() <= AFTER_LIMIT {
                                evidence.conclusion = "confirmed";
                                break;
                            }
                        }
                        Ok(None) => {
                            evidence.conclusion = "unavailable";
                            break;
                        }
                        Err(error) => {
                            evidence.conclusion = "unavailable";
                            evidence.error_code = Some(error.code().0);
                            break;
                        }
                    }
                }
                evidence.confirmation_ms = Some(sent.elapsed().as_secs_f64() * 1000.0);
                let _ = after.send(evidence);
            });
        probe
    }

    pub fn baseline(&self) -> Evidence {
        let wait = Instant::now();
        let mut evidence = self
            .before
            .recv_timeout(BEFORE_LIMIT.saturating_sub(self.started.elapsed()))
            .unwrap_or_else(|_| {
                self.cancelled.store(true, Ordering::Release);
                Evidence {
                    before_ms: self.started.elapsed().as_secs_f64() * 1000.0,
                    conclusion: "before_timeout",
                    ..Default::default()
                }
            });
        evidence.delay_ms = wait.elapsed().as_secs_f64() * 1000.0;
        evidence
    }

    pub fn sent(&self, at: Instant) {
        let _ = self.sent.send(at);
    }

    pub fn finish(&self, mut before: Evidence, sent: Instant) -> Evidence {
        if before.conclusion != "pending" {
            return before;
        }
        match self
            .after
            .recv_timeout(AFTER_LIMIT.saturating_sub(sent.elapsed()))
        {
            Ok(mut evidence) => {
                evidence.delay_ms = before.delay_ms;
                evidence
            }
            Err(_) => {
                self.cancelled.store(true, Ordering::Release);
                before.conclusion = "timeout";
                before.confirmation_ms = Some(sent.elapsed().as_secs_f64() * 1000.0);
                before
            }
        }
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}
