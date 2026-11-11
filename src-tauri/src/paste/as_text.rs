// 目标只收文字时的降级：文件给路径，图片存成 PNG 给路径，富文本去掉格式。
use super::{keys::TextStyle, request::Mode};
use crate::{
    history::content::{Content, Data, FILES, Hash},
    platform,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Instant};

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Conversion {
    Paths,
    ImagePath,
    PlainText,
}

// 自动降级只管键表标了只收文字的目标，而且只转文件和图片，文字和富文本本来就能粘；
// Ctrl+Enter 对任何目标都转，富文本也去掉格式。只复制（Shift+Enter）一律不转。
pub fn plan(kind: &str, mode: Mode, rule: Option<TextStyle>) -> Option<(Conversion, TextStyle)> {
    let manual = mode == Mode::Text;
    if !manual && (mode != Mode::Paste || rule.is_none()) {
        return None;
    }
    let style = rule.unwrap_or(TextStyle::Lines);
    match kind {
        "文件" => Some((Conversion::Paths, style)),
        "图片" => Some((Conversion::ImagePath, style)),
        "富文本" if manual => Some((Conversion::PlainText, style)),
        _ => None,
    }
}

pub fn join(paths: &[String], style: TextStyle) -> String {
    match style {
        TextStyle::Lines => paths.join("\r\n"),
        TextStyle::Shell => paths
            .iter()
            .map(|path| quote(path))
            .collect::<Vec<_>>()
            .join(" "),
    }
}

// 只用双引号：cmd、PowerShell 和 bash 都认，Windows 路径里也不会出现双引号。
// 双引号挡不住 PowerShell、bash 展开 `$` 和 cmd 展开 `%`，这种路径少见，先不管。
fn quote(path: &str) -> String {
    if path.is_empty()
        || path
            .chars()
            .any(|c| c.is_whitespace() || "&()[]{}^=;!'+,`~$@#%".contains(c))
    {
        format!("\"{path}\"")
    } else {
        path.to_owned()
    }
}

