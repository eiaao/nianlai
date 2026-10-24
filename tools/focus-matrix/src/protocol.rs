use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub fn normalize(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}
pub fn rename_suffix(
    name: &str,
    selection: Option<&str>,
    filename: &str,
) -> Result<String, String> {
    if let Some(selected) = selection.filter(|s| !s.is_empty()) {
        return name
            .strip_prefix(selected)
            .map(str::to_owned)
            .ok_or_else(|| "重命名选区不是文件名开头".into());
    }
    let (stem, extension) = filename.rsplit_once('.').ok_or("测试文件没有扩展名")?;
    if name == filename {
        Ok(format!(".{extension}"))
    } else if name == stem {
        Ok(String::new())
    } else {
        Err("重命名值不等于本次文件名，不能推断扩展名".into())
    }
}
pub fn fnv(s: &str) -> u32 {
    normalize(s).encode_utf16().fold(0x811c9dc5u32, |h, c| {
        (h ^ c as u32).wrapping_mul(0x01000193)
    })
}
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Digest {
    pub len: usize,
    pub hash: u32,
}
impl Digest {
    pub fn of(s: &str) -> Self {
        Self {
            len: normalize(s).encode_utf16().count(),
            hash: fnv(s),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TitleState {
    pub token: String,
    pub fields: BTreeMap<String, String>,
}
impl TitleState {
    pub fn complete(&self, web: bool) -> bool {
        if self.number("focus").is_err() {
            return false;
        }
        if web {
            ["loads", "submit", "blur", "focus_count"]
                .iter()
                .all(|k| self.number(k).is_ok())
                && self.fields.contains_key("active")
                && ["input", "textarea", "editable"].iter().all(|field| {
                    self.digest(field).is_ok()
                        && ["enter", "esc", "digit", "chars"]
                            .iter()
                            .all(|key| self.number(&format!("{field}_{key}")).is_ok())
                })
        } else {
            self.digest("").is_ok()
                && ["enter", "esc", "digit", "chars", "deact"]
                    .iter()
                    .all(|k| self.number(k).is_ok())
        }
    }
    pub fn parse(title: &str, expected_token: &str) -> Result<Self, String> {
        let start = title.find("NLFM|").ok_or("标题缺少协议起点")?;
        let text = &title[start + 5..];
        let end = text.find("|END").ok_or("标题协议被截断")?;
        let mut fields: BTreeMap<String, String> = BTreeMap::new();
        for pair in text[..end].split('|') {
            let (key, value) = pair.split_once('=').ok_or("标题键值无效")?;
            if key.is_empty() || fields.insert(key.into(), value.into()).is_some() {
                return Err("标题键重复或为空".into());
            }
        }
        let token = fields.get("t").ok_or("标题缺少标记")?.clone();
        if token != expected_token {
            return Err("标题标记不是本次运行".into());
        }
        Ok(Self { token, fields })
    }
    pub fn number(&self, key: &str) -> Result<u64, String> {
        self.fields
            .get(key)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| format!("标题缺少数值 {key}"))
    }
    pub fn digest(&self, field: &str) -> Result<Digest, String> {
        let prefix = if field.is_empty() {
            String::new()
        } else {
            format!("{field}_")
        };
        let hash = self
            .fields
            .get(&format!("{prefix}h"))
            .and_then(|s| u32::from_str_radix(s, 16).ok())
            .ok_or("哈希缺失")?;
        Ok(Digest {
            len: self.number(&format!("{prefix}len"))? as usize,
            hash,
        })
    }
}

pub fn expand(
    template: &str,
    before: &str,
    pasted: &str,
    before_ext: &str,
) -> Result<String, String> {
    let mut result = String::new();
    let mut rest = template;
    while let Some(index) = rest.find('{') {
        result.push_str(&rest[..index]);
        let tail = &rest[index + 1..];
        let end = tail.find('}').ok_or("模板括号未闭合")?;
        result.push_str(match &tail[..end] {
            "before" => before,
            "pasted" => pasted,
            "before_ext" => before_ext,
            _ => return Err("模板只支持 {before}、{pasted}、{before_ext}".into()),
        });
        rest = &tail[end + 1..];
    }
    if rest.contains('}') {
        return Err("模板有多余括号".into());
    }
    result.push_str(rest);
    Ok(normalize(&result))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hashes_utf16() {
        assert_eq!(fnv(""), 0x811c9dc5);
        assert_eq!(fnv("hello"), 0x4f9f2cab);
        assert_eq!(fnv("a\r\nb\rc"), fnv("a\nb\nc"));
        assert_eq!(Digest::of("🚀").len, 2);
        for (text, expected) in [
            ("a\r\nb\rc", 0xd5eb74f7),
            ("🚀", 0x4b328e38),
            ("你好，世界！", 0xdffc7a47),
            ("👩‍💻 🇨🇳", 0x54c7e413),
        ] {
            assert_eq!(fnv(text), expected);
        }
        let units = [0xd83d_u16, 0xde80];
        assert_eq!(
            fnv("🚀"),
            units
                .iter()
                .fold(0x811c9dc5u32, |h, c| (h ^ *c as u32).wrapping_mul(16777619))
        );
    }
    #[test]
    fn title_protocol() {
        let s =
            TitleState::parse("NLFM|t=abc|len=5|h=4f9f2cab|focus=1|END - Chrome", "abc").unwrap();
        assert_eq!(s.digest("").unwrap(), Digest::of("hello"));
        assert!(!s.complete(false));
        assert!(
            TitleState::parse(
                "NLFM|t=abc|len=0|h=811c9dc5|enter=0|esc=0|digit=0|chars=0|deact=0|focus=1|END",
                "abc"
            )
            .unwrap()
            .complete(false)
        );
        for s in ["NLFM|t=bad|END", "NLFM|t=abc|t=abc|END", "NLFM|t=abc|len=1"] {
            assert!(TitleState::parse(s, "abc").is_err());
        }
    }
    #[test]
    fn templates_do_not_reinterpret_values() {
        assert_eq!(
            expand("{before}{pasted}", "{pasted}", "中\r\n文", "").unwrap(),
            "{pasted}中\n文"
        );
        assert_eq!(
            expand("{pasted}{before_ext}", "", "file", ".txt").unwrap(),
            "file.txt"
        );
        assert!(expand("{bad}", "", "", "").is_err());
    }
    #[test]
    fn rename_hidden_extension_and_selection() {
        assert_eq!(
            rename_suffix("NLFM.txt", Some("NLFM"), "NLFM.txt").unwrap(),
            ".txt"
        );
        assert_eq!(rename_suffix("NLFM", None, "NLFM.txt").unwrap(), "");
        assert_eq!(rename_suffix("NLFM.txt", None, "NLFM.txt").unwrap(), ".txt");
        assert_eq!(
            rename_suffix("NLFM.txt", Some("NLFM.txt"), "NLFM.txt").unwrap(),
            ""
        );
        assert!(rename_suffix("other", None, "NLFM.txt").is_err());
    }
}
