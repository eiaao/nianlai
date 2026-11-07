//! 来源应用的显示名与图标（A06）。
//!
//! 采集线程只把 exe 路径随事件写进库（一次点查加可选插入，不拖慢“复制到已保存”）；
//! FileDescription、图标提取和 PNG 缓存都在这里的后台线程完成。图标缓存键由路径加文件
//! 修改时间决定，exe 升级后键变化、重新提取。隐私：只读进程与程序信息，不碰窗口标题。

use crate::history::Shared;
use crate::log::Logger;
use serde_json::json;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

// 同一路径在这个窗口内不重复提取；路径是完整 exe 路径，数量有限。
const REFRESH_DEBOUNCE: Duration = Duration::from_secs(30);

// 图标缓存键：路径 + 文件修改时间的 SHA256 前 16 字节。exe 升级（修改时间变）后键变化。
pub fn icon_key(exe_path: &str, modified: SystemTime) -> String {
    let millis = modified
        .duration_since(UNIX_EPOCH)
        .map(|passed| passed.as_millis() as u64)
        .unwrap_or(0);
    let mut input = Vec::with_capacity(exe_path.len() + 8);
    input.extend_from_slice(exe_path.as_bytes());
    input.extend_from_slice(&millis.to_le_bytes());
    crate::platform::hash::sha256(&input)
        .map(|fingerprint| {
            fingerprint[..8]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        })
        .unwrap_or_else(|_| format!("unhashed-{millis:x}"))
}

// exe 版本信息的 FileDescription（本地化）；没有版本信息或没有该字段时返回 None。
pub fn file_description(exe_path: &str) -> Option<String> {
    use windows::Win32::Storage::FileSystem::{
        FILE_VER_GET_LOCALISED, GetFileVersionInfoExW, GetFileVersionInfoSizeExW, VerQueryValueW,
    };
    use windows::core::PCWSTR;
    let wide: Vec<u16> = exe_path.encode_utf16().chain([0]).collect();
    let size = unsafe {
        GetFileVersionInfoSizeExW(
            FILE_VER_GET_LOCALISED,
            PCWSTR(wide.as_ptr()),
            std::ptr::null_mut(),
        )
    };
    if size == 0 {
        return None;
    }
    let mut data = vec![0u8; size as usize];
    unsafe {
        GetFileVersionInfoExW(
            FILE_VER_GET_LOCALISED,
            PCWSTR(wide.as_ptr()),
            None,
            size,
            data.as_mut_ptr().cast(),
        )
        .ok()?;
    }
    let mut block = std::ptr::null_mut();
    let mut length = 0u32;
    if !unsafe {
        VerQueryValueW(
            data.as_ptr().cast(),
            windows::core::w!("\\VarFileInfo\\Translation"),
            &mut block,
            &mut length,
        )
    }
    .as_bool()
        || length < 4
    {
        return None;
    }
    let translation = block.cast::<u8>();
    let language = u16::from_le_bytes([unsafe { *translation }, unsafe { *translation.add(1) }]);
    let codepage = u16::from_le_bytes([unsafe { *translation.add(2) }, unsafe {
        *translation.add(3)
    }]);
    let query: Vec<u16> =
        format!("\\StringFileInfo\\{language:04x}{codepage:04x}\\FileDescription")
            .encode_utf16()
            .chain([0])
            .collect();
    let mut value = std::ptr::null_mut();
    let mut value_len = 0u32;
    if !unsafe {
        VerQueryValueW(
            data.as_ptr().cast(),
            PCWSTR(query.as_ptr()),
            &mut value,
            &mut value_len,
        )
    }
    .as_bool()
        || value_len == 0
    {
        return None;
    }
    let units = unsafe { std::slice::from_raw_parts(value.cast::<u16>(), value_len as usize) };
    let text = String::from_utf16_lossy(
        &units[..units
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(units.len())],
    );
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

// 图标边长：基准 32 像素，按系统 DPI 放大，限制在 16–64。
fn icon_size() -> i32 {
    let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForSystem() }.max(96);
    ((32 * dpi as i64 / 96) as i32).clamp(16, 64)
}

