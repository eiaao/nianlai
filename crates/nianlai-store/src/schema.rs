use crate::{
    content::{Content, Data, Hash},
    *,
};
use rusqlite::{OptionalExtension, Row};
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
    c.prepare("SELECT id,fingerprint,kind,preview,file_names,bytes,partial,sort_ms,source,copy_count,sort_order,search_text FROM contents LIMIT 0")?;
    c.prepare("SELECT content_id,name,fingerprint,bytes,encoding,data FROM formats LIMIT 0")?;
    c.prepare("SELECT id,content_id,time_ms,source_process,owner_process,owner_pid,sequence FROM copy_events LIMIT 0")?;
    let triggers: i64 = c.query_row("SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND name IN ('content_insert','content_delete','content_search_update')", [], |r|r.get(0))?;
    if triggers != 3 {
        return Err(Error::Invalid("missing_index_triggers"));
    }
    c.prepare("SELECT id FROM contents INDEXED BY content_order LIMIT 0")?;
    Ok(())
}

pub const COLUMNS: &str = "c.id,c.fingerprint,c.kind,c.preview,c.file_names,c.bytes,c.partial,c.sort_ms,c.source,c.copy_count,c.sort_order";
pub fn metadata_row(r: &Row<'_>) -> rusqlite::Result<Metadata> {
    let fingerprint: Vec<u8> = r.get(1)?;
    let names: String = r.get(4)?;
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
        &format!("SELECT {COLUMNS} FROM contents c WHERE id=?1"),
        [id as i64],
        metadata_row,
    )?;
    formats_meta(c, &mut item)?;
    Ok(item)
}
pub fn recent(c: &Connection) -> Result<Vec<Metadata>> {
    let mut items = c
        .prepare(&format!(
            "SELECT {COLUMNS} FROM contents c ORDER BY sort_ms DESC,sort_order DESC LIMIT 300"
        ))?
        .query_map([], metadata_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for item in &mut items {
        formats_meta(c, item)?;
    }
    Ok(items)
}
pub fn counts(c: &Connection) -> Result<Counts> {
    Ok(Counts {
        contents: c.query_row("SELECT count(*) FROM contents", [], |r| number(r, 0))?,
        formats: c.query_row("SELECT count(*) FROM formats", [], |r| number(r, 0))?,
        events: c.query_row("SELECT count(*) FROM copy_events", [], |r| number(r, 0))?,
        indexed: c.query_row("SELECT count(*) FROM search_index_docsize", [], |r| {
            number(r, 0)
        })?,
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
