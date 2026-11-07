use serde_json::Value;
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

#[derive(Default)]
pub struct Decoder {
    pending: Vec<u8>,
    discard_first: bool,
}
impl Decoder {
    pub fn new(discard_first: bool) -> Self {
        Self {
            discard_first,
            ..Self::default()
        }
    }
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<(String, Value)>, String> {
        self.pending.extend_from_slice(bytes);
        if self.pending.len() > 4 * 1024 * 1024 {
            return Err("日志单行过长或未结束".into());
        }
        let mut lines = Vec::new();
        while let Some(pos) = self.pending.iter().position(|b| *b == b'\n') {
            let row: Vec<_> = self.pending.drain(..=pos).collect();
            if self.discard_first {
                self.discard_first = false;
                continue;
            }
            let text = String::from_utf8(row).map_err(|_| "日志不是 UTF-8")?;
            if text.trim().is_empty() {
                continue;
            }
            let event = serde_json::from_str(&text).map_err(|e| format!("日志 JSON 无效：{e}"))?;
            lines.push((text, event));
        }
        Ok(lines)
    }
}
pub struct Tail {
    path: PathBuf,
    offset: u64,
    decoder: Decoder,
}
impl Tail {
    pub fn at_end(path: &Path) -> io::Result<Self> {
        let mut partial = false;
        let offset = match File::open(path) {
            Ok(mut file) => {
                let length = file.metadata()?.len();
                if length > 0 {
                    file.seek(SeekFrom::End(-1))?;
                    let mut last = [0];
                    file.read_exact(&mut last)?;
                    partial = last[0] != b'\n';
                }
                length
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
            Err(e) => return Err(e),
        };
        Ok(Self {
            path: path.into(),
            offset,
            decoder: Decoder::new(partial),
        })
    }
    pub fn poll(&mut self) -> Result<Vec<(String, Value)>, String> {
        let mut file = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.to_string()),
        };
        self.read_increment(&mut file)
    }
    fn read_increment(
        &mut self,
        file: &mut (impl Read + Seek),
    ) -> Result<Vec<(String, Value)>, String> {
        if file.seek(SeekFrom::End(0)).map_err(|e| e.to_string())? < self.offset {
            return Err("样机日志被截断，不能继续匹配本次会话".into());
        }
        file.seek(SeekFrom::Start(self.offset))
            .map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        file.take(2 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        self.offset += bytes.len() as u64;
        self.decoder.feed(&bytes)
    }
}
pub fn matching<'a>(
    events: &'a [Value],
    from: usize,
    name: &str,
    session: Option<u64>,
) -> Option<&'a Value> {
    events
        .get(from..)?
        .iter()
        .find(|v| v["event"] == name && session.is_none_or(|s| v["session"].as_u64() == Some(s)))
}
#[derive(Default, Debug)]
pub struct KnownHistory {
    order: Vec<u64>,
    samples: std::collections::BTreeMap<u64, usize>,
}
impl KnownHistory {
    pub fn written(&mut self, record: u64, sample: Option<usize>) {
        self.order.retain(|id| *id != record);
        self.order.insert(0, record);
        if let Some(sample) = sample {
            self.samples.insert(record, sample);
        } else {
            self.samples.remove(&record);
        }
    }
    pub fn choose(&self, requested: u8, allowed: impl Fn(usize) -> bool) -> Option<(u8, usize)> {
        for n in [requested, 1] {
            if let Some(sample) = self
                .order
                .get(n as usize - 1)
                .and_then(|id| self.samples.get(id))
                .copied()
                .filter(|s| allowed(*s))
            {
                return Some((n, sample));
            }
        }
        None
    }
    pub fn clear(&mut self) {
        self.order.clear();
        self.samples.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn half_lines_and_boundary() {
        let mut d = Decoder::default();
        assert!(d.feed(b"{\"event\":").unwrap().is_empty());
        let a = d
            .feed(b"\"shown\",\"session\":2}\n{\"event\":\"pa")
            .unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(d.feed(b"ste\",\"session\":2}\n").unwrap().len(), 1);
        let mut d = Decoder::new(true);
        assert!(d.feed(b"old suffix\n").unwrap().is_empty());
        assert!(d.feed(b"bad json\n").is_err());
    }
    #[test]
    fn cursor_only_reads_appended_bytes_and_detects_truncation() {
        let old = b"{\"event\":\"old\"}\n".to_vec();
        let mut cursor = io::Cursor::new(old.clone());
        let mut tail = Tail {
            path: PathBuf::new(),
            offset: old.len() as u64,
            decoder: Decoder::default(),
        };
        assert!(tail.read_increment(&mut cursor).unwrap().is_empty());
        cursor
            .get_mut()
            .extend_from_slice(b"{\"event\":\"shown\",\"session\":3}");
        assert!(tail.read_increment(&mut cursor).unwrap().is_empty());
        cursor
            .get_mut()
            .extend_from_slice(b"\n{\"event\":\"paste\",\"session\":3}\n");
        let rows = tail.read_increment(&mut cursor).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].1["event"], "shown");
        assert!(tail.read_increment(&mut cursor).unwrap().is_empty());
        cursor.get_mut().clear();
        assert!(tail.read_increment(&mut cursor).is_err());
    }
    #[test]
    fn sessions_not_last_event() {
        let events = vec![
            serde_json::json!({"event":"paste","session":1}),
            serde_json::json!({"event":"paste","session":2}),
        ];
        assert_eq!(
            matching(&events, 0, "paste", Some(1)).unwrap()["session"],
            1
        );
        assert!(matching(&events, 1, "paste", Some(1)).is_none());
        assert!(matching(&events, 0, "shown", None).is_none());
    }
    #[test]
    fn recency_never_guesses() {
        let mut h = KnownHistory::default();
        assert!(h.choose(1, |_| true).is_none());
        h.written(50, Some(1));
        h.written(17, Some(2));
        assert_eq!(h.choose(2, |_| true), Some((2, 1)));
        assert_eq!(h.choose(9, |_| true), Some((1, 2)));
        h.written(18, None);
        assert!(h.choose(1, |_| true).is_none());
        assert_eq!(h.choose(2, |_| true), Some((2, 2)));
    }
}
