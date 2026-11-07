use std::{collections::BTreeMap, sync::Arc};

pub type Fingerprint = [u8; 32];
pub type Hash = fn(&[u8]) -> Result<Fingerprint, String>;
pub const TEXT: &str = "CF_UNICODETEXT";
pub const HTML: &str = "HTML Format";
pub const RTF: &str = "Rich Text Format";
pub const PNG: &str = "PNG";
pub const DIB: &str = "CF_DIB";
pub const DIBV5: &str = "CF_DIBV5";
pub const FILES: &str = "CF_HDROP";

#[derive(Clone, PartialEq, Eq)]
pub enum Data {
    Bytes(Arc<[u8]>),
    Files(Arc<[String]>),
}

impl Data {
    pub fn size(&self) -> usize {
        match self {
            Self::Bytes(bytes) => bytes.len(),
            Self::Files(paths) => paths.iter().map(String::len).sum(),
        }
    }

    fn append_canonical(&self, output: &mut Vec<u8>) {
        match self {
            Self::Bytes(bytes) => output.extend_from_slice(bytes),
            Self::Files(paths) => {
                output.extend_from_slice(&(paths.len() as u64).to_le_bytes());
                for path in paths.iter() {
                    // 只统一分隔符；不访问磁盘，也不折叠大小写（Windows 可启用区分大小写）。
                    let normalized = path.replace('/', "\\");
                    let units: Vec<_> = normalized.encode_utf16().collect();
                    output.extend_from_slice(&(units.len() as u64).to_le_bytes());
                    output.extend(units.into_iter().flat_map(u16::to_le_bytes));
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct Format {
    pub data: Data,
    pub fingerprint: Fingerprint,
}

pub struct Content {
    pub formats: BTreeMap<String, Format>,
    pub fingerprint: Fingerprint,
    pub bytes: usize,
    pub kind: &'static str,
    pub preview: String,
    pub file_names: Vec<String>,
}

impl Content {
    pub fn new(formats: BTreeMap<String, Data>, hash: Hash) -> Result<Arc<Self>, String> {
        let mut canonical = Vec::new();
        let mut stored = BTreeMap::new();
        let mut bytes = 0;
        for (name, data) in formats {
            bytes += data.size();
            canonical.extend_from_slice(&(name.len() as u64).to_le_bytes());
            canonical.extend_from_slice(name.as_bytes());
            let length_at = canonical.len();
            canonical.extend_from_slice(&0u64.to_le_bytes());
            let start = canonical.len();
            data.append_canonical(&mut canonical);
            let fingerprint = hash(&canonical[start..])?;
            let length = (canonical.len() - start) as u64;
            canonical[length_at..start].copy_from_slice(&length.to_le_bytes());
            stored.insert(name, Format { data, fingerprint });
        }
        let fingerprint = hash(&canonical)?;
        drop(canonical);
        let mut content = Self {
            formats: stored,
            fingerprint,
            bytes,
            kind: "文本",
            preview: String::new(),
            file_names: Vec::new(),
        };
        content.make_preview();
        Ok(Arc::new(content))
    }

    pub fn text(text: &str, hash: Hash) -> Result<Arc<Self>, String> {
        let bytes: Vec<_> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        Self::new(
            BTreeMap::from([(TEXT.into(), Data::Bytes(bytes.into()))]),
            hash,
        )
    }

    pub fn text_value(&self) -> Option<String> {
        self.raw(TEXT).map(|bytes| {
            String::from_utf16_lossy(
                &bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| u16::from_le_bytes(*pair))
                    .take_while(|unit| *unit != 0)
                    .collect::<Vec<_>>(),
            )
        })
    }

    pub fn raw(&self, name: &str) -> Option<&[u8]> {
        match &self.formats.get(name)?.data {
            Data::Bytes(bytes) => Some(bytes),
            Data::Files(_) => None,
        }
    }

    pub fn expanded_from(&self, old: &Self) -> bool {
        self.formats.len() > old.formats.len()
            && old.formats.iter().all(|(name, old)| {
                self.formats
                    .get(name)
                    .is_some_and(|new| new.fingerprint == old.fingerprint && new.data == old.data)
            })
    }

    pub fn merge(&self, extra: &Self, hash: Hash) -> Result<Arc<Self>, String> {
        let mut formats: BTreeMap<_, _> = extra
            .formats
            .iter()
            .map(|(name, format)| (name.clone(), format.data.clone()))
            .collect();
        // 同一观察只补格式，不覆盖已保存的原文。
        formats.extend(
            self.formats
                .iter()
                .map(|(name, format)| (name.clone(), format.data.clone())),
        );
        Self::new(formats, hash)
    }

    fn make_preview(&mut self) {
        if let Some(Format {
            data: Data::Files(paths),
            ..
        }) = self.formats.get(FILES)
        {
            self.kind = "文件";
            self.file_names = paths
                .iter()
                .map(|path| {
                    path.trim_end_matches(['\\', '/'])
                        .rsplit(['\\', '/'])
                        .next()
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect();
            self.preview = format!(
                "{} 个文件 · {}",
                paths.len(),
                self.file_names
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("、")
            );
        } else if !self.formats.contains_key(TEXT)
            && [PNG, DIBV5, DIB]
                .iter()
                .any(|name| self.formats.contains_key(*name))
        {
            self.kind = "图片";
            let dimensions = self
                .raw(PNG)
                .and_then(|data| {
                    if data.get(..8)? != b"\x89PNG\r\n\x1a\n" || data.get(12..16)? != b"IHDR" {
                        return None;
                    }
                    Some((
                        u32::from_be_bytes(data.get(16..20)?.try_into().ok()?),
                        u32::from_be_bytes(data.get(20..24)?.try_into().ok()?),
                    ))
                })
                .or_else(|| {
                    [DIBV5, DIB].iter().find_map(|name| {
                        let data = self.raw(name)?;
                        let header = u32::from_le_bytes(data.get(..4)?.try_into().ok()?);
                        if header == 12 {
                            Some((
                                u16::from_le_bytes(data.get(4..6)?.try_into().ok()?) as u32,
                                u16::from_le_bytes(data.get(6..8)?.try_into().ok()?) as u32,
                            ))
                        } else if matches!(header, 40 | 52 | 56 | 108 | 124) {
                            Some((
                                i32::from_le_bytes(data.get(4..8)?.try_into().ok()?).unsigned_abs(),
                                i32::from_le_bytes(data.get(8..12)?.try_into().ok()?)
                                    .unsigned_abs(),
                            ))
                        } else {
                            None
                        }
                    })
                })
                .filter(|(width, height)| *width != 0 && *height != 0);
            self.preview = dimensions.map_or_else(
                || "图片（尺寸未知）".into(),
                |(width, height)| format!("图片 {width}×{height}"),
            );
        } else {
            if self.formats.contains_key(HTML) || self.formats.contains_key(RTF) {
                self.kind = "富文本";
            }
            self.preview = self
                .raw(TEXT)
                .map(|bytes| {
                    let units = bytes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|pair| u16::from_le_bytes(*pair))
                        .take_while(|unit| *unit != 0);
                    char::decode_utf16(units)
                        .map(|character| character.unwrap_or(char::REPLACEMENT_CHARACTER))
                        .take(300)
                        .collect()
                })
                .unwrap_or_else(|| "富文本（无纯文本预览）".into());
        }
    }
}