// 从 exe 提取图标并编码成 PNG；没有图标资源或路径超长时返回 None。
pub fn extract_icon_png(exe_path: &str, size: i32) -> Option<Vec<u8>> {
    use windows::Win32::UI::WindowsAndMessaging::{
        DestroyIcon, LR_DEFAULTCOLOR, PrivateExtractIconsW,
    };
    // PrivateExtractIconsW 的路径按 MAX_PATH 处理。
    let units: Vec<u16> = exe_path.encode_utf16().collect();
    if units.len() >= 260 {
        return None;
    }
    let mut wide = [0u16; 260];
    wide[..units.len()].copy_from_slice(&units);
    let mut icon = windows::Win32::UI::WindowsAndMessaging::HICON::default();
    let extracted = unsafe {
        PrivateExtractIconsW(
            &wide,
            0,
            size,
            size,
            Some(std::slice::from_mut(&mut icon)),
            None,
            LR_DEFAULTCOLOR.0,
        )
    };
    if extracted != 1 || icon.is_invalid() {
        return None;
    }
    let png = icon_pixels(icon, size).map(|(width, height, rgba)| encode_png(width, height, &rgba));
    let _ = unsafe { DestroyIcon(icon) };
    png
}

// HICON → 自顶向下 32 位 RGBA。老图标的颜色位图 alpha 全零，用单色掩码恢复透明区域。
fn icon_pixels(
    icon: windows::Win32::UI::WindowsAndMessaging::HICON,
    size: i32,
) -> Option<(u32, u32, Vec<u8>)> {
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, CreateCompatibleDC, DIB_RGB_COLORS, GetDIBits,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetIconInfo;
    let mut info = windows::Win32::UI::WindowsAndMessaging::ICONINFO::default();
    unsafe { GetIconInfo(icon, &mut info).ok()? };
    let _color = BitmapGuard(info.hbmColor);
    let _mask = BitmapGuard(info.hbmMask);
    if info.hbmColor.is_invalid() {
        return None;
    }
    let hdc = unsafe { CreateCompatibleDC(None) };
    if hdc.is_invalid() {
        return None;
    }
    let _dc = DcGuard(hdc);
    let header = windows::Win32::Graphics::Gdi::BITMAPINFOHEADER {
        biSize: std::mem::size_of::<windows::Win32::Graphics::Gdi::BITMAPINFOHEADER>() as u32,
        biWidth: size,
        biHeight: -size,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        ..Default::default()
    };
    let mut color = BITMAPINFO {
        bmiHeader: header,
        ..Default::default()
    };
    let mut pixels = vec![0u8; (size as usize) * (size as usize) * 4];
    if unsafe {
        GetDIBits(
            hdc,
            info.hbmColor,
            0,
            size as u32,
            Some(pixels.as_mut_ptr().cast()),
            &mut color,
            DIB_RGB_COLORS,
        )
    } != size
    {
        return None;
    }
    let width = color.bmiHeader.biWidth.unsigned_abs();
    let height = color.bmiHeader.biHeight.unsigned_abs();
    // 单色掩码（1 位/像素，行按 4 字节对齐）：位为 1 表示透明。
    let stride = (width as usize).div_ceil(32) * 4;
    let mut mask = vec![0u8; stride * height as usize];
    let mut mask_info = BITMAPINFO {
        bmiHeader: windows::Win32::Graphics::Gdi::BITMAPINFOHEADER {
            biSize: std::mem::size_of::<windows::Win32::Graphics::Gdi::BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 1,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mask_ready = unsafe {
        GetDIBits(
            hdc,
            info.hbmMask,
            0,
            height,
            Some(mask.as_mut_ptr().cast()),
            &mut mask_info,
            DIB_RGB_COLORS,
        )
    } == height as i32;
    let mut has_alpha = false;
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2); // BGRA → RGBA
        has_alpha |= pixel[3] != 0;
    }
    if !has_alpha && mask_ready {
        for y in 0..height as usize {
            for x in 0..width as usize {
                let transparent = mask[y * stride + x / 8] & (0x80 >> (x % 8)) != 0;
                pixels[(y * width as usize + x) * 4 + 3] = u8::from(!transparent) * 255;
            }
        }
    }
    Some((width, height, pixels))
}

struct BitmapGuard(windows::Win32::Graphics::Gdi::HBITMAP);
impl Drop for BitmapGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(self.0.into());
        }
    }
}
struct DcGuard(windows::Win32::Graphics::Gdi::HDC);
impl Drop for DcGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::DeleteDC(self.0);
        }
    }
}

// PNG（RGBA、8 位、stored deflate 块）：合法 PNG，浏览器可解码。32×32 约 4 KB、64×64 约
// 16 KB，图标缓存不在乎体积，换来零第三方依赖。
pub(crate) fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let row = (width as usize) * 4;
    let mut raw = Vec::with_capacity((row + 1) * height as usize);
    for line in rgba.chunks_exact(row) {
        raw.push(0); // 每行滤波类型：None
        raw.extend_from_slice(line);
    }
    let mut png = Vec::with_capacity(raw.len() + 128);
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    append_chunk(&mut png, b"IHDR", &ihdr);
    append_chunk(&mut png, b"IDAT", &zlib_stored(&raw));
    append_chunk(&mut png, b"IEND", &[]);
    png
}