// 返回要写进剪贴板的内容和日志。转不成时退回原内容照常粘贴，日志记下原因；日志里不写路径和文字。
pub fn apply(
    content: &Arc<Content>,
    mode: Mode,
    rule: Option<TextStyle>,
    exports: Option<&Path>,
    hash: Hash,
) -> (Arc<Content>, Value) {
    let trigger = if mode == Mode::Text { "manual" } else { "rule" };
    let Some((conversion, style)) = plan(content.kind, mode, rule) else {
        let report = if mode == Mode::Text {
            json!({"trigger":trigger,"conversion":null,"reason":"already_text"})
        } else {
            Value::Null
        };
        return (content.clone(), report);
    };
    let started = Instant::now();
    let mut report = json!({"trigger":trigger,"conversion":conversion,"style":style});
    let text = match conversion {
        Conversion::Paths => match content.formats.get(FILES).map(|format| &format.data) {
            Some(Data::Files(paths)) => {
                report["files"] = json!(paths.len());
                Ok(join(paths, style))
            }
            _ => Err("no_files"),
        },
        Conversion::ImagePath => exports
            .ok_or("no_export_directory")
            .and_then(|directory| platform::image::save_png(content, directory))
            .and_then(|saved| {
                report["png_source"] = json!(saved.source);
                report["png_reused"] = json!(saved.reused);
                report["png_bytes"] = json!(saved.bytes);
                report["encode_ms"] = json!(saved.encode_ms);
                saved
                    .path
                    .to_str()
                    .map(|path| join(&[path.to_owned()], style))
                    .ok_or("path_encoding")
            }),
        Conversion::PlainText => content.text_value().ok_or("no_plain_text"),
    };
    let converted = text.and_then(|text| Content::text(&text, hash).map_err(|_| "hash_failed"));
    report["elapsed_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    match converted {
        Ok(converted) => (converted, report),
        Err(error) => {
            report["error"] = json!(error);
            (content.clone(), report)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        history::content::{HTML, TEXT},
        platform::hash::sha256,
    };
    use std::collections::BTreeMap;

    fn files(paths: &[&str]) -> Arc<Content> {
        let paths: Vec<String> = paths.iter().map(|path| (*path).to_owned()).collect();
        Content::new(
            BTreeMap::from([(FILES.into(), Data::Files(paths.into()))]),
            sha256,
        )
        .unwrap()
    }

    #[test]
    fn converts_only_what_the_trigger_allows() {
        let lines = Some(TextStyle::Lines);
        for kind in ["文件", "图片"] {
            assert!(plan(kind, Mode::Paste, lines).is_some());
            assert!(plan(kind, Mode::Paste, None).is_none());
            assert!(plan(kind, Mode::Copy, lines).is_none());
            assert!(plan(kind, Mode::Text, None).is_some());
        }
        assert_eq!(
            plan("富文本", Mode::Text, None),
            Some((Conversion::PlainText, TextStyle::Lines))
        );
        assert!(plan("富文本", Mode::Paste, lines).is_none());
        assert!(plan("文本", Mode::Text, lines).is_none());
        assert_eq!(
            plan("图片", Mode::Paste, Some(TextStyle::Shell)),
            Some((Conversion::ImagePath, TextStyle::Shell))
        );
    }

    #[test]
    fn joins_paths_per_line_or_quotes_them_for_a_shell() {
        let paths = [
            r"C:\a.txt".to_owned(),
            r"D:\测试文件\报价 单.xlsx".to_owned(),
            r"E:\R&D\x(1).png".to_owned(),
        ];
        assert_eq!(
            join(&paths, TextStyle::Lines),
            "C:\\a.txt\r\nD:\\测试文件\\报价 单.xlsx\r\nE:\\R&D\\x(1).png"
        );
        assert_eq!(
            join(&paths, TextStyle::Shell),
            r#"C:\a.txt "D:\测试文件\报价 单.xlsx" "E:\R&D\x(1).png""#
        );
        assert_eq!(join(&paths[..1], TextStyle::Shell), r"C:\a.txt");
    }

    #[test]
    fn files_become_plain_text_and_failures_fall_back_to_the_original() {
        let content = files(&[r"C:\a.txt", r"C:\b c.txt"]);
        let (written, report) = apply(&content, Mode::Paste, Some(TextStyle::Shell), None, sha256);
        assert_eq!(
            written.text_value().as_deref(),
            Some(r#"C:\a.txt "C:\b c.txt""#)
        );
        assert_eq!(written.formats.keys().collect::<Vec<_>>(), [TEXT]);
        assert_eq!(report["trigger"], "rule");
        assert_eq!(report["files"], 2);
        assert!(report.get("error").is_none());

        let (written, report) = apply(&content, Mode::Copy, Some(TextStyle::Shell), None, sha256);
        assert!(Arc::ptr_eq(&written, &content));
        assert!(report.is_null());

        let image = Content::new(
            BTreeMap::from([(
                crate::history::content::DIB.into(),
                Data::Bytes(vec![0; 8].into()),
            )]),
            sha256,
        )
        .unwrap();
        let (written, report) = apply(&image, Mode::Text, None, None, sha256);
        assert!(Arc::ptr_eq(&written, &image));
        assert_eq!(report["error"], "no_export_directory");
    }

    #[test]
    fn images_become_the_path_of_a_saved_png() {
        // 1×1、24 位的 DIB：40 字节信息头加一行像素（补齐到 4 字节）。
        let mut dib = vec![0u8; 40];
        dib[0] = 40;
        dib[4] = 1;
        dib[8] = 1;
        dib[12] = 1;
        dib[14] = 24;
        dib.extend_from_slice(&[1, 2, 3, 0]);
        let image = Content::new(
            BTreeMap::from([(crate::history::content::DIB.into(), Data::Bytes(dib.into()))]),
            sha256,
        )
        .unwrap();
        let directory = std::env::temp_dir().join(format!(
            "nianlai-as-text-test-{}-{}",
            std::process::id(),
            crate::log::now_ms()
        ));
        let (written, report) = apply(
            &image,
            Mode::Paste,
            Some(TextStyle::Lines),
            Some(&directory),
            sha256,
        );
        let path = written.text_value().unwrap();
        assert!(path.ends_with(".png"), "{path}");
        assert!(std::path::Path::new(&path).is_file());
        assert_eq!(report["conversion"], "image_path");
        assert_eq!(report["png_source"], "dib");
        let (again, report) = apply(&image, Mode::Text, None, Some(&directory), sha256);
        assert_eq!(again.text_value(), Some(path));
        assert_eq!(report["png_reused"], true);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn manual_mode_strips_formatting_and_leaves_plain_text_alone() {
        let utf16 = |text: &str| -> Arc<[u8]> {
            text.encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>()
                .into()
        };
        let rich = Content::new(
            BTreeMap::from([
                (TEXT.into(), Data::Bytes(utf16("加粗 链接"))),
                (HTML.into(), Data::Bytes(b"<b>x</b>".to_vec().into())),
            ]),
            sha256,
        )
        .unwrap();
        let (written, report) = apply(&rich, Mode::Text, None, None, sha256);
        assert_eq!(written.formats.keys().collect::<Vec<_>>(), [TEXT]);
        assert_eq!(written.text_value().as_deref(), Some("加粗 链接"));
        assert_eq!(report["conversion"], "plain_text");

        let plain = Content::text("纯文字", sha256).unwrap();
        let (written, report) = apply(&plain, Mode::Text, None, None, sha256);
        assert!(Arc::ptr_eq(&written, &plain));
        assert_eq!(report["reason"], "already_text");
    }
}
