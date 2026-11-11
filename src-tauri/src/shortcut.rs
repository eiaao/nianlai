//! 呼出键字符串的解析、保留组合与冲突提示；纯函数，不依赖运行时注册（settings 与 hotkey 共用）。
use crate::platform;
use tauri_plugin_global_shortcut::{Modifiers, Shortcut};

fn modifier(value: &str) -> Option<Modifiers> {
    match value.to_ascii_lowercase().as_str() {
        "ctrl" | "control" | "controlleft" | "controlright" => Some(Modifiers::CONTROL),
        "alt" | "altleft" | "altright" | "option" => Some(Modifiers::ALT),
        "shift" | "shiftleft" | "shiftright" => Some(Modifiers::SHIFT),
        "win" | "super" | "meta" | "metaleft" | "metaright" | "cmd" | "command" => {
            Some(Modifiers::SUPER)
        }
        _ => None,
    }
}

pub fn parse(value: &str) -> Result<Shortcut, String> {
    let mut mods = Modifiers::empty();
    let mut key = None;
    for part in value.trim().split('+').map(str::trim) {
        if part.is_empty() {
            return Err("主键不能为空，请选择一个主键".into());
        }
        if let Some(one) = modifier(part) {
            if key.is_some() {
                return Err("修饰键必须放在主键之前".into());
            }
            if mods.contains(one) {
                return Err("同一个修饰键不能重复选择".into());
            }
            mods |= one;
        } else if key.replace(part).is_some() {
            return Err("只能选择一个主键，不支持按键序列".into());
        }
    }
    let key = key.ok_or("不能只使用修饰键，请选择一个主键")?;
    if mods.is_empty() {
        return Err("至少选择一个修饰键（Ctrl / Alt / Shift / Win）".into());
    }
    // Win 是界面名称，底层解析器使用 Super；主键统一使用物理 Code。
    let mut parts = Vec::new();
    for (flag, name) in [
        (Modifiers::CONTROL, "Ctrl"),
        (Modifiers::ALT, "Alt"),
        (Modifiers::SHIFT, "Shift"),
        (Modifiers::SUPER, "Super"),
    ] {
        if mods.contains(flag) {
            parts.push(name);
        }
    }
    parts.push(key);
    let shortcut: Shortcut = parts
        .join("+")
        .parse()
        .map_err(|_| "主键无效，请选择支持的键")?;
    let key = shortcut.key.to_string();
    if let Some(reason) = reserved(&key, mods) {
        return Err(reason.into());
    }
    if !platform::hook::supported(&shortcut) {
        return Err("此主键暂不受面板钩子支持，请选择其他键".into());
    }
    Ok(shortcut)
}

fn reserved(key: &str, mods: Modifiers) -> Option<&'static str> {
    let ctrl = Modifiers::CONTROL;
    let alt = Modifiers::ALT;
    let shift = Modifiers::SHIFT;
    let win = Modifiers::SUPER;
    if mods == (ctrl | alt | shift) && matches!(key, "KeyQ" | "KeyL") {
        return Some("呼出键不能占用退出键或采集调试键");
    }
    if key == "Delete" && mods.contains(ctrl | alt) {
        return Some("Ctrl+Alt+Del 是 Windows 安全保留键，不能用作呼出键");
    }
    if key == "F12" {
        return Some("F12 由 Windows 保留给调试器，不能用作全局呼出键");
    }
    if (mods == alt || mods == (alt | shift)) && matches!(key, "Tab" | "Escape" | "F4" | "Space") {
        return Some("此组合用于系统窗口切换、关闭或窗口菜单，不能用作呼出键");
    }
    if (mods == ctrl || mods == (ctrl | shift)) && key == "Escape" {
        return Some("此组合用于开始菜单或任务管理器，不能用作呼出键");
    }
    if mods == win
        && matches!(
            key,
            "KeyA"
                | "KeyB"
                | "KeyC"
                | "KeyD"
                | "KeyE"
                | "KeyG"
                | "KeyH"
                | "KeyI"
                | "KeyK"
                | "KeyL"
                | "KeyM"
                | "KeyN"
                | "KeyP"
                | "KeyR"
                | "KeyS"
                | "KeyT"
                | "KeyU"
                | "KeyV"
                | "KeyW"
                | "KeyX"
                | "KeyZ"
                | "Tab"
                | "Space"
                | "Enter"
                | "Escape"
                | "Home"
                | "Pause"
                | "PrintScreen"
                | "Equal"
                | "Minus"
                | "ArrowLeft"
                | "ArrowRight"
                | "ArrowUp"
                | "ArrowDown"
                | "Digit0"
                | "Digit1"
                | "Digit2"
                | "Digit3"
                | "Digit4"
                | "Digit5"
                | "Digit6"
                | "Digit7"
                | "Digit8"
                | "Digit9"
        )
    {
        return Some("此 Win 组合是已知的 Windows 系统快捷键，不能用作呼出键");
    }
    if (mods == (win | shift)
        && matches!(
            key,
            "KeyS" | "KeyM" | "ArrowLeft" | "ArrowRight" | "ArrowUp" | "ArrowDown"
        ))
        || (mods == (win | ctrl)
            && matches!(
                key,
                "KeyD" | "KeyF" | "F4" | "ArrowLeft" | "ArrowRight" | "Enter" | "Space"
            ))
        || (mods == (win | ctrl | shift) && key == "KeyB")
        || (mods == (win | alt) && matches!(key, "KeyR" | "PrintScreen"))
        || (mods.contains(win) && key == "KeyL")
    {
        return Some("此组合是已知的 Windows 系统保留操作，不能用作呼出键");
    }
    None
}

