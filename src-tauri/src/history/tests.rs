use super::*;
use crate::platform::hash::sha256;
use content::{Data, HTML, TEXT};
use std::collections::BTreeMap;

fn content(parts: &[(&str, &[u8])]) -> Arc<Content> {
    Content::new(
        parts
            .iter()
            .map(|(name, bytes)| ((*name).into(), Data::Bytes((*bytes).into())))
            .collect(),
        sha256,
    )
    .unwrap()
}
fn event(sequence: u32, observed: Instant) -> CopyEvent {
    CopyEvent {
        time_ms: sequence as u64,
        sequence,
        source_process: "source.exe".into(),
        owner_process: "owner.exe".into(),
        owner_pid: 7,
        observed,
    }
}

#[test]
fn all_ingest_outcomes_preserve_copy_events() {
    let mut history = History::new(sha256);
    let at = Instant::now();
    let text = content(&[(TEXT, b"a")]);
    let rich = content(&[(HTML, b"<b>a</b>"), (TEXT, b"a")]);
    assert_eq!(
        history
            .ingest(text.clone(), event(10, at), false, at)
            .unwrap()
            .outcome,
        "saved"
    );
    assert_eq!(
        history
            .ingest(text.clone(), event(10, at), false, at)
            .unwrap()
            .outcome,
        "merged_same_observation"
    );
    assert_eq!(
        history
            .ingest(rich.clone(), event(11, at), false, at)
            .unwrap()
            .outcome,
        "merged_late_formats"
    );
    assert_eq!(
        history
            .ingest(rich.clone(), event(12, at), false, at)
            .unwrap()
            .outcome,
        "duplicate"
    );
    assert_eq!(
        history
            .ingest(text, event(9, at), false, at)
            .unwrap()
            .outcome,
        "stale"
    );
    assert_eq!(history.records.len(), 1);
    assert_eq!(history.records[0].events.len(), 2);
    assert_eq!(history.records[0].events[0].sequence, 10);
    assert_eq!(history.records[0].events[1].sequence, 12);
    assert_eq!(history.records[0].content.formats.len(), 2);
}

#[test]
fn same_observation_supplements_without_overwriting_original() {
    let mut history = History::new(sha256);
    let at = Instant::now();
    history
        .ingest(content(&[(TEXT, b"a")]), event(1, at), true, at)
        .unwrap();
    let result = history
        .ingest(
            content(&[(TEXT, b"b"), (HTML, b"html")]),
            event(1, at),
            false,
            at,
        )
        .unwrap();
    assert_eq!(result.outcome, "merged_same_observation");
    assert_eq!(history.records[0].content.raw(TEXT), Some(b"a".as_slice()));
    assert_eq!(history.records[0].events.len(), 1);
}

#[test]
fn late_merge_requires_owner_time_superset_and_equal_bytes() {
    let at = Instant::now();
    for case in 0..5 {
        let mut history = History::new(sha256);
        history
            .ingest(content(&[(TEXT, b"a")]), event(1, at), false, at)
            .unwrap();
        let mut next = event(
            2,
            at + Duration::from_millis(if case == 0 { 1001 } else { 1000 }),
        );
        if case == 1 {
            next.owner_pid = 8;
        }
        if case == 2 {
            next.owner_pid = 0;
        }
        let bytes = if case == 3 { b"b" } else { b"a" };
        let rich = content(&[(TEXT, bytes), (HTML, b"html")]);
        let result = history.ingest(rich, next, false, at).unwrap();
        assert_eq!(
            result.outcome,
            if case == 4 {
                "merged_late_formats"
            } else {
                "saved"
            }
        );
    }
}

#[test]
fn fingerprint_is_stable_order_independent_and_includes_every_format() {
    let left = content(&[(TEXT, b"same"), (HTML, b"html")]);
    let right = content(&[(HTML, b"html"), (TEXT, b"same")]);
    assert_eq!(left.fingerprint, right.fingerprint);
    assert_ne!(left.fingerprint, content(&[(TEXT, b"same")]).fingerprint);
    assert_ne!(
        left.fingerprint,
        content(&[(TEXT, b"same"), (HTML, b"other")]).fingerprint
    );
    let files = |path: &str| {
        Content::new(
            BTreeMap::from([(content::FILES.into(), Data::Files(vec![path.into()].into()))]),
            sha256,
        )
        .unwrap()
    };
    assert_eq!(
        files("C:/sample/a").fingerprint,
        files("C:\\sample\\a").fingerprint
    );
    assert_ne!(
        files("C:/sample/A").fingerprint,
        files("C:/sample/a").fingerprint
    );
}

