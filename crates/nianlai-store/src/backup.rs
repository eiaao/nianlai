//! 备份导出与导入（M2-4 / B07）。
//!
//! 备份文件 `.nianlai-backup` 是一个 zip，只含三类文件：
//! - `history.db`：从现役库用 VACUUM INTO 取得的一致副本，副本里把回收站物理清掉（现役库不动）；
//! - `icons/<文件名>`：副本里 `source_apps.icon_file` 仍在引用的图标；
//! - `manifest.json`：格式版本、数量和每个文件的大小与 SHA-256。
//!
//! 导入先 `inspect`：解到 `<数据目录>\imports\<令牌>\` 并分三步校验（清单、校验值、数据库），
//! 通过后写下 `ready` 标记，之后才能 `merge`（合并进现役库）或 `prepare_overwrite`（覆盖，下次启动时执行）。
//! 校验的任何一步失败都只删除这个临时目录，不碰现有数据。

use crate::{hash::Sha256, *};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use std::{
    collections::{BTreeSet, HashMap},
    fs::{self, File},
    io::{BufReader, Read, Write},
    path::PathBuf,
};

pub const FORMAT: u32 = 1;
pub const EXTENSION: &str = "nianlai-backup";
const MANIFEST: &str = "manifest.json";
const DATABASE: &str = "history.db";
const ICONS: &str = "icons/";
const READY: &str = "ready";
/// 解压后总大小与条目数的上限，挡住压缩炸弹。
const MAX_TOTAL: u64 = 8 * 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 100_000;
const MAX_MANIFEST: u64 = 16 * 1024 * 1024;

fn imports(directory: &Path) -> PathBuf {
    directory.join("imports")
}

