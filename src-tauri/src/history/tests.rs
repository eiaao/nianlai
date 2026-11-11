//! 内存模式的历史服务：库打不开（或完整性检查失败）时，历史服务用同一个 `Store` 的内存库。
//! 这里验证“内存库上的历史服务”该有的行为；存储层本身的行为在 nianlai-store 的测试里。
use super::*;
use crate::{
    platform::hash::sha256,
    settings::{Directories, tests::Directory},
};
use content::{Data, HTML, TEXT};
use nianlai_store::MemoryOptions;
use std::{collections::BTreeMap, fs};

fn directories(directory: &Directory) -> Directories {
    Directories {
        settings_file: directory.0.join("settings.json"),
        data: directory.0.join("data"),
        logs: directory.0.join("logs"),
        data_source: "设置文件指定",
        test_data_dir: false,
    }
}
// 没调用 initialize：历史服务停在内存库上。
fn memory_history(directory: &Directory) -> History {
    let logger = Logger::with_settings(&directories(directory), 20).unwrap();
    History::new(sha256, logger)
}
// 换一个带自定义上限的内存库。
fn with_limits(history: &History, options: MemoryOptions) {
    *history.store.write().unwrap() = Store::open_memory(sha256, options).unwrap();
}
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
fn save(history: &History, content: Arc<Content>, sequence: u32, at: Instant) -> Ingest {
    history
        .ingest(content, event(sequence, at), false, at)
        .unwrap()
}
fn id(ingest: &Ingest) -> usize {
    ingest.record_id.unwrap()
}
fn ids(history: &History) -> Vec<usize> {
    history.snapshot().items.iter().map(|i| i.id).collect()
}

#[test]
fn memory_mode_is_a_memory_store_and_reports_loading_until_opened() {
    let directory = Directory::new();
    let history = memory_history(&directory);
    assert!(history.db().is_memory());
    assert!(history.store().is_none());
    assert!(history.store_directory().is_none());
    assert_eq!(history.snapshot().store_status, Some("正在加载历史"));
    // 编号和磁盘库错开，界面里残留的旧请求取不到新内容。
    let first = save(&history, content(&[(TEXT, b"a")]), 1, Instant::now());
    assert!(id(&first) > 1 << 52);
    // 内存模式不往数据目录写任何东西。
    assert!(!directories(&directory).data.exists());
}

#[test]
fn all_ingest_outcomes_preserve_copy_events() {
    let directory = Directory::new();
    let history = memory_history(&directory);
    let at = Instant::now();
    let text = content(&[(TEXT, b"a")]);
    let rich = content(&[(HTML, b"<b>a</b>"), (TEXT, b"a")]);
    assert_eq!(save(&history, text.clone(), 10, at).outcome, "saved");
    assert_eq!(
        save(&history, text.clone(), 10, at).outcome,
        "merged_same_observation"
    );
    assert_eq!(
        save(&history, rich.clone(), 11, at).outcome,
        "merged_late_formats"
    );
    assert_eq!(save(&history, rich.clone(), 12, at).outcome, "duplicate");
    assert_eq!(save(&history, text, 9, at).outcome, "stale");
    let snapshot = history.snapshot();
    assert_eq!(snapshot.total, 1);
    assert_eq!(snapshot.items[0].copy_count, 2);
    let stored = history.get(snapshot.items[0].id).unwrap().unwrap();
    assert_eq!(stored.formats.len(), 2);
}

#[test]
fn same_observation_supplements_without_overwriting_original() {
    let directory = Directory::new();
    let history = memory_history(&directory);
    let at = Instant::now();
    let first = history
        .ingest(content(&[(TEXT, b"a")]), event(1, at), true, at)
        .unwrap();
    let result = save(&history, content(&[(TEXT, b"b"), (HTML, b"html")]), 1, at);
    assert_eq!(result.outcome, "merged_same_observation");
    let stored = history.get(id(&first)).unwrap().unwrap();
    assert_eq!(stored.raw(TEXT), Some(b"a".as_slice()));
    assert_eq!(history.snapshot().items[0].copy_count, 1);
}

