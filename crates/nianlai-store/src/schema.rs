use crate::{
    content::{Content, Data, Hash},
    *,
};
use rusqlite::{OptionalExtension, Row, params};
use std::{collections::BTreeMap, sync::Arc};

pub fn version(c: &Connection) -> Result<u32> {
    let version = migrations::user_version(c)?;
    if version > VERSION {
        return Err(Error::NewerSchema {
            found: version,
            supported: VERSION,
        });
    }
    Ok(version)
}
pub fn readonly(path: &Path) -> Result<Connection> {
    let c = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    c.busy_timeout(std::time::Duration::from_secs(5))?;
    if version(&c)? != VERSION {
        return Err(Error::Invalid("schema_version"));
    }
    Ok(c)
}
/// 只读打开任意不高于当前版本的库（快照校验用）；版本比当前新（NewerSchema）或为 0 都拒绝。
pub fn readonly_any(path: &Path) -> Result<Connection> {
    let c = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    c.busy_timeout(std::time::Duration::from_secs(5))?;
    if version(&c)? == 0 {
        return Err(Error::Invalid("schema_version"));
    }
    Ok(c)
}
// 建表和升级都在 migrations 里；这里只负责“打开一个可写库”的固定流程。返回迁移前的版本（没迁移为 None）。
pub fn initialize(c: &mut Connection, directory: &Path) -> Result<Option<u32>> {
    let migrated_from = migrations::migrate(c, migrations::STEPS, &mut |c, from| {
        migrations::backup(c, directory, from).map(drop)
    })?;
    structure(c)?;
    c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")?;
    Ok(migrated_from)
}

pub fn structure(c: &Connection) -> Result<()> {
    c.prepare("SELECT id,fingerprint,kind,preview,file_names,bytes,partial,sort_ms,source,copy_count,sort_order,source_app_id FROM contents LIMIT 0")?;
    c.prepare("SELECT content_id,name,fingerprint,bytes,encoding,data FROM formats LIMIT 0")?;
    c.prepare("SELECT id,content_id,time_ms,source_process,owner_process,owner_pid,sequence,source_app_id FROM copy_events LIMIT 0")?;
    c.prepare("SELECT id,process_name,exe_path,display_name,icon_file,icon_key,updated_ms FROM source_apps LIMIT 0")?;
    c.prepare(
        "SELECT content_id,position,title,note,created_ms,updated_ms FROM favorites LIMIT 0",
    )?;
    c.prepare("SELECT content_id,deleted_ms FROM trash LIMIT 0")?;
    let triggers: i64 = c.query_row("SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND name IN ('content_insert','content_delete','content_search_update')", [], |r|r.get(0))?;
    if triggers != 3 {
        return Err(Error::Invalid("missing_index_triggers"));
    }
    c.prepare("SELECT id FROM contents INDEXED BY content_order LIMIT 0")?;
    Ok(())
}