/// 临时目录，离开作用域就删除。
struct Work(PathBuf);
impl Work {
    fn new(directory: &Path, prefix: &str) -> Result<Self> {
        let path = imports(directory).join(format!("{prefix}{}", snapshot::stamp()));
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Work {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// 图标文件名必须是单层的普通文件名；库里的内容不可信，不能用来拼出目录外的路径。
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
}

fn valid_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 64
        && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

// ---------- 摘要 ----------

#[derive(Clone, Debug, Default, Serialize)]
pub struct Summary {
    pub items: usize,
    pub favorites: usize,
    pub icons: usize,
    /// 导出文件大小的估计：数据库页字节减去回收站内容再加图标（未计压缩）。
    pub bytes: u64,
}

const LIVE_CONTENT: &str = "NOT EXISTS(SELECT 1 FROM trash t WHERE t.content_id=c.id)";
// 回收站清掉之后仍被引用的来源应用。
const REFERENCED_APPS: &str = "SELECT c.source_app_id FROM contents c WHERE c.source_app_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM trash t WHERE t.content_id=c.id) \
     UNION SELECT e.source_app_id FROM copy_events e JOIN contents c ON c.id=e.content_id WHERE e.source_app_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM trash t WHERE t.content_id=c.id)";

/// 现役库里会被导出的图标文件名（存在于 icons 目录的、名字合法的）。
fn exported_icons(c: &Connection, icons: &Path) -> Result<Vec<String>> {
    let names: BTreeSet<String> = c
        .prepare(&format!(
            "SELECT DISTINCT icon_file FROM source_apps WHERE icon_file!='' AND id IN ({REFERENCED_APPS})"
        ))?
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    Ok(names
        .into_iter()
        .filter(|name| plain_name(name) && icons.join(name).is_file())
        .collect())
}

pub fn summary(directory: &Path) -> Result<Summary> {
    let c = schema::readonly(&directory.join(DATABASE))?;
    let tx = c.unchecked_transaction()?;
    let items = tx.query_row(
        &format!("SELECT count(*) FROM contents c WHERE {LIVE_CONTENT}"),
        [],
        |r| schema::number(r, 0),
    )?;
    let favorites = tx.query_row(
        &format!("SELECT count(*) FROM favorites fv JOIN contents c ON c.id=fv.content_id WHERE {LIVE_CONTENT}"),
        [],
        |r| schema::number(r, 0),
    )?;
    let trashed_bytes: i64 = tx.query_row(
        "SELECT coalesce(sum(c.bytes),0) FROM trash t JOIN contents c ON c.id=t.content_id",
        [],
        |r| r.get(0),
    )?;
    let pages: i64 = tx.pragma_query_value(None, "page_count", |r| r.get(0))?;
    let free: i64 = tx.pragma_query_value(None, "freelist_count", |r| r.get(0))?;
    let page: i64 = tx.pragma_query_value(None, "page_size", |r| r.get(0))?;
    let icons_directory = directory.join("icons");
    let icons = exported_icons(&tx, &icons_directory)?;
    let icon_bytes: u64 = icons
        .iter()
        .map(|name| fs::metadata(icons_directory.join(name)).map_or(0, |m| m.len()))
        .sum();
    tx.commit()?;
    let database = ((pages - free).max(0) * page.max(0) - trashed_bytes).max(0) as u64;
    Ok(Summary {
        items,
        favorites,
        icons: icons.len(),
        bytes: database + icon_bytes,
    })
}

// ---------- 导出 ----------

#[derive(Clone, Debug, Serialize)]
pub struct ExportReport {
    pub bytes: u64,
    pub items: usize,
    pub favorites: usize,
    pub icons: usize,
}

#[derive(Serialize, Deserialize)]
struct ManifestCounts {
    items: usize,
    favorites: usize,
    icons: usize,
}
#[derive(Serialize, Deserialize)]
struct ManifestFile {
    name: String,
    bytes: u64,
    sha256: String,
}
#[derive(Serialize, Deserialize)]
struct Manifest {
    format: u32,
    app: String,
    app_version: String,
    schema_version: u32,
    exported_ms: u64,
    counts: ManifestCounts,
    files: Vec<ManifestFile>,
}

fn zip_error(_: zip::result::ZipError) -> Error {
    Error::Invalid("zip")
}

// 在副本上物理删除回收站里的内容及关联行，再清掉不再被引用的来源应用，最后 VACUUM 收回空间。
fn strip_trash(path: &Path) -> Result<(usize, usize)> {
    let c = writable(path)?;
    {
        let tx = c.unchecked_transaction()?;
        tx.execute_batch(
            "DELETE FROM favorites WHERE content_id IN (SELECT content_id FROM trash);
             DELETE FROM contents WHERE id IN (SELECT content_id FROM trash);
             DELETE FROM trash;",
        )?;
        tx.execute(
            "DELETE FROM source_apps WHERE id NOT IN (SELECT source_app_id FROM contents WHERE source_app_id IS NOT NULL \
                 UNION SELECT source_app_id FROM copy_events WHERE source_app_id IS NOT NULL)",
            [],
        )?;
        tx.commit()?;
    }
    let items = c.query_row("SELECT count(*) FROM contents", [], |r| {
        schema::number(r, 0)
    })?;
    let favorites = c.query_row("SELECT count(*) FROM favorites", [], |r| {
        schema::number(r, 0)
    })?;
    c.execute_batch("VACUUM; PRAGMA journal_mode=DELETE;")?;
    Ok((items, favorites))
}

fn add_file(zip: &mut zip::ZipWriter<File>, name: &str, path: &Path) -> Result<ManifestFile> {
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);
    zip.start_file(name, options).map_err(zip_error)?;
    let mut input = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut bytes = 0u64;
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        zip.write_all(&buffer[..read])?;
        bytes += read as u64;
    }
    Ok(ManifestFile {
        name: name.to_owned(),
        bytes,
        sha256: hash::hex(&hasher.finish()),
    })
}

