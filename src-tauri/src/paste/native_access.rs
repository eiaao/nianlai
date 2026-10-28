// 原生粘贴发送前的元数据探针与单一等待预算；不读取剪贴板正文。
use std::time::Duration;

pub const BUDGET: Duration = Duration::from_millis(200);
const POLL: Duration = Duration::from_millis(5);
pub const BUSY: &str = "native_clipboard_busy";
pub const CHANGED: &str = "native_clipboard_changed";
pub const CLOSE_FAILED: &str = "native_clipboard_close_failed";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Publication {
    sequence: u32,
    owner: usize,
}

impl Publication {
    // 写线程必须先成功 Close，再调用；sequence -> 精确 owner -> sequence。
    pub fn sample(
        writer: usize,
        mut sequence: impl FnMut() -> u32,
        owner: impl FnOnce() -> usize,
    ) -> Option<Self> {
        let before = sequence();
        let owner = owner();
        let after = sequence();
        (writer != 0 && before != 0 && before == after && owner == writer).then_some(Self {
            sequence: after,
            owner,
        })
    }
}

pub fn publish(
    writer: usize,
    close: impl FnOnce() -> bool,
    sequence: impl FnMut() -> u32,
    owner: impl FnOnce() -> usize,
) -> Result<Publication, &'static str> {
    // 发布快照在成功关闭之后采样；不假设 Close 前后的 sequence 必须相等。
    if !close() {
        return Err(CLOSE_FAILED);
    }
    Publication::sample(writer, sequence, owner).ok_or(CHANGED)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    Ready,
    Busy,
    Changed,
    CloseFailed,
}

pub trait Clipboard {
    fn open(&mut self) -> bool;
    fn sequence(&mut self) -> u32;
    fn owner(&mut self) -> usize;
    fn close(&mut self) -> bool;
}

pub fn probe(clipboard: &mut impl Clipboard, expected: Publication) -> Probe {
    if !clipboard.open() {
        // GetOpenClipboardWindow 的 NULL 不参与就绪判断，实际 Open 失败始终为 Busy。
        return Probe::Busy;
    }
    let before = clipboard.sequence();
    let owner = clipboard.owner();
    let after = clipboard.sequence();
    // 无论 stamp 是否匹配，都必须显式检查关闭；绝不持 OS 锁同步事件或发送。
    if !clipboard.close() {
        Probe::CloseFailed
    } else if before == expected.sequence && after == expected.sequence && owner == expected.owner {
        Probe::Ready
    } else {
        Probe::Changed
    }
}

pub trait Environment {
    fn now(&self) -> Duration;
    fn pause(&mut self, duration: Duration);
    fn guard(&mut self) -> Result<(), &'static str>;
    fn probe(&mut self, publication: Publication) -> Probe;
}

pub struct Budget {
    started: Duration,
    observed: Duration,
    probes: u32,
    waits: u32,
}

impl Budget {
    pub fn new(now: Duration) -> Self {
        Self {
            started: now,
            observed: now,
            probes: 0,
            waits: 0,
        }
    }

    pub fn probes(&self) -> u32 {
        self.probes
    }

    pub fn waits(&self) -> u32 {
        self.waits
    }

    pub fn elapsed(&self, now: Duration) -> Duration {
        now.saturating_sub(self.started)
    }

    pub fn observed_elapsed(&self) -> Duration {
        self.elapsed(self.observed)
    }

    // prepare 与实际发送紧前重复调用同一个 Budget；只重试条件探针，绝不重试消息。
    pub fn wait(
        &mut self,
        environment: &mut impl Environment,
        publication: Option<Publication>,
    ) -> Result<(), &'static str> {
        let result = self.wait_inner(environment, publication);
        self.observed = environment.now();
        result
    }

    fn wait_inner(
        &mut self,
        environment: &mut impl Environment,
        publication: Option<Publication>,
    ) -> Result<(), &'static str> {
        loop {
            // 每轮（也包括等待返回后）同步事件并复核所有原资格；取消优先于超时。
            environment.guard()?;
            let publication = publication.ok_or(CHANGED)?;
            let remaining = BUDGET.saturating_sub(self.elapsed(environment.now()));
            if remaining.is_zero() {
                return Err(BUSY);
            }
            self.probes += 1;
            match environment.probe(publication) {
                Probe::Ready if self.elapsed(environment.now()) < BUDGET => return Ok(()),
                Probe::Ready => return Err(BUSY),
                Probe::Changed => return Err(CHANGED),
                Probe::CloseFailed => return Err(CLOSE_FAILED),
                Probe::Busy => {}
            }
            let remaining = BUDGET.saturating_sub(self.elapsed(environment.now()));
            if remaining.is_zero() {
                return Err(BUSY);
            }
            self.waits += 1;
            environment.pause(remaining.min(POLL));
        }
    }
}

#[cfg(test)]
#[path = "native_access_tests.rs"]
mod tests;
