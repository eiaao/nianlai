// 通用键盘粘贴的生产编排；平台副作用通过 Backend 注入。
pub fn availability_reason(reason: &'static str) -> &'static str {
    match reason {
        super::native_access::BUSY => "generic_clipboard_busy",
        super::native_access::CHANGED => "generic_clipboard_changed",
        super::native_access::CLOSE_FAILED => "generic_clipboard_close_failed",
        reason => reason,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Submission {
    Sent,
    Confirmed,
    Unknown,
    Refused,
}

impl Submission {
    pub fn from_input_count(sent: u32, expected: usize) -> Self {
        if sent == 0 {
            Self::Refused
        } else if sent as usize == expected {
            Self::Sent
        } else {
            // 非零部分注入也可能已触发目标粘贴，不能降级为“只复制”。
            Self::Unknown
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Completion {
    NotSubmitted(&'static str),
    Submitted(Submission),
}

pub trait Backend {
    fn coordinate(&mut self) -> Result<(), String>;
    fn check_before_publish(&mut self) -> Result<(), &'static str>;
    fn publish(&mut self) -> Result<(), String>;
    fn restore(&mut self) -> Result<(), &'static str>;
    fn prepare_submit(&mut self) -> Result<(), &'static str>;
    fn available(&mut self) -> Result<(), &'static str>;
    fn send(&mut self) -> Result<Submission, &'static str>;
    fn observe(&mut self) -> bool;
    fn finish(&mut self, result: Option<Completion>);
}

pub fn run(backend: &mut impl Backend) -> Result<Completion, String> {
    let result = (|| {
        if let Err(reason) = backend.check_before_publish() {
            return Ok(Completion::NotSubmitted(reason));
        }
        backend.coordinate()?;
        if let Err(reason) = backend.check_before_publish() {
            return Ok(Completion::NotSubmitted(reason));
        }
        // writer 在打开系统剪贴板前完成所有已物化格式的分配/转换。
        backend.publish()?;
        if let Err(reason) = backend
            .check_before_publish()
            .and_then(|()| backend.restore())
            .and_then(|()| backend.prepare_submit())
            .and_then(|()| backend.available())
        {
            return Ok(Completion::NotSubmitted(reason));
        }
        // available 最后执行实际 Open/stamp/Close；发送时不持 OS 锁。
        // 目标异步处理输入前仍可能出现外部竞争，任何结果都不自动重发。
        match backend.send() {
            Err(reason) => Ok(Completion::NotSubmitted(reason)),
            Ok(Submission::Refused) => Ok(Completion::NotSubmitted("send_failed")),
            Ok(Submission::Sent) if backend.observe() => {
                Ok(Completion::Submitted(Submission::Confirmed))
            }
            Ok(submission) => Ok(Completion::Submitted(submission)),
        }
    })();
    backend.finish(result.as_ref().ok().copied());
    result
}

#[cfg(test)]
#[path = "generic_tests.rs"]
mod tests;
