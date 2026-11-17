# 拈来 Nianlai

[![CI](https://github.com/eiaao/nianlai/actions/workflows/ci.yml/badge.svg)](https://github.com/eiaao/nianlai/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/eiaao/nianlai?include_prereleases&label=release)](https://github.com/eiaao/nianlai/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

> 取历史之一缕，付当前之一键。

拈来是一个开源的剪贴板历史工具，支持 Windows，并提供 macOS Beta 构建：复制过的内容都留在本机，按一下快捷键呼出，挑一条直接粘贴到你正在用的程序里。呼出面板时**不抢焦点**，光标还在原来的位置；打字就能搜，中文也能搜。

> 已发布的 **0.1.0** 仅提供 Windows x64 版。本分支增加 macOS Beta，安装与系统交互仍需完整真机验收。数据库格式升级时会自动迁移，升级前自动备份。

## 特点

- **不打断手头的事**：面板不激活窗口，目标程序的光标和选区保持原样；选中后直接粘贴回去。
- **键盘优先**：最近 7 张像一手牌，数字键 1–7 直接粘贴，Enter 粘贴，Ctrl+Enter 粘贴为纯文本，Shift+Enter 只复制。
- **打字就搜**：全文搜索，支持中文；1 万条历史里搜索也在十几毫秒内完成。
- **粘贴可靠**：发键前检查目标和剪贴板状态，尽量确认确实粘贴进去了；没把握时明确告诉你“已复制，请手动粘贴”，不会悄悄失败。
- **收藏**：常用的内容设为收藏后单独一手、顺序固定，可以加标题和备注，不随历史过期。
- **手牌与摊开**：平时是一手扇面，要找更早的就把牌摊开成一列，连续滚动、按时间分组；深浅主题跟随系统。
- **保留格式**：文字、富文本、图片、文件都按原格式记录，并显示来源程序的图标。
- **本地优先**：数据只存在本机，不联网，不上传。

## 下载

### Windows

在 [Releases](https://github.com/eiaao/nianlai/releases) 下载 `nianlai-<版本>-windows-x64-portable.zip`，解压到任意可写的文件夹，双击 `nianlai.exe` 即可，不需要安装。

- 需要 Microsoft Edge WebView2 运行时（Windows 11 已自带；Windows 10 若缺失，请安装微软的 WebView2 Evergreen 运行时）。
- 程序暂未签名。首次运行时 Windows SmartScreen 可能拦截，确认文件来自本仓库后，点“更多信息 → 仍要运行”。每个版本都附有 SHA-256 校验值。
- 开机自启：在托盘菜单里勾选。

### macOS（Beta）

需要 **macOS 13 或更新版本**。同一个 universal 安装包支持 Apple Silicon 和 Intel。在包含 macOS 产物的 [Release](https://github.com/eiaao/nianlai/releases) 中，下载 `nianlai-<版本>-macos-universal.dmg` 及同名的 `.dmg.sha256` 校验文件；已发布的 Windows 0.1.0 不包含 Mac 安装包。

开发测试可在 [CI](https://github.com/eiaao/nianlai/actions/workflows/ci.yml) 中选择对应分支与提交的成功运行，下载 **nianlai-macos** artifact（需登录 GitHub）。解压 artifact 后得到 DMG 和 SHA-256 文件；这是该提交的测试构建，不代表已经发布或完成真机验收。在两份文件所在目录运行 `shasum -a 256 -c *.dmg.sha256` 可核对安装包。

1. 打开 DMG，把 **Nianlai.app** 拖进“应用程序”，再从“应用程序”打开。开启开机启动前先完成这一步，避免自启指向已弹出的磁盘映像。
2. Beta 使用 ad-hoc 签名，尚无 Developer ID 签名或 Apple 公证。首次打开被拦截时，确认来自本仓库后，在“系统设置 › 隐私与安全性”中选择“仍要打开”。
3. 在“系统设置 › 隐私与安全性 › 辅助功能”中允许 **Nianlai（拈来）**。未授权时仍能复制选中的内容，并提示按 **⌘V** 手动粘贴；授权后可直接粘贴。引导、面板提示和设置“关于”页都提供去授权的入口。

更新 ad-hoc 构建后，系统可能要求重新授予辅助功能权限。开机启动在菜单栏菜单或设置里开启。

Beta 暂不支持手动逐张粘贴（每按一次 ⌘V 放一张）；有已选时按 Enter 的“一起粘贴”仍可用。Mac 粘贴统一发送一次 ⌘V，不使用 Windows 的原生编辑控件直投，也不能据此确认目标 App 已接收内容。Beta 不提供内存采样、WebView2 挂起与重挂起、正式开发者签名和公证。

## 使用

下表为 Windows 键位。Mac 默认呼出键为 **⌃+`**（Control 加反引号，不是 Command）；其余主要操作把 Ctrl 换成 **⌘**：例如 ⌘↩ 贴纯文本、⌘1–7 搜索中粘贴、⌘D 收藏、⌘Z 撤销。删除用 **⌫**，搜索中用 **⌘⌫**；有已选时 ⌫ 仍是放回最后选的一张。

按 **Ctrl+`**（Esc 下面的反引号键）呼出面板，再按一次收起。快捷键可以在托盘菜单的“设置…”里改。

| 按键 | 作用 |
|---|---|
| 1–7 | 粘贴对应的一张（摊开时是看得见的前 7 条，键帽上标着数字） |
| Enter | 粘贴当前这张；有已选时按顺序一起粘贴 |
| ← → | 上一张 / 下一张（任何状态都一样，搜索中也是） |
| ↑ | 扇面里：切到收藏；再按回到最近 |
| ↓ / PageDown | 最近扇面里：摊开成一列；摊开里 ↓ 下一条、PageDown 翻一屏 |
| Home / End | 摊开里：收回扇面 / 最后一条 |
| 直接打字 | 搜索最近和收藏（自动摊开）；搜索时数字是输入，用 Ctrl+1–7 粘贴 |
| Ctrl+Enter | 粘贴为纯文本 |
| Shift+Enter | 只复制当前一张，不粘贴（有已选时不执行，会提示） |
| Tab | 看全（超长内容、原图）；看全里 Z 切换原图 |
| 空格 | 多选；Backspace 放回最后选的一张 |
| Ctrl+Shift+Enter | 已选的逐张放：之后每按一次 Ctrl+V 粘贴下一张，Esc 结束 |
| Ctrl+D | 设为收藏 / 取消收藏 |
| Delete | 把当前这张放进回收站（收藏的要按住不放，进度走满才删）；搜索时用 Ctrl+Delete |
| Ctrl+Z | 撤销最近的删除，关掉面板再打开也可以；可以连续撤销 |
| Esc | 逐层退出：看全 → 搜索 → 已选 → 摊开 → 收起面板 |

按住 Ctrl 或 Shift 稍等片刻，按键提示会换成对应的组合键。鼠标悬停在牌上会拈起它，单击直接粘贴。滚轮向下摊开，摊开里滚轮逐条。

在面板里搜“设置”（或 settings）会置顶一张“打开设置”，按 Enter 就打开设置窗口。第一次启动时有一张小纸笺带你走一遍“复制 → 呼出 → 粘贴”，之后可以在设置的“关于”页再看一次。

Windows 退出：托盘图标右键“退出”，或按 Ctrl+Alt+Shift+Q。Mac 退出：菜单栏图标菜单中的“退出”，或在面板接收键盘输入时按 ⌘Q；Mac 不注册 Windows 的全局退出组合键。

**目标是管理员权限的程序时**：面板照常呼出、可以打字搜索；但普通权限的拈来无法向它发送按键，会把内容放进剪贴板并提示你手动按 Ctrl+V。

## 数据与隐私

- Windows 历史记录存在 `%LOCALAPPDATA%\Nianlai\data`，设置在 `%APPDATA%\Nianlai\settings.json`，日志在 `%LOCALAPPDATA%\Nianlai\logs`。
- Mac 数据和 `settings.json` 位于 `~/Library/Application Support/Nianlai/`，日志位于 `~/Library/Logs/Nianlai/`。备份格式与 Windows 相同，可互相导入。
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
| v0.1 · M1 | 日常可用：来源程序、收藏、三种布局（v10 起改为手牌）、主题、快捷键设置 | ✅ |
| v0.1 · M2 | 数据安全与整理：排除应用、跳过密码、暂停记录、快照与备份、存储管理、防误删、收藏整理、完整预览 | ✅ |
| v0.1 · M3 | 发布准备：首次引导 ✅；应用内更新、安装包 | 进行中 |
| v0.5 | 拼音搜索、接管 Win+V、分组、从 Ditto / EcoPaste 导入、macOS Beta | macOS Beta 在本分支开发与验证，其余计划中 |
| v1.0 | 连续粘贴、文本转换、手机扫码互传等 | 计划中 |

问题和建议欢迎提 [Issue](https://github.com/eiaao/nianlai/issues)。

## 从源码构建

技术栈：Tauri 2 + Rust，前端 Svelte 5 + TypeScript。

准备 rustup（Rust 版本由 `rust-toolchain.toml` 固定，首次构建时自动安装）、Node.js 22、pnpm 10。Windows 还需要 Visual Studio 生成工具和 WebView2；Mac 需要 Xcode 命令行工具（`xcode-select --install`）。

```bash
pnpm install
pnpm tauri dev                 # 开发运行
pnpm tauri build --no-bundle   # 正式版构建
```

Windows 打便携版：`powershell -NoProfile -ExecutionPolicy Bypass -File tools\release.ps1`，输出到 `release\`。

在 Mac 上构建 universal App 和 DMG：

```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin
pnpm tauri build --target universal-apple-darwin
```

默认输出位于 `target/universal-apple-darwin/release/bundle/`；设置了 `CARGO_TARGET_DIR` 时，以该目录替代 `target`。具体开发前提、产物验证与 Mac 手测清单见 [CONTRIBUTING.md](CONTRIBUTING.md)，其他开发工具见 [tools/README.md](tools/README.md)。

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

CI（`.github/workflows/ci.yml`）在 PR、推送到 `main` 和手动触发时运行：执行 Windows 与 macOS 检查，再构建 Windows 便携版和 Mac universal DMG。`nianlai-windows`、`nianlai-macos` 两份 artifact 保留 30 天；Mac 构建会核对双架构与 ad-hoc 签名，并附 SHA-256。CI 通过不能代替 [Mac 真机验收](CONTRIBUTING.md#mac-真机验收)。

### 发版

1. 选择尚未发布的版本号；不要重用或覆盖现有 `v0.1.0` 标签。用 `tools\release.ps1` 的 `-Version` 参数同步 `src-tauri/Cargo.toml`、`package.json` 和 `Cargo.lock`，具体命令见 [CONTRIBUTING.md](CONTRIBUTING.md#发布)。
2. 提交版本号改动，推送到 `main`。
3. 完成对应版本的检查与真机验收，再创建并推送与版本一致的 `v<版本>` 标签。

推送 `v*` 标签后，`.github/workflows/release.yml` 会核对标签与版本一致 → 完整检查和构建 → 核对校验值和来源提交 → 在同一个 GitHub Release 上传 Windows 便携版和 Mac DMG。Mac 资产附独立的 `.dmg.sha256`，Release 说明标注 macOS Beta 及其限制；不为 Mac 单独创建发布。版本号含 `-`（例如 alpha、beta、rc 版本）时，整个 Release 标为预发布。

## English

Nianlai (拈来) is an open-source clipboard history manager for Windows, with a macOS Beta build on this branch. Press <kbd>Ctrl</kbd>+<kbd>`</kbd> to open the panel **without stealing focus**, type to search (including Chinese), and press Enter or 1–7 to paste into the app you were using. Everything stays on your machine — no network access. The published 0.1.0 release contains only the Windows x64 portable ZIP.

The macOS Beta requires macOS 13 or later and uses one universal DMG for Apple Silicon and Intel. Download it from a release that includes Mac assets, or obtain the `nianlai-macos` artifact for the intended commit from [CI](https://github.com/eiaao/nianlai/actions/workflows/ci.yml). CI artifacts are test builds; real Mac installation and interaction acceptance is still required. Move `Nianlai.app` to Applications before enabling launch at login. The Beta is ad-hoc signed, without Developer ID signing or notarization: use System Settings → Privacy & Security → Open Anyway when first launch is blocked, then grant Accessibility permission. Updates may require granting permission again. Without it, use ⌘V after copying from the panel.

On Mac, the panel shortcut remains Control+backtick; primary actions use Command, deletion uses Backspace (Command+Backspace while searching), and ⌘Q quits while the panel receives keyboard input. The Beta supports automatic multi-item paste but omits manual one-at-a-time paste, Windows native edit-control insertion, memory sampling, WebView2 suspension, Developer ID signing and notarization. Build and acceptance instructions are in [CONTRIBUTING.md](CONTRIBUTING.md).

## 许可证

[MIT](LICENSE)
