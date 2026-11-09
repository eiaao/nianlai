// 文件失效检查和存储管理用到的磁盘信息（M2-3）。
use std::path::Path;
use windows::{
    Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW},
    core::HSTRING,
};

const DRIVE_REMOTE: u32 = 4;

/// 网络位置：UNC 路径（\\server\share）或映射到网络的盘符。这类路径可能长时间无响应，检查时要限时。
pub fn is_remote(path: &str) -> bool {
    if path.starts_with("\\\\") || path.starts_with("//") {
        return true;
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        let root = HSTRING::from(format!("{}:\\", bytes[0] as char));
        return unsafe { GetDriveTypeW(&root) } == DRIVE_REMOTE;
    }
    false
}

/// 目录所在磁盘对当前用户可用的剩余空间。
pub fn free_bytes(directory: &Path) -> Option<u64> {
    let mut available = 0u64;
    unsafe {
        GetDiskFreeSpaceExW(
            &HSTRING::from(directory.as_os_str()),
            Some(&mut available),
            None,
            None,
        )
    }
    .ok()?;
    Some(available)
}
