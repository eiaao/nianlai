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
        source_path: r"C:\Apps\source\source.exe".into(),
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
    assert_eq!(store.report().version, VERSION);
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
    c.execute_batch(&format!(
        "PRAGMA user_version={}; PRAGMA wal_checkpoint(TRUNCATE);",
        VERSION + 1
    ))
    .unwrap();
    drop(c);
    let before = fs::read(temp.0.join("history.db")).unwrap();
    assert!(matches!(
        Store::open(&temp.0, sha256),
        Err(Error::NewerSchema {
            found,
            supported: VERSION
        }) if found == VERSION + 1
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
fn snapshot_reasons_listing_and_size_retention() {
    let temp = Temp::new();
    let store = temp.store();
    ingest(&store, text("快照列表"), 1, Instant::now());
    // 没有任何快照：先拍每日快照，不算升级。
    assert_eq!(
        snapshot_reason(&temp.0, "1.0").unwrap(),
        Some(Reason::Daily)
    );
    let first = snapshot_with(
        &temp.0,
        Reason::Daily,
        Some(Retention::default()),
        Some("1.0"),
    )
    .unwrap();
    assert_eq!(snapshot_reason(&temp.0, "1.0").unwrap(), None);
    assert_eq!(
        snapshot_reason(&temp.0, "1.1").unwrap(),
        Some(Reason::Upgrade)
    );
    snapshot_with(
        &temp.0,
        Reason::Upgrade,
        Some(Retention::default()),
        Some("1.1"),
    )
    .unwrap();
    assert_eq!(snapshot_reason(&temp.0, "1.1").unwrap(), None);
    // 不轮换的快照不删旧文件。
    let tight = Retention {
        keep: 1,
        max_bytes: u64::MAX,
    };
    let kept = snapshot_with(&temp.0, Reason::BeforeRestore, None, None).unwrap();
    assert_eq!((kept.retained, kept.deleted), (3, 0));
    let list = list_snapshots(&temp.0).unwrap();
    assert_eq!(
        list.iter().map(|s| s.reason).collect::<Vec<_>>(),
        [
            Some(Reason::BeforeRestore),
            Some(Reason::Upgrade),
            Some(Reason::Daily)
        ]
    );
    assert!(list.iter().all(|s| s.bytes > 0 && s.created_ms > 0));
    let name = first.file.file_name().unwrap().to_str().unwrap();
    assert_eq!(snapshot_file(&temp.0, name), Some(first.file.clone()));
    assert_eq!(snapshot_file(&temp.0, "../history.db"), None);
    // 总大小上限比一份还小：只留最新的一份。
    fs::write(temp.0.join("snapshots").join("history-x.pending"), b"half").unwrap();
    let report = snapshot_with(
        &temp.0,
        Reason::Manual,
        Some(Retention {
            keep: 7,
            max_bytes: 1,
        }),
        None,
    )
    .unwrap();
    assert_eq!((report.retained, report.deleted), (1, 3));
    assert_eq!(
        list_snapshots(&temp.0).unwrap()[0].reason,
        Some(Reason::Manual)
    );
    assert!(!temp.0.join("snapshots").join("history-x.pending").exists());
    let report = snapshot_with(&temp.0, Reason::Daily, Some(tight), None).unwrap();
    assert_eq!((report.retained, report.deleted), (1, 1));
    // 旧版本没有原因后缀的文件名照样列出。
    fs::copy(
        &report.file,
        temp.0
            .join("snapshots")
            .join("history-20200101-000000-000-1-0.db"),
    )
    .unwrap();
    assert_eq!(list_snapshots(&temp.0).unwrap().len(), 2);
    assert!(
        list_snapshots(&temp.0)
            .unwrap()
            .iter()
            .any(|s| s.reason.is_none())
    );
    validate_snapshot(&report.file).unwrap();
    drop(store);
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

// A12：去重合并后每一次复制事件都保留，且内容移到历史最前；同次观察后到的格式不新增事件。
#[test]
fn dedup_keeps_every_event_moves_content_to_front_and_reuse_keeps_order() {
    let temp = Temp::new();
    let store = temp.store();
    let at = Instant::now();
    for i in 1..=5 {
        ingest(&store, text(&format!("第{i}条")), i, at);
    }
    // 再复制第 1 条（duplicate）：追加事件、前移到最前，总记录数不变。
    let result = ingest(&store, text("第1条"), 6, at);
    assert_eq!(result.outcome, "duplicate");
    assert_eq!(store.cache().total, 5);
    assert_eq!(store.cache().items[0].preview, "第1条");
    assert_eq!(store.cache().items[0].copy_count, 2);
    // 粘贴复用（reuse）也前移，但不追加事件。
    store.reuse(3, 700).unwrap();
    assert_eq!(store.cache().items[0].preview, "第3条");
    assert_eq!(store.cache().items[0].copy_count, 1);
    // 再次复制第 3 条仍走 duplicate；随后补齐的富文本格式合并进原记录，不新增事件。
    assert_eq!(ingest(&store, text("第3条"), 7, at).outcome, "duplicate");
    assert_eq!(
        ingest(&store, rich("第3条"), 8, at).outcome,
        "merged_late_formats"
    );
    let counts = check(&temp.0).unwrap();
    assert_eq!(counts.events, 7);
    // 事件明细：每条内容的事件数与内容表一致。
    let c = writable(&temp.0.join("history.db")).unwrap();
    let mismatch: usize = c
        .query_row(
            "SELECT count(*) FROM contents WHERE copy_count!=(SELECT count(*) FROM copy_events WHERE content_id=contents.id)",
            [],
            |r| schema::number(r, 0),
        )
        .unwrap();
    assert_eq!(mismatch, 0);
}

// A06：来源应用行随事件写入并被列表引用；同进程同路径只建一行，最近一次复制决定列表来源。
#[test]
fn source_apps_upsert_with_events_and_lists_latest_source() {
    let temp = Temp::new();
    let store = temp.store();
    let at = Instant::now();
    let mut first = event(1, at);
    first.source_process = "app.exe".into();
    first.source_path = r"C:\Tools\app.exe".into();
    store.ingest(text("来自 app"), first, false, at).unwrap();
    let mut second = event(2, at);
    second.source_process = "app.exe".into();
    second.source_path = r"C:\Tools\app.exe".into();
    store
        .ingest(text("再次来自 app"), second, false, at)
        .unwrap();
    // 拿不到路径的进程只记进程名，是另一行（显示名退回进程名）。
    let mut third = event(3, at);
    third.source_process = "app.exe".into();
    third.source_path = String::new();
    store.ingest(text("无路径来源"), third, false, at).unwrap();
    let reader = store.reader().unwrap();
    assert_eq!(reader.counts().unwrap().source_apps, 2);
    let c = writable(&temp.0.join("history.db")).unwrap();
    let rows: Vec<(String, String, String)> = c
        .prepare("SELECT process_name,exe_path,display_name FROM source_apps ORDER BY exe_path")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            ("app.exe".into(), String::new(), "app.exe".into()),
            ("app.exe".into(), r"C:\Tools\app.exe".into(), "app".into()),
        ]
    );
    // 每条内容引用最近一次复制的事件来源：最新记录来自无路径事件，显示名退回进程名。
    assert_eq!(store.cache().items[0].preview, "无路径来源");
    assert_eq!(
        store.cache().items[0].source_name.as_deref(),
        Some("app.exe")
    );
    let missing: usize = c
        .query_row(
            "SELECT count(*) FROM copy_events WHERE source_app_id IS NULL",
            [],
            |r| schema::number(r, 0),
        )
        .unwrap();
    assert_eq!(missing, 0);
    check(&temp.0).unwrap();
}

// E01/B03：收藏的增删、顺序稳定；再次复制、搜索、粘贴复用都不改变收藏顺序。
#[test]
fn favorites_are_stable_ordered_and_survive_copies_search_and_reuse() {
    let temp = Temp::new();
    let store = temp.store();
    let at = Instant::now();
    for i in 1..=4 {
        ingest(&store, text(&format!("内容{i}")), i, at);
    }
    // 收藏 2、4、1（顺序即收藏顺序）。
    store.set_favorite(2, true).unwrap().unwrap();
    store.set_favorite(4, true).unwrap().unwrap();
    store.set_favorite(1, true).unwrap().unwrap();
    let order = |store: &Store| {
        store
            .reader()
            .unwrap()
            .favorites()
            .unwrap()
            .into_iter()
            .map(|item| item.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(order(&store), vec![2, 4, 1]);
    // 再次复制被收藏的内容、搜索、粘贴复用之后顺序不变。
    ingest(&store, text("内容1"), 5, at);
    store.reader().unwrap().search("内容").unwrap();
    store.reuse(4, 600).unwrap();
    assert_eq!(order(&store), vec![2, 4, 1]);
    // 列表项带着收藏状态。
    let listed: Vec<(usize, bool, Option<u64>)> = store
        .cache()
        .items
        .iter()
        .map(|item| (item.id, item.favorite, item.favorite_position))
        .collect();
    assert!(listed.contains(&(2, true, Some(1))));
    assert!(listed.contains(&(4, true, Some(2))));
    assert!(listed.contains(&(1, true, Some(3))));
    assert!(listed.contains(&(3, false, None)));
    // 取消收藏后从列表消失；再收藏排到最后（顺序稳定但不复活旧位置）。
    store.set_favorite(2, false).unwrap().unwrap();
    assert_eq!(order(&store), vec![4, 1]);
    store.set_favorite(2, true).unwrap().unwrap();
    assert_eq!(order(&store), vec![4, 1, 2]);
    // 收藏不存在的内容返回 None。
    assert!(store.set_favorite(999, true).unwrap().is_none());
    check(&temp.0).unwrap();
}

// B03：补齐合并撞上被收藏的重复内容时保留收藏行；外键兜底，被收藏的内容行不能直接物理删除。
#[test]
fn merges_keep_the_favorite_and_favorites_block_physical_delete() {
    let temp = Temp::new();
    let store = temp.store();
    let at = Instant::now();
    ingest(&store, rich("收藏后补齐合并"), 1, at);
    store.set_favorite(1, true).unwrap().unwrap();

    // 已有一条未收藏的纯文本记录；补齐成富文本后与“收藏后补齐合并”同指纹，必须并成一行。
    ingest(&store, text("收藏后补齐合并"), 2, at);
    assert_eq!(
        ingest(&store, rich("收藏后补齐合并"), 3, at).outcome,
        "merged_late_formats"
    );
    let counts = check(&temp.0).unwrap();
    assert_eq!(counts.contents, 1);
    assert_eq!(counts.favorites, 1);
    // 收藏仍在，且指向合并后保留的那一行。
    let favorites = store.reader().unwrap().favorites().unwrap();
    assert_eq!(favorites.len(), 1);
    assert_eq!(favorites[0].preview, "收藏后补齐合并");
    assert!(favorites[0].favorite);
    // 外键兜底：直接删被收藏的内容行会被数据库拒绝。
    let c = writable(&temp.0.join("history.db")).unwrap();
    c.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    let id: i64 = c
        .query_row("SELECT content_id FROM favorites", [], |r| r.get(0))
        .unwrap();
    assert!(c.execute("DELETE FROM contents WHERE id=?1", [id]).is_err());
}

// A06：后台刷新只改显示名和图标引用，不动事件与指纹。
#[test]
fn refresh_source_app_updates_display_and_icon_reference_only() {
    let temp = Temp::new();
    let store = temp.store();
    let at = Instant::now();
    let mut event = event(1, at);
    event.source_process = "app.exe".into();
    event.source_path = r"C:\Tools\app.exe".into();
    store
        .ingest(text("有来源的记录"), event, false, at)
        .unwrap();
    let app = store
        .reader()
        .unwrap()
        .source_app_by_path(r"C:\Tools\app.exe")
        .unwrap()
        .unwrap();
    store
        .refresh_source_app(
            app.id,
            "工具盒".into(),
            "abc123".into(),
            "abc123.png".into(),
        )
        .unwrap();
    let item = &store.cache().items[0];
    assert_eq!(item.source_name.as_deref(), Some("工具盒"));
    assert_eq!(item.icon_file.as_deref(), Some("abc123.png"));
    let counts = check(&temp.0).unwrap();
    assert_eq!((counts.events, counts.source_apps), (1, 1));
}

// 面板 Delete（B12）：先进回收站，列表、搜索、收藏里都看不到；能恢复回原位；再复制同样内容也会回来。
// 物理删除连带格式、复制事件、搜索索引和收藏行。
#[test]
fn delete_moves_to_trash_then_restore_revive_and_purge() {
    let temp = Temp::new();
    let store = temp.store();
    let at = Instant::now();
    for i in 1..=4 {
        ingest(&store, text(&format!("删除测试{i}")), i, at);
    }
    store.set_favorite(3, true).unwrap().unwrap();
    // 收藏的也能删（界面先确认），恢复后仍是收藏。
    assert!(store.delete(3).unwrap());
    assert!(store.delete(2).unwrap());
    assert!(!store.delete(2).unwrap());
    assert!(!store.delete(99).unwrap());
    let cache = store.cache();
    assert_eq!(cache.total, 2);
    assert!(cache.items.iter().all(|item| item.id != 2 && item.id != 3));
    let reader = store.reader().unwrap();
    assert!(reader.favorites().unwrap().is_empty());
    assert!(reader.content(2).unwrap().is_some(), "回收站里的内容还在");
    let found = |reader: &Reader| -> Vec<usize> {
        let mut ids: Vec<usize> = reader
            .search("删除测试")
            .unwrap()
            .items
            .into_iter()
            .map(|item| item.id)
            .collect();
        ids.sort_unstable();
        ids
    };
    assert_eq!(found(&reader), [1, 4]);
    assert_eq!(found(&reader), {
        let mut ids: Vec<usize> = reader.recent().unwrap().iter().map(|i| i.id).collect();
        ids.sort_unstable();
        ids
    });
    let trash = reader.trash().unwrap();
    assert_eq!(
        trash.iter().map(|(item, _)| item.id).collect::<Vec<_>>(),
        [2, 3]
    );
    assert!(trash.iter().all(|(_, at)| *at > 0));
    assert!(trash[1].0.favorite);

    // 恢复：回到原来的位置，收藏还在。
    let restored = store.restore(3).unwrap().unwrap();
    assert!(restored.favorite);
    assert!(store.restore(3).unwrap().is_none());
    assert_eq!(store.cache().total, 3);
    assert_eq!(
        store.cache().items.iter().map(|i| i.id).collect::<Vec<_>>(),
        [4, 3, 1]
    );
    // 回收站里的内容再被复制：回到列表，复制次数累加。
    let again = ingest(&store, text("删除测试2"), 5, at);
    assert_eq!((again.outcome, again.record_id), ("duplicate", Some(2)));
    assert_eq!(store.cache().total, 4);
    assert!(store.reader().unwrap().trash().unwrap().is_empty());

    // 物理删除：指定条目，或按删除时间清掉过期的；不在回收站里的不动。
    store.delete(3).unwrap();
    store.delete(1).unwrap();
    assert_eq!(store.purge(Some(vec![3, 4]), 0).unwrap(), 1);
    assert_eq!(store.purge(None, 0).unwrap(), 0, "删除时间不早于 0");
    assert_eq!(store.purge(None, u64::MAX).unwrap(), 1);
    let counts = check(&temp.0).unwrap();
    assert_eq!(
        (counts.contents, counts.trashed, counts.favorites),
        (2, 0, 0)
    );
    let c = writable(&temp.0.join("history.db")).unwrap();
    let orphans: usize = c
        .query_row(
            "SELECT (SELECT count(*) FROM copy_events WHERE content_id IN (1,3))+(SELECT count(*) FROM formats WHERE content_id IN (1,3))",
            [],
            |r| schema::number(r, 0),
        )
        .unwrap();
    assert_eq!(orphans, 0);
    assert_eq!(ingest(&store, text("删除测试3"), 6, at).outcome, "saved");
    drop(store);
    // 重新打开：列表总数不含回收站。
    store_reopen_total(&temp, 3);
}
fn store_reopen_total(temp: &Temp, expected: usize) {
    let store = temp.store();
    store.delete(store.cache().items[0].id).unwrap();
    drop(store);
    assert_eq!(temp.store().cache().total, expected - 1);
}

#[test]
fn storage_usage_file_lists_images_bulk_trash_and_compact() {
    let temp = Temp::new();
    let store = temp.store();
    let at = Instant::now();
    let files = |paths: &[&str]| {
        Content::new(
            BTreeMap::from([(
                FILES.into(),
                Data::Files(
                    paths
                        .iter()
                        .map(|p| p.to_string())
                        .collect::<Vec<_>>()
                        .into(),
                ),
            )]),
            sha256,
        )
        .unwrap()
    };
    let image = |fill: u8, size: usize| {
        Content::new(
            BTreeMap::from([("PNG".into(), Data::Bytes(vec![fill; size].into()))]),
            sha256,
        )
        .unwrap()
    };
    ingest(&store, text("普通文本"), 1, at);
    ingest(&store, files(&["C:\\a.txt", "C:\\b.txt"]), 2, at);
    ingest(&store, image(1, 300_000), 3, at);
    ingest(&store, image(2, 100_000), 4, at);
    ingest(&store, files(&["D:\\c.txt"]), 5, at);
    store.set_favorite(4, true).unwrap().unwrap();
    store.set_favorite(5, true).unwrap().unwrap();

    let reader = store.reader().unwrap();
    let lists = reader.file_lists().unwrap();
    assert_eq!(lists.len(), 2);
    assert_eq!(lists[0].id, 2);
    assert_eq!(lists[0].paths, ["C:\\a.txt", "C:\\b.txt"]);
    assert!(!lists[0].favorite && lists[1].favorite);

    let usage = reader.usage().unwrap();
    let image_usage = usage.kinds.iter().find(|k| k.kind == "图片").unwrap();
    assert_eq!(image_usage.count, 2);
    assert!(image_usage.bytes >= 400_000);
    assert_eq!(usage.trashed_count, 0);

    // 按大小从大到小，收藏的也在（界面上不让选）；时间筛选取不到未来以前的就为空。
    let images: Vec<usize> = reader.images(None).unwrap().iter().map(|i| i.id).collect();
    assert_eq!(images, [3, 4]);
    assert!(reader.images(Some(0)).unwrap().is_empty());

    // 批量清理跳过收藏、重复和不存在的。
    let moved = store.trash_many(vec![2, 3, 4, 5, 3, 99]).unwrap();
    assert_eq!(moved, [2, 3]);
    assert_eq!(store.cache().total, 3);
    let reader = store.reader().unwrap();
    assert_eq!(reader.file_lists().unwrap().len(), 1, "回收站里的不再检查");
    let usage = reader.usage().unwrap();
    assert_eq!(usage.trashed_count, 2);
    assert!(usage.trashed_bytes >= 300_000);

    // 彻底删除后空出来的页可以压缩回收。
    store.purge(None, u64::MAX).unwrap();
    assert!(store.reader().unwrap().usage().unwrap().reclaimable_bytes > 0);
    store.compact().unwrap();
    assert_eq!(
        store.reader().unwrap().usage().unwrap().reclaimable_bytes,
        0
    );
    assert_eq!(store.cache().total, 3);
}

// E03/E04：重排只在未删除收藏现有的槽位里置换；回收站里的收藏保持原位；出错不改动。
#[test]
fn reorder_favorites_permutes_live_slots_and_rejects_bad_input() {
    let temp = Temp::new();
    let store = temp.store();
    let at = Instant::now();
    for i in 1..=4 {
        ingest(&store, text(&format!("内容{i}")), i, at);
    }
    for id in [1, 2, 3, 4] {
        store.set_favorite(id, true).unwrap().unwrap();
    }
    // 2 进回收站，占着槽位 2。
    assert!(store.delete(2).unwrap());
    let positions = |store: &Store| {
        let tx = store.reader().unwrap();
        tx.favorites()
            .unwrap()
            .into_iter()
            .map(|item| (item.id, item.favorite_position.unwrap()))
            .collect::<Vec<_>>()
    };
    assert_eq!(positions(&store), vec![(1, 1), (3, 3), (4, 4)]);
    let items = store.reorder_favorites(&[4, 1, 3]).unwrap();
    assert_eq!(
        items
            .iter()
            .map(|i| (i.id, i.favorite_position.unwrap()))
            .collect::<Vec<_>>(),
        vec![(4, 1), (1, 3), (3, 4)]
    );
    assert_eq!(positions(&store), vec![(4, 1), (1, 3), (3, 4)]);
    // 恢复后 2 仍在槽位 2。
    store.restore(2).unwrap();
    assert_eq!(
        store
            .reader()
            .unwrap()
            .favorites()
            .unwrap()
            .iter()
            .map(|i| i.id)
            .collect::<Vec<_>>(),
        vec![4, 2, 1, 3]
    );
    // 缓存里的收藏顺序也同步。
    assert!(
        store
            .cache()
            .items
            .iter()
            .any(|i| i.id == 4 && i.favorite_position == Some(1))
    );
    // 错误：重复、非收藏、缺项、多项；都不改变任何东西。
    let before = positions(&store);
    for bad in [
        vec![4, 4, 1, 3],
        vec![4, 2, 1, 99],
        vec![4, 2, 1],
        vec![4, 2, 1, 3, 3],
        vec![],
    ] {
        assert!(store.reorder_favorites(&bad).is_err(), "{bad:?}");
        assert_eq!(positions(&store), before);
    }
    check(&temp.0).unwrap();
}

// E03/E04：标题与备注的清理、上限、非收藏报错，以及 C09 搜索覆盖标题 / 备注。
#[test]
fn favorite_labels_are_cleaned_limited_and_searchable() {
    let temp = Temp::new();
    let store = temp.store();
    let at = Instant::now();
    ingest(&store, text("alpha 正文"), 1, at);
    ingest(&store, text("beta 正文"), 2, at);
    ingest(&store, text("gamma 正文"), 3, at);
    store.set_favorite(1, true).unwrap().unwrap();
    store.set_favorite(2, true).unwrap().unwrap();
    // 不是收藏：报错。
    assert!(store.set_favorite_label(3, "x", "y").is_err());
    let item = store
        .set_favorite_label(1, "  第一行\r\n第二行  ", "\n 备注一\n备注二 \n")
        .unwrap();
    assert_eq!(item.title, "第一行  第二行");
    assert_eq!(item.note, "备注一\n备注二");
    // 缓存同步，且 favorites() 带出标题与备注。
    let cached = store.cache().items.iter().find(|i| i.id == 1).cloned();
    assert_eq!(cached.unwrap().title, "第一行  第二行");
    assert_eq!(
        store.reader().unwrap().favorites().unwrap()[0].note,
        "备注一\n备注二"
    );
    // 上限按字符数。
    let item = store
        .set_favorite_label(1, &"题".repeat(300), &"注".repeat(3000))
        .unwrap();
    assert_eq!(item.title.chars().count(), 200);
    assert_eq!(item.note.chars().count(), 2000);
    // 搜索：标题 / 备注命中，且大小写不敏感。
    store
        .set_favorite_label(1, "Invoice 发票", "needle 备忘")
        .unwrap();
    store.set_favorite_label(2, "", "").unwrap();
    let reader = store.reader().unwrap();
    let ids = |q: &str| {
        reader
            .search(q)
            .unwrap()
            .items
            .into_iter()
            .map(|i| i.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids("invoice"), vec![1]); // 长词，只在标题里
    assert_eq!(ids("NEEDLE"), vec![1]); // 长词，只在备注里
    assert_eq!(ids("发票"), vec![1]); // 短词
    assert_eq!(ids("备忘 alpha"), vec![1]); // 短词（标题）+ 长词（正文）
    assert_eq!(ids("invoice 正文"), vec![1]); // 标题命中 + 正文命中
    assert_eq!(ids("invoice beta"), Vec::<usize>::new());
    // 取消收藏后旧标签不再命中。
    store.set_favorite(1, false).unwrap().unwrap();
    let reader = store.reader().unwrap();
    assert!(reader.search("invoice").unwrap().items.is_empty());
    assert!(reader.search("发票").unwrap().items.is_empty());
    assert_eq!(reader.search("alpha").unwrap().items.len(), 1);
    check(&temp.0).unwrap();
}
