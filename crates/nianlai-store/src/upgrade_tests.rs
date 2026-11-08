// 旧库升级测试与迁移框架测试。
//
// 夹具 tests/fixtures/v1.db 是用 v1 代码（G0 样机四的结构）经正常写入路径生成的库：几十条记录，含文本、富文本、
// 图片格式、文件列表、重复复制与同次观察合并；v1.json 是生成时记下的期望清单（记录、搜索结果、结构）。
// 这里的测试每次都把夹具复制到临时目录再打开，仓库里的文件不会被改动。
use super::{tests::Temp, *};
use content::{Content, DIB, Data, FILES, HTML, PNG, RTF, TEXT};
use hash::sha256;
use migrations::{STEPS, Step, migrate, user_version};
use rusqlite::Transaction;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::PathBuf, sync::Arc, time::Duration};

// 2026-10-25 00:00 UTC；记录时间按序列号逐分钟递增，夹具内容与生成时间无关。
const BASE_MS: u64 = 1_792_886_400_000;
const QUERIES: &[&str] = &[
    "",
    "拈来",
    "剪贴板工具",
    "clipboard",
    "CLIPBOARD",
    "example.com",
    "12345",
    "println",
    "订单尾号",
    "😀",
    "👩‍💻",
    "%",
    "\"引号\"",
    "rust",
    "重复内容",
    "项目报告",
    "IMG_0002",
    "目录",
    "路径",
    "富文本",
    "合并前",
    "ab",
    "订 8867",
    "select",
    "不存在的词语",
    "长句",
    "  前后空白  ",
];

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
struct Manifest {
    version: u32,
    counts: ManifestCounts,
    records: Vec<Record>,
    searches: Vec<SearchCase>,
    schema: Vec<SchemaRow>,
}
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
struct ManifestCounts {
    contents: usize,
    formats: usize,
    events: usize,
    indexed: usize,
    // v2 起有的计数；旧清单（v1.json）没有这两个字段，读入时按 0 处理。
    #[serde(default)]
    favorites: usize,
    #[serde(default)]
    source_apps: usize,
}
impl From<Counts> for ManifestCounts {
    fn from(c: Counts) -> Self {
        Self {
            contents: c.contents,
            formats: c.formats,
            events: c.events,
            indexed: c.indexed,
            favorites: c.favorites,
            source_apps: c.source_apps,
        }
    }
}
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
struct Record {
    id: usize,
    kind: String,
    preview: String,
    file_names: Vec<String>,
    bytes: usize,
    partial: bool,
    copied_ms: u64,
    source: String,
    copy_count: usize,
    fingerprint: String,
    formats: Vec<String>,
    text: Option<String>,
}
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
struct SearchCase {
    query: String,
    mode: String,
    ids: Vec<usize>,
}
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
struct SchemaRow {
    kind: String,
    name: String,
    table: String,
    sql: Option<String>,
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}
fn load_manifest(version: u32) -> Manifest {
    serde_json::from_slice(&fs::read(fixtures().join(format!("v{version}.json"))).unwrap()).unwrap()
}
// 夹具复制到临时目录当作 history.db；仓库里的夹具永远不直接打开。
fn fixture_copy(version: u32) -> Temp {
    let temp = Temp::new();
    fs::copy(
        fixtures().join(format!("v{version}.db")),
        temp.0.join("history.db"),
    )
    .unwrap();
    temp
}

