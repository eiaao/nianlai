//! 数据库版本与迁移框架。
//!
//! 版本号记在 SQLite 的 `PRAGMA user_version`：0 表示还没建表的新文件，n 表示已经执行完 `STEPS[..n]`。
//! `STEPS` 是有序的迁移步骤：第 n 项（从 1 数）把库从版本 n-1 升到 n。每一步在一个 `BEGIN IMMEDIATE`
//! 事务里执行，`user_version=n` 写在同一个事务里，所以一步要么整体生效、要么完全不留痕迹；
//! 中途失败就停在最后一个完整执行的版本（每个版本都是自洽的结构），下次打开再续。
//!
//! `Store::open` 打开库时经 `schema::initialize` 调用 `migrate`：
//! - user_version == 0 且没有任何表 → 从第一步开始全部执行（相当于直接建到最新）；有表却没版本号的库拒绝打开。
//! - 0 < user_version < VERSION → 先把库复制到 `<数据目录>\backups\pre-migrate-v<from>-<时间>.db`
//!   （`backup`：用 VACUUM INTO 读一致快照，WAL 里已提交的内容一并带上，再复核副本；复制或复核失败就不迁移），
//!   然后逐步补上缺的步骤。
//! - user_version == VERSION → 什么都不做。
//! - user_version > VERSION → `Error::NewerSchema { found, supported }`，不改库。
//!
//! 将来怎么加一步（以 v3 为例）：
//! 1. 写 `fn v3(tx: &Transaction<'_>) -> Result<()>`，里面只放 SQL（DDL / DML）；不要 COMMIT、不要改 journal_mode、
//!    不要写 user_version（框架负责），也不要调用按“最新结构”读写的业务代码——此刻库还是旧结构。
//! 2. 在 `STEPS` 末尾追加 `Step { apply: v3 }`，`VERSION` 自动变成 3。已经发布过的步骤不能再改，
//!    改结构只能追加新步骤；`schema::structure` 补上新结构的检查，`snapshot::relational_check` 有新不变量也补上。
//! 3. 测试（见 upgrade_tests.rs）：`fixture_v1_opens_with_latest_code_and_matches_manifest` 会自动从
//!    `tests/fixtures/v1.db` 升到 VERSION 并比对 v1.json，不用改；再加一条针对 v3 的注入测试——
//!    用 `migrate` 传入“v3 中途失败”的步骤，断言库仍停在 v2、文件完好。
//! 4. v3 发布后，在 VERSION 仍为 3 的提交上跑 `rebuild_fixture(3)`（照 `rebuild_v1_fixture` 加一个 ignored 测试）
//!    生成 `v3.db` / `v3.json`，给以后的升级测试用；旧夹具代表旧版本写出的库，不要重建。
//!    （v2 夹具同理：v2 定稿发布后跑 `rebuild_fixture(2)`，当前还没有生成。）

use crate::*;
use rusqlite::{Transaction, TransactionBehavior};
use std::{fs, path::PathBuf};

// 一步迁移就是一个函数；函数名 v<N> 即目标版本，说明写在函数上方的注释里。
#[derive(Clone, Copy)]
pub struct Step {
    pub apply: fn(&Transaction<'_>) -> Result<()>,
}

pub const STEPS: &[Step] = &[Step { apply: v1 }, Step { apply: v2 }, Step { apply: v3 }];
pub const VERSION: u32 = STEPS.len() as u32;

pub fn user_version(c: &Connection) -> Result<u32> {
    Ok(c.pragma_query_value(None, "user_version", |r| r.get(0))?)
}

/// 按 `steps` 把库升到最新；返回迁移前的版本，新建的库或已是最新时为 None。
/// `backup` 在第一步之前执行（新建的库不备份），返回错误就整个放弃，库不会被改动。
pub fn migrate(
    c: &mut Connection,
    steps: &[Step],
    backup: &mut dyn FnMut(&Connection, u32) -> Result<()>,
) -> Result<Option<u32>> {
    let latest = steps.len() as u32;
    let current = user_version(c)?;
    if current > latest {
        return Err(Error::NewerSchema {
            found: current,
            supported: latest,
        });
    }
    if current == latest {
        return Ok(None);
    }
    if current == 0 {
        // 不把未知的无版本数据库当作空库升级。
        let tables: i64 = c.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
            [],
            |r| r.get(0),
        )?;
        if tables != 0 {
            return Err(Error::Invalid("unversioned_database"));
        }
    } else {
        backup(c, current)?;
    }
    for (index, step) in steps.iter().enumerate().skip(current as usize) {
        let version = index as u32 + 1;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        (step.apply)(&tx)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
    }
    Ok((current > 0).then_some(current))
}