fn append_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc_input);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

// zlib 流：固定头 + 不压缩的 stored 块 + Adler-32。块最大 65535 字节。
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 65535 * 5 + 6);
    out.extend_from_slice(&[0x78, 0x01]);
    let mut blocks = data.chunks(65_535).peekable();
    if data.is_empty() {
        out.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    while let Some(block) = blocks.next() {
        let last = blocks.peek().is_none();
        out.push(u8::from(last));
        out.extend_from_slice(&(block.len() as u16).to_le_bytes());
        out.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        out.extend_from_slice(block);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in data {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut low, mut high) = (1u32, 0u32);
    for byte in data {
        low = (low + *byte as u32) % 65_521;
        high = (high + low) % 65_521;
    }
    (high << 16) | low
}

// 后台刷新器：采集线程把 exe 路径丢进通道（非阻塞），这里串行完成提取与落库。
#[derive(Clone)]
pub struct Refresher {
    sender: mpsc::Sender<String>,
}

impl Refresher {
    pub fn refresh(&self, exe_path: &str) {
        if !exe_path.is_empty() {
            let _ = self.sender.send(exe_path.to_owned());
        }
    }
}

pub fn start(shared: Arc<Shared>, logger: Logger) -> Refresher {
    let (sender, receiver) = mpsc::channel::<String>();
    let spawn_logger = logger.clone();
    let spawned = std::thread::Builder::new()
        .name("source-apps".into())
        .spawn(move || run(receiver, &shared, &spawn_logger));
    if spawned.is_err() {
        logger.event("source_apps_thread_failed", json!({}));
    }
    Refresher { sender }
}

fn run(receiver: mpsc::Receiver<String>, shared: &Shared, logger: &Logger) {
    let mut seen: HashMap<String, Instant> = HashMap::new();
    while let Ok(exe_path) = receiver.recv() {
        if seen.len() > 256 {
            seen.retain(|_, at| at.elapsed() < REFRESH_DEBOUNCE);
        }
        if seen
            .get(&exe_path)
            .is_some_and(|at| at.elapsed() < REFRESH_DEBOUNCE)
        {
            continue;
        }
        seen.insert(exe_path.clone(), Instant::now());
        refresh_one(&exe_path, shared, logger);
    }
}

fn refresh_one(exe_path: &str, shared: &Shared, logger: &Logger) {
    let started = Instant::now();
    let outcome = (|| -> Result<&'static str, String> {
        let Some(store) = shared.store() else {
            return Ok("no_store");
        };
        let reader = store.reader().map_err(|e| e.to_string())?;
        let app = reader
            .source_app_by_path(exe_path)
            .map_err(|e| e.to_string())?
            .ok_or("app_missing")?;
        let modified = fs::metadata(exe_path)
            .and_then(|meta| meta.modified())
            .map_err(|_| "metadata".to_owned())?;
        let key = icon_key(exe_path, modified);
        let display = file_description(exe_path).unwrap_or_else(|| app.display_name.clone());
        if app.icon_key == key && app.display_name == display {
            return Ok("unchanged");
        }
        let icons = shared.icons().ok_or_else(|| "icons_dir".to_owned())?;
        let icon_file = if app.icon_key == key && !app.icon_file.is_empty() {
            app.icon_file.clone()
        } else {
            match extract_icon_png(exe_path, icon_size()) {
                Some(png) => {
                    write_icon(&icons, &key, &png).map_err(|_| "icon_write".to_owned())?;
                    format!("{key}.png")
                }
                None => String::new(),
            }
        };
        store
            .refresh_source_app(app.id, display, key, icon_file)
            .map_err(|e| e.to_string())?;
        Ok("refreshed")
    })();
    logger.event(
        "source_app_refresh",
        json!({"outcome":outcome,"exe_bytes":exe_path.len(),"elapsed_ms":started.elapsed().as_secs_f64()*1000.0}),
    );
}

// 图标先写成临时文件再改名：webview 不会读到半个 PNG。
fn write_icon(icons: &Path, key: &str, png: &[u8]) -> std::io::Result<()> {
    fs::create_dir_all(icons)?;
    let target: PathBuf = icons.join(format!("{key}.png"));
    let pending = icons.join(format!("{key}.png.pending"));
    fs::write(&pending, png)?;
    fs::rename(&pending, &target)
}

#[cfg(test)]
mod tests {
    use super::*;

