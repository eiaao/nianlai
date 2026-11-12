# 后端结构（src-tauri/src 与 crates/nianlai-store）

这份说明讲后端各模块管什么、数据怎么流。前端见 [`src/lib/README.md`](../src/lib/README.md)。

## 一句话

`nianlai.exe` 常驻托盘：监听剪贴板变化，按规则过滤后写进 SQLite；按快捷键呼出面板，选中一条后把它写回剪贴板并粘贴到原来的窗口。

## 进程

- 启动的 `nianlai.exe` 先当**守护者**（`recovery.rs`）：不建窗口、不碰剪贴板，只用 `--child` 参数再拉起自己。
- 子进程才是真正的应用。它崩溃时守护者等 2 秒重新拉起，24 小时内最多 3 次；它以退出码 75 退出时立即重新拉起（快照恢复、覆盖导入靠这个重启）。

## 模块

| 模块 | 管什么 |
|---|---|
| `lib.rs` | 启动顺序、全部 Tauri 命令（前端能调的入口都在这里）；`set_settings` 的编排 `save_settings`（写盘、换键、广播 `settings:changed`、让采集规则等即时生效）；历史服务以 `State<History>` 交给命令 |
| `app.rs` | 应用总装对象 `Controller`：持有面板状态机、历史服务、采集服务、剪贴板写入线程和日志；`start_capture`（启动采集）、`copy_selection`、`shutdown` 在这里 |
| `recovery.rs` | 守护者：拉起、重启额度、退出码约定 |
| `settings.rs` | 读写 `settings.json`，叠加测试用的环境变量，给出数据目录、日志目录；`settings::save` 规定保存顺序（写盘 → 换呼出键 → 广播） |
| `directories.rs` | `Directories`：设置文件、数据、日志目录（纯数据，settings 解析，log 和 history 使用） |
| `log.rs` | 结构化日志（jsonl），后台线程写，按天和大小清理；不记正文 |
| `capture/` | 采集**规则**：排除应用、“不要记录”标记、敏感内容、暂停、临时内容 |
| `history/` | 历史服务（对外只有 `History`，内部是 `Arc`，可 Clone；启动时 `app.manage` 一份，Controller 和采集、剪贴板线程持有同一个）：写入、列表、搜索、收藏、回收站、文件失效检查、存储管理、快照、备份（磁盘库和内存库共用同一个 `Store`）；存储层错误由 `history::user_message` 翻成中文再交给前端（原始错误照常进日志） |
| `panel.rs` | 面板状态机（`Panel`、`Action`、`dispatch`）；`dispatch` 在主线程上从 `Controller` 取日志、剪贴板线程、历史服务等 |
| `paste/` | 粘贴**策略**：能不能粘、用哪种方式、怎么判断粘成了；不直接调系统接口。`outcome.rs` 是一次粘贴的有类型结果 |
| `shortcut.rs` | 呼出键字符串的解析、保留组合、冲突提示（纯函数，settings 和 hotkey 共用） |
| `hotkey.rs` | 全局快捷键的注册和更换，结果（成功、冲突、错误）记在状态里交给调用者；不管保存设置 |
| `tray.rs`、`settings_window.rs` | 托盘菜单、设置窗口 |
| `platform/windows/` | 唯一的平台实现（只支持 Windows，其他系统编译时直接报错）：剪贴板监听和读写、键盘钩子、窗口和焦点、粘贴执行、图片、来源图标、WebView 挂起 |
| `platform/windows/queue_hook.rs` | 逐张放期间的专用低层钩子：成对吞下 Ctrl+V（自己跟踪物理左右 Ctrl，注入的 marker 键不算），Esc 成对吞掉、抬起时取消；其它键放行；空闲超时兜底 |
| `platform/windows/window.rs` | 960×540 透明手牌窗口、按内容包围盒避开光标行的定位（`place_content`，回退顺序：下方→上方→侧边→贴边覆盖）、逐张放小指示，以及按窗口 DPI 换算前端矩形的窗口区域（`SetWindowRgn`）点击穿透；钩子线程不改窗口样式 |
| `autocycle.rs`、`samples.rs`、`resuspend.rs` | 测试和测量用：自动循环呼出、示例数据、隐藏后重新挂起 WebView |
| `crates/nianlai-store` | 存储：表结构和升级（`schema`、`migrations`）、写入和缓存（`write`，含内存库）、搜索（`search`）、快照（`snapshot`）、备份（`backup`）、内容和指纹（`content`、`hash`） |

