use crate::{history::content::*, platform::Result};
use windows::{
    Win32::{
        Foundation::{GetLastError, SetLastError, WIN32_ERROR},
        Globalization::{CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS, MultiByteToWideChar},
        System::DataExchange::{
            EnumClipboardFormats, GetClipboardFormatNameW, RegisterClipboardFormatW,
        },
    },
    core::PCWSTR,
};

pub const MIB: usize = 1024 * 1024;

pub fn limit(name: &str) -> Option<usize> {
    match name {
        TEXT | HTML | RTF => Some(2 * MIB),
        PNG => Some(20 * MIB),
        DIB | DIBV5 => Some(64 * MIB),
        FILES => Some(usize::MAX),
        _ => None,
    }
}

pub fn log_name(name: &str) -> String {
    name.chars().take(64).collect()
}

pub fn name(id: u32) -> String {
    let standard = match id {
        1 => "CF_TEXT",
        2 => "CF_BITMAP",
        3 => "CF_METAFILEPICT",
        4 => "CF_SYLK",
        5 => "CF_DIF",
        6 => "CF_TIFF",
        7 => "CF_OEMTEXT",
        8 => DIB,
        9 => "CF_PALETTE",
        10 => "CF_PENDATA",
        11 => "CF_RIFF",
        12 => "CF_WAVE",
        13 => TEXT,
        14 => "CF_ENHMETAFILE",
        15 => FILES,
        16 => "CF_LOCALE",
        17 => DIBV5,
        0x80 => "CF_OWNERDISPLAY",
        0x81 => "CF_DSPTEXT",
        0x82 => "CF_DSPBITMAP",
        0x83 => "CF_DSPMETAFILEPICT",
        0x8e => "CF_DSPENHMETAFILE",
        _ => "",
    };
    if !standard.is_empty() {
        return standard.into();
    }
    if id >= 0xc000 {
        let mut buffer = [0; 256];
        let length = unsafe { GetClipboardFormatNameW(id, &mut buffer) }.max(0) as usize;
        if length > 0 {
            return String::from_utf16_lossy(&buffer[..length]);
        }
    }
    format!("CF_UNKNOWN_{id}")
}

/// 失败时返回 Win32 错误码和失败前已经列出的格式数。
pub fn enumerate() -> std::result::Result<Vec<(u32, String)>, (u32, usize)> {
    let mut formats = Vec::new();
    let mut previous = 0;
    loop {
        unsafe {
            SetLastError(WIN32_ERROR(0));
        }
        let id = unsafe { EnumClipboardFormats(previous) };
        if id == 0 {
            let code = unsafe { GetLastError() }.0;
            return if code == 0 {
                Ok(formats)
            } else {
                Err((code, formats.len()))
            };
        }
        formats.push((id, name(id)));
        previous = id;
    }
}

pub fn register(name: &str) -> Result<u32> {
    let standard = match name {
        TEXT => Some(13),
        DIB => Some(8),
        DIBV5 => Some(17),
        FILES => Some(15),
        _ => None,
    };
    if let Some(id) = standard {
        return Ok(id);
    }
    let wide: Vec<_> = name.encode_utf16().chain(Some(0)).collect();
    let id = unsafe { RegisterClipboardFormatW(PCWSTR(wide.as_ptr())) };
    if id == 0 {
        Err("clipboard_register_format_failed".into())
    } else {
        Ok(id)
    }
}

pub fn nul_length(data: &[u8], wide: bool) -> Option<usize> {
    if wide {
        if let Some(index) = data
            .as_chunks::<2>()
            .0
            .iter()
            .position(|unit| *unit == [0, 0])
        {
            return Some(index * 2);
        }
        data.len().is_multiple_of(2).then_some(data.len())
    } else {
        Some(
            data.iter()
                .position(|byte| *byte == 0)
                .unwrap_or(data.len()),
        )
    }
}

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        data.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}
fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

pub fn png_length(data: &[u8]) -> Option<usize> {
    if data.get(..8)? != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let mut at: usize = 8;
    let mut first = true;
    loop {
        let length =
            u32::from_be_bytes(data.get(at..at.checked_add(4)?)?.try_into().ok()?) as usize;
        let kind = data.get(at + 4..at + 8)?;
        if first && (kind != b"IHDR" || length != 13) {
            return None;
        }
        first = false;
        let end = at.checked_add(12)?.checked_add(length)?;
        data.get(..end)?;
        if kind == b"IEND" {
            return (length == 0).then_some(end);
        }
        at = end;
    }
}

