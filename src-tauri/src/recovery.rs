//! 崩溃自动拉起（守护者）。
//!
//! nianlai.exe 启动时先当“守护者”：不初始化 Tauri、不碰剪贴板、不建窗口、不写库，只以 `--child` 参数
//! 再启动一个自己作为真正的应用（其余命令行参数原样透传），然后等它退出：
//! - 退出码 0（托盘退出、Ctrl+Alt+Shift+Q、单实例发现已有实例后让位）→ 守护者随之退出，退出码 0；
//! - 其它（崩溃、被杀、release 下 panic=abort、启动失败）→ 等 2 秒重新拉起；24 小时滚动窗口内最多拉起 3 次，
//!   额度用完后再次异常退出就弹系统消息框“拈来连续异常退出 3 次，已停止自动重启。日志在 <目录>”，然后以 1 退出。
//!
//! 每次子进程退出只往日志目录的当天 UTC 文件 panel-YYYYMMDD.jsonl 追加一行：时间、退出码、第几次、动作。
//! 有效的 NIANLAI_G0_DATA_DIR 同时覆盖日志位置并启用 panel-log.jsonl 兼容副本；守护者不读设置文件，
//! 日志目录本来也不受文件设置影响。轮转删除仅由应用执行，守护者每次追加后关闭句柄，避免写入已删除的旧文件。
//!
//! 角色怎么判断（`role`，按顺序）：
//! 1. 命令行带 `--child` → 真正的应用（这是守护者拉起的子进程）。
//! 2. 环境变量 `NIANLAI_GUARD=0` → 不守护，直接当应用；`NIANLAI_GUARD=1` → 强制守护。
//! 3. 都没有：debug 构建（`pnpm tauri dev`、`cargo run`）不守护，release 构建守护。
//!    所以开发时默认就不走守护（tauri dev 热重启杀掉的是应用本身）；要在开发构建里试守护，设 `NIANLAI_GUARD=1`。
//!
//! 额度只记在守护者进程的内存里：用户手动再次启动是新的守护者，重新计数。守护者被单独结束时
//! 子进程照常运行，只是没人再拉起它；任务管理器结束“拈来”结束的是子进程，守护者会把它当异常退出拉起一次。

use crate::log;
use serde_json::json;
use std::{
    collections::VecDeque,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

pub const CHILD_FLAG: &str = "--child";
const SWITCH: &str = "NIANLAI_GUARD";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Guardian,
    Application,
}

pub fn role<I, S>(args: I, switch: Option<&str>, debug: bool) -> Role
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    if args.into_iter().any(|arg| arg.as_ref() == CHILD_FLAG) {
        return Role::Application;
    }
    match switch.map(str::trim) {
        Some("0") => Role::Application,
        Some("1") => Role::Guardian,
        _ if debug => Role::Application,
        _ => Role::Guardian,
    }
}

// 真正的应用是不是由守护者拉起的（只用于日志）。
pub fn is_child() -> bool {
    std::env::args_os().skip(1).any(|arg| arg == CHILD_FLAG)
}

#[derive(Clone, Copy)]
pub struct Policy {
    pub limit: u32,
    pub window: Duration,
    pub delay: Duration,
}
pub const POLICY: Policy = Policy {
    limit: 3,
    window: Duration::from_secs(24 * 60 * 60),
    delay: Duration::from_secs(2),
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Finished,
    Restart { attempt: u32 },
    GiveUp { attempts: u32 },
}

