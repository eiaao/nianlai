//! 版本文件的解析、校验与版本比较（纯逻辑，不碰网络和文件系统）。
//!
//! 版本文件 `latest-<windows|macos>.json` 由发布工作流生成，客户端固定从
//! `https://github.com/eiaao/nianlai/releases/latest/download/` 读取。

use semver::Version;
use serde::{Deserialize, Serialize};

/// 安装包下载地址必须以此开头，否则整份版本文件视为无效。
pub const DOWNLOAD_PREFIX: &str = "https://github.com/eiaao/nianlai/releases/download/";
/// 版本文件的固定地址前缀。
pub const FEED_BASE: &str = "https://github.com/eiaao/nianlai/releases/latest/download/";
/// 自动更新不可用时，让用户去的下载页。
pub const DOWNLOAD_PAGE: &str = "https://github.com/eiaao/nianlai/releases/latest";
/// 版本文件响应体上限。
pub const FEED_MAX_BYTES: usize = 64 * 1024;

const MAX_NOTE_VERSIONS: usize = 5;
const MAX_NOTE_ITEMS: usize = 20;
const MAX_NOTE_CHARS: usize = 200;
const MAX_URL_CHARS: usize = 512;

#[cfg(windows)]
pub const PLATFORM: &str = "windows";
#[cfg(target_os = "macos")]
pub const PLATFORM: &str = "macos";
#[cfg(not(any(windows, target_os = "macos")))]
pub const PLATFORM: &str = "other";

/// 本平台安装包的扩展名；用于核对 `url` 指向的是对的文件类型。
#[cfg(target_os = "macos")]
pub const PACKAGE_EXT: &str = ".dmg";
#[cfg(not(target_os = "macos"))]
pub const PACKAGE_EXT: &str = ".zip";

pub fn feed_url() -> String {
    format!("{FEED_BASE}latest-{PLATFORM}.json")
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub version: String,
    pub items: Vec<String>,
}

/// 校验通过的最新版本信息；也是写进设置文件的缓存形态。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Latest {
    pub version: String,
    #[serde(default)]
    pub date: String,
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub notes: Vec<Note>,
}

pub fn parse_version(text: &str) -> Option<Version> {
    Version::parse(text.trim().trim_start_matches('v')).ok()
}

/// `candidate` 是否比 `current` 新（语义化版本；任一无法解析则不算新）。
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

pub fn valid_download_url(url: &str, expected_ext: &str) -> bool {
    url.len() <= MAX_URL_CHARS
        && url.starts_with(DOWNLOAD_PREFIX)
        && url.len() > DOWNLOAD_PREFIX.len()
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
        && !url.contains(['?', '#', '\\'])
        && !url[DOWNLOAD_PREFIX.len()..]
            .split('/')
            .any(|part| part == "..")
        && url.to_ascii_lowercase().ends_with(expected_ext)
}

fn valid_sha256(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn clean_text(text: &str) -> Option<String> {
    let text: String = text.chars().filter(|c| !c.is_control()).collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.chars().take(MAX_NOTE_CHARS).collect())
}

impl Latest {
    /// 校验并整理；任何关键字段无效都返回 None（视为无效版本文件）。
    pub fn sanitize(mut self, expected_ext: &str) -> Option<Self> {
        let version = parse_version(&self.version)?;
        self.version = version.to_string();
        if !valid_download_url(&self.url, expected_ext) || !valid_sha256(&self.sha256) {
            return None;
        }
        self.date = self
            .date
            .chars()
            .filter(|c| !c.is_control())
            .take(32)
            .collect();
        let mut notes: Vec<(Version, Note)> = self
            .notes
            .into_iter()
            .filter_map(|note| {
                let version = parse_version(&note.version)?;
                let items: Vec<String> = note
                    .items
                    .iter()
                    .filter_map(|item| clean_text(item))
                    .take(MAX_NOTE_ITEMS)
                    .collect();
                (!items.is_empty()).then(|| {
                    (
                        version.clone(),
                        Note {
                            version: version.to_string(),
                            items,
                        },
                    )
                })
            })
            .collect();
        notes.sort_by(|a, b| b.0.cmp(&a.0));
        notes.dedup_by(|a, b| a.0 == b.0);
        notes.truncate(MAX_NOTE_VERSIONS);
        self.notes = notes.into_iter().map(|(_, note)| note).collect();
        Some(self)
    }

    /// 只保留比当前版本新的更新说明。
    pub fn notes_newer_than(&self, current: &str) -> Vec<Note> {
        self.notes
            .iter()
            .filter(|note| is_newer(&note.version, current))
            .cloned()
            .collect()
    }
}