fn noise(size: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..size)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}
fn utf16(text: &str) -> Data {
    Data::Bytes(
        text.encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
            .into(),
    )
}
fn bytes(value: &[u8]) -> Data {
    Data::Bytes(value.into())
}
fn png(width: u32, height: u32, payload: usize, seed: u64) -> Data {
    let mut data = b"\x89PNG\r\n\x1a\n".to_vec();
    data.extend_from_slice(&13u32.to_be_bytes());
    data.extend_from_slice(b"IHDR");
    data.extend_from_slice(&width.to_be_bytes());
    data.extend_from_slice(&height.to_be_bytes());
    data.extend_from_slice(&[8, 6, 0, 0, 0]);
    data.extend(noise(payload, seed));
    Data::Bytes(data.into())
}
fn dib(width: i32, height: i32, payload: usize, seed: u64) -> Data {
    let mut data = vec![0u8; 40];
    data[..4].copy_from_slice(&40u32.to_le_bytes());
    data[4..8].copy_from_slice(&width.to_le_bytes());
    data[8..12].copy_from_slice(&height.to_le_bytes());
    data[12..14].copy_from_slice(&1u16.to_le_bytes());
    data[14..16].copy_from_slice(&32u16.to_le_bytes());
    data.extend(noise(payload, seed));
    Data::Bytes(data.into())
}
fn files(paths: &[&str]) -> Data {
    Data::Files(
        paths
            .iter()
            .map(|p| (*p).to_owned())
            .collect::<Vec<_>>()
            .into(),
    )
}
fn content(formats: &[(&str, Data)]) -> Arc<Content> {
    Content::new(
        formats
            .iter()
            .map(|(name, data)| ((*name).to_owned(), data.clone()))
            .collect::<BTreeMap<_, _>>(),
        sha256,
    )
    .unwrap()
}
fn text(value: &str) -> Arc<Content> {
    Content::text(value, sha256).unwrap()
}
fn event(sequence: u32, origin: Instant) -> CopyEvent {
    let sources = ["notepad.exe", "Code.exe", "chrome.exe", "explorer.exe"];
    let source = sources[sequence as usize % sources.len()];
    CopyEvent {
        time_ms: BASE_MS + sequence as u64 * 60_000,
        sequence,
        source_process: source.into(),
        source_path: format!(
            r"C:\Program Files\{}\{source}",
            source.trim_end_matches(".exe")
        ),
        owner_process: source.into(),
        owner_pid: 1000 + sequence % 4,
        observed: origin + Duration::from_secs(sequence as u64 * 60),
    }
}

// 夹具的全部写入；顺序决定 id，改动它会让 v1.json 失配，所以只在重建夹具时才允许改。
fn populate(store: &Store, origin: Instant) {
    let ingest = |content: Arc<Content>, sequence: u32, partial: bool| -> &'static str {
        store
            .ingest(content, event(sequence, origin), partial, origin)
            .unwrap()
            .outcome
    };
    let long = "这是一段用于测试全文索引的中文长句，包含标点、数字 42 和英文 word。".repeat(20);
    let texts = [
        "拈来是一个开源剪贴板工具，呼出快、不抢焦点。",
        "Clipboard history should survive restarts and upgrades.",
        "https://example.com/path/to/resource?id=12345&lang=zh-CN",
        "fn main() {\n    println!(\"你好，拈来\");\n}\n",
        "订单尾号 8867，收件人：张三，电话尾号 0042",
        "😀 表情与组合字符 👩‍💻 测试",
        "100% 完成 a_b\\c \"引号\" 'single' <tag> & more",
        "ABC Mixed Case rUsT RUST rust",
        "2026-10-25 21:30:00 时间戳与数字 3.14159 1e10",
        long.as_str(),
        "　全角空格　与\t制表符\r\n换行 CRLF",
        "C:\\Users\\张三\\Documents\\报告.docx 路径文本",
        "SELECT * FROM contents WHERE kind='文本' ORDER BY sort_ms DESC;",
        "日本語テキスト と 한국어 텍스트 и русский",
        "重复内容 A",
        "重复内容 B",
        "Ditto EcoPaste Maccy PasteNow 竞品名单",
        "短",
        "ab",
        "   前后空白   ",
    ];
    let mut sequence = 0;
    for value in texts {
        sequence += 1;
        assert_eq!(ingest(text(value), sequence, false), "saved");
    }
    // 21–25 富文本；24 是同一次观察先到纯文本、后补 HTML 的合并。
    let rich = [
        content(&[
            (TEXT, utf16("富文本 一号")),
            (HTML, bytes("<b>富文本 一号</b>".as_bytes())),
        ]),
        content(&[
            (TEXT, utf16("Rich text two")),
            (HTML, bytes(b"<p>Rich text two</p>")),
            (RTF, bytes(b"{\\rtf1\\ansi Rich text two}")),
        ]),
        content(&[(HTML, bytes("<i>只有 HTML 没有纯文本</i>".as_bytes()))]),
    ];
    for value in rich {
        sequence += 1;
        assert_eq!(ingest(value, sequence, false), "saved");
    }
    sequence += 1;
    assert_eq!(ingest(text("合并前的纯文本"), sequence, false), "saved");
    assert_eq!(
        ingest(
            content(&[
                (TEXT, utf16("合并前的纯文本")),
                (HTML, bytes("<span>合并前的纯文本</span>".as_bytes())),
            ]),
            sequence,
            false,
        ),
        "merged_same_observation"
    );
    sequence += 1;
    assert_eq!(
        ingest(
            content(&[
                (TEXT, utf16("Markdown **加粗** `code`")),
                (HTML, bytes(b"<strong>Markdown</strong>")),
            ]),
            sequence,
            false,
        ),
        "saved"
    );
    // 26–30 图片：PNG、DIB、两种格式同在、坏头。
    let images = [
        content(&[(PNG, png(16, 16, 400, 26))]),
        content(&[(PNG, png(1920, 1080, 600, 27))]),
        content(&[(DIB, dib(64, -32, 300, 28))]),
        content(&[(PNG, png(320, 240, 200, 29)), (DIB, dib(320, 240, 200, 29))]),
        content(&[(PNG, bytes(&noise(128, 30)))]),
    ];
    for value in images {
        sequence += 1;
        assert_eq!(ingest(value, sequence, false), "saved");
    }
    // 31–34 文件列表。
    let lists = [
        content(&[(FILES, files(&["C:\\资料\\项目报告.docx"]))]),
        content(&[(
            FILES,
            files(&[
                "D:\\photos\\IMG_0001.jpg",
                "D:\\photos\\IMG_0002.jpg",
                "D:\\photos\\IMG_0003.jpg",
                "D:\\photos\\IMG_0004.jpg",
            ]),
        )]),
        content(&[(FILES, files(&["E:\\目录\\"]))]),
        content(&[(FILES, files(&["C:/forward/slash/路径.txt"]))]),
    ];
    for value in lists {
        sequence += 1;
        assert_eq!(ingest(value, sequence, false), "saved");
    }
    // 35 带图片格式的文本；36 标记为不完整；37 最后一条。
    sequence += 1;
    assert_eq!(
        ingest(
            content(&[
                (TEXT, utf16("图文混合：带图片格式的文本")),
                (PNG, png(8, 8, 64, 35))
            ]),
            sequence,
            false,
        ),
        "saved"
    );
    sequence += 1;
    assert_eq!(ingest(text("读取时被打断的内容"), sequence, true), "saved");
    sequence += 1;
    assert_eq!(ingest(text("最后一条记录"), sequence, false), "saved");
    // 重复复制：15 复制三次，16 复制两次；再把 1 复用到最前。
    sequence += 1;
    assert_eq!(ingest(text(texts[14]), sequence, false), "duplicate");
    sequence += 1;
    assert_eq!(ingest(text(texts[15]), sequence, false), "duplicate");
    sequence += 1;
    assert_eq!(ingest(text(texts[14]), sequence, false), "duplicate");
    store
        .reuse(1, BASE_MS + (sequence as u64 + 1) * 60_000)
        .unwrap();
}

