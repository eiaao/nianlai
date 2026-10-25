# 前端结构（src/lib）

M0-7 把原来一个文件的 `src/App.svelte` 拆成下面这些文件，外观和行为与拆分前一致。M1 按正式设计稿重做界面时，在这个结构上改。

## 目录

| 文件 | 作用 |
|---|---|
| `../main.ts`、`../app.css` | 面板入口（`index.html`）；`app.css` 只放全局基础样式 |
| `theme.css` | 主题变量（颜色、间距、圆角、字号、阴影），深浅两套；面板和设置窗口共用 |
| `ipc.ts` | 和后端的全部往来：命令（invoke）、事件（listen）、载荷类型。命令名、事件名只在这里出现 |
| `state.svelte.ts` | `PanelState`：面板状态（会话、模式、列表、选中项、搜索词、粘贴结果）和交互逻辑，用 Svelte 5 runes |
| `panel/Panel.svelte` | 根组件：创建 `PanelState`；摆放顶部（标题、条数、记录状态、存储状态、搜索框、按键提示）、列表、页脚 |
| `panel/SearchBox.svelte` | 搜索框 |
| `panel/HistoryList.svelte`、`panel/HistoryItem.svelte` | 列表、单条（槽位号、类型、三行摘要和高亮、来源 · 时间） |
| `panel/Footer.svelte` | 页脚：粘贴结果和提示（含管理员提示）、模式、呼出耗时、退出键、版本号 |
| `panel/Preview.svelte` | 预览区占位（F04），现在不挂载 |
| `../settings/`、`/settings.html` | 设置窗口的入口和占位页；窗口由 `src-tauri/src/settings_window.rs` 按需创建 |

## 状态流向

```
后端 ──事件 panel:*──▶ ipc.ts ──▶ PanelState.connect() 里的处理函数 ──▶ $state 字段 ──▶ 组件重新渲染
组件的 DOM 事件 ──▶ PanelState 的方法（navigate、choose、keydown……）──▶ ipc.ts 的命令 ──▶ 后端
```

- 只有 `Panel.svelte` 创建 `PanelState`，以 `panel` 属性传给子组件。子组件只读状态、调方法，不直接调 `ipc.ts`。
- `Panel.svelte` 初始化时先 `panel.watch()`（登记搜索、选中项两个副作用，顺序不要换），再 `onMount(panel.connect)`（订阅事件、发 `frontend_ready`，卸载时清理）。
- 快速模式页面没有焦点，按键由键盘钩子经 `panel:key` 转给 `navigate()`；搜索模式的按键走 `<svelte:window onkeydown>` → `keydown()`。
- DOM 引用 `input`、`list` 由 SearchBox、HistoryList 用 `bind:this` 填进 `PanelState`。

## 设置窗口壳

- `settings.html` 与 `index.html` 是 Vite 的两个构建入口，设置页目前只有“设置”和“开发中”。
- 窗口定义集中在 `settings_window.rs`，不在启动配置中创建第二个 WebView；调用异步命令 `open_settings` 才创建。重复打开会显示并聚焦已有窗口，关闭后销毁，下次重新创建。
- `lib.rs` 按 `settings` 标签放行关闭请求，关闭设置窗口不会走面板的退出分支。托盘入口由后续任务接入；从菜单或托盘回调调用 `settings_window::open()` 时须先用异步运行时切离主线程，避免 Windows WebView2 创建窗口时死锁。

## 主题变量

- 名字对齐设计稿 `tokens.css` 的规则（`--color-<用途>-<角色>`、`--space-<n>` 即 n × 4px、`--radius-*`、`--font-*`、`--shadow-*`），值是 G0 现状。M1 换成设计稿的值时只改 `theme.css`；标着“M0 现状专用”的名字在 M1 改完样式后删掉。
- 深色默认跟随系统；`<html data-theme="dark">` / `"light"` 可以手动覆盖，现在没有代码设置它。
- 没有变量化的：行高、字重、固定尺寸（搜索框高 38px、槽位号宽 20px、模式圆点 6px、页脚状态行 24px）、1px 边框宽、圆点的 50% 圆角。

## 不能动的细节（都是 G0 踩过的坑）

1. 新搜索请求只在 `search()` 里递增 `searchRequest`，不在 oninput 里加（否则刚发出的请求立刻过期，回复全被丢掉）；显示、隐藏和卸载时的递增仅用于使旧回复失效，保持原有行为。
2. 输入法组合期间不搜索：`composing` 故意不是 `$state`，组合结束时 `searchRevision += 1` 触发一次补搜。
3. 进入搜索模式：聚焦搜索框后等两帧（两次 requestAnimationFrame）再发 `search_ready`，后端才补发首键。
4. `onbeforeinput` 上报补发后的第一个输入是直接上屏还是进了输入法（只报类型和时序，不报内容）。
5. Ctrl+Enter 粘贴为文本（`paste_item` 的 mode 为 `text`）；输入法上屏后 50 ms 内的 Ctrl+Enter、Ctrl+1–9、Enter、Esc、↑↓ 不处理。
6. `class:quiet` 和 `caret-color: transparent`：实验开关 NIANLAI_G0_SEARCHBOX_FOCUS 开着时，快速模式下搜索框持有焦点但不显示光标和焦点色。
7. 搜索结果高亮按 `searchedQuery`（已经拿到回复的那次搜索词），不按正在输入的 `query`。
8. M0-6 管理员提示：粘贴结果带 `hint_dwell_ms` 时只显示 `ELEVATED_HINT`，`busy` 保持到面板收起（停留期间 1–9、Enter、点条目都不发粘贴）。
9. M0-5 页脚版本号：启动时 `getVersion()`（`ipc.appVersion`）。
10. 全部事件订阅好之后才发 `frontend_ready`；每次 `panel:shown` 都重置选中项、`busy`、输入法状态，两帧后发 `report_painted`。
11. 条目按下时 `preventDefault`，不抢搜索框焦点；快速模式点搜索框是请求 `start_search`，不是直接聚焦。
12. 面板里禁用右键菜单。

这些细节改动前要在样机上走一遍：呼出、打字搜索、中文输入、数字键、Enter、Ctrl+Enter、Esc。