/// 把现役库导出成备份文件。先写 `<目标>.pending`，fsync 后改名，失败时清理暂存文件。
pub fn export(directory: &Path, destination: &Path, app_version: &str) -> Result<ExportReport> {
    let work = Work::new(directory, "export-")?;
    let database = work.0.join(DATABASE);
    {
        let source = schema::readonly(&directory.join(DATABASE))?;
        snapshot::copy_database(&source, &database)?;
    }
    let (items, favorites) = strip_trash(&database)?;
    let schema_version = {
        let c = schema::readonly(&database)?;
        schema::version(&c)?
    };
    let icons_directory = directory.join("icons");
    let icons = {
        let c = schema::readonly(&database)?;
        exported_icons(&c, &icons_directory)?
    };

    let mut stage = destination.as_os_str().to_os_string();
    stage.push(".pending");
    let stage = PathBuf::from(stage);
    let result = (|| -> Result<u64> {
        let mut zip = zip::ZipWriter::new(File::create(&stage)?);
        let mut files = vec![add_file(&mut zip, DATABASE, &database)?];
        for name in &icons {
            files.push(add_file(
                &mut zip,
                &format!("{ICONS}{name}"),
                &icons_directory.join(name),
            )?);
        }
        let manifest = Manifest {
            format: FORMAT,
            app: "nianlai".into(),
            app_version: app_version.to_owned(),
            schema_version,
            exported_ms: now_ms(),
            counts: ManifestCounts {
                items,
                favorites,
                icons: icons.len(),
            },
            files,
        };
        zip.start_file(MANIFEST, zip::write::SimpleFileOptions::default())
            .map_err(zip_error)?;
        zip.write_all(
            &serde_json::to_vec_pretty(&manifest).map_err(|_| Error::Invalid("manifest"))?,
        )?;
        let file = zip.finish().map_err(zip_error)?;
        file.sync_all()?;
        let bytes = file.metadata()?.len();
        drop(file);
        fs::rename(&stage, destination)?;
        Ok(bytes)
    })();
    match result {
        Ok(bytes) => Ok(ExportReport {
            bytes,
            items,
            favorites,
            icons: icons.len(),
        }),
        Err(error) => {
            let _ = fs::remove_file(&stage);
            Err(error)
        }
    }
}

// ---------- 导入前检查 ----------

