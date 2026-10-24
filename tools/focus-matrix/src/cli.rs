use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::PathBuf};

pub const HELP: &str = "focus-matrix [--dry-run | --real-input] [--exe <路径>] [--scenarios <JSON>]
  [--only <ID,...>] [--modes quick,search] [--trials 30] [--hotkey Ctrl+Shift+F12]
  [--out <目录>] [--countdown 10] [--max-minutes 120] [--keep-temp] [--json]
默认只读空跑；真跑会接管输入并由样机覆盖剪贴板。动鼠标、Ctrl+C 或输出目录 STOP 文件中止。
helper-window 是内部子命令，不应手动运行。";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Quick,
    Search,
}
impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Quick => "快速取用",
            Self::Search => "搜索模式",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Options {
    pub real_input: bool,
    pub exe: PathBuf,
    pub scenarios: Option<PathBuf>,
    pub only: Vec<String>,
    pub modes: Vec<Mode>,
    pub trials: u32,
    pub hotkey: String,
    pub out: PathBuf,
    pub countdown: u64,
    pub max_minutes: u64,
    pub keep_temp: bool,
    pub json: bool,
}
impl Options {
    pub fn parse(args: Vec<String>) -> Result<Self, String> {
        let mut o = Self {
            real_input: false,
            exe: "target/release/nianlai.exe".into(),
            scenarios: None,
            only: vec![],
            modes: vec![Mode::Search, Mode::Quick],
            trials: 30,
            hotkey: "Ctrl+Shift+F12".into(),
            out: format!("target/focus-matrix/{}", crate::now_ms()).into(),
            countdown: 10,
            max_minutes: 120,
            keep_temp: false,
            json: false,
        };
        let mut seen = BTreeSet::new();
        let mut iter = args.into_iter();
        while let Some(flag) = iter.next() {
            if !seen.insert(flag.clone()) {
                return Err(format!("参数重复：{flag}"));
            }
            let mut value = || iter.next().ok_or_else(|| format!("{flag} 缺少值"));
            match flag.as_str() {
                "--dry-run" => {}
                "--real-input" => o.real_input = true,
                "--exe" => o.exe = value()?.into(),
                "--scenarios" => o.scenarios = Some(value()?.into()),
                "--only" => {
                    o.only = value()?.split(',').map(str::to_owned).collect();
                    if o.only.iter().any(String::is_empty) {
                        return Err("--only 含空 ID".into());
                    }
                }
                "--modes" => {
                    o.modes = value()?
                        .split(',')
                        .map(|m| match m {
                            "quick" => Ok(Mode::Quick),
                            "search" => Ok(Mode::Search),
                            _ => Err(format!("未知模式：{m}")),
                        })
                        .collect::<Result<_, _>>()?;
                    if o.modes.iter().collect::<BTreeSet<_>>().len() != o.modes.len() {
                        return Err("模式重复".into());
                    }
                }
                "--trials" => o.trials = value()?.parse().map_err(|_| "trials 必须为整数")?,
                "--countdown" => {
                    o.countdown = value()?.parse().map_err(|_| "countdown 必须为整数")?
                }
                "--max-minutes" => {
                    o.max_minutes = value()?.parse().map_err(|_| "max-minutes 必须为整数")?
                }
                "--hotkey" => o.hotkey = value()?,
                "--out" => o.out = value()?.into(),
                "--keep-temp" => o.keep_temp = true,
                "--json" => o.json = true,
                _ => return Err(format!("未知参数：{flag}")),
            }
        }
        if seen.contains("--dry-run") && o.real_input {
            return Err("--dry-run 与 --real-input 互斥".into());
        }
        if !(1..=1000).contains(&o.trials)
            || !(5..=300).contains(&o.countdown)
            || !(1..=1440).contains(&o.max_minutes)
        {
            return Err("trials 范围 1–1000，countdown 范围 5–300，max-minutes 范围 1–1440".into());
        }
        let key = Hotkey::parse(&o.hotkey)?;
        if key == Hotkey::parse("Ctrl+Alt+Shift+Q")? || key == Hotkey::parse("Ctrl+Alt+Shift+L")? {
            return Err("呼出键与样机退出键或采集调试键冲突".into());
        }
        if key.modifiers.is_empty() {
            return Err("呼出键必须带修饰键".into());
        }
        if o.exe.as_os_str().is_empty() || o.out.as_os_str().is_empty() {
            return Err("路径不能为空".into());
        }
        Ok(o)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hotkey {
    pub modifiers: Vec<u16>,
    pub key: u16,
}
impl Hotkey {
    pub fn parse(s: &str) -> Result<Self, String> {
        let mut modifiers = BTreeSet::new();
        let mut key = None;
        for part in s.split('+') {
            let part = part.trim().to_ascii_lowercase();
            let modifier = match part.as_str() {
                "ctrl" | "control" => Some(0x11),
                "shift" => Some(0x10),
                "alt" => Some(0x12),
                "win" | "super" | "meta" => Some(0x5b),
                _ => None,
            };
            if let Some(m) = modifier {
                if !modifiers.insert(m) {
                    return Err("修饰键重复".into());
                }
                continue;
            }
            let vk = match part.as_str() {
                "enter" | "return" => 0x0d,
                "esc" | "escape" => 0x1b,
                "delete" | "del" => 0x2e,
                "backspace" => 0x08,
                "tab" => 0x09,
                "space" => 0x20,
                "home" => 0x24,
                "end" => 0x23,
                "left" => 0x25,
                "up" => 0x26,
                "right" => 0x27,
                "down" => 0x28,
                _ if part.len() == 1 && part.as_bytes()[0].is_ascii_alphanumeric() => {
                    part.as_bytes()[0].to_ascii_uppercase() as u16
                }
                _ if part.starts_with("key")
                    && part.len() == 4
                    && part.as_bytes()[3].is_ascii_alphabetic() =>
                {
                    part.as_bytes()[3].to_ascii_uppercase() as u16
                }
                _ if part.starts_with("digit")
                    && part.len() == 6
                    && part.as_bytes()[5].is_ascii_digit() =>
                {
                    part.as_bytes()[5] as u16
                }
                _ if part.starts_with('f') => {
                    let n = part[1..]
                        .parse::<u16>()
                        .map_err(|_| format!("不支持的按键：{part}"))?;
                    if !(1..=24).contains(&n) {
                        return Err("功能键范围 F1–F24".into());
                    }
                    0x6f + n
                }
                _ => return Err(format!("不支持的按键：{part}")),
            };
            if key.replace(vk).is_some() {
                return Err("组合键必须且只能有一个主键".into());
            }
        }
        Ok(Self {
            modifiers: modifiers.into_iter().collect(),
            key: key.ok_or("缺少主键")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accelerators() {
        assert_eq!(
            Hotkey::parse("Ctrl+Shift+F12").unwrap(),
            Hotkey::parse(" SHIFT + CONTROL + f12 ").unwrap()
        );
        assert_eq!(Hotkey::parse("Control+KeyQ").unwrap().key, 0x51);
        for bad in ["", "ctrl", "Ctrl++A", "Ctrl+Ctrl+A", "A+B", "F25"] {
            assert!(Hotkey::parse(bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn defaults_and_guards() {
        assert!(!Options::parse(vec![]).unwrap().real_input);
        for args in [
            vec!["--real-input", "--dry-run"],
            vec!["--countdown", "4"],
            vec!["--hotkey", "Ctrl+Alt+Shift+Q"],
            vec!["--trials", "0"],
            vec!["--modes", "quick,quick"],
        ] {
            assert!(Options::parse(args.into_iter().map(str::to_owned).collect()).is_err());
        }
    }
}