pub fn warning(shortcut: &Shortcut) -> Option<&'static str> {
    let mods = shortcut.mods;
    let key = shortcut.key.to_string();
    if mods == Modifiers::SHIFT {
        Some("仅 Shift 的组合可能影响正常输入；建议同时使用 Ctrl 或 Alt。此提示不阻止保存。")
    } else if (mods == Modifiers::CONTROL || mods == (Modifiers::CONTROL | Modifiers::SHIFT))
        && matches!(
            key.as_str(),
            "KeyA"
                | "KeyC"
                | "KeyF"
                | "KeyN"
                | "KeyO"
                | "KeyP"
                | "KeyS"
                | "KeyT"
                | "KeyV"
                | "KeyW"
                | "KeyX"
                | "KeyY"
                | "KeyZ"
                | "Space"
                | "Tab"
        )
    {
        Some("此组合常用于编辑器、浏览器或输入法，可能覆盖其他应用的操作；仍可保存。")
    } else if mods.contains(Modifiers::CONTROL | Modifiers::ALT) {
        Some("Ctrl+Alt 组合可能与其他应用或 AltGr 输入冲突；仍可保存。")
    } else if mods == Modifiers::ALT {
        Some("此组合可能与其他应用的菜单或快捷键冲突；仍可保存。")
    } else if mods.contains(Modifiers::SUPER) {
        Some("Win 组合的可用性受 Windows 版本和系统设置影响，未列出的系统组合仍可能注册失败。")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(value: &str) -> Shortcut {
        parse(value).unwrap()
    }

    #[test]
    fn validation_requires_modifiers_and_exactly_one_supported_main_key() {
        for (value, reason) in [
            ("", "主键不能为空"),
            ("Ctrl+", "主键不能为空"),
            ("Ctrl+Shift", "不能只使用修饰键"),
            ("Win", "不能只使用修饰键"),
            ("Ctrl+ShiftLeft", "不能只使用修饰键"),
            ("A", "至少选择一个修饰键"),
            ("Ctrl+A+B", "只能选择一个主键"),
            ("Ctrl+Ctrl+A", "不能重复"),
            ("Ctrl+not-a-key", "主键无效"),
        ] {
            assert!(parse(value).unwrap_err().contains(reason), "{value}");
        }
        for value in [
            "Ctrl+`",
            "control+Backquote",
            "Alt+V",
            "Ctrl+Shift+F11",
            "Win+F8",
            "Shift+KeyA",
            "Ctrl+Numpad3",
        ] {
            assert!(parse(value).is_ok(), "{value}");
        }
        assert_eq!(key("Win+F8"), key("Super+F8"));
    }

    #[test]
    fn reserved_combinations_are_blocked_but_common_application_conflicts_only_warn() {
        for value in [
            "Win+L",
            "Win+Shift+L",
            "Super+V",
            "Ctrl+Alt+Delete",
            "Ctrl+Alt+Shift+Delete",
            "Alt+Tab",
            "Alt+F4",
            "Ctrl+Shift+Escape",
            "Ctrl+F12",
            "Win+Shift+S",
            "Win+Ctrl+Shift+B",
            "Ctrl+Alt+Shift+Q",
            "Ctrl+Alt+Shift+L",
        ] {
            assert!(parse(value).is_err(), "{value}");
        }
        for value in [
            "Ctrl+V",
            "Ctrl+C",
            "Ctrl+Shift+P",
            "Ctrl+Space",
            "Alt+V",
            "Ctrl+Alt+V",
        ] {
            let shortcut = key(value);
            assert!(warning(&shortcut).is_some(), "{value}");
        }
    }
}
