//! M2-V 验收门槛 5、6 的应用层部分（存储层部分在 nianlai-store 的 gates_tests.rs，
//! 真实应用的那一半在 tools/m2v-gates.ps1）。
//!
//! 门槛 5：回收站“删除 → 恢复 → 再删除 → 过期”走应用层的真实代码，用传入的时间代替真实等待。
//! 门槛 6：被暂停、排除应用、不要记录标记、敏感规则挡下的正文，按真实的规则链走一遍后，
//! 在库、WAL、SHM、日志里按原始字节都找不到；被记录的对照组必须找得到，证明扫描有效。

use super::*;
use crate::{
    capture::{blocking_marker, rules::Gate},
    settings::{Capture, Directories, tests::Directory},
};
use content::{Data, TEXT};
use std::{collections::BTreeMap, fs, path::Path};

fn directories(directory: &Directory) -> Directories {
    Directories {
        settings_file: directory.0.join("settings.json"),
        data: directory.0.join("data"),
        logs: directory.0.join("logs"),
        data_source: "设置文件指定",
        test_data_dir: false,
    }
}
fn history(directories: &Directories, persistent: bool) -> History {
    let logger = Logger::with_settings(directories, 20).unwrap();
    let history = History::new(nianlai_store::hash::sha256, logger);
    if persistent {
        history.initialize();
        assert!(history.0.store().is_some());
    }
    history
}
fn event(sequence: u32, process: &str) -> CopyEvent {
    CopyEvent {
        time_ms: now_ms() + sequence as u64 * 10,
        sequence,
        source_process: process.into(),
        source_path: String::new(),
        owner_process: process.into(),
        owner_pid: 1,
        observed: Instant::now(),
    }
}
fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

// ---------------------------------------------------------------- 门槛 5

/// 模拟“现在是 `at_ms`”时的过期清理：与 `purge_expired` 同一个截止算法，只是时钟由调用方给。
fn expire_at(shared: &Shared, at_ms: u64) -> usize {
    shared
        .purge_before(None, at_ms.saturating_sub(TRASH_DAYS * DAY_MS), "expired")
        .unwrap()
}

#[test]
fn gate5_trash_chain_delete_restore_delete_and_expire_in_both_modes() {
    assert_eq!(TRASH_DAYS, 7, "存储层测试里的保留期常量要同步");
    for persistent in [false, true] {
        let directory = Directory::new();
        let history = history(&directories(&directory), persistent);
        let shared = &history.0;
        let save = |text: &str, sequence: u32| {
            shared
                .ingest(
                    Content::text(text, nianlai_store::hash::sha256).unwrap(),
                    event(sequence, "chain.exe"),
                    false,
                    Instant::now(),
                )
                .unwrap()
                .record_id
                .unwrap()
        };
        let keep = save("回收站全链路 保留", 1);
        let target = save("回收站全链路 目标", 2);
        let in_trash =
            || -> Vec<usize> { shared.trash().unwrap().iter().map(|t| t.item.id).collect() };
        let listed = || -> Vec<usize> { shared.snapshot().items.iter().map(|i| i.id).collect() };
        assert_eq!(listed(), [target, keep], "新的在前");
        let t0 = now_ms();

        // 删除 → 进回收站（带删除时间），列表里没有。
        assert!(shared.delete(target).unwrap());
        assert_eq!(in_trash(), [target]);
        assert!(shared.trash().unwrap()[0].deleted_ms >= t0);
        assert_eq!(listed(), [keep]);
        // 恢复 → 回到列表。
        assert_eq!(shared.restore(target).unwrap().unwrap().id, target);
        assert!(in_trash().is_empty());
        assert_eq!(listed(), [target, keep]);
        // 再删除 → 又进回收站。
        assert!(shared.delete(target).unwrap());
        assert_eq!(in_trash(), [target]);

        // 时钟走到删除后 7 天少一小时：还没过期；过了 7 天一小时：过期并物理清除。
        assert_eq!(expire_at(shared, t0 + TRASH_DAYS * DAY_MS - 3_600_000), 0);
        assert_eq!(in_trash(), [target], "未满保留期");
        assert_eq!(expire_at(shared, t0 + TRASH_DAYS * DAY_MS + 3_600_000), 1);
        assert!(in_trash().is_empty());
        assert!(shared.get(target).unwrap().is_none(), "内容已物理删除");
        assert!(shared.restore(target).unwrap().is_none());
        assert_eq!(listed(), [keep]);
        assert!(shared.get(keep).unwrap().is_some());
        // 真实时钟的后台清理：刚删的不会被清。
        let fresh = save("回收站全链路 刚删", 3);
        assert!(shared.delete(fresh).unwrap());
        shared.purge_expired();
        assert_eq!(in_trash(), [fresh]);
    }
}

// ---------------------------------------------------------------- 门槛 6

/// 目录下任何文件里是否有这段正文（UTF-8 或 UTF-16LE 原样字节），返回命中的文件。
fn hits(directory: &Path, body: &str) -> Vec<String> {
    let needles = [body.as_bytes().to_vec(), utf16(body)];
    let mut found = Vec::new();
    let mut stack = vec![directory.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(&path).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let bytes = fs::read(&path).unwrap_or_default();
            if needles
                .iter()
                .any(|n| bytes.windows(n.len()).any(|w| w == n.as_slice()))
            {
                found.push(path.file_name().unwrap().to_string_lossy().into_owned());
            }
        }
    }
    found
}

