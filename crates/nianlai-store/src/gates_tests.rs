//! M2-V 验收门槛的存储层部分（门槛 1–5），可在 CI 里直接跑。
//! 真实应用、真实剪贴板的那一半在 `tools/m2v-gates.ps1`。
//!
//! 1. 可控复制零丢失：每条不同的文字恰好进库一次，重复复制只累加次数。
//! 2. 写入中途强杀：子进程反复写库，收到回执后被强杀，已回执的提交不丢、库通过完整检查。
//! 3. 旧库升级：v1、v2 夹具升级后，旧表里的每一行（逐列）都原样保留。
//! 4. 备份导出再导入：合并进空库、覆盖进旧库，内容、收藏顺序、标题备注、来源应用和图标都一致。
//! 5. 回收站全链路：删除 → 进回收站 → 恢复 → 再删除 → 过期物理清除（用传入的时间代替真实等待）。
//!
//! 默认规模较小；`NIANLAI_GATE1_COPIES`、`NIANLAI_GATE2_ROUNDS` 可以调大，
//! 完整规模（1 万条、30 次强杀）见 `gate1_ten_thousand_copies`、`gate2_thirty_kills`（#[ignore]）。

use crate::{
    content::{Content, Data, FILES, HTML, PNG},
    hash::{hex, sha256},
    tests::Temp,
    *,
};
use rusqlite::{params, types::Value};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, mpsc},
    time::Duration,
};

