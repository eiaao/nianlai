//! 假密码管理器测试（M2 测试约定）：不碰真实的 KeePass 等密码管理器。
//!
//! 做法：把测试程序自己复制（优先硬链接）成 `KeePass.exe` 之类的文件名，再以辅助模式启动，
//! 由它往真实系统剪贴板写内容。采集看到的剪贴板所有者进程名就是那个文件名，
//! 和真实的密码管理器走完全相同的路径（采集服务、排除名单、标记检查、历史库、日志）。
//! 不启动完整的应用和界面，不发送任何按键。
//!
//! 覆盖：默认排除名单里的每个程序名（含大小写差异）、四种“不要记录”标记、普通进程名的对照组。
//! 断言：被跳过的内容不进历史库、不进搜索、不进索引文件，且日志里找不到任何一份正文。
//!
//! 注意：测试会改写系统剪贴板，结束时只能还原纯文本内容（图片等其他格式无法还原）；
//! 正在运行的拈来如果同时开着，也会把对照组的内容记进它自己的历史。
//! 平时的 `cargo test` 不跑它（#[ignore]），CI 单独跑。手动运行（先关掉日常的拈来）：
//! `cargo test --target x86_64-pc-windows-msvc fake_password_manager -- --ignored --nocapture`

use super::super::{clipboard, hash};
use super::*;
use crate::{
    capture::{MARKERS, rules::gate},
    history::History,
    settings::{Capture, DEFAULT_EXCLUDED_APPS, Directories},
};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::SystemTime,
};
use windows::{
    Win32::{
        Foundation::{HANDLE, HGLOBAL},
        System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
    },
    core::PCWSTR,
};

const DIR_ENV: &str = "NIANLAI_FAKE_PM_DIR";
const TEXT_ENV: &str = "NIANLAI_FAKE_PM_TEXT";
const MARKER_ENV: &str = "NIANLAI_FAKE_PM_MARKER";
const DWORD_ENV: &str = "NIANLAI_FAKE_PM_DWORD";
const CF_UNICODETEXT: u32 = 13;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
// 不在排除名单里的普通程序名，作对照组。
const NORMAL_APP: &str = "FakeNotepad.exe";

fn error(what: impl ToString) -> String {
    what.to_string()
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// 申请一块可移动内存放数据，所有权交给 SetClipboardData。
unsafe fn global(bytes: &[u8]) -> Result<HANDLE> {
    unsafe {
        let memory: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)).map_err(error)?;
        let pointer = GlobalLock(memory);
        if pointer.is_null() {
            return Err("GlobalLock 失败".into());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer.cast(), bytes.len());
        let _ = GlobalUnlock(memory);
        Ok(HANDLE(memory.0))
    }
}

fn owner_window() -> Result<HWND> {
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!(""),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            None,
            None,
        )
        .map_err(error)
    }
}