/// 一次“系统剪贴板变化”走的规则链：暂停/排除 → 不要记录标记 → 内容规则 → 写入。
/// 返回结论（"stored" 或跳过的原因）。日志里只记原因，和真实采集一致。
fn offer(
    gate: &Gate,
    history: &History,
    owner: &str,
    markers: &[(&str, u32)],
    body: &str,
    sequence: u32,
) -> String {
    let skipped = |reason: &str| {
        history.0.logger.event(
            "capture",
            json!({"outcome":"skipped","reason":reason,"owner_process":owner}),
        );
        reason.to_owned()
    };
    if let Some(reason) = gate.before_read(owner, owner) {
        return skipped(reason);
    }
    let value = |name: &str| markers.iter().find(|m| m.0 == name).map(|m| m.1);
    if blocking_marker(markers.iter().map(|m| m.0), value).is_some() {
        return skipped("marker");
    }
    let mut data = BTreeMap::from([(TEXT.to_owned(), Data::Bytes(utf16(body).into()))]);
    if let Err(skip) = gate.after_read(&mut data) {
        return skipped(skip.reason);
    }
    let content = Content::new(data, nianlai_store::hash::sha256).unwrap();
    let now = Instant::now();
    Shared::capture(
        &history.0,
        content,
        event(sequence, owner),
        false,
        now,
        None,
    )
    .unwrap();
    history
        .0
        .logger
        .event("capture", json!({"outcome":"saved","owner_process":owner}));
    "stored".into()
}

#[test]
fn gate6_excluded_paused_marked_and_sensitive_content_leaves_no_trace() {
    let directory = Directory::new();
    let directories = directories(&directory);
    let history = history(&directories, true);
    let gate = Gate::default();
    gate.apply(&Capture::default());
    // 唯一标记词不含任何敏感规则的前缀；正文还带中文，保证 UTF-16 形式也被扫到。
    let stamp = now_ms();
    let body = |tag: &str| format!("m2vtrace{tag}{stamp} 拈来验收样本正文 {tag}");
    let mut skipped: Vec<String> = Vec::new();
    let mut stored: Vec<String> = Vec::new();
    let mut sequence = 0;
    let mut run = |gate: &Gate, owner: &str, markers: &[(&str, u32)], text: String, want: &str| {
        sequence += 1;
        let got = offer(gate, &history, owner, markers, &text, sequence);
        assert_eq!(got, want, "{owner} {markers:?}");
        if want == "stored" {
            stored.push(text);
        } else {
            skipped.push(text);
        }
    };

    // 排除应用：默认名单里的每一个，再加大小写不同的写法。
    let mut owners: Vec<String> = crate::settings::DEFAULT_EXCLUDED_APPS
        .map(String::from)
        .to_vec();
    owners.push("keepassxc.EXE".into());
    for (i, owner) in owners.iter().enumerate() {
        run(
            &gate,
            owner,
            &[],
            body(&format!("excluded{i}")),
            "excluded_app",
        );
    }
    // 四种“不要记录”标记。
    for (i, marker) in crate::capture::MARKERS.iter().enumerate() {
        run(
            &gate,
            "FakeNotepad.exe",
            &[(marker, 0)],
            body(&format!("marker{i}")),
            "marker",
        );
    }
    // 敏感内容（GitHub Token 形态）。
    let token = format!("ghp_{}", "aB3dE6gH9jK2mN5pQ8sT1vW4yZ7bC0eF3hJ6");
    run(
        &gate,
        "FakeNotepad.exe",
        &[],
        format!("{} {token}", body("sensitive")),
        "sensitive",
    );
    // 暂停期间：普通程序也不记。
    gate.set_paused(true);
    run(&gate, "FakeNotepad.exe", &[], body("paused"), "paused");
    gate.set_paused(false);
    // 对照组：普通复制、带“允许”标记值（1）的复制，必须记下。
    run(&gate, "FakeNotepad.exe", &[], body("control"), "stored");
    run(
        &gate,
        "FakeNotepad.exe",
        &[("CanIncludeInClipboardHistory", 1)],
        body("control2"),
        "stored",
    );

    // 记录之后的各种查看途径也不能把被挡的东西带出来。
    history.flush_deferred();
    let snapshot = serde_json::to_string(&history.snapshot()).unwrap();
    for text in &skipped {
        let key = text.split(' ').next().unwrap();
        assert!(!snapshot.contains(key), "列表里有被挡的内容 {key}");
        let found = serde_json::to_string(&history.search(key, 1).unwrap()).unwrap();
        assert!(!found.contains(key), "搜索找到被挡的内容 {key}");
    }
    for text in &stored {
        assert!(snapshot.contains(text.as_str()), "对照组没进列表 {text}");
    }
    let store = history.0.store().unwrap();
    // 让 WAL 里有东西：不做检查点，直接扫文件。
    history.0.logger.flush();
    let (data, logs) = (&directories.data, &directories.logs);
    for text in &stored {
        assert!(
            !hits(data, text).is_empty(),
            "扫描方式失灵：库目录里找不到对照组 {text}"
        );
    }
    for text in &skipped {
        assert_eq!(
            hits(data, text),
            Vec::<String>::new(),
            "库、WAL、SHM 或图标目录里有被挡的正文"
        );
        assert_eq!(hits(logs, text), Vec::<String>::new(), "日志里有被挡的正文");
    }
    // 日志里谁的正文都不该有，包括对照组。
    for text in skipped.iter().chain(&stored) {
        assert_eq!(hits(logs, text), Vec::<String>::new(), "日志里有正文");
    }
    // 库文件（含 WAL）确实在被扫描：至少有 history.db 和一个日志文件。
    assert!(data.join("history.db").is_file());
    assert!(fs::read_dir(logs).unwrap().count() > 0);
    drop(store);
}
