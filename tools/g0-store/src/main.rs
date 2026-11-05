mod data;
mod paste_fixture;
use nianlai_store::{
    self as store, CopyEvent, Reader, Store,
    content::{Content, Data, PNG},
    hash::sha256,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Args {
    command: String,
    options: BTreeMap<String, String>,
}
impl Args {
    fn parse() -> Result<Self> {
        let mut args = std::env::args().skip(1);
        let command = args.next().ok_or("expected_command")?;
        let mut options = BTreeMap::new();
        while let Some(key) = args.next() {
            if !key.starts_with("--") {
                return Err("expected_option".into());
            }
            let value = args.next().ok_or("missing_option_value")?;
            if options.insert(key, value).is_some() {
                return Err("duplicate_option".into());
            }
        }
        Ok(Self { command, options })
    }
    fn number(&self, name: &str, default: u64) -> Result<u64> {
        self.options.get(name).map_or(Ok(default), |s| {
            s.parse().map_err(|_| "invalid_number".into())
        })
    }
    fn directory(&self) -> Result<PathBuf> {
        self.options
            .get("--dir")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("NIANLAI_G0_DATA_DIR").map(PathBuf::from))
            .ok_or("explicit_data_directory_required".into())
    }
}
fn event(number: u64) -> CopyEvent {
    CopyEvent {
        time_ms: store::now_ms(),
        sequence: number as u32,
        source_process: "synthetic.exe".into(),
        // M1-S：来源应用路径；合成数据没有真实进程，留空（只记进程名）。
        source_path: String::new(),
        owner_process: "synthetic.exe".into(),
        owner_pid: 0,
        observed: Instant::now(),
    }
}
fn write(db: &Store, content: Arc<Content>, number: u64) -> Result<store::Ingest> {
    let result = db.ingest(content, event(number), false, Instant::now())?;
    if !result.transaction_ok || result.event_id.is_none() {
        return Err("expected_committed_copy_event".into());
    }
    Ok(result)
}
fn percentiles(values: &mut [f64]) -> Value {
    values.sort_by(f64::total_cmp);
    let at = |n: usize| {
        values
            .get((values.len() * n).div_ceil(100).saturating_sub(1))
            .copied()
            .unwrap_or_default()
    };
    json!({"samples":values.len(),"p50_ms":at(50),"p95_ms":at(95),"max_ms":at(100)})
}
fn sizes(directory: &Path) -> Value {
    let size = |name: &str| {
        std::fs::metadata(directory.join(name))
            .map(|m| m.len())
            .unwrap_or_default()
    };
    let db = size("history.db");
    let wal = size("history.db-wal");
    json!({"db_bytes":db,"wal_bytes":wal,"total_bytes":db+wal})
}
fn fill(directory: &Path, count: u64, seed: u64) -> Result<Value> {
    let db = Store::open(directory, sha256)?;
    if db.cache().total != 0 {
        return Err("fill_requires_empty_database".into());
    }
    let started = Instant::now();
    let mut times = Vec::new();
    let mut number = 0;
    let mut binary = 0;
    let mut files = 0;
    for index in 1..=count {
        let content = data::sample(index, seed)?;
        binary += usize::from(content.formats.contains_key(PNG));
        files += usize::from(content.formats.contains_key(store::content::FILES));
        number += 1;
        times.push(write(&db, content.clone(), number)?.write_ms);
        if index % 20 == 0 {
            number += 1;
            times.push(write(&db, content, number)?.write_ms);
        }
        if index % 10000 == 0 {
            eprintln!("fill_progress records={index} commits={number}");
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    let counts = db.reader()?.counts()?;
    if counts.contents != count as usize || counts.events != number as usize {
        return Err("fill_counts_mismatch".into());
    }
    drop(db);
    Ok(
        json!({"command":"fill","seed":seed,"requested":count,"counts":counts,"binary_records":binary,"file_records":files,
        "elapsed_seconds":elapsed,"commits_per_second":number as f64/elapsed,"records_per_second":count as f64/elapsed,
        "write":percentiles(&mut times),"size":sizes(directory)}),
    )
}
fn search_bench(directory: &Path, iterations: u64) -> Result<Value> {
    if iterations == 0 {
        return Err("iterations_must_be_positive".into());
    }
    let reader = Reader::open(directory, sha256)?;
    let categories = [
        ("one_hanzi", ["订", "文", "龘"]),
        ("two_hanzi", ["订单", "报告", "龘龘"]),
        ("three_plus_hanzi", ["订单尾号", "项目报告", "不存在的词语"]),
        ("english_case", ["cLiPbOaRd", "RuSt", "NONEXISTENTwordxyz"]),
        ("url", ["?id=", "example.com/path", "/absent-path/"]),
        ("numbers", ["12345", "2026", "999999999999999999"]),
        ("symbols", ["%", "=>", "@@@absent"]),
        ("emoji", ["😀", "👩‍💻", "🦄"]),
        (
            "multiple_words",
            ["订单 Rust", "example.com 12345", "订单 不存在的词语"],
        ),
    ];
    let mut reports = Vec::new();
    for (category, queries) in categories {
        let mut times = Vec::new();
        let mut hits = 0;
        let mut zero = 0;
        for index in 0..iterations {
            let result = reader.search(queries[index as usize % queries.len()])?;
            times.push(result.elapsed_ms);
            hits += result.items.len();
            zero += usize::from(result.items.is_empty());
        }
        reports.push(json!({"category":category,"latency":percentiles(&mut times),"total_hits":hits,"zero_result_queries":zero}));
    }
    Ok(json!({"command":"search-bench","counts":reader.counts()?,"categories":reports}))
}
fn writer(directory: &Path, seed: u64) -> Result<()> {
    let db = Store::open(directory, sha256)?;
    let mut number = db.reader()?.counts()?.events as u64;
    let mut previous: Option<Arc<Content>> = None;
    let mut output = io::stdout().lock();
    loop {
        number += 1;
        let content = if number.is_multiple_of(11)
            && let Some(previous) = &previous
        {
            previous.clone()
        } else {
            data::writer_sample(number, seed)?
        };
        let result = write(&db, content.clone(), number)?;
        previous = Some(content);
        writeln!(output, "{}", result.event_id.unwrap())?;
        output.flush()?;
    }
}
fn blob_bench(directory: &Path) -> Result<Value> {
    let db = Store::open(directory, sha256)?;
    let mut reports = Vec::new();
    for (index, mib) in [1, 8, 32].into_iter().enumerate() {
        let prepare = Instant::now();
        let content = Content::new(
            BTreeMap::from([(
                PNG.into(),
                Data::Bytes(data::binary(mib * 1024 * 1024, 0x1729 + mib as u64).into()),
            )]),
            sha256,
        )?;
        let prepare_ms = prepare.elapsed().as_secs_f64() * 1000.0;
        let started = Instant::now();
        let result = write(&db, content.clone(), index as u64 + 1)?;
        let write_ms = started.elapsed().as_secs_f64() * 1000.0;
        let started = Instant::now();
        let loaded = db
            .reader()?
            .content(result.record_id.unwrap())?
            .ok_or("blob_missing")?;
        let read_ms = started.elapsed().as_secs_f64() * 1000.0;
        if loaded.raw(PNG) != content.raw(PNG) {
            return Err("blob_mismatch".into());
        }
        reports.push(json!({"mib":mib,"prepare_ms":prepare_ms,"write_ms":write_ms,"transaction_ms":result.write_ms,"read_and_verify_ms":read_ms}));
    }
    Ok(json!({"command":"blob-bench","results":reports}))
}
fn run() -> Result<()> {
    let args = Args::parse()?;
    // Fixture creation must never inherit a user's data directory from the environment.
    if args.command == "paste-fixture" {
        println!("{}", paste_fixture::create(&args.options)?);
        return Ok(());
    }
    let directory = args.directory()?;
    let result=match args.command.as_str() {
        "fill"=>fill(&directory,args.number("--count",10000)?,args.number("--seed",0x4e49414e4c4149)?)?,
        "search-bench"=>search_bench(&directory,args.number("--iterations",100)?)?,
        "writer"=>{writer(&directory,args.number("--seed",0x4e49414e4c4149)?)?;return Ok(());},
        "check"=>{
            let counts=store::check(&directory)?;
            let last=args.options.get("--last-id").map(|v|v.parse::<i64>()).transpose()?;
            if let Some(id)=last && !Reader::open(&directory,sha256)?.event_exists(id)? {return Err("committed_event_missing".into());}
            json!({"command":"check","ok":true,"counts":counts,"last_committed_id":last,"last_committed_present":last.map(|_|true)})
        }
        "snapshot"=>json!({"command":"snapshot","report":store::snapshot(&directory)?}),
        "restore"=>{
            let source=args.options.get("--snapshot").ok_or("snapshot_required")?;
            let backup=store::restore(&directory,Path::new(source))?;
            json!({"command":"restore","backup":backup,"counts":store::check(&directory)?})
        }
        "open-bench"=>{
            let at=Instant::now();let db=Store::open(&directory,sha256)?;
            let report=json!({"command":"open-bench","elapsed_ms":at.elapsed().as_secs_f64()*1000.0,"report":db.report(),"cached":db.cache().items.len(),"size":sizes(&directory)});
            println!("{report}");io::stdout().flush()?;
            std::thread::sleep(std::time::Duration::from_millis(args.number("--hold-ms",0)?));return Ok(());
        }
        "blob-bench"=>blob_bench(&directory)?,
        _=>return Err("commands: fill search-bench writer check snapshot restore open-bench blob-bench paste-fixture; --dir PATH is required".into()),
    };
    println!("{result}");
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("g0_store_error: {error}");
        std::process::exit(1);
    }
}
