//! 安装包的下载、sha256 校验、便携版 zip 的结构校验与原地替换（文件层面，不碰进程和界面）。
//!
//! 只有用户点了“立即更新”才会走到这里。用户数据（设置、历史库、日志）都在
//! `%APPDATA%\Nianlai` 和 `%LOCALAPPDATA%\Nianlai`，不在程序所在的文件夹，
//! 这里只处理程序文件夹里的文件，绝不碰用户数据。
//!
//! 便携版 zip 的校验与替换只在 Windows 上调用（macOS 走 mac.rs），其他平台只编译、由测试覆盖。
#![cfg_attr(not(windows), allow(dead_code))]

use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

/// 安装包大小上限（便携版约几十 MB，留足余量）。
pub const MAX_PACKAGE_BYTES: u64 = 400 * 1024 * 1024;
const MAX_ENTRIES: usize = 16;
const MAX_FILE_BYTES: u64 = 300 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 400 * 1024 * 1024;
/// 被替换的旧文件改名后缀；新进程启动时清理。
pub const OLD_SUFFIX: &str = ".old";
/// 新文件先以此后缀放进程序文件夹，再改名生效（同一卷内改名才是原子的）。
pub const NEW_SUFFIX: &str = ".new";
pub const EXE_NAME: &str = "nianlai.exe";

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|_| "读不到下载的文件".to_owned())?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| "读取下载的文件失败".to_owned())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 下载到 `dest`；`progress(已收, 总长)` 由调用方自己节流。
pub fn download(
    url: &str,
    relaxed: bool,
    dest: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<(), String> {
    let mut file = File::create(dest).map_err(|_| "没法在临时目录写文件".to_owned())?;
    let mut received = 0u64;
    let result = super::http::fetch(
        url,
        relaxed,
        std::time::Duration::from_secs(20 * 60),
        &mut |chunk, total| {
            received += chunk.len() as u64;
            if received > MAX_PACKAGE_BYTES || total.is_some_and(|t| t > MAX_PACKAGE_BYTES) {
                return Err("安装包比预期大得多，已放弃".into());
            }
            file.write_all(chunk)
                .map_err(|_| "磁盘写入失败，可能空间不足".to_owned())?;
            progress(received, total);
            Ok(())
        },
    );
    if result.is_ok() {
        file.sync_all().map_err(|_| "磁盘写入失败".to_owned())?;
    }
    result
}

/// 是否是 Windows x64 的 PE 程序（只看文件头）。
pub fn is_pe_x64(header: &[u8]) -> bool {
    if header.len() < 64 || &header[..2] != b"MZ" {
        return false;
    }
    let offset = u32::from_le_bytes([header[60], header[61], header[62], header[63]]) as usize;
    header
        .get(offset..offset + 6)
        .is_some_and(|pe| &pe[..4] == b"PE\0\0" && pe[4] == 0x64 && pe[5] == 0x86)
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && !name.ends_with('.')
        && !name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|', '\0'])
        && !name.chars().any(char::is_control)
        && !name.to_ascii_lowercase().ends_with(OLD_SUFFIX)
        && !name.to_ascii_lowercase().ends_with(NEW_SUFFIX)
}

/// 校验便携版 zip：只允许根目录下的普通文件、必须含 nianlai.exe 且是 x64 程序，
/// 并限制数量与大小。返回要放进程序文件夹的文件名。
pub fn validate_portable_zip(zip_path: &Path) -> Result<Vec<String>, String> {
    let bad = || "安装包内容不符合预期，已放弃".to_owned();
    let file = File::open(zip_path).map_err(|_| "读不到下载的文件".to_owned())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|_| bad())?;
    if archive.is_empty() || archive.len() > MAX_ENTRIES {
        return Err(bad());
    }
    let (mut names, mut total, mut has_exe) = (Vec::new(), 0u64, false);
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|_| bad())?;
        let name = entry.name().map_err(|_| bad())?.into_owned();
        if entry.is_dir() || !safe_name(&name) {
            return Err(bad());
        }
        if names
            .iter()
            .any(|seen: &String| seen.eq_ignore_ascii_case(&name))
        {
            return Err(bad());
        }
        total = total.saturating_add(entry.size());
        if entry.size() > MAX_FILE_BYTES || total > MAX_TOTAL_BYTES {
            return Err(bad());
        }
        if name.eq_ignore_ascii_case(EXE_NAME) {
            let mut header = vec![0u8; 1024];
            let read = entry.read(&mut header).map_err(|_| bad())?;
            if !is_pe_x64(&header[..read]) {
                return Err("安装包里的程序不是 Windows x64 程序，已放弃".into());
            }
            has_exe = true;
        }
        names.push(name);
    }
    if !has_exe {
        return Err("安装包里没有 nianlai.exe，已放弃".into());
    }
    Ok(names)
}

