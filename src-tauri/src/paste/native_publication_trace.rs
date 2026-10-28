// 仅测试构建：记录真实 publish 闭包返回值；不提供任何发布许可，也不调用剪贴板 API。
use serde::Serialize;
use std::{cell::RefCell, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Timed<T> {
    pub value: T,
    pub elapsed_ns: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Owner {
    pub nonzero: bool,
    pub matches_writer: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PublicationTrace {
    pub locked_sequence: Option<Timed<u32>>,
    pub checked_close: Option<Timed<bool>>,
    pub post_sequence_before: Option<Timed<u32>>,
    pub owner: Option<Timed<Owner>>,
    pub post_sequence_after: Option<Timed<u32>>,
}

impl PublicationTrace {
    pub fn complete(&self) -> bool {
        if self.locked_sequence.is_none() {
            return false;
        }
        match self.checked_close {
            None => false,
            Some(close) if close.value => {
                self.post_sequence_before.is_some()
                    && self.owner.is_some()
                    && self.post_sequence_after.is_some()
            }
            Some(_) => {
                self.post_sequence_before.is_none()
                    && self.owner.is_none()
                    && self.post_sequence_after.is_none()
            }
        }
    }
}

pub struct Recorder<N> {
    now: N,
    trace: RefCell<PublicationTrace>,
}

impl<N: Fn() -> Duration> Recorder<N> {
    pub fn new(now: N) -> Self {
        Self {
            now,
            trace: RefCell::new(PublicationTrace::default()),
        }
    }

    fn timed<T>(&self, value: T) -> Timed<T> {
        Timed {
            value,
            elapsed_ns: u64::try_from((self.now)().as_nanos()).unwrap_or(u64::MAX),
        }
    }

    pub fn baseline(&self, value: u32) {
        let value = self.timed(value);
        assert!(
            self.trace
                .borrow_mut()
                .locked_sequence
                .replace(value)
                .is_none()
        );
    }

    pub fn close(&self, value: bool) {
        let value = self.timed(value);
        assert!(
            self.trace
                .borrow_mut()
                .checked_close
                .replace(value)
                .is_none()
        );
    }

    pub fn sequence(&self, value: u32) {
        let value = self.timed(value);
        let mut trace = self.trace.borrow_mut();
        if trace.post_sequence_before.is_none() {
            trace.post_sequence_before = Some(value);
        } else {
            assert!(trace.post_sequence_after.replace(value).is_none());
        }
    }

    pub fn owner(&self, value: usize, writer: usize) {
        let value = self.timed(Owner {
            nonzero: value != 0,
            matches_writer: value == writer,
        });
        assert!(self.trace.borrow_mut().owner.replace(value).is_none());
    }

    pub fn finish(self) -> PublicationTrace {
        self.trace.into_inner()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paste::native_access::{self, Publication};
    use std::cell::Cell;

    fn collect(
        baseline: u32,
        close: bool,
        before: u32,
        owner: usize,
        after: u32,
    ) -> (
        Result<Publication, &'static str>,
        PublicationTrace,
        Vec<&'static str>,
    ) {
        let events = RefCell::new(Vec::new());
        let time = Cell::new(0);
        let recorder = Recorder::new(|| {
            time.set(time.get() + 10);
            Duration::from_nanos(time.get())
        });
        let mut sequences = [before, after].into_iter();
        let result = native_access::publish(
            7,
            || {
                events.borrow_mut().push("test_baseline");
                recorder.baseline(baseline);
                events.borrow_mut().push("close");
                recorder.close(close);
                close
            },
            || {
                events.borrow_mut().push("sequence");
                let sequence = sequences.next().expect("重复调用了 sequence 闭包");
                recorder.sequence(sequence);
                sequence
            },
            || {
                events.borrow_mut().push("owner");
                recorder.owner(owner, 7);
                owner
            },
        );
        (result, recorder.finish(), events.into_inner())
    }

    #[test]
    fn traces_original_publish_closures_once_in_order_with_monotonic_time() {
        for baseline in [0, 41, 42] {
            let (result, trace, events) = collect(baseline, true, 42, 7, 42);
            assert_eq!(
                result,
                Publication::sample(7, || 42, || 7).ok_or(native_access::CHANGED)
            );
            assert!(trace.complete());
            assert_eq!(trace.locked_sequence.unwrap().value, baseline);
            assert!(trace.checked_close.unwrap().value);
            assert_eq!(trace.post_sequence_before.unwrap().value, 42);
            assert_eq!(
                trace.owner.unwrap().value,
                Owner {
                    nonzero: true,
                    matches_writer: true
                }
            );
            assert_eq!(trace.post_sequence_after.unwrap().value, 42);
            assert_eq!(
                events,
                ["test_baseline", "close", "sequence", "owner", "sequence"]
            );
            assert_eq!(
                [
                    trace.locked_sequence.unwrap().elapsed_ns,
                    trace.checked_close.unwrap().elapsed_ns,
                    trace.post_sequence_before.unwrap().elapsed_ns,
                    trace.owner.unwrap().elapsed_ns,
                    trace.post_sequence_after.unwrap().elapsed_ns,
                ],
                [10, 20, 30, 40, 50]
            );
        }
    }

    #[test]
    fn rejected_post_samples_still_have_complete_evidence_without_changing_the_result() {
        for (before, owner, after) in [(0, 7, 0), (42, 7, 0), (42, 7, 43), (42, 0, 42), (42, 8, 42)]
        {
            let (result, trace, events) = collect(41, true, before, owner, after);
            assert_eq!(result, Err(native_access::CHANGED));
            assert!(trace.complete());
            assert_eq!(trace.post_sequence_before.unwrap().value, before);
            assert_eq!(trace.post_sequence_after.unwrap().value, after);
            assert_eq!(trace.owner.unwrap().value.nonzero, owner != 0);
            assert_eq!(trace.owner.unwrap().value.matches_writer, owner == 7);
            assert_eq!(
                events,
                ["test_baseline", "close", "sequence", "owner", "sequence"]
            );
        }
    }

    #[test]
    fn close_failure_keeps_unreached_fields_none_and_never_calls_post_sample_closures() {
        let (result, trace, events) = collect(0, false, 42, 7, 42);
        assert_eq!(result, Err(native_access::CLOSE_FAILED));
        assert!(trace.complete());
        assert_eq!(trace.locked_sequence.unwrap().value, 0);
        assert!(!trace.checked_close.unwrap().value);
        assert_eq!(trace.post_sequence_before, None);
        assert_eq!(trace.owner, None);
        assert_eq!(trace.post_sequence_after, None);
        assert_eq!(events, ["test_baseline", "close"]);
        let json = serde_json::to_value(trace).unwrap();
        assert!(json["post_sequence_before"].is_null());
        assert!(!PublicationTrace::default().complete());
    }
}
