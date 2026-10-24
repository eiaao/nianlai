use super::{
    helper, hwnd,
    input::Input,
    observe::{self, Window},
    process::{self, Child, Job},
    safety::Control,
    uia::{Element, Uia},
};
use crate::{
    preflight::expand_env,
    protocol::{Digest, TitleState},
    scenario::{Kind, Scenario, Step},
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::{LPARAM, POINT, WPARAM},
    System::Threading::{AttachThreadInput, GetCurrentThreadId},
    UI::WindowsAndMessaging::*,
};

pub struct Target {
    pub scenario: Scenario,
    pub root: Window,
    pub active: isize,
    pub token: String,
    pub dir: PathBuf,
    pub file: PathBuf,
    pub child: Option<Child>,
    pub delegated_child: Option<Child>,
    pub element: Option<Element>,
    pub before_ext: String,
    pub original_file: String,
    pub owns_delegated_pid: bool,
}
#[derive(Clone)]
pub struct Snapshot {
    pub title: Option<TitleState>,
    pub element: Option<Element>,
    pub digest: Digest,
    pub text: Option<String>,
    pub address: Option<Digest>,
    pub fg: isize,
    pub focus: isize,
    pub alive: bool,
    pub observed: bool,
}
impl Target {
    pub fn launch(
        s: &Scenario,
        base: &Path,
        token: &str,
        job: &Job,
        control: &Arc<Control>,
    ) -> Result<Self, String> {
        control.check()?;
        let old_windows: BTreeSet<_> = observe::windows()?.iter().map(|w| w.raw).collect();
        let old_pids: BTreeSet<_> = observe::processes()?.iter().map(|p| p.pid).collect();
        if s.target.exclusive
            && observe::processes()?
                .iter()
                .any(|p| p.name.eq_ignore_ascii_case(&s.target.process))
        {
            return Err("独占目标已有同名进程，跳过".into());
        }
        let dir = base.join(format!("{}-{token}", s.id));
        std::fs::create_dir(&dir).map_err(|e| e.to_string())?;
        let original_file = format!("NLFM-{token}.txt");
        let file = dir.join(&original_file);
        if matches!(s.target.kind, Kind::File | Kind::Explorer) {
            std::fs::write(&file, b"").map_err(|e| e.to_string())?;
        }
        let page = dir.join("page.html");
        let profile = dir.join("profile");
        if matches!(s.target.kind, Kind::Web | Kind::Address | Kind::Bookmark) {
            std::fs::write(&page, crate::scenario::PAGE).map_err(|e| e.to_string())?;
        }
        let url = format!("{}?t={token}&field={}", file_url(&page), s.target.field);
        let mut child = None;
        let mut delegated_child = None;
        let delegated = s.target.kind == Kind::Explorer
            || (s.target.kind == Kind::File
                && s.target.process.eq_ignore_ascii_case("notepad.exe"));
        let started = if s.target.kind == Kind::Helper {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            job.spawn(
                &exe,
                &["helper-window".into(), "--token".into(), token.into()],
                &[("NLFM_HELPER_TOKEN".into(), Some(token.into()))],
            )?
        } else {
            let exe = expand_env(s.target.program.as_deref().ok_or("缺少程序")?);
            let args = s
                .target
                .args
                .iter()
                .map(|a| {
                    a.replace("{file}", &file.to_string_lossy())
                        .replace("{dir}", &dir.to_string_lossy())
                        .replace("{url}", &url)
                        .replace("{profile}", &profile.to_string_lossy())
                        .replace("{token}", token)
                })
                .collect::<Vec<_>>();
            control.check()?;
            if delegated {
                process::delegated(&exe, &args)?
            } else {
                job.spawn(&exe, &args, &[])?
            }
        };
        if delegated {
            delegated_child = Some(started);
        } else {
            child = Some(started);
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        let root = loop {
            control.check()?;
            let candidate = observe::windows()?.into_iter().find(|w| {
                !old_windows.contains(&w.raw)
                    && w.class == s.target.window_class
                    && w.process.eq_ignore_ascii_case(&s.target.process)
                    && (s.target.kind == Kind::Explorer
                        || (!old_pids.contains(&w.pid) && (delegated || job.owns(w.pid))))
                    && observe::owned_title(w.raw).contains(token)
            });
            if let Some(w) = candidate {
                break w;
            }
            if Instant::now() > deadline {
                // 不认领旧标签页；无标记窗口也不能发送关闭或键盘消息。
                return Err("未认出新窗口和运行标记；可能转交到已有标签页，已放弃该场景".into());
            }
            control.wait(50)?;
        };
        let owns_delegated_pid =
            delegated && s.target.kind == Kind::File && !old_pids.contains(&root.pid);
        if owns_delegated_pid {
            delegated_child = Some(process::claim_new_process(root.pid)?);
        }
        Ok(Self {
            scenario: s.clone(),
            active: root.raw,
            root,
            token: token.into(),
            dir,
            file,
            child,
            delegated_child,
            element: None,
            before_ext: String::new(),
            original_file,
            owns_delegated_pid,
        })
    }
    pub fn owns(&self, raw: isize) -> bool {
        raw == self.root.raw || (raw == self.active && observe::pid(raw) == self.root.pid)
    }
    pub fn ensure(&self) -> Result<(), String> {
        if !observe::visible(self.root.raw) || observe::pid(self.root.raw) != self.root.pid {
            return Err("已认领目标窗口消失或句柄被复用".into());
        }
        if !observe::owned_title(self.root.raw).contains(&self.token) {
            return Err("目标标题不再包含本次标记".into());
        }
        Ok(())
    }
    pub fn focus(&mut self, input: &mut Input, uia: &Uia) -> Result<(), String> {
        input.control.check()?;
        self.ensure()?;
        input.own(self.root.raw, self.root.pid);
        if self.element.is_none() && self.scenario.target.kind == Kind::Web {
            self.element =
                Some(uia.find(self.root.raw, Some(&self.scenario.target.field), None, None)?);
        }
        if let Some(e) = &self.element {
            let _ = uia.focus(e.id);
        }
        input.control.check()?;
        if observe::foreground() != self.active {
            unsafe {
                let current = GetCurrentThreadId();
                let target = GetWindowThreadProcessId(hwnd(self.active), None);
                let attached = AttachThreadInput(current, target, true).as_bool();
                let _ = SetForegroundWindow(hwnd(self.active));
                if attached {
                    let _ = AttachThreadInput(current, target, false);
                }
            }
        }
        input.check(&[self.active])?;
        if let Some(e) = &self.element
            && uia
                .focused(self.active, &self.scenario.target.control)
                .is_err()
        {
            let [x1, y1, x2, y2] = e.rect;
            if x2 > x1 && y2 > y1 {
                input.mouse(
                    POINT {
                        x: x1 + (x2 - x1) / 2,
                        y: y1 + (y2 - y1) / 2,
                    },
                    true,
                    &[self.active],
                )?;
            }
        }
        Ok(())
    }
    pub fn position(&mut self, input: &mut Input) -> Result<(), String> {
        let s = &self.scenario;
        if s.target.second_monitor {
            let monitors = observe::monitors();
            let monitor = monitors
                .iter()
                .find(|m| !m.primary)
                .ok_or("没有第二块屏幕")?;
            let r = observe::rect(self.root.raw).ok_or("目标窗口矩形不可读")?;
            let [x1, y1, x2, y2] = monitor.rect;
            input.control.check()?;
            unsafe {
                SetWindowPos(
                    hwnd(self.root.raw),
                    None,
                    x1 + 30,
                    y1 + 30,
                    (r.right - r.left).min(x2 - x1 - 60),
                    (r.bottom - r.top).min(y2 - y1 - 60),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                )
                .map_err(|e| e.to_string())?;
            }
            input.check(&[self.active])?;
            input.mouse(
                POINT {
                    x: x1 + 80,
                    y: y1 + 80,
                },
                false,
                &[self.active],
            )?;
            if observe::monitor_window(self.root.raw) != monitor.id
                || observe::monitor_mouse() != monitor.id
            {
                return Err("第二屏定位未成功".into());
            }
        }
        if s.target.fullscreen {
            let r = observe::rect(self.root.raw).ok_or("全屏窗口矩形不可读")?;
            let m = observe::monitors()
                .into_iter()
                .find(|m| m.id == observe::monitor_window(self.root.raw))
                .ok_or("显示器不可读")?;
            if [
                r.left - m.rect[0],
                r.top - m.rect[1],
                r.right - m.rect[2],
                r.bottom - m.rect[3],
            ]
            .iter()
            .any(|n| n.abs() > 2)
            {
                return Err("浏览器没有铺满所在显示器".into());
            }
        }
        Ok(())
    }
    pub fn prepare(&mut self, input: &mut Input, uia: &Uia) -> Result<Snapshot, String> {
        self.active = self.root.raw;
        self.before_ext.clear();
        self.focus(input, uia)?;
        match self.scenario.target.kind {
            Kind::Address => input.named("Ctrl+L", &[self.root.raw])?,
            Kind::Bookmark => {
                let old: BTreeSet<_> = observe::windows()?.iter().map(|w| w.raw).collect();
                input.named("Ctrl+D", &[self.root.raw])?;
                input.control.wait(200)?;
                let fg = observe::foreground();
                if fg != self.root.raw && !old.contains(&fg) && observe::pid(fg) == self.root.pid {
                    self.active = fg;
                    input.own(fg, self.root.pid);
                } else {
                    return Err("书签气泡没有成为新的前台顶层窗口；不猜测目标".into());
                }
            }
            Kind::Explorer => {
                // 先用本次唯一文件名找到文件项，避免 F2 作用于其他文件。
                let stem = self.file.file_stem().unwrap_or_default().to_string_lossy();
                let item = uia
                    .find(self.root.raw, Some(&self.original_file), None, None)
                    .or_else(|_| uia.find(self.root.raw, Some(&stem), None, None))?;
                uia.focus(item.id)?;
                let focused = uia.focused(self.root.raw, &Default::default())?;
                if !uia.same(item.id, focused.id) {
                    return Err("重命名前焦点不是本次测试文件".into());
                }
                input.named("F2", &[self.root.raw])?;
            }
            _ => {}
        }
        input.control.wait(100)?;
        self.element = Some(uia.focused(self.active, &self.scenario.target.control)?);
        let steps = self.scenario.prepare.clone();
        self.steps(&steps, input, uia)?;
        input.control.wait(100)?;
        let snapshot = self.snapshot(uia, None)?;
        self.element = snapshot.element.clone();
        if !snapshot.observed || snapshot.element.is_none() || snapshot.focus == 0 {
            return Err("准备阶段没有取得可靠的控件和文本证据".into());
        }
        if matches!(
            self.scenario.target.kind,
            Kind::Web | Kind::Address | Kind::Bookmark
        ) && (snapshot.title.is_none() || snapshot.address.is_none())
        {
            return Err("网页状态或地址栏读不到，无法排除误粘到其他位置".into());
        }
        if self.scenario.target.kind == Kind::Web
            && snapshot.title.as_ref().is_none_or(|t| {
                t.fields.get("active") != Some(&self.scenario.target.field)
                    || t.number("focus").ok() != Some(1)
            })
        {
            return Err("网页焦点不是场景指定的字段".into());
        }
        if self.scenario.target.kind == Kind::Explorer {
            let e = snapshot.element.as_ref().ok_or("重命名框不可读")?;
            let name = e.text.as_deref().ok_or("重命名框文本不可读")?;
            // 原生 EDIT 有时只提供 Value；此时只根据自己的精确文件名和 F2 规则判断。
            self.before_ext =
                crate::protocol::rename_suffix(name, e.selection.as_deref(), &self.original_file)?;
        }
        input.check(&[self.active])?;
        Ok(snapshot)
    }
    pub fn steps(&mut self, steps: &[Step], input: &mut Input, uia: &Uia) -> Result<(), String> {
        for step in steps {
            match step {
                Step::Focus => self.focus(input, uia)?,
                Step::Clear => {
                    input.check(&[self.active])?;
                    if self.scenario.target.kind == Kind::Helper {
                        unsafe {
                            if SendMessageTimeoutW(
                                hwnd(self.root.raw),
                                helper::CLEAR,
                                WPARAM(0),
                                LPARAM(0),
                                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                                300,
                                None,
                            )
                            .0 == 0
                            {
                                return Err("基准窗口清空超时".into());
                            }
                        }
                    } else {
                        input.named("Ctrl+A", &[self.active])?;
                        input.named("Delete", &[self.active])?;
                    }
                }
                Step::Key { chord } => {
                    if chord == "Esc" {
                        let now = uia.focused(self.active, &self.scenario.target.control)?;
                        if let Some(original) = &self.element
                            && !uia.same(original.id, now.id)
                        {
                            return Err("清理 Esc 前输入控件已经变化，不发送 Esc".into());
                        }
                        if !matches!(self.scenario.target.kind, Kind::Bookmark | Kind::Explorer) {
                            return Err("清理 Esc 仅允许仍存在的书签/重命名控件".into());
                        }
                    }
                    input.named(chord, &[self.active])?;
                }
                Step::Wait { ms } => input.control.wait(*ms)?,
            }
        }
        Ok(())
    }
    pub fn snapshot(&self, uia: &Uia, before: Option<&Snapshot>) -> Result<Snapshot, String> {
        let browser = matches!(
            self.scenario.target.kind,
            Kind::Web | Kind::Address | Kind::Bookmark
        );
        let protocol = browser || self.scenario.target.kind == Kind::Helper;
        let title = if protocol {
            TitleState::parse(&observe::owned_title(self.root.raw), &self.token)
                .ok()
                .filter(|t| t.complete(browser))
        } else {
            None
        };
        let alive = observe::visible(self.active);
        let focused = if alive {
            uia.focused(self.active, &self.scenario.target.control).ok()
        } else {
            None
        };
        let element = if let Some(old) = before.and_then(|b| b.element.as_ref()) {
            uia.read(old.id).ok()
        } else {
            focused.clone()
        };
        let text = element.as_ref().and_then(|e| e.text.clone());
        let title_digest = title
            .as_ref()
            .and_then(|t| match self.scenario.target.kind {
                Kind::Helper => t.digest("").ok(),
                Kind::Web => t.digest(&self.scenario.target.field).ok(),
                _ => None,
            });
        let digest = title_digest
            .or_else(|| text.as_ref().map(|s| Digest::of(s)))
            .or_else(|| before.map(|b| b.digest))
            .unwrap_or_default();
        let address = if browser {
            let default = crate::scenario::Control {
                class: Some("OmniboxViewViews".into()),
                control_type: Some(50004),
            };
            let spec = self
                .scenario
                .target
                .address_control
                .as_ref()
                .unwrap_or(&default);
            uia.find(
                self.root.raw,
                None,
                spec.class.as_deref(),
                spec.control_type,
            )
            .ok()
            .and_then(|e| e.text)
            .map(|s| Digest::of(&s))
        } else {
            None
        };
        let mut observed = if matches!(self.scenario.target.kind, Kind::Bookmark) && !alive {
            title.is_some() && address.is_some()
        } else {
            title_digest.is_some() || text.is_some()
        };
        if browser {
            observed &= title.is_some() && address.is_some();
        }
        if self.scenario.target.kind == Kind::Helper {
            observed &= title.is_some();
        }
        Ok(Snapshot {
            title,
            element: focused,
            digest,
            text,
            address,
            fg: observe::foreground(),
            focus: observe::focus(self.active),
            alive,
            observed,
        })
    }
    pub fn disk_changed(&self) -> bool {
        self.scenario.target.kind == Kind::Explorer && !self.file.is_file()
    }
    pub fn repair_filename(&self) -> Result<(), String> {
        if !self.disk_changed() {
            return Ok(());
        }
        let files = std::fs::read_dir(&self.dir)
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .filter(|e| e.path().is_file())
            .collect::<Vec<_>>();
        if files.len() != 1 {
            return Err("重命名后目录内容不唯一，无法安全恢复".into());
        }
        // 两个路径都在本次专属目录内，不覆盖已存在的文件。
        if files[0].path().parent() != Some(self.dir.as_path()) || self.file.exists() {
            return Err("文件恢复边界检查失败".into());
        }
        std::fs::rename(files[0].path(), &self.file).map_err(|e| e.to_string())
    }
    pub fn cleanup_trial(&mut self, input: &mut Input, uia: &Uia) -> Result<(), String> {
        let steps = self.scenario.cleanup.clone();
        let result =
            if !observe::visible(self.active) && self.scenario.target.kind == Kind::Bookmark {
                Ok(())
            } else {
                self.steps(&steps, input, uia)
            };
        self.active = self.root.raw;
        self.element = None;
        self.repair_filename()?;
        result
    }
    pub fn close(&mut self) {
        // 中止后不再发保存键。仅关闭已认领窗口；必要时只终止已确认的新记事本 PID。
        if self.ensure().is_ok() {
            unsafe {
                let _ = PostMessageW(Some(hwnd(self.root.raw)), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        if self.owns_delegated_pid
            && let Some(child) = &self.delegated_child
            && !child.wait(Duration::from_millis(500))
        {
            let _ = child.kill();
        }
        if let Some(child) = &self.child
            && !child.wait(Duration::from_millis(300))
        {
            let _ = child.kill();
        }
    }
}
impl Drop for Target {
    fn drop(&mut self) {
        self.close();
    }
}
pub fn file_url(path: &Path) -> String {
    let path = path.to_string_lossy();
    let path = path.strip_prefix(r"\\?\").unwrap_or(&path);
    let raw = path.replace('\\', "/");
    let mut result = if raw.starts_with("//") {
        "file:"
    } else {
        "file:///"
    }
    .to_owned();
    for byte in raw.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-_.~:".contains(&byte) {
            result.push(byte as char);
        } else {
            result.push_str(&format!("%{byte:02X}"));
        }
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_url_encoding() {
        assert_eq!(
            file_url(Path::new("C:\\a b\\x#%.html")),
            "file:///C:/a%20b/x%23%25.html"
        );
        assert_eq!(
            file_url(Path::new(r"\\?\C:\a b\x.html")),
            "file:///C:/a%20b/x.html"
        );
    }
}