fn text(value: &str) -> Arc<Content> {
    Content::text(value, sha256).unwrap()
}
fn event(sequence: u32, process: &str) -> CopyEvent {
    CopyEvent {
        time_ms: 1_000_000 + sequence as u64,
        sequence,
        source_process: process.into(),
        source_path: format!(r"C:\Apps\{process}"),
        owner_process: process.into(),
        owner_pid: 7,
        observed: Instant::now(),
    }
}
fn add(store: &Store, content: Arc<Content>, sequence: u32, process: &str) -> Ingest {
    let result = store
        .ingest(content, event(sequence, process), false, Instant::now())
        .unwrap();
    assert!(result.transaction_ok, "{result:?}");
    result
}
/// 只读连接，直接看库里的真实行（不经过存储层的缓存）。
fn raw(directory: &Path) -> Connection {
    Connection::open_with_flags(
        directory.join("history.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
}
fn fingerprints(directory: &Path) -> Vec<Vec<u8>> {
    raw(directory)
        .prepare("SELECT fingerprint FROM contents")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}
fn env_number(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

// ---------------------------------------------------------------- 门槛 1

fn gate1(total: usize) {
    let temp = Temp::new();
    let store = temp.store();
    let mut texts = Vec::with_capacity(total);
    let mut ids = HashMap::new();
    let (mut sequence, mut repeats) = (0u32, 0usize);
    for i in 1..=total {
        let value = format!("gate1-{i:06}-{}", "拈来".repeat(i % 5));
        sequence += 1;
        let saved = add(&store, text(&value), sequence, "gate.exe");
        assert_eq!(saved.outcome, "saved", "第 {i} 条没有存下：{saved:?}");
        ids.insert(value.clone(), saved.record_id.unwrap());
        texts.push(value);
        // 每 10 条再复制一遍更早的一条：只累加次数，不新增内容。
        if i % 10 == 0 {
            let old = &texts[i / 2 - 1];
            sequence += 1;
            let again = add(&store, text(old), sequence, "gate.exe");
            assert_eq!(again.outcome, "duplicate", "{old}");
            assert_eq!(again.record_id, Some(ids[old]), "{old}");
            repeats += 1;
        }
    }
    let verify = |directory: &Path| {
        let counts = check(directory).unwrap();
        assert_eq!(
            (counts.contents, counts.formats, counts.indexed),
            (total, total, total)
        );
        assert_eq!(counts.events, total + repeats, "每次复制都有一条事件");
        let stored = fingerprints(directory);
        let distinct: HashSet<_> = stored.iter().cloned().collect();
        assert_eq!(stored.len(), distinct.len(), "有内容重复入库");
        let missing: Vec<_> = texts
            .iter()
            .filter(|t| !distinct.contains(text(t).fingerprint.as_slice()))
            .take(10)
            .collect();
        assert!(missing.is_empty(), "丢失：{missing:?}");
    };
    verify(&temp.0);
    // 抽样搜索：索引里每条恰好一份。
    let reader = store.reader().unwrap();
    for value in texts.iter().step_by((total / 40).max(1)) {
        let key = value.split('拈').next().unwrap();
        let found = reader.search(key).unwrap();
        assert_eq!(found.items.len(), 1, "{key}");
        assert_eq!(found.items[0].id, ids[value]);
    }
    drop(reader);
    drop(store);
    // 重启后一样。
    let reopened = temp.store();
    assert_eq!(reopened.cache().total, total);
    assert_eq!(reopened.report().migrated_from, None);
    drop(reopened);
    verify(&temp.0);
}

#[test]
fn gate1_distinct_copies_are_recorded_exactly_once() {
    gate1(env_number("NIANLAI_GATE1_COPIES", 1000));
}

#[test]
#[ignore = "完整规模 1 万条，约几十秒：cargo test -p nianlai-store gate1_ten -- --ignored"]
fn gate1_ten_thousand_copies() {
    gate1(10_000);
}

// ---------------------------------------------------------------- 门槛 2

const WRITER_DIR: &str = "NIANLAI_GATE2_DIR";
const WRITER_ROUND: &str = "NIANLAI_GATE2_ROUND";

/// 辅助模式：被门槛 2 的测试用同一个测试程序再启动，不停写库，每次提交后在标准输出打回执。
/// 平时的 `cargo test` 里没有环境变量，立即返回。
#[test]
fn gate2_writer_helper() {
    let Some(directory) = std::env::var_os(WRITER_DIR).map(PathBuf::from) else {
        return;
    };
    let round: u32 = std::env::var(WRITER_ROUND).unwrap().parse().unwrap();
    let store = Store::open(&directory, sha256).unwrap();
    let mut out = std::io::stdout().lock();
    for n in 1u32..=999_999 {
        let value = format!("gate2-r{round:03}-{n:07}");
        // 序列号随轮次单调增加，重开后不会被当成过期通知。
        add(&store, text(&value), round * 1_000_000 + n, "writer.exe");
        writeln!(out, "ACK {value}").unwrap();
        out.flush().unwrap();
    }
}

fn gate2(rounds: usize) {
    let temp = Temp::new();
    let mut acked: Vec<String> = Vec::new();
    for round in 1..=rounds {
        // 每轮收到不同数量的回执后强杀，落点分散在不同的写入阶段。
        let target = 15 + (round * 37) % 90;
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "gates_tests::gate2_writer_helper",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(WRITER_DIR, &temp.0)
            .env(WRITER_ROUND, round.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(|l| l.ok()) {
                if let Some(value) = line.strip_prefix("ACK ") {
                    let _ = sender.send(value.to_owned());
                }
            }
        });
        let mut this_round = Vec::new();
        while this_round.len() < target {
            match receiver.recv_timeout(Duration::from_secs(60)) {
                Ok(value) => this_round.push(value),
                Err(_) => {
                    let _ = child.kill();
                    panic!("第 {round} 轮：写入进程 60 秒内没有给出足够的回执");
                }
            }
        }
        // 此时写入进程仍在高速提交，强杀就落在写入中途。
        child.kill().unwrap();
        child.wait().unwrap();
        reader.join().unwrap();
        // 强杀前已经打出的回执也算（管道里剩余的行）。
        this_round.extend(receiver.try_iter());
        acked.extend(this_round);

        // 每次强杀之后：整库完整检查（含 FTS 外部内容比对），已回执的一条不少。
        let counts = check(&temp.0).unwrap_or_else(|e| panic!("第 {round} 轮检查失败：{e:?}"));
        assert_eq!(counts.contents, counts.indexed, "索引条数和内容不一致");
        let stored: HashSet<Vec<u8>> = fingerprints(&temp.0).into_iter().collect();
        let lost: Vec<_> = acked
            .iter()
            .filter(|v| !stored.contains(text(v).fingerprint.as_slice()))
            .take(10)
            .collect();
        assert!(lost.is_empty(), "第 {round} 轮丢失已回执的提交：{lost:?}");
        assert!(counts.contents >= acked.len());
        // 干净启动：能正常打开、不触发迁移。下一轮的子进程会再打开一次。
        let store = temp.store();
        assert_eq!(store.report().migrated_from, None);
        assert_eq!(store.cache().total, counts.contents);
    }
}

#[test]
fn gate2_kill_while_writing_keeps_acknowledged_commits() {
    // 辅助模式下不递归。
    if std::env::var_os(WRITER_DIR).is_some() {
        return;
    }
    gate2(env_number("NIANLAI_GATE2_ROUNDS", 4));
}

#[test]
#[ignore = "30 轮强杀，约一两分钟：cargo test -p nianlai-store gate2_thirty -- --ignored"]
fn gate2_thirty_kills() {
    if std::env::var_os(WRITER_DIR).is_some() {
        return;
    }
    gate2(30);
}

// ---------------------------------------------------------------- 门槛 3

type Table = (Vec<String>, Vec<Vec<Value>>);

/// 整张表按列原样取出，行按所有列排序。
fn dump(c: &Connection, table: &str, columns: Option<&[String]>) -> Table {
    let names: Vec<String> = match columns {
        Some(columns) => columns.to_vec(),
        None => c
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(|r| r.unwrap())
            .collect(),
    };
    let order: Vec<String> = (1..=names.len()).map(|n| n.to_string()).collect();
    let rows = c
        .prepare(&format!(
            "SELECT {} FROM {table} ORDER BY {}",
            names.join(","),
            order.join(",")
        ))
        .unwrap()
        .query_map([], |r| {
            (0..names.len()).map(|i| r.get::<_, Value>(i)).collect()
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    (names, rows)
}

#[test]
fn gate3_old_fixtures_upgrade_without_changing_a_single_cell() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for version in [1u32, 2] {
        let temp = Temp::new();
        fs::copy(
            fixtures.join(format!("v{version}.db")),
            temp.0.join("history.db"),
        )
        .unwrap();
        // 升级前：旧库里所有表的所有列。
        let before = raw(&temp.0);
        assert_eq!(migrations::user_version(&before).unwrap(), version);
        let tables: Vec<String> = [
            "contents",
            "formats",
            "copy_events",
            "source_apps",
            "favorites",
        ]
        .into_iter()
        .filter(|t| {
            before
                .query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name=?1",
                    [t],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap()
                == 1
        })
        .map(String::from)
        .collect();
        assert!(tables.len() >= 3, "v{version} 夹具缺表：{tables:?}");
        let old: Vec<(String, Table)> = tables
            .iter()
            .map(|t| (t.clone(), dump(&before, t, None)))
            .collect();
        let live_rows = old[0].1.1.len();
        assert!(live_rows >= 30, "夹具太小：{live_rows}");
        drop(before);

        let store = Store::open(&temp.0, sha256).unwrap();
        assert_eq!(store.report().version, VERSION);
        assert_eq!(
            store.report().migrated_from,
            (VERSION > version).then_some(version)
        );
        let after = raw(&temp.0);
        // 升级后：旧表里的每一列、每一行都原样还在（新增的列和行不比较）。
        // v2 起迁移会给 contents 补来源应用的外键列，那是新列，旧列不变。
        for (table, (columns, rows)) in &old {
            let now = dump(&after, table, Some(columns));
            if table == "source_apps" {
                // v2 才回填；v1 夹具没有这张表，v2 夹具里的行必须保留。
                assert!(now.1.len() >= rows.len(), "{table} 行数减少");
                for row in rows {
                    assert!(now.1.contains(row), "v{version} {table} 的行丢了：{row:?}");
                }
                continue;
            }
            assert_eq!(now.1.len(), rows.len(), "v{version} {table} 行数变了");
            assert_eq!(&now.1, rows, "v{version} {table} 内容变了");
        }
        // 每条内容还能读出来、指纹复算一致；文字内容能被搜到。
        let reader = store.reader().unwrap();
        let ids: Vec<i64> = after
            .prepare("SELECT id FROM contents")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        let mut searched = 0;
        for id in ids {
            let content = reader
                .content(id as usize)
                .unwrap()
                .unwrap_or_else(|| panic!("v{version} 内容 {id} 读不出来"));
            let stored: Vec<u8> = after
                .query_row("SELECT fingerprint FROM contents WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(content.fingerprint.as_slice(), stored.as_slice());
            if let Some(value) = content.text_value()
                && let Some(word) = value.split_whitespace().next()
                && word.chars().count() >= 3
            {
                let found = reader.search(word).unwrap();
                assert!(
                    found.items.iter().any(|m| m.id == id as usize),
                    "v{version} 搜“{word}”找不到内容 {id}"
                );
                searched += 1;
            }
        }
        assert!(searched >= 10, "v{version} 可搜索的样本太少：{searched}");
        drop((reader, after));
        check(&temp.0).unwrap();
        drop(store);
        // 再开一次不再迁移，旧库的备份副本在。
        let again = Store::open(&temp.0, sha256).unwrap();
        assert_eq!(again.report().migrated_from, None);
        assert_eq!(again.cache().total, live_rows);
        let backups = fs::read_dir(temp.0.join("backups"))
            .map(|d| d.count())
            .unwrap_or(0);
        assert_eq!(backups, usize::from(VERSION > version), "迁移前备份");
    }
}

// ---------------------------------------------------------------- 门槛 4

#[derive(Debug, PartialEq)]
struct Item {
    kind: String,
    preview: String,
    bytes: i64,
    sort_ms: i64,
    copy_count: i64,
    events: i64,
    app: Option<String>,
    icon: Option<String>,
    icon_sha: Option<String>,
    formats: Vec<(String, String, i64)>,
}
#[derive(Debug, PartialEq)]
struct Report {
    items: BTreeMap<String, Item>,
    favorites: Vec<(String, String, String)>,
    trashed: i64,
}

/// 按内容指纹整理出一份可比较的报告（id 在不同库里不同，所以不用 id）。
fn report(directory: &Path) -> Report {
    let c = raw(directory);
    let mut items = BTreeMap::new();
    let mut statement = c
        .prepare(
            "SELECT c.id,hex(c.fingerprint),c.kind,c.preview,c.bytes,c.sort_ms,c.copy_count,\
             (SELECT count(*) FROM copy_events e WHERE e.content_id=c.id),sa.display_name,sa.icon_file \
             FROM contents c LEFT JOIN source_apps sa ON sa.id=c.source_app_id \
             WHERE NOT EXISTS(SELECT 1 FROM trash t WHERE t.content_id=c.id)",
        )
        .unwrap();
    let rows: Vec<_> = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<String>>(9)?,
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    for (id, fingerprint, kind, preview, bytes, sort_ms, copy_count, events, app, icon) in rows {
        let formats = c
            .prepare(
                "SELECT name,hex(fingerprint),bytes FROM formats WHERE content_id=?1 ORDER BY name",
            )
            .unwrap()
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        let icon_sha = icon.as_deref().filter(|f| !f.is_empty()).map(|f| {
            match fs::read(directory.join("icons").join(f)) {
                Ok(bytes) => hex(&sha256(&bytes).unwrap()),
                Err(_) => "<缺失>".to_owned(),
            }
        });
        let item = Item {
            kind,
            preview,
            bytes,
            sort_ms,
            copy_count,
            events,
            app,
            icon,
            icon_sha,
            formats,
        };
        assert!(items.insert(fingerprint, item).is_none(), "指纹重复");
    }
    let favorites = c
        .prepare(
            "SELECT hex(c.fingerprint),f.title,f.note FROM favorites f JOIN contents c ON c.id=f.content_id \
             WHERE NOT EXISTS(SELECT 1 FROM trash t WHERE t.content_id=c.id) ORDER BY f.position",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let trashed = c
        .query_row("SELECT count(*) FROM trash", [], |r| r.get(0))
        .unwrap();
    Report {
        items,
        favorites,
        trashed,
    }
}

fn set_icon(store: &Store, process: &str) {
    let file = format!("{}.png", process.trim_end_matches(".exe"));
    let icons = store.directory().join("icons");
    fs::create_dir_all(&icons).unwrap();
    // 每个图标内容不同，才能分辨串了图标。
    fs::write(icons.join(&file), format!("png-bytes-of-{file}").repeat(20)).unwrap();
    let app = store
        .reader()
        .unwrap()
        .source_app_by_path(&format!(r"C:\Apps\{process}"))
        .unwrap()
        .unwrap();
    store
        .refresh_source_app(
            app.id,
            format!("应用 {process}"),
            format!("key-{file}"),
            file,
        )
        .unwrap();
}

/// 源库：文字、富文本、文件列表、图片混合，多个来源应用带图标，重复复制，
/// 8 个收藏（倒序重排、3 个带标题备注），4 条在回收站里。
fn rich_source() -> (Temp, Store) {
    let temp = Temp::new();
    let store = temp.store();
    let apps = ["a.exe", "b.exe", "c.exe", "d.exe"];
    let mut ids = Vec::new();
    let mut sequence = 0;
    let mut next = |store: &Store, content: Arc<Content>, process: &str| {
        sequence += 1;
        add(store, content, sequence, process).record_id.unwrap()
    };
    for i in 0..40 {
        ids.push(next(
            &store,
            text(&format!("backup-text-{i:03} 内容 {}", "字".repeat(i % 7))),
            apps[i % 4],
        ));
    }
    for i in 0..5 {
        let rich = text(&format!("backup-rich-{i}"))
            .merge(
                &Content::new(
                    BTreeMap::from([(
                        HTML.into(),
                        Data::Bytes(format!("<b>rich {i}</b>").into_bytes().into()),
                    )]),
                    sha256,
                )
                .unwrap(),
                sha256,
            )
            .unwrap();
        ids.push(next(&store, rich, apps[i % 4]));
    }
    for i in 0..3 {
        let files = Content::new(
            BTreeMap::from([(
                FILES.into(),
                Data::Files(vec![format!(r"C:\data\file-{i}.txt"), r"D:\x\y.rs".into()].into()),
            )]),
            sha256,
        )
        .unwrap();
        ids.push(next(&store, files, apps[i % 4]));
    }
    for i in 0..3u8 {
        let image = Content::new(
            BTreeMap::from([(PNG.into(), Data::Bytes(vec![i; 2000 + i as usize].into()))]),
            sha256,
        )
        .unwrap();
        ids.push(next(&store, image, apps[i as usize % 4]));
    }
    // 重复复制：次数和事件都累加。
    for i in [0usize, 5, 12, 30, 41] {
        next(
            &store,
            store.reader().unwrap().content(ids[i]).unwrap().unwrap(),
            "b.exe",
        );
    }
    for app in apps {
        set_icon(&store, app);
    }
    // 收藏：先按创建顺序，再整体倒序，标题备注写在第 1、3、6 个。
    let favorites: Vec<usize> = ids.iter().copied().step_by(5).take(8).collect();
    for id in &favorites {
        store.set_favorite(*id, true).unwrap().unwrap();
    }
    let reversed: Vec<usize> = favorites.iter().rev().copied().collect();
    store.reorder_favorites(&reversed).unwrap();
    for (n, id) in [reversed[0], reversed[2], reversed[5]]
        .into_iter()
        .enumerate()
    {
        store
            .set_favorite_label(id, &format!("标题{n}"), &format!("备注{n}\n第二行"))
            .unwrap();
    }
    // 回收站里的不导出：挑非收藏的几条。
    let trash: Vec<usize> = ids
        .iter()
        .copied()
        .filter(|id| !favorites.contains(id))
        .skip(3)
        .take(4)
        .collect();
    for id in trash {
        assert!(store.delete(id).unwrap());
    }
    (temp, store)
}

#[test]
fn gate4_backup_round_trip_merge_and_overwrite_reproduce_the_same_set() {
    let (source, store) = rich_source();
    let expected = report(&source.0);
    assert_eq!(expected.items.len(), 47, "构造的内容数：51 条减 4 条回收站");
    assert_eq!(expected.favorites.len(), 8);
    assert!(expected.trashed > 0, "源库里要有回收站内容");
    let out = Temp::new();
    let file = out.0.join("m2v.nianlai-backup");
    let exported = export_backup(&source.0, &file, "m2v").unwrap();
    assert_eq!(exported.items, expected.items.len());

    // 合并进空库。回收站里的不导入，其余逐项一致。
    let empty = Temp::new();
    let target = empty.store();
    let inspection = inspect_backup(&empty.0, &file);
    assert!(inspection.ok, "{:?}", inspection.steps);
    let merged = target.import_merge(&inspection.token).unwrap();
    assert_eq!(merged.added_items, expected.items.len());
    assert_eq!(merged.added_favorites, 8);
    let after_merge = report(&empty.0);
    assert_eq!(after_merge.trashed, 0);
    assert_eq!(
        Report {
            trashed: expected.trashed,
            ..after_merge
        },
        expected,
        "合并后的内容、收藏顺序、标题备注、来源应用、图标应与源库一致"
    );
    check(&empty.0).unwrap();
    let reader = target.reader().unwrap();
    // 搜索：导入的文字内容（回收站里的不在其中）每条都能搜到。
    let live_texts: Vec<_> = expected
        .items
        .values()
        .filter(|i| i.preview.starts_with("backup-text-"))
        .map(|i| i.preview.split_whitespace().next().unwrap().to_owned())
        .collect();
    assert!(live_texts.len() >= 30);
    for key in &live_texts {
        assert_eq!(reader.search(key).unwrap().items.len(), 1, "{key}");
    }
    drop(reader);
    // 再合并一次：什么都不加。
    let again = target.import_merge(&inspection.token).unwrap();
    assert_eq!((again.added_items, again.added_favorites), (0, 0));
    drop(target);

    // 覆盖：目标里先放一些不同的本地内容，覆盖后只剩备份里的。
    let local = Temp::new();
    {
        let mine = local.store();
        for n in 1..=5 {
            add(&mine, text(&format!("only-local-{n}")), n, "mine.exe");
        }
        mine.set_favorite(1, true).unwrap();
        set_icon(&mine, "mine.exe");
    }
    let inspection = inspect_backup(&local.0, &file);
    assert!(inspection.ok);
    assert_eq!(
        inspection.overwrite.as_ref().unwrap().items,
        expected.items.len()
    );
    prepare_overwrite(&local.0, &inspection.token).unwrap();
    apply_overwrite(&local.0, &inspection.token).unwrap();
    let restored = local.store();
    drop(restored);
    let after_overwrite = report(&local.0);
    assert_eq!(after_overwrite.trashed, 0);
    assert_eq!(
        Report {
            trashed: expected.trashed,
            ..after_overwrite
        },
        expected,
        "覆盖后应当恰好是备份里的那一套"
    );
    assert!(
        !local.0.join("icons/mine.png").exists(),
        "本地旧图标应被清掉"
    );
    check(&local.0).unwrap();
    drop(store);
}

// ---------------------------------------------------------------- 门槛 5

const HOUR_MS: u64 = 3_600_000;
// 与 src-tauri 的 TRASH_DAYS 保持一致（那边的测试用真实常量）。
const RETENTION_MS: u64 = 7 * 24 * HOUR_MS;

fn backdate(directory: &Path, id: usize, deleted_ms: u64) {
    let c = writable(&directory.join("history.db")).unwrap();
    let changed = c
        .execute(
            "UPDATE trash SET deleted_ms=?2 WHERE content_id=?1",
            params![id as i64, deleted_ms as i64],
        )
        .unwrap();
    assert_eq!(changed, 1);
}
fn rows_of(directory: &Path, id: usize) -> (i64, i64, i64, i64, i64) {
    let c = raw(directory);
    let count = |sql: &str| {
        c.query_row(sql, [id as i64], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    (
        count("SELECT count(*) FROM contents WHERE id=?1"),
        count("SELECT count(*) FROM formats WHERE content_id=?1"),
        count("SELECT count(*) FROM copy_events WHERE content_id=?1"),
        count("SELECT count(*) FROM favorites WHERE content_id=?1"),
        count("SELECT count(*) FROM trash WHERE content_id=?1"),
    )
}

#[test]
fn gate5_delete_restore_delete_again_and_expire_purges_everything() {
    let temp = Temp::new();
    let store = temp.store();
    let keep = add(&store, text("trashchain 保留"), 1, "x.exe")
        .record_id
        .unwrap();
    let a = add(&store, text("trashchain 甲 收藏"), 2, "x.exe")
        .record_id
        .unwrap();
    let b = add(&store, text("trashchain 乙"), 3, "x.exe")
        .record_id
        .unwrap();
    store.set_favorite(a, true).unwrap().unwrap();
    let reader = store.reader().unwrap();
    let in_trash = |r: &Reader| -> Vec<usize> {
        let mut ids: Vec<_> = r.trash().unwrap().iter().map(|(m, _)| m.id).collect();
        ids.sort_unstable();
        ids
    };
    let found = |r: &Reader| -> Vec<usize> {
        let mut ids: Vec<_> = r
            .search("trashchain")
            .unwrap()
            .items
            .iter()
            .map(|m| m.id)
            .collect();
        ids.sort_unstable();
        ids
    };

    // 删除 → 进回收站（带删除时间），列表、收藏、搜索里都看不到。
    let t0 = now_ms();
    assert!(store.delete(a).unwrap());
    assert!(store.delete(b).unwrap());
    assert_eq!(in_trash(&reader), [a, b]);
    assert!(reader.trash().unwrap().iter().all(|(_, at)| *at >= t0));
    assert_eq!(found(&reader), [keep]);
    assert!(reader.favorites().unwrap().is_empty());
    assert_eq!(store.cache().total, 1);

    // 恢复：回来，收藏状态还在；回收站里只剩 b。
    assert_eq!(store.restore(a).unwrap().unwrap().id, a);
    assert!(
        store
            .reader()
            .unwrap()
            .favorites()
            .unwrap()
            .iter()
            .any(|m| m.id == a)
    );
    assert_eq!(in_trash(&reader), [b]);
    assert_eq!(found(&reader), [keep, a]);

    // 再次删除 a。
    assert!(store.delete(a).unwrap());
    assert_eq!(in_trash(&reader), [a, b]);

    // 时钟：b 删除于 8 天前（已满 7 天），a 删除于 6 天前，边界条目恰好满 7 天。
    let now = now_ms();
    let c = add(&store, text("trashchain 丙边界"), 4, "x.exe")
        .record_id
        .unwrap();
    assert!(store.delete(c).unwrap());
    let cutoff = now - RETENTION_MS;
    backdate(&temp.0, b, now - 8 * 24 * HOUR_MS);
    backdate(&temp.0, a, now - 6 * 24 * HOUR_MS);
    backdate(&temp.0, c, cutoff);
    // 现在清理：只有 b 过期；恰好等于截止时间的 c 不算（严格早于才清）。
    assert_eq!(store.purge(None, cutoff).unwrap(), 1);
    assert_eq!(
        rows_of(&temp.0, b),
        (0, 0, 0, 0, 0),
        "过期的内容在各表里都没有了"
    );
    assert_eq!(in_trash(&reader), [a, c]);
    assert!(store.restore(b).unwrap().is_none(), "已清除的不能恢复");
    assert_eq!(found(&reader), [keep], "回收站里的仍搜不到");
    // 时钟前进 2 天：a（6 天前删）满 8 天，c 满 9 天，都过期。
    let later = now + 2 * 24 * HOUR_MS;
    assert_eq!(store.purge(None, later - RETENTION_MS).unwrap(), 2);
    for id in [a, b, c] {
        assert_eq!(rows_of(&temp.0, id), (0, 0, 0, 0, 0), "内容 {id}");
    }
    assert!(reader.trash().unwrap().is_empty());
    assert!(reader.favorites().unwrap().is_empty(), "收藏行随内容清除");
    assert_eq!(rows_of(&temp.0, keep), (1, 1, 1, 0, 0));
    // 整库完整检查（含索引和外部内容比对），索引里只剩保留的一条。
    let counts = check(&temp.0).unwrap();
    assert_eq!(
        (
            counts.contents,
            counts.indexed,
            counts.trashed,
            counts.favorites
        ),
        (1, 1, 0, 0)
    );
    drop(reader);
    drop(store);
    assert_eq!(temp.store().cache().total, 1);
}