    // stored deflate 能原样解回：手写解包器验证往返。
    fn unpack_stored(zlib: &[u8]) -> Vec<u8> {
        assert_eq!(&zlib[..2], &[0x78, 0x01]);
        let mut out = Vec::new();
        let mut at = 2;
        loop {
            let last = zlib[at];
            let length = u16::from_le_bytes([zlib[at + 1], zlib[at + 2]]);
            let complement = u16::from_le_bytes([zlib[at + 3], zlib[at + 4]]);
            assert_eq!(!length, complement);
            out.extend_from_slice(&zlib[at + 5..at + 5 + length as usize]);
            at += 5 + length as usize;
            if last == 1 {
                break;
            }
        }
        assert_eq!(adler32(&out).to_be_bytes(), zlib[at..at + 4]);
        out
    }

    #[test]
    fn checksums_match_reference_vectors() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        assert_eq!(adler32(b"123456789"), 0x091E_01DE);
        assert_eq!(adler32(b""), 1);
    }

    #[test]
    fn png_roundtrips_through_stored_deflate() {
        let rgba: Vec<u8> = (0u8..64).collect();
        let png = encode_png(4, 4, &rgba);
        assert_eq!(
            &png[..8],
            &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
        );
        // 长度、类型、数据、CRC 逐段拆开；IDAT 里的 zlib 解回后逐字节等于原始像素。
        let mut at = 8;
        let mut kinds: Vec<&[u8; 4]> = Vec::new();
        let mut raw = None;
        while at < png.len() {
            let length = u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
            let kind = &png[at + 4..at + 8];
            kinds.push(kind.try_into().unwrap());
            let data = &png[at + 8..at + 8 + length as usize];
            let crc = u32::from_be_bytes(
                png[at + 8 + length as usize..at + 12 + length as usize]
                    .try_into()
                    .unwrap(),
            );
            assert_eq!(crc, crc32(&png[at + 4..at + 8 + length as usize]));
            if kind == b"IDAT" {
                raw = Some(unpack_stored(data));
            }
            if kind == b"IHDR" {
                assert_eq!(&data[..4], &4u32.to_be_bytes());
                assert_eq!(&data[4..8], &4u32.to_be_bytes());
                assert_eq!(&data[8..13], &[8, 6, 0, 0, 0]);
            }
            at += 12 + length as usize;
        }
        assert_eq!(kinds, [b"IHDR", b"IDAT", b"IEND"]);
        let raw = raw.unwrap();
        assert_eq!(raw.len(), 4 * (1 + 16));
        for row in 0..4 {
            assert_eq!(raw[row * 17], 0);
            assert_eq!(
                &raw[row * 17 + 1..row * 17 + 17],
                &rgba[row * 16..row * 16 + 16]
            );
        }
    }

    #[test]
    fn zlib_stored_splits_large_payload_and_reassembles() {
        let data: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
        assert_eq!(unpack_stored(&zlib_stored(&data)), data);
        assert_eq!(unpack_stored(&zlib_stored(&[])), Vec::<u8>::new());
    }

    #[test]
    fn icon_key_tracks_path_and_modification_time() {
        let first = icon_key(r"C:\Tools\app.exe", UNIX_EPOCH + Duration::from_secs(1000));
        assert_eq!(
            first,
            icon_key(r"C:\Tools\app.exe", UNIX_EPOCH + Duration::from_secs(1000))
        );
        assert_ne!(
            first,
            icon_key(r"C:\Tools\app.exe", UNIX_EPOCH + Duration::from_secs(2000))
        );
        assert_ne!(
            first,
            icon_key(
                r"C:\Tools\other.exe",
                UNIX_EPOCH + Duration::from_secs(1000)
            )
        );
    }

    #[test]
    fn system_executables_yield_description_icon_and_key() {
        let notepad = r"C:\Windows\System32\notepad.exe";
        if !Path::new(notepad).is_file() {
            return; // 非 Windows 目录结构（如 ARM 的 wow64 路径变化）时跳过。
        }
        assert!(file_description(notepad).is_some_and(|d| !d.trim().is_empty()));
        let png = extract_icon_png(notepad, 32).expect("notepad 有图标");
        assert_eq!(
            &png[..8],
            &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
        );
        let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
        assert_eq!((width, height), (32, 32));
        // 修改时间变化（模拟 exe 升级）后键变化。
        let modified = fs::metadata(notepad).and_then(|m| m.modified()).unwrap();
        assert_ne!(
            icon_key(notepad, modified),
            icon_key(notepad, modified + Duration::from_secs(1))
        );
    }

    #[test]
    fn icon_file_write_is_renamed_atomically() {
        let directory =
            std::env::temp_dir().join(format!("nianlai-icons-{}", crate::log::now_ms()));
        write_icon(&directory, "abcd1234", &encode_png(1, 1, &[1, 2, 3, 4])).unwrap();
        assert!(directory.join("abcd1234.png").is_file());
        assert!(!directory.join("abcd1234.png.pending").exists());
        let _ = std::fs::remove_dir_all(directory);
    }
}
