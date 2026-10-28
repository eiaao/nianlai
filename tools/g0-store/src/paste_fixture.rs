//! One offline, synthetic record for binding a later paste run to exact stored bytes.
//! This module has no clipboard, window, focus, or input dependencies.

use crate::Result;
use nianlai_store::{
    Store,
    content::{Content, Data, HTML, RTF, TEXT},
    hash::sha256,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fmt::Write, fs, io::ErrorKind, path::PathBuf, sync::Arc};

const FIXTURE_MARKER: &str = "NIANLAI_M0_PASTE_FIXTURE_V1";
const FIXTURE_TEXT: &str = "NIANLAI_M0_PASTE_FIXTURE_V1 | English 中文验收 😀 | <&> {\\} END";

#[derive(Clone, Copy)]
enum FixtureFormat {
    Html,
    Rtf,
    Text,
}

impl FixtureFormat {
    fn parse(value: Option<&str>) -> Result<Self> {
        match value {
            Some("html") => Ok(Self::Html),
            Some("rtf") => Ok(Self::Rtf),
            Some("text") => Ok(Self::Text),
            Some(_) => Err("paste_fixture_format_must_be_html_rtf_or_text".into()),
            None => Err("paste_fixture_format_required".into()),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Html => "html",
            Self::Rtf => "rtf",
            Self::Text => "text",
        }
    }
}

fn html_header(start_html: usize, end_html: usize, start: usize, end: usize) -> String {
    format!(
        "Version:1.0\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\nStartFragment:{start:010}\r\nEndFragment:{end:010}\r\n"
    )
}

fn html() -> Vec<u8> {
    let escaped = FIXTURE_TEXT
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let prefix = "<html><head><meta charset=\"utf-8\"></head><body><!--StartFragment-->";
    let fragment = format!("<p><strong>{escaped}</strong></p>");
    let suffix = "<!--EndFragment--></body></html>";
    // CF_HTML offsets count bytes from the beginning of the entire UTF-8 payload.
    let start_html = html_header(0, 0, 0, 0).len();
    let start_fragment = start_html + prefix.len();
    let end_fragment = start_fragment + fragment.len();
    let end_html = end_fragment + suffix.len();
    let header = html_header(start_html, end_html, start_fragment, end_fragment);
    format!("{header}{prefix}{fragment}{suffix}").into_bytes()
}

fn rtf() -> Vec<u8> {
    let mut output = String::from("{\\rtf1\\ansi\\ansicpg1252\\uc1\\b ");
    for unit in FIXTURE_TEXT.encode_utf16() {
        match unit {
            0x5c | 0x7b | 0x7d => {
                output.push('\\');
                output.push(char::from_u32(unit.into()).unwrap());
            }
            0x20..=0x7e => output.push(char::from_u32(unit.into()).unwrap()),
            // RTF \u values are signed 16-bit units, including both emoji surrogates.
            _ => write!(output, "\\u{}?", unit as i16).unwrap(),
        }
    }
    output.push_str("\\b0}");
    output.into_bytes()
}