## 三条主线

### 采集：复制 → 入库

```
系统剪贴板变化
  → platform/windows/capture（监听线程收到通知，读取线程读格式）
  → capture/rules、capture/sensitive（排除 / 标记 / 敏感 / 暂停，被挡的不读正文或读完即丢）
  → history::History::capture
  → nianlai-store 写入（一次事务：内容、格式、复制事件、搜索索引、最近 300 条缓存）
  → Action::HistoryChanged → 事件 panel:history-changed → 前端重新取列表
```

### 呼出：快捷键 → 面板

```
全局快捷键 → Action::Toggle → panel::dispatch（主线程）
  → Panel 装键盘钩子（platform/windows/hook）、定位并显示窗口、唤醒 WebView
  → 事件 panel:shown（带历史快照）
```

- 快速模式不抢焦点：按键由钩子截下，经事件 `panel:key` 转给前端。
- 搜索模式才激活面板窗口，之后用浏览器自己的键盘事件。
- 面板隐藏时 WebView 被挂起（`platform/windows/webview.rs`）省内存；挂起期间前端不处理任何调用。

### 粘贴：选中 → 回到原窗口

```
前端 paste_item → Action::Paste
  → paste/permit 检查许可（目标窗口还在不在、能不能粘、只复制的原因）
  → 工作线程 panel::run_paste（任务由 Panel::begin_paste 打包成 PasteJob）→ platform/windows/paste::execute：
      写剪贴板 → 把焦点还给原窗口 → 发按键或走原生提交 → 收集证据判断是否粘成
  → 返回 paste::outcome::Outcome（有类型的结论 PasteResult、原因 Reason、是否写入、是否取消；
    只给日志看的证据留在 evidence 里）
  → Action::PasteFinished → Panel::finish_paste 按枚举判断下一步 → 事件 panel:paste-result
    （日志行 paste 和前端载荷都由 Outcome 序列化，取值字符串由测试钉住）
```

### 逐张放：拈一叠 → 收起手牌 → 后续 Ctrl+V

```
前端 start_queue（顺序固定的一串 id）
  → Panel 建 PasteQueue，手牌换成小指示，queue_hook 只接管 Ctrl+V / Esc
  → 每次 Ctrl+V 抬起 → queue-paste worker 重新捕获当前目标（是拈来自己或没有前台则不写、可重试）
  → 为这次明确操作建立独立 quick 许可，复用 paste::execute 写剪贴板并提交
  → QueueFinished 弹出队首并广播 queue:changed；记录/文件不在了跳过这张接着放；
    Esc 取消（成对吞掉）、外部复制（剪贴板 owner/序号变化）停止并保留新内容、两分钟无操作停止；
    结果不明确时不自动前进；一起放下（automatic）的非末项文本加分隔（单行编辑框空格，其余换行）
```

普通 `paste_item` 仍完全走上图的单条路径。逐张放不修改单条许可或既有 PasteJob；每次后续 Ctrl+V 都是用户的新操作，失败不会改换既有粘贴策略。

### 命中区域与避让（`set_hit_regions`）

前端用 CSS 像素、窗口客户区坐标上报，后端按窗口 DPI（`GetDpiForWindow`）换算：

- `regions`：点击区域，只用于 `SetWindowRgn` 裁剪（区域外点击直接落到下面的窗口）；`armed` 为真但数组为空时按清除处理（整窗可点）。
- `avoid: {left, top, width, height} | null`：当前姿态的避让外形，只用于定位。只有收到非空且与上次不同的 `avoid` 才重新判断位置，判断结果不比当前好就不动；省略或 null（例如拖牌时上报整窗区域）绝不挪窗口。需要挪时约 120ms 缓出移动，不激活、不改层次，并发 `panel:placement {caret, placement}`。
- `session`（可省）：给出时只对当前会话生效；`scale` 旧参数仍接受但不再使用。

呼出时的默认避让外形（`window.rs` 的 `DEFAULT_AVOID`）与前端 `src/lib/cards.ts` 的 `fanOutline()` 逐值相同，96 DPI 下为 `(left 28, top 27, right 957, bottom 529)`，推导见 `src/lib/README.md`。改任一侧都要同步另一侧。已选清单、提示等临时元素不计入外形。

