// 图片降级成路径时，把记录里的图片存成 PNG。用系统自带的 WIC 编码，不加依赖。
use crate::history::content::{Content, DIB, DIBV5, PNG};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};
use windows::{
    Win32::{
        Foundation::{GENERIC_WRITE, HGLOBAL},
        Graphics::Imaging::*,
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize, STATFLAG_NONAME, STREAM_SEEK_SET,
            StructuredStorage::CreateStreamOnHGlobal,
        },
    },
    core::HSTRING,
};

pub struct Saved {
    pub path: PathBuf,
    pub reused: bool,
    pub bytes: u64,
    pub source: &'static str,
    pub encode_ms: f64,
}

// 文件名取内容指纹，同一张图再粘直接复用，不重复编码。
pub fn save_png(content: &Content, directory: &Path) -> Result<Saved, &'static str> {
    let name: String = content.fingerprint[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let path = directory.join(format!("nianlai-{name}.png"));
    if let Ok(metadata) = fs::metadata(&path)
        && metadata.len() > 0
    {
        return Ok(Saved {
            path,
            reused: true,
            bytes: metadata.len(),
            source: "existing",
            encode_ms: 0.0,
        });
    }
    fs::create_dir_all(directory).map_err(|_| "export_directory_failed")?;
    // 先写临时文件再改名，中途失败也不会留下半个 PNG 让人引用。
    let stage = directory.join(format!("nianlai-{name}.png.partial"));
    let started = Instant::now();
    let written = match content
        .raw(PNG)
        .filter(|data| data.starts_with(b"\x89PNG\r\n\x1a\n"))
    {
        Some(png) => fs::write(&stage, png)
            .map(|_| "png")
            .map_err(|_| "export_write_failed"),
        None => content
            .raw(DIBV5)
            .or_else(|| content.raw(DIB))
            .ok_or("no_image")
            .and_then(bitmap_file)
            .and_then(|bitmap| encode(&bitmap, &stage).map(|_| "dib")),
    };
    let result = written.and_then(|source| {
        fs::rename(&stage, &path).map_err(|_| "export_rename_failed")?;
        Ok(Saved {
            bytes: fs::metadata(&path).map_or(0, |m| m.len()),
            path,
            reused: false,
            source,
            encode_ms: started.elapsed().as_secs_f64() * 1000.0,
        })
    });
    if result.is_err() {
        let _ = fs::remove_file(&stage);
    }
    result
}

// 剪贴板里的 DIB 就是 BMP 文件去掉了 14 字节的文件头；补上文件头，交给 WIC 的 BMP 解码器，
// 各种位深、调色板、位域和自上而下的行序都由它处理。
fn bitmap_file(dib: &[u8]) -> Result<Vec<u8>, &'static str> {
    let read = |offset: usize, size: usize| -> Result<u32, &'static str> {
        let bytes = dib.get(offset..offset + size).ok_or("invalid_dib")?;
        Ok(bytes
            .iter()
            .rev()
            .fold(0u32, |value, byte| value << 8 | *byte as u32))
    };
    let header = read(0, 4)? as usize;
    let (bits, used, masks, entry) = match header {
        12 => (read(10, 2)?, 0, 0, 3),
        40 | 52 | 56 | 108 | 124 => {
            let compression = read(16, 4)?;
            // 只有 40 字节的信息头把位域掩码放在头后面；更长的头已经包含掩码。
            let masks = match (header, compression) {
                (40, 3) => 12,
                (40, 6) => 16,
                _ => 0,
            };
            (read(14, 2)?, read(32, 4)?, masks, 4)
        }
        _ => return Err("invalid_dib"),
    };
    let colors = if used != 0 {
        used as usize
    } else if bits <= 8 {
        1 << bits
    } else {
        0
    };
    let offset = colors
        .checked_mul(entry)
        .and_then(|palette| (14 + header + masks).checked_add(palette))
        .filter(|offset| *offset <= 14 + dib.len())
        .ok_or("invalid_dib")?;
    let size = u32::try_from(14 + dib.len()).map_err(|_| "invalid_dib")?;
    let mut file = Vec::with_capacity(14 + dib.len());
    file.extend_from_slice(b"BM");
    file.extend_from_slice(&size.to_le_bytes());
    file.extend_from_slice(&[0; 4]);
    file.extend_from_slice(&(offset as u32).to_le_bytes());
    file.extend_from_slice(dib);
    Ok(file)
}

