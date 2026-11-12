# 前端结构（src/lib）

这份说明讲前端各文件管什么、状态怎么流。后端见 [`src-tauri/README.md`](../../src-tauri/README.md)。

## 目录

| 文件 | 作用 |
|---|---|
| `../main.ts`、`../app.css` | 面板入口（`index.html`）；`app.css` 只放全局基础样式 |
| `theme.css` | v10 手牌主题 tokens（纸张、牌面、朱砂、共享状态色、动效时长），深浅两套；面板和设置窗口共用 |
| `preferences.ts` | 主题、动效的类型、默认值与设置页应用逻辑；v10 起只有手牌布局，旧 `panel.layout` / `panel.density` 不再读取 |
| `ipc.ts` | 和后端的全部往来：命令（invoke）、事件（listen）、载荷类型。命令名、事件名只在这里出现（例外：设置窗口的 `settings:navigate` 在 `Settings.svelte` 里直接监听） |
| `state.svelte.ts` | `PanelState`：会话、模式、姿态（扇面 / 摊开）、冻结顺序、选中项、数字对应、已选、搜索词、预览与粘贴反馈 |
| `cards.ts` | 纯函数：九种牌面判定、网址 / 颜色 / 邮箱拆分、时间分组与相对时间、扇面和摊开的几何（`fanBox`、`spreadLayout`） |
| `panel/Panel.svelte` | 手牌根组件：会话、激活、逐张放小指示与原生命中区域上报 |
| `panel/HandPanel.svelte` | 唯一的摆放入口：按姿态算每张牌的目标位置，底板、分组标题、时间分隔、牌堆、按键提示、已选 |
| `panel/HandCard.svelte` | 单张牌：扇面牌面与摊开行面两层内容、拈起、已选、拖出和鼠标取用 |
| `panel/SearchBox.svelte` | 同一个真实搜索输入框（扇面时看不见但保留焦点，摊开时在一列顶部） |
| `panel/Preview.svelte` | 文本/长文/代码/链接/图片/颜色/文件预览；图片用 `get_image` 取缩略图或原图，文件逐个显示是否还在 |
| `panel/PreviewOverlay.svelte` | 手牌中央 440×440 的“看全”浮层，Tab/Esc 合上，← → 换一张，Z 切换原图 |
| `panel/RecordBanner.svelte`、`panel/Footer.svelte` | 记录状态横幅；轻提示条（粘贴结果、撤销删除、notice、错误），不含按键提示 |
| `panel/motion.ts` | 共用 SpringValue 与减少动画策略 |
| `../settings/`、`/settings.html` | 设置窗口，见下面“设置窗口” |

## 设置窗口（src/settings）

| 文件 | 作用 |
|---|---|
| `Settings.svelte` | 窗口外壳和导航；“通用”页（快捷键、主题、动效）；“关于”页还是占位 |
| `CapturePage.svelte` | 隐私与记录：暂停和“忽略下一次复制”、排除应用、敏感内容规则、记录哪些类型、文字上限、临时内容等待时间 |
| `StoragePage.svelte` | 数据与备份：占用统计、垃圾清理、存档（代码里叫 snapshot）、回收站、备份导出导入的入口 |
| `SnapshotSection.svelte`、`TrashSection.svelte`、`ImagePicker.svelte` | 数据与备份页里的存档列表、回收站、图片清理 |
| `BackupExport.svelte`、`BackupImport.svelte` | 备份导出、导入向导（三步校验，合并或覆盖） |
| `OrganizePage.svelte` | 整理：常用排序、标题和备注、粘贴后是否移到最前 |
| `InteractionPage.svelte` | 交互：呼出位置、单击和中键行为 |
| `HotkeyEditor.svelte`、`ConfirmButton.svelte`、`RollingNumber.svelte` | 快捷键录制、两步确认按钮、滚动数字 |
| `format.ts`、`settings.css` | 大小 / 数量 / 时间的显示格式；设置页共用样式 |

设置页的组件直接调 `ipc.ts` 里的命令，没有像面板那样的集中状态对象；设置保存后后端广播 `settings:changed`，面板和设置窗口都据此更新。

## 状态流向

```
后端 ──事件 panel:*──▶ ipc.ts ──▶ PanelState.connect() 里的处理函数 ──▶ $state 字段 ──▶ 组件重新渲染
组件的 DOM 事件 ──▶ PanelState 的方法（navigate、choose、keydown……）──▶ ipc.ts 的命令 ──▶ 后端
```

