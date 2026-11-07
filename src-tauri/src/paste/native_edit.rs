// 标准 Edit 的初始运输选择与固定 Unicode buffer；不访问窗口、输入或剪贴板。
use std::{fmt, time::Duration};

pub const MAX_UNITS: usize = 32767;
pub const QUERY_BUDGET: Duration = Duration::from_millis(200);
pub const QUERY_FAILED: &str = "native_edit_query_failed";
pub const QUERY_TIMEOUT: &str = "native_edit_query_timeout";
pub const STATE_CHANGED: &str = "native_edit_state_changed";

// Win32 EDIT style bits. Keeping classification pure also makes it usable off desktop.
const MULTILINE: u32 = 0x0004;
const UPPERCASE: u32 = 0x0008;
const LOWERCASE: u32 = 0x0010;
const PASSWORD: u32 = 0x0020;
const AUTOHSCROLL: u32 = 0x0080;
const OEMCONVERT: u32 = 0x0400;
const READONLY: u32 = 0x0800;
const NUMBER: u32 = 0x2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style(u32);
impl Style {
    pub fn from_bits(bits: u32) -> Self {
        Self(
            bits & (MULTILINE
                | UPPERCASE
                | LOWERCASE
                | PASSWORD
                | AUTOHSCROLL
                | OEMCONVERT
                | READONLY
                | NUMBER),
        )
    }
    fn check(self) -> Result<(), &'static str> {
        if self.0 & READONLY != 0 {
            Err("native_edit_read_only")
        } else {
            Ok(())
        }
    }
    pub fn compatible(self, text: &Text) -> bool {
        self.0 & (PASSWORD | UPPERCASE | LOWERCASE | OEMCONVERT | NUMBER) == 0
            && (self.0 & MULTILINE != 0 || (self.0 & AUTOHSCROLL != 0 && !text.line_breaks))
    }
}

pub struct Text {
    terminated: Vec<u16>,
    line_breaks: bool,
}
impl fmt::Debug for Text {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Text")
            .field("units", &self.payload_units())
            .finish()
    }
}
impl Text {
    pub fn payload_units(&self) -> usize {
        self.terminated.len() - 1
    }
    pub fn terminated(&self) -> &[u16] {
        &self.terminated
    }
}

// None is known compatibility: preserve the old WM_PASTE path, never truncate/normalize.
pub fn prepare(bytes: &[u8]) -> Result<Option<Text>, &'static str> {
    if !bytes.len().is_multiple_of(2) {
        return Err("native_edit_invalid_text");
    }
    let mut units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    if char::decode_utf16(units.iter().copied()).any(|unit| unit.is_err()) {
        return Err("native_edit_invalid_text");
    }
    if units.is_empty() || units.len() > MAX_UNITS || units.contains(&0) {
        return Ok(None);
    }
    let mut line_breaks = false;
    let mut index = 0;
    while index < units.len() {
        match units[index] {
            13 if units.get(index + 1) == Some(&10) => {
                line_breaks = true;
                index += 2;
            }
            10 | 13 => return Ok(None),
            _ => index += 1,
        }
    }
    units.push(0);
    Ok(Some(Text {
        terminated: units,
        line_breaks,
    }))
}

pub fn legacy_length(length: usize) -> Result<bool, &'static str> {
    if length > i32::MAX as usize {
        Err(QUERY_FAILED)
    } else {
        Ok(length > MAX_UNITS)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot {
    style: Style,
    length: usize,
    start: usize,
    end: usize,
    limit: usize,
    password_character: usize,
}
impl Snapshot {
    pub fn from_queries(
        style: Style,
        length: usize,
        packed: usize,
        limit: usize,
        password_character: usize,
    ) -> Result<Self, &'static str> {
        style.check()?;
        // EM_GETSEL's packed result has only 16-bit boundaries; never pretend otherwise.
        if length > MAX_UNITS
            || packed >= u32::MAX as usize
            || limit == 0
            || limit > u32::MAX as usize
            || password_character > u16::MAX as usize
        {
            return Err(QUERY_FAILED);
        }
        let start = packed & 0xffff;
        let end = (packed >> 16) & 0xffff;
        if start > end || end > length {
            return Err(QUERY_FAILED);
        }
        Ok(Self {
            style,
            length,
            start,
            end,
            limit,
            password_character,
        })
    }
}

#[derive(Debug)]
pub enum NativeTransport {
    WmPaste { style: Style },
    ReplaceSel { text: Text, snapshot: Snapshot },
}
impl NativeTransport {
    pub fn wm_paste(style: Style) -> Result<Self, &'static str> {
        style.check()?;
        Ok(Self::WmPaste { style })
    }
    pub fn choose(text: Text, snapshot: Snapshot) -> Result<Self, &'static str> {
        snapshot.style.check()?;
        let resulting = snapshot
            .length
            .checked_sub(snapshot.end - snapshot.start)
            .and_then(|length| length.checked_add(text.payload_units()))
            .ok_or(QUERY_FAILED)?;
        if !snapshot.style.compatible(&text)
            || snapshot.password_character != 0
            || resulting > MAX_UNITS
            || resulting > snapshot.limit
        {
            // This choice happens BEFORE the one target mutation, not after failed delivery.
            Self::wm_paste(snapshot.style)
        } else {
            Ok(Self::ReplaceSel { text, snapshot })
        }
    }
    pub fn method(&self) -> &'static str {
        match self {
            Self::WmPaste { .. } => "wm_paste",
            Self::ReplaceSel { .. } => "em_replacesel",
        }
    }
    pub fn validate_style(expected: Style, observed: Style) -> Result<(), &'static str> {
        observed.check()?;
        if expected == observed {
            Ok(())
        } else {
            Err(STATE_CHANGED)
        }
    }
    pub fn validate_snapshot(expected: Snapshot, observed: Snapshot) -> Result<(), &'static str> {
        Self::validate_style(expected.style, observed.style)?;
        if expected == observed {
            Ok(())
        } else {
            Err(STATE_CHANGED)
        }
    }
}

#[derive(Clone, Copy)]
pub struct QueryBudget {
    started: Duration,
}
impl QueryBudget {
    pub fn new(now: Duration) -> Self {
        Self { started: now }
    }
    pub fn remaining_ms(self, now: Duration) -> Result<u32, &'static str> {
        let remaining = QUERY_BUDGET
            .saturating_sub(now.saturating_sub(self.started))
            .as_millis();
        if remaining == 0 {
            Err(QUERY_TIMEOUT)
        } else {
            Ok(remaining as u32)
        }
    }
}

#[cfg(test)]
#[path = "native_edit_tests.rs"]
mod tests;