前端首次上报的扇面外形与它相同，不会触发移动。回退顺序：光标行下方 → 上方 → 侧边（不盖光标）→ 贴空间大的一侧覆盖；内容始终在工作区内，内容以外的透明部分可以伸出工作区。`panel:shown` 带 `caret`（光标行相对窗口的 CSS 坐标）和 `placement`。

### 逐张粘贴停止

`queue:changed` 停止时带 `remaining: 0`、`remaining_ids`（没粘贴的条目，原顺序）、`stopped`（原因名）、`error`、`skipped`、`dwell_ms`。异常停止（有 `error`）时 `dwell_ms = 3000`：小指示带着错误停留 3 秒再收起（之后才发 `panel:hidden`），停留期间不再接管 Ctrl+V / Esc；放完、取消、Esc、× 为 0，立即收起。Esc 在逐张粘贴期间成对吞掉、抬起时取消。

## 读连接

磁盘库的读（`Store::reader()`）从一个小连接池借只读连接，`Reader` drop 时归还：最多留 4 条空闲连接，空闲超过 45 秒的在下次存取池时关闭（没有后台线程），面板隐藏时 `History::release_idle` 立即关掉全部空闲连接（借出的照常归还）；压缩（VACUUM）、库关闭时池清空，清空前借出的连接归还时直接关掉；带着未结束事务的连接不放回池。内存库不走池，读写共用写入线程里的那条连接。完整预览的缩略图另有一个 8 张的小缓存（`history/preview.rs`，单张 data URL 超过 1 MiB 的不缓存），清除内容、导入备份、换库、面板隐藏时清空，同一内容补齐格式（合并到已有 id）时清掉那一条。

## 后台维护

- `history-check` 线程每小时跑一次：每天一份自动快照、清回收站里满 7 天的、检查文件类条目的文件还在不在、检查库完整性。
- 快照恢复和覆盖导入都不在运行中换库：只写一个待处理标记（`snapshots/restore-pending`），重启后在打开库之前由 `history/snapshots.rs` 的 `apply_pending` 应用，并先存一份“恢复前”快照。
- 库打不开或完整性检查失败时，历史退到内存模式（完整性检查失败时旧的磁盘 Store 收进 `retired` 持有到进程退出、不关闭，免得 SQLite 关库时做检查点改动被判损坏的文件）：还是同一个 `Store`，只是用 `Store::open_memory` 开在内存里（同一套表结构、迁移、搜索；读写共用写入线程里的那一条连接；最近 300 条、256 MiB 的上限由存储层的 `MemoryOptions` 执行）。还能复制、呼出、粘贴，但不落盘，面板会提示。会碰磁盘的功能（快照、备份导入导出、存储统计和清理）靠 `History::store()`（内存模式返回 `None`）拒绝，其余功能一律走 `History::db()`，不区分模式。

## 线程

| 线程 | 做什么 |
|---|---|
| 主线程 | Tauri 事件循环；所有 `Action` 都在这里由 `panel::dispatch` 处理 |
| `capture-listener`、`capture-reader` | 收剪贴板通知、读剪贴板内容 |
| `paste-clipboard` | 写剪贴板（粘贴时） |
| `paste-request` | 一次粘贴的执行 |
| `history-startup`、`history-check`、`history-deferred` | 打开库和应用待处理恢复、每小时维护、临时内容延迟入库 |
| `panel-log` | 写日志 |
| `source-apps`、`file-check`、`file-probe` | 刷新来源图标、检查文件是否还在 |

耗时的 Tauri 命令（搜索、导出导入、存储统计）都放到阻塞线程池跑，不占主线程。

## 读代码时容易看错的地方

- **`platform/windows` 不是纯粹的底层。** 它里面是采集循环、剪贴板线程、粘贴执行这些“服务的 Windows 实现”，会用到 `paste/` 的策略类型和 `history` 的内容类型；`paste/` 也会反过来调 `platform`。
- **`NIANLAI_G0_*` 环境变量是测试用的。** 名字来自样机阶段，现在由 `tools/` 里的脚本设置（隔离数据目录、示例数据、自动循环等）；日常运行不需要。