fn hex(fingerprint: &[u8]) -> String {
    fingerprint.iter().map(|b| format!("{b:02x}")).collect()
}
fn schema_rows(c: &Connection) -> Vec<SchemaRow> {
    c.prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name")
        .unwrap()
        .query_map([], |r| {
            Ok(SchemaRow {
                kind: r.get(0)?,
                name: r.get(1)?,
                table: r.get(2)?,
                sql: r.get(3)?,
            })
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}
// 按正常读取路径（Reader）把库里的一切整理成清单。
fn manifest_of(directory: &Path) -> Manifest {
    let reader = Reader::open(directory, sha256).unwrap();
    let mut items = reader.recent().unwrap();
    items.sort_by_key(|item| item.id);
    let records = items
        .into_iter()
        .map(|item| {
            let content = reader.content(item.id).unwrap().unwrap();
            assert_eq!(content.fingerprint, item.fingerprint);
            Record {
                id: item.id,
                kind: item.kind,
                preview: item.preview,
                file_names: item.file_names,
                bytes: item.bytes,
                partial: item.partial,
                copied_ms: item.copied_ms,
                source: item.source,
                copy_count: item.copy_count,
                fingerprint: hex(&item.fingerprint),
                formats: item.formats.into_iter().map(|f| f.name).collect(),
                text: content.text_value(),
            }
        })
        .collect();
    let searches = QUERIES
        .iter()
        .map(|query| {
            let result = reader.search(query).unwrap();
            SearchCase {
                query: (*query).to_owned(),
                mode: format!("{:?}", result.mode),
                ids: result.items.iter().map(|item| item.id).collect(),
            }
        })
        .collect();
    Manifest {
        version: user_version(&reader.connection).unwrap(),
        counts: reader.counts().unwrap().into(),
        records,
        searches,
        schema: schema_rows(&reader.connection),
    }
}
// 结构以外的部分逐项比对（结构在真正迁移之后会变，单独测）。
// v2 起回填来源应用表，其行数取决于旧数据，不列入逐项比对（专门的 v2 测试单独断言）；
// 收藏表迁移不产生行，保持比对。
fn assert_matches(directory: &Path, expected: &Manifest) {
    let actual = manifest_of(directory);
    let counts = |c: &ManifestCounts| (c.contents, c.formats, c.events, c.indexed, c.favorites);
    assert_eq!(counts(&actual.counts), counts(&expected.counts));
    assert_eq!(actual.records.len(), expected.records.len());
    for (actual, expected) in actual.records.iter().zip(&expected.records) {
        assert_eq!(actual, expected, "记录 {} 与清单不一致", expected.id);
    }
    for (actual, expected) in actual.searches.iter().zip(&expected.searches) {
        assert_eq!(actual, expected, "搜索“{}”与清单不一致", expected.query);
    }
    assert_eq!(actual.searches.len(), expected.searches.len());
}
fn backups(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory.join("backups")) else {
        return Vec::new();
    };
    let mut files: Vec<_> = entries.map(|e| e.unwrap().path()).collect();
    files.sort();
    files
}
fn assert_valid_backup(file: &Path, from: u32, expected: &Manifest) {
    let name = file.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with(&format!("pre-migrate-v{from}-")) && name.ends_with(".db"),
        "{name}"
    );
    let c = Connection::open_with_flags(file, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(user_version(&c).unwrap(), from);
    schema::pragma_check(&c, "integrity_check").unwrap();
    let contents: usize = c
        .query_row("SELECT count(*) FROM contents", [], |r| {
            schema::number(r, 0)
        })
        .unwrap();
    assert_eq!(contents, expected.counts.contents);
}
fn with_extra(extra: &[Step]) -> Vec<Step> {
    STEPS.iter().chain(extra).copied().collect()
}
fn table_exists(c: &Connection, name: &str) -> bool {
    c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
        [name],
        |r| r.get(0),
    )
    .unwrap()
}
fn probe(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE migration_probe(step INTEGER NOT NULL); INSERT INTO migration_probe VALUES(1);",
    )?;
    Ok(())
}
fn probe_again(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch("INSERT INTO migration_probe VALUES(2);")?;
    Ok(())
}
// 先改了结构再失败：事务回滚后这张表不能存在。
fn failing(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch("CREATE TABLE migration_failed(x);")?;
    Err(Error::Invalid("injected_migration_failure"))
}

