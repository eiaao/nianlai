# 拈来 Nianlai

复制过的，信手拈来。

一个开源的剪贴板工具，Windows 先行，macOS 随后。目前还在规划阶段，没有可用的版本。

An open-source clipboard manager for Windows and macOS. Everything you copied, at hand. Work in progress.

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
- `g0_log_report.py`：把样机的诊断日志汇总成 Markdown 报告（采集、监听、粘贴、呼出、错误）。运行 `python tools/g0_log_report.py --log <panel-log.jsonl>`。
- `g0-store`：历史库的压测和维护工具，命令有 `fill`、`search-bench`、`writer`、`check`、`snapshot`、`restore` 等。运行 `cargo run --release -p g0-store -- <命令> --dir <数据目录>`。
- `g0-crash-test.ps1`、`g0-store-validation.ps1`、`g0-smoke-test.ps1`：基于 `g0-store` 的故障注入（反复强杀写入进程再检查库）、存储压测汇总，以及正式版的冒烟测试。

## 许可证

待定。
