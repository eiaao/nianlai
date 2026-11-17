# 参与开发

从 `main` 新建分支，完成后提 PR。跨平台改动需同时通过 Windows 与 macOS CI；Mac 系统交互还需要真机验证。不要把 CI 通过写成安装、权限、粘贴或多屏行为已经验收。

## 开发前提

- rustup：仓库的 `rust-toolchain.toml` 固定 Rust 及 rustfmt、clippy 版本，首次执行 Cargo 时会安装。
- Node.js 22、pnpm 10，与 CI 一致。
- Windows：Visual Studio 生成工具中的 C++ 桌面开发组件，以及 Microsoft Edge WebView2 运行时。
- Mac：macOS 13 或更新版本，安装 Xcode 命令行工具：

```bash
xcode-select --install
```

在仓库根目录安装依赖并开发运行：

```bash
pnpm install --frozen-lockfile
pnpm tauri dev
```

Mac 会自动合并 `src-tauri/tauri.macos.conf.json`。应用名称为 `Nianlai.app`，最低系统版本为 13，Beta 使用 ad-hoc 签名。无需配置开发者证书即可构建；这不等于 Developer ID 签名或 Apple 公证。

## 构建与产物

Windows 便携版继续使用现有 PowerShell 脚本：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tools\release.ps1
```

产物位于 `release\`，脚本同时生成校验值与来源信息。不要为 Mac 改写这条 Windows 路径。

在 Mac 上构建同时包含 Apple Silicon 与 Intel 的安装包：

```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin
pnpm tauri build --target universal-apple-darwin
```

`universal-apple-darwin` 是 Tauri 的合成目标，不传给 `rustup target add`。未设置 `CARGO_TARGET_DIR` 时，App 位于 `target/universal-apple-darwin/release/bundle/macos/Nianlai.app`，DMG 位于同一 `bundle` 目录下的 `dmg/`；设置该环境变量时，以其值替代 `target`。

CI 会检查 App 主程序同时含 `arm64`、`x86_64` 架构，并使用 `codesign --verify --deep --strict` 核对签名结构。随后把 DMG 归一命名为 `nianlai-<版本>-macos-universal.dmg`，生成同名 `.dmg.sha256`，一并上传到 `nianlai-macos` artifact。签名结构验证不代表已经公证。

本地可按相同方式检查 App（以下使用默认输出目录）：

```bash
app="target/universal-apple-darwin/release/bundle/macos/Nianlai.app"
executable=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$app/Contents/Info.plist")
lipo "$app/Contents/MacOS/$executable" -verify_arch arm64 x86_64
codesign --verify --deep --strict "$app"
```

在 [CI](https://github.com/eiaao/nianlai/actions/workflows/ci.yml) 中核对运行的分支和提交，下载成功运行的 `nianlai-macos` artifact 并解压。在 DMG 与校验文件所在目录运行：

```bash
shasum -a 256 -c *.dmg.sha256
```

CI artifact 保留 30 天，是对应提交的测试构建。不要把旧提交的包或开发运行结果当成当前提交的安装验收结果。

## 提交前检查

在 Windows 与 Mac 上运行共用检查：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm check
pnpm build
node tools/test-panel-key-repeat.mjs
node tools/test-platform.mjs
```

涉及设置、引导或面板页面时，运行对应的现有浏览器回归：

```bash
node tools/test-settings-pages.mjs
node tools/test-guide.mjs
node tools/test-guide-component.mjs
node tools/test-task13.mjs
```

浏览器脚本使用内存 IPC。Windows 默认寻找 Edge；Mac 可用 `EDGE_PATH` 指向 Chrome 或 Edge 的可执行文件，例如：

```bash
export EDGE_PATH="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
```

部分现有浏览器断言针对 Windows 键名，应在 Windows 上保持通过；指定 Mac 浏览器运行并不代替下方的真实 App 验收。

Windows CI 还会检查发布脚本及版本来源：

