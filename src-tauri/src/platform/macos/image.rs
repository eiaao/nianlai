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
    let resized = if limit > 0 {
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
