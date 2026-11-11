//! 完整预览（M2-7 / F04）：全文、图片。
//!
//! 列表里的预览只存前 300 个字；完整预览按需从库里取全文，先给前一段，滚到底再取其余。
//! 图片现场从库里的数据解码、按需缩小成 PNG，不落盘。
use super::{History, storage::base64};
use serde::Serialize;

#[derive(Serialize)]
pub struct TextChunk {
    pub text: String,
    /// 全文的字数（按字符计）。
    pub total: usize,
    /// 这次给了多少字；小于 total 表示后面还有。
    pub shown: usize,
}

#[derive(Serialize, Clone)]
pub struct ImageView {
    /// PNG 的 data URL。
    pub url: String,
    /// 原图尺寸（不是这张 PNG 的尺寸）。
    pub width: u32,
    pub height: u32,
    /// 记录里保存的字节数。
    pub bytes: usize,
}

/// 最近预览过的缩略图（按内容 id 和长边上限），最多 `IMAGE_CACHE` 张。
/// 面板里同一张图会被卡片和预览各取一次，每次都要读全部格式、复核哈希再解码缩放（4K 截图约 190 ms）。
/// 内容清除（purge）、导入备份、换库、面板隐藏时清空，同一内容补齐格式时只清它那条；原尺寸（`max` 为空）的图太大，不缓存。
#[derive(Default)]
pub struct ImageCache(pub(super) Vec<((usize, u32), ImageView)>);
const IMAGE_CACHE: usize = 8;
const IMAGE_CACHE_MAX_URL: usize = 1 << 20;
impl ImageCache {
    pub fn clear(&mut self) {
        self.0.clear();
    }
    /// 这条内容变了（合并补齐了格式）：它所有尺寸的缩略图作废。
    pub(super) fn remove(&mut self, id: usize) {
        self.0.retain(|((cached, _), _)| *cached != id);
    }
    pub(super) fn get(&mut self, key: (usize, u32)) -> Option<ImageView> {
        let at = self.0.iter().position(|(k, _)| *k == key)?;
        let entry = self.0.remove(at);
        let view = entry.1.clone();
        self.0.push(entry);
        Some(view)
    }
    pub(super) fn put(&mut self, key: (usize, u32), view: &ImageView) {
        if view.url.len() > IMAGE_CACHE_MAX_URL {
            return;
        }
        self.0.retain(|(k, _)| *k != key);
        if self.0.len() >= IMAGE_CACHE {
            self.0.remove(0);
        }
        self.0.push((key, view.clone()));
    }
}

impl History {
    /// 全文的前 `limit` 个字；`limit` 为 None 时给全文。不是文字内容返回 None。
    pub fn full_text(&self, id: usize, limit: Option<usize>) -> Result<Option<TextChunk>, String> {
        let Some(content) = self.get(id)? else {
            return Ok(None);
        };
        let Some(text) = content.text_value() else {
            return Ok(None);
        };
        let total = text.chars().count();
        let text = match limit {
            Some(limit) if limit < total => text.chars().take(limit).collect(),
            _ => text,
        };
        let shown = total.min(limit.unwrap_or(total));
        Ok(Some(TextChunk { text, total, shown }))
    }

    /// 图片：长边缩到 `max` 以内；`max` 为 None 时给原尺寸。不是图片返回 None。
    pub fn image(&self, id: usize, max: Option<u32>) -> Result<Option<ImageView>, String> {
        if let Some(max) = max
            && let Some(view) = self.images.lock().unwrap().get((id, max))
        {
            return Ok(Some(view));
        }
        let Some(content) = self.get(id)? else {
            return Ok(None);
        };
        if content.kind != "图片" {
            return Ok(None);
        }
        let rendered = crate::platform::image::render(&content, max.unwrap_or(u32::MAX))?;
        let view = ImageView {
            url: format!("data:image/png;base64,{}", base64(&rendered.png)),
            width: rendered.width,
            height: rendered.height,
            bytes: content.bytes,
        };
        if let Some(max) = max {
            self.images.lock().unwrap().put((id, max), &view);
        }
        Ok(Some(view))
    }
}
