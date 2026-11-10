use super::*;
use crate::{content::Content, hash::sha256, tests::Temp};
use std::{fs, io::Read, io::Write, path::PathBuf, sync::Arc};

fn text(value: &str) -> Arc<Content> {
    Content::text(value, sha256).unwrap()
}
fn event(sequence: u32, process: &str) -> CopyEvent {
    CopyEvent {
        time_ms: 1_000 + sequence as u64,
        sequence,
        source_process: process.into(),
        source_path: format!(r"C:\Apps\{process}"),
        owner_process: process.into(),
        owner_pid: 7,
        observed: Instant::now(),
    }
}
fn add(store: &Store, value: &str, sequence: u32, process: &str) -> usize {
    let outcome = store
        .ingest(text(value), event(sequence, process), false, Instant::now())
        .unwrap();
    outcome.record_id.unwrap()
}
fn icon(store: &Store, process: &str, file: &str) {
    let directory = store.directory().join("icons");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join(file), format!("png:{file}")).unwrap();
    let app = store
        .reader()
        .unwrap()
        .source_app_by_path(&format!(r"C:\Apps\{process}"))
        .unwrap()
        .unwrap();
    store
        .refresh_source_app(
            app.id,
            format!("名-{process}"),
            format!("k-{file}"),
            file.into(),
        )
        .unwrap();
}

/// 源库：a、b、c 三条活的内容（a、c 收藏，c 有标题和备注），d 在回收站里（来自只有它用的 gone.exe）。
fn source() -> (Temp, Store) {
    let temp = Temp::new();
    let store = temp.store();
    let a = add(&store, "alpha", 1, "source.exe");
    add(&store, "bravo", 2, "other.exe");
    let c = add(&store, "charlie", 3, "source.exe");
    let d = add(&store, "delta-trash", 4, "gone.exe");
    icon(&store, "source.exe", "source.png");
    icon(&store, "other.exe", "other.png");
    icon(&store, "gone.exe", "gone.png");
    fs::write(temp.0.join("icons").join("orphan.png"), b"x").unwrap();
    store.set_favorite(c, true).unwrap();
    store.set_favorite(a, true).unwrap();
    store
        .set_favorite_label(c, "标题", "第一行\n第二行")
        .unwrap();
    assert!(store.delete(d).unwrap());
    (temp, store)
}

fn entries(file: &Path) -> Vec<(String, Vec<u8>)> {
    let mut archive = zip::ZipArchive::new(fs::File::open(file).unwrap()).unwrap();
    (0..archive.len())
        .map(|i| {
            let mut entry = archive.by_index(i).unwrap();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            (entry.name().unwrap().into_owned(), bytes)
        })
        .collect()
}
fn write_zip(file: &Path, entries: &[(String, Vec<u8>)]) {
    let mut zip = zip::ZipWriter::new(fs::File::create(file).unwrap());
    for (name, bytes) in entries {
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
}
fn tamper(from: &Path, to: &Path, change: impl FnOnce(&mut Vec<(String, Vec<u8>)>)) {
    let mut list = entries(from);
    change(&mut list);
    write_zip(to, &list);
}
fn rewrite_manifest(list: &mut [(String, Vec<u8>)]) {
    let files: Vec<serde_json::Value> = list
        .iter()
        .filter(|(name, _)| name != "manifest.json")
        .map(|(name, bytes)| {
            serde_json::json!({"name":name,"bytes":bytes.len(),"sha256":hash::hex(&sha256(bytes).unwrap())})
        })
        .collect();
    let manifest = list.iter_mut().find(|(n, _)| n == "manifest.json").unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&manifest.1).unwrap();
    value["files"] = files.into();
    manifest.1 = serde_json::to_vec(&value).unwrap();
}

fn exported(store: &Store) -> (Temp, PathBuf) {
    let out = Temp::new();
    let file = out.0.join("备份.nianlai-backup");
    export_backup(store.directory(), &file, "9.9.9").unwrap();
    (out, file)
}
fn live(store: &Store) -> Vec<(Fingerprint, u64)> {
    let mut items: Vec<_> = store
        .reader()
        .unwrap()
        .recent()
        .unwrap()
        .into_iter()
        .map(|m| (m.fingerprint, m.copied_ms))
        .collect();
    items.sort();
    items
}
fn favorites(store: &Store) -> Vec<(Fingerprint, String, String)> {
    store
        .reader()
        .unwrap()
        .favorites()
        .unwrap()
        .into_iter()
        .map(|m| (m.fingerprint, m.title, m.note))
        .collect()
}
fn apps(store: &Store) -> Vec<(String, String, String)> {
    let c = store.reader().unwrap();
    let mut apps: Vec<_> = ["source.exe", "other.exe", "gone.exe"]
        .iter()
        .filter_map(|p| c.source_app_by_path(&format!(r"C:\Apps\{p}")).unwrap())
        .map(|a| (a.process_name, a.display_name, a.icon_file))
        .collect();
    apps.sort();
    apps
}
fn failed_step(inspection: &Inspection) -> &'static str {
    assert!(!inspection.ok);
    let step = inspection.steps.last().unwrap();
    assert!(!step.ok);
    step.id
}