#[derive(Clone, Debug, Serialize)]
pub struct Step {
    pub id: &'static str,
    pub ok: bool,
    pub detail: String,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct MergePreview {
    pub new_items: usize,
    pub new_favorites: usize,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct OverwritePreview {
    pub items: usize,
}
#[derive(Clone, Debug, Serialize)]
pub struct Inspection {
    pub token: String,
    pub file_name: String,
    pub exported_ms: u64,
    pub app_version: String,
    pub schema_version: u32,
    pub upgraded_from: Option<u32>,
    pub steps: Vec<Step>,
    pub ok: bool,
    pub merge: Option<MergePreview>,
    pub overwrite: Option<OverwritePreview>,
}

fn step(id: &'static str, ok: bool, detail: impl Into<String>) -> Step {
    Step {
        id,
        ok,
        detail: detail.into(),
    }
}

type Archive = zip::ZipArchive<BufReader<File>>;

// 第一步：zip 能打开、清单能解析、格式版本对、条目名只在允许的范围内。
fn read_manifest(archive: &mut Archive) -> std::result::Result<Manifest, String> {
    if archive.len() > MAX_ENTRIES {
        return Err("文件条目太多，不像是拈来的备份".into());
    }
    let mut names = BTreeSet::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let entry = archive
            .by_index_raw(index)
            .map_err(|_| "备份文件已损坏，无法读取目录")?;
        let name = entry
            .name()
            .map_err(|_| "文件名不是有效的 UTF-8，不像是拈来的备份")?
            .into_owned();
        let allowed = name == DATABASE
            || name == MANIFEST
            || name.strip_prefix(ICONS).is_some_and(plain_name);
        if entry.is_dir() || !allowed {
            return Err(format!("含有不属于备份的文件：{}", display_name(&name)));
        }
        if !names.insert(name.clone()) {
            return Err(format!("文件重复出现：{}", display_name(&name)));
        }
        total = total.saturating_add(entry.size());
    }
    if total > MAX_TOTAL {
        return Err("解压后的总大小超出限制".into());
    }
    if !names.contains(MANIFEST) {
        return Err("缺少清单 manifest.json，这不是拈来的备份文件".into());
    }
    let mut text = Vec::new();
    archive
        .by_name(MANIFEST)
        .map_err(|_| "清单无法读取")?
        .take(MAX_MANIFEST + 1)
        .read_to_end(&mut text)
        .map_err(|_| "清单无法读取")?;
    if text.len() as u64 > MAX_MANIFEST {
        return Err("清单过大".into());
    }
    let manifest: Manifest =
        serde_json::from_slice(&text).map_err(|_| "清单格式不对，这不是拈来的备份文件")?;
    if manifest.app != "nianlai" {
        return Err("这不是拈来的备份文件".into());
    }
    if manifest.format != FORMAT {
        return Err(format!(
            "备份格式版本 {} 不受支持（本版本只认识 {FORMAT}）",
            manifest.format
        ));
    }
    Ok(manifest)
}

fn display_name(name: &str) -> String {
    name.chars().filter(|c| !c.is_control()).take(80).collect()
}

// 第二步：清单里的文件逐个解出来，大小和 SHA-256 都要对上，清单与压缩包里的文件一一对应。
fn extract(
    archive: &mut Archive,
    manifest: &Manifest,
    staging: &Path,
) -> std::result::Result<(), String> {
    let mut listed = BTreeSet::new();
    let mut total = 0u64;
    for file in &manifest.files {
        let allowed =
            file.name == DATABASE || file.name.strip_prefix(ICONS).is_some_and(plain_name);
        if !allowed {
            return Err(format!(
                "清单里有不允许的文件名：{}",
                display_name(&file.name)
            ));
        }
        if !listed.insert(file.name.as_str()) {
            return Err(format!("清单里文件重复：{}", file.name));
        }
        total = total.saturating_add(file.bytes);
    }
    if !listed.contains(DATABASE) {
        return Err("清单里没有 history.db".into());
    }
    if total > MAX_TOTAL {
        return Err("清单声明的总大小超出限制".into());
    }
    let mut present = BTreeSet::new();
    for index in 0..archive.len() {
        let entry = archive
            .by_index_raw(index)
            .map_err(|_| "备份文件已损坏，无法读取目录")?;
        let name = entry.name().map_err(|_| "文件名无法识别")?.into_owned();
        if name != MANIFEST {
            present.insert(name);
        }
    }
    if let Some(missing) = listed.iter().find(|name| !present.contains(**name)) {
        return Err(format!("缺少文件：{missing}"));
    }
    if let Some(extra) = present.iter().find(|name| !listed.contains(name.as_str())) {
        return Err(format!("多出清单之外的文件：{}", display_name(extra)));
    }
    fs::create_dir_all(staging.join("icons")).map_err(|_| "无法创建临时目录")?;
    let mut buffer = vec![0u8; 64 * 1024];
    for file in &manifest.files {
        let mut entry = archive
            .by_name(&file.name)
            .map_err(|_| format!("无法读取：{}", file.name))?;
        if entry.size() != file.bytes {
            return Err(format!("{} 的大小与清单不符", file.name));
        }
        let target = staging.join(&file.name);
        let mut output = File::create(&target).map_err(|_| "无法写入临时目录")?;
        let mut hasher = Sha256::new();
        let mut copied = 0u64;
        // 最多多读一个字节：声明小、实际大的压缩包在这里就会被发现。
        let mut limited = (&mut entry).take(file.bytes + 1);
        loop {
            let read = limited
                .read(&mut buffer)
                .map_err(|_| format!("{} 解压失败，文件可能已损坏", file.name))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            output
                .write_all(&buffer[..read])
                .map_err(|_| "无法写入临时目录")?;
            copied += read as u64;
        }
        output.sync_all().map_err(|_| "无法写入临时目录")?;
        if copied != file.bytes {
            return Err(format!("{} 的大小与清单不符", file.name));
        }
        if !hash::hex(&hasher.finish()).eq_ignore_ascii_case(&file.sha256) {
            return Err(format!(
                "{} 的校验值对不上，文件可能损坏或被改动",
                file.name
            ));
        }
    }
    Ok(())
}

// 第三步：在临时区对解出来的库做完整检查；旧版本就地升级（只动临时副本）。返回升级前的版本。
fn prepare_database(path: &Path) -> Result<Option<u32>> {
    let mut c = writable(path)?;
    let found = schema::version(&c)?;
    if found == 0 {
        return Err(Error::Invalid("schema_version"));
    }
    schema::pragma_check(&c, "integrity_check")?;
    migrations::migrate(&mut c, migrations::STEPS, &mut |_, _| Ok(()))?;
    relational_check(&c)?;
    c.execute(
        "INSERT INTO search_index(search_index,rank) VALUES('integrity-check',1)",
        [],
    )?;
    c.execute_batch("PRAGMA journal_mode=DELETE;")?;
    Ok((found < VERSION).then_some(found))
}

fn relational_check(c: &Connection) -> Result<()> {
    snapshot::relational_check(c).map(drop)
}

fn live_contents(c: &Connection) -> Result<usize> {
    Ok(c.query_row(
        &format!("SELECT count(*) FROM contents c WHERE {LIVE_CONTENT}"),
        [],
        |r| schema::number(r, 0),
    )?)
}

/// 检查备份文件并解到 `<数据目录>\imports\<令牌>\`。所有问题都体现在 `steps` 里，不返回错误；
/// 没有全部通过时临时目录会被删掉。
pub fn inspect(directory: &Path, archive_path: &Path) -> Inspection {
    let token = snapshot::stamp();
    let mut inspection = Inspection {
        token: token.clone(),
        file_name: archive_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        exported_ms: 0,
        app_version: String::new(),
        schema_version: 0,
        upgraded_from: None,
        steps: Vec::new(),
        ok: false,
        merge: None,
        overwrite: None,
    };
    let staging = imports(directory).join(&token);
    let finished = inspect_steps(directory, archive_path, &staging, &mut inspection);
    inspection.ok = finished && inspection.steps.iter().all(|s| s.ok);
    if inspection.ok && fs::write(staging.join(READY), &token).is_err() {
        inspection.ok = false;
        inspection
            .steps
            .push(step("database", false, "无法在临时目录写入标记"));
    }
    if !inspection.ok {
        inspection.merge = None;
        inspection.overwrite = None;
        let _ = fs::remove_dir_all(&staging);
    }
    inspection
}

fn inspect_steps(
    directory: &Path,
    archive_path: &Path,
    staging: &Path,
    inspection: &mut Inspection,
) -> bool {
    let fail = |inspection: &mut Inspection, id, detail: String| {
        inspection.steps.push(step(id, false, detail));
        false
    };
    let opened = File::open(archive_path)
        .map_err(|_| "无法打开这个文件".to_owned())
        .and_then(|file| {
            zip::ZipArchive::new(BufReader::new(file))
                .map_err(|_| "不是有效的备份文件（不是 zip 或已损坏）".to_owned())
        });
    let mut archive = match opened {
        Ok(archive) => archive,
        Err(detail) => return fail(inspection, "manifest", detail),
    };
    let manifest = match read_manifest(&mut archive) {
        Ok(manifest) => manifest,
        Err(detail) => return fail(inspection, "manifest", detail),
    };
    inspection.exported_ms = manifest.exported_ms;
    inspection.app_version.clone_from(&manifest.app_version);
    inspection.schema_version = manifest.schema_version;
    inspection.steps.push(step("manifest", true, "格式正确"));

    if let Err(detail) = extract(&mut archive, &manifest, staging) {
        return fail(inspection, "hashes", detail);
    }
    inspection.steps.push(step(
        "hashes",
        true,
        format!("{} 个文件的校验值都对", manifest.files.len()),
    ));

    let database = staging.join(DATABASE);
    match prepare_database(&database) {
        Ok(upgraded) => {
            inspection.upgraded_from = upgraded;
            inspection.steps.push(step(
                "database",
                true,
                match upgraded {
                    Some(from) => {
                        format!("完整性通过 · 旧版本 v{from}，已在临时区升级到 v{VERSION}")
                    }
                    None => "完整性通过".to_owned(),
                },
            ));
        }
        Err(Error::NewerSchema { found, supported }) => {
            return fail(
                inspection,
                "database",
                format!(
                    "备份来自更新版本的拈来（数据库 v{found}，本版本最高支持 v{supported}），请先升级拈来"
                ),
            );
        }
        Err(_) => {
            return fail(
                inspection,
                "database",
                "数据库完整性检查没有通过，备份可能已损坏".to_owned(),
            );
        }
    }
    // 预览：合并会新增什么、覆盖会得到什么。
    let preview = (|| -> Result<(MergePreview, OverwritePreview)> {
        let backup = schema::readonly(&database)?;
        let items = live_contents(&backup)?;
        let local = schema::readonly(&directory.join(DATABASE))?;
        Ok((merge_preview(&local, &backup)?, OverwritePreview { items }))
    })();
    match preview {
        Ok((merge, overwrite)) => {
            inspection.merge = Some(merge);
            inspection.overwrite = Some(overwrite);
            true
        }
        Err(_) => fail(
            inspection,
            "database",
            "无法与现有历史库比对，请稍后再试".to_owned(),
        ),
    }
}

// 本地已有的指纹（回收站里的也算）→ 是否在回收站、是否已收藏。
fn local_state(c: &Connection) -> Result<HashMap<Vec<u8>, (i64, bool, bool)>> {
    Ok(c.prepare(
        "SELECT c.fingerprint,c.id,EXISTS(SELECT 1 FROM trash t WHERE t.content_id=c.id),\
         EXISTS(SELECT 1 FROM favorites f WHERE f.content_id=c.id) FROM contents c",
    )?
    .query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?))))?
    .collect::<std::result::Result<_, _>>()?)
}