#[test]
fn fixture_v1_opens_with_latest_code_and_matches_manifest() {
    let manifest = load_manifest(1);
    assert_eq!(manifest.version, 1);
    assert!(manifest.records.len() >= 30);
    let temp = fixture_copy(1);
    let store = Store::open(&temp.0, sha256).unwrap();
    assert_eq!(store.report().version, VERSION);
    assert_eq!(store.report().migrated_from, (VERSION > 1).then_some(1));
    assert_eq!(store.cache().total, manifest.counts.contents);
    assert_matches(&temp.0, &manifest);
    check(&temp.0).unwrap();
    let origin = Instant::now();
    let result = store
        .ingest(text("升级后新增的记录"), event(99, origin), false, origin)
        .unwrap();
    assert_eq!(result.outcome, "saved");
    let reader = store.reader().unwrap();
    assert_eq!(reader.search("升级后新增").unwrap().items.len(), 1);
    assert_eq!(reader.search("剪贴板工具").unwrap().items.len(), 1);
    let backups = backups(&temp.0);
    if VERSION > 1 {
        assert_eq!(backups.len(), 1);
        assert_valid_backup(&backups[0], 1, &manifest);
    } else {
        assert!(backups.is_empty());
        assert!(!temp.0.join("backups").exists());
    }
}