#[test]
fn late_merge_requires_owner_time_superset_and_equal_bytes() {
    let at = Instant::now();
    for case in 0..5 {
        let directory = Directory::new();
        let history = memory_history(&directory);
        save(&history, content(&[(TEXT, b"a")]), 1, at);
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
    let directory = Directory::new();
    let history = memory_history(&directory);
    let at = Instant::now();
    let text = content(&[(TEXT, b"a")]);
    let first = id(&save(&history, text.clone(), 1, at));
    assert!(history.before_write(first, text.fingerprint, text.fingerprint, at));
    assert!(!history.before_write(first, [0; 32], text.fingerprint, at));
    let echoed = history
        .ingest(
            text.clone(),
            event(2, at),
            false,
            at + Duration::from_millis(4999),
        )
        .unwrap();
    assert_eq!(echoed.reason, Some("own_echo"));
    let after = history
        .ingest(text, event(3, at), false, at + Duration::from_secs(5))
        .unwrap();
    assert_eq!(after.outcome, "duplicate");
    history.reuse(first);
    assert_eq!(history.snapshot().items[0].copy_count, 2);
    assert!(!history.before_write(999, [0; 32], [0; 32], at));
}

#[test]
fn capacity_evicts_oldest_and_late_merge_still_follows_the_last_observation() {
    let directory = Directory::new();
    let history = memory_history(&directory);
    // 每条文字是 UTF-16：一个字符 2 字节。上限 2 条、6 字节。
    with_limits(
        &history,
        MemoryOptions {
            max_records: 2,
            max_bytes: 6,
            ..MemoryOptions::unbounded()
        },
    );
    let at = Instant::now();
    let utf16 =
        |text: &str| -> Vec<u8> { text.encode_utf16().flat_map(u16::to_le_bytes).collect() };
    let text = |value: &str| content(&[(TEXT, &utf16(value))]);
    let a = id(&save(&history, text("a"), 1, at));
    let b = id(&save(&history, text("b"), 2, at));
    // 复用不改变“上一次观察”：随后到的补全格式仍并进 b。
    history.reuse(a);
    let merged = save(
        &history,
        content(&[(TEXT, &utf16("b")), (HTML, b"c")]),
        3,
        at,
    );
    assert_eq!(
        (merged.outcome, merged.record_id),
        ("merged_late_formats", Some(b))
    );
    assert_eq!(history.snapshot().total, 2);
    // 第三条进来：条数超了，最旧的（按时间）被淘汰。
    let c = id(&save(&history, text("c"), 4, at));
    assert_eq!(history.snapshot().total, 2);
    assert!(history.get(c).unwrap().is_some());
    let kept = [a, b]
        .into_iter()
        .filter(|id| history.get(*id).unwrap().is_some())
        .count();
    assert_eq!(kept, 1);
    // 字节数超限同样淘汰：一条 8 字节的单条超过 6 字节上限，直接不记。
    let big = history
        .ingest(text("abcd"), event(5, at), false, at)
        .unwrap();
    assert_eq!((big.outcome, big.reason), ("skipped", Some("capacity")));
    assert!(history.get(c).unwrap().is_some());
}

#[test]
fn serial_wrap_and_skipped_observations_are_ordered() {
    assert!(older(u32::MAX, 1));
    assert!(!older(1, u32::MAX));
    let directory = Directory::new();
    let history = memory_history(&directory);
    let at = Instant::now();
    save(&history, content(&[(TEXT, b"a")]), u32::MAX, at);
    history.skipped_sequence(0);
    assert_eq!(
        save(&history, content(&[(TEXT, b"a"), (HTML, b"html")]), 1, at).outcome,
        "saved"
    );
}

#[test]
fn preview_is_bounded_and_sample_seeding_does_not_advance_real_sequence() {
    let text = Content::text(&"😀".repeat(400), sha256).unwrap();
    assert_eq!(text.preview.chars().count(), 300);
    let directory = Directory::new();
    let history = memory_history(&directory);
    for sample in crate::samples::all().into_iter().rev() {
        history
            .db()
            .seed(
                Content::text(&sample.text, sha256).unwrap(),
                CopyEvent {
                    time_ms: now_ms(),
                    sequence: 0,
                    source_process: "示例".into(),
                    source_path: String::new(),
                    owner_process: "示例".into(),
                    owner_pid: 0,
                    observed: Instant::now(),
                },
            )
            .unwrap();
    }
    assert_eq!(history.snapshot().total, 50);
    assert_eq!(history.latest_sequence(), None);
    let at = Instant::now();
    assert_eq!(
        save(&history, content(&[(TEXT, b"real")]), 0, at).outcome,
        "saved"
    );
}

#[test]
fn merging_into_existing_content_preserves_both_copy_events() {
    let directory = Directory::new();
    let history = memory_history(&directory);
    let at = Instant::now();
    let rich = content(&[(TEXT, b"a"), (HTML, b"html")]);
    save(&history, rich.clone(), 1, at);
    history
        .ingest(content(&[(TEXT, b"a")]), event(2, at), true, at)
        .unwrap();
    save(&history, rich, 3, at);
    let snapshot = history.snapshot();
    assert_eq!(snapshot.total, 1);
    assert_eq!(snapshot.items[0].copy_count, 2);
}

#[test]
fn oversized_record_does_not_flush_existing_history() {
    let directory = Directory::new();
    let history = memory_history(&directory);
    with_limits(
        &history,
        MemoryOptions {
            max_bytes: 1,
            ..MemoryOptions::unbounded()
        },
    );
    let at = Instant::now();
    let kept = id(&save(&history, content(&[(TEXT, b"a")]), 1, at));
    assert_eq!(
        save(&history, content(&[(TEXT, b"ab")]), 2, at).reason,
        Some("capacity")
    );
    assert!(history.get(kept).unwrap().is_some());
}

#[test]
fn default_count_limit_and_frontend_preview_limit_are_enforced() {
    let directory = Directory::new();
    let history = memory_history(&directory);
    let at = Instant::now();
    let mut first = None;
    let mut last = 0;
    for sequence in 1..=1001 {
        let bytes = sequence.to_string();
        last = id(&save(
            &history,
            content(&[(TEXT, bytes.as_bytes())]),
            sequence,
            at,
        ));
        first.get_or_insert(last);
    }
    assert!(history.get(first.unwrap()).unwrap().is_none());
    let snapshot = history.snapshot();
    assert_eq!(snapshot.total, 300);
    assert_eq!(snapshot.items.len(), 300);
    assert_eq!(snapshot.items[0].id, last);
}

// E01：收藏接口与磁盘模式同名同义；顺序稳定、容量淘汰不清理收藏（B03）。
#[test]
fn favorites_are_ordered_stable_and_survive_eviction() {
    let directory = Directory::new();
    let history = memory_history(&directory);
    with_limits(
        &history,
        MemoryOptions {
            max_records: 3,
            ..MemoryOptions::unbounded()
        },
    );
    let at = Instant::now();
    let saved: Vec<usize> = (1..=3u32)
        .map(|sequence| {
            id(&save(
                &history,
                content(&[(TEXT, sequence.to_string().as_bytes())]),
                sequence,
                at,
            ))
        })
        .collect();
    let (one, two, three) = (saved[0], saved[1], saved[2]);
    history.set_favorite(one, true).unwrap();
    history.set_favorite(three, true).unwrap();
    let order = |history: &History| {
        history
            .favorites()
            .unwrap()
            .into_iter()
            .map(|item| item.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(order(&history), vec![one, three]);
    // 再次复制与复用不改变收藏顺序（愉悦标准：可预期）。
    save(&history, content(&[(TEXT, b"1")]), 4, at);
    history.db().reuse(three, now_ms()).unwrap();
    assert_eq!(order(&history), vec![one, three]);
    // 容量淘汰只动未收藏的记录。
    save(&history, content(&[(TEXT, b"5")]), 5, at);
    assert!(history.get(two).unwrap().is_none());
    assert!(history.get(one).unwrap().is_some());
    assert_eq!(order(&history), vec![one, three]);
    // 取消后从收藏列表消失；再收藏排到最后。
    history.set_favorite(one, false).unwrap();
    assert_eq!(order(&history), vec![three]);
    history.set_favorite(one, true).unwrap();
    assert_eq!(order(&history), vec![three, one]);
    // 不存在的记录返回 None。
    assert!(history.set_favorite(999, true).unwrap().is_none());
    // 快照项带收藏字段与来源显示名（无路径来源退回进程名原样）。
    let listed = history.snapshot();
    let favorited: Vec<_> = listed
        .items
        .iter()
        .filter(|item| item.favorite)
        .map(|item| (item.id, item.favorite_position))
        .collect();
    assert_eq!(favorited, vec![(three, Some(2)), (one, Some(3))]);
    let first = listed.items.iter().find(|item| item.id == one).unwrap();
    assert_eq!(first.source_name.as_deref(), Some("source.exe"));
    assert_eq!(first.source_icon, None);
}

// A06：内存模式记来源路径；显示名退回文件名去扩展名，无路径时用进程名。
#[test]
fn source_names_fall_back_to_file_stem_then_process() {
    let directory = Directory::new();
    let history = memory_history(&directory);
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

// E03/E04：重排只在未删除收藏的槽位里置换，回收站里的收藏保持原位；出错不改动。
#[test]
fn reorder_keeps_trashed_slot_and_rejects_bad_input() {
    let directory = Directory::new();
    let history = memory_history(&directory);
    let at = Instant::now();
    let saved: Vec<usize> = (1..=4u32)
        .map(|i| {
            id(&save(
                &history,
                content(&[(TEXT, i.to_string().as_bytes())]),
                i,
                at,
            ))
        })
        .collect();
    let (a, b, c, d) = (saved[0], saved[1], saved[2], saved[3]);
    for id in &saved {
        history.set_favorite(*id, true).unwrap();
    }
    assert!(history.delete(b).unwrap());
    let slots = |history: &History| {
        history
            .favorites()
            .unwrap()
            .into_iter()
            .map(|item| (item.id, item.favorite_position.unwrap()))
            .collect::<Vec<_>>()
    };
    assert_eq!(slots(&history), vec![(a, 1), (c, 3), (d, 4)]);
    let items = history.reorder_favorites(&[d, a, c]).unwrap();
    assert_eq!(
        items
            .iter()
            .map(|i| (i.id, i.favorite_position.unwrap()))
            .collect::<Vec<_>>(),
        vec![(d, 1), (a, 3), (c, 4)]
    );
    // 恢复后被删的收藏仍在槽位 2。
    history.restore(b).unwrap();
    assert_eq!(
        history
            .favorites()
            .unwrap()
            .into_iter()
            .map(|i| i.id)
            .collect::<Vec<_>>(),
        vec![d, b, a, c]
    );
    let before = slots(&history);
    for bad in [vec![d, d, a, c], vec![d, b, a, 99], vec![d, b, a], vec![]] {
        assert!(history.reorder_favorites(&bad).is_err());
        assert_eq!(slots(&history), before);
    }
}

// E04/C09：标题备注的清理、上限、取消收藏后清空，以及搜索命中。
#[test]
fn labels_are_cleaned_limited_cleared_and_searchable() {
    let directory = Directory::new();
    let history = memory_history(&directory);
    let at = Instant::now();
    let item = id(&history
        .ingest(
            Content::text("body", sha256).unwrap(),
            event(1, at),
            false,
            at,
        )
        .unwrap());
    let shared = &history;
    assert!(shared.set_favorite_label(item, "x", "y").is_err());
    shared.set_favorite(item, true).unwrap();
    let labeled = shared
        .set_favorite_label(item, " Alpha\r\nBeta ", "\n line1\nline2 \n")
        .unwrap();
    assert_eq!(labeled.title, "Alpha  Beta");
    assert_eq!(labeled.note, "line1\nline2");
    let long = shared
        .set_favorite_label(item, &"题".repeat(300), &"注".repeat(3000))
        .unwrap();
    assert_eq!(long.title.chars().count(), 200);
    assert_eq!(long.note.chars().count(), 2000);
    shared.set_favorite_label(item, "Needle", "").unwrap();
    let found = |query: &str| shared.search(query, 1).unwrap().items.len();
    assert_eq!(found("needle"), 1);
    assert_eq!(found("needle body"), 1);
    assert_eq!(found("needle nothing"), 0);
    shared.set_favorite(item, false).unwrap();
    assert_eq!(found("needle"), 0);
}

#[test]
fn trash_delete_restore_and_purge_work_on_the_memory_store() {
    let directory = Directory::new();
    let history = memory_history(&directory);
    let at = Instant::now();
    let a = id(&save(&history, content(&[(TEXT, b"a")]), 1, at));
    let b = id(&save(&history, content(&[(TEXT, b"b")]), 2, at));
    assert!(history.delete(a).unwrap());
    assert_eq!(ids(&history), [b]);
    assert_eq!(history.snapshot().total, 1);
    assert_eq!(history.trash().unwrap()[0].item.id, a);
    assert_eq!(history.undo_delete().unwrap().unwrap().id, a);
    assert_eq!(history.snapshot().total, 2);
    assert!(history.delete(a).unwrap());
    assert_eq!(history.purge(None).unwrap(), 1);
    assert!(history.get(a).unwrap().is_none());
}

#[test]
fn search_log_in_memory_mode_keeps_the_memory_substring_label() {
    let directory = Directory::new();
    let directories = directories(&directory);
    let logger = Logger::with_settings(&directories, 20).unwrap();
    let history = History::new(sha256, logger.clone());
    save(
        &history,
        Content::text("日志模式哨兵", sha256).unwrap(),
        1,
        Instant::now(),
    );
    history.search("哨兵", 1).unwrap();
    logger.flush();
    let text = fs::read_dir(&directories.logs)
        .unwrap()
        .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect::<String>();
    let row: serde_json::Value = text
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|row| row["event"] == "search")
        .unwrap();
    assert_eq!(row["mode"], "memory_substring");
    assert_eq!(row["results"], 1);
}

// 库打不开（数据目录建不出来）：退到内存模式，提示和状态与之前一致，功能照常，磁盘功能拒绝。
#[test]
fn unopenable_store_falls_back_to_memory_with_the_same_status_and_refusals() {
    let directory = Directory::new();
    let mut directories = directories(&directory);
    // 数据目录的位置上是一个普通文件：建不出目录，也就开不了库。
    directories.data = directory.0.join("blocked");
    fs::write(&directories.data, b"not a directory").unwrap();
    let logger = Logger::with_settings(&directories, 20).unwrap();
    let history = History::new(sha256, logger);
    history.initialize();
    assert!(history.store().is_none());
    assert!(history.db().is_memory());
    assert_eq!(history.snapshot().store_status, Some("历史库异常，未落盘"));
    // 复制、列表、搜索、收藏照常。
    let at = Instant::now();
    let saved = id(&save(
        &history,
        Content::text("退到内存-6603", sha256).unwrap(),
        1,
        at,
    ));
    assert_eq!(ids(&history), [saved]);
    assert_eq!(history.search("退到内存-6603", 1).unwrap().items.len(), 1);
    assert!(history.set_favorite(saved, true).unwrap().unwrap().favorite);
    // 会碰磁盘的功能照旧拒绝，文案不变。
    assert_eq!(
        history.storage_overview().err().as_deref(),
        Some("历史库没有打开，存储管理暂时不可用")
    );
    assert_eq!(
        history.backup_summary().err().as_deref(),
        Some("历史库没有打开，备份与导入暂时不可用")
    );
    assert!(history.store_directory().is_none());
    // 数据目录里没有被写进任何东西。
    assert_eq!(fs::read(&directories.data).unwrap(), b"not a directory");
}

// 完整性检查失败：换成全新的内存库，旧库文件原样留在磁盘上。
#[test]
fn failed_integrity_check_switches_to_a_fresh_memory_store_and_leaves_the_file() {
    let directory = Directory::new();
    let directories = directories(&directory);
    let logger = Logger::with_settings(&directories, 20).unwrap();
    let history = History::new(sha256, logger);
    history.initialize();
    let at = Instant::now();
    save(
        &history,
        Content::text("落盘的一条", sha256).unwrap(),
        1,
        at,
    );
    assert!(history.store().is_some());
    assert_eq!(history.snapshot().store_status, None);
    let database = directories.data.join("history.db");
    let wal = directories.data.join("history.db-wal");
    assert!(database.is_file());
    // 库还开着，写入在 -wal 里；换下旧库不能让 SQLite 关闭连接（那会做检查点、改写库文件、删掉 -wal）。
    let database_before = fs::read(&database).unwrap();
    let wal_before = fs::read(&wal).expect("库开着时 -wal 存在");
    assert!(!wal_before.is_empty());
    history.fall_back_to_memory("历史库异常，未落盘");
    assert_eq!(
        fs::read(&database).unwrap(),
        database_before,
        "库文件被改动"
    );
    assert_eq!(
        fs::read(&wal).ok().as_deref(),
        Some(wal_before.as_slice()),
        "-wal 被改动或删除"
    );
    assert!(history.store().is_none());
    assert!(history.store_directory().is_none());
    assert_eq!(history.snapshot().store_status, Some("历史库异常，未落盘"));
    assert!(history.snapshot().items.is_empty());
    let fresh = id(&save(
        &history,
        Content::text("内存里的一条", sha256).unwrap(),
        2,
        at,
    ));
    assert!(fresh > 1 << 52);
    assert!(database.is_file());
    assert_eq!(fs::read(&database).unwrap(), database_before);
    assert_eq!(fs::read(&wal).ok().as_deref(), Some(wal_before.as_slice()));
    assert_eq!(history.retired.lock().unwrap().len(), 1);
}

#[test]
fn storage_errors_become_plain_chinese_messages() {
    use super::user_message;
    use nianlai_store::Error;
    use std::io::ErrorKind;
    let busy = "数据库正忙，请稍后重试";
    let full = "磁盘空间不足，请清理磁盘后重试";
    let readonly = "数据文件是只读的或没有权限，请检查数据目录的权限";
    let corrupt = "历史库文件已损坏，可以从快照恢复";
    let missing = "找不到数据文件，可能已被移动或删除";
    let cannot_open = "找不到或无法打开数据文件，可能已被移动、删除或被占用";
    let moved = "数据文件已被移动或删除，请重启拈来";
    for (error, expected) in [
        (Error::Busy, busy),
        (Error::Sqlite(5), busy),
        (Error::Sqlite(6), busy),
        (Error::Sqlite(261), busy),
        (Error::Sqlite(13), full),
        (Error::Io(ErrorKind::StorageFull), full),
        (Error::Sqlite(8), readonly),
        (Error::Sqlite(1032), moved),
        (Error::Sqlite(3), readonly),
        (Error::Io(ErrorKind::PermissionDenied), readonly),
        (Error::Sqlite(11), corrupt),
        (Error::Sqlite(26), corrupt),
        (Error::Sqlite(14), cannot_open),
        (Error::Sqlite(270), cannot_open),
        (Error::Io(ErrorKind::NotFound), missing),
        (
            Error::NewerSchema {
                found: 9,
                supported: 3,
            },
            "历史库来自更新版本的拈来，请先升级拈来",
        ),
        (Error::Sqlite(1), "操作失败，请稍后重试（Sqlite 1）"),
        (Error::Sqlite(-1), "操作失败，请稍后重试（Sqlite -1）"),
        (
            Error::Io(ErrorKind::TimedOut),
            "操作失败，请稍后重试（Io TimedOut）",
        ),
        (Error::Invalid("x"), "操作失败，请稍后重试（Invalid x）"),
        (Error::Closed, "操作失败，请稍后重试（Closed）"),
    ] {
        assert_eq!(user_message(&error), expected, "{error:?}");
    }
}

// 测量用：4K 截图（PNG + DIB）连续预览两次的耗时。
// cargo test --release image_preview_timing -- --ignored --nocapture
#[test]
#[ignore = "测量用"]
fn image_preview_timing() {
    use content::{DIB, PNG};
    let (w, h) = (3840usize, 2160usize);
    // 带噪点的渐变，PNG 压不到很小，接近真实截图。
    let mut seed = 1u32;
    let mut row = vec![0u8; w * 3];
    let mut dib = Vec::new();
    dib.extend_from_slice(&40u32.to_le_bytes());
    dib.extend_from_slice(&(w as i32).to_le_bytes());
    dib.extend_from_slice(&(h as i32).to_le_bytes());
    dib.extend_from_slice(&1u16.to_le_bytes());
    dib.extend_from_slice(&24u16.to_le_bytes());
    dib.resize(40, 0);
    for y in 0..h {
        for x in 0..w {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let n = (seed >> 28) as u8;
            row[x * 3] = ((x / 16) as u8).wrapping_add(n);
            row[x * 3 + 1] = ((y / 8) as u8).wrapping_add(n);
            row[x * 3 + 2] = (((x + y) / 24) as u8).wrapping_add(n);
        }
        dib.extend_from_slice(&row);
    }
    let dib_content = content(&[(DIB, &dib)]);
    let png = crate::platform::image::render(&dib_content, u32::MAX)
        .unwrap()
        .png;
    println!("dib {} MB, png {} MB", dib.len() >> 20, png.len() >> 20);
    let both = content(&[(DIB, &dib), (PNG, &png)]);
    let directory = Directory::new();
    let history = memory_history(&directory);
    let disk = directories(&directory).data;
    fs::create_dir_all(&disk).unwrap();
    *history.store.write().unwrap() = Store::open(&disk, sha256).unwrap();
    let at = Instant::now();
    let record = id(&save(&history, both, 1, at));
    let ms = |t: Instant| t.elapsed().as_secs_f64() * 1000.0;
    for round in 1..=3 {
        let t = Instant::now();
        let got = history.get(record).unwrap().unwrap();
        println!(
            "#{round} get (read all formats + verify hash): {:.1} ms",
            ms(t)
        );
        drop(got);
        let t = Instant::now();
        history.image(record, Some(480)).unwrap().unwrap();
        println!("#{round} image(480) total: {:.1} ms", ms(t));
    }
    let t = Instant::now();
    history.image(record, None).unwrap().unwrap();
    println!("image(full) total: {:.1} ms", ms(t));
}

#[test]
fn thumbnails_are_cached_per_id_and_size_and_dropped_when_content_is_purged() {
    use content::DIB;
    let mut dib = Vec::new();
    dib.extend_from_slice(&40u32.to_le_bytes());
    dib.extend_from_slice(&300i32.to_le_bytes());
    dib.extend_from_slice(&100i32.to_le_bytes());
    dib.extend_from_slice(&1u16.to_le_bytes());
    dib.extend_from_slice(&24u16.to_le_bytes());
    dib.resize(40, 0);
    dib.extend(std::iter::repeat_n(120u8, 900 * 100));
    let directory = Directory::new();
    let history = memory_history(&directory);
    let record = id(&save(&history, content(&[(DIB, &dib)]), 1, Instant::now()));
    let first = history.image(record, Some(120)).unwrap().unwrap();
    assert_eq!(history.images.lock().unwrap().0.len(), 1);
    let again = history.image(record, Some(120)).unwrap().unwrap();
    assert_eq!((first.url.as_str(), first.width), (again.url.as_str(), 300));
    // 另一个尺寸另算；原尺寸不进缓存。
    history.image(record, Some(60)).unwrap().unwrap();
    history.image(record, None).unwrap().unwrap();
    assert_eq!(history.images.lock().unwrap().0.len(), 2);
    // 清除内容后取不到，缓存也空了。
    assert!(history.delete(record).unwrap());
    assert_eq!(history.purge(Some(vec![record])).unwrap(), 1);
    assert!(history.images.lock().unwrap().0.is_empty());
    assert!(history.image(record, Some(120)).unwrap().is_none());
}

#[test]
fn merging_into_an_existing_record_drops_its_cached_thumbnails() {
    use content::DIB;
    let mut dib = Vec::new();
    dib.extend_from_slice(&40u32.to_le_bytes());
    dib.extend_from_slice(&300i32.to_le_bytes());
    dib.extend_from_slice(&100i32.to_le_bytes());
    dib.extend_from_slice(&1u16.to_le_bytes());
    dib.extend_from_slice(&24u16.to_le_bytes());
    dib.resize(40, 0);
    dib.extend(std::iter::repeat_n(120u8, 900 * 100));
    let directory = Directory::new();
    let history = memory_history(&directory);
    let at = Instant::now();
    let other = id(&save(&history, content(&[(TEXT, b"other")]), 1, at));
    let record = id(&save(&history, content(&[(DIB, &dib)]), 2, at));
    history.image(record, Some(120)).unwrap().unwrap();
    history.image(record, Some(60)).unwrap().unwrap();
    assert_eq!(history.images.lock().unwrap().0.len(), 2);
    // 同一次复制又补来一个格式：合并到已有 id，它的缩略图作废。
    let merged = save(&history, content(&[(DIB, &dib), (HTML, b"html")]), 2, at);
    assert_eq!(merged.outcome, "merged_same_observation");
    assert!(merged.merged());
    assert_eq!(id(&merged), record);
    assert!(history.images.lock().unwrap().0.is_empty());
    // 别的 id 的缓存不受影响。
    history.images.lock().unwrap().put(
        (other, 120),
        &preview::ImageView {
            url: "u".into(),
            width: 1,
            height: 1,
            bytes: 1,
        },
    );
    history.image(record, Some(120)).unwrap().unwrap();
    save(
        &history,
        content(&[(DIB, &dib), (HTML, b"html"), (TEXT, b"t")]),
        2,
        at,
    );
    assert_eq!(history.images.lock().unwrap().0.len(), 1);
    assert!(history.images.lock().unwrap().get((other, 120)).is_some());
}

#[test]
fn hiding_the_panel_releases_thumbnails_and_idle_readers() {
    let directory = Directory::new();
    let directories = directories(&directory);
    let logger = Logger::with_settings(&directories, 20).unwrap();
    let history = History::new(sha256, logger);
    history.initialize();
    history.images.lock().unwrap().put(
        (1, 120),
        &preview::ImageView {
            url: "u".into(),
            width: 1,
            height: 1,
            bytes: 1,
        },
    );
    history.release_idle();
    assert!(history.images.lock().unwrap().0.is_empty());
    // 释放后读照常可用。
    assert!(history.get(1).unwrap().is_none());
}

#[test]
fn oversized_thumbnails_are_not_cached() {
    let mut cache = preview::ImageCache::default();
    let view = |len: usize| preview::ImageView {
        url: "x".repeat(len),
        width: 1,
        height: 1,
        bytes: 1,
    };
    cache.put((1, 480), &view(2 << 20));
    assert!(cache.get((1, 480)).is_none());
    cache.put((1, 480), &view(1 << 20));
    assert!(cache.get((1, 480)).is_some());
}

#[test]
fn thumbnail_cache_keeps_the_eight_most_recent() {
    let mut cache = preview::ImageCache::default();
    let view = preview::ImageView {
        url: "u".into(),
        width: 1,
        height: 1,
        bytes: 1,
    };
    for n in 0..8 {
        cache.put((n, 480), &view);
    }
    assert!(cache.get((0, 480)).is_some()); // 刚用过，变成最新
    cache.put((8, 480), &view); // 挤掉最久没用的 1
    assert!(cache.get((1, 480)).is_none());
    assert!(cache.get((0, 480)).is_some());
}
