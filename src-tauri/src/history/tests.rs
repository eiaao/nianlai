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
        source_path: String::new(),
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
    assert!(history.before_write(1, text.fingerprint, text.fingerprint, at));
    assert!(!history.before_write(1, [0; 32], text.fingerprint, at));
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
    assert!(!history.before_write(999, [0; 32], [0; 32], at));
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
    assert_eq!(history.records.len(), 300);
    assert!(history.get(1).is_none());
    let snapshot = history.snapshot();
    assert_eq!(snapshot.total, 300);
    assert_eq!(snapshot.items.len(), 300);
    assert_eq!(snapshot.items[0].id, 1001);
}

// E01：内存模式的收藏接口与磁盘模式同名同义；顺序稳定、容量淘汰不清理收藏（B03）。
#[test]
fn memory_favorites_are_ordered_stable_and_survive_eviction() {
    let mut history = History::new(sha256);
    let at = Instant::now();
    history.max_records = 3;
    for sequence in 1..=3 {
        history
            .ingest(
                content(&[(TEXT, sequence.to_string().as_bytes())]),
                event(sequence, at),
                false,
                at,
            )
            .unwrap();
    }
    history.set_favorite(1, true).unwrap();
    history.set_favorite(3, true).unwrap();
    let order = |history: &History| {
        history
            .favorites()
            .into_iter()
            .map(|item| item.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(order(&history), vec![1, 3]);
    // 再次复制与复用不改变收藏顺序（愉悦标准：可预期）。
    history
        .ingest(content(&[(TEXT, b"1")]), event(4, at), false, at)
        .unwrap();
    history.reuse(3);
    assert_eq!(order(&history), vec![1, 3]);
    // 容量淘汰只动未收藏的记录。
    history
        .ingest(content(&[(TEXT, b"5")]), event(5, at), false, at)
        .unwrap();
    assert!(history.get(2).is_none());
    assert!(history.get(1).is_some());
    assert_eq!(order(&history), vec![1, 3]);
    // 取消后从收藏列表消失；再收藏排到最后。
    history.set_favorite(1, false).unwrap();
    assert_eq!(order(&history), vec![3]);
    history.set_favorite(1, true).unwrap();
    assert_eq!(order(&history), vec![3, 1]);
    // 不存在的记录返回 None。
    assert!(history.set_favorite(999, true).is_none());
    // 快照项带收藏字段与来源显示名（无路径来源退回进程名原样）。
    let listed = history.snapshot();
    let favorited: Vec<_> = listed
        .items
        .iter()
        .filter(|item| item.favorite)
        .map(|item| (item.id, item.favorite_position))
        .collect();
    assert_eq!(favorited, vec![(3, Some(2)), (1, Some(3))]);
    let first = listed.items.iter().find(|item| item.id == 1).unwrap();
    assert_eq!(first.source_name.as_deref(), Some("source.exe"));
    assert_eq!(first.source_icon, None);
}

// A06：内存模式记来源路径；显示名退回文件名去扩展名，无路径时用进程名。
#[test]
fn memory_source_names_fall_back_to_file_stem_then_process() {
    let mut history = History::new(sha256);
    let at = Instant::now();
    let mut with_path = event(1, at);
    with_path.source_path = r"C:\Program Files\Editor\The Editor.exe".into();
    history
        .ingest(content(&[(TEXT, b"p")]), with_path, false, at)
        .unwrap();
    let mut without_path = event(2, at);
    without_path.source_path = String::new();
    history
        .ingest(content(&[(TEXT, b"n")]), without_path, false, at)
        .unwrap();
    let items = history.snapshot().items;
    assert_eq!(items[0].source_name.as_deref(), Some("source.exe"));
    assert_eq!(items[1].source_name.as_deref(), Some("The Editor"));
}