fn merge_preview(local: &Connection, backup: &Connection) -> Result<MergePreview> {
    let known = local_state(local)?;
    let new_items = backup
        .prepare(&format!(
            "SELECT c.fingerprint FROM contents c WHERE {LIVE_CONTENT}"
        ))?
        .query_map([], |r| r.get::<_, Vec<u8>>(0))?
        .filter(|fp| fp.as_ref().is_ok_and(|fp| !known.contains_key(fp)))
        .count();
    let new_favorites = backup
        .prepare(&format!(
            "SELECT c.fingerprint FROM favorites fv JOIN contents c ON c.id=fv.content_id WHERE {LIVE_CONTENT}"
        ))?
        .query_map([], |r| r.get::<_, Vec<u8>>(0))?
        .filter(|fp| {
            fp.as_ref().is_ok_and(|fp| match known.get(fp) {
                None => true,
                Some((_, trashed, favorite)) => !trashed && !favorite,
            })
        })
        .count();
    Ok(MergePreview {
        new_items,
        new_favorites,
    })
}

// ---------- 临时区管理 ----------

/// 检查通过的临时目录。令牌必须是 `inspect` 发的，且带 `ready` 标记。
pub fn staging(directory: &Path, token: &str) -> Option<PathBuf> {
    if !valid_token(token) {
        return None;
    }
    let path = imports(directory).join(token);
    path.join(READY).is_file().then_some(path)
}

