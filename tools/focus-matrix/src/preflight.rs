use crate::{
    cli::Options,
    scenario::{Config, Scenario},
};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Serialize)]
pub struct Environment {
    pub platform: String,
    // 整机的进程和窗口列表只用来判断，不写进 --json 输出和 run.json。
    #[serde(skip_serializing)]
    pub processes: Vec<Process>,
    #[serde(skip_serializing)]
    pub window_classes: Vec<String>,
    pub monitors: usize,
    pub chinese_keyboard: bool,
    pub remote: bool,
    pub errors: Vec<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct Process {
    pub pid: u32,
    pub name: String,
}
#[derive(Debug, Serialize)]
pub struct FileInfo {
    pub path: PathBuf,
    pub exists: bool,
    pub bytes: Option<u64>,
    pub modified_ms: Option<u64>,
}
pub fn file_info(path: &Path) -> FileInfo {
    let m = std::fs::metadata(path).ok().filter(|m| m.is_file());
    FileInfo {
        path: path.into(),
        exists: m.is_some(),
        bytes: m.as_ref().map(|m| m.len()),
        modified_ms: m
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64),
    }
}
pub fn expand_env(s: &str) -> PathBuf {
    let mut result = s.to_owned();
    for key in [
        "WINDIR",
        "LOCALAPPDATA",
        "ProgramFiles",
        "ProgramFiles(x86)",
    ] {
        if let Ok(value) = std::env::var(key) {
            result = result.replace(&format!("%{key}%"), &value);
        }
    }
    result.into()
}
// 每次真跑都给样机建独立的数据目录（NIANLAI_G0_DATA_DIR）；样机四起，日志也写在这个目录里。
pub const NIANLAI_DATA: &str = "nianlai-data";
pub fn nianlai_log(base: &Path) -> PathBuf {
    base.join(NIANLAI_DATA).join("panel-log.jsonl")
}
fn readable_ancestor(path: &Path) -> bool {
    let mut p = if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        path
    };
    while !p.exists() {
        let Some(parent) = p.parent() else {
            return false;
        };
        p = parent;
    }
    if p.as_os_str().is_empty() {
        p = Path::new(".");
    }
    p.is_dir() && std::fs::read_dir(p).is_ok()
}
#[derive(Debug, Serialize)]
pub struct PlanItem {
    pub id: String,
    pub name: String,
    pub row: u8,
    pub modes: Vec<String>,
    pub trials: u32,
    pub status: String,
    pub reason: String,
    pub program: Option<FileInfo>,
}
#[derive(Debug, Serialize)]
pub struct Preflight {
    pub ready: bool,
    pub dry_run: bool,
    pub executable: FileInfo,
    pub environment: Environment,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
    pub plan: Vec<PlanItem>,
    pub estimated_minutes: f64,
    pub upper_minutes: f64,
    pub temp: PathBuf,
    pub manual_rows: Vec<u8>,
}
pub fn inspect(o: &Options, c: &Config, selected: &[&Scenario]) -> Preflight {
    #[cfg(windows)]
    let environment = crate::win::environment();
    #[cfg(not(windows))]
    let environment = Environment {
        platform: std::env::consts::OS.into(),
        errors: vec!["运行只支持 Windows；纯逻辑场景校验可用".into()],
        ..Default::default()
    };
    let exe = file_info(&o.exe);
    let mut blockers = environment.errors.clone();
    if !exe.exists {
        blockers.push(format!("缺少样机：{}", o.exe.display()));
    }
    if environment
        .processes
        .iter()
        .any(|p| p.name.eq_ignore_ascii_case("nianlai.exe"))
    {
        blockers.push("已有 nianlai 正在运行；真跑前请退出".into());
    }
    let temp = std::env::temp_dir();
    if !temp.is_dir() || !readable_ancestor(&temp) {
        blockers.push("TEMP 目录不可读或不存在".into());
    }
    if !readable_ancestor(o.out.parent().unwrap_or(Path::new("."))) {
        blockers.push("输出目录的现存祖先不可读".into());
    }
    let mut warnings = vec![
        "只读检查不验证写权限和快捷键实际注册；未读取用户窗口标题/内容，未接触剪贴板。".into(),
        "真跑会覆盖剪贴板；请切英文输入法、关闭自动锁屏并离开电脑；动一下鼠标最可靠。".into(),
        "记事本/资源管理器转交窗口、新版 UIA、收藏夹气泡和输入法均待用户离机后冒烟验证。".into(),
        "已知历史不足时会额外打开原生 helper 窗口做搜索预热（trial=0，不计入验收）。".into(),
    ];
    if environment.chinese_keyboard {
        warnings
            .push("检测到中文键盘布局：首字使用普通虚拟键，中文组合输入会使搜索核对失败。".into());
    }
    if environment.remote {
        warnings.push("当前是远程桌面会话，焦点/注入行为可能受远程端影响。".into());
    }
    if environment
        .window_classes
        .iter()
        .any(|s| s == "CabinetWClass")
    {
        warnings.push("已有资源管理器文件夹窗口：可能复用标签；工具只认新窗口，否则跳过。".into());
    }
    let plan = selected
        .iter()
        .map(|s| {
            let program = s.target.program.as_ref().map(|p| file_info(&expand_env(p)));
            let (status, reason) = if s.target.second_monitor && environment.monitors < 2 {
                ("skipped", "只有一块显示器，跳过第二屏")
            } else if program.as_ref().is_some_and(|p| !p.exists) {
                ("blocked", "目标可执行文件不存在")
            } else if s.target.exclusive
                && environment
                    .processes
                    .iter()
                    .any(|p| p.name.eq_ignore_ascii_case(&s.target.process))
            {
                ("blocked", "独占目标已有同名进程；不会碰现有窗口")
            } else {
                ("ready", "就绪（真跑路径尚需离机验证）")
            };
            PlanItem {
                id: s.id.clone(),
                name: s.name.clone(),
                row: s.row,
                modes: o.modes.iter().map(|m| m.label().into()).collect(),
                trials: o.trials,
                status: status.into(),
                reason: reason.into(),
                program,
            }
        })
        .collect::<Vec<_>>();
    let count = plan.iter().filter(|p| p.status == "ready").count() as f64
        * o.modes.len() as f64
        * o.trials as f64;
    Preflight {
        ready: blockers.is_empty(),
        dry_run: !o.real_input,
        executable: exe,
        environment,
        blockers,
        warnings,
        plan,
        estimated_minutes: count * 4.0 / 60.0 + selected.len() as f64 * 0.5,
        upper_minutes: count * c.timeouts.trial_ms as f64 / 60000.0 + selected.len() as f64,
        temp,
        manual_rows: vec![4, 5, 6, 7],
    }
}
impl Preflight {
    pub fn chinese(&self) -> String {
        let mut s = format!(
            "拈来 focus-matrix {}\n场景校验：通过\n样机：{}\n存在：{}；大小：{}；修改时间（Unix ms）：{}\n环境：{}；显示器 {}；中文布局 {}；远程会话 {}\n",
            if self.dry_run {
                "只读空跑"
            } else {
                "真跑预检"
            },
            self.executable.path.display(),
            self.executable.exists,
            self.executable
                .bytes
                .map(|n| n.to_string())
                .unwrap_or_else(|| "—".into()),
            self.executable
                .modified_ms
                .map(|n| n.to_string())
                .unwrap_or_else(|| "—".into()),
            self.environment.platform,
            self.environment.monitors,
            self.environment.chinese_keyboard,
            self.environment.remote
        );
        for issue in &self.blockers {
            s += &format!("阻断：{issue}\n");
        }
        for warning in &self.warnings {
            s += &format!("提示：{warning}\n");
        }
        s += "计划打开以下独立目标（本次空跑不打开）：\n";
        for p in &self.plan {
            s += &format!(
                "  {}：{}；第 {} 行；{} × {} 次；{}：{}\n",
                p.id,
                p.name,
                p.row,
                p.modes.join("/"),
                p.trials,
                p.status,
                p.reason
            );
        }
        s += &format!(
            "预计 {:.1} 分钟；按单次上限估算 {:.1} 分钟（另受 max-minutes 限制）。\n手工：第 4/5/6/7 行，另含独占全屏游戏。\nTEMP：{}\n日志：每次运行的临时目录下 {}\\panel-log.jsonl（样机的数据目录）\n结论：{}\n",
            self.estimated_minutes,
            self.upper_minutes,
            self.temp.display(),
            NIANLAI_DATA,
            if self.ready {
                "全局预检就绪；逐场景阻断/跳过见上"
            } else {
                "存在阻断问题"
            }
        );
        s
    }
}
