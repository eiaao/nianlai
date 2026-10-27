use crate::log::Logger;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum Key {
    #[serde(rename = "Ctrl+V")]
    CtrlV,
    #[serde(rename = "Ctrl+Shift+V")]
    CtrlShiftV,
    #[serde(rename = "Shift+Insert")]
    ShiftInsert,
}

// 目标只收文字时，文件和图片改成路径再粘：Lines 每行一个，Shell 用空格分隔并给需要的路径加引号。
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TextStyle {
    Lines,
    Shell,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    process: Option<String>,
    class: Option<String>,
    key: Key,
    #[serde(default)]
    as_text: Option<TextStyle>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub struct Match {
    pub source: &'static str,
    pub index: usize,
    pub key: Key,
    pub as_text: Option<TextStyle>,
}

pub struct Table {
    builtin: Vec<Rule>,
    overrides: Vec<Rule>,
}

fn parse(text: &str, builtin: bool) -> Result<Vec<Rule>, &'static str> {
    let rules: Vec<Rule> = serde_json::from_str(text).map_err(|_| "invalid_json_or_rule")?;
    if builtin && rules.is_empty() {
        return Err("missing_default");
    }
    for (index, rule) in rules.iter().enumerate() {
        if rule.process.as_ref().is_some_and(|s| s.trim().is_empty())
            || rule.class.as_ref().is_some_and(|s| s.trim().is_empty())
        {
            return Err("empty_match");
        }
        // 无条件规则只能放在末尾；覆盖表也可显式覆盖默认键。
        if rule.process.is_none() && rule.class.is_none() && index + 1 != rules.len() {
            return Err("default_must_be_last");
        }
    }
    if builtin
        && rules
            .last()
            .is_some_and(|r| r.process.is_some() || r.class.is_some())
    {
        return Err("missing_default");
    }
    Ok(rules)
}

impl Table {
    fn from_text(override_text: Option<&str>) -> (Self, Option<&'static str>) {
        let builtin = parse(include_str!("../../paste-keys.json"), true).expect("内置粘贴键表无效");
        let (overrides, error) = match override_text.map(|text| parse(text, false)) {
            Some(Ok(rules)) => (rules, None),
            Some(Err(error)) => (Vec::new(), Some(error)),
            None => (Vec::new(), None),
        };
        (Self { builtin, overrides }, error)
    }

    pub fn load(logger: &Logger) -> Self {
        let path = std::env::var_os("NIANLAI_G0_DATA_DIR")
            .map(|base| Path::new(&base).join("paste-keys.json"))
            .or_else(|| {
                std::env::var_os("LOCALAPPDATA")
                    .map(|base| Path::new(&base).join("Nianlai/g0/paste-keys.json"))
            });
        let (text, read_error) = match path.map(std::fs::read_to_string) {
            Some(Ok(text)) => (Some(text), false),
            Some(Err(error)) if error.kind() != std::io::ErrorKind::NotFound => (None, true),
            _ => (None, false),
        };
        let (table, parse_error) = Self::from_text(text.as_deref());
        logger.event(
            "paste_keys_loaded",
            json!({
                "builtin_rules":table.builtin.len(), "override_rules":table.overrides.len(),
                "override_invalid":read_error || parse_error.is_some(),
                "reason":parse_error.or(if read_error { Some("read_failed") } else { None }),
            }),
        );
        table
    }