- 只有 `Panel.svelte` 创建 `PanelState`，以 `panel` 属性传给子组件。子组件只读状态、调方法，不直接调 `ipc.ts`。
- `Panel.svelte` 初始化时先 `panel.watch()`（登记搜索、选中项两个副作用，顺序不要换），再 `onMount(panel.connect)`（订阅事件、发 `frontend_ready`，卸载时清理）。
- 普通 `quick` 不激活，数字由键盘钩子经 `panel:key` 转给 `navigate()`；其原有搜索首键缓冲、两帧就绪和补发路径保留。权限较高或无法确认的目标先进入 `activation_pending`，取得面板前台后进入 `activated`，初始 `mode` 仍是 `quick`。
- `activated` 只用真实 DOM `keydown` / `keyup`，忽略 `panel:key` 防双发。后端放行新键，只保留历史吞键配对。确认键按下时固定会话、条目和模式；匹配可信抬起且已观察按键/修饰键全部松开后，才取用或关闭。方向、翻页键允许重复导航。搜索选择也在按下时固定原 `selection`，松键期间 revision 变化就取消，不刷新后重发。
- `interaction` 和本地 DOM 焦点共同控制能否操作。`activated` 先挂载同一个可编辑搜索框，确认 `document.hasFocus()` 且 `activeElement` 为该框后才展示列表和快捷键；准备超过 500 ms 或焦点失败时提供本地鼠标重试，不调用仅适用于 `activation_failed` 的后端重试。原生 `activation_pending` / `activation_failed` 无列表和搜索入口，失败重试仍走后端。`passive` 保留只读历史与预览，但 `filtered` 为空，组件展示的 `visibleItems` 不能用于取用；页脚显示后端提示和明确鼠标关闭入口。
- 原生提交等待采集访问失败且未写入时进入 `close_only`，显示未提交及重新呼出的说明，只保留关闭按钮和可信 Esc 配对关闭。旧会话的监听已结束，迟到的激活、模式、结果与错误不能重新开放取用；下一次 `panel:shown` 才初始化新会话。等待期间已请求关闭的取消，在 worker 完成后静默收起，不抢回目标焦点。
- Activated 的普通首字和输入法由浏览器在原框自然编辑；`beforeinput` / `compositionstart` 请求进入搜索，`searchStarting` 期间不处理 Quick 数字取用，后续字符继续留在原框。`/`、Ctrl+F 以空查询进入搜索。后端切换不重新激活、MoveFocus 或补发首字，前端也不二次聚焦；切换失败或 1 秒没有模式回执时显示重新呼出提示，不能永久停在准备态。
- 根内容只在本会话 `visible` 时渲染，`interactive` 同样依赖可见性；隐藏后不保留搜索、编号列表和快捷键提示，避免窗口先于下一次 `panel:shown` 显示时露出旧会话的就绪承诺。
- 搜索取用必须传实际 DOM 选择事件。`PanelState` 在第一处 `await` 前固定会话、条目、粘贴模式及 `selection {revision, nonce}`；`panel:key` 不能建立搜索许可。每个前台 revision 回调记录 `performance.now()`，只允许同一时钟下接收后生成的新可信事件，迟到事件提示重新选择。旧/重复 revision 不降低高水位，但把接收时刻推进到现在；非法 revision 暂停选择，需新合法通知或重新呼出。
- DOM 引用 `input` 由 SearchBox 用 `bind:this` 填进 `PanelState`。
- 按键有两条入口，语义必须一致：普通 `quick` 由键盘钩子经 `panel:key` 送到 `navigate()`（钩子只转发方向、翻页、Home/End、Enter 组合、Tab、Backspace、Esc、Space、Delete、1–7、Ctrl+D/Z 等；可打印字符触发搜索）；`activated` 和搜索模式由 DOM `keydown` 换算成同名动作再调 `navigate()`。新增按键要两边都接上。

## 设置窗口壳

