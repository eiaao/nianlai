//! macOS 原地更新：挂载 DMG，把 .app 拷到临时位置并校验签名，再原子替换当前 .app。
//! 任何一步失败都保留旧 .app。

use super::package::directory_writable;
use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const NOT_WRITABLE: &str = "拈来所在的文件夹没有写入权限（比如还在从磁盘映像里运行），没法自动更新。请把拈来拖进“应用程序”，或到下载页手动更新";

/// 当前运行的 .app 路径：`…/Nianlai.app/Contents/MacOS/<可执行文件>` 往上三级。
pub fn bundle_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let app = exe.ancestors().nth(3)?.to_path_buf();
    (app.extension() == Some(OsStr::new("app"))).then_some(app)
}

/// 是否有条件原地更新；没有时返回给用户看的原因。
pub fn install_block() -> Option<String> {
    let Some(app) = bundle_path() else {
        return Some("没找到拈来的应用位置，没法自动更新".into());
    };
    match app.parent() {
        Some(parent) if directory_writable(parent) => None,
        _ => Some(NOT_WRITABLE.into()),
    }
}

fn run(program: &str, args: &[&OsStr]) -> Result<(), String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|_| format!("没法运行 {program}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("{program} 没成功"))
    }
}

fn plist_value(app: &Path, key: &str) -> Option<String> {
    let output = Command::new("/usr/libexec/PlistBuddy")
        .arg("-c")
        .arg(format!("Print :{key}"))
        .arg(app.join("Contents/Info.plist"))
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

struct Mounted<'a>(&'a Path);
impl Drop for Mounted<'_> {
    fn drop(&mut self) {
        let _ = run(
            "/usr/bin/hdiutil",
            &["detach".as_ref(), self.0.as_os_str(), "-force".as_ref()],
        );
    }
}

pub fn install(dmg: &Path, work: &Path) -> Result<(), String> {
    let current = bundle_path().ok_or("没找到拈来的应用位置")?;
    let parent = current
        .parent()
        .ok_or("没找到拈来的应用位置")?
        .to_path_buf();
    if !directory_writable(&parent) {
        return Err(NOT_WRITABLE.into());
    }
    let name = current
        .file_name()
        .ok_or("没找到拈来的应用位置")?
        .to_string_lossy()
        .into_owned();
    let pid = std::process::id();
    let staged = parent.join(format!(".update-{pid}-{name}"));
    let backup = parent.join(format!(".old-{pid}-{name}"));
    let _ = fs::remove_dir_all(&staged);

    let mount = work.join("mnt");
    fs::create_dir_all(&mount).map_err(|_| "没法创建临时目录".to_owned())?;
    run(
        "/usr/bin/hdiutil",
        &[
            "attach".as_ref(),
            "-nobrowse".as_ref(),
            "-readonly".as_ref(),
            "-noautoopen".as_ref(),
            "-mountpoint".as_ref(),
            mount.as_os_str(),
            dmg.as_os_str(),
        ],
    )
    .map_err(|_| "没能打开下载的安装映像".to_owned())?;
    let copied = {
        let _mounted = Mounted(&mount);
        let source = find_app(&mount)?;
        run("/usr/bin/ditto", &[source.as_os_str(), staged.as_os_str()])
            .map_err(|_| "没能把新版本拷出来".to_owned())
    };
    let verified = copied.and_then(|()| verify(&staged, &current));
    if let Err(error) = verified {
        let _ = fs::remove_dir_all(&staged);
        return Err(error);
    }

    // 先移走旧的再移入新的，失败就还原。
    fs::rename(&current, &backup).map_err(|_| {
        let _ = fs::remove_dir_all(&staged);
        "没能替换旧版本，已保留原样".to_owned()
    })?;
    if fs::rename(&staged, &current).is_err() {
        let _ = fs::rename(&backup, &current);
        let _ = fs::remove_dir_all(&staged);
        return Err("没能放入新版本，已还原旧版本".into());
    }
    let _ = fs::remove_dir_all(&backup);
    Ok(())
}

fn find_app(mount: &Path) -> Result<PathBuf, String> {
    let mut apps = fs::read_dir(mount)
        .map_err(|_| "安装映像里读不到内容".to_owned())?
        .flatten()
        .filter(|entry| {
            entry.path().extension() == Some(OsStr::new("app"))
                && entry.file_type().is_ok_and(|kind| kind.is_dir())
        })
        .map(|entry| entry.path());
    match (apps.next(), apps.next()) {
        (Some(app), None) => Ok(app),
        _ => Err("安装映像里的内容不符合预期，已放弃".into()),
    }
}

fn verify(staged: &Path, current: &Path) -> Result<(), String> {
    run(
        "/usr/bin/codesign",
        &[
            "--verify".as_ref(),
            "--deep".as_ref(),
            "--strict".as_ref(),
            staged.as_os_str(),
        ],
    )
    .map_err(|_| "新版本的签名校验没通过，已放弃".to_owned())?;
    let (new_id, old_id) = (
        plist_value(staged, "CFBundleIdentifier"),
        plist_value(current, "CFBundleIdentifier"),
    );
    if new_id.is_none() || new_id != old_id {
        return Err("新版本的应用标识与当前不一致，已放弃".into());
    }
    Ok(())
}

/// 用新的 .app 启动一个新实例，并让它等当前进程退出后再初始化。
pub fn relaunch() -> Result<(), String> {
    let app = bundle_path().ok_or("没找到拈来的应用位置")?;
    Command::new("/usr/bin/open")
        .arg("-n")
        .arg(&app)
        .arg("--args")
        .arg(super::process::WAIT_ARG)
        .arg(std::process::id().to_string())
        .spawn()
        .map(drop)
        .map_err(|_| "新版本已装好，但没能自动打开，请手动打开拈来".to_owned())
}
