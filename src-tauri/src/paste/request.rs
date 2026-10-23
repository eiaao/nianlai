use crate::{history::content::Content, platform::Target};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Instant};

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Paste,
    Copy,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Copied,
    Sent,
    Confirmed,
    CopiedOnly,
}

pub fn conclude(mode: Mode, sent: bool, confirmed: bool, degraded: bool) -> Outcome {
    if mode == Mode::Copy {
        Outcome::Copied
    } else if degraded || !sent {
        Outcome::CopiedOnly
    } else if confirmed {
        Outcome::Confirmed
    } else {
        Outcome::Sent
    }
}

pub struct Request {
    pub target: Target,
    pub panel: usize,
    pub record_id: usize,
    pub content: Arc<Content>,
    pub mode: Mode,
    pub started: Instant,
    pub external_switch: bool,
    pub restore_focus: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_content_evidence_confirms_a_successfully_sent_paste() {
        assert_eq!(conclude(Mode::Copy, false, false, false), Outcome::Copied);
        assert_eq!(conclude(Mode::Paste, true, false, false), Outcome::Sent);
        assert_eq!(conclude(Mode::Paste, true, true, false), Outcome::Confirmed);
        assert_eq!(
            conclude(Mode::Paste, false, true, false),
            Outcome::CopiedOnly
        );
        assert_eq!(conclude(Mode::Paste, true, true, true), Outcome::CopiedOnly);
    }
}