struct Com;
impl Drop for Com {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

fn encode(bitmap: &[u8], output: &Path) -> Result<(), &'static str> {
    unsafe {
        // S_OK 和 S_FALSE 都要配对释放；线程已是单线程套间时会失败，但 WIC 照样可用。
        let _com = CoInitializeEx(None, COINIT_MULTITHREADED)
            .is_ok()
            .then_some(Com);
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                .map_err(|_| "wic_unavailable")?;
        let decoded = (|| -> windows::core::Result<IWICBitmapFrameDecode> {
            let input = factory.CreateStream()?;
            input.InitializeFromMemory(bitmap)?;
            let decoder = factory.CreateDecoderFromStream(
                &input,
                std::ptr::null(),
                WICDecodeMetadataCacheOnDemand,
            )?;
            decoder.GetFrame(0)
        })()
        .map_err(|_| "image_decode_failed")?;
        (|| -> windows::core::Result<()> {
            let stream = factory.CreateStream()?;
            stream.InitializeFromFilename(&HSTRING::from(output.as_os_str()), GENERIC_WRITE.0)?;
            let encoder = factory.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
            encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
            let mut frame = None;
            let mut options = None;
            encoder.CreateNewFrame(&mut frame, &mut options)?;
            let frame = frame.ok_or_else(windows::core::Error::empty)?;
            frame.Initialize(options.as_ref())?;
            frame.WriteSource(&decoded, std::ptr::null())?;
            frame.Commit()?;
            encoder.Commit()
        })()
        .map_err(|_| "image_encode_failed")
    }
}

/// 记录里的图片原样数据：优先 PNG，没有就把 DIB 补成 BMP 文件。
fn image_source(content: &Content) -> Result<Vec<u8>, &'static str> {
    match content
        .raw(PNG)
        .filter(|data| data.starts_with(b"\x89PNG\r\n\x1a\n"))
    {
        Some(png) => Ok(png.to_vec()),
        None => content
            .raw(DIBV5)
            .or_else(|| content.raw(DIB))
            .ok_or("no_image")
            .and_then(bitmap_file),
    }
}