// 滚动窗口里的拉起次数；时间由调用方传入，便于测试。
pub struct Budget {
    policy: Policy,
    restarts: VecDeque<Instant>,
}
impl Budget {
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            restarts: VecDeque::new(),
        }
    }
    // 子进程退出后决定下一步：退出码 0 → 结束；否则窗口内还有额度就重启，没有就放弃。
    pub fn decide(&mut self, code: Option<i32>, now: Instant) -> Decision {
        if code == Some(0) {
            return Decision::Finished;
        }
        while self
            .restarts
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= self.policy.window)
        {
            self.restarts.pop_front();
        }
        let used = self.used();
        if used >= self.policy.limit {
            return Decision::GiveUp { attempts: used };
        }
        self.restarts.push_back(now);
        Decision::Restart { attempt: used + 1 }
    }
    pub fn used(&self) -> u32 {
        self.restarts.len() as u32
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exit {
    pub code: Option<i32>,
    pub spawn_failed: bool,
    pub decision: Decision,
    // 窗口内已经拉起的次数（含这次决定的拉起）。
    pub restarts: u32,
}

// 守护循环。`launch` 启动子进程并等它退出，返回退出码（启动失败返回 Err，按异常退出计）；
// 每次退出都调用 `observe`。返回守护者自己的退出码：子进程正常退出为 0，放弃拉起为 1。
pub fn supervise(
    policy: Policy,
    mut launch: impl FnMut() -> io::Result<Option<i32>>,
    mut observe: impl FnMut(Exit),
) -> i32 {
    let mut budget = Budget::new(policy);
    loop {
        let (code, spawn_failed) = match launch() {
            Ok(code) => (code, false),
            Err(_) => (None, true),
        };
        let decision = budget.decide(code, Instant::now());
        observe(Exit {
            code,
            spawn_failed,
            decision,
            restarts: budget.used(),
        });
        match decision {
            Decision::Finished => return 0,
            Decision::Restart { .. } => std::thread::sleep(policy.delay),
            Decision::GiveUp { .. } => return 1,
        }
    }
}

// 守护者的日志：共用应用的目录解析与文件名，但不读设置文件、不开线程、不轮转删除。
pub struct Journal {
    pub directory: PathBuf,
    compatibility: bool,
}
impl Journal {
    pub fn open() -> Self {
        // Resolved 只解析传入的默认值和环境变量，不调用 Store::load，也不创建设置文件。
        let resolved = crate::settings::Resolved::new(
            crate::settings::Settings::default(),
            crate::settings::settings_path(),
        );
        Self::at(
            &resolved.directories.logs,
            resolved.directories.test_data_dir,
            log::now_ms(),
        )
    }
    fn at(directory: &Path, compatibility: bool, time: u64) -> Self {
        let mut journal = Self {
            directory: directory.to_path_buf(),
            compatibility,
        };
        let _ = journal.files(time);
        journal
    }
    fn open_in(
        directory: &Path,
        compatibility: bool,
        time: u64,
    ) -> io::Result<(File, Option<File>)> {
        fs::create_dir_all(directory)?;
        let open = |name| {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(directory.join(name))
        };
        let file = open(log::daily_file_name(time))?;
        let compatibility = compatibility
            .then(|| open(log::COMPATIBILITY_FILE.to_owned()))
            .transpose()?;
        Ok((file, compatibility))
    }
    fn files(&mut self, time: u64) -> io::Result<(File, Option<File>)> {
        Self::open_in(&self.directory, self.compatibility, time).or_else(|_| {
            self.directory = log::fallback_directory();
            Self::open_in(&self.directory, self.compatibility, time)
        })
    }
    // 一行 JSON：时间、退出码、第几次、动作。整行一次写入，和应用进程交错追加也不会拆开。
    pub fn record(&mut self, exit: Exit) {
        self.record_at(exit, log::now_ms());
    }
    fn record_at(&mut self, exit: Exit, time: u64) {
        let action = match exit.decision {
            Decision::Finished => "exit",
            Decision::Restart { .. } => "restart",
            Decision::GiveUp { .. } => "stop",
        };
        let row = json!({
            "time_ms": time, "event": "guardian_child_exit",
            "exit_code": exit.code, "spawn_failed": exit.spawn_failed,
            "attempt": exit.restarts, "action": action,
        });
        if let Ok((mut file, compatibility)) = self.files(time) {
            let line = format!("{row}\n");
            let _ = file.write_all(line.as_bytes());
            if let Some(mut file) = compatibility {
                let _ = file.write_all(line.as_bytes());
            }
        }
    }
}

// 守护者入口：None 表示本进程应继续当真正的应用；Some(code) 表示守护结束，进程应以 code 退出。
pub fn guard() -> Option<i32> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let switch = std::env::var(SWITCH).ok();
    if role(&args, switch.as_deref(), cfg!(debug_assertions)) == Role::Application {
        return None;
    }
    // 找不到自己的路径就拉不起子进程，退回不守护的旧行为。
    let exe = std::env::current_exe().ok()?;
    let mut journal = Journal::open();
    let code = supervise(
        POLICY,
        || {
            Ok(Command::new(&exe)
                .arg(CHILD_FLAG)
                .args(&args)
                .status()?
                .code())
        },
        |exit| {
            journal.record(exit);
            if let Decision::GiveUp { attempts } = exit.decision {
                crate::platform::dialog::error(&format!(
                    "拈来连续异常退出 {attempts} 次，已停止自动重启。日志在 {}",
                    journal.directory.display()
                ));
            }
        },
    );
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(origin: Instant, seconds: u64) -> Instant {
        origin + Duration::from_secs(seconds)
    }
    const NONE: [&str; 0] = [];

    #[test]
    fn role_follows_child_flag_then_switch_then_build_profile() {
        assert_eq!(role(["--child"], None, false), Role::Application);
        assert_eq!(role(["--child"], Some("1"), true), Role::Application);
        assert_eq!(
            role(["--autostart", "--child"], Some("1"), false),
            Role::Application
        );
        assert_eq!(role(["--childish"], None, false), Role::Guardian);
        assert_eq!(role(NONE, Some("0"), false), Role::Application);
        assert_eq!(role(NONE, Some("1"), true), Role::Guardian);
        assert_eq!(role(NONE, Some(" 1 "), true), Role::Guardian);
        assert_eq!(role(NONE, None, true), Role::Application);
        assert_eq!(role(NONE, None, false), Role::Guardian);
        assert_eq!(role(NONE, Some("yes"), false), Role::Guardian);
        assert_eq!(role(NONE, Some("yes"), true), Role::Application);
    }

    #[test]
    fn budget_allows_three_restarts_per_rolling_day() {
        let origin = Instant::now();
        let mut budget = Budget::new(POLICY);
        assert_eq!(budget.decide(Some(0), origin), Decision::Finished);
        assert_eq!(
            budget.decide(Some(1), at(origin, 0)),
            Decision::Restart { attempt: 1 }
        );
        // 0xC0000409：release 下 panic=abort 的退出码。
        assert_eq!(
            budget.decide(Some(-1_073_740_791), at(origin, 10)),
            Decision::Restart { attempt: 2 }
        );
        assert_eq!(
            budget.decide(None, at(origin, 20)),
            Decision::Restart { attempt: 3 }
        );
        assert_eq!(
            budget.decide(Some(1), at(origin, 30)),
            Decision::GiveUp { attempts: 3 }
        );
        // 正常退出不占额度，也不清零。
        assert_eq!(budget.decide(Some(0), at(origin, 40)), Decision::Finished);
        assert_eq!(
            budget.decide(Some(1), at(origin, 50)),
            Decision::GiveUp { attempts: 3 }
        );
        // 最早那次拉起满 24 小时后，额度滚动释放。
        assert_eq!(
            budget.decide(Some(1), at(origin, 86_400)),
            Decision::Restart { attempt: 3 }
        );
        assert_eq!(
            budget.decide(Some(1), at(origin, 86_401)),
            Decision::GiveUp { attempts: 3 }
        );
        assert_eq!(
            budget.decide(Some(1), at(origin, 86_420)),
            Decision::Restart { attempt: 2 }
        );
        assert_eq!(budget.used(), 2);
    }

    #[test]
    fn supervise_restarts_until_normal_exit_and_reports_each_exit() {
        let policy = Policy {
            delay: Duration::ZERO,
            ..POLICY
        };
        let mut outcomes = VecDeque::from([
            Ok(Some(1)),
            Err(io::Error::other("拉不起来")),
            Ok(Some(0)),
            Ok(Some(9)),
        ]);
        let mut seen = Vec::new();
        let code = supervise(
            policy,
            || outcomes.pop_front().expect("正常退出后不该再拉起"),
            |exit| seen.push(exit),
        );
        assert_eq!(code, 0);
        assert_eq!(outcomes.len(), 1);
        assert_eq!(
            seen,
            [
                Exit {
                    code: Some(1),
                    spawn_failed: false,
                    decision: Decision::Restart { attempt: 1 },
                    restarts: 1
                },
                Exit {
                    code: None,
                    spawn_failed: true,
                    decision: Decision::Restart { attempt: 2 },
                    restarts: 2
                },
                Exit {
                    code: Some(0),
                    spawn_failed: false,
                    decision: Decision::Finished,
                    restarts: 2
                },
            ]
        );
    }

    #[test]
    fn supervise_gives_up_after_the_limit_and_returns_failure() {
        let policy = Policy {
            delay: Duration::ZERO,
            ..POLICY
        };
        let mut launches = 0;
        let mut last = None;
        let code = supervise(
            policy,
            || {
                launches += 1;
                Ok(Some(1))
            },
            |exit| last = Some(exit),
        );
        assert_eq!(code, 1);
        assert_eq!(launches, 4);
        assert_eq!(
            last,
            Some(Exit {
                code: Some(1),
                spawn_failed: false,
                decision: Decision::GiveUp { attempts: 3 },
                restarts: 3
            })
        );
    }

    // 用 cmd /C exit N 代替 nianlai.exe --child：按参数立刻以指定退出码退出，不建窗口、不碰剪贴板。
    #[cfg(windows)]
    #[test]
    fn real_child_exit_codes_flow_through_the_loop() {
        let policy = Policy {
            delay: Duration::ZERO,
            ..POLICY
        };
        let shell = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
        let mut codes = VecDeque::from([7, -1_073_740_791, 0]);
        let mut seen = Vec::new();
        let code = supervise(
            policy,
            || {
                let code = codes.pop_front().expect("正常退出后不该再拉起");
                Ok(Command::new(&shell)
                    .args(["/C", &format!("exit {code}")])
                    .status()?
                    .code())
            },
            |exit| seen.push((exit.code, exit.decision)),
        );
        assert_eq!(code, 0);
        assert_eq!(
            seen,
            [
                (Some(7), Decision::Restart { attempt: 1 }),
                (Some(-1_073_740_791), Decision::Restart { attempt: 2 }),
                (Some(0), Decision::Finished),
            ]
        );
    }

    #[test]
    fn journal_writes_one_line_per_exit_with_time_code_and_attempt() {
        let directory = std::env::temp_dir().join(format!(
            "nianlai-guardian-test-{}-{}",
            std::process::id(),
            log::now_ms()
        ));
        let time = log::now_ms();
        let mut journal = Journal::at(&directory, false, time);
        journal.record_at(
            Exit {
                code: Some(1),
                spawn_failed: false,
                decision: Decision::Restart { attempt: 1 },
                restarts: 1,
            },
            time,
        );
        journal.record_at(
            Exit {
                code: None,
                spawn_failed: true,
                decision: Decision::GiveUp { attempts: 3 },
                restarts: 3,
            },
            time,
        );
        journal.record_at(
            Exit {
                code: Some(0),
                spawn_failed: false,
                decision: Decision::Finished,
                restarts: 2,
            },
            time,
        );
        let text = fs::read_to_string(directory.join(log::daily_file_name(time))).unwrap();
        fs::remove_dir_all(&directory).unwrap();
        let rows: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["event"], "guardian_child_exit");
        assert_eq!(rows[0]["exit_code"], 1);
        assert_eq!(rows[0]["attempt"], 1);
        assert_eq!(rows[0]["action"], "restart");
        assert!(rows[0]["time_ms"].as_u64().unwrap() > 0);
        assert_eq!(rows[1]["exit_code"], serde_json::Value::Null);
        assert_eq!(rows[1]["spawn_failed"], true);
        assert_eq!(rows[1]["attempt"], 3);
        assert_eq!(rows[1]["action"], "stop");
        assert_eq!(rows[2]["exit_code"], 0);
        assert_eq!(rows[2]["action"], "exit");
        // 只有这几个字段：不含路径、参数或别的内容。
        let mut keys: Vec<_> = rows[0].as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "action",
                "attempt",
                "event",
                "exit_code",
                "spawn_failed",
                "time_ms"
            ]
        );
    }

    #[test]
    fn journal_appends_across_utc_midnight_and_restart_with_compatibility_copy() {
        let directory = crate::settings::tests::Directory::new();
        let exit = Exit {
            code: Some(0),
            spawn_failed: false,
            decision: Decision::Finished,
            restarts: 0,
        };
        let mut journal = Journal::at(&directory.0, true, 86_399_999);
        journal.record_at(exit, 86_399_999);
        journal.record_at(exit, 86_400_000);
        drop(journal);
        let mut journal = Journal::at(&directory.0, true, 86_400_001);
        journal.record_at(exit, 86_400_001);

        let first = fs::read_to_string(directory.0.join("panel-19700101.jsonl")).unwrap();
        let second = fs::read_to_string(directory.0.join("panel-19700102.jsonl")).unwrap();
        assert_eq!(first.lines().count(), 1);
        assert_eq!(second.lines().count(), 2);
        let times: Vec<u64> = first
            .lines()
            .chain(second.lines())
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["time_ms"]
                    .as_u64()
                    .unwrap()
            })
            .collect();
        assert_eq!(times, [86_399_999, 86_400_000, 86_400_001]);
        assert_eq!(
            fs::read_to_string(directory.0.join(log::COMPATIBILITY_FILE)).unwrap(),
            format!("{first}{second}")
        );
    }

    #[test]
    fn journal_uses_daily_file_without_compatibility_copy_by_default() {
        let directory = crate::settings::tests::Directory::new();
        let mut journal = Journal::at(&directory.0, false, 0);
        journal.record_at(
            Exit {
                code: Some(0),
                spawn_failed: false,
                decision: Decision::Finished,
                restarts: 0,
            },
            0,
        );
        let text = fs::read_to_string(directory.0.join("panel-19700101.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(!directory.0.join(log::COMPATIBILITY_FILE).exists());
    }

    #[test]
    fn journal_open_resolves_isolated_environment_without_changing_settings() {
        const PROBE: &str = "NIANLAI_JOURNAL_TEST_ROOT";
        if let Some(root) = std::env::var_os(PROBE) {
            let root = PathBuf::from(root);
            let settings = root.join("profile/Nianlai/settings.json");
            assert_eq!(crate::settings::settings_path(), settings);
            let before = fs::read(&settings).ok();
            let compatibility = std::env::var_os("NIANLAI_G0_DATA_DIR").is_some();
            let expected = if compatibility {
                root.join("test-data")
            } else {
                root.join("local/Nianlai/logs")
            };
            let mut journal = Journal::open();
            assert_eq!(journal.directory, expected);
            journal.record(Exit {
                code: Some(0),
                spawn_failed: false,
                decision: Decision::Finished,
                restarts: 0,
            });
            let rows: Vec<_> = fs::read_dir(&expected)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.file_name().unwrap() != log::COMPATIBILITY_FILE)
                .flat_map(|path| {
                    fs::read_to_string(path)
                        .unwrap()
                        .lines()
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .collect();
            assert_eq!(rows.len(), 1);
            let row: serde_json::Value = serde_json::from_str(&rows[0]).unwrap();
            assert_eq!(row["event"], "guardian_child_exit");
            assert_eq!(row["action"], "exit");
            assert_eq!(
                expected.join(log::COMPATIBILITY_FILE).exists(),
                compatibility
            );
            assert_eq!(fs::read(&settings).ok(), before);
            assert!(!root.join("local/Nianlai/data").exists());
            assert!(!root.join("test-data/history.db").exists());
            return;
        }

        // 环境只注入测试子进程，避免修改并行测试或日常样机的配置。
        for compatibility in [false, true] {
            let directory = crate::settings::tests::Directory::new();
            let mut child = Command::new(std::env::current_exe().unwrap());
            child.args([
                "--exact",
                "recovery::tests::journal_open_resolves_isolated_environment_without_changing_settings",
                "--nocapture",
            ]);
            child
                .env(PROBE, &directory.0)
                .env("APPDATA", directory.0.join("profile"))
                .env("LOCALAPPDATA", directory.0.join("local"))
                .env_remove("NIANLAI_G0_DATA_DIR")
                .env_remove("NIANLAI_HOTKEY")
                .env_remove("NIANLAI_G0_RESUSPEND_MIN");
            if compatibility {
                let settings = directory.0.join("profile/Nianlai/settings.json");
                fs::create_dir_all(settings.parent().unwrap()).unwrap();
                fs::write(settings, b"{malformed-private-settings").unwrap();
                child.env("NIANLAI_G0_DATA_DIR", directory.0.join("test-data"));
            }
            let output = child.output().unwrap();
            assert!(
                output.status.success(),
                "隔离守护日志测试失败：{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        }
    }

    #[test]
    fn journal_reopens_after_application_rotation_and_never_prunes() {
        let directory = crate::settings::tests::Directory::new();
        let old = directory.0.join("panel-19691231.jsonl");
        let database = directory.0.join("history.db");
        fs::write(&old, b"1234567890").unwrap();
        fs::write(&database, b"1234567890").unwrap();
        let mut journal = Journal::at(&directory.0, true, 0);
        let exit = Exit {
            code: Some(1),
            spawn_failed: false,
            decision: Decision::Restart { attempt: 1 },
            restarts: 1,
        };
        journal.record_at(exit, 0);
        let daily = directory.0.join("panel-19700101.jsonl");
        let compatibility = directory.0.join(log::COMPATIBILITY_FILE);
        let first = fs::read_to_string(&daily).unwrap();
        // 模拟应用轮转删除；守护者不能留着旧句柄，下一次退出必须写回新文件。
        fs::remove_file(&daily).unwrap();
        fs::remove_file(&compatibility).unwrap();
        journal.record_at(exit, 0);
        assert_eq!(fs::read_to_string(daily).unwrap(), first);
        assert_eq!(fs::read_to_string(compatibility).unwrap(), first);
        assert_eq!(fs::read(old).unwrap(), b"1234567890");
        assert_eq!(fs::read(database).unwrap(), b"1234567890");
    }

    #[test]
    fn journal_shares_daily_files_and_compatibility_with_application() {
        for compatibility in [false, true] {
            let directory = crate::settings::tests::Directory::new();
            let directories = crate::settings::Directories {
                settings_file: directory.0.join("settings.json"),
                data: directory.0.join("data"),
                logs: directory.0.join("logs"),
                data_source: "默认位置",
                test_data_dir: compatibility,
            };
            let logger = log::Logger::with_settings(&directories, 20).unwrap();
            logger.event("应用验证", json!({"time_ms":0}));
            logger.flush();
            let mut journal = Journal::at(&directories.logs, compatibility, 0);
            journal.record_at(
                Exit {
                    code: Some(0),
                    spawn_failed: false,
                    decision: Decision::Finished,
                    restarts: 0,
                },
                0,
            );
            let daily = fs::read_to_string(directories.logs.join("panel-19700101.jsonl")).unwrap();
            let rows: Vec<serde_json::Value> = daily
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0]["event"], "应用验证");
            assert_eq!(rows[1]["event"], "guardian_child_exit");
            let copy = directories.logs.join(log::COMPATIBILITY_FILE);
            if compatibility {
                assert_eq!(fs::read_to_string(copy).unwrap(), daily);
            } else {
                assert!(!copy.exists());
            }
            assert!(!directories.settings_file.exists());
            assert!(!directories.data.exists());
        }
    }
}