#[test]
fn export_contains_only_the_allowed_files_without_trash() {
    let (_temp, store) = source();
    let summary = backup_summary(store.directory()).unwrap();
    assert_eq!((summary.items, summary.favorites, summary.icons), (3, 2, 2));
    let (_out, file) = exported(&store);
    let list = entries(&file);
    let mut names: Vec<_> = list.iter().map(|(n, _)| n.as_str()).collect();
    names.sort();
    assert_eq!(
        names,
        [
            "history.db",
            "icons/other.png",
            "icons/source.png",
            "manifest.json"
        ]
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&list.iter().find(|(n, _)| n == "manifest.json").unwrap().1)
            .unwrap();
    assert_eq!(manifest["format"], 1);
    assert_eq!(manifest["app"], "nianlai");
    assert_eq!(manifest["app_version"], "9.9.9");
    assert_eq!(manifest["schema_version"], VERSION);
    assert_eq!(manifest["counts"]["items"], 3);
    assert_eq!(manifest["counts"]["favorites"], 2);
    assert_eq!(manifest["counts"]["icons"], 2);
    assert_eq!(manifest["files"].as_array().unwrap().len(), 3);
    // 导出的库里没有回收站内容，也没有只被它引用的来源应用。
    let inspection = inspect_backup(store.directory(), &file);
    assert!(inspection.ok, "{:?}", inspection.steps);
    let staged = backup::staging(store.directory(), &inspection.token).unwrap();
    let c = schema::readonly(&staged.join("history.db")).unwrap();
    let counts = schema::counts(&c).unwrap();
    assert_eq!(
        (counts.contents, counts.trashed, counts.source_apps),
        (3, 0, 2)
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM contents WHERE preview LIKE '%trash%'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    // 现役库不受影响：回收站还在。
    assert_eq!(store.reader().unwrap().trash().unwrap().len(), 1);
    // 没有遗留 .pending。
    assert!(!file.with_extension("nianlai-backup.pending").exists());
}

#[test]
fn export_inspect_merge_into_fresh_database_reproduces_the_live_contents() {
    let (_temp, store) = source();
    let (_out, file) = exported(&store);
    let target = Temp::new();
    let fresh = target.store();
    let inspection = inspect_backup(target.0.as_path(), &file);
    assert!(inspection.ok, "{:?}", inspection.steps);
    assert_eq!(inspection.app_version, "9.9.9");
    assert_eq!(inspection.upgraded_from, None);
    assert_eq!(inspection.steps.len(), 3);
    assert_eq!(inspection.steps[1].detail, "3 个文件的校验值都对");
    assert_eq!(
        inspection.merge,
        Some(MergePreview {
            new_items: 3,
            new_favorites: 2
        })
    );
    assert_eq!(inspection.overwrite, Some(OverwritePreview { items: 3 }));
    let report = fresh.import_merge(&inspection.token).unwrap();
    assert_eq!(
        report,
        MergeReport {
            added_items: 3,
            added_favorites: 2
        }
    );
    assert_eq!(live(&fresh), live(&store));
    assert_eq!(favorites(&fresh), favorites(&store));
    let imported = favorites(&fresh);
    assert_eq!(imported[0].1, "标题");
    assert_eq!(imported[0].2, "第一行\n第二行");
    let mut expected = apps(&store);
    expected.retain(|(p, ..)| p != "gone.exe");
    assert_eq!(apps(&fresh), expected);
    assert!(target.0.join("icons/source.png").is_file());
    assert!(target.0.join("icons/other.png").is_file());
    assert!(!target.0.join("icons/gone.png").exists());
    // 缓存已刷新，顺序与源一致；索引可搜索；整库通过完整检查。
    let cache = fresh.cache();
    assert_eq!(cache.total, 3);
    let source_order: Vec<_> = store.cache().items.iter().map(|m| m.fingerprint).collect();
    let fresh_order: Vec<_> = cache.items.iter().map(|m| m.fingerprint).collect();
    assert_eq!(source_order, fresh_order);
    assert!(cache.items.iter().all(|m| m.source_name.is_some()));
    assert_eq!(
        fresh
            .reader()
            .unwrap()
            .search("charlie")
            .unwrap()
            .items
            .len(),
        1
    );
    check(&target.0).unwrap();
    // 第二次合并同一个备份：什么都不加。
    let again = fresh.import_merge(&inspection.token).unwrap();
    assert_eq!(
        again,
        MergeReport {
            added_items: 0,
            added_favorites: 0
        }
    );
    assert_eq!(fresh.cache().total, 3);
    check(&target.0).unwrap();
    // 之后新采集的内容排在导入之后。
    let newest = add(&fresh, "fresh-copy", 99, "source.exe");
    assert_eq!(fresh.cache().items[0].id, newest);
    discard_import(&target.0, &inspection.token);
    assert!(backup::staging(&target.0, &inspection.token).is_none());
    assert!(fresh.import_merge(&inspection.token).is_err());
}

#[test]
fn merge_into_existing_data_adds_only_new_contents_and_appends_favorites() {
    let (_temp, store) = source();
    let (_out, file) = exported(&store);
    let target = Temp::new();
    let local = target.store();
    // 本地已有 bravo（未收藏）、alpha（已收藏，不同标题）、charlie 在回收站里，另有自己的 zulu（收藏）。
    let zulu = add(&local, "zulu", 10, "source.exe");
    add(&local, "bravo", 11, "other.exe");
    let alpha = add(&local, "alpha", 12, "source.exe");
    let charlie = add(&local, "charlie", 13, "source.exe");
    local.set_favorite(zulu, true).unwrap();
    local.set_favorite(alpha, true).unwrap();
    local.set_favorite_label(alpha, "本地标题", "").unwrap();
    assert!(local.delete(charlie).unwrap());
    let before = favorites(&local);
    let inspection = inspect_backup(&target.0, &file);
    assert!(inspection.ok);
    // 三条里 alpha、bravo、charlie 本地都有（回收站里的也算），没有新内容；收藏里只有……都已收藏或在回收站。
    assert_eq!(
        inspection.merge,
        Some(MergePreview {
            new_items: 0,
            new_favorites: 0
        })
    );
    let report = local.import_merge(&inspection.token).unwrap();
    assert_eq!(
        report,
        MergeReport {
            added_items: 0,
            added_favorites: 0
        }
    );
    assert_eq!(favorites(&local), before);
    assert!(local.reader().unwrap().trash().unwrap().len() == 1);

    // 换一个本地：只有 bravo；备份里 alpha、charlie 是新内容，且是收藏。
    let target = Temp::new();
    let local = target.store();
    let first = add(&local, "first-local", 20, "source.exe");
    let bravo = add(&local, "bravo", 21, "other.exe");
    local.set_favorite(first, true).unwrap();
    local.set_favorite(bravo, true).unwrap();
    local.set_favorite_label(bravo, "我的", "备注").unwrap();
    let inspection = inspect_backup(&target.0, &file);
    assert_eq!(
        inspection.merge,
        Some(MergePreview {
            new_items: 2,
            new_favorites: 2
        })
    );
    let report = local.import_merge(&inspection.token).unwrap();
    assert_eq!(
        report,
        MergeReport {
            added_items: 2,
            added_favorites: 2
        }
    );
    let merged = favorites(&local);
    assert_eq!(merged.len(), 4);
    // 原有收藏的顺序和标签不变，新收藏按备份里的收藏顺序（charlie 先、alpha 后）接在后面。
    assert_eq!(
        merged[..2],
        before_labels(&local, &["first-local", "bravo"])
    );
    assert_eq!(merged[1].1, "我的");
    assert_eq!(merged[2].0, text("charlie").fingerprint);
    assert_eq!(merged[2].1, "标题");
    assert_eq!(merged[3].0, text("alpha").fingerprint);
    // 本地 bravo 原来的 copy_count 等不动。
    assert_eq!(local.cache().total, 4);
    let again = local.import_merge(&inspection.token).unwrap();
    assert_eq!(
        again,
        MergeReport {
            added_items: 0,
            added_favorites: 0
        }
    );
    check(&target.0).unwrap();
}

fn before_labels(_store: &Store, texts: &[&str]) -> Vec<(Fingerprint, String, String)> {
    texts
        .iter()
        .map(|t| {
            let label = if *t == "bravo" { "我的" } else { "" };
            let note = if *t == "bravo" { "备注" } else { "" };
            (text(t).fingerprint, label.to_owned(), note.to_owned())
        })
        .collect()
}

#[test]
fn tampering_fails_at_the_right_step_and_leaves_data_alone() {
    let (_temp, store) = source();
    let (out, file) = exported(&store);
    let target = Temp::new();
    let local = target.store();
    add(&local, "mine", 1, "source.exe");
    let before = (live(&local), favorites(&local));
    let bad = out.0.join("bad.nianlai-backup");
    let check_untouched = |inspection: &Inspection| {
        assert!(inspection.merge.is_none() && inspection.overwrite.is_none());
        assert!(backup::staging(&target.0, &inspection.token).is_none());
        assert!(!target.0.join("imports").join(&inspection.token).exists());
        assert_eq!((live(&local), favorites(&local)), before);
    };

    // 改了库内容但没改清单 → 校验值对不上。
    tamper(&file, &bad, |list| {
        let database = list.iter_mut().find(|(n, _)| n == "history.db").unwrap();
        let last = database.1.len() - 1;
        database.1[last] ^= 0xff;
    });
    let inspection = inspect_backup(&target.0, &bad);
    assert_eq!(failed_step(&inspection), "hashes");
    assert_eq!(inspection.steps.len(), 2);
    assert!(inspection.steps[0].ok);
    assert!(inspection.steps[1].detail.contains("history.db"));
    check_untouched(&inspection);

    // 压缩包里多出一个不允许的文件 / 路径穿越 / 绝对路径 → 清单步骤失败。
    for name in [
        "extra.txt",
        "../evil",
        "icons/../evil",
        "/abs",
        "icons/sub/x.png",
        "icons/",
        "C:/x",
    ] {
        tamper(&file, &bad, |list| list.push((name.into(), b"x".to_vec())));
        let inspection = inspect_backup(&target.0, &bad);
        assert_eq!(failed_step(&inspection), "manifest", "{name}");
        assert_eq!(inspection.steps.len(), 1);
        check_untouched(&inspection);
    }

    // 图标名合法但不在清单里 → 校验值步骤发现多出文件；清单里的文件缺失同理。
    tamper(&file, &bad, |list| {
        list.push(("icons/extra.png".into(), b"x".to_vec()))
    });
    let inspection = inspect_backup(&target.0, &bad);
    assert_eq!(failed_step(&inspection), "hashes");
    assert!(inspection.steps[1].detail.contains("多出"));
    check_untouched(&inspection);
    tamper(&file, &bad, |list| {
        list.retain(|(n, _)| n != "icons/other.png")
    });
    let inspection = inspect_backup(&target.0, &bad);
    assert_eq!(failed_step(&inspection), "hashes");
    assert!(inspection.steps[1].detail.contains("缺少"));
    check_untouched(&inspection);
    // 清单里的大小不符。
    tamper(&file, &bad, |list| {
        let manifest = list.iter_mut().find(|(n, _)| n == "manifest.json").unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&manifest.1).unwrap();
        value["files"][1]["bytes"] = 1.into();
        manifest.1 = serde_json::to_vec(&value).unwrap();
    });
    assert_eq!(failed_step(&inspect_backup(&target.0, &bad)), "hashes");

    // 清单格式版本不对 / 缺清单 / 不是 zip / 不是 zip 的空文件。
    tamper(&file, &bad, |list| {
        let manifest = list.iter_mut().find(|(n, _)| n == "manifest.json").unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&manifest.1).unwrap();
        value["format"] = 2.into();
        manifest.1 = serde_json::to_vec(&value).unwrap();
    });
    let inspection = inspect_backup(&target.0, &bad);
    assert_eq!(failed_step(&inspection), "manifest");
    assert!(inspection.steps[0].detail.contains("格式版本 2"));
    tamper(&file, &bad, |list| {
        list.retain(|(n, _)| n != "manifest.json")
    });
    assert_eq!(failed_step(&inspect_backup(&target.0, &bad)), "manifest");
    fs::write(&bad, b"not a zip").unwrap();
    assert_eq!(failed_step(&inspect_backup(&target.0, &bad)), "manifest");
    assert_eq!(
        failed_step(&inspect_backup(
            &target.0,
            &out.0.join("missing.nianlai-backup")
        )),
        "manifest"
    );

    // 比当前新的库版本 → 数据库步骤失败，信息里说明。
    tamper(&file, &bad, |list| {
        let database = list.iter_mut().find(|(n, _)| n == "history.db").unwrap();
        let dir = Temp::new();
        let path = dir.0.join("h.db");
        fs::write(&path, &database.1).unwrap();
        writable(&path)
            .unwrap()
            .execute_batch(&format!("PRAGMA user_version={};", VERSION + 1))
            .unwrap();
        database.1 = fs::read(&path).unwrap();
        rewrite_manifest(list);
    });
    let inspection = inspect_backup(&target.0, &bad);
    assert_eq!(failed_step(&inspection), "database");
    assert_eq!(inspection.steps.len(), 3);
    assert!(inspection.steps[2].detail.contains("更新版本"));
    check_untouched(&inspection);

    // 库内容损坏（校验值与清单一致，但不是数据库）→ 数据库步骤失败。
    tamper(&file, &bad, |list| {
        let database = list.iter_mut().find(|(n, _)| n == "history.db").unwrap();
        database.1 = vec![7; 4096];
        rewrite_manifest(list);
    });
    let inspection = inspect_backup(&target.0, &bad);
    assert_eq!(failed_step(&inspection), "database");
    check_untouched(&inspection);
    // 原文件始终是好的。
    assert!(inspect_backup(&target.0, &file).ok);
}