pub fn parse(bytes: &[u8], expected_ext: &str) -> Result<Latest, &'static str> {
    if bytes.len() > FEED_MAX_BYTES {
        return Err("版本文件过大");
    }
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let latest: Latest = serde_json::from_slice(bytes).map_err(|_| "版本文件格式不对")?;
    latest.sanitize(expected_ext).ok_or("版本文件内容无效")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn feed(version: &str, url: &str, sha: &str) -> String {
        format!(
            r#"{{"version":"{version}","date":"2026-11-25","url":"{url}","sha256":"{sha}",
            "notes":[{{"version":"0.1.1","items":["旧的"]}},{{"version":"0.1.2","items":["新的 A","新的 B"]}}]}}"#
        )
    }
    fn good_url(version: &str) -> String {
        format!("{DOWNLOAD_PREFIX}v{version}/nianlai-{version}-windows-x64-portable.zip")
    }

    #[test]
    fn semver_comparison_follows_precedence_rules() {
        assert!(is_newer("0.1.2", "0.1.1"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "1.0.0-rc.1"));
        assert!(!is_newer("1.0.0-rc.1", "1.0.0"));
        assert!(!is_newer("0.1.1", "0.1.1"));
        assert!(!is_newer("0.1.0", "0.1.1"));
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(!is_newer("garbage", "0.1.1"));
        assert!(!is_newer("0.2.0", "garbage"));
    }

    #[test]
    fn download_url_must_live_under_the_release_prefix() {
        assert!(valid_download_url(&good_url("0.1.2"), ".zip"));
        assert!(!valid_download_url(&good_url("0.1.2"), ".dmg"));
        for bad in [
            "http://github.com/eiaao/nianlai/releases/download/v1/a.zip",
            "https://github.com/eiaao/nianlai/releases/download/",
            "https://github.com/evil/nianlai/releases/download/v1/a.zip",
            "https://github.com.evil.com/eiaao/nianlai/releases/download/v1/a.zip",
            "https://github.com/eiaao/nianlai/releases/download/../../../evil/a.zip",
            "https://github.com/eiaao/nianlai/releases/download/v1/a.zip?x=1",
            "https://github.com/eiaao/nianlai/releases/download/v1/a b.zip",
            "https://github.com/eiaao/nianlai/releases/latest/download/a.zip",
        ] {
            assert!(!valid_download_url(bad, ".zip"), "{bad}");
        }
    }

    #[test]
    fn parses_a_valid_feed_and_orders_notes_newest_first() {
        let latest = parse(feed("0.1.2", &good_url("0.1.2"), SHA).as_bytes(), ".zip").unwrap();
        assert_eq!(latest.version, "0.1.2");
        assert_eq!(latest.notes[0].version, "0.1.2");
        assert_eq!(latest.notes[1].version, "0.1.1");
        // 只显示比当前版本新的条目。
        let shown = latest.notes_newer_than("0.1.1");
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].items, ["新的 A", "新的 B"]);
        assert!(latest.notes_newer_than("0.1.2").is_empty());
    }

    #[test]
    fn rejects_invalid_feeds() {
        let ok = good_url("0.1.2");
        assert!(parse(feed("nope", &ok, SHA).as_bytes(), ".zip").is_err());
        assert!(
            parse(
                feed("0.1.2", "https://example.com/a.zip", SHA).as_bytes(),
                ".zip"
            )
            .is_err()
        );
        assert!(parse(feed("0.1.2", &ok, "ABC").as_bytes(), ".zip").is_err());
        assert!(parse(feed("0.1.2", &ok, &SHA.to_uppercase()).as_bytes(), ".zip").is_err());
        assert!(parse(b"not json", ".zip").is_err());
        assert!(parse(&vec![b' '; FEED_MAX_BYTES + 1], ".zip").is_err());
    }

    #[test]
    fn accepts_bom_and_missing_optional_fields_and_trims_notes() {
        let mut body = b"\xEF\xBB\xBF".to_vec();
        body.extend_from_slice(
            format!(
                r#"{{"version":"0.2.0","url":"{}","sha256":"{SHA}","notes":[{{"version":"0.2.0","items":["  ok\u0007 ","", "{}"]}}]}}"#,
                good_url("0.2.0"),
                "长".repeat(500)
            )
            .as_bytes(),
        );
        let latest = parse(&body, ".zip").unwrap();
        assert_eq!(latest.date, "");
        assert_eq!(latest.notes[0].items[0], "ok");
        assert_eq!(latest.notes[0].items.len(), 2);
        assert_eq!(latest.notes[0].items[1].chars().count(), MAX_NOTE_CHARS);
    }

    #[test]
    fn keeps_at_most_five_note_versions() {
        let notes: Vec<String> = (0..8)
            .map(|n| format!(r#"{{"version":"0.1.{n}","items":["x"]}}"#))
            .collect();
        let body = format!(
            r#"{{"version":"0.1.7","url":"{}","sha256":"{SHA}","notes":[{}]}}"#,
            good_url("0.1.7"),
            notes.join(",")
        );
        let latest = parse(body.as_bytes(), ".zip").unwrap();
        assert_eq!(latest.notes.len(), 5);
        assert_eq!(latest.notes[0].version, "0.1.7");
        assert_eq!(latest.notes[4].version, "0.1.3");
    }

    #[test]
    fn feed_url_is_the_fixed_latest_download_path() {
        assert!(
            feed_url()
                .starts_with("https://github.com/eiaao/nianlai/releases/latest/download/latest-")
        );
        assert!(feed_url().ends_with(".json"));
    }
}