- `settings.html` 与 `index.html` 是 Vite 的两个构建入口。通用页通过 `get_settings` / `set_settings` 持久化 `theme`、`feedback.motion` 和 `hotkey`；写盘及运行时换键完成后由后端统一广播 `settings:changed`。手牌没有布局或密度选项，旧 `panel.layout` / `panel.density` 只作兼容数据保留。
- `HotkeyEditor.svelte` 是按键录制框：点击后直接按组合键，`validate_hotkey`（校验规则的唯一来源）通过即保存并即时生效，Esc 或窗口失焦取消。录制期间调 `pause_hotkey(true)` 暂时注销当前呼出键（否则按到同一组合会被系统拦走），结束时恢复；后端 30 秒兜底自动恢复。`event.code` 为空（远程桌面、改键工具）时按 `keyCode` 补出键名。`get_hotkey_status` 只在出错、测试覆盖或暂停注册时展示。
- 后端换键先注销旧键再注册新键，失败不使用隐藏备用键；面板允许没有注册键，现有钩子只就地更新放行组合，不重置会话、吞键配对或焦点。离线及占用场景手测见 `tools/test-hotkey.md`。
- 窗口定义集中在 `settings_window.rs`，不在启动配置中创建第二个 WebView；调用异步命令 `open_settings` 才创建。重复打开会显示并聚焦已有窗口，关闭后销毁，下次重新创建。
- `lib.rs` 按 `settings` 标签放行关闭请求，关闭设置窗口不会走面板的退出分支。托盘已提供“设置…”入口；从菜单或托盘回调调用 `settings_window::open()` 时先用异步运行时切离主线程，避免 Windows WebView2 创建窗口时死锁。

## 主题变量

- v10 的纸张、牌面、三级墨色（`--hand-ink` / `-ink-2` / `-ink-3`）、朱砂、阴影（深浅两套）、共享状态色和动效常量统一放在 `theme.css`；字号只用 12 / 13 / 15 / 20；宋体（`--font-display`）只用于字标和印记；朱砂只用于选中键帽、常用印记和已选计数；窗口和 `app.css` 背景透明，只有牌和小件上色。`data-theme="system|light|dark"` 控制主题，系统 reduce 优先于动效设置。
- 原生窗口为 960×540（按 DPI 缩放）。前端把 `[data-hit]` 的客户区矩形通过 `set_hit_regions` 报给后端；后端只用 `SetWindowRgn` 把窗口裁成这些区域，区域以外窗口不存在，点击自然落到下层窗口。区域同时也是可见范围，所以上报时外扩 16px 盖住阴影。
- `set_hit_regions` 还带 `avoid`（避让外形，客户区 CSS 像素 `{left, top, width, height}`）：后端只按它挪窗口避开光标行，`regions` 只管点击。外形由 `cards.ts` 的 `fanOutline()` / `spreadOutline()` 给出，同一姿态固定不变，只在变化时发；拖牌期间不带；没有手牌画面时为 `null`。按键提示、看全、横幅、通知、已选清单这些临时件只进 `regions`，不进 `avoid`。
  - **扇面 `{left: 28, top: 27, width: 929, height: 502}`**（后端呼出时的默认外形用同一组数）。推导：手牌原点 `ORIGIN_X = 112`、`ORIGIN_Y = 13`，手牌顶边 `HAND_TOP = 13 + 32 + 212 + 28 = 285`。左 = 112 − 12（标签间距）− 72（“↑ 常用”标签宽度预留）= 28；上 = 285 − 30（拈起牌底边离手牌）− 212（拈起最大高）− 16（阴影）= 27；右 = 牌堆 828 + 104 + 9（错开的牌层）+ 16（阴影）= 957；下 = 285 + 216 + 24 + 4（位置条 / 回收站那一行及浅阴影）= 529。常用露头在手牌顶边上方 26px，已在其内。
  - **摊开 `{left: 96, top: 0, width: 648, height: 540}`**：底板 112, 13, 616 × 514 四边各加 16px 阴影，再裁到窗口 960 × 540 以内。
- 窗口优先放光标下方，其次上方；两边都放不下时侧移，尽量避开正在输入的行。逐张放的小指示优先放光标所在行右侧。

## 交互规则（11-07 交互优化）