    pub fn select(&self, process: &str, class: &str) -> Match {
        for (source, rules) in [("override", &self.overrides), ("builtin", &self.builtin)] {
            for (index, rule) in rules.iter().enumerate() {
                if (rule.process.is_none() && rule.class.is_none())
                    || rule
                        .process
                        .as_ref()
                        .is_some_and(|p| p.eq_ignore_ascii_case(process))
                    || rule.class.as_ref().is_some_and(|c| c == class)
                {
                    return Match {
                        source,
                        index: index + 1,
                        key: rule.key,
                        as_text: rule.as_text,
                    };
                }
            }
        }
        unreachable!("内置表总有末尾默认规则")
    }
    pub fn is_builtin_default_ctrl_v(&self, selected: Match) -> bool {
        self.builtin.last().is_some_and(|rule| {
            rule.process.is_none()
                && rule.class.is_none()
                && rule.key == Key::CtrlV
                && selected.source == "builtin"
                && selected.index == self.builtin.len()
                && selected.key == rule.key
                && selected.as_text == rule.as_text
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_strategy_requires_the_actual_builtin_default_not_a_matching_override() {
        let (table, _) = Table::from_text(None);
        assert!(table.is_builtin_default_ctrl_v(table.select("sample.exe", "Sample")));
        assert!(!table.is_builtin_default_ctrl_v(table.select("Notepad.exe", "Edit")));
        assert!(!table.is_builtin_default_ctrl_v(table.select("WindowsTerminal.exe", "Other")));
        let (table, _) = Table::from_text(Some(r#"[{"key":"Ctrl+V"}]"#));
        assert!(!table.is_builtin_default_ctrl_v(table.select("sample.exe", "Sample")));
    }

    #[test]
    fn builtin_process_class_and_default_rules() {
        let (table, error) = Table::from_text(None);
        assert!(error.is_none());
        assert_eq!(table.select("WINDOWSTERMINAL.EXE", "").key, Key::CtrlShiftV);
        assert_eq!(
            table
                .select("other.exe", "CASCADIA_HOSTING_WINDOW_CLASS")
                .index,
            1
        );
        // 旧控制台默认没开 InterceptCopyPaste，Ctrl+Shift+V 会变成 ^V；Ctrl+V 才粘贴。
        assert_eq!(
            table.select("cmd.exe", "ConsoleWindowClass").key,
            Key::CtrlV
        );
        for process in ["putty.exe", "kitty.exe", "mintty.exe"] {
            assert_eq!(table.select(process, "").key, Key::ShiftInsert);
        }
        assert_eq!(
            table.select("code.exe", "Chrome_WidgetWin_1").key,
            Key::CtrlV
        );
    }

    #[test]
    fn terminals_and_plain_editors_take_non_text_items_as_paths() {
        let (table, _) = Table::from_text(None);
        for (process, class) in [
            ("WindowsTerminal.exe", ""),
            ("cmd.exe", "ConsoleWindowClass"),
            ("putty.exe", ""),
            ("kitty.exe", ""),
            ("mintty.exe", ""),
        ] {
            assert_eq!(table.select(process, class).as_text, Some(TextStyle::Shell));
        }
        for process in ["notepad++.exe", "Notepad.exe"] {
            let selected = table.select(process, "");
            assert_eq!(selected.as_text, Some(TextStyle::Lines));
            assert_eq!(selected.key, Key::CtrlV);
        }
        for process in ["Feishu.exe", "explorer.exe", "WINWORD.EXE", "Code.exe"] {
            assert_eq!(table.select(process, "").as_text, None);
        }
    }

    #[test]
    fn overrides_can_mark_a_target_as_text_only_and_reject_unknown_styles() {
        let (table, error) = Table::from_text(Some(
            r#"[{"process":"sublime_text.exe","key":"Ctrl+V","as_text":"lines"}]"#,
        ));
        assert!(error.is_none());
        assert_eq!(
            table.select("sublime_text.exe", "").as_text,
            Some(TextStyle::Lines)
        );
        let (table, error) = Table::from_text(Some(
            r#"[{"process":"x.exe","key":"Ctrl+V","as_text":"words"}]"#,
        ));
        assert!(error.is_some());
        assert_eq!(table.select("x.exe", "").as_text, None);
    }

    #[test]
    fn overrides_match_in_order_before_builtin_and_conditions_are_or() {
        let (table, error) = Table::from_text(Some(
            r#"[
            {"process":"code.exe","class":"Edit","key":"Shift+Insert"},
            {"process":"CODE.EXE","key":"Ctrl+Shift+V"}
        ]"#,
        ));
        assert!(error.is_none());
        assert_eq!(table.select("CODE.EXE", "").source, "override");
        assert_eq!(table.select("code.exe", "").index, 1);
        assert_eq!(table.select("other.exe", "Edit").key, Key::ShiftInsert);
        assert_eq!(table.select("putty.exe", "").source, "builtin");
    }

    #[test]
    fn invalid_override_is_discarded_in_full() {
        for text in [
            "not json",
            "{}",
            r#"[{"key":"Alt+V"}]"#,
            r#"[{"key":"Ctrl+V"},{"process":"x","key":"Ctrl+V"}]"#,
            r#"[{"process":"","key":"Ctrl+V"}]"#,
            r#"[{"process":"putty.exe","key":"Ctrl+V"},{"typo":true}]"#,
        ] {
            let (table, error) = Table::from_text(Some(text));
            assert!(error.is_some());
            assert_eq!(table.select("putty.exe", "").source, "builtin");
            assert_eq!(table.select("putty.exe", "").key, Key::ShiftInsert);
        }
    }

    #[test]
    fn override_can_explicitly_change_the_default_key() {
        let (table, error) = Table::from_text(Some(r#"[{"key":"Shift+Insert"}]"#));
        assert!(error.is_none());
        assert_eq!(table.select("any.exe", "").source, "override");
        assert_eq!(table.select("any.exe", "").key, Key::ShiftInsert);
    }
}
