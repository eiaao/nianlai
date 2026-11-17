use crate::{
    history::{
        History,
        content::{Content, Data, FILES, Fingerprint, HTML, PNG, RTF, TEXT},
    },
    log::Logger,
    paste::queue,
    platform::Result,
};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSData, NSString, NSURL};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicIsize, Ordering},
    },
    time::Instant,
};

const TYPES: [(&str, &str); 6] = [
    ("public.utf8-plain-text", TEXT),
    ("public.html", HTML),
    ("public.rtf", RTF),
    ("public.png", PNG),
    ("public.tiff", PNG),
    ("public.file-url", FILES),
];
#[cfg(test)]
fn format_name(uti: &str) -> Option<&'static str> {
    TYPES
        .iter()
        .find(|(name, _)| *name == uti)
        .map(|(_, format)| *format)
}
fn html_header(html: &[u8]) -> Vec<u8> {
    let header = |start: usize, end: usize| {
        format!(
            "Version:1.0\r\nStartHTML:{start:010}\r\nEndHTML:{end:010}\r\nStartFragment:{start:010}\r\nEndFragment:{end:010}\r\n"
        )
    };
    let start = header(0, 0).len();
    let mut bytes = header(start, start + html.len()).into_bytes();
    bytes.extend_from_slice(html);
    bytes
}
fn html_body(bytes: &[u8]) -> Result<&[u8]> {
    if !bytes.starts_with(b"Version:") {
        return Ok(bytes);
    }
    let text = String::from_utf8_lossy(bytes);
    let offset = |name| {
        text.lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|value| value.trim().parse::<usize>().ok())
    };
    let (Some(start), Some(end)) = (offset("StartHTML:"), offset("EndHTML:")) else {
        return Err("invalid_html_header".into());
    };
    bytes
        .get(start..end)
        .ok_or_else(|| "invalid_html_range".into())
}

pub(super) fn read(board: &NSPasteboard, logger: &Logger) -> Result<BTreeMap<String, Data>> {
    let mut formats = BTreeMap::new();
    let mut failed = None;
    let types = board.types().ok_or("clipboard_read_denied")?;
    // 固定先尝试 PNG；只有成功读取后才跳过备用 TIFF。
    for (uti, name) in TYPES {
        if !types.containsObject(&NSString::from_str(uti)) {
            continue;
        }
        if name == FILES || formats.contains_key(name) {
            continue;
        }
        let result = (|| -> Result<Vec<u8>> {
            Ok(if name == TEXT {
                board
                    .stringForType(&NSString::from_str(uti))
                    .ok_or("clipboard_read_failed")?
                    .to_string()
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect()
            } else {
                let data = board
                    .dataForType(&NSString::from_str(uti))
                    .ok_or("clipboard_read_failed")?
                    .to_vec();
                match uti {
                    "public.html" => html_header(&data),
                    "public.tiff" => super::image::png(&data, 0)?.png,
                    _ => data,
                }
            })
        })();
        match result {
            Ok(data) => {
                formats.insert(name.to_owned(), Data::Bytes(data.into()));
            }
            Err(error) => {
                logger.event(
                    "capture_format_failed",
                    serde_json::json!({"format":uti,"error":error}),
                );
                failed = Some(error);
            }
        }
    }
    if let Some(items) = board.pasteboardItems() {
        let mut paths = Vec::new();
        for item in items {
            if let Some(url) = item.stringForType(&NSString::from_str("public.file-url")) {
                if let Some(path) = NSURL::URLWithString(&url).and_then(|url| url.path()) {
                    paths.push(path.to_string());
                } else {
                    logger.event(
                        "capture_format_failed",
                        serde_json::json!({"format":"public.file-url","error":"invalid_file_url"}),
                    );
                    failed = Some("invalid_file_url".into());
                }
            }
        }
        if !paths.is_empty() {
            formats.insert(FILES.to_owned(), Data::Files(paths.into()));
        }
    }
    if formats.is_empty()
        && let Some(error) = failed
    {
        return Err(error);
    }
    Ok(formats)
}

fn prepare(
    content: &Content,
) -> Result<Retained<NSArray<ProtocolObject<dyn NSPasteboardWriting>>>> {
    let first = NSPasteboardItem::new();
    let mut items = vec![first];
    let mut written = false;
    let mut failed_html = None;
    for (name, format) in &content.formats {
        if let Data::Files(paths) = &format.data {
            for (index, path) in paths.iter().enumerate() {
                if index > 0 {
                    items.push(NSPasteboardItem::new());
                }
                let url = NSURL::fileURLWithPath(&NSString::from_str(path))
                    .absoluteString()
                    .ok_or("invalid_file_url")?;
                if !items[index].setString_forType(&url, &NSString::from_str("public.file-url")) {
                    return Err("clipboard_write_failed".into());
                }
                written = true;
            }
        } else if let Some((uti, _)) = TYPES.iter().find(|(_, stored)| *stored == name) {
            let bytes = content.raw(name).ok_or("clipboard_format_failed")?;
            let ok = if name == TEXT {
                items[0].setString_forType(
                    &NSString::from_str(&content.text_value().ok_or("clipboard_format_failed")?),
                    &NSString::from_str(uti),
                )
            } else {
                let bytes = if name == HTML {
                    match html_body(bytes) {
                        Ok(body) => body,
                        Err(error) => {
                            failed_html = Some(error);
                            continue;
                        }
                    }
                } else {
                    bytes
                };
                items[0].setData_forType(&NSData::with_bytes(bytes), &NSString::from_str(uti))
            };
            if !ok {
                return Err("clipboard_write_failed".into());
            }
            written = true;
        }
    }
    if !written {
        return Err(failed_html.unwrap_or_else(|| "clipboard_format_failed".into()));
    }
    let objects: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = items
        .into_iter()
        .map(ProtocolObject::from_retained)
        .collect();
    Ok(NSArray::from_retained_slice(&objects))
}