/// 存储管理里手动挑选截图用的缩略图（M2-3）：长边缩到 `max` 像素以内，在内存里编码成 PNG，不落盘。
pub fn thumbnail(content: &Content, max: u32) -> Result<Vec<u8>, &'static str> {
    let source = image_source(content)?;
    unsafe {
        let _com = CoInitializeEx(None, COINIT_MULTITHREADED)
            .is_ok()
            .then_some(Com);
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                .map_err(|_| "wic_unavailable")?;
        let frame = (|| -> windows::core::Result<IWICBitmapFrameDecode> {
            let input = factory.CreateStream()?;
            input.InitializeFromMemory(&source)?;
            factory
                .CreateDecoderFromStream(&input, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?
                .GetFrame(0)
        })()
        .map_err(|_| "image_decode_failed")?;
        (|| -> windows::core::Result<Vec<u8>> {
            let (mut width, mut height) = (0, 0);
            frame.GetSize(&mut width, &mut height)?;
            let longest = width.max(height).max(1);
            let scale = (max as f64 / longest as f64).min(1.0);
            let scaler = factory.CreateBitmapScaler()?;
            scaler.Initialize(
                &frame,
                ((width as f64 * scale).round() as u32).max(1),
                ((height as f64 * scale).round() as u32).max(1),
                WICBitmapInterpolationModeFant,
            )?;
            let stream = CreateStreamOnHGlobal(HGLOBAL::default(), true)?;
            let encoder = factory.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
            encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
            let mut output = None;
            let mut options = None;
            encoder.CreateNewFrame(&mut output, &mut options)?;
            let output = output.ok_or_else(windows::core::Error::empty)?;
            output.Initialize(options.as_ref())?;
            output.WriteSource(&scaler, std::ptr::null())?;
            output.Commit()?;
            encoder.Commit()?;
            let mut stat = Default::default();
            stream.Stat(&mut stat, STATFLAG_NONAME)?;
            stream.Seek(0, STREAM_SEEK_SET, None)?;
            let mut bytes = vec![0u8; stat.cbSize as usize];
            let mut read = 0;
            stream
                .Read(
                    bytes.as_mut_ptr().cast(),
                    bytes.len() as u32,
                    Some(&mut read),
                )
                .ok()?;
            bytes.truncate(read as usize);
            Ok(bytes)
        })()
        .map_err(|_| "image_encode_failed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        history::content::{Content, Data},
        platform::hash::sha256,
    };
    use std::collections::BTreeMap;

    // 测试并行运行，目录名要各不相同，不能只靠时间戳。
    fn temporary() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "nianlai-image-test-{}-{}-{}",
            std::process::id(),
            crate::log::now_ms(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn info_header(
        size: u32,
        width: i32,
        height: i32,
        bits: u16,
        compression: u32,
        used: u32,
    ) -> Vec<u8> {
        let mut header = Vec::new();
        header.extend_from_slice(&size.to_le_bytes());
        header.extend_from_slice(&width.to_le_bytes());
        header.extend_from_slice(&height.to_le_bytes());
        header.extend_from_slice(&1u16.to_le_bytes());
        header.extend_from_slice(&bits.to_le_bytes());
        header.extend_from_slice(&compression.to_le_bytes());
        header.extend_from_slice(&[0; 12]);
        header.extend_from_slice(&used.to_le_bytes());
        header.extend_from_slice(&0u32.to_le_bytes());
        header.resize(size as usize, 0);
        header
    }

    // 读回 PNG 的尺寸和指定像素（BGRA），确认编码出来的图对得上。
    fn decode(path: &Path, x: u32, y: u32) -> ((u32, u32), [u8; 4]) {
        unsafe {
            let _com = CoInitializeEx(None, COINIT_MULTITHREADED)
                .is_ok()
                .then_some(Com);
            let factory: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap();
            let decoder = factory
                .CreateDecoderFromFilename(
                    &HSTRING::from(path.as_os_str()),
                    None,
                    windows::Win32::Foundation::GENERIC_READ,
                    WICDecodeMetadataCacheOnDemand,
                )
                .unwrap();
            let frame = decoder.GetFrame(0).unwrap();
            let (mut width, mut height) = (0, 0);
            frame.GetSize(&mut width, &mut height).unwrap();
            let converter = factory.CreateFormatConverter().unwrap();
            converter
                .Initialize(
                    &frame,
                    &GUID_WICPixelFormat32bppBGRA,
                    WICBitmapDitherTypeNone,
                    None::<&IWICPalette>,
                    0.0,
                    WICBitmapPaletteTypeCustom,
                )
                .unwrap();
            let mut pixel = [0u8; 4];
            let rect = WICRect {
                X: x as i32,
                Y: y as i32,
                Width: 1,
                Height: 1,
            };
            converter.CopyPixels(&rect, 4, &mut pixel).unwrap();
            ((width, height), pixel)
        }
    }

    fn image(name: &str, data: Vec<u8>) -> std::sync::Arc<Content> {
        Content::new(
            BTreeMap::from([(name.into(), Data::Bytes(data.into()))]),
            sha256,
        )
        .unwrap()
    }

    #[test]
    fn thumbnails_shrink_the_long_side_and_keep_small_images() {
        // 300×100 的 24 位图，每行 900 字节正好 4 字节对齐。
        let mut dib = info_header(40, 300, 100, 24, 0, 0);
        dib.extend(std::iter::repeat_n(200u8, 900 * 100));
        let png = thumbnail(&image(DIB, dib), 240).unwrap();
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        let directory = temporary();
        let path = directory.join("thumb.png");
        fs::write(&path, &png).unwrap();
        assert_eq!(decode(&path, 0, 0).0, (240, 80));
        // 比上限小的不放大。
        let mut small = info_header(40, 2, 1, 24, 0, 0);
        small.extend_from_slice(&[1, 2, 3, 4, 5, 6, 0, 0]);
        fs::write(&path, thumbnail(&image(DIB, small), 240).unwrap()).unwrap();
        assert_eq!(decode(&path, 0, 0).0, (2, 1));
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn bottom_up_24_bit_dib_keeps_size_and_row_order() {
        // 3×2，24 位，自下而上：第一行数据是图的最下面一行。每行 9 字节补到 12。
        let mut dib = info_header(40, 3, 2, 24, 0, 0);
        let bottom = [[0, 0, 255], [0, 255, 0], [255, 0, 0]];
        let top = [[255, 255, 255], [0, 0, 0], [0, 128, 255]];
        for row in [bottom, top] {
            for pixel in row {
                dib.extend_from_slice(&pixel);
            }
            dib.extend_from_slice(&[0; 3]);
        }
        let directory = temporary();
        let saved = save_png(&image(DIB, dib), &directory).unwrap();
        assert_eq!(saved.source, "dib");
        assert!(!saved.reused);
        assert_eq!(decode(&saved.path, 0, 1), ((3, 2), [0, 0, 255, 255]));
        assert_eq!(decode(&saved.path, 2, 0).1, [0, 128, 255, 255]);
        assert!(
            !directory
                .join(saved.path.file_name().unwrap())
                .with_extension("png.partial")
                .exists()
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn top_down_v5_bitfields_with_alpha_and_paletted_dibs_encode() {
        // BITMAPV5HEADER，32 位，位域掩码在头里，高度为负表示自上而下。
        let mut v5 = info_header(124, 2, -1, 32, 3, 0);
        v5[40..56].copy_from_slice(
            &[0x00FF0000u32, 0x0000FF00, 0x000000FF, 0xFF000000]
                .iter()
                .flat_map(|mask| mask.to_le_bytes())
                .collect::<Vec<_>>(),
        );
        v5[56..60].copy_from_slice(b"BGRs"); // LCS_sRGB
        v5.extend_from_slice(&[10, 20, 30, 255, 40, 50, 60, 128]);
        // 40 字节的头加 BI_BITFIELDS：三个掩码跟在头后面。
        let mut fields = info_header(40, 1, 1, 32, 3, 0);
        for mask in [0x00FF0000u32, 0x0000FF00, 0x000000FF] {
            fields.extend_from_slice(&mask.to_le_bytes());
        }
        fields.extend_from_slice(&[1, 2, 3, 0]);
        // 8 位调色板：两种颜色，像素取第二种。
        let mut paletted = info_header(40, 1, 1, 8, 0, 2);
        paletted.extend_from_slice(&[0, 0, 0, 0, 9, 99, 199, 0]);
        paletted.extend_from_slice(&[1, 0, 0, 0]);
        let directory = temporary();
        for (name, dib, size, pixel) in [
            (DIBV5, v5, (2, 1), [10, 20, 30, 255]),
            (DIB, fields, (1, 1), [1, 2, 3, 255]),
            (DIB, paletted, (1, 1), [9, 99, 199, 255]),
        ] {
            let saved = save_png(&image(name, dib), &directory).unwrap();
            assert_eq!(decode(&saved.path, 0, 0), (size, pixel));
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn existing_png_is_written_as_is_and_reused_on_the_next_paste() {
        let directory = temporary();
        // 先用 DIB 编出一张真的 PNG，再把它当成“记录里自带的 PNG 格式”。
        let mut dib = info_header(40, 1, 1, 24, 0, 0);
        dib.extend_from_slice(&[7, 8, 9, 0]);
        let first = save_png(&image(DIB, dib), &directory).unwrap();
        let png = fs::read(&first.path).unwrap();
        let content = image(PNG, png.clone());
        let saved = save_png(&content, &directory).unwrap();
        assert_eq!(saved.source, "png");
        assert_eq!(fs::read(&saved.path).unwrap(), png);
        let again = save_png(&content, &directory).unwrap();
        assert!(again.reused);
        assert_eq!(again.path, saved.path);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejects_truncated_or_unknown_headers_without_leaving_files() {
        assert_eq!(bitmap_file(&[40, 0]), Err("invalid_dib"));
        assert_eq!(
            bitmap_file(&info_header(64, 1, 1, 24, 0, 0)),
            Err("invalid_dib")
        );
        // 声称有 100 万种颜色的调色板，超出数据长度。
        assert_eq!(
            bitmap_file(&info_header(40, 1, 1, 8, 0, 1_000_000)),
            Err("invalid_dib")
        );
        let directory = temporary();
        assert_eq!(
            save_png(&image(DIB, vec![1, 2, 3]), &directory).err(),
            Some("invalid_dib")
        );
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
        fs::remove_dir_all(directory).unwrap();
    }
}