- **两种姿态（v10.1）。** `pose` 为 `fan`（扇面，最近或常用的前 7 张）或 `spread`（同一批牌放平成一列）。搜索词非空时总是摊开；清空搜索（Esc 或 Backspace 删空）回扇面。HandPanel 以条目 id 为 key 复用同一个元素，扇面 ↔ 摊开补间 translate / rotate / 宽高（240ms，每张错峰 10ms），两层内容交叉淡入；减少动画时位置尺寸直接到位，只留 100ms 淡入淡出。
- **键位。** ← → 任何状态都是上一张 / 下一张（扇面第 7 张再按 → 摊开到第 8 条）。最近扇面 ↑ 切常用、↓ / PgDn 摊开；常用扇面 ↑ ↓ 都回最近，PgDn 摊开（超过 7 张时）。摊开里 ↑ ↓ 逐条，第一条按 ↑ 或 Home 收回，PgUp/PgDn 翻一屏（可见条数 − 1，首尾不动），End 最后一条。数字 1–7 由 `digitTarget()` 换算：扇面是第 n 张，摊开是看得见的第 n 条，键帽与取用同源；搜索中数字是输入，Ctrl+1–7 取用（Ctrl+8/9 只拦截）。搜索框为空时数字、空格、Backspace、Delete 回到扇面语义。滚轮：扇面向下摊开，摊开里逐条，第一条再向上收回。
- **Esc 逐层退出。** 看全 → 搜索（清空并收回扇面）→ 已选（清空，5 秒内 Ctrl+Z 恢复）→ 摊开（收回扇面）→ 收起面板。
- **本次呼出顺序冻结。** `panel:shown` 分别快照最近和常用的 ID 顺序；新复制、复用置顶及常用状态变化只更新内容或标记，不重排当前手牌，也不重新发起搜索；新来的条数（`arrivals`）只在位置条上提示“新来 N 张”，下次呼出才按新顺序。删除或文件失效会移除该牌。最近为空而常用有内容时落到常用。
- **悬停与忙时保护。** 扇面里悬停拈起，但只在真实 `pointermove` 之后（键盘换牌会清掉），busy、等待 DOM 松键、看全打开、拖动时忽略；摊开里悬停只加底色。已选的牌不响应数字和单击（给提示），删除时同步移出已选，Backspace 逐张放回。
- **拖出去。** 拖过 6px 开始拖动，命中区域临时扩到整个窗口；松回手牌 / 一列里算取消并弹回，松在外面才粘贴（后端目前只能粘贴到原光标处）；拖动过就吞掉随后的 click，`lostpointercapture` 复位。
- **提示只有一处。** 按键提示扇面在拈起那张旁边、摊开在一列底部，只列当前能用的键；按住 Ctrl / Shift 超过 300 ms（`heldModifier`）换成对应组合；有已选时换成一起粘贴 / 逐张放 / 放回。粘贴成功不出声；`message`、`notice`、撤销提示占位置条（扇面）或提示（摊开）的位置。
- **已选。** 空格多选，最多 32 张（到上限给提示）；Enter / Ctrl+Enter 一起粘贴，Ctrl+Shift+Enter 逐张放；Shift+Enter 只复制当前一张，有已选时不执行并提示。已选跨隐藏保留，可以带进搜索里一起粘贴（不要求出现在当前结果里），只有队列确认入队、Backspace 放回或 Esc 清空才减少。`start_queue` 返回的中文原因原样显示。
- **命中区域。** `Panel.svelte` 读所有 `[data-hit]` 的矩形（牌另带 `data-hit-target` 目标矩形，过渡期间取新旧并集），外扩 16px 盖住阴影，带会话号调 `set_hit_regions`；MutationObserver（DOM 一变、绘制前）、transitionend / animationend（过渡结束后收紧）、ResizeObserver、窗口缩放和 `resolution` 媒体查询都会重报，内容不变不重复上报。后端按上报的内容包围盒避让光标行。
- **Delete 进回收站，可撤销。** 快速模式按 Delete，搜索模式按 Ctrl+Delete（Delete 留给输入框删字）。`delete_item` 直接把条目移进回收站（库里的 trash 表，保留 7 天）；常用的条目要按住：钩子在第一下按下时送 `Delete:down`、松开时送 `Delete`（页面自己收到的 Delete/Ctrl+Delete 用 keydown/keyup），按住 600 ms 才删，提前松开或移开选择就取消，条目上显示进度和提示；普通条目按下就删。`undo_delete` 不带参数：后端记着本次运行里的删除顺序，Ctrl+Z 依次恢复，跨呼出有效。页脚的撤销提示只是提示，停留 5 秒（带倒计时条），新一次呼出时清掉。
- **启动预热。** `frontend_ready` 之前用真实历史隐藏渲染一次（`warming`），首帧不再付冷启动开销；呼出时 `visible` 优先。

## 不能动的细节（都是 G0 踩过的坑）