pub fn discard(directory: &Path, token: &str) {
    if valid_token(token) {
        let _ = fs::remove_dir_all(imports(directory).join(token));
    }
}

/// 启动时清掉上次遗留的临时目录。
pub fn clean_stale(directory: &Path) {
    let _ = fs::remove_dir_all(imports(directory));
}

// ---------- 合并 ----------

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct MergeReport {
    pub added_items: usize,
    pub added_favorites: usize,
}

struct AppMapper<'a> {
    source: &'a Connection,
    backup_icons: &'a Path,
    icons: &'a Path,
    mapped: HashMap<i64, Option<i64>>,
}
impl AppMapper<'_> {
    // 备份里的来源应用 → 本地行：按（进程名，exe 路径）找，没有就建；图标缺就从备份复制。
    fn local(&mut self, tx: &Connection, backup_id: Option<i64>) -> Result<Option<i64>> {
        let Some(backup_id) = backup_id else {
            return Ok(None);
        };
        if let Some(id) = self.mapped.get(&backup_id) {
            return Ok(*id);
        }
        let row = self
            .source
            .query_row(
                "SELECT process_name,exe_path,display_name,icon_file,icon_key,updated_ms FROM source_apps WHERE id=?1",
                [backup_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, i64>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((process, path, display, icon, key, updated)) = row else {
            self.mapped.insert(backup_id, None);
            return Ok(None);
        };
        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM source_apps WHERE process_name=?1 AND exe_path=?2",
                params![process, path],
                |r| r.get(0),
            )
            .optional()?;
        let id = match existing {
            Some(id) => id,
            None => {
                let icon = self.copy_icon(&icon);
                tx.execute(
                    "INSERT INTO source_apps(process_name,exe_path,display_name,icon_file,icon_key,updated_ms) VALUES(?1,?2,?3,?4,?5,?6)",
                    params![process, path, display, icon, key, updated],
                )?;
                tx.last_insert_rowid()
            }
        };
        self.mapped.insert(backup_id, Some(id));
        Ok(Some(id))
    }
    // 返回写进库里的 icon_file：名字不合法或两边都没有文件时为空串。
    fn copy_icon(&self, name: &str) -> String {
        if !plain_name(name) {
            return String::new();
        }
        let target = self.icons.join(name);
        if target.is_file() {
            return name.to_owned();
        }
        let from = self.backup_icons.join(name);
        if !from.is_file() {
            return String::new();
        }
        let mut stage = target.clone().into_os_string();
        stage.push(".pending");
        let stage = PathBuf::from(stage);
        if fs::copy(&from, &stage)
            .and_then(|_| fs::rename(&stage, &target))
            .is_ok()
        {
            name.to_owned()
        } else {
            let _ = fs::remove_file(&stage);
            String::new()
        }
    }
}