fn publish(
    board: &NSPasteboard,
    objects: &NSArray<ProtocolObject<dyn NSPasteboardWriting>>,
) -> Result<isize> {
    let count = board.clearContents();
    if !board.writeObjects(objects) {
        return Err("clipboard_write_failed".into());
    }
    // 外部写入的代数不能记成自己的发布。
    if board.changeCount() != count {
        return Err("generic_clipboard_changed".into());
    }
    Ok(count)
}

pub struct Record {
    pub id: usize,
    pub fingerprint: Fingerprint,
}
pub struct Observation {
    pub stamp: queue::Stamp,
}
pub type Watcher = Box<dyn Fn(queue::Stamp) + Send>;
struct Inner {
    history: History,
    pub lock: Mutex<()>,
    own_change: AtomicIsize,
    watcher: Mutex<Option<Watcher>>,
}
#[derive(Clone)]
pub struct Service(Arc<Inner>);
impl Service {
    pub fn start(_: Logger, history: History) -> Result<Self> {
        Ok(Self(Arc::new(Inner {
            history,
            lock: Mutex::new(()),
            own_change: AtomicIsize::new(-1),
            watcher: Mutex::new(None),
        })))
    }
    pub fn owner(&self) -> usize {
        std::process::id() as usize
    }
    pub fn stamp(&self) -> queue::Stamp {
        objc2::rc::autoreleasepool(|_| self.sample(NSPasteboard::generalPasteboard().changeCount()))
    }
    fn sample(&self, count: isize) -> queue::Stamp {
        queue::Stamp {
            owner: if self.is_own(count) { self.owner() } else { 0 },
            sequence: count as u32,
        }
    }
    pub(super) fn is_own(&self, count: isize) -> bool {
        self.0.own_change.load(Ordering::Acquire) == count
    }
    pub(super) fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.0.lock.lock().unwrap()
    }
    pub(super) fn changed(&self, count: isize) {
        if let Some(watcher) = self.0.watcher.lock().unwrap().as_ref() {
            watcher(self.sample(count));
        }
    }
    pub fn set_clipboard_watcher(&self, watcher: Option<Watcher>) -> Result<()> {
        *self.0.watcher.lock().unwrap() = watcher;
        Ok(())
    }
    pub fn write(
        &self,
        record: Record,
        content: Arc<Content>,
        _: u32,
        _: bool,
    ) -> Result<Arc<Mutex<Observation>>> {
        self.write_inner(record, content, None)
    }
    pub fn write_guarded(
        &self,
        record: Record,
        content: Arc<Content>,
        _: u32,
        _: bool,
        guard: Arc<queue::Guard>,
    ) -> Result<Arc<Mutex<Observation>>> {
        self.write_inner(record, content, Some(guard))
    }
    fn write_inner(
        &self,
        record: Record,
        content: Arc<Content>,
        guard: Option<Arc<queue::Guard>>,
    ) -> Result<Arc<Mutex<Observation>>> {
        if !self.0.history.before_write(
            record.id,
            record.fingerprint,
            content.fingerprint,
            Instant::now(),
        ) {
            return Err("record_missing".into());
        }
        let _lock = self.lock();
        objc2::rc::autoreleasepool(|_| {
            let board = NSPasteboard::generalPasteboard();
            let objects = prepare(&content)?;
            let mut lease = guard.as_ref().map(|guard| guard.lease());
            if let Some(lease) = lease.as_mut() {
                lease
                    .begin(self.sample(board.changeCount()))
                    .map_err(|reason| reason.name().to_owned())?;
            }
            let count = publish(&board, &objects)?;
            self.0.own_change.store(count, Ordering::Release);
            let stamp = self.sample(count);
            if let Some(lease) = lease.as_mut() {
                lease.published(stamp);
            }
            Ok(Arc::new(Mutex::new(Observation { stamp })))
        })
        .inspect(|_| self.0.history.reuse(record.id))
    }
    pub fn write_for_capture(&self, content: Arc<Content>) -> Result<()> {
        let _lock = self.lock();
        objc2::rc::autoreleasepool(|_| {
            let objects = prepare(&content)?;
            publish(&NSPasteboard::generalPasteboard(), &objects).map(|_| ())
        })
    }
    // macOS 没有剪贴板消息窗口，这里不需要。
    pub fn shutdown(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uti_uses_existing_storage_names() {
        assert_eq!(
            TYPES.map(|(uti, _)| format_name(uti)),
            [
                Some(TEXT),
                Some(HTML),
                Some(RTF),
                Some(PNG),
                Some(PNG),
                Some(FILES)
            ]
        );
    }
    #[test]
    fn unknown_uti_is_not_recorded() {
        assert_eq!(format_name("org.example.unknown"), None);
    }
    #[test]
    fn html_offsets_count_utf8_bytes() {
        let html = "<b>会议</b>".as_bytes();
        assert_eq!(html_body(&html_header(html)), Ok(html));
    }
    #[test]
    fn raw_html_stays_unchanged() {
        assert_eq!(html_body(b"<p>hello</p>"), Ok(b"<p>hello</p>".as_slice()));
    }
}