/// 迁移前的备份：`<数据目录>\backups\pre-migrate-v<from>-<时间>.db`。
/// 副本来自 VACUUM INTO 的一致快照，写完 fsync 再改名；随后复核版本号和 quick_check，不通过就删掉副本并报错。
pub fn backup(c: &Connection, directory: &Path, from: u32) -> Result<PathBuf> {
    let backups = directory.join("backups");
    fs::create_dir_all(&backups)?;
    let file = backups.join(format!("pre-migrate-v{from}-{}.db", snapshot::stamp()));
    snapshot::copy_database(c, &file)?;
    let verified = (|| -> Result<()> {
        let copy = Connection::open_with_flags(&file, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        if user_version(&copy)? != from {
            return Err(Error::Invalid("backup_version"));
        }
        schema::pragma_check(&copy, "quick_check")
    })();
    if let Err(error) = verified {
        let _ = fs::remove_file(&file);
        return Err(error);
    }
    Ok(file)
}

// v1：G0 样机四的结构。已经发布，不要再改；改结构请追加新步骤。
fn v1(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE contents (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                fingerprint BLOB NOT NULL UNIQUE CHECK(length(fingerprint)=32),
                kind TEXT NOT NULL, preview TEXT NOT NULL, file_names TEXT NOT NULL,
                bytes INTEGER NOT NULL CHECK(bytes>=0), partial INTEGER NOT NULL,
                sort_ms INTEGER NOT NULL, source TEXT NOT NULL, copy_count INTEGER NOT NULL,
                sort_order INTEGER NOT NULL DEFAULT 0,
                search_text TEXT NOT NULL);
            CREATE INDEX content_order ON contents(sort_ms DESC,sort_order DESC);
            CREATE INDEX content_revision ON contents(sort_order DESC);
            CREATE TABLE formats (
                content_id INTEGER NOT NULL REFERENCES contents(id) ON DELETE CASCADE,
                name TEXT NOT NULL, fingerprint BLOB NOT NULL CHECK(length(fingerprint)=32),
                bytes INTEGER NOT NULL CHECK(bytes>=0), encoding INTEGER NOT NULL CHECK(encoding IN (0,1)),
                data BLOB NOT NULL, PRIMARY KEY(content_id,name));
            CREATE TABLE copy_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                content_id INTEGER NOT NULL REFERENCES contents(id) ON DELETE CASCADE,
                time_ms INTEGER NOT NULL, source_process TEXT NOT NULL, owner_process TEXT NOT NULL,
                owner_pid INTEGER NOT NULL, sequence INTEGER NOT NULL);
            CREATE INDEX event_content ON copy_events(content_id,time_ms,id);
            CREATE VIRTUAL TABLE search_index USING fts5(search_text, content='contents', content_rowid='id', tokenize='trigram');
            CREATE TRIGGER content_insert AFTER INSERT ON contents BEGIN
                INSERT INTO search_index(rowid,search_text) VALUES(new.id,new.search_text);
            END;
            CREATE TRIGGER content_delete AFTER DELETE ON contents BEGIN
                INSERT INTO search_index(search_index,rowid,search_text) VALUES('delete',old.id,old.search_text);
            END;
            CREATE TRIGGER content_search_update AFTER UPDATE OF search_text ON contents BEGIN
                INSERT INTO search_index(search_index,rowid,search_text) VALUES('delete',old.id,old.search_text);
                INSERT INTO search_index(rowid,search_text) VALUES(new.id,new.search_text);
            END;",
    )?;
    Ok(())
}

// v2（M1-S）：来源应用（A06）与收藏（E01/B03）。
// - source_apps：来源进程的程序信息。进程名 + exe 路径决定一行（拿不到路径时路径为空串），
//   显示名先落一个退回值（文件名去扩展名或进程名），采集线程之外的后台刷新再改成
//   版本信息里的 FileDescription 并补图标；icon_key 由路径和文件修改时间决定，exe 升级后变化。
// - favorites：收藏引用 contents 行，position 是稳定顺序（新收藏放最后，永不重排）；
//   外键保证物理删除被收藏的内容会失败——B03“删除历史不影响收藏”的数据库层兜底。
// - copy_events.source_app_id / contents.source_app_id：事件级来源应用引用；
//   contents 上的是最近一次复制的事件引用，列表取它，避免按行子查询。
// - 回填：v1 的 copy_events.source_process 只有进程名，按它建 source_apps 行
//   （display_name 先用进程名，等该应用下次被复制时后台刷新修正）。
fn v2(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE source_apps (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                process_name TEXT NOT NULL, exe_path TEXT NOT NULL,
                display_name TEXT NOT NULL, icon_file TEXT NOT NULL, icon_key TEXT NOT NULL,
                updated_ms INTEGER NOT NULL,
                UNIQUE(process_name, exe_path));
            CREATE INDEX source_app_path ON source_apps(exe_path);
            CREATE TABLE favorites (
                content_id INTEGER PRIMARY KEY REFERENCES contents(id),
                position INTEGER NOT NULL UNIQUE,
                title TEXT NOT NULL DEFAULT '', note TEXT NOT NULL DEFAULT '',
                created_ms INTEGER NOT NULL, updated_ms INTEGER NOT NULL);
            ALTER TABLE copy_events ADD COLUMN source_app_id INTEGER REFERENCES source_apps(id);
            ALTER TABLE contents ADD COLUMN source_app_id INTEGER REFERENCES source_apps(id);
            INSERT INTO source_apps(process_name,exe_path,display_name,icon_file,icon_key,updated_ms)
                SELECT DISTINCT source_process,'',source_process,'','',
                    CAST(strftime('%s','now') AS INTEGER)*1000
                FROM copy_events WHERE source_process != '';
            UPDATE copy_events SET source_app_id=(
                SELECT a.id FROM source_apps a
                WHERE a.process_name=copy_events.source_process AND a.exe_path='')
                WHERE source_process != '';
            UPDATE contents SET source_app_id=(
                SELECT e.source_app_id FROM copy_events e
                WHERE e.content_id=contents.id AND e.source_app_id IS NOT NULL
                ORDER BY e.time_ms DESC,e.id DESC LIMIT 1);",
    )?;
    Ok(())
}

// v3（M2-5）：回收站（B12）。删除先在 trash 里登记，保留期满或手动清空时才物理删除；
// 内容行物理删除时 trash 行随之级联删除。收藏的内容也能进回收站：收藏行保留，恢复后仍是收藏，
// 物理删除前由写入方先删收藏行（favorites 外键不级联）。
fn v3(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE trash (
                content_id INTEGER PRIMARY KEY REFERENCES contents(id) ON DELETE CASCADE,
                deleted_ms INTEGER NOT NULL);
            CREATE INDEX trash_time ON trash(deleted_ms);",
    )?;
    Ok(())
}