/// 把备份里本地没有的内容合并进现役库（在写入线程里调用）。一个事务：要么全进，要么都不进。
/// 返回报告和新的 sort_order 计数。
pub(crate) fn merge_into(
    c: &mut Connection,
    database: &Path,
    backup_icons: &Path,
    icons: &Path,
    mut sort_order: i64,
) -> Result<(MergeReport, i64)> {
    let source = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    if schema::version(&source)? != VERSION {
        return Err(Error::Invalid("schema_version"));
    }
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut known: HashMap<Vec<u8>, i64> = local_state(&tx)?
        .into_iter()
        .map(|(fingerprint, (id, _, _))| (fingerprint, id))
        .collect();
    let mut apps = AppMapper {
        source: &source,
        backup_icons,
        icons,
        mapped: HashMap::new(),
    };
    let mut added_items = 0;
    // 按原来的时间顺序一条条放进去，sort_order 随之递增，和同一时间的先后一致。
    let rows: Vec<(i64, Vec<u8>)> = source
        .prepare(&format!(
            "SELECT c.id,c.fingerprint FROM contents c WHERE {LIVE_CONTENT} ORDER BY c.sort_ms,c.sort_order,c.id"
        ))?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    for (backup_id, fingerprint) in rows {
        if known.contains_key(&fingerprint) {
            continue;
        }
        sort_order += 1;
        let (kind, preview, names, bytes, partial, sort_ms, search_text): (
            String,
            String,
            String,
            i64,
            bool,
            i64,
            String,
        ) = source.query_row(
            "SELECT kind,preview,file_names,bytes,partial,sort_ms,search_text FROM contents WHERE id=?1",
            [backup_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            },
        )?;
        tx.execute(
            "INSERT INTO contents(fingerprint,kind,preview,file_names,bytes,partial,sort_ms,source,copy_count,sort_order,search_text) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,'',0,?8,?9)",
            params![fingerprint, kind, preview, names, bytes, partial, sort_ms, sort_order, search_text],
        )?;
        let id = tx.last_insert_rowid();
        {
            let mut formats = source.prepare(
                "SELECT name,fingerprint,bytes,encoding,data FROM formats WHERE content_id=?1",
            )?;
            let mut insert = tx.prepare_cached(
                "INSERT INTO formats(content_id,name,fingerprint,bytes,encoding,data) VALUES(?1,?2,?3,?4,?5,?6)",
            )?;
            let mut rows = formats.query([backup_id])?;
            while let Some(row) = rows.next()? {
                insert.execute(params![
                    id,
                    row.get::<_, String>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Vec<u8>>(4)?
                ])?;
            }
        }
        {
            let mut events = source.prepare(
                "SELECT time_ms,source_process,owner_process,owner_pid,sequence,source_app_id FROM copy_events WHERE content_id=?1 ORDER BY time_ms,id",
            )?;
            let mut rows = events.query([backup_id])?;
            while let Some(row) = rows.next()? {
                let app = apps.local(&tx, row.get::<_, Option<i64>>(5)?)?;
                tx.execute(
                    "INSERT INTO copy_events(content_id,time_ms,source_process,owner_process,owner_pid,sequence,source_app_id) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        id,
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        app
                    ],
                )?;
            }
        }
        // 与写入线程一致：复制次数、来源取自最近一次复制事件。
        tx.execute(
            "UPDATE contents SET copy_count=(SELECT count(*) FROM copy_events WHERE content_id=?1),\
             source=coalesce((SELECT source_process FROM copy_events WHERE content_id=?1 ORDER BY time_ms DESC,id DESC LIMIT 1),''),\
             source_app_id=(SELECT source_app_id FROM copy_events WHERE content_id=?1 ORDER BY time_ms DESC,id DESC LIMIT 1) WHERE id=?1",
            [id],
        )?;
        known.insert(fingerprint, id);
        added_items += 1;
    }

    // 收藏：排在现有收藏之后，按备份里的收藏顺序；本地已收藏的不动，回收站里的本地内容不替它收藏。
    let mut added_favorites = 0;
    let favorites: Vec<(Vec<u8>, String, String, i64, i64)> = source
        .prepare(&format!(
            "SELECT c.fingerprint,fv.title,fv.note,fv.created_ms,fv.updated_ms FROM favorites fv \
             JOIN contents c ON c.id=fv.content_id WHERE {LIVE_CONTENT} ORDER BY fv.position"
        ))?
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<std::result::Result<_, _>>()?;
    for (fingerprint, title, note, created, updated) in favorites {
        let Some(id) = known.get(&fingerprint) else {
            continue;
        };
        if schema::trashed(&tx, *id as usize)? {
            continue;
        }
        added_favorites += tx.execute(
            "INSERT INTO favorites(content_id,position,title,note,created_ms,updated_ms) \
             VALUES(?1,(SELECT coalesce(max(position),0)+1 FROM favorites),?2,?3,?4,?5) \
             ON CONFLICT(content_id) DO NOTHING",
            params![id, clean_title(&title), clean_note(&note), created, updated],
        )?;
    }
    tx.commit()?;
    Ok((
        MergeReport {
            added_items,
            added_favorites,
        },
        sort_order,
    ))
}

