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
- 普通 `quick` 不激活，数字由键盘钩子经 `panel:key` 转给 `navigate()`；其原有搜索首键缓冲、两帧就绪和补发路径保留。权限较高或无法确认的目标先进入 `activation_pending`，取得面板前台后进入 `activated`，初始 `mode` 仍是 `quick`。
- `activated` 只用真实 DOM `keydown` / `keyup`，忽略 `panel:key` 防双发。后端放行新键，只保留历史吞键配对。确认键按下时固定会话、条目和模式；匹配可信抬起且已观察按键/修饰键全部松开后，才取用或关闭。方向、翻页键允许重复导航。搜索选择也在按下时固定原 `selection`，松键期间 revision 变化就取消，不刷新后重发。
- `interaction` 和本地 DOM 焦点共同控制能否操作。`activated` 先挂载同一个可编辑搜索框，确认 `document.hasFocus()` 且 `activeElement` 为该框后才展示列表和快捷键；准备超过 500 ms 或焦点失败时提供本地鼠标重试，不调用仅适用于 `activation_failed` 的后端重试。原生 `activation_pending` / `activation_failed` 无列表和搜索入口，失败重试仍走后端。`passive` 只显示复制提示和鼠标关闭入口。
- Activated 的普通首字和输入法由浏览器在原框自然编辑；`beforeinput` / `compositionstart` 请求进入搜索，`searchStarting` 期间不处理 Quick 数字取用，后续字符继续留在原框。`/`、Ctrl+F 以空查询进入搜索。后端切换不重新激活、MoveFocus 或补发首字，前端也不二次聚焦；切换失败或 1 秒没有模式回执时显示重新呼出提示，不能永久停在准备态。
- 根内容只在本会话 `visible` 时渲染，`interactive` 同样依赖可见性；隐藏后不保留搜索、编号列表和快捷键提示，避免窗口先于下一次 `panel:shown` 显示时露出旧会话的就绪承诺。
- 搜索取用必须传实际 DOM 选择事件。`PanelState` 在第一处 `await` 前固定会话、条目、粘贴模式及 `selection {revision, nonce}`；`panel:key` 不能建立搜索许可。每个前台 revision 回调记录 `performance.now()`，只允许同一时钟下接收后生成的新可信事件，迟到事件提示重新选择。旧/重复 revision 不降低高水位，但把接收时刻推进到现在；非法 revision 暂停选择，需新合法通知或重新呼出。
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
3. 普通 Quick 进入搜索：聚焦搜索框后等两帧（两次 requestAnimationFrame）再发 `search_ready`，后端才补发首键。Activated 沿用已聚焦的同一输入框，握手期间不再 focus，后端只结束空缓冲流程，不补发或做会打断组合的焦点探针。
4. `onbeforeinput` 上报补发后的第一个输入是直接上屏还是进了输入法（只报类型和时序，不报内容）。
5. Ctrl+Enter 粘贴为文本（`paste_item` 的 mode 为 `text`）；输入法上屏后 50 ms 内的 Ctrl+Enter、Ctrl+1–9、Enter、Esc、↑↓ 不处理。
6. `class:quiet` 和 `caret-color: transparent`：实验开关 NIANLAI_G0_SEARCHBOX_FOCUS 开着时，普通 `quick` 下搜索框持有焦点但不显示光标和焦点色；Activated 可编辑以承载自然首字，数字与提交键通过 `preventDefault` 保持快速取用语义。
7. 搜索结果高亮按 `searchedQuery`（已经拿到回复的那次搜索词），不按正在输入的 `query`。
8. M0 批准的激活式交互：粘贴结果带 `hint_dwell_ms` 时转为 `passive`，只显示后端 `hint_message` 和明确鼠标关闭入口，`busy` 保持到收起。没有消息时用中性的“已复制，请先点击目标输入位置，再按 Ctrl+V”，不猜管理员权限或原输入控件已恢复。归还前台后的被动提示不承诺吞键、Ctrl+V 或 Esc 立即关闭，也不凭提示结束宣称已粘贴。
9. M0-5 页脚版本号：启动时 `getVersion()`（`ipc.appVersion`）。
10. 全部事件订阅好之后才发 `frontend_ready`；每次 `panel:shown` 都重置选中项、`busy`、输入法状态，两帧后发 `report_painted`。
11. 条目按下时 `preventDefault`，不抢搜索框焦点；鼠标选择同样等待 DOM 已观察的真实按键/修饰键松开。普通 Quick 点搜索框请求 `start_search`；Activated 使用已聚焦原框，焦点未就绪则先本地重试。
12. 面板里禁用右键菜单。
13. 搜索提交忽略 `event.repeat`，输入法组合中和上屏后 50 ms 不取用；鼠标同样受保护。提交还检查 DOM 原始 `timeStamp` 与组合结束的间隔，防止排队超过 50 ms 后把防抖期内的按键算成新确认。同一个 DOM 事件只能使用一次，失败后必须有新事件，nonce 继续递增。
14. DOM 待发选择在失焦、隐藏、会话/模式变化时取消，每个 held code/修饰标记记录按下时刻，迟到的旧 keyup 不能清新证据。非组合的新 Esc 优先把待发选择替换成关闭，匹配 Esc 和其它已观察键全部抬起后收起；已 busy 的请求也允许关闭交后端取消。重复或组合中的 Esc 不关闭。没有按键释放的超时放行。

离线回归使用 `node tools/test-panel-key-repeat.mjs` 编译真实 `PanelState`；实机仍需覆盖呼出、激活成功/失败、打字搜索、中文输入、数字键、Enter、Ctrl+Enter、Esc 和被动提示。离线测试不能替代输入法和焦点实机验收。