pub fn dib_length(data: &[u8]) -> Option<usize> {
    let header = u32_at(data, 0)? as usize;
    data.get(..header)?;
    let (width, height, bits, compression, colors, image_size, entry_size) = if header == 12 {
        if u16_at(data, 8)? != 1 {
            return None;
        }
        (
            u16_at(data, 4)? as usize,
            u16_at(data, 6)? as usize,
            u16_at(data, 10)? as usize,
            0,
            0,
            0,
            3,
        )
    } else if matches!(header, 40 | 52 | 56 | 108 | 124) {
        let width = u32_at(data, 4)? as i32;
        let height = u32_at(data, 8)? as i32;
        if width <= 0 || height == 0 || height == i32::MIN || u16_at(data, 12)? != 1 {
            return None;
        }
        (
            width as usize,
            height.unsigned_abs() as usize,
            u16_at(data, 14)? as usize,
            u32_at(data, 16)?,
            u32_at(data, 32)? as usize,
            u32_at(data, 20)? as usize,
            4,
        )
    } else {
        return None;
    };
    if width == 0 || height == 0 || !matches!(bits, 0 | 1 | 4 | 8 | 16 | 24 | 32) {
        return None;
    }
    let colors = if colors != 0 {
        colors
    } else if (1..=8).contains(&bits) {
        1usize << bits
    } else {
        0
    };
    if bits <= 8 && bits != 0 && colors > (1usize << bits) {
        return None;
    }
    let masks = if header == 40 {
        match compression {
            3 => 12,
            6 => 16,
            _ => 0,
        }
    } else {
        0
    };
    let mut start = header
        .checked_add(masks)?
        .checked_add(colors.checked_mul(entry_size)?)?;
    let pixels = match compression {
        0 | 3 | 6 if bits != 0 => width
            .checked_mul(bits)?
            .checked_add(31)?
            .checked_div(32)?
            .checked_mul(4)?
            .checked_mul(height)?,
        1..=5 if image_size != 0 => image_size,
        _ => return None,
    };
    let mut profile_end = 0;
    if header == 124 {
        let offset = u32_at(data, 112)? as usize;
        let size = u32_at(data, 116)? as usize;
        if size != 0 {
            if offset < start {
                return None;
            }
            profile_end = offset.checked_add(size)?;
            if offset == start {
                start = profile_end;
            } else if offset < start.checked_add(pixels)? {
                return None;
            }
        }
    }
    let end = start.checked_add(pixels)?.max(profile_end);
    data.get(..end).map(|_| end)
}

pub fn effective_length(name: &str, data: &[u8]) -> Option<usize> {
    match name {
        TEXT => nul_length(data, true),
        HTML | RTF => nul_length(data, false),
        PNG => png_length(data),
        DIB | DIBV5 => dib_length(data),
        _ => None,
    }
}

pub fn parse_dropfiles(data: &[u8]) -> Option<Vec<String>> {
    let offset = u32_at(data, 0)? as usize;
    let wide = u32_at(data, 16)? != 0;
    if offset < 20 {
        return None;
    }
    let payload = data.get(offset..)?;
    let mut paths = Vec::new();
    if wide {
        if !offset.is_multiple_of(2) {
            return None;
        }
        let units: Vec<_> = payload
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        let mut at = 0;
        loop {
            let remaining = units.get(at..)?;
            let length = remaining.iter().position(|unit| *unit == 0)?;
            if length == 0 {
                return (!paths.is_empty()).then_some(paths);
            }
            paths.push(String::from_utf16(&remaining[..length]).ok()?);
            at += length + 1;
        }
    } else {
        let mut at = 0;
        loop {
            let remaining = payload.get(at..)?;
            let length = remaining.iter().position(|byte| *byte == 0)?;
            if length == 0 {
                return (!paths.is_empty()).then_some(paths);
            }
            let text = &remaining[..length];
            if text.len() > i32::MAX as usize {
                return None;
            }
            let count = unsafe {
                MultiByteToWideChar(CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), text, None)
            };
            if count <= 0 {
                return None;
            }
            let mut buffer = vec![0; count as usize];
            if unsafe {
                MultiByteToWideChar(
                    CP_ACP,
                    MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0),
                    text,
                    Some(&mut buffer),
                )
            } != count
            {
                return None;
            }
            paths.push(String::from_utf16(&buffer).ok()?);
            at += length + 1;
        }
    }
}

pub fn build_dropfiles(paths: &[String]) -> Result<Vec<u8>> {
    if paths.is_empty()
        || paths
            .iter()
            .any(|path| path.is_empty() || path.contains('\0'))
    {
        return Err("invalid_file_list".into());
    }
    let mut bytes = vec![0; 20];
    bytes[..4].copy_from_slice(&20u32.to_le_bytes());
    bytes[16..20].copy_from_slice(&1u32.to_le_bytes());
    for path in paths {
        bytes.extend(
            path.encode_utf16()
                .chain(Some(0))
                .flat_map(u16::to_le_bytes),
        );
    }
    bytes.extend_from_slice(&[0, 0]);
    Ok(bytes)
}

#[cfg(test)]
mod tests;
