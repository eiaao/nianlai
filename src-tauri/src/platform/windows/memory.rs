use crate::platform::Result;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX2,
};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

struct Entry {
    pid: u32,
    parent: u32,
    name: String,
}

struct Usage {
    private_ws: u64,
    private_commit: u64,
    page_faults: u32,
}

fn exe_name(mut file: &[u16]) -> String {
    if let Some(end) = file.iter().position(|&c| c == 0) {
        file = &file[..end];
    }
    String::from_utf16_lossy(file)
}

fn snapshot_processes() -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    unsafe {
        let snapshot =
            CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).map_err(|e| e.to_string())?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                entries.push(Entry {
                    pid: entry.th32ProcessID,
                    parent: entry.th32ParentProcessID,
                    name: exe_name(&entry.szExeFile),
                });
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    Ok(entries)
}

fn usage(pid: u32) -> Option<Usage> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut counters = PROCESS_MEMORY_COUNTERS_EX2 {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32,
            ..Default::default()
        };
        let result = GetProcessMemoryInfo(
            process,
            &mut counters as *mut PROCESS_MEMORY_COUNTERS_EX2 as *mut PROCESS_MEMORY_COUNTERS,
            counters.cb,
        );
        let _ = CloseHandle(process);
        result.ok()?;
        Some(Usage {
            private_ws: counters.PrivateWorkingSetSize as u64,
            private_commit: counters.PrivateUsage as u64,
            page_faults: counters.PageFaultCount,
        })
    }
}

// 从当前进程出发按父子关系收集整棵进程树，WebView2 各进程都在其中；用 visited 防止父号复用成环。
fn collect_tree(entries: &[Entry], root: u32) -> Vec<&Entry> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut by_pid: HashMap<u32, &Entry> = HashMap::new();
    for entry in entries {
        children.entry(entry.parent).or_default().push(entry.pid);
        by_pid.insert(entry.pid, entry);
    }
    let mut tree = Vec::new();
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    visited.insert(root);
    queue.push_back(root);
    while let Some(pid) = queue.pop_front() {
        let Some(entry) = by_pid.get(&pid) else {
            continue;
        };
        tree.push(*entry);
        for pid in children.get(&pid).into_iter().flatten() {
            if visited.insert(*pid) {
                queue.push_back(*pid);
            }
        }
    }
    tree
}

fn megabytes(bytes: u64) -> f64 {
    (bytes as f64 / 1048576.0 * 10.0).round() / 10.0
}

pub fn sample() -> Value {
    let mut error = None;
    let entries = snapshot_processes().unwrap_or_else(|reason| {
        error = Some(reason);
        Vec::new()
    });
    let tree = collect_tree(&entries, unsafe { GetCurrentProcessId() });
    let mut unreadable = 0;
    let mut private_ws = 0;
    let mut private_commit = 0;
    let mut processes = Vec::new();
    for entry in tree {
        // 打不开或读失败的进程不计入合计，只计入 unreadable。
        if let Some(counters) = usage(entry.pid) {
            private_ws += counters.private_ws;
            private_commit += counters.private_commit;
            processes.push(json!({
                "pid":entry.pid, "parent":entry.parent, "name":entry.name,
                "private_ws_mb":megabytes(counters.private_ws),
                "private_commit_mb":megabytes(counters.private_commit),
                "page_faults":counters.page_faults,
            }));
        } else {
            unreadable += 1;
        }
    }
    json!({
        "private_ws_mb":megabytes(private_ws),
        "private_commit_mb":megabytes(private_commit),
        "unreadable":unreadable,
        "processes":processes,
        "error":error,
    })
}
