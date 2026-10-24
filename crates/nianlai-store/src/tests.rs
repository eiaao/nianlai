use super::*;
use content::{Content, Data, FILES, HTML};
use hash::sha256;
use std::{collections::BTreeMap, fs, path::PathBuf, sync::Arc, time::Duration};

pub(crate) struct Temp(pub(crate) PathBuf);
impl Temp {
    pub(crate) fn new() -> Self {
        let path = std::env::temp_dir().join(format!("nianlai-store-test-{}", snapshot::stamp()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    pub(crate) fn store(&self) -> Store {
        Store::open(&self.0, sha256).unwrap()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn text(value: &str) -> Arc<Content> {
    Content::text(value, sha256).unwrap()
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
fn rich(value: &str) -> Arc<Content> {
    text(value)
        .merge(
            &Content::new(
                BTreeMap::from([(HTML.into(), Data::Bytes(b"<b>html</b>".as_slice().into()))]),
                sha256,
            )
            .unwrap(),
            sha256,
        )
        .unwrap()
}
fn ingest(store: &Store, content: Arc<Content>, sequence: u32, at: Instant) -> Ingest {
    store
        .ingest(content, event(sequence, at), false, at)
        .unwrap()
}

#[test]
fn create_reopen_and_refuse_future_version_without_touching_file() {
    let temp = Temp::new();
    let store = temp.store();
    assert_eq!(store.report().version, 1);
    let c = store.reader().unwrap();
    assert_eq!(
        c.connection
            .pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    drop(c);
    drop(store);
    let c = writable(&temp.0.join("history.db")).unwrap();
    c.execute_batch("PRAGMA user_version=2; PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    drop(c);
    let before = fs::read(temp.0.join("history.db")).unwrap();
    assert!(matches!(
        Store::open(&temp.0, sha256),
        Err(Error::NewerSchema {
            found: 2,
            supported: VERSION
        })
    ));
    assert_eq!(before, fs::read(temp.0.join("history.db")).unwrap());
}
#[test]
fn rollback_after_formats_keeps_database_cache_and_observation_unchanged() {
    let temp = Temp::new();
    let store = temp.store();
    let at = Instant::now();
    store.inject_after_formats().unwrap();
    assert!(
        store
            .ingest(text("回滚全文"), event(1, at), false, at)
            .is_err()
    );
    assert_eq!(store.cache().total, 0);
    assert_eq!(store.cache().latest_sequence, None);
    let counts = check(&temp.0).unwrap();
    assert_eq!(
        (
            counts.contents,
            counts.formats,
            counts.events,
            counts.indexed
        ),
        (0, 0, 0, 0)
    );
    assert!(
        store
            .reader()
            .unwrap()
            .search("回滚全文")
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(ingest(&store, text("回滚全文"), 1, at).outcome, "saved");
    store.inject_after_formats().unwrap();
    assert!(
        store
            .ingest(rich("回滚全文"), event(1, at), false, at)
            .is_err()
    );
    assert_eq!(
        store
            .reader()
            .unwrap()
            .content(1)
            .unwrap()
            .unwrap()
            .formats
            .len(),
        1
    );
    assert_eq!(store.cache().items[0].formats.len(), 1);
    assert_eq!(check(&temp.0).unwrap().events, 1);
}
#[test]
fn dedup_survives_reopen_and_search_covers_beyond_cache() {
    let temp = Temp::new();
    let store = temp.store();
    let at = Instant::now();
    for i in 1..=305 {
        ingest(&store, text(&format!("历史记录-{i:04}")), i, at);
    }
    assert_eq!(store.cache().items.len(), 300);
    assert_eq!(store.cache().total, 305);
    assert_eq!(
        store.reader().unwrap().search("记录-0001").unwrap().items[0].id,
        1
    );
    drop(store);
    let store = temp.store();
    assert_eq!(
        ingest(&store, text("历史记录-0001"), 306, at).outcome,
        "duplicate"
    );
    assert_eq!(store.cache().total, 305);
    assert_eq!(store.cache().items[0].id, 1);
    assert_eq!(store.cache().items[0].copy_count, 2);
    assert_eq!(check(&temp.0).unwrap().events, 306);
}
#[test]
fn same_observation_late_merge_collision_and_index_remain_atomic() {
    for same in [true, false] {
        let temp = Temp::new();
        let store = temp.store();
        let at = Instant::now();
        ingest(&store, rich("中文全文原文"), 1, at);
        ingest(&store, text("中文全文原文"), 2, at);
        let before = store.cache().items[0].fingerprint;
        let result = ingest(&store, rich("中文全文原文"), if same { 2 } else { 3 }, at);
        assert_eq!(
            result.outcome,
            if same {
                "merged_same_observation"
            } else {
                "merged_late_formats"
            }
        );
        assert_eq!(store.cache().total, 1);
        assert_eq!(store.cache().items[0].copy_count, 2);
        assert_ne!(store.cache().items[0].fingerprint, before);
        let reader = store.reader().unwrap();
        assert_eq!(reader.search("全文原").unwrap().items.len(), 1);
        assert!(reader.content(1).unwrap().is_none());
        let counts = check(&temp.0).unwrap();
        assert_eq!(
            (
                counts.contents,
                counts.formats,
                counts.events,
                counts.indexed
            ),
            (1, 2, 2, 1)
        );
    }
}
#[test]
fn original_bytes_win_conflicts_and_search_updates_with_new_format() {
    let temp = Temp::new();
    let store = temp.store();
    let at = Instant::now();
    let html = Content::new(
        BTreeMap::from([(HTML.into(), Data::Bytes(b"html".as_slice().into()))]),
        sha256,
    )
    .unwrap();
    ingest(&store, html, 1, at);
    ingest(&store, text("新增可搜索正文"), 1, at);
    assert_eq!(
        store
            .reader()
            .unwrap()
            .search("可搜索")
            .unwrap()
            .items
            .len(),
        1
    );
    ingest(&store, text("不能覆盖旧文"), 1, at);
    let reader = store.reader().unwrap();
    assert_eq!(
        reader.content(1).unwrap().unwrap().text_value().unwrap(),
        "新增可搜索正文"
    );
    assert!(reader.search("不能覆盖").unwrap().items.is_empty());
    assert!(store.cache().items[0].partial);
    assert_eq!(check(&temp.0).unwrap().events, 1);
}
#[test]
fn search_unicode_literals_quotes_and_and_semantics() {
    let temp = Temp::new();
    let store = temp.store();
    let at = Instant::now();
    ingest(
        &store,
        text("订单尾号 AbC-123 https://Example.com/path?id=12345 100% a_b \\\"引号\" 😀 👩‍💻"),
        1,
        at,
    );
    let reader = store.reader().unwrap();
    for (query, mode) in [
        ("订", SearchMode::Substring),
        ("订单", SearchMode::Substring),
        ("订单尾", SearchMode::Index),
        ("aBc", SearchMode::Index),
        ("ExAmPle.COM", SearchMode::Index),
        ("?id=", SearchMode::Index),
        ("12345", SearchMode::Index),
        ("%", SearchMode::Substring),
        ("_", SearchMode::Substring),
        ("\\", SearchMode::Substring),
        ("😀", SearchMode::Substring),
        ("👩‍💻", SearchMode::Index),
        ("\"引号\"", SearchMode::Index),
        ("订 AbC-123", SearchMode::Mixed),
        ("订单尾号\nABC", SearchMode::Index),
    ] {
        let result = reader.search(query).unwrap();
        assert_eq!(result.mode, mode, "{query}");
        assert_eq!(result.items.len(), 1, "{query}");
    }
    assert!(reader.search("订单 不存在").unwrap().items.is_empty());
    assert!(reader.search("NEAR(a b)").unwrap().items.is_empty());
    assert_eq!(reader.search(" \t").unwrap().mode, SearchMode::Recent);
}
#[test]
fn file_lists_and_binary_roundtrip_and_file_names_are_searchable() {
    let temp = Temp::new();
    let store = temp.store();
    let at = Instant::now();
    let original = Content::new(
        BTreeMap::from([
            (
                FILES.into(),
                Data::Files(vec!["C:/资料/项目报告😀.txt".into(), "D:\\x\\A.txt".into()].into()),
            ),
            ("PNG".into(), Data::Bytes(vec![37; 1024 * 1024].into())),
        ]),
        sha256,
    )
    .unwrap();
    ingest(&store, original.clone(), 1, at);
    drop(store);
    let store = temp.store();
    let reader = store.reader().unwrap();
    let loaded = reader.content(1).unwrap().unwrap();
    assert_eq!(loaded.fingerprint, original.fingerprint);
    for (name, format) in &original.formats {
        assert!(loaded.formats[name].data == format.data);
    }
    assert_eq!(reader.search("项目报告").unwrap().items.len(), 1);
}
#[test]
fn late_merge_requires_same_owner_superset_and_one_second() {
    for case in 0..5 {
        let temp = Temp::new();
        let store = temp.store();
        let at = Instant::now();
        ingest(&store, text("原文"), 1, at);
        let mut event = event(
            2,
            at + Duration::from_millis(if case == 0 { 1001 } else { 1000 }),
        );
        if case == 1 {
            event.owner_pid = 8;
        }
        if case == 2 {
            event.owner_pid = 0;
        }
        let content = rich(if case == 3 { "新文" } else { "原文" });
        let result = store.ingest(content, event, false, at).unwrap();
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
fn converted_paste_checks_the_record_but_registers_the_written_text_as_echo() {
    let temp = Temp::new();
    let store = temp.store();
    let at = Instant::now();
    let original = text("原文");
    ingest(&store, original.clone(), 1, at);
    let written = text(r"C:\a.txt");
    assert!(
        !store
            .before_write(1, written.fingerprint, written.fingerprint, at)
            .unwrap()
    );
    assert!(
        store
            .before_write(1, original.fingerprint, written.fingerprint, at)
            .unwrap()
    );
    assert_eq!(
        store
            .ingest(written, event(2, at), false, at)
            .unwrap()
            .reason,
        Some("own_echo")
    );
}
#[test]
fn echo_expiry_stale_wrap_skips_and_reuse_preserve_events() {
    let temp = Temp::new();
    let store = temp.store();
    let at = Instant::now();
    let content = text("测试");
    ingest(&store, content.clone(), u32::MAX - 1, at);
    assert!(
        store
            .before_write(1, content.fingerprint, content.fingerprint, at)
            .unwrap()
    );
    assert!(
        !store
            .before_write(999, content.fingerprint, content.fingerprint, at)
            .unwrap()
    );
    assert_eq!(
        store
            .ingest(
                content.clone(),
                event(u32::MAX, at),
                false,
                at + Duration::from_millis(4999)
            )
            .unwrap()
            .reason,
        Some("own_echo")
    );
    assert_eq!(
        store
            .ingest(content, event(0, at), false, at + Duration::from_secs(5))
            .unwrap()
            .outcome,
        "duplicate"
    );
    assert_eq!(ingest(&store, text("旧"), u32::MAX, at).outcome, "stale");
    store.skipped_sequence(1).unwrap();
    assert_eq!(ingest(&store, rich("测试"), 2, at).outcome, "saved");
    store.reuse(1, 100).unwrap();
    assert_eq!(store.cache().items[0].id, 1);
    assert_eq!(check(&temp.0).unwrap().events, 3);
    drop(store);
    let store = temp.store();
    assert_eq!(store.cache().items[0].id, 1);
}
#[test]
fn snapshots_online_retention_restore_lock_and_corruption_rejection() {
    let temp = Temp::new();
    let store = temp.store();
    let at = Instant::now();
    ingest(&store, text("快照完整内容"), 1, at);
    assert!(snapshot_due(&temp.0).unwrap());
    let mut report = snapshot(&temp.0).unwrap();
    assert!(!snapshot_due(&temp.0).unwrap());
    assert!(matches!(restore(&temp.0, &report.file), Err(Error::Busy)));
    for _ in 0..7 {
        report = snapshot(&temp.0).unwrap();
    }
    assert_eq!(report.retained, 7);
    assert_eq!(report.deleted, 1);
    ingest(&store, text("快照后新增"), 2, at);
    drop(store);
    let guard = application_guard(&temp.0).unwrap();
    assert!(matches!(restore(&temp.0, &report.file), Err(Error::Busy)));
    drop(guard);
    let db = temp.0.join("history.db");
    let before = fs::read(&db).unwrap();
    let bad = temp.0.join("broken.db");
    fs::write(&bad, b"broken snapshot").unwrap();
    assert!(restore(&temp.0, &bad).is_err());
    assert_eq!(fs::read(&db).unwrap(), before);
    let backup = restore(&temp.0, &report.file).unwrap();
    assert_eq!(fs::read(backup).unwrap(), before);
    assert_eq!(check(&temp.0).unwrap().contents, 1);
}
#[test]
fn malformed_fts_is_rejected_without_modifying_original_database() {
    let temp = Temp::new();
    let store = temp.store();
    ingest(&store, text("全文索引校验"), 1, Instant::now());
    let report = snapshot(&temp.0).unwrap();
    drop(store);
    let c = writable(&report.file).unwrap();
    c.execute("INSERT INTO search_index(search_index,rowid,search_text) VALUES('delete',1,'全文索引校验')",[]).unwrap();
    drop(c);
    let before = fs::read(temp.0.join("history.db")).unwrap();
    assert!(restore(&temp.0, &report.file).is_err());
    assert_eq!(fs::read(temp.0.join("history.db")).unwrap(), before);
}
#[test]
fn portable_sha256_matches_standard_vectors() {
    for (bytes, expected) in [
        (
            b"".as_slice(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            b"abc".as_slice(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq".as_slice(),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
        ),
    ] {
        let actual: String = sha256(bytes)
            .unwrap()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(actual, expected);
    }
}

#[test]
fn equal_timestamps_reuse_and_new_copies_keep_committed_order_after_restart() {
    let temp = Temp::new();
    let store = temp.store();
    let at = Instant::now();
    for sequence in 1..=2 {
        let mut e = event(sequence, at);
        e.time_ms = 10;
        store
            .ingest(text(&sequence.to_string()), e, false, at)
            .unwrap();
    }
    store.reuse(1, 10).unwrap();
    assert_eq!(store.cache().items[0].id, 1);
    assert_eq!(store.reader().unwrap().search("").unwrap().items[0].id, 1);
    drop(store);
    let store = temp.store();
    assert_eq!(store.cache().items[0].id, 1);
    let mut e = event(3, at);
    e.time_ms = 10;
    store.ingest(text("2"), e, false, at).unwrap();
    assert_eq!(store.cache().items[0].id, 2);
    assert_eq!(check(&temp.0).unwrap().events, 3);
}

#[test]
fn samples_dedup_without_advancing_the_real_observation_and_readers_see_commits() {
    let temp = Temp::new();
    let store = temp.store();
    let at = Instant::now();
    for _ in 0..2 {
        for i in 0..50 {
            store
                .seed(text(&format!("示例内容-{i}")), event(0, at))
                .unwrap();
        }
    }
    assert_eq!(store.cache().total, 50);
    assert_eq!(store.latest_sequence(), None);
    let reader = store.reader().unwrap();
    let writer = store.clone();
    let thread = std::thread::spawn(move || {
        for i in 0..30 {
            ingest(&writer, text(&format!("并发提交-{i}")), i, at);
        }
    });
    let shot = snapshot(&temp.0).unwrap();
    while !thread.is_finished() {
        reader.search("并发提交").unwrap();
    }
    thread.join().unwrap();
    assert_eq!(reader.search("并发提交").unwrap().items.len(), 30);
    let counts = validate_snapshot(&shot.file).unwrap();
    assert!((50..=80).contains(&counts.contents));
    check(&temp.0).unwrap();
}
