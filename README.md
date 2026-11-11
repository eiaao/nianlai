# 拈来 Nianlai

[![CI](https://github.com/eiaao/nianlai/actions/workflows/ci.yml/badge.svg)](https://github.com/eiaao/nianlai/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/eiaao/nianlai?include_prereleases&label=release)](https://github.com/eiaao/nianlai/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

> 取历史之一缕，付当前之一键。

拈来是一个开源的 Windows 剪贴板历史工具：复制过的内容都留在本机，按一下快捷键呼出，挑一条直接粘贴到你正在用的程序里。呼出面板时**不抢焦点**，光标还在原来的位置；打字就能搜，中文也能搜。

> **早期预览版（0.1.0-alpha）**：只支持 Windows x64，功能和数据格式仍可能变化。日常可用，但还在自用验证阶段。

## 特点

- **不打断手头的事**：面板不激活窗口，目标程序的光标和选区保持原样；选中后直接粘贴回去。
- **键盘优先**：数字键 1–9 直接取用，Enter 粘贴，Ctrl+Enter 粘贴为纯文本，Shift+Enter 只复制。
- **打字就搜**：全文搜索，支持中文；1 万条历史里搜索也在十几毫秒内完成。
- **粘贴可靠**：发键前检查目标和剪贴板状态，尽量确认确实粘贴进去了；没把握时明确告诉你“已复制，请手动粘贴”，不会悄悄失败。
- **收藏**：常用内容收藏后固定在顶部，不随历史过期，也不占用数字编号。
- **三种布局**：分栏预览、极简列表、卡片流，可随时切换；深浅主题跟随系统。
- **保留格式**：文字、富文本、图片、文件都按原格式记录，并显示来源程序的图标。
- **本地优先**：数据只存在本机，不联网，不上传。

## 下载

在 [Releases](https://github.com/eiaao/nianlai/releases) 下载 `nianlai-<版本>-windows-x64-portable.zip`，解压到任意可写的文件夹，双击 `nianlai.exe` 即可，不需要安装。

- 需要 Microsoft Edge WebView2 运行时（Windows 11 已自带；Windows 10 若缺失，请安装微软的 WebView2 Evergreen 运行时）。
- 程序暂未签名。首次运行时 Windows SmartScreen 可能拦截，确认文件来自本仓库后，点“更多信息 → 仍要运行”。每个版本都附有 SHA-256 校验值。
- 开机自启：在托盘菜单里勾选。

## 使用

按 **Ctrl+`**（Esc 下面的反引号键）呼出面板，再按一次收起。快捷键可以在托盘菜单的“设置…”里改。

| 按键 | 作用 |
|---|---|
| 直接打字 | 搜索 |
| ↑ ↓ / PageUp PageDown / Home End | 选择（卡片流布局用 ← →） |
| 1–9 | 直接粘贴对应条目（搜索时用 Ctrl+1–9） |
| Enter | 粘贴 |
| Ctrl+Enter | 粘贴为纯文本 |
| Shift+Enter | 只复制，不粘贴 |
| Space | 预览完整内容 |
| Ctrl+D | 收藏 / 取消收藏 |
| Delete | 把当前条目移到回收站（收藏的要按住不放，进度走满才删）；搜索时用 Ctrl+Delete |
| Ctrl+Z | 撤销最近的删除，关掉面板再打开也可以；可以连续撤销 |
| Esc | 收起面板 |

按住 Ctrl 或 Shift 稍等片刻，面板底部会提示对应的组合键。鼠标单击条目也可以直接取用。

退出：托盘图标右键“退出”，或按 Ctrl+Alt+Shift+Q。

**目标是管理员权限的程序时**：普通权限的拈来无法向它发送按键，会把内容放进剪贴板并提示你手动按 Ctrl+V。

## 数据与隐私

- 历史记录存在 `%LOCALAPPDATA%\Nianlai\data`，设置在 `%APPDATA%\Nianlai\settings.json`，日志在 `%LOCALAPPDATA%\Nianlai\logs`。
- 不联网：没有统计、遥测，也不检查更新。
- 遵守密码管理器等程序设置的“不要记录”标记，这类内容不会进入历史。
- 日志不记录剪贴板正文和窗口标题。
- 程序崩溃后会自动重启，24 小时内最多 3 次；数据库升级前自动备份。
- 复制文件时只记路径，不复制文件本身；原文件被移走或删除后，条目会标成“已失效”。
- 不会自动删除任何记录。设置的“数据与备份”页能看到占用，手动清理失效条目、截图和临时文件；清理的条目先进回收站，7 天内可以恢复。

## 进展

| 阶段 | 内容 | 状态 |
|---|---|---|
| v0.1 · M0 | 工程底座：单实例、托盘、自启、崩溃恢复、数据迁移、便携版 | ✅ |
| v0.1 · M1 | 日常可用：来源程序、收藏、三种布局、主题、快捷键设置 | ✅ |
| v0.1 · M2 | 数据安全：排除应用、跳过密码、暂停记录、快照与备份、存储管理、防误删 | 进行中 |
| v0.1 · M3 | 发布准备：应用内更新、首次引导、安装包 | 计划中 |
| v0.5 | 拼音搜索、接管 Win+V、分组、从 Ditto / EcoPaste 导入、macOS Beta | 计划中 |
| v1.0 | 连续粘贴、文本转换、手机扫码互传等 | 计划中 |

问题和建议欢迎提 [Issue](https://github.com/eiaao/nianlai/issues)。

## 从源码构建

技术栈：Tauri 2 + Rust，前端 Svelte 5 + TypeScript。

准备 rustup（Rust 版本由 `rust-toolchain.toml` 固定，首次构建时自动安装）、Node.js 20 以上、pnpm，以及 Visual Studio 生成工具和 WebView2。

```bash
pnpm install
pnpm tauri dev                 # 开发运行
pnpm tauri build --no-bundle   # 正式版构建
```

打便携版：`powershell -NoProfile -ExecutionPolicy Bypass -File tools\release.ps1`，输出到 `release\`。开发和验证用的工具见 [tools/README.md](tools/README.md)。

## 参与开发

从 `main` 新建分支，完成后提 PR。提交前在仓库根目录跑一遍 CI 用到的检查：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm check
node tools/test-panel-key-repeat.mjs
python -X utf8 -I tools/check_release_versions.py
python -X utf8 -I tools/check_ps1_encoding.py
python -X utf8 -I -m unittest discover -s tools -p "test_release*.py" -v
```

CI（`.github/workflows/ci.yml`）在 PR、推送到 `main` 和手动触发时运行：先做上面的检查，再在 Windows 上真实构建便携版，产物保留 30 天。

### 发版

1. 改版本号：`powershell -NoProfile -ExecutionPolicy Bypass -File tools\release.ps1 -Version 0.1.0-alpha.3 -AllowDirty`，会同步 `src-tauri/Cargo.toml`、`package.json` 和 `Cargo.lock`。
2. 提交版本号改动，推送到 `main`。
3. 打标签并推送：`git tag v0.1.0-alpha.3 && git push origin v0.1.0-alpha.3`。

推送 `v*` 标签后，`.github/workflows/release.yml` 会核对标签与版本一致 → 完整检查和构建 → 核对校验值和来源提交 → 创建 GitHub Release 并上传便携版。带 alpha、beta、rc 的版本标为预发布。

## English

Nianlai (拈来) is an open-source clipboard history manager for Windows. Press <kbd>Ctrl</kbd>+<kbd>`</kbd> to open the panel **without stealing focus**, type to search (including Chinese), and press Enter or 1–9 to paste straight into the app you were using. Everything stays on your machine — no network access. It is an early alpha: Windows x64 only, portable zip from [Releases](https://github.com/eiaao/nianlai/releases). macOS is planned for v0.5.

## 许可证

[MIT](LICENSE)
