//! 完整预览（M2-7 / F04）：全文、图片。
//!
//! 列表里的预览只存前 300 个字；完整预览按需从库里取全文，先给前一段，滚到底再取其余。
//! 图片现场从库里的数据解码、按需缩小成 PNG，不落盘。
use super::{Shared, storage::base64};
use serde::Serialize;

#[derive(Serialize)]
pub struct TextChunk {
    pub text: String,
    /// 全文的字数（按字符计）。
    pub total: usize,
    /// 这次给了多少字；小于 total 表示后面还有。
    pub shown: usize,
}

#[derive(Serialize)]
pub struct ImageView {
    /// PNG 的 data URL。
    pub url: String,
    /// 原图尺寸（不是这张 PNG 的尺寸）。
    pub width: u32,
    pub height: u32,
    /// 记录里保存的字节数。
    pub bytes: usize,
}

impl Shared {
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
        let Some(content) = self.get(id)? else {
            return Ok(None);
        };
        if content.kind != "图片" {
            return Ok(None);
        }
        let rendered = crate::platform::image::render(&content, max.unwrap_or(u32::MAX))?;
        Ok(Some(ImageView {
            url: format!("data:image/png;base64,{}", base64(&rendered.png)),
            width: rendered.width,
            height: rendered.height,
            bytes: content.bytes,
        }))
    }
}
