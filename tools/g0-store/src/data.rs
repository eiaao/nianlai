use nianlai_store::{
    content::{Content, Data, FILES, PNG, TEXT},
    hash::sha256,
};
use std::{collections::BTreeMap, sync::Arc};

struct Random(u64);
impl Random {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}
pub fn binary(size: usize, seed: u64) -> Vec<u8> {
    let mut random = Random(seed.max(1));
    let mut bytes = vec![0; size];
    for chunk in bytes.chunks_mut(8) {
        chunk.copy_from_slice(&random.next().to_le_bytes()[..chunk.len()]);
    }
    bytes
}
pub fn sample(index: u64, seed: u64) -> Result<Arc<Content>, String> {
    let mut random = Random(seed ^ index.wrapping_mul(0x9e3779b97f4a7c15));
    let salt = random.next();
    let value = match index % 9 {
        0 => format!("订单尾号 12345 项目报告 第 {index} 条 Rust 交付记录 {salt:016x}"),
        1 => format!("Clipboard History mixed CASE Rust review #{index} key={salt:016x}"),
        2 => format!("https://example.com/path/{index}?id=12345&key={salt:016x}"),
        3 => format!(
            "fn item_{index}() {{\n    let value = {salt};\n    // Rust 中文注释\n    value => 2026\n}}"
        ),
        4 => format!("记录 {index} 😀 👩‍💻 🚀 2026 年项目进展 {salt:016x}"),
        5 => format!("编号 {index:012} 12345 2026 3141592653589 校验 {salt:016x}"),
        6 => format!("符号 #{index} 100% a_b ?id= [x] => {{}} \\\"quote\" {salt:016x}"),
        7 => format!("长文 {index}：项目报告 Clipboard 与订单尾号 12345，逐条保存。 {salt:016x}\n"),
        _ => String::new(),
    };
    let mut formats = BTreeMap::new();
    if index % 9 == 8 {
        formats.insert(
            FILES.into(),
            Data::Files(
                vec![
                    format!("C:\\synthetic\\项目报告-{index}-{salt:016x}.txt"),
                    format!("D:\\synthetic\\Clipboard-{index}.rs"),
                ]
                .into(),
            ),
        );
    } else {
        let value = if index % 500 == 7 {
            value.repeat(180)
        } else if index % 9 == 7 {
            value.repeat(8 + (salt as usize % 24))
        } else {
            value
        };
        formats.insert(
            TEXT.into(),
            Data::Bytes(
                value
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>()
                    .into(),
            ),
        );
    }
    if index.is_multiple_of(257) {
        let sizes = [1024, 4096, 65536, 1024 * 1024, 4 * 1024 * 1024];
        formats.insert(
            PNG.into(),
            Data::Bytes(binary(sizes[(index / 257) as usize % 5], salt).into()),
        );
    }
    Content::new(formats, sha256)
}
pub fn writer_sample(index: u64, seed: u64) -> Result<Arc<Content>, String> {
    let original = sample(index, seed)?;
    if !index.is_multiple_of(7) {
        return Ok(original);
    }
    let extra = Content::new(
        BTreeMap::from([(
            PNG.into(),
            Data::Bytes(binary(4 * 1024 * 1024, seed ^ index).into()),
        )]),
        sha256,
    )?;
    original.merge(&extra, sha256)
}