#[test]
fn old_version_backup_is_upgraded_in_the_staging_area_only() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("v2.db");
    let bytes = fs::read(&fixture).unwrap();
    let expected: usize = {
        let c = Connection::open_with_flags(&fixture, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        c.query_row("SELECT count(*) FROM contents", [], |r| {
            schema::number(r, 0)
        })
        .unwrap()
    };
    let favorites_expected: usize = {
        let c = Connection::open_with_flags(&fixture, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        c.query_row("SELECT count(*) FROM favorites", [], |r| {
            schema::number(r, 0)
        })
        .unwrap()
    };
    let out = Temp::new();
    let file = out.0.join("old.nianlai-backup");
    let mut list = vec![
        ("history.db".to_owned(), bytes.clone()),
        (
            "manifest.json".to_owned(),
            serde_json::to_vec(&serde_json::json!({
                "format":1,"app":"nianlai","app_version":"0.0.1","schema_version":2,"exported_ms":5,
                "counts":{"items":expected,"favorites":favorites_expected,"icons":0},"files":[]
            }))
            .unwrap(),
        ),
    ];
    rewrite_manifest(&mut list);
    write_zip(&file, &list);
    let target = Temp::new();
    let local = target.store();
    let inspection = inspect_backup(&target.0, &file);
    assert!(inspection.ok, "{:?}", inspection.steps);
    assert_eq!(inspection.upgraded_from, Some(2));
    assert_eq!(
        inspection.steps[2].detail,
        format!("完整性通过 · 旧版本 v2，已在临时区升级到 v{VERSION}")
    );
    assert_eq!(
        inspection.overwrite,
        Some(OverwritePreview { items: expected })
    );
    assert_eq!(
        inspection.merge,
        Some(MergePreview {
            new_items: expected,
            new_favorites: favorites_expected
        })
    );
    // 升级发生在临时副本上，用户给的文件原样。
    assert_eq!(entries(&file)[0].1, bytes);
    let report = local.import_merge(&inspection.token).unwrap();
    assert_eq!(report.added_items, expected);
    assert_eq!(report.added_favorites, favorites_expected);
    assert_eq!(local.cache().total, expected);
    check(&target.0).unwrap();
}

#[test]
fn overwrite_replaces_database_and_icons_through_the_restore_path() {
    let (_temp, store) = source();
    let (_out, file) = exported(&store);
    let target = Temp::new();
    {
        let local = target.store();
        add(&local, "only-local", 1, "mine.exe");
        icon(&local, "mine.exe", "mine.png");
    }
    let inspection = inspect_backup(&target.0, &file);
    assert!(inspection.ok);
    assert!(prepare_overwrite(&target.0, "nope").is_err());
    assert!(prepare_overwrite(&target.0, "../x").is_err());
    prepare_overwrite(&target.0, &inspection.token).unwrap();
    apply_overwrite(&target.0, &inspection.token).unwrap();
    assert!(!target.0.join("icons/mine.png").exists());
    assert!(target.0.join("icons/source.png").is_file());
    assert!(target.0.join("icons/other.png").is_file());
    // 旧库留作 before-restore 副本。
    assert!(fs::read_dir(&target.0).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("history.db.before-restore-")
    }));
    let restored = target.store();
    assert_eq!(live(&restored), live(&store));
    assert_eq!(favorites(&restored), favorites(&store));
    check(&target.0).unwrap();
    // 旧版的覆盖备份也能通过 prepare（临时区里已是最新版本）。
    clean_stale_imports(&target.0);
    assert!(!target.0.join("imports").exists());
}