// 列表项统一带收藏状态和来源应用信息（A06/E01）；三个 JOIN 都是主键点查，不改变排序索引的驱动方式。
pub const COLUMNS: &str = "c.id,c.fingerprint,c.kind,c.preview,c.file_names,c.bytes,c.partial,c.sort_ms,c.source,c.copy_count,c.sort_order,fv.position,sa.display_name,sa.icon_file,fv.title,fv.note";
/// 不在回收站里的内容（B12）。列表、收藏、搜索都只看这些。
pub const LIVE: &str = "NOT EXISTS(SELECT 1 FROM trash t WHERE t.content_id=c.id)";
pub const JOINS: &str = "FROM contents c LEFT JOIN favorites fv ON fv.content_id=c.id LEFT JOIN source_apps sa ON sa.id=c.source_app_id";
pub fn metadata_row(r: &Row<'_>) -> rusqlite::Result<Metadata> {
    let fingerprint: Vec<u8> = r.get(1)?;
    let names: String = r.get(4)?;
    let favorite_position: Option<i64> = r.get(11)?;
    Ok(Metadata {
        id: number(r, 0)?,
        fingerprint: fingerprint
            .try_into()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        kind: r.get(2)?,
        preview: r.get(3)?,
        file_names: serde_json::from_str(&names).map_err(|_| rusqlite::Error::InvalidQuery)?,
        bytes: number(r, 5)?,
        partial: r.get(6)?,
        copied_ms: r.get::<_, i64>(7)? as u64,
        source: r.get(8)?,
        copy_count: number(r, 9)?,
        favorite: favorite_position.is_some(),
        favorite_position: favorite_position.map(|v| v as u64),
        source_name: r.get(12)?,
        icon_file: r.get(13)?,
        title: r.get::<_, Option<String>>(14)?.unwrap_or_default(),
        note: r.get::<_, Option<String>>(15)?.unwrap_or_default(),
        sort_order: r.get(10)?,
        formats: Vec::new(),
    })
}
pub fn formats_meta(c: &Connection, item: &mut Metadata) -> Result<()> {
    item.formats = c
        .prepare_cached(
            "SELECT name,fingerprint,bytes FROM formats WHERE content_id=?1 ORDER BY name",
        )?
        .query_map([item.id as i64], |r| {
            let fingerprint: Vec<u8> = r.get(1)?;
            Ok(FormatMeta {
                name: r.get(0)?,
                fingerprint: fingerprint
                    .try_into()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                bytes: number(r, 2)?,
            })
        })?
        .collect::<std::result::Result<_, _>>()?;
    Ok(())
}
pub fn metadata(c: &Connection, id: usize) -> Result<Metadata> {
    let mut item = c.query_row(
        &format!("SELECT {COLUMNS} {JOINS} WHERE c.id=?1"),
        [id as i64],
        metadata_row,
    )?;
    formats_meta(c, &mut item)?;
    Ok(item)
}
pub fn recent(c: &Connection) -> Result<Vec<Metadata>> {
    let mut items = c
        .prepare(&format!(
            "SELECT {COLUMNS} {JOINS} WHERE {LIVE} ORDER BY c.sort_ms DESC,c.sort_order DESC LIMIT 300"
        ))?
        .query_map([], metadata_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for item in &mut items {
        formats_meta(c, item)?;
    }
    Ok(items)
}
// 收藏列表：按收藏顺序（position）返回；content 仍在 contents 排序之外单独取。
pub fn favorites(c: &Connection) -> Result<Vec<Metadata>> {
    let mut items = c
        .prepare(&format!(
            "SELECT {COLUMNS} FROM contents c JOIN favorites fv ON fv.content_id=c.id \
             LEFT JOIN source_apps sa ON sa.id=c.source_app_id WHERE {LIVE} ORDER BY fv.position ASC LIMIT 300"
        ))?
        .query_map([], metadata_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for item in &mut items {
        formats_meta(c, item)?;
    }
    Ok(items)
}
/// 这条内容是否没有被收藏。同指纹合并时保留被收藏的那行（B03）。
/// 收藏引用由外键兜底——物理删除被收藏的内容会在数据库层失败，必须先删收藏行。
pub fn deletable(c: &Connection, id: usize) -> Result<bool> {
    let favorited: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM favorites WHERE content_id=?1)",
        [id as i64],
        |r| r.get(0),
    )?;
    Ok(!favorited)
}
/// 回收站里的内容，最近删除的在前；第二项是删除时间。
pub fn trash(c: &Connection) -> Result<Vec<(Metadata, u64)>> {
    let mut items = c
        .prepare(&format!(
            "SELECT {COLUMNS},t.deleted_ms {JOINS} JOIN trash t ON t.content_id=c.id \
             ORDER BY t.deleted_ms DESC,c.id DESC LIMIT 1000"
        ))?
        .query_map([], |r| Ok((metadata_row(r)?, r.get::<_, i64>(16)? as u64)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (item, _) in &mut items {
        formats_meta(c, item)?;
    }
    Ok(items)
}
/// 文件类内容的路径列表都存在 encoding=1 的格式里（JSON 数组）。
pub fn file_lists(c: &Connection) -> Result<Vec<FileList>> {
    let mut lists: Vec<FileList> = Vec::new();
    let mut statement = c.prepare(&format!(
        "SELECT f.content_id,fv.content_id IS NOT NULL,f.data FROM formats f \
         JOIN contents c ON c.id=f.content_id LEFT JOIN favorites fv ON fv.content_id=c.id \
         WHERE f.encoding=1 AND {LIVE} ORDER BY f.content_id"
    ))?;
    let rows = statement.query_map([], |r| {
        Ok((number(r, 0)?, r.get::<_, bool>(1)?, r.get::<_, Vec<u8>>(2)?))
    })?;
    for row in rows {
        let (id, favorite, data) = row?;
        let paths: Vec<String> =
            serde_json::from_slice(&data).map_err(|_| Error::Invalid("file_list"))?;
        // 一条内容一般只有一个文件列表格式；有多个时合并去重。
        match lists.last_mut() {
            Some(last) if last.id == id => {
                for path in paths {
                    if !last.paths.contains(&path) {
                        last.paths.push(path);
                    }
                }
            }
            _ => lists.push(FileList {
                id,
                favorite,
                paths,
            }),
        }
    }
    Ok(lists)
}
pub fn usage(c: &Connection) -> Result<Usage> {
    let kinds = c
        .prepare(&format!(
            "SELECT c.kind,count(*),coalesce(sum(c.bytes),0) FROM contents c WHERE {LIVE} \
             GROUP BY c.kind ORDER BY 3 DESC"
        ))?
        .query_map([], |r| {
            Ok(KindUsage {
                kind: r.get(0)?,
                count: number(r, 1)?,
                bytes: r.get::<_, i64>(2)?.max(0) as u64,
            })
        })?
        .collect::<std::result::Result<_, _>>()?;
    let (trashed_count, trashed_bytes) = c.query_row(
        "SELECT count(*),coalesce(sum(c.bytes),0) FROM trash t JOIN contents c ON c.id=t.content_id",
        [],
        |r| Ok((number(r, 0)?, r.get::<_, i64>(1)?.max(0) as u64)),
    )?;
    let free: i64 = c.pragma_query_value(None, "freelist_count", |r| r.get(0))?;
    let page: i64 = c.pragma_query_value(None, "page_size", |r| r.get(0))?;
    Ok(Usage {
        kinds,
        trashed_count,
        trashed_bytes,
        reclaimable_bytes: (free.max(0) * page.max(0)) as u64,
    })
}
/// 存在库里的图片，按大小从大到小；收藏的也返回（界面上标出、不让选）。
pub fn images(c: &Connection, before_ms: Option<u64>) -> Result<Vec<Metadata>> {
    let before = before_ms.map_or(i64::MAX, |ms| ms.min(i64::MAX as u64) as i64);
    Ok(c.prepare(&format!(
        "SELECT {COLUMNS} {JOINS} WHERE {LIVE} AND c.kind=?1 AND c.sort_ms<?2 \
             ORDER BY c.bytes DESC,c.id DESC LIMIT 2000"
    ))?
    .query_map(params![IMAGE_KIND, before], metadata_row)?
    .collect::<std::result::Result<Vec<_>, _>>()?)
}
/// 图片内容的 kind（见 content.rs 的分类）。
pub const IMAGE_KIND: &str = "图片";
pub fn trashed(c: &Connection, id: usize) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM trash WHERE content_id=?1)",
        [id as i64],
        |r| r.get(0),
    )?)
}
pub fn source_app_by_path(c: &Connection, exe_path: &str) -> Result<Option<SourceApp>> {
    let app = c
        .query_row(
            "SELECT id,process_name,exe_path,display_name,icon_file,icon_key \
             FROM source_apps WHERE exe_path=?1 ORDER BY id LIMIT 1",
            [exe_path],
            |r| {
                Ok(SourceApp {
                    id: r.get(0)?,
                    process_name: r.get(1)?,
                    exe_path: r.get(2)?,
                    display_name: r.get(3)?,
                    icon_file: r.get(4)?,
                    icon_key: r.get(5)?,
                })
            },
        )
        .optional()?;
    Ok(app)
}
pub fn counts(c: &Connection) -> Result<Counts> {
    Ok(Counts {
        contents: c.query_row("SELECT count(*) FROM contents", [], |r| number(r, 0))?,
        formats: c.query_row("SELECT count(*) FROM formats", [], |r| number(r, 0))?,
        events: c.query_row("SELECT count(*) FROM copy_events", [], |r| number(r, 0))?,
        indexed: c.query_row("SELECT count(*) FROM search_index_docsize", [], |r| {
            number(r, 0)
        })?,
        favorites: c.query_row("SELECT count(*) FROM favorites", [], |r| number(r, 0))?,
        source_apps: c.query_row("SELECT count(*) FROM source_apps", [], |r| number(r, 0))?,
        trashed: c.query_row("SELECT count(*) FROM trash", [], |r| number(r, 0))?,
    })
}
pub fn content(c: &Connection, id: usize, hash: Hash) -> Result<Option<Arc<Content>>> {
    let fingerprint: Option<Vec<u8>> = c
        .query_row(
            "SELECT fingerprint FROM contents WHERE id=?1",
            [id as i64],
            |r| r.get(0),
        )
        .optional()?;
    let Some(fingerprint) = fingerprint else {
        return Ok(None);
    };
    let mut statement = c.prepare_cached(
        "SELECT name,encoding,data FROM formats WHERE content_id=?1 ORDER BY name",
    )?;
    let rows = statement.query_map([id as i64], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, u8>(1)?,
            r.get::<_, Vec<u8>>(2)?,
        ))
    })?;
    let mut formats = BTreeMap::new();
    for row in rows {
        let (name, encoding, bytes) = row?;
        let data = if encoding == 0 {
            Data::Bytes(bytes.into())
        } else {
            Data::Files(
                serde_json::from_slice::<Vec<String>>(&bytes)
                    .map_err(|_| Error::Invalid("file_list"))?
                    .into(),
            )
        };
        formats.insert(name, data);
    }
    if formats.is_empty() {
        return Err(Error::Invalid("missing_formats"));
    }
    let content = Content::new(formats, hash).map_err(|_| Error::Invalid("hash_failed"))?;
    if content.fingerprint.as_slice() != fingerprint {
        return Err(Error::Invalid("content_fingerprint"));
    }
    Ok(Some(content))
}
pub fn pragma_check(c: &Connection, pragma: &str) -> Result<()> {
    let mut stmt = c.prepare(&format!("PRAGMA {pragma}"))?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if rows != ["ok"] {
        return Err(Error::Invalid("integrity_check"));
    }
    Ok(())
}

pub fn number(row: &Row<'_>, index: usize) -> rusqlite::Result<usize> {
    row.get::<_, i64>(index)?
        .try_into()
        .map_err(|_| rusqlite::Error::InvalidQuery)
}