#[test]
fn echo_expires_at_exactly_five_seconds_and_reuse_does_not_add_event() {
    let mut history = History::new(sha256);
    let at = Instant::now();
    let text = content(&[(TEXT, b"a")]);
    history
        .ingest(text.clone(), event(1, at), false, at)
        .unwrap();
    assert!(history.before_write(1, text.fingerprint, at));
    assert_eq!(
        history
            .ingest(
                text.clone(),
                event(2, at),
                false,
                at + Duration::from_millis(4999)
            )
            .unwrap()
            .reason,
        Some("own_echo")
    );
    assert_eq!(
        history
            .ingest(text, event(3, at), false, at + Duration::from_secs(5))
            .unwrap()
            .outcome,
        "duplicate"
    );
    history.reuse(1);
    assert_eq!(history.records[0].events.len(), 2);
    assert!(!history.before_write(999, [0; 32], at));
}

#[test]
fn capacity_evicts_oldest_and_reuse_does_not_change_last_observation() {
    let mut history = History::new(sha256);
    history.max_records = 2;
    history.max_bytes = 3;
    let at = Instant::now();
    history
        .ingest(content(&[(TEXT, b"a")]), event(1, at), false, at)
        .unwrap();
    history
        .ingest(content(&[(TEXT, b"b")]), event(2, at), false, at)
        .unwrap();
    history.reuse(1);
    assert_eq!(history.records[0].id, 1);
    assert_eq!(
        history
            .ingest(
                content(&[(TEXT, b"b"), (HTML, b"c")]),
                event(3, at),
                false,
                at
            )
            .unwrap()
            .outcome,
        "merged_late_formats"
    );
    assert_eq!(history.records.len(), 2);
    history
        .ingest(content(&[(TEXT, b"dd")]), event(4, at), false, at)
        .unwrap();
    assert_eq!(history.records.len(), 1);
    assert!(history.get(1).is_none());
    assert!(history.get(2).is_none());
    assert_eq!(history.records[0].content.bytes, 2);
}

#[test]
fn serial_wrap_and_skipped_observations_are_ordered() {
    assert!(older(u32::MAX, 1));
    assert!(!older(1, u32::MAX));
    let mut history = History::new(sha256);
    let at = Instant::now();
    history
        .ingest(content(&[(TEXT, b"a")]), event(u32::MAX, at), false, at)
        .unwrap();
    history.skipped_sequence(0);
    assert_eq!(
        history
            .ingest(
                content(&[(TEXT, b"a"), (HTML, b"html")]),
                event(1, at),
                false,
                at
            )
            .unwrap()
            .outcome,
        "saved"
    );
}

#[test]
fn preview_is_bounded_and_sample_seeding_does_not_advance_real_sequence() {
    let text = Content::text(&"😀".repeat(400), sha256).unwrap();
    assert_eq!(text.preview.chars().count(), 300);
    let mut history = History::new(sha256);
    history.seed_samples().unwrap();
    assert_eq!(history.records.len(), 50);
    assert_eq!(history.latest_sequence, None);
    let at = Instant::now();
    assert_eq!(
        history
            .ingest(content(&[(TEXT, b"real")]), event(0, at), false, at)
            .unwrap()
            .outcome,
        "saved"
    );
}

#[test]
fn merging_into_existing_content_preserves_both_copy_events() {
    let mut history = History::new(sha256);
    let at = Instant::now();
    let rich = content(&[(TEXT, b"a"), (HTML, b"html")]);
    history
        .ingest(rich.clone(), event(1, at), false, at)
        .unwrap();
    history
        .ingest(content(&[(TEXT, b"a")]), event(2, at), true, at)
        .unwrap();
    history.ingest(rich, event(3, at), false, at).unwrap();
    assert_eq!(history.records.len(), 1);
    assert_eq!(history.records[0].events.len(), 2);
}

#[test]
fn oversized_record_does_not_flush_existing_history() {
    let mut history = History::new(sha256);
    history.max_bytes = 1;
    let at = Instant::now();
    history
        .ingest(content(&[(TEXT, b"a")]), event(1, at), false, at)
        .unwrap();
    assert_eq!(
        history
            .ingest(content(&[(TEXT, b"ab")]), event(2, at), false, at)
            .unwrap()
            .reason,
        Some("capacity")
    );
    assert!(history.get(1).is_some());
}

#[test]
fn default_count_limit_and_frontend_preview_limit_are_enforced() {
    let mut history = History::new(sha256);
    let at = Instant::now();
    for sequence in 1..=1001 {
        let bytes = sequence.to_string();
        history
            .ingest(
                content(&[(TEXT, bytes.as_bytes())]),
                event(sequence, at),
                false,
                at,
            )
            .unwrap();
    }
    assert_eq!(history.records.len(), 1000);
    assert!(history.get(1).is_none());
    let snapshot = history.snapshot();
    assert_eq!(snapshot.total, 1000);
    assert_eq!(snapshot.items.len(), 300);
    assert_eq!(snapshot.items[0].id, 1001);
}
