# 拈来 Nianlai

复制过的，信手拈来。

一个开源的剪贴板工具，Windows 先行，macOS 随后。目前是早期预览版（0.1.0-alpha），只支持 Windows x64，功能和数据格式仍可能变化。

An open-source clipboard manager for Windows and macOS. Everything you copied, at hand. Early alpha, Windows only for now.

## 开发

技术栈：Tauri 2 + Rust，前端 Svelte 5 + TypeScript。

准备：Rust（stable，Windows 上用 MSVC 工具链）、Node.js 20 以上、pnpm；Windows 需要 Visual Studio 生成工具和 WebView2（Windows 11 自带）。

```bash
pnpm install
```

```bash
pnpm tauri dev
```

开发辅助工具在 `tools/` 下：

- `focus-probe`：焦点诊断小工具，实时显示前台窗口、焦点控件和插入符，并在屏幕上框出焦点控件。运行 `cargo run -p focus-probe`，加 `--once` 只看一次，加 `--json` 输出 JSON。
- `focus-matrix`：焦点验收矩阵的无人值守跑法（探路版）。默认只做只读空跑 `cargo run -p focus-matrix -- --dry-run`；真跑要加 `--real-input`，会接管键盘鼠标，用法和注意事项见 `tools/focus-matrix/README.md`。
- `measure-memory.ps1`：统计应用及其 WebView2 子进程的内存合计。
- `release.ps1`：一条命令构建 Windows x64 便携版：`powershell -NoProfile -ExecutionPolicy Bypass -File tools\release.ps1`。默认版本来自 `src-tauri/Cargo.toml`，同步 `package.json`，输出 `release/nianlai-<版本>-windows-x64-portable.zip`、`nianlai-<版本>.sha256` 和 `build-info.json`；固定构建目标 `x86_64-pc-windows-msvc`，读取 Cargo 目标目录下 `x86_64-pc-windows-msvc/release/nianlai.exe` 并校验 PE x64 架构和打包哈希。`-Version 0.1.0-alpha.1` 指定版本，`-SkipBuild` 打包该目标目录中的已有程序，`-OutDir <目录>` 指定输出目录，`-AllowDirty` 允许未提交改动。指定新版本后需审查并提交清单和锁文件；跳过构建时需自行确认已有程序的版本，构建信息记录 `skip_build=true`、`source_commit=null`，其中 `git_commit` 仅表示打包时的工作区提交。ZIP 附带运行、WebView2、SmartScreen、数据目录和退出说明，不含安装包及签名。
- `measure-memory-overnight.ps1`：整夜每分钟采样样机进程树的内存，可以带重挂起开关（`-ResuspendMinutes`）启动样机，结束时切出本次日志并生成摘要；用法见脚本开头的说明。
- `g0_log_report.py`：把样机的诊断日志汇总成 Markdown 报告（采集、监听、粘贴、呼出、错误）。运行 `python tools/g0_log_report.py --log <panel-log.jsonl>`。
- `g0_matrix_report.py`：焦点矩阵手工格子的日志汇总，按“目标程序 × 模式”统计粘贴结论、降级原因和可疑试验。运行 `python tools/g0_matrix_report.py --log <panel-log.jsonl> --out <报告.md>`。
- `measure-show-under-pressure.ps1`：测内存紧张时的呼出耗时。用自动循环反复隐藏、显示样机，显示前修剪工作集（`trim`）或用辅助进程占内存（`pressure`），输出每次显示的耗时；`-DryRun` 只打印计划。
- `g0-night-batch.ps1`：夜间无人值守批处理，按顺序重建样机、跑焦点矩阵（空跑 → 冒烟 → 全量）和内存基线（默认 8 小时），逐步记录 `batch.log`，收尾写 `summary.txt`；`-DryRun` 只打印计划，`-SkipBuild` 跳过重建样机，`-SkipMatrix` 跳过矩阵只跑内存。
- `g0-store`：历史库的压测和维护工具，命令有 `fill`、`search-bench`、`writer`、`check`、`snapshot`、`restore` 等。运行 `cargo run --release -p g0-store -- <命令> --dir <数据目录>`。
- `g0-crash-test.ps1`、`g0-store-validation.ps1`、`g0-smoke-test.ps1`：基于 `g0-store` 的故障注入（反复强杀写入进程再检查库）、存储压测汇总，以及正式版的冒烟测试。

## 开发流程

`main` 是 v0.1 的开发主线。每个任务从 `main` 新建独立分支（如 `m0/ci`），完成后通过 PR 合并。

CI 在 Windows 上使用 Node.js 22、pnpm 10、Rust stable（含 clippy、rustfmt）和 Python 3.12。首次安装依赖使用 `pnpm install --frozen-lockfile`，提交前在仓库根目录执行以下验收命令：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm check
pnpm build
python -X utf8 -I tools/check_release_versions.py
python -X utf8 -I -m unittest discover -s tools -p "test_release*.py" -v
```

CI（`.github/workflows/ci.yml`）在所有 PR、推送到 `main` 和手动触发时运行：先跑上面的检查和面板状态机回归，再用 `tools/release.ps1` 真实构建便携版，ZIP、SHA-256 校验文件和 `build-info.json` 作为构建产物保留 30 天。同一 PR 有新推送时会取消旧的运行。

### 发版

1. 改版本号：`powershell -NoProfile -ExecutionPolicy Bypass -File tools\release.ps1 -Version 0.1.0-alpha.3 -AllowDirty`。它会同步 `src-tauri/Cargo.toml`、`package.json` 和 `Cargo.lock`，顺带在本地打一个包。
2. 提交版本号改动并推送到 `main`。
3. 打标签并推送：`git tag v0.1.0-alpha.3 && git push origin v0.1.0-alpha.3`。

推送 `v*` 标签会触发 `.github/workflows/release.yml`：核对标签与 `Cargo.toml` 版本一致 → 跑完整的检查和构建 → 核对校验值、版本和来源提交 → 创建 GitHub Release 并上传 ZIP、校验文件和构建信息。版本号带 `-`（alpha、beta、rc）的标为预发布。说明里附 SHA-256，并自动列出自上个标签以来的提交。

建议仓库维护者在 GitHub 网页上为 `main` 设置分支保护：禁止直接推送，要求通过 PR 合并，并要求 CI 检查通过。

## 许可证

[MIT](LICENSE)。