/// 把已校验的 zip 解压成 `<dir>/<name><NEW_SUFFIX>`（与目标同一文件夹，之后可原子改名）。
pub fn extract_as_new(zip_path: &Path, names: &[String], dir: &Path) -> Result<(), String> {
    fn fail<E>(_: E) -> String {
        "解压安装包失败".to_owned()
    }
    let mut archive = zip::ZipArchive::new(File::open(zip_path).map_err(fail)?).map_err(fail)?;
    for name in names {
        let entry = archive.by_name(name).map_err(fail)?;
        let target = dir.join(format!("{name}{NEW_SUFFIX}"));
        let mut out =
            File::create(&target).map_err(|_| "程序所在文件夹不可写，没法更新".to_owned())?;
        // 以实际读到的字节为准，不信任声明的大小。
        let copied = std::io::copy(&mut entry.take(MAX_FILE_BYTES + 1), &mut out).map_err(fail)?;
        if copied > MAX_FILE_BYTES {
            return Err("安装包内容不符合预期，已放弃".into());
        }
        out.sync_all().map_err(fail)?;
    }
    Ok(())
}

fn with_suffix(dir: &Path, name: &str, suffix: &str) -> PathBuf {
    dir.join(format!("{name}{suffix}"))
}

/// 原地替换：对每个文件，现有的 → `<name>.old`，`<name>.new` → `<name>`。
/// 运行中的 exe 在 Windows 上可以改名但不能删除，所以用改名腾位置。
/// 中途失败会把已经换过的全部还原，旧版本保持可用。
pub fn swap_in(dir: &Path, names: &[String]) -> Result<(), String> {
    let mut done: Vec<&String> = Vec::new();
    for name in names {
        let (current, old, new) = (
            dir.join(name),
            with_suffix(dir, name, OLD_SUFFIX),
            with_suffix(dir, name, NEW_SUFFIX),
        );
        let step = (|| -> std::io::Result<()> {
            if old.exists() {
                fs::remove_file(&old)?;
            }
            let had_current = current.exists();
            if had_current {
                fs::rename(&current, &old)?;
            }
            if let Err(error) = fs::rename(&new, &current) {
                if had_current {
                    let _ = fs::rename(&old, &current);
                }
                return Err(error);
            }
            Ok(())
        })();
        if step.is_err() {
            rollback(dir, &done);
            discard_new(dir, names);
            return Err("替换程序文件失败，已保留旧版本".into());
        }
        done.push(name);
    }
    Ok(())
}

/// 还原 `swap_in` 已换过的文件（新进程没起来时也用它）。
pub fn rollback(dir: &Path, names: &[&String]) {
    for name in names.iter().rev() {
        let (current, old) = (dir.join(name), with_suffix(dir, name, OLD_SUFFIX));
        if old.exists() {
            let _ = fs::remove_file(&current);
            let _ = fs::rename(&old, &current);
        }
    }
}

pub fn discard_new(dir: &Path, names: &[String]) {
    for name in names {
        let _ = fs::remove_file(with_suffix(dir, name, NEW_SUFFIX));
    }
}

