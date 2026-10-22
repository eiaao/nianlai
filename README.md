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
- `measure-memory.ps1`：统计应用及其 WebView2 子进程的内存合计。

## 许可证

待定。