1. 新搜索请求只在 `search()` 里递增 `searchRequest`，不在 oninput 里加（否则刚发出的请求立刻过期，回复全被丢掉）；显示、隐藏和卸载时的递增仅用于使旧回复失效，保持原有行为。
2. 输入法组合期间不搜索：`composing` 故意不是 `$state`，组合结束时 `searchRevision += 1` 触发一次补搜。
3. 普通 Quick 进入搜索：聚焦搜索框后等两帧（两次 requestAnimationFrame）再发 `search_ready`，后端才补发首键。Activated 沿用已聚焦的同一输入框，握手期间不再 focus，后端只结束空缓冲流程，不补发或做会打断组合的焦点探针。
4. `onbeforeinput` 上报补发后的第一个输入是直接上屏还是进了输入法（只报类型和时序，不报内容）。
5. Ctrl+Enter 粘贴为文本（`paste_item` 的 mode 为 `text`）；输入法上屏后 50 ms 内的 Ctrl+Enter、Ctrl+1–7、Enter、Esc、方向和翻页键不处理。
6. `caret-color: transparent`：搜索框默认不显示光标，只有 `.active`（搜索态）才显示；普通 `quick` 下搜索框是 `readonly`、不持有焦点，按键由后端钩子转发。Activated 可编辑以承载自然首字，数字与提交键通过 `preventDefault` 保持快速取用语义。
7. 搜索结果高亮按 `searchedQuery`（已经拿到回复的那次搜索词），不按正在输入的 `query`。
8. M0 批准的激活式交互：粘贴结果带 `hint_dwell_ms` 时转为 `passive`，只显示后端 `hint_message` 和明确鼠标关闭入口，`busy` 保持到收起。没有消息时用中性的“已复制，请先点击目标输入位置，再按 Ctrl+V”，不猜管理员权限或原输入控件已恢复。归还前台后的被动提示不承诺吞键、Ctrl+V 或 Esc 立即关闭，也不凭提示结束宣称已粘贴。
9. M0-5 页脚版本号：启动时 `getVersion()`（`ipc.appVersion`）。
10. 全部事件订阅好之后才发 `frontend_ready`；每次 `panel:shown` 都重置选中项、`busy`、输入法状态，两帧后发 `report_painted`。
11. 条目按下时 `preventDefault`，不抢搜索框焦点；鼠标选择同样等待 DOM 已观察的真实按键/修饰键松开。普通 Quick 点搜索框请求 `start_search`；Activated 使用已聚焦原框，焦点未就绪则先本地重试。
12. 面板里禁用右键菜单。
13. 搜索提交忽略 `event.repeat`，输入法组合中和上屏后 50 ms 不取用；鼠标同样受保护。提交还检查 DOM 原始 `timeStamp` 与组合结束的间隔，防止排队超过 50 ms 后把防抖期内的按键算成新确认。同一个 DOM 事件只能使用一次，失败后必须有新事件，nonce 继续递增。
14. DOM 待发选择在失焦、隐藏、会话/模式变化时取消，每个 held code/修饰标记记录按下时刻，迟到的旧 keyup 不能清新证据。非组合的新 Esc 优先把待发选择替换成关闭，匹配 Esc 和其它已观察键全部抬起后收起；已 busy 的请求也允许关闭交后端取消。重复或组合中的 Esc 不关闭。没有按键释放的超时放行。

离线回归使用 `node tools/test-panel-key-repeat.mjs` 编译真实 `PanelState`；实机仍需覆盖呼出、激活成功/失败、打字搜索、中文输入、数字键、Enter、Ctrl+Enter、Esc 和被动提示。离线测试不能替代输入法和焦点实机验收。

M1-U alpha.3 的普通 quick 钩子补投 Space 与 C 的左右键；v10 继续由后端补投 Tab、Backspace、Ctrl+Shift+Enter，并都在抬起时投递，避免卸钩后把配对抬起漏给目标。Esc 不再由 Rust 转发后无条件收起，而是交给前端 `navigate("Escape")` 分层：有浮层只关浮层，无浮层仍走 `hide_panel`。因此普通 quick 无浮层 Esc 的 `hide_source` 日志来源现在是 `frontend`；搜索模式、粘贴、焦点与首键补发路径不变。

逐张放由 `start_queue` 建立后端队列，手牌隐藏后保留小指示与 `queue:changed`。`queue_hook` 只接管成对的 Ctrl+V / Esc，Ctrl+V 抬起后重新捕获当前目标，复用 `paste::execute` 写剪贴板并提交；其它键（包括 Tab 和呼出键）放行。普通单条 `paste_item`、前台许可、搜索选择和证据收集路径不改。