```powershell
python -X utf8 -I tools/check_release_versions.py
python -X utf8 -I tools/check_ps1_encoding.py
python -X utf8 -I -m unittest discover -s tools -p "test_release*.py" -v
```

## Mac 真机验收

在 macOS 13 或更新的真实 Mac 上记录系统版本、CPU 架构、屏幕排列、缩放及被测提交。安装验收使用实际 DMG：将 `Nianlai.app` 拖入“应用程序”，从该位置打开；未签名来源提示与辅助功能授权步骤见 [README](README.md#macosbeta)。至少在一台未安装开发工具的 Mac 上完成安装、打开、授权及下列第 1–6 项。

以下是待执行的清单，列出条目不表示已经通过：

1. 按 Control+反引号呼出，菜单栏仍显示原 App；再按收起，原输入位置保留且能继续打字。
2. 在备忘录复制中文，呼出后按 1，内容贴到原位置；在 Safari 地址栏、VS Code、终端和微信中也检查 Enter 与数字键粘贴。
3. 用系统拼音输入法搜索“会议”，按 ⌘1 粘贴。
4. 网页片段贴到 Pages 或备忘录时保留富文本；⌘↩ 改贴纯文本，⇧↩ 只复制。
5. 用 ⌘⇧⌃4 截图到剪贴板，历史出现图片，Tab 看全后可贴进微信。
6. 在访达复制两个文件，可贴到另一文件夹或作为邮件附件。
7. 从 1Password 或 KeePassXC 复制密码不进入历史；把备忘录的 App 名称或 Bundle ID 加入排除列表后，其复制内容不记录。
8. 撤销辅助功能权限后，选择内容显示“已复制”与授权入口；重新授权后不重启即可恢复直接粘贴。检查光标定位，不支持读取时回退到鼠标位置。
9. 全屏 Safari、其他桌面空间、第二块屏幕上均可呼出；检查不同缩放以及左侧、上方副屏的面板和引导位置。
10. ⌫ 删除、⌘Z 撤销、⌘D 收藏与 ↑ 查看收藏可用；按住 ⌘⌫ 删除收藏时，进度走满才删除。
11. 设置中修改呼出键、切换深浅主题、导出再导入备份、在访达显示文件、查看占用、恢复回收站条目、切换开机启动；菜单栏模板图在深浅色下可见。
12. 退出重开后历史仍在；重启电脑后可开机自启；再次打开 App 不产生第二个实例。新手引导可点击，呼出面板后引导不会抢走键盘输入。

在 PR 中填写实际结果和未通过项目。CI 的双架构与签名检查不覆盖以上系统交互；未通过或未执行的条目应明确保留为待验收。

## 发布

选择尚未发布的版本号，不覆盖已存在的标签。Windows 维护者继续使用现有脚本同步版本，以下 `<新版本>` 需替换为选定的版本号：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tools\release.ps1 -Version "<新版本>" -AllowDirty
```

该脚本同步 `src-tauri/Cargo.toml`、`package.json` 与 `Cargo.lock`。提交版本改动并完成双平台 CI 和真机验收后，再创建和推送与版本一致的 `v<新版本>` 标签。

标签触发 `.github/workflows/release.yml`：核对版本、运行共用 CI、构建 Mac universal DMG、校验 Windows 来源信息和两平台校验值，最后在同一个 GitHub Release 上传 Windows 便携版、Mac DMG 及各自校验文件。Mac 构建步骤直接位于 workflow 中，不另写发布脚本。普通 PR 和手动 CI 提供开发产物；标签发布时由 release workflow 构建 Mac 包，避免重复构建。

Release 说明须标注 macOS Beta、最低系统版本、Apple Silicon/Intel、首次打开与辅助功能授权方式，以及 [README 中的 Beta 限制](README.md#macosbeta)。Beta 只使用 ad-hoc 签名；Developer ID 签名和公证不在本阶段范围内。版本号包含 `-` 时整个 Release 为预发布，不为 Mac 单独创建另一份 Release。
