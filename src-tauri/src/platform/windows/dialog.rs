use windows::{
    Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MessageBoxW,
    },
    core::HSTRING,
};

// 系统消息框，阻塞到用户关掉。守护者没有自己的窗口，只有这一种提示方式。
pub fn error(text: &str) {
    unsafe {
        let _ = MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from("拈来"),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
        );
    }
}

/// 系统“另存为 / 打开”对话框，只认 `.nianlai-backup`（M2-4）。
/// 对话框要 STA 线程，所以另起一个线程跑，调用方等它结束；`owner` 是所属窗口句柄（0 表示没有）。
/// 取消返回 `Ok(None)`。
pub fn choose_backup(
    owner: usize,
    save: bool,
    default_name: &str,
) -> Result<Option<String>, String> {
    let name = default_name.to_owned();
    std::thread::Builder::new()
        .name("backup-dialog".into())
        .spawn(move || unsafe { run_dialog(owner, save, &name) })
        .map_err(|_| "无法打开文件对话框".to_owned())?
        .join()
        .map_err(|_| "文件对话框异常退出".to_owned())?
        .map_err(|error| format!("文件对话框出错（{error}）"))
}

unsafe fn run_dialog(
    owner: usize,
    save: bool,
    default_name: &str,
) -> windows::core::Result<Option<String>> {
    use windows::{
        Win32::{
            Foundation::{ERROR_CANCELLED, HWND},
            System::Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemFree, CoUninitialize,
            },
            UI::Shell::{
                Common::COMDLG_FILTERSPEC, FOLDERID_Documents, FOS_FILEMUSTEXIST,
                FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, FOS_PATHMUSTEXIST, FileOpenDialog,
                FileSaveDialog, IFileDialog, IShellItem, KF_FLAG_DEFAULT, SHGetKnownFolderItem,
                SIGDN_FILESYSPATH,
            },
        },
        core::{HSTRING, Interface, PCWSTR},
    };
    unsafe {
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let result = (|| -> windows::core::Result<Option<String>> {
            let dialog: IFileDialog = if save {
                CoCreateInstance::<_, windows::Win32::UI::Shell::IFileSaveDialog>(
                    &FileSaveDialog,
                    None,
                    CLSCTX_INPROC_SERVER,
                )?
                .cast()?
            } else {
                CoCreateInstance::<_, windows::Win32::UI::Shell::IFileOpenDialog>(
                    &FileOpenDialog,
                    None,
                    CLSCTX_INPROC_SERVER,
                )?
                .cast()?
            };
            let label = HSTRING::from("拈来备份 (*.nianlai-backup)");
            let pattern = HSTRING::from("*.nianlai-backup");
            dialog.SetFileTypes(&[COMDLG_FILTERSPEC {
                pszName: PCWSTR(label.as_ptr()),
                pszSpec: PCWSTR(pattern.as_ptr()),
            }])?;
            dialog.SetFileTypeIndex(1)?;
            dialog.SetDefaultExtension(&HSTRING::from("nianlai-backup"))?;
            dialog.SetTitle(&HSTRING::from(if save {
                "导出备份"
            } else {
                "选择要导入的备份"
            }))?;
            if save && !default_name.is_empty() {
                dialog.SetFileName(&HSTRING::from(default_name))?;
            }
            let options = dialog.GetOptions()?;
            dialog.SetOptions(
                options
                    | FOS_FORCEFILESYSTEM
                    | FOS_PATHMUSTEXIST
                    | if save {
                        FOS_OVERWRITEPROMPT
                    } else {
                        FOS_FILEMUSTEXIST
                    },
            )?;
            // 起始位置：文档文件夹；取不到就用系统默认。
            if let Ok(folder) =
                SHGetKnownFolderItem::<IShellItem>(&FOLDERID_Documents, KF_FLAG_DEFAULT, None)
            {
                let _ = dialog.SetFolder(&folder);
            }
            let owner = (owner != 0).then_some(HWND(owner as *mut _));
            if let Err(error) = dialog.Show(owner) {
                return if error.code() == ERROR_CANCELLED.to_hresult() {
                    Ok(None)
                } else {
                    Err(error)
                };
            }
            let item = dialog.GetResult()?;
            let path = item.GetDisplayName(SIGDN_FILESYSPATH)?;
            let text = path.to_string().ok();
            CoTaskMemFree(Some(path.0 as *const _));
            Ok(text)
        })();
        if initialized {
            CoUninitialize();
        }
        result
    }
}

/// 在资源管理器里选中这个文件。
pub fn reveal(path: &std::path::Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", path.display()))
        .spawn()
        .map(drop)
        .map_err(|_| "无法打开资源管理器".to_owned())
}