/// 往剪贴板立即写入一段文本，外加若干标记格式（None 表示存在型标记，Some 是 DWORD 值）。
fn write_clipboard(owner: HWND, text: &str, markers: &[(&str, Option<u32>)]) -> Result<()> {
    unsafe {
        let mut opened = false;
        for _ in 0..100 {
            if OpenClipboard(Some(owner)).is_ok() {
                opened = true;
                break;
            }
            thread::sleep(Duration::from_millis(30));
        }
        if !opened {
            return Err("打开剪贴板失败".into());
        }
        let result = (|| -> Result<()> {
            EmptyClipboard().map_err(error)?;
            let units: Vec<u8> = wide(text).into_iter().flat_map(u16::to_le_bytes).collect();
            SetClipboardData(CF_UNICODETEXT, Some(global(&units)?)).map_err(error)?;
            for (name, value) in markers {
                let name = wide(name);
                let format = RegisterClipboardFormatW(PCWSTR(name.as_ptr()));
                if format == 0 {
                    return Err("注册标记格式失败".into());
                }
                let data = value.unwrap_or(0).to_le_bytes();
                SetClipboardData(format, Some(global(&data)?)).map_err(error)?;
            }
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}

fn read_text() -> Option<String> {
    unsafe {
        OpenClipboard(None).ok()?;
        let text = (|| {
            let handle = GetClipboardData(CF_UNICODETEXT).ok()?;
            let memory = HGLOBAL(handle.0);
            let pointer = GlobalLock(memory);
            if pointer.is_null() {
                return None;
            }
            let pointer = pointer.cast::<u16>();
            let mut length = 0;
            while *pointer.add(length) != 0 {
                length += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(pointer, length));
            let _ = GlobalUnlock(memory);
            Some(text)
        })();
        let _ = CloseClipboard();
        text
    }
}

/// 测试开始前记下剪贴板里的纯文本，结束（含 panic）时还原。
struct RestoreClipboard(Option<String>);
impl Drop for RestoreClipboard {
    fn drop(&mut self) {
        let Ok(owner) = owner_window() else { return };
        match &self.0 {
            Some(text) => {
                let _ = write_clipboard(owner, text, &[]);
            }
            None => unsafe {
                if OpenClipboard(Some(owner)).is_ok() {
                    let _ = EmptyClipboard();
                    let _ = CloseClipboard();
                }
            },
        }
        unsafe {
            let _ = DestroyWindow(owner);
        }
    }
}

/// 辅助模式的入口。正常 `cargo test` 里没有环境变量，立即返回；
/// 被测试复制并改名后，以 `--exact` 再次启动才会真正工作。
/// 写完剪贴板后创建 ready 文件，并保持存活（泵消息，避免卡住别人清空剪贴板）直到出现 stop 文件。
#[test]
fn fake_password_manager_helper() {
    let Some(dir) = std::env::var_os(DIR_ENV).map(PathBuf::from) else {
        return;
    };
    let text = std::env::var(TEXT_ENV).unwrap_or_default();
    let marker = std::env::var(MARKER_ENV).ok();
    let dword = std::env::var(DWORD_ENV)
        .ok()
        .and_then(|value| value.parse::<u32>().ok());
    let owner = owner_window().expect("创建所有者窗口");
    let markers: Vec<(&str, Option<u32>)> = marker.iter().map(|m| (m.as_str(), dword)).collect();
    let status = match write_clipboard(owner, &text, &markers) {
        Ok(()) => "ok".to_owned(),
        Err(error) => error,
    };
    fs::write(dir.join("ready"), status).expect("写 ready 文件");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut message = MSG::default();
    while !dir.join("stop").exists() && Instant::now() < deadline {
        unsafe {
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        thread::sleep(Duration::from_millis(15));
    }
    unsafe {
        let _ = DestroyWindow(owner);
    }
}

struct Helper {
    child: Child,
    dir: PathBuf,
}
impl Helper {
    fn stop(&mut self) {
        let _ = fs::write(self.dir.join("stop"), b"");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        // 只终止自己启动的辅助进程。
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Drop for Helper {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Lab {
    root: PathBuf,
    exes: HashMap<String, PathBuf>,
    cases: usize,
    logger: Logger,
    logs: PathBuf,
}

impl Lab {
    /// 以指定文件名得到测试程序的副本：先试硬链接（不占空间），不行再复制。
    fn exe(&mut self, name: &str) -> Result<PathBuf> {
        let key = name.to_ascii_lowercase();
        if let Some(path) = self.exes.get(&key) {
            return Ok(path.clone());
        }
        let directory = self.root.join(format!("bin-{}", self.exes.len()));
        fs::create_dir_all(&directory).map_err(error)?;
        let path = directory.join(name);
        let current = std::env::current_exe().map_err(error)?;
        if fs::hard_link(&current, &path).is_err() {
            fs::copy(&current, &path).map_err(error)?;
        }
        self.exes.insert(key, path.clone());
        Ok(path)
    }

    fn events(&self) -> Vec<Value> {
        self.logger.flush();
        let mut events = Vec::new();
        for entry in fs::read_dir(&self.logs).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // 兼容文件 panel-log.jsonl 是同一份事件的副本，只读按天的文件。
            if !(name.starts_with("panel-2") && name.ends_with(".jsonl")) {
                continue;
            }
            let text = fs::read_to_string(entry.path()).unwrap_or_default();
            events.extend(
                text.lines()
                    .filter_map(|line| serde_json::from_str(line).ok()),
            );
        }
        events
    }

    /// 让名为 `name` 的假进程写入剪贴板，等采集给出这次的结论，返回那条 capture 日志。
    fn copy(
        &mut self,
        name: &str,
        text: &str,
        marker: Option<(&str, Option<u32>)>,
    ) -> Result<Value> {
        let exe = self.exe(name)?;
        self.cases += 1;
        let dir = self.root.join(format!("case-{}", self.cases));
        fs::create_dir_all(&dir).map_err(error)?;
        let started = now_ms();
        let mut command = Command::new(exe);
        command
            .args([
                &helper_test_name(),
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(DIR_ENV, &dir)
            .env(TEXT_ENV, text)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW);
        if let Some((marker, value)) = marker {
            command.env(MARKER_ENV, marker);
            if let Some(value) = value {
                command.env(DWORD_ENV, value.to_string());
            }
        }
        let mut helper = Helper {
            child: command.spawn().map_err(error)?,
            dir: dir.clone(),
        };
        let ready = poll(Duration::from_secs(15), || {
            fs::read_to_string(dir.join("ready")).ok()
        })
        .ok_or("假进程没有按时写好剪贴板")?;
        if ready != "ok" {
            return Err(format!("假进程写剪贴板失败：{ready}"));
        }
        let event = poll(Duration::from_secs(15), || {
            self.events().into_iter().rev().find(|event| {
                event["event"] == "capture"
                    && event["time_ms"]
                        .as_u64()
                        .is_some_and(|time| time >= started)
                    && event["owner_process"]
                        .as_str()
                        .is_some_and(|owner| owner.eq_ignore_ascii_case(name))
                    && event["reason"] != "sequence_changed"
            })
        })
        .ok_or_else(|| format!("没有等到 {name} 的 capture 日志"))?;
        helper.stop();
        Ok(event)
    }
}

/// libtest 的测试名不带 crate 名。
fn helper_test_name() -> String {
    let path = module_path!().split_once("::").map_or("", |(_, rest)| rest);
    format!("{path}::fake_password_manager_helper")
}

fn poll<T>(timeout: Duration, mut check: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = check() {
            return Some(value);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(40));
    }
}

fn files_under(directory: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_under(&path, found);
        } else {
            found.push(path);
        }
    }
}

/// 目录下任何文件里是否有这段正文（UTF-8 或 UTF-16LE 原样字节）。
fn contains_body(directory: &Path, body: &str) -> Vec<PathBuf> {
    let utf8 = body.as_bytes().to_vec();
    let utf16: Vec<u8> = body.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut files = Vec::new();
    files_under(directory, &mut files);
    files
        .into_iter()
        .filter(|path| {
            let Ok(bytes) = fs::read(path) else {
                return false;
            };
            [&utf8, &utf16].iter().any(|needle| {
                bytes
                    .windows(needle.len())
                    .any(|part| part == needle.as_slice())
            })
        })
        .collect()
}

fn assert_not_recorded(event: &Value, label: &str) {
    assert_eq!(event["outcome"], "skipped", "{label}: {event}");
    assert!(event["record_id"].is_null(), "{label}: {event}");
    assert_eq!(event["saved_formats"], json!([]), "{label}: {event}");
}

// 会改写系统剪贴板，日常开着的拈来也会记下对照组的文字，所以平时的 cargo test 不跑；
// CI 里单独用 --ignored 跑（见 .github/workflows/ci.yml）。
#[test]
#[ignore = "改写系统剪贴板：cargo test fake_password_manager -- --ignored"]
fn fake_password_manager_is_not_recorded_and_leaves_no_trace() {
    if std::env::var_os(DIR_ENV).is_some() {
        return;
    }
    let root = std::env::temp_dir().join(format!("nianlai-fake-pm-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let directories = Directories {
        settings_file: root.join("unused-settings.json"),
        data: root.join("data"),
        logs: root.join("logs"),
        data_source: "fake_password_manager_test",
        test_data_dir: true,
    };
    fs::create_dir_all(&directories.logs).unwrap();
    let logger = Logger::with_settings(&directories, 5).unwrap();
    assert_eq!(
        logger.directories.logs, directories.logs,
        "日志目录出现 fallback"
    );

    // 用默认采集规则（含默认排除名单），不依赖也不改动真实设置。
    gate().apply(&Capture::default());
    let _restore = RestoreClipboard(read_text());
    let history = Arc::new(History::new(hash::sha256, logger.clone()));
    history.initialize();
    let clipboard = clipboard::Service::start(logger.clone(), history.clone()).unwrap();
    let service = Service::start(
        logger.clone(),
        history.clone(),
        &clipboard,
        true,
        Arc::new(|| {}),
    )
    .unwrap();
    let mut lab = Lab {
        root: root.clone(),
        exes: HashMap::new(),
        cases: 0,
        logger: logger.clone(),
        logs: directories.logs.clone(),
    };
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    // 正文带唯一的 ASCII 词（方便全文搜索）和中文，避免撞上敏感内容规则。
    let body = |tag: &str| format!("fakepm{tag}{stamp} 拈来假密码管理器测试正文 {tag}");
    let mut skipped = Vec::new();
    let mut stored = Vec::new();

    // 1. 默认排除名单里的每个程序名；再用大小写不同的名字确认不区分大小写。
    let mut names: Vec<String> = DEFAULT_EXCLUDED_APPS.map(String::from).to_vec();
    names.push("keepassxc.EXE".into());
    for (index, name) in names.iter().enumerate() {
        let text = body(&format!("excluded{index}"));
        let event = lab.copy(name, &text, None).unwrap();
        assert_not_recorded(&event, name);
        assert_eq!(event["reason"], "excluded_app", "{name}: {event}");
        // 排除应用在列出格式之前就跳过，连格式名都没有读。
        assert_eq!(event["format_count"], 0, "{name}: {event}");
        skipped.push(text);
    }

    // 2. 四种“不要记录”标记，进程名是普通程序：存在型标记，DWORD 标记值为 0。
    for marker in MARKERS {
        let value = (marker == "CanIncludeInClipboardHistory"
            || marker == "CanUploadToCloudClipboard")
            .then_some(0);
        let text = body(&format!("marker{}", skipped.len()));
        let event = lab.copy(NORMAL_APP, &text, Some((marker, value))).unwrap();
        assert_not_recorded(&event, marker);
        assert_eq!(event["reason"], "marker", "{marker}: {event}");
        assert_eq!(event["marker"], marker, "{marker}: {event}");
        skipped.push(text);
    }

    // 3. 对照组：同样的普通进程名，没有标记；DWORD 标记值为 1（允许）也应正常记录。
    let control = body("control");
    let event = lab.copy(NORMAL_APP, &control, None).unwrap();
    assert!(event["reason"].is_null(), "对照组被跳过：{event}");
    assert!(
        ["saved", "deferred"].contains(&event["outcome"].as_str().unwrap()),
        "{event}"
    );
    stored.push(control);
    for marker in ["CanIncludeInClipboardHistory", "CanUploadToCloudClipboard"] {
        let text = body(&format!("allowed{}", stored.len()));
        let event = lab
            .copy(NORMAL_APP, &text, Some((marker, Some(1))))
            .unwrap();
        assert!(event["reason"].is_null(), "{marker}=1 不应跳过：{event}");
        stored.push(text);
    }

    // 4. 落盘后的检查。暂存的内容先落库，再看库、搜索、索引文件和日志。
    history.flush_deferred();
    thread::sleep(Duration::from_millis(300));
    let snapshot = serde_json::to_string(&history.snapshot()).unwrap();
    for text in &stored {
        assert!(snapshot.contains(text.as_str()), "对照组没进历史：{text}");
        let found =
            serde_json::to_string(&history.search(text.split(' ').next().unwrap(), 1).unwrap())
                .unwrap();
        assert!(found.contains(text.as_str()), "对照组搜不到：{text}");
    }
    for text in &skipped {
        assert!(
            !snapshot.contains(text.as_str()),
            "被跳过的内容进了历史：{text}"
        );
        let query = text.split(' ').next().unwrap();
        let found = serde_json::to_string(&history.search(query, 2).unwrap()).unwrap();
        assert!(
            !found.contains(text.as_str()) && !found.contains(query),
            "搜到了被跳过的内容：{text}"
        );
    }
    service.shutdown();
    clipboard.shutdown();
    drop(history);
    lab.logger.flush();
    // 对照组至少要在库文件里找得到，证明下面的“找不到”不是扫描方式失灵。
    let data = &directories.data;
    for text in &stored {
        assert!(
            !contains_body(data, text).is_empty(),
            "扫描方式失灵：库目录里找不到对照组正文 {text}"
        );
    }
    for text in &skipped {
        assert_eq!(
            contains_body(data, text),
            Vec::<PathBuf>::new(),
            "库或索引里有被跳过的正文"
        );
    }
    // 日志里谁的正文都不该有（包括对照组）。
    for text in skipped.iter().chain(&stored) {
        assert_eq!(
            contains_body(&directories.logs, text),
            Vec::<PathBuf>::new(),
            "日志里有正文"
        );
    }

    drop(lab);
    let _ = fs::remove_dir_all(&root);
}
