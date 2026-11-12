// 目标只收文字时的降级：文件给路径，图片存成 PNG 给路径，富文本去掉格式。
use super::{keys::TextStyle, request::Mode};
use crate::{
    history::content::{Content, Data, FILES, HTML, Hash, RTF, TEXT},
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

// 一叠一起放下（automatic 队列）的条目分隔。规则：
// - 除最后一条外，文本类条目（纯文本，及其 HTML/RTF 形态）在末尾加一个分隔；
// - 分隔默认是换行；只有能可靠判断目标是单行编辑框（Win32 Edit/RichEdit/WinForms 文本框且
//   没有 ES_MULTILINE）时用空格——单行框粘贴带换行的文字会在换行处截断，换行等于没分隔；
// - 文件、图片等没有文字格式的条目原样返回（同一 Arc），仍按逐条粘贴；
// - 逐张放（手动 Ctrl+V）不加分隔。
// 重算的指纹就是实际写入的内容：回声登记（before_write）用同一个 Content，不会被当成新复制。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Separator {
    Newline,
    Space,
}

impl Separator {
    pub fn name(self) -> &'static str {
        match self {
            Self::Newline => "newline",
            Self::Space => "space",
        }
    }
}

/// 按目标焦点控件决定分隔。`style` 是焦点窗口的 GWL_STYLE（读不到为 None）。
/// 判断不了（浏览器、Office、终端、自绘控件……）一律换行。
pub fn separator_for(focus_class: &str, style: Option<u32>) -> Separator {
    const ES_MULTILINE: u32 = 0x0004;
    let class = focus_class.to_ascii_lowercase();
    let edit = matches!(
        class.as_str(),
        "edit" | "richedit" | "richedit20a" | "richedit20w" | "richedit50w" | "richedit50a"
    ) || class.starts_with("windowsforms10.edit.")
        || class.starts_with("windowsforms10.richedit");
    match style {
        Some(style) if edit && style & ES_MULTILINE == 0 => Separator::Space,
        _ => Separator::Newline,
    }
}

fn until_nul(bytes: &[u8]) -> &[u8] {
    &bytes[..bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len())]
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// CF_HTML：把 `insert` 放进片段末尾（EndFragment 处），并按原位宽改写头部偏移。
/// 头部格式不认识、偏移越界或位宽放不下新数字时返回 None（调用方丢掉这个格式）。
fn html_insert(data: &[u8], insert: &[u8]) -> Option<Vec<u8>> {
    let data = until_nul(data);
    let header_end = data.iter().position(|byte| *byte == b'<')?;
    let field = |key: &[u8]| -> Option<(usize, usize, i64)> {
        let at = find(&data[..header_end], key)? + key.len();
        let digits = data[at..header_end]
            .iter()
            .take_while(|byte| byte.is_ascii_digit() || **byte == b'-')
            .count();
        let value = std::str::from_utf8(&data[at..at + digits])
            .ok()?
            .parse()
            .ok()?;
        Some((at, at + digits, value))
    };
    let (_, _, start) = field(b"StartFragment:")?;
    let (_, _, end) = field(b"EndFragment:")?;
    if start < header_end as i64 || end < start || end > data.len() as i64 {
        return None;
    }
    let at = end as usize;
    let mut output = data.to_vec();
    for key in [
        &b"StartHTML:"[..],
        b"EndHTML:",
        b"StartFragment:",
        b"EndFragment:",
        b"StartSelection:",
        b"EndSelection:",
    ] {
        let Some((from, to, value)) = field(key) else {
            continue;
        };
        let shifted = value > at as i64 || (value == at as i64 && key.starts_with(b"End"));
        if value < 0 || !shifted {
            continue;
        }
        let next = format!("{:0width$}", value + insert.len() as i64, width = to - from);
        if next.len() != to - from {
            return None;
        }
        output[from..to].copy_from_slice(next.as_bytes());
    }
    output.splice(at..at, insert.iter().copied());
    Some(output)
}

/// RTF：分隔放在最外层右花括号之前，仍属于文档内容。
fn rtf_insert(data: &[u8], insert: &[u8]) -> Option<Vec<u8>> {
    let data = until_nul(data);
    let at = data.iter().rposition(|byte| *byte == b'}')?;
    let mut output = data.to_vec();
    output.splice(at..at, insert.iter().copied());
    Some(output)
}

