use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const CANCEL_POLL: Duration = Duration::from_millis(10);

#[derive(Default)]
pub struct CaptureAccess {
    held: Mutex<bool>,
    changed: Condvar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessError {
    Cancelled,
    Timeout,
    Poisoned,
}
impl AccessError {
    pub fn reason(self) -> &'static str {
        match self {
            Self::Cancelled => "request_cancelled",
            Self::Timeout | Self::Poisoned => "clipboard_capture_busy",
        }
    }

    pub fn outcome(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::Poisoned => "poisoned",
        }
    }
}

pub struct AccessGuard {
    access: Arc<CaptureAccess>,
}
impl Drop for AccessGuard {
    fn drop(&mut self) {
        // 释放不能因 poison 再次 panic；poison 本身仍保留，后续获取会明确失败。
        let mut held = self
            .access
            .held
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        *held = false;
        self.access.changed.notify_all();
    }
}

impl CaptureAccess {
    pub fn native(
        self: &Arc<Self>,
        timeout: Duration,
        cancelled: impl Fn() -> bool,
    ) -> Result<AccessGuard, AccessError> {
        self.acquire(Some(Instant::now() + timeout), cancelled)
    }

    pub fn reader(self: &Arc<Self>, stopping: &AtomicBool) -> Result<AccessGuard, AccessError> {
        self.acquire(None, || stopping.load(Ordering::Acquire))
    }

    fn acquire(
        self: &Arc<Self>,
        deadline: Option<Instant>,
        cancelled: impl Fn() -> bool,
    ) -> Result<AccessGuard, AccessError> {
        if cancelled() {
            return Err(AccessError::Cancelled);
        }
        let mut held = self.held.lock().map_err(|_| {
            if cancelled() {
                AccessError::Cancelled
            } else {
                AccessError::Poisoned
            }
        })?;
        loop {
            if cancelled() {
                return Err(AccessError::Cancelled);
            }
            let remaining = deadline.map(|until| until.saturating_duration_since(Instant::now()));
            if remaining == Some(Duration::ZERO) {
                return Err(if cancelled() {
                    AccessError::Cancelled
                } else {
                    AccessError::Timeout
                });
            }
            if !*held {
                *held = true;
                return Ok(AccessGuard {
                    access: self.clone(),
                });
            }
            let wait = remaining.map_or(CANCEL_POLL, |remaining| remaining.min(CANCEL_POLL));
            held = self
                .changed
                .wait_timeout(held, wait)
                .map_err(|_| {
                    if cancelled() {
                        AccessError::Cancelled
                    } else {
                        AccessError::Poisoned
                    }
                })?
                .0;
        }
    }
}

pub struct OwnerSnapshot {
    pub sequence_before: u32,
    pub owner: usize,
    pub sequence_after: u32,
}

pub enum CaptureAttempt<R, E> {
    OwnWrite(u32),
    Opened(CaptureRead<R>),
    OpenFailed(E),
}

pub struct CaptureRead<R> {
    // Rust 按声明顺序 drop：OS clipboard 必须先关闭，再归还进程内许可。
    _resource: R,
    _access: AccessGuard,
}

pub fn capture_attempt<R, E>(
    access: &Arc<CaptureAccess>,
    stopping: &AtomicBool,
    own_owner: usize,
    snapshot: impl FnOnce() -> OwnerSnapshot,
    open: impl FnOnce() -> Result<R, E>,
) -> Result<CaptureAttempt<R, E>, AccessError> {
    let permit = access.reader(stopping)?;
    if stopping.load(Ordering::Acquire) {
        return Err(AccessError::Cancelled);
    }
    let snapshot = snapshot();
    if stopping.load(Ordering::Acquire) {
        return Err(AccessError::Cancelled);
    }
    if own_owner != 0
        && snapshot.owner == own_owner
        && snapshot.sequence_before != 0
        && snapshot.sequence_before == snapshot.sequence_after
    {
        return Ok(CaptureAttempt::OwnWrite(snapshot.sequence_after));
    }
    Ok(match open() {
        Ok(resource) => CaptureAttempt::Opened(CaptureRead {
            _resource: resource,
            _access: permit,
        }),
        Err(error) => CaptureAttempt::OpenFailed(error),
    })
}

#[cfg(test)]
#[path = "capture_access_tests.rs"]
mod tests;