fn content(format: FixtureFormat) -> Result<Arc<Content>> {
    let unicode: Vec<_> = FIXTURE_TEXT
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let mut formats = BTreeMap::from([(TEXT.into(), Data::Bytes(unicode.into()))]);
    let extra = match format {
        FixtureFormat::Html => Some((HTML, html())),
        FixtureFormat::Rtf => Some((RTF, rtf())),
        FixtureFormat::Text => None,
    };
    if let Some((name, bytes)) = extra {
        formats.insert(name.into(), Data::Bytes(bytes.into()));
    }
    Ok(Content::new(formats, sha256)?)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Only explicit CLI options are accepted. In particular, this does not consult
/// NIANLAI_G0_DATA_DIR and cannot open any already-existing directory or file.
pub fn create(options: &BTreeMap<String, String>) -> Result<Value> {
    // Validate the complete request before creating even an empty directory.
    let format = FixtureFormat::parse(options.get("--format").map(String::as_str))?;
    if options
        .keys()
        .any(|name| name != "--dir" && name != "--format")
    {
        return Err("paste_fixture_unknown_option".into());
    }
    let directory = options
        .get("--dir")
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .ok_or("explicit_data_directory_required")?;
    let fixture = content(format)?;
    // No create_dir_all: the parent must already exist, and create_dir atomically
    // rejects existing paths (including empty directories and symlinks).
    match fs::create_dir(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            return Err("paste_fixture_requires_new_directory".into());
        }
        Err(error) => return Err(error.into()),
    }

    let db = Store::open(&directory, sha256)?;
    let reader = db.reader()?;
    let empty = reader.counts()?;
    if empty.contents != 0 || empty.formats != 0 || empty.events != 0 || empty.indexed != 0 {
        return Err("paste_fixture_requires_empty_database".into());
    }
    let inserted = crate::write(&db, fixture.clone(), 1)?;
    let record_id = inserted
        .record_id
        .ok_or("paste_fixture_record_id_missing")?;
    let loaded = reader
        .content(record_id)?
        .ok_or("paste_fixture_record_missing")?;
    let counts = reader.counts()?;
    if counts.contents != 1
        || counts.events != 1
        || counts.indexed != 1
        || counts.formats != fixture.formats.len()
        || loaded.fingerprint != fixture.fingerprint
        || loaded.formats.len() != fixture.formats.len()
        || fixture
            .formats
            .keys()
            .any(|name| loaded.raw(name) != fixture.raw(name))
    {
        return Err("paste_fixture_roundtrip_mismatch".into());
    }
    let formats: Vec<_> = loaded
        .formats
        .iter()
        .map(|(name, format)| {
            json!({"name":name,"byte_length":format.data.size(),"sha256":hex(&format.fingerprint)})
        })
        .collect();
    let unicode = loaded.raw(TEXT).ok_or("paste_fixture_unicode_missing")?;
    let result = json!({
        "command":"paste-fixture",
        "manifest_version":1,
        "schema_version":db.report().version,
        "fixture_marker":FIXTURE_MARKER,
        "format":format.name(),
        "data_directory":fs::canonicalize(&directory)?,
        "record_id":record_id,
        "event_id":inserted.event_id,
        "counts":counts,
        "formats":formats,
        "hash_algorithm":"sha256",
        "content_fingerprint":hex(&loaded.fingerprint),
        "content_byte_length":loaded.bytes,
        "expected_utf16_length":FIXTURE_TEXT.encode_utf16().count(),
        "expected_utf16_sha256":hex(&sha256(unicode)?),
        "expected_utf16_hash_encoding":"utf-16le-without-nul",
        "expected_text_sha256_utf8":hex(&sha256(FIXTURE_TEXT.as_bytes())?),
        "safe_sample_preview_text":FIXTURE_TEXT,
        "expected_paste_mode":"normal",
        "expected_submission_method":"depends",
        "submission_method_note":"Depends on target qualification, the selected paste-key rule, and runtime checks; no fixed submission method is promised.",
    });
    drop(reader);
    drop(db);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nianlai_store::Reader;
    use std::{
        path::Path,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    struct Temp {
        directory: PathBuf,
        base: PathBuf,
    }

    impl Temp {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let base = fs::canonicalize(std::env::temp_dir()).unwrap();
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let directory = base.join(format!(
                "nianlai-paste-fixture-test-{}-{stamp}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&directory).unwrap();
            Self { directory, base }
        }

        fn child(&self, name: &str) -> PathBuf {
            self.directory.join(name)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let resolved = fs::canonicalize(&self.directory).unwrap();
            assert_eq!(resolved.parent(), Some(self.base.as_path()));
            assert!(
                resolved
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("nianlai-paste-fixture-test-")
            );
            fs::remove_dir_all(resolved).unwrap();
        }
    }

    fn options(directory: Option<&Path>, format: Option<&str>) -> BTreeMap<String, String> {
        let mut options = BTreeMap::new();
        if let Some(directory) = directory {
            options.insert("--dir".into(), directory.to_str().unwrap().into());
        }
        if let Some(format) = format {
            options.insert("--format".into(), format.into());
        }
        options
    }

    fn entries(directory: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn formats_contain_the_same_exact_unicode_text() {
        for (format, expected) in [
            (FixtureFormat::Html, vec![TEXT, HTML]),
            (FixtureFormat::Rtf, vec![TEXT, RTF]),
            (FixtureFormat::Text, vec![TEXT]),
        ] {
            let fixture = content(format).unwrap();
            assert_eq!(
                fixture
                    .formats
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                expected
            );
            assert_eq!(fixture.text_value().as_deref(), Some(FIXTURE_TEXT));
            assert_eq!(fixture.preview, FIXTURE_TEXT);
            let unicode = fixture.raw(TEXT).unwrap();
            assert_eq!(unicode.len(), FIXTURE_TEXT.encode_utf16().count() * 2);
            assert!(!unicode.ends_with(&[0, 0]));
            assert!(FIXTURE_TEXT.contains("English 中文验收 😀"));
        }
    }

    #[test]
    fn html_offsets_are_valid_utf8_byte_ranges_including_chinese() {
        let payload = html();
        let document = std::str::from_utf8(&payload).unwrap();
        let offset = |name: &str| -> usize {
            document
                .lines()
                .find_map(|line| line.strip_prefix(name))
                .unwrap()
                .parse()
                .unwrap()
        };
        let start_html = offset("StartHTML:");
        let end_html = offset("EndHTML:");
        let start_fragment = offset("StartFragment:");
        let end_fragment = offset("EndFragment:");
        assert_eq!(end_html, payload.len());
        assert!(document[..start_html].ends_with("\r\n"));
        assert!(document[start_html..end_html].starts_with("<html>"));
        assert!(document[start_html..end_html].ends_with("</html>"));
        assert!(document[..start_fragment].ends_with("<!--StartFragment-->"));
        assert!(document[end_fragment..].starts_with("<!--EndFragment-->"));
        assert_eq!(
            std::str::from_utf8(&payload[start_fragment..end_fragment]).unwrap(),
            "<p><strong>NIANLAI_M0_PASTE_FIXTURE_V1 | English 中文验收 😀 | &lt;&amp;&gt; {\\} END</strong></p>"
        );
        assert!(document[..end_fragment].chars().count() < end_fragment);
    }

    #[test]
    fn rtf_has_correct_signed_unicode_surrogates_and_literal_escaping() {
        let payload = rtf();
        assert!(payload.is_ascii());
        let document = std::str::from_utf8(&payload).unwrap();
        assert_eq!(
            document,
            r"{\rtf1\ansi\ansicpg1252\uc1\b NIANLAI_M0_PASTE_FIXTURE_V1 | English \u20013?\u25991?\u-25972?\u25910? \u-10179?\u-8704? | <&> \{\\\} END\b0}"
        );
        let units: Vec<_> = document
            .split("\\u")
            .skip(1)
            .filter_map(|part| part.split('?').next()?.parse::<i16>().ok())
            .map(|unit| unit as u16)
            .collect();
        assert_eq!(String::from_utf16(&units).unwrap(), "中文验收😀");
    }

    #[test]
    fn missing_explicit_directory_and_invalid_requests_create_nothing() {
        let temp = Temp::new();
        assert_eq!(
            create(&options(None, Some("html")))
                .unwrap_err()
                .to_string(),
            "explicit_data_directory_required"
        );
        let target = temp.child("fixture");
        for format in [Some("unknown"), Some("HTML"), None] {
            assert!(create(&options(Some(&target), format)).is_err());
            assert!(entries(&temp.directory).is_empty());
        }
        let mut unknown = options(Some(&target), Some("rtf"));
        unknown.insert("--seed".into(), "1".into());
        assert_eq!(
            create(&unknown).unwrap_err().to_string(),
            "paste_fixture_unknown_option"
        );
        assert!(entries(&temp.directory).is_empty());
        let missing_parent = temp.child("missing-parent").join("fixture");
        assert!(create(&options(Some(&missing_parent), Some("html"))).is_err());
        assert!(entries(&temp.directory).is_empty());
    }

    #[test]
    fn existing_empty_directory_database_and_file_are_refused_unchanged() {
        let temp = Temp::new();
        let empty = temp.child("empty");
        fs::create_dir(&empty).unwrap();
        let populated = temp.child("populated");
        fs::create_dir(&populated).unwrap();
        let database = populated.join("history.db");
        let sentinel = b"existing user database: must never be opened or modified";
        fs::write(&database, sentinel).unwrap();
        let file = temp.child("file");
        fs::write(&file, sentinel).unwrap();
        let original_entries = entries(&temp.directory);
        for target in [&empty, &populated, &file] {
            assert_eq!(
                create(&options(Some(target), Some("html")))
                    .unwrap_err()
                    .to_string(),
                "paste_fixture_requires_new_directory"
            );
        }
        assert!(entries(&empty).is_empty());
        assert_eq!(entries(&populated), vec!["history.db"]);
        assert_eq!(fs::read(database).unwrap(), sentinel);
        assert_eq!(fs::read(file).unwrap(), sentinel);
        assert_eq!(entries(&temp.directory), original_entries);
    }

    #[test]
    fn store_roundtrip_manifest_binds_one_record_to_exact_formats_and_hashes() {
        let temp = Temp::new();
        for format in [FixtureFormat::Html, FixtureFormat::Rtf, FixtureFormat::Text] {
            let directory = temp.child(format.name());
            let manifest = create(&options(Some(&directory), Some(format.name()))).unwrap();
            let reader = Reader::open(&directory, sha256).unwrap();
            let counts = reader.counts().unwrap();
            let id = manifest["record_id"].as_u64().unwrap() as usize;
            let loaded = reader.content(id).unwrap().unwrap();
            assert_eq!(counts.contents, 1);
            assert_eq!(counts.events, 1);
            assert_eq!(counts.indexed, 1);
            assert_eq!(counts.formats, loaded.formats.len());
            assert_eq!(reader.recent().unwrap()[0].id, id);
            assert!(
                reader
                    .event_exists(manifest["event_id"].as_i64().unwrap())
                    .unwrap()
            );
            assert_eq!(loaded.text_value().as_deref(), Some(FIXTURE_TEXT));
            assert_eq!(manifest["manifest_version"], 1);
            assert_eq!(manifest["schema_version"], nianlai_store::VERSION);
            assert_eq!(manifest["format"], format.name());
            assert_eq!(manifest["fixture_marker"], FIXTURE_MARKER);
            assert_eq!(manifest["safe_sample_preview_text"], FIXTURE_TEXT);
            assert_eq!(manifest["content_fingerprint"], hex(&loaded.fingerprint));
            assert_eq!(manifest["content_byte_length"], loaded.bytes);
            assert_eq!(
                manifest["expected_utf16_length"],
                FIXTURE_TEXT.encode_utf16().count()
            );
            assert_eq!(
                manifest["expected_utf16_sha256"],
                hex(&sha256(loaded.raw(TEXT).unwrap()).unwrap())
            );
            assert_eq!(
                manifest["expected_utf16_hash_encoding"],
                "utf-16le-without-nul"
            );
            assert_eq!(
                manifest["expected_text_sha256_utf8"],
                hex(&sha256(FIXTURE_TEXT.as_bytes()).unwrap())
            );
            assert_eq!(manifest["expected_paste_mode"], "normal");
            assert_eq!(manifest["expected_submission_method"], "depends");
            let formats = manifest["formats"].as_array().unwrap();
            assert_eq!(formats.len(), loaded.formats.len());
            for entry in formats {
                let name = entry["name"].as_str().unwrap();
                let raw = loaded.raw(name).unwrap();
                assert_eq!(entry["byte_length"], raw.len());
                assert_eq!(entry["sha256"], hex(&sha256(raw).unwrap()));
            }
            assert!(create(&options(Some(&directory), Some(format.name()))).is_err());
        }
    }
}