pub fn append_separator(content: &Arc<Content>, separator: Separator, hash: Hash) -> Arc<Content> {
    let Some(text) = content.text_value() else {
        // 没有纯文本形态（文件、图片、只有自定义格式）：不改，按原样逐条粘贴。
        return content.clone();
    };
    let (plain, html, rtf): (&str, &[u8], &[u8]) = match separator {
        Separator::Newline => ("\r\n", b"<br>", b"\\par "),
        Separator::Space => (" ", b" ", b"{ }"),
    };
    let mut formats = std::collections::BTreeMap::new();
    for (name, format) in &content.formats {
        let data = match (name.as_str(), content.raw(name)) {
            (TEXT, _) => Some(Data::Bytes(
                format!("{text}{plain}")
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>()
                    .into(),
            )),
            // 富文本形态改不了就不写这个格式：目标退回带分隔的其它形态，而不是粘成一串。
            (HTML, Some(bytes)) => html_insert(bytes, html).map(|bytes| Data::Bytes(bytes.into())),
            (RTF, Some(bytes)) => rtf_insert(bytes, rtf).map(|bytes| Data::Bytes(bytes.into())),
            _ => Some(format.data.clone()),
        };
        if let Some(data) = data {
            formats.insert(name.clone(), data);
        }
    }
    Content::new(formats, hash).unwrap_or_else(|_| content.clone())
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

    fn utf16(text: &str) -> Arc<[u8]> {
        text.encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
            .into()
    }

    // 按 CF_HTML 规则拼一份带头部的 HTML（偏移按字节、10 位补零）。
    fn cf_html(fragment: &str) -> Vec<u8> {
        let header_len = "Version:0.9\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n".len();
        let before = "<html><body><!--StartFragment-->";
        let after = "<!--EndFragment--></body></html>";
        let start_fragment = header_len + before.len();
        let end_fragment = start_fragment + fragment.len();
        let end_html = end_fragment + after.len();
        format!(
            "Version:0.9\r\nStartHTML:{header_len:010}\r\nEndHTML:{end_html:010}\r\nStartFragment:{start_fragment:010}\r\nEndFragment:{end_fragment:010}\r\n{before}{fragment}{after}"
        )
        .into_bytes()
    }

    fn offset(data: &[u8], key: &str) -> usize {
        let text = std::str::from_utf8(data).unwrap();
        let at = text.find(key).unwrap() + key.len();
        text[at..at + 10].parse().unwrap()
    }

    // 一叠一起放下（问题 4）：纯文本、HTML 片段、RTF 都在内容里加同一种分隔。
    #[test]
    fn separator_goes_inside_every_text_format() {
        let plain = Content::text("王小拗", sha256).unwrap();
        let separated = append_separator(&plain, Separator::Newline, sha256);
        assert_eq!(separated.text_value().as_deref(), Some("王小拗\r\n"));
        assert_ne!(separated.fingerprint, plain.fingerprint);
        assert_eq!(separated.formats.keys().collect::<Vec<_>>(), [TEXT]);
        let spaced = append_separator(&plain, Separator::Space, sha256);
        assert_eq!(spaced.text_value().as_deref(), Some("王小拗 "));

        let html = cf_html("<b>第一行</b>");
        let rich = Content::new(
            BTreeMap::from([
                (TEXT.into(), Data::Bytes(utf16("第一行"))),
                (HTML.into(), Data::Bytes(html.clone().into())),
                (
                    RTF.into(),
                    Data::Bytes("{\\rtf1 第一行}".as_bytes().to_vec().into()),
                ),
            ]),
            sha256,
        )
        .unwrap();
        let separated = append_separator(&rich, Separator::Newline, sha256);
        assert_eq!(separated.text_value().as_deref(), Some("第一行\r\n"));
        let written = separated.raw(HTML).unwrap();
        let start = offset(written, "StartFragment:");
        let end = offset(written, "EndFragment:");
        assert_eq!(
            &written[start..end],
            "<b>第一行</b><br>".as_bytes(),
            "换行在片段里面"
        );
        assert_eq!(offset(written, "EndHTML:"), written.len());
        assert_eq!(offset(written, "StartHTML:"), offset(&html, "StartHTML:"));
        assert_eq!(
            separated.raw(RTF).unwrap(),
            "{\\rtf1 第一行\\par }".as_bytes()
        );
        assert_eq!(separated.kind, "富文本");
    }

    #[test]
    fn unreadable_rich_formats_are_dropped_instead_of_pasting_without_separator() {
        let rich = Content::new(
            BTreeMap::from([
                (TEXT.into(), Data::Bytes(utf16("甲"))),
                (
                    HTML.into(),
                    Data::Bytes(b"<b>no header</b>".to_vec().into()),
                ),
                (RTF.into(), Data::Bytes(b"no braces".to_vec().into())),
            ]),
            sha256,
        )
        .unwrap();
        let separated = append_separator(&rich, Separator::Newline, sha256);
        assert_eq!(separated.formats.keys().collect::<Vec<_>>(), [TEXT]);
        assert_eq!(separated.text_value().as_deref(), Some("甲\r\n"));
        // 位宽放不下新偏移：同样丢掉 HTML，不写坏头部。
        let header = "StartFragment:34\r\nEndFragment:98\r\n";
        assert_eq!(header.len(), 34);
        let narrow = format!("{header}<{}", "a".repeat(63)).into_bytes();
        assert_eq!(narrow.len(), 98);
        assert_eq!(html_insert(&narrow, b"<br>"), None);
        assert!(html_insert(&narrow, b"").is_some(), "同样的头部本身可解析");
    }

    #[test]
    fn trailing_nul_in_stored_text_does_not_hide_the_separator() {
        let mut bytes = utf16("乙").to_vec();
        bytes.extend_from_slice(&[0, 0]);
        let content = Content::new(
            BTreeMap::from([(TEXT.into(), Data::Bytes(bytes.into()))]),
            sha256,
        )
        .unwrap();
        let separated = append_separator(&content, Separator::Newline, sha256);
        assert_eq!(separated.raw(TEXT).unwrap(), &utf16("乙\r\n")[..]);
    }

    #[test]
    fn separator_leaves_files_and_images_untouched() {
        let content = files(&[r"C:\a.txt"]);
        assert!(
            Arc::ptr_eq(
                &append_separator(&content, Separator::Newline, sha256),
                &content
            ),
            "文件不加分隔"
        );
        let image = Content::new(
            BTreeMap::from([(
                crate::history::content::DIB.into(),
                Data::Bytes(vec![0; 44].into()),
            )]),
            sha256,
        )
        .unwrap();
        assert!(
            Arc::ptr_eq(
                &append_separator(&image, Separator::Newline, sha256),
                &image
            ),
            "图片不加分隔"
        );
    }

    // Ctrl+Enter（pasteMode=text）的 automatic 队列同样分隔：转换成纯文本后再追加。
    #[test]
    fn separator_applies_after_text_conversion() {
        let content = files(&[r"C:\a.txt"]);
        let (written, _) = apply(&content, Mode::Text, None, None, sha256);
        let separated = append_separator(&written, Separator::Newline, sha256);
        assert_eq!(separated.text_value().as_deref(), Some("C:\\a.txt\r\n"));
        assert_ne!(separated.fingerprint, written.fingerprint);
    }

    // 只有能确认是单行 Win32 编辑框才用空格；其它一律换行。
    #[test]
    fn only_verified_single_line_edits_get_a_space() {
        const ES_MULTILINE: u32 = 0x4;
        assert_eq!(separator_for("Edit", Some(0x5001_0080)), Separator::Space);
        assert_eq!(
            separator_for("RICHEDIT50W", Some(0x5001_0000)),
            Separator::Space
        );
        assert_eq!(
            separator_for(
                "WindowsForms10.EDIT.app.0.141b42a_r6_ad1",
                Some(0x5001_0000)
            ),
            Separator::Space
        );
        assert_eq!(
            separator_for("Edit", Some(0x5001_0000 | ES_MULTILINE)),
            Separator::Newline
        );
        assert_eq!(
            separator_for("Edit", None),
            Separator::Newline,
            "样式读不到"
        );
        for class in ["Chrome_RenderWidgetHostHWND", "_WwG", "Scintilla", ""] {
            assert_eq!(
                separator_for(class, Some(0x5001_0000)),
                Separator::Newline,
                "{class}"
            );
        }
    }
}
