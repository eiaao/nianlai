//! 设置窗口：用到时才创建，关掉就销毁。
//!
//! 用代码创建、不写进 `tauri.conf.json`：配置里的窗口在启动时就会创建（要按需创建还得写
//! `create: false` 再用代码建，窗口定义就分在两处），常驻的第二个 WebView2 渲染进程也会抬高
//! 面板隐藏时的内存基线。页面是前端的第二个入口 `settings.html`。

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// 设置窗口的标签。`lib.rs` 的关闭处理按它区分：关设置窗口只销毁这个窗口，不退出程序。
pub const LABEL: &str = "settings";

/// 打开设置窗口：已经开着就提到前面，没有就新建。
///
/// Windows 上在主线程（同步命令、菜单和托盘的事件回调）里新建 WebView2 窗口会死锁，
/// 所以只能在异步命令或另起的线程里调用；托盘的“设置”菜单项接进来时，在回调里用
/// `tauri::async_runtime::spawn` 包一层再调。
pub fn open(app: &AppHandle) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(LABEL) {
        window.unminimize()?;
        window.show()?;
        return window.set_focus();
    }
    WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("settings.html".into()))
        .title("拈来 · 设置")
        .inner_size(860.0, 620.0)
        .center()
        .build()?;
    Ok(())
}

/// 打开设置窗口的命令。异步命令跑在 Tauri 的异步线程上，避开上面说的死锁。
#[tauri::command]
pub async fn open_settings(app: AppHandle) -> Result<(), String> {
    open(&app).map_err(|error| error.to_string())
}