#[test]
fn fixture_file_matches_manifest_schema_and_fresh_database_when_current() {
    let manifest = load_manifest(1);
    let temp = fixture_copy(1);
    let c =
        Connection::open_with_flags(temp.0.join("history.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    assert_eq!(user_version(&c).unwrap(), 1);
    assert_eq!(schema_rows(&c), manifest.schema);
    drop(c);
    if VERSION == 1 {
        // v1 还是最新时，新建的库必须和夹具一模一样；改了 v1 的 SQL 这里会失败——应该追加步骤而不是改 v1。
        let fresh = Temp::new();
        drop(fresh.store());
        let c = schema::readonly(&fresh.0.join("history.db")).unwrap();
        assert_eq!(schema_rows(&c), manifest.schema);
    }
}

#[test]
fn migration_steps_run_in_order_after_one_backup() {
    let temp = fixture_copy(1);
    let mut c = writable(&temp.0.join("history.db")).unwrap();
    let steps = with_extra(&[Step { apply: probe }, Step { apply: probe_again }]);
    let mut log = Vec::new();
    let from = migrate(&mut c, &steps, &mut |c, from| {
        log.push((
            from,
            user_version(c).unwrap(),
            table_exists(c, "migration_probe"),
        ));
        Ok(())
    })
    .unwrap();
    assert_eq!(from, Some(1));
    assert_eq!(log, [(1, 1, false)]);
    assert_eq!(user_version(&c).unwrap(), VERSION + 2);
    let rows: Vec<i64> = c
        .prepare("SELECT step FROM migration_probe ORDER BY step")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(rows, [1, 2]);
    // 已是最新：不迁移也不备份。
    assert_eq!(
        migrate(&mut c, &steps, &mut |_, _| panic!("已是最新时不该备份")).unwrap(),
        None
    );
    // 比步骤列表更新的库：拒绝且不动。
    assert_eq!(
        migrate(&mut c, STEPS, &mut |_, _| panic!("拒绝新库时不该备份")),
        Err(Error::NewerSchema {
            found: VERSION + 2,
            supported: VERSION
        })
    );
    assert_eq!(user_version(&c).unwrap(), VERSION + 2);
}

#[test]
fn failed_step_keeps_last_complete_version_and_intact_file() {
    let manifest = load_manifest(1);
    for fail_first in [true, false] {
        let temp = fixture_copy(1);
        let db = temp.0.join("history.db");
        let mut c = writable(&db).unwrap();
        let steps = if fail_first {
            with_extra(&[Step { apply: failing }])
        } else {
            with_extra(&[Step { apply: probe }, Step { apply: failing }])
        };
        let directory = temp.0.clone();
        let result = migrate(&mut c, &steps, &mut |c, from| {
            migrations::backup(c, &directory, from).map(drop)
        });
        assert_eq!(result, Err(Error::Invalid("injected_migration_failure")));
        // 失败的那一步整体回滚，之前完成的步骤保留。
        let expected_version = if fail_first { VERSION } else { VERSION + 1 };
        assert_eq!(user_version(&c).unwrap(), expected_version);
        assert!(!table_exists(&c, "migration_failed"));
        assert_eq!(table_exists(&c, "migration_probe"), !fail_first);
        schema::pragma_check(&c, "integrity_check").unwrap();
        let contents: usize = c
            .query_row("SELECT count(*) FROM contents", [], |r| {
                schema::number(r, 0)
            })
            .unwrap();
        assert_eq!(contents, manifest.counts.contents);
        drop(c);
        // 迁移前的备份已经落盘，并且仍是 v1。
        let backups = backups(&temp.0);
        assert_eq!(backups.len(), 1);
        assert_valid_backup(&backups[0], 1, &manifest);
        if fail_first {
            // 库还是本程序支持的版本：按正常路径能打开、读写、搜索。
            let store = Store::open(&temp.0, sha256).unwrap();
            assert_eq!(store.report().version, VERSION);
            assert_matches(&temp.0, &manifest);
            check(&temp.0).unwrap();
        }
    }
}

#[test]
fn backup_failure_prevents_migration() {
    let temp = fixture_copy(1);
    let mut c = writable(&temp.0.join("history.db")).unwrap();
    let steps = with_extra(&[Step { apply: probe }]);
    let result = migrate(&mut c, &steps, &mut |_, _| {
        Err(Error::Io(std::io::ErrorKind::StorageFull))
    });
    assert_eq!(result, Err(Error::Io(std::io::ErrorKind::StorageFull)));
    assert_eq!(user_version(&c).unwrap(), 1);
    assert!(!table_exists(&c, "migration_probe"));
    schema::pragma_check(&c, "integrity_check").unwrap();
    drop(c);
    // 库还停在 v1；按正常路径打开会续上迁移，内容与清单一致。
    drop(Store::open(&temp.0, sha256).unwrap());
    assert_matches(&temp.0, &load_manifest(1));
    check(&temp.0).unwrap();
}

#[test]
fn reopened_migration_resumes_after_last_committed_step() {
    let manifest = load_manifest(1);
    let temp = fixture_copy(1);
    let db = temp.0.join("history.db");
    let mut c = writable(&db).unwrap();
    let failing_steps = with_extra(&[Step { apply: probe }, Step { apply: failing }]);
    assert_eq!(
        migrate(&mut c, &failing_steps, &mut |c, from| {
            migrations::backup(c, &temp.0, from).map(drop)
        }),
        Err(Error::Invalid("injected_migration_failure"))
    );
    assert_eq!(user_version(&c).unwrap(), VERSION + 1);
    drop(c);

    // 重新打开，已提交的建表步骤不能再执行，失败步骤的改动不能残留。
    let mut c = writable(&db).unwrap();
    assert!(!table_exists(&c, "migration_failed"));
    let successful_steps = with_extra(&[Step { apply: probe }, Step { apply: probe_again }]);
    assert_eq!(
        migrate(&mut c, &successful_steps, &mut |c, from| {
            assert_eq!(from, VERSION + 1);
            migrations::backup(c, &temp.0, from).map(drop)
        })
        .unwrap(),
        Some(VERSION + 1)
    );
    assert_eq!(user_version(&c).unwrap(), VERSION + 2);
    let rows: Vec<i64> = c
        .prepare("SELECT step FROM migration_probe ORDER BY step")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(rows, [1, 2]);
    schema::pragma_check(&c, "integrity_check").unwrap();
    let copies = backups(&temp.0);
    assert_eq!(copies.len(), 2);
    assert_valid_backup(&copies[0], 1, &manifest);
    assert_valid_backup(&copies[1], VERSION + 1, &manifest);
    let resumed_backup =
        Connection::open_with_flags(&copies[1], OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert!(table_exists(&resumed_backup, "migration_probe"));
    assert!(!table_exists(&resumed_backup, "migration_failed"));
    drop(resumed_backup);

    // 探针只扩展测试结构；按旧结构的完整清单复核内容和全部搜索结果。
    c.pragma_update(None, "user_version", VERSION).unwrap();
    drop(c);
    assert_matches(&temp.0, &manifest);
    check(&temp.0).unwrap();
}

#[test]
fn real_backup_directory_failure_prevents_migration_without_changing_data() {
    let manifest = load_manifest(1);
    let temp = fixture_copy(1);
    let db = temp.0.join("history.db");
    let before = fs::read(&db).unwrap();
    let blocked = temp.0.join("backups");
    fs::write(&blocked, b"existing-file").unwrap();
    let mut c = writable(&db).unwrap();
    let steps = with_extra(&[Step { apply: probe }]);
    let result = migrate(&mut c, &steps, &mut |c, from| {
        migrations::backup(c, &temp.0, from).map(drop)
    });
    assert!(matches!(result, Err(Error::Io(_))));
    assert_eq!(user_version(&c).unwrap(), 1);
    assert!(!table_exists(&c, "migration_probe"));
    schema::pragma_check(&c, "integrity_check").unwrap();
    drop(c);
    assert_eq!(fs::read(&db).unwrap(), before);
    assert_eq!(fs::read(&blocked).unwrap(), b"existing-file");
    // 备份目录仍被占着，但库本身没动：清掉占位文件后按正常路径打开、迁移、比对。
    fs::remove_file(blocked).unwrap();
    drop(Store::open(&temp.0, sha256).unwrap());
    assert_matches(&temp.0, &manifest);
    check(&temp.0).unwrap();
}

#[test]
fn backup_is_a_verified_copy_including_wal_content() {
    let manifest = load_manifest(1);
    let temp = fixture_copy(1);
    let db = temp.0.join("history.db");
    // 开着库再写一条，让 WAL 里有尚未检查点的内容；备份必须包含它。
    let store = Store::open(&temp.0, sha256).unwrap();
    let origin = Instant::now();
    store
        .ingest(text("备份前最后写入"), event(98, origin), false, origin)
        .unwrap();
    assert!(fs::metadata(temp.0.join("history.db-wal")).unwrap().len() > 0);
    let c = writable(&db).unwrap();
    let file = migrations::backup(&c, &temp.0, VERSION).unwrap();
    drop(c);
    drop(store);
    assert_eq!(file.parent().unwrap(), temp.0.join("backups"));
    assert!(!file.with_extension("pending").exists());
    let mut wal = file.as_os_str().to_os_string();
    wal.push("-wal");
    assert!(!PathBuf::from(wal).exists());
    let copy = Connection::open_with_flags(&file, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(user_version(&copy).unwrap(), VERSION);
    schema::pragma_check(&copy, "integrity_check").unwrap();
    let contents: usize = copy
        .query_row("SELECT count(*) FROM contents", [], |r| {
            schema::number(r, 0)
        })
        .unwrap();
    assert_eq!(contents, manifest.counts.contents + 1);
    // 版本对不上的副本会被删掉并报错。
    let c = writable(&db).unwrap();
    assert_eq!(
        migrations::backup(&c, &temp.0, VERSION + 7),
        Err(Error::Invalid("backup_version"))
    );
    // 打开夹具时的 v1→v2 迁移留下一份 v1 备份，手动备份是第二份（最新）；失败副本不落盘。
    let copies = backups(&temp.0);
    assert_eq!(copies.len(), 2);
    assert_eq!(copies[1], file);
    assert!(
        copies[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("pre-migrate-v1-")
    );
}

#[test]
fn newer_schema_is_refused_without_touching_file() {
    let temp = fixture_copy(1);
    let db = temp.0.join("history.db");
    let c = writable(&db).unwrap();
    c.execute_batch("PRAGMA user_version=99;").unwrap();
    drop(c);
    let before = fs::read(&db).unwrap();
    assert_eq!(
        Store::open(&temp.0, sha256).err(),
        Some(Error::NewerSchema {
            found: 99,
            supported: VERSION
        })
    );
    assert!(matches!(
        Reader::open(&temp.0, sha256),
        Err(Error::NewerSchema { found: 99, .. })
    ));
    assert_eq!(fs::read(&db).unwrap(), before);
    assert!(!temp.0.join("backups").exists());
}

#[test]
fn fresh_database_starts_at_latest_without_backup() {
    let temp = Temp::new();
    let store = temp.store();
    assert_eq!(store.report().version, VERSION);
    assert_eq!(store.report().migrated_from, None);
    let c = store.reader().unwrap();
    assert_eq!(user_version(&c.connection).unwrap(), VERSION);
    assert!(!temp.0.join("backups").exists());
}

// 重建夹具：只在夹具丢失或损坏时，在该版本仍是最新的提交上运行
//   cargo test -p nianlai-store rebuild_v1_fixture -- --ignored
// 生成 tests/fixtures/v<版本>.db 和 v<版本>.json。以后出了 v2，就照这个加 rebuild_v2_fixture → rebuild_fixture(2)。
fn rebuild_fixture(version: u32) {
    assert_eq!(version, VERSION, "旧版夹具要在该版本仍是最新的提交上重建");
    let temp = Temp::new();
    let store = temp.store();
    populate(&store, Instant::now());
    drop(store);
    let manifest = manifest_of(&temp.0);
    assert_eq!(manifest.version, version);
    fs::create_dir_all(fixtures()).unwrap();
    let target = fixtures().join(format!("v{version}.db"));
    let _ = fs::remove_file(&target);
    let c = schema::readonly(&temp.0.join("history.db")).unwrap();
    snapshot::copy_database(&c, &target).unwrap();
    drop(c);
    // 夹具是独立文件，不带 -wal / -shm；打开时由 Store 再切回 WAL。
    let c = writable(&target).unwrap();
    c.execute_batch("PRAGMA journal_mode=DELETE;").unwrap();
    drop(c);
    assert!(
        fs::metadata(&target).unwrap().len() < 128 * 1024,
        "夹具要保持在 128 KB 以内"
    );
    fs::write(
        fixtures().join(format!("v{version}.json")),
        serde_json::to_string_pretty(&manifest).unwrap() + "\n",
    )
    .unwrap();
    // 复核：刚生成的夹具按正常路径能打开，并与清单一致。
    let verify = fixture_copy(version);
    drop(Store::open(&verify.0, sha256).unwrap());
    assert_matches(&verify.0, &manifest);
    validate_snapshot(&target).unwrap();
}
#[test]
#[ignore = "重建夹具才跑，见 rebuild_fixture 的说明"]
fn rebuild_v1_fixture() {
    rebuild_fixture(1);
}
#[test]
#[ignore = "重建夹具才跑，见 rebuild_fixture 的说明"]
fn rebuild_v2_fixture() {
    rebuild_fixture(2);
}

// v1 夹具里出现过的来源进程名（populate 的 sources）；回填的 source_apps 应当恰好是这些。
const BACKFILLED_APPS: &[&str] = &["Code.exe", "chrome.exe", "explorer.exe", "notepad.exe"];

#[test]
fn v2_migration_backfills_source_apps_and_keeps_content_and_search() {
    let manifest = load_manifest(1);
    let temp = fixture_copy(1);
    let store = Store::open(&temp.0, sha256).unwrap();
    assert_eq!(store.report().version, VERSION);
    assert_eq!(store.report().migrated_from, Some(1));
    // 记录、搜索结果与清单一致；收藏表为空。
    assert_matches(&temp.0, &manifest);
    check(&temp.0).unwrap();
    // 来源应用从旧的 source_process 回填：每个进程名一行，路径为空、显示名先等于进程名。
    let c =
        Connection::open_with_flags(temp.0.join("history.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let apps: Vec<(String, String, String)> = c
        .prepare("SELECT process_name,exe_path,display_name FROM source_apps ORDER BY process_name")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let mut expected: Vec<_> = BACKFILLED_APPS
        .iter()
        .map(|n| ((*n).to_owned(), String::new(), (*n).to_owned()))
        .collect();
    expected.sort();
    assert_eq!(apps, expected);
    // 事件全部指向回填行；列表的最近来源引用也补齐了。
    let unlinked: usize = c
        .query_row(
            "SELECT count(*) FROM copy_events WHERE source_process != '' \
             AND source_app_id IS NULL",
            [],
            |r| schema::number(r, 0),
        )
        .unwrap();
    assert_eq!(unlinked, 0);
    let listed: usize = c
        .query_row(
            "SELECT count(*) FROM contents WHERE source_app_id IS NOT NULL",
            [],
            |r| schema::number(r, 0),
        )
        .unwrap();
    assert!(listed > 0);
    // 升级后的新复制给同一个应用复用回填行之外新建带路径的行（路径不同算不同来源应用）。
    drop(c);
    let origin = Instant::now();
    let mut event = event(100, origin);
    event.source_process = "notepad.exe".into();
    event.source_path = r"C:\Windows\System32\notepad.exe".into();
    store
        .ingest(text("升级后带路径的新复制"), event, false, origin)
        .unwrap();
    let reader = store.reader().unwrap();
    assert_eq!(
        reader.counts().unwrap().source_apps,
        BACKFILLED_APPS.len() + 1
    );
    let item = reader
        .search("升级后带路径")
        .unwrap()
        .items
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(item.source_name.as_deref(), Some("notepad"));
    assert!(!item.favorite);
    assert_eq!(item.favorite_position, None);
}

// v2 中途失败：先建了 favorites 再失败，事务回滚后库仍是完整的 v1，文件完好。
#[test]
fn failing_v2_step_keeps_database_at_v1() {
    let manifest = load_manifest(1);
    let temp = fixture_copy(1);
    let db = temp.0.join("history.db");
    let mut c = writable(&db).unwrap();
    let failing_v2 = |tx: &Transaction<'_>| -> Result<()> {
        tx.execute_batch("CREATE TABLE favorites(x);")?;
        Err(Error::Invalid("injected_v2_failure"))
    };
    let steps = vec![STEPS[0], Step { apply: failing_v2 }];
    let directory = temp.0.clone();
    assert_eq!(
        migrate(&mut c, &steps, &mut |c, from| {
            migrations::backup(c, &directory, from).map(drop)
        }),
        Err(Error::Invalid("injected_v2_failure"))
    );
    assert_eq!(user_version(&c).unwrap(), 1);
    assert!(!table_exists(&c, "source_apps"));
    assert!(!table_exists(&c, "favorites"));
    let columns: Vec<String> = c
        .prepare("PRAGMA table_info(copy_events)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(!columns.contains(&"source_app_id".to_owned()));
    schema::pragma_check(&c, "integrity_check").unwrap();
    drop(c);
    let backups = backups(&temp.0);
    assert_eq!(backups.len(), 1);
    assert_valid_backup(&backups[0], 1, &manifest);
    // 修好步骤后重新打开：正常升到最新，内容与清单一致。
    drop(Store::open(&temp.0, sha256).unwrap());
    assert_matches(&temp.0, &manifest);
}

// v2 夹具（0.1.0-alpha.3 发布时的结构，带来源应用）升到最新：记录、搜索、来源应用不变，回收站为空且可用。
#[test]
fn fixture_v2_opens_with_latest_code_and_trash_works() {
    let manifest = load_manifest(2);
    assert_eq!(manifest.version, 2);
    let temp = fixture_copy(2);
    let c =
        Connection::open_with_flags(temp.0.join("history.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    assert_eq!(schema_rows(&c), manifest.schema);
    drop(c);
    let store = Store::open(&temp.0, sha256).unwrap();
    assert_eq!(store.report().version, VERSION);
    assert_eq!(store.report().migrated_from, (VERSION > 2).then_some(2));
    assert_eq!(store.cache().total, manifest.counts.contents);
    let actual = manifest_of(&temp.0);
    assert_eq!(actual.counts, manifest.counts);
    assert_eq!(actual.records, manifest.records);
    assert_eq!(actual.searches, manifest.searches);
    check(&temp.0).unwrap();
    let backups = backups(&temp.0);
    assert_eq!(backups.len(), usize::from(VERSION > 2));
    if VERSION > 2 {
        assert_valid_backup(&backups[0], 2, &manifest);
    }
    let id = store.cache().items[0].id;
    assert!(store.delete(id).unwrap());
    assert_eq!(store.cache().total, manifest.counts.contents - 1);
    assert_eq!(store.reader().unwrap().trash().unwrap().len(), 1);
    assert!(store.restore(id).unwrap().is_some());
    check(&temp.0).unwrap();
}