// ---------- 覆盖 ----------

/// 登记覆盖前的检查：临时目录有效，且里面的库通过完整校验。
pub fn prepare_overwrite(directory: &Path, token: &str) -> Result<()> {
    let staging = staging(directory, token).ok_or(Error::Invalid("import_missing"))?;
    snapshot::validate_snapshot(&staging.join(DATABASE)).map(drop)
}

/// 启动时（库还没打开）执行覆盖：替换 history.db，再用备份里的图标替换 icons 目录里的文件。
/// 旧库留在 `history.db.before-restore-*`，和快照恢复一样；调用方负责事先另拍快照。
pub fn apply_overwrite(directory: &Path, token: &str) -> Result<()> {
    let staging = staging(directory, token).ok_or(Error::Invalid("import_missing"))?;
    restore(directory, &staging.join(DATABASE))?;
    let icons = directory.join("icons");
    fs::create_dir_all(&icons)?;
    for entry in fs::read_dir(&icons)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            let _ = fs::remove_file(entry.path());
        }
    }
    let backup = staging.join("icons");
    if backup.is_dir() {
        for entry in fs::read_dir(&backup)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if entry.file_type()?.is_file() && plain_name(&name) {
                // 图标只是缓存，个别复制失败不影响恢复。
                let _ = fs::copy(entry.path(), icons.join(&*name));
            }
        }
    }
    Ok(())
}
