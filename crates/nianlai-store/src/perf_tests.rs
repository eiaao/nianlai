//! 测量用（#[ignore]）：读连接的打开耗时占一次读的比例，以及短词搜索耗时。
//! `cargo test -p nianlai-store --release perf_ -- --ignored --nocapture`
use super::*;
use content::Content;
use hash::sha256;
use std::{sync::Arc, time::Instant};

fn seeded(directory: &Path, total: usize) -> Store {
    let store = Store::open(directory, sha256).unwrap();
    if store.cache().total >= total {
        return store;
    }
    for n in 1..=total {
        // 每 500 条一条约 200 KB 的大文本，其余是普通短文本。
        let value = if n % 500 == 0 {
            format!("big{n} {}", "拈来 lorem ipsum dolor sit amet ".repeat(6000))
        } else {
            format!("记录{n:06} hello world sample text {n} 用于性能测量")
        };
        let at = Instant::now();
        store
            .ingest(
                Content::text(&value, sha256).unwrap(),
                CopyEvent {
                    time_ms: n as u64,
                    sequence: n as u32,
                    source_process: "s.exe".into(),
                    source_path: r"C:\s.exe".into(),
                    owner_process: "o.exe".into(),
                    owner_pid: 1,
                    observed: at,
                },
                false,
                at,
            )
            .unwrap();
    }
    store
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

#[test]
#[ignore = "测量用"]
fn perf_reader_open_vs_read() {
    let dir = std::env::var("NIANLAI_PERF_DIR")
        .unwrap_or(r"D:\project\nianlai\target\claude-tmp\perf-db".into());
    std::fs::create_dir_all(&dir).unwrap();
    let store = seeded(Path::new(&dir), 10_000);
    let ms = |t: Instant| t.elapsed().as_secs_f64() * 1000.0;
    let mut open = vec![];
    for _ in 0..200 {
        let t = Instant::now();
        let r = store.reader().unwrap();
        open.push(ms(t));
        drop(r);
    }
    let mut recent = vec![];
    let mut search_idx = vec![];
    let mut search_short = vec![];
    let mut counts = vec![];
    for _ in 0..50 {
        let t = Instant::now();
        store.reader().unwrap().recent().unwrap();
        recent.push(ms(t));
        let t = Instant::now();
        store.reader().unwrap().counts().unwrap();
        counts.push(ms(t));
        let t = Instant::now();
        store.reader().unwrap().search("hello").unwrap();
        search_idx.push(ms(t));
        let t = Instant::now();
        store.reader().unwrap().search("zz").unwrap();
        search_short.push(ms(t));
    }
    let open_m = median(open);
    println!("open(reader()) median {open_m:.3} ms");
    for (name, v) in [
        ("recent", recent),
        ("counts", counts),
        ("search hello", search_idx),
        ("search zz (short)", search_short),
    ] {
        let m = median(v);
        println!(
            "{name}: total {m:.3} ms, open share {:.0}%",
            open_m / m * 100.0
        );
    }
    let _ = Arc::new(0);
}
