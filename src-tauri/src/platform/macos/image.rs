use crate::history::content::{Content, PNG};
use std::{
    io::Cursor,
    path::{Path, PathBuf},
    time::Instant,
};

pub struct Rendered {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}
pub struct Saved {
    pub path: PathBuf,
    pub reused: bool,
    pub bytes: u64,
    pub source: &'static str,
    pub encode_ms: f64,
}
pub(super) fn png(bytes: &[u8], limit: u32) -> Result<Rendered, &'static str> {
    let decoded = image::load_from_memory(bytes).map_err(|_| "image_decode_failed")?;
    let (width, height) = (decoded.width(), decoded.height());
    let resized = if limit > 0 && (width > limit || height > limit) {
        decoded.thumbnail(limit, limit)
    } else {
        decoded
    };
    let mut output = Cursor::new(Vec::new());
    resized
        .write_to(&mut output, image::ImageFormat::Png)
        .map_err(|_| "image_encode_failed")?;
    Ok(Rendered {
        png: output.into_inner(),
        width,
        height,
    })
}

pub fn render(content: &Content, limit: u32) -> Result<Rendered, &'static str> {
    png(content.raw(PNG).ok_or("no_image")?, limit)
}
pub fn thumbnail(content: &Content, limit: u32) -> Result<Vec<u8>, &'static str> {
    render(content, limit).map(|image| image.png)
}
pub fn save_png(content: &Content, directory: &Path) -> Result<Saved, &'static str> {
    let started = Instant::now();
    let bytes = content.raw(PNG).ok_or("no_image")?;
    let name: String = content.fingerprint[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let path = directory.join(format!("nianlai-{name}.png"));
    std::fs::create_dir_all(directory).map_err(|_| "export_directory_failed")?;
    std::fs::write(&path, bytes).map_err(|_| "export_write_failed")?;
    Ok(Saved {
        path,
        reused: false,
        bytes: bytes.len() as u64,
        source: "png",
        encode_ms: started.elapsed().as_secs_f64() * 1000.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resized_size(width: u32, height: u32, limit: u32) -> (u32, u32) {
        let mut input = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(width, height)
            .write_to(&mut input, image::ImageFormat::Png)
            .unwrap();
        let rendered = png(input.get_ref(), limit).unwrap();
        assert_eq!((rendered.width, rendered.height), (width, height));
        let output = image::load_from_memory(&rendered.png).unwrap();
        (output.width(), output.height())
    }

    #[test]
    fn original_preview_preserves_dimensions() {
        assert_eq!(resized_size(8, 4, u32::MAX), (8, 4));
    }

    #[test]
    fn thumbnail_does_not_enlarge_small_images() {
        assert_eq!(resized_size(8, 4, 64), (8, 4));
    }

    #[test]
    fn thumbnail_shrinks_long_side_proportionally() {
        assert_eq!(resized_size(32, 16, 8), (8, 4));
    }
}