/// 清理上次更新留下的 `.old` / `.new`（只处理程序自己的文件名）。能删多少删多少。
/// 旧守护进程可能还没退出、`.old` 暂时被占用，调用方会稍后重试。返回仍然残留的数量。
pub fn clean_leftovers(dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut remaining = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        let ours = [OLD_SUFFIX, NEW_SUFFIX]
            .iter()
            .any(|suffix| name.strip_suffix(suffix).is_some_and(is_known_file));
        if ours && fs::remove_file(entry.path()).is_err() {
            remaining += 1;
        }
    }
    remaining
}

fn is_known_file(base: &str) -> bool {
    base == EXE_NAME || base == "使用说明.txt"
}

/// 程序文件夹能不能写（创建并删除一个探针文件）。
pub fn directory_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".nianlai-write-test-{}", std::process::id()));
    let ok = File::create(&probe).is_ok();
    let _ = fs::remove_file(&probe);
    ok
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("nianlai-update-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub fn fake_exe() -> Vec<u8> {
        let mut bytes = vec![0u8; 256];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60] = 128;
        bytes[128..132].copy_from_slice(b"PE\0\0");
        bytes[132] = 0x64;
        bytes[133] = 0x86;
        bytes
    }

    pub fn make_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let mut writer = zip::ZipWriter::new(File::create(path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        for (name, data) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap();
    }

    #[test]
    fn sha256_matches_known_vector() {
        let dir = temp_dir("sha");
        let file = dir.join("a.txt");
        fs::write(&file, b"abc").unwrap();
        assert_eq!(
            sha256_file(&file).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(sha256_file(&dir.join("missing")).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pe_header_check_requires_x64() {
        assert!(is_pe_x64(&fake_exe()));
        let mut arm = fake_exe();
        arm[132] = 0x64;
        arm[133] = 0xAA;
        assert!(!is_pe_x64(&arm));
        assert!(!is_pe_x64(b"MZ"));
        assert!(!is_pe_x64(&vec![0u8; 300]));
    }

    #[test]
    fn accepts_the_real_portable_layout() {
        let dir = temp_dir("zip-ok");
        let zip = dir.join("p.zip");
        make_zip(&zip, &[(EXE_NAME, &fake_exe()), ("使用说明.txt", b"hi")]);
        assert_eq!(
            validate_portable_zip(&zip).unwrap(),
            [EXE_NAME.to_owned(), "使用说明.txt".to_owned()]
        );
        fs::remove_dir_all(dir).unwrap();
    }

    /// 一个测试用例：名字和 zip 里的（文件名，内容）。
    type Case<'a> = (&'a str, Vec<(&'a str, Vec<u8>)>);

    #[test]
    fn rejects_unsafe_or_incomplete_packages() {
        let dir = temp_dir("zip-bad");
        let cases: Vec<Case> = vec![
            ("no-exe", vec![("readme.txt", b"x".to_vec())]),
            ("not-pe", vec![(EXE_NAME, b"MZ nope".to_vec())]),
            (
                "subdir",
                vec![(EXE_NAME, fake_exe()), ("sub/a.dll", b"x".to_vec())],
            ),
            (
                "traversal",
                vec![(EXE_NAME, fake_exe()), ("../evil.exe", b"x".to_vec())],
            ),
            (
                "backslash",
                vec![(EXE_NAME, fake_exe()), ("a\\b.dll", b"x".to_vec())],
            ),
            (
                "drive",
                vec![(EXE_NAME, fake_exe()), ("C:evil.dll", b"x".to_vec())],
            ),
            (
                "dot",
                vec![(EXE_NAME, fake_exe()), (".hidden", b"x".to_vec())],
            ),
            (
                "old",
                vec![(EXE_NAME, fake_exe()), ("a.old", b"x".to_vec())],
            ),
            ("empty", vec![]),
        ];
        for (name, entries) in cases {
            let zip = dir.join(format!("{name}.zip"));
            let borrowed: Vec<(&str, &[u8])> =
                entries.iter().map(|(n, d)| (*n, d.as_slice())).collect();
            make_zip(&zip, &borrowed);
            assert!(validate_portable_zip(&zip).is_err(), "{name}");
        }
        let garbage = dir.join("garbage.zip");
        fs::write(&garbage, b"not a zip").unwrap();
        assert!(validate_portable_zip(&garbage).is_err());
        let many: Vec<(String, Vec<u8>)> = (0..=MAX_ENTRIES)
            .map(|n| (format!("f{n}.txt"), vec![1]))
            .collect();
        let many_ref: Vec<(&str, &[u8])> = many
            .iter()
            .map(|(n, d)| (n.as_str(), d.as_slice()))
            .collect();
        let zip = dir.join("many.zip");
        make_zip(&zip, &many_ref);
        assert!(validate_portable_zip(&zip).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    fn staged(dir: &Path) -> (PathBuf, Vec<String>) {
        let install = dir.join("app");
        fs::create_dir_all(&install).unwrap();
        fs::write(install.join(EXE_NAME), b"old exe").unwrap();
        fs::write(install.join("使用说明.txt"), b"old text").unwrap();
        let zip = dir.join("p.zip");
        make_zip(
            &zip,
            &[(EXE_NAME, &fake_exe()), ("使用说明.txt", b"new text")],
        );
        let names = validate_portable_zip(&zip).unwrap();
        extract_as_new(&zip, &names, &install).unwrap();
        (install, names)
    }

    #[test]
    fn swap_replaces_files_keeps_old_copies_and_leaves_other_files_alone() {
        let dir = temp_dir("swap");
        let (install, names) = staged(&dir);
        fs::write(install.join("user-notes.txt"), b"mine").unwrap();
        swap_in(&install, &names).unwrap();
        assert_eq!(fs::read(install.join(EXE_NAME)).unwrap(), fake_exe());
        assert_eq!(
            fs::read(install.join("nianlai.exe.old")).unwrap(),
            b"old exe"
        );
        assert_eq!(fs::read(install.join("使用说明.txt")).unwrap(), b"new text");
        assert_eq!(fs::read(install.join("user-notes.txt")).unwrap(), b"mine");
        assert!(!install.join("nianlai.exe.new").exists());
        assert_eq!(clean_leftovers(&install), 0);
        assert!(!install.join("nianlai.exe.old").exists());
        assert!(install.join("user-notes.txt").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn swap_failure_rolls_back_to_the_old_version() {
        let dir = temp_dir("rollback");
        let (install, names) = staged(&dir);
        // 第二个文件的新版本丢失，模拟中途失败。
        fs::remove_file(install.join("使用说明.txt.new")).unwrap();
        assert!(swap_in(&install, &names).is_err());
        assert_eq!(fs::read(install.join(EXE_NAME)).unwrap(), b"old exe");
        assert_eq!(fs::read(install.join("使用说明.txt")).unwrap(), b"old text");
        assert!(!install.join("nianlai.exe.old").exists());
        assert!(!install.join("nianlai.exe.new").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn explicit_rollback_restores_after_a_failed_launch() {
        let dir = temp_dir("rollback2");
        let (install, names) = staged(&dir);
        swap_in(&install, &names).unwrap();
        rollback(&install, &names.iter().collect::<Vec<_>>());
        assert_eq!(fs::read(install.join(EXE_NAME)).unwrap(), b"old exe");
        assert_eq!(fs::read(install.join("使用说明.txt")).unwrap(), b"old text");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn clean_leftovers_only_touches_known_names() {
        let dir = temp_dir("clean");
        for name in [
            "nianlai.exe.old",
            "NIANLAI.EXE.NEW",
            "other.old",
            "keep.txt",
        ] {
            fs::write(dir.join(name), b"x").unwrap();
        }
        clean_leftovers(&dir);
        assert!(!dir.join("nianlai.exe.old").exists());
        assert!(!dir.join("NIANLAI.EXE.NEW").exists());
        assert!(dir.join("other.old").exists());
        assert!(dir.join("keep.txt").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn writable_probe_reports_missing_directories_as_not_writable() {
        let dir = temp_dir("writable");
        assert!(directory_writable(&dir));
        assert!(!directory_writable(&dir.join("nope")));
        fs::remove_dir_all(dir).unwrap();
    }
}
