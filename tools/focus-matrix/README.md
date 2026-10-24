# focus-matrix：G0 焦点矩阵探路工具

把 PRD 6.4 的重复焦点试验编排成可检查的日志与矩阵。每个场景、每种模式默认 30 次，工具自己核对目标内容、焦点、提交副作用，再与样机的粘贴结论交叉核对。它不使用系统剪贴板 API；真跑时由样机写入剪贴板。

本版已做编译、静态检查、纯逻辑测试和只读空跑。**没有验证任何真实输入、窗口创建、UIA 读写或清理行为。先离开电脑做小规模冒烟，再扩大范围。** 空跑“就绪”仅表示可见的前置条件满足，不代表焦点矩阵通过。

## 真跑前准备

1. 自行构建正式样机：在项目根目录运行 `pnpm tauri build --no-bundle`，确认 `target\release\nianlai.exe` 存在。工具不会代为构建样机。
2. 退出所有 nianlai、记事本和资源管理器文件夹窗口。Win11 记事本、Explorer 会复用现有窗口的标签页；工具不会操作旧窗口。
3. 把输入法切到英文。搜索首字要用当前键盘布局的普通虚拟键触发样机搜索，中文组合输入可能让首字尚未成为文本。
4. 需要第二屏验证时接好第二块显示器。关闭自动锁屏，确保电源、锁屏策略和远程连接不会中断试验；工具会请求系统保持唤醒和显示。
5. 保存工作，处理完可能弹出的系统提示，离开电脑。**样机粘贴会覆盖原剪贴板内容，工具不会备份或恢复剪贴板。**
6. WPS 示例默认禁用且未验证。启用前必须关闭全部 WPS 文档，修改场景中的实际程序路径和 UIA 条件。

## 命令

在仓库根目录的 PowerShell 中运行。以下真跑命令由用户在空闲时自行执行。

```powershell
cargo build -p focus-matrix

# 默认就是只读空跑；不会创建 --out 或临时目录
cargo run -p focus-matrix -- --dry-run
cargo run -p focus-matrix -- --dry-run --exe D:\project\nianlai\target\release\nianlai.exe

# 用户离机后先做 2 次 × 2 模式的基准冒烟
cargo run -p focus-matrix -- --real-input --only helper --trials 2 --exe D:\project\nianlai\target\release\nianlai.exe

# 2 次不会轮到 Esc；4 次能覆盖数字、Enter、数字、Esc 的完整轮换
cargo run -p focus-matrix -- --real-input --only helper --trials 4

# 验证浏览器普通输入框
cargo run -p focus-matrix -- --real-input --only chrome-input --trials 4

# 全部默认场景，明确指定新的结果目录
cargo run -p focus-matrix -- --real-input --trials 30 --out target\focus-matrix\my-run

# 自定义场景文件，只读查看 JSON 预检报告
cargo run -p focus-matrix -- --dry-run --scenarios tools\focus-matrix\scenarios\default.json --modes quick,search --json
```

| 参数 | 默认值与含义 |
|---|---|
| `--dry-run` | 默认模式。只读场景、文件元数据、进程名、窗口类名、显示器和键盘布局 |
| `--real-input` | 明确启用真实输入；与 `--dry-run` 互斥 |
| `--exe` | 相对当前目录的 `target/release/nianlai.exe` |
| `--scenarios` | 不指定时用编入程序的 `scenarios/default.json` |
| `--only` | 逗号分隔的 ID；可显式选择默认禁用的场景；未知 ID 报错 |
| `--modes` | `quick,search`，执行时始终搜索优先交替；可只选一种 |
| `--trials` | 每场景每模式 30 次，范围 1–1000；不会用额外重试掩盖失败 |
| `--hotkey` | `Ctrl+Shift+F12`；与退出键/采集调试键冲突时报错 |
| `--out` | `target/focus-matrix/<Unix毫秒>`；真跑拒绝覆盖已存在的目录 |
| `--countdown` | 10 秒，范围 5–300；中止机制在倒计时期间已生效 |
| `--max-minutes` | 120 分钟，范围 1–1440，包含倒计时和准备阶段 |
| `--keep-temp` | 保留本次临时文件、浏览器独立用户目录和样机数据目录 |
| `--json` | 空跑输出 JSON；真跑标准输出最终 summary JSON，进度输出到 stderr |
| `helper-window` | 内部子命令，必须有匹配的子进程环境标记，勿手工启动 |

退出码：0 = 空跑全局就绪且没有场景阻断 / 所有选中自动格通过；1 = 阻断、失败、中止或有效次数不足；2 = 参数或场景格式错误。单屏导致的第二屏跳过本身不阻断空跑；真跑汇总仍把该格列为未完成。

非 Windows 可以编译和使用纯逻辑校验/测试；运行预检会明确提示只支持 Windows，返回 1，不走 Windows 实现。

## 自动覆盖

| PRD 行 | 默认 ID | 快速取用 | 搜索模式 |
|---|---|---|---|
| 1 | `helper`、`notepad`、`chrome-input`、`chrome-textarea`、`chrome-editable` | 必须通过 | 必须通过 |
| 2 | `chrome-address`、`chrome-bookmark` | 必须通过 | 尽力，失败须正当降级 |
| 3 | `explorer-rename` | 必须通过 | 尽力，失败须正当降级 |
| 5 | `wps-writer`（默认禁用、未验证） | 必须通过 | 必须通过 |
| 8 | `edge-fullscreen`、`chrome-second` | 必须通过 | 必须通过 |

`helper` 是独立进程中的原生多行 EDIT；窗口激活时把焦点交给 EDIT，通过标题暴露长度、哈希、收到的 Enter/Esc/数字/普通字符、失活计数和焦点状态。准备时发内部清空消息，不用键盘清空。

网页包含 form/input、textarea 和 contenteditable，数据和计数在 `document.title` 中。提交会被拦下并计数；使用 `window.name` 保留加载次数；三字段的长度/哈希、Enter/Esc/数字/普通字符、blur/focus、活动字段也都记录。浏览器窗口标题后面附加的产品名不影响解析。

地址栏用 Ctrl+L，收藏夹用 Ctrl+D。收藏夹必须出现新的同进程前台顶层窗口，否则报 harness_error，不把主窗口误认为气泡。Explorer 只认新出现、类名匹配且标题含专属目录标记的窗口；准备时用 UIA 确认焦点文件，再 F2。优先读取名称框的 TextPattern 选区来判断扩展名；仅有 Value 时，只接受值精确等于本次文件名或其 stem，再按 Explorer 的 F2 规则推导后缀，其他情况报工具错误。核对后在相同名称控件仍聚焦时 Esc 取消；若文件名已改变，记误提交，并仅在本次目录里恢复原名。

全屏是浏览器 `--start-fullscreen`，会核对窗口矩形是否覆盖所在显示器。第二屏是通用 `second_monitor` 修饰项：移动目标并用带注入标记的鼠标输入移动指针，记录目标、面板和鼠标所在显示器；只有一块屏幕时跳过。

## 场景文件

完整示例见 [scenarios/default.json](scenarios/default.json)。未知字段会报错，避免拼写错误悄悄改变试验。所有场景（包括禁用条目）都参与结构和内容校验；只有被选中的场景参与可执行文件和独占状态预检。

顶层字段：

- `version: 1`。
- `timeouts`：`startup_ms=30000`、`shown_ms=3000`、`search_ms=3000`、`paste_ms=6000`、`settle_ms=1500`、`trial_ms=20000`。试验总时限覆盖准备、发键、UIA 和等待；启动单独计时。目标窗口认领等待最多 15 秒。
- `scenarios`：场景数组。

每个场景：

| 字段 | 含义 |
|---|---|
| `id / name / row / enabled / note` | 唯一 ASCII ID、中文名、PRD 行号 1–8、默认启用、备注 |
| `requirements.quick/search` | `required` 或 `best_effort`；helper 两种模式都必须 required |
| `target.kind` | `helper / file / web / address / bookmark / explorer` |
| `target.program / args` | 程序完整路径、参数数组；不用 shell 拼接执行 |
| `target.process / window_class` | 进程文件名和顶层窗口类；工具还会核对新窗口、PID 和本次随机标记 |
| `target.control` | `{"class": null, "control_type": null}`；UIA 焦点控件可选限制 |
| `target.address_control` | 浏览器旁路地址栏的 UIA 条件；默认 `OmniboxViewViews / 50004`；默认场景显式写出，可按本机版本调整 |
| `target.field` | 网页 `input / textarea / editable`；其他目标用空字符串 |
| `target.exclusive / single_line` | 已有同名进程时阻断此场景；禁止多行示例进入单行控件 |
| `target.fullscreen / second_monitor` | 全屏核对 / 第二屏定位 |
| `prepare / cleanup` | 少量可校验动作，见下文 |
| `expected` | 支持 `{before}{pasted}`、`{pasted}`、`{pasted}{before_ext}` 等模板，不重复解释插入值里的花括号 |
| `samples` | 如 `[{"sample": 11, "query": "sample item 11。"}]`，示例正文直接复用样机源码 |
| `quick_digits / search_digit` | 1–9 的快速数字轮换；唯一搜索结果只能配置 `search_digit: 1` |

`program` 支持 `%WINDIR%`、`%LOCALAPPDATA%`、`%ProgramFiles%`、`%ProgramFiles(x86)%`。参数占位符只有 `{file}`、`{dir}`、`{profile}`、`{url}`、`{token}`。路径均来自本次临时目录。默认浏览器参数包含独立 `--user-data-dir`、DNS 禁用映射、不可用本机代理、禁止同步/扩展/后台网络、禁用首次运行及默认浏览器提示和强制可访问性；缺少隔离参数时校验失败。全屏条目还须有 `--start-fullscreen`。

动作集合：

```json
[
  {"action":"focus"},
  {"action":"clear"},
  {"action":"key","chord":"Ctrl+A"},
  {"action":"wait","ms":100}
]
```

`key` 只允许 Ctrl+A、Delete、Ctrl+L、Ctrl+D、F2、Esc、Ctrl+S、End、Home，单步 wait 最多 3000 ms。Esc 清理还须在书签/重命名场景且原控件仍然聚焦；不会把 Esc 随便发给普通目标。启动文件型目标默认创建空白的带随机标记 .txt；记事本每次清理先 Ctrl+S 保存自己的文件。请勿把此探路工具配置成操作用户原有文件。

搜索词首字符必须为小写 ASCII 字母，在“前 300 字预览的连续子串匹配”和“按空白切词的 AND 匹配”两种语义下都唯一命中预期示例。文件名还检查非法字符、控制字符、保留设备名、尾随点/空格和 120 个 UTF-16 码元的上限。配置了单行控件的场景拒绝含换行的示例。

## 试验与历史顺序

每轮先搜索、再快速（只启用一种模式时只跑该模式）。每种模式按 **数字、Enter、数字、Esc** 四次循环，即完整循环 50% 数字、25% Enter、25% Esc；30 次时是 15 次数字、8 次 Enter、7 次 Esc。搜索数字使用 Ctrl+1。

每轮搜索词从 `samples` 循环取；首字用目标线程布局的 VkKeyScanExW 映射，余字用 Unicode 输入。等待匹配会话的 search_ready 后，读取名称为“搜索预览和文件名”的搜索框。读到却不相等时只收起面板，记 harness_error，不发送选择键；接口不可读会记入 notes 后继续，仍须通过最终内容核对。

工具不把 `record_id` 当作示例编号，也不假设最初哪条在首位。只有从已完成试验的实际内容与 paste 日志建立对应后，才更新已知 MRU。快速 Enter/数字 1 取最近已知条目；数字 N 只有已知且适用于当前控件才使用，否则回退 1 并注明。连第一条也未知时，会额外启动原生 helper 做搜索预热；预热 `trial=0`，不计入格子的 N 次。预热失败不会猜测历史。日志里出现带 `record_id` 的 `capture` 事件（别的程序复制了内容，样机把它排到了最前）时，最前一条变为未知，下一次快速取用前同样先预热。

每次开始记录原生焦点句柄、UIA 元素及内容摘要；SetWinEventHook 记录前台切换。快速取用要求目标一直前台且失活计数不增长；搜索要求目标 → 面板 → 目标。发选择键前再次确认面板可见；日志必须匹配本轮 shown 的 session。paste 日志等完、观察完整 settle 窗口后才进行下一轮。paste 等待超时会终止后续试验，避免尚未结束的发键污染下一轮。

内容换行统一成 LF。哈希为 32 位 FNV-1a，**每个 UTF-16 码元一次异或和乘法**，不是 UTF-8 字节。网页与 Rust 实现有共享向量核对；标题只包含工具生成的标记、计数、长度和哈希。

## 中止与安全边界

- 默认空跑不发输入、不装钩子、不创建窗口或进程、不写文件；检查进程/窗口时不读取用户标题或内容。
- 真跑的独立线程监听物理键盘/鼠标。鼠标移动累计超过 8 像素中止，鼠标按键和滚轮立即中止；忽略带注入标志的事件。**面板的钩子可能先吞掉物理键盘，动一下鼠标最可靠。**
- Ctrl+C、输出目录出现 `STOP` 文件、总时限、前台持续为空 2 秒（面板收起、气泡关闭时会短暂为空，不算）、SendInput 未完整成功、连续三次前台不符合预期，都会中止。UIA 单次调用还有 1.2 秒主线程硬等待预算，超出则整体中止。
- 中止后只补抬起工具可能按住的修饰键，不再发 Esc、保存键或退出键。之后关闭已认领窗口、清理自己的进程和临时文件。
- 每次 SendInput 批次前确认前台句柄及 PID 属于目标或工具启动的样机；外来前台只记类名和进程名。一个组合键的按下/抬起一起入队，避免面板在 Enter 按下时关闭导致抬起丢失。
- 工具标记为 `0x4E4C464D`，不同于样机的 `0x4E4C4730`。不能改成样机标记，否则样机钩子会放过工具的选择键。
- 聚焦优先 UIA SetFocus，再尝试 AttachThreadInput + SetForegroundWindow，最后在已核对所属窗口的控件中心点击。不会按 Alt 抢前台。面板开着时不点击。
- 样机、浏览器、helper 和普通 file 程序通过 CreateProcessW 挂起创建，加入 KILL_ON_JOB_CLOSE 作业后再恢复。工具崩溃时作业关闭，收走其中进程。记事本、Explorer 的转交启动器不加入作业，避免误终止 shell；只认领新窗口和本次标记。
- 正常完成时尽量在仍属于工具的前台发送 Ctrl+Alt+Shift+Q；超时才终止该样机 PID。所有进程终止通过保留的自建/认领 PID 对应句柄执行，避免 PID 重用；不按进程名结束，**绝不终止 explorer.exe**。
- Explorer 只发送 WM_CLOSE。记事本正常试验先 Ctrl+S；中止时不保存，只关闭本次窗口，新出现且已确认所属的 PID 必要时才终止。
- 样机环境设置 `NIANLAI_HOTKEY`、`NIANLAI_G0_SAMPLES=1`、本次专属 `NIANLAI_G0_DATA_DIR`，去掉 `NIANLAI_G0_AUTOCYCLE` 和 `NIANLAI_MEMORY_TIER`。需要样机四及以后的版本：样机三会忽略数据目录变量，把日志写到全局位置，工具读不到。
- 读本次数据目录里的 `panel-log.jsonl`（样机四起日志跟着数据目录走），只读、不清空、不改写。新半行等待补齐；日志被截断时停止匹配。
- 所有测试资料在 `%TEMP%\nianlai-focus-matrix-<时间>-<pid>-<随机标记>\`。删除前核对规范化路径的父目录和前缀；`--keep-temp` 保留。输出报告留在 `--out`。
- Per-Monitor V2 DPI 感知，坐标使用物理像素。保持唤醒不等于绕过企业锁屏策略，也不能保证在系统崩溃/断电后执行清理。

## 结果文件和判定

`results.jsonl` 每完成一轮即追加、flush 和 sync；`summary.json` 是结构化汇总；`summary.md` 包含 8 行 × 2 模式矩阵、各场景有效次数/分类计数、呼出和选中到结果耗时的中位数/P95（最近秩法）；`run.json` 保存环境（不含整机的进程和窗口列表）、参数、样机元数据、场景源、起止时间、中止原因；`nianlai-log.jsonl` 是本次增量日志。时间采用 Unix 毫秒。

| 分类 | 含义 |
|---|---|
| `pass` | 内容、焦点、切换序列和提交证据均满足要求 |
| `degraded` | 样机报告 copied_only 且写剪贴板成功、带原因；可观测目标未变化，无误提交/漏键/旁路写入 |
| `wrong_paste` | 粘错条目、粘到别处、重复粘贴或面板应吞掉的字符泄漏 |
| `wrong_submit` | Enter/Esc 泄漏、提交/跳转/重命名生效或非预期取消/关闭 |
| `focus_lost` | 原控件焦点、原生句柄或规定切换序列不符合 |
| `claim_mismatch` | 样机 sent/confirmed 但内容未进入，或 copied_only/copied 却改变了内容等结论不符 |
| `timeout` | 本轮等待或总预算超时 |
| `harness_error` | 工具准备、UIA、窗口认领或清理不足，不记为样机有效试验 |
| `skipped` | 该场景前置条件不满足，例如单屏或独占目标已打开 |
| `aborted` | 物理输入或其他全局中止导致试验结束 |

同一轮可能有多项异常，`result.flags` 会保留它们；主分类优先误提交、误粘、结论不符、焦点问题。不要只看主分类而忽略 flags 和 cleanup_error。

“必须通过”要求有效次数达到 N 且全部 pass。“尽力”要求有效次数达到 N，只有 pass 或有充分证据的 degraded，且无误粘、误提交、结论不符；focus_lost/timeout 本身不等于正当降级。有效次数不含 harness_error、skipped、aborted；不足列“未完成”。任何一次（包括随后被中止的那次）的 flags 里出现误粘、误提交或结论不符，这格就直接“不通过”。没有选中自动场景的格列“手工”。预热不计数。一次失败不会靠加跑覆盖。

报告不写完整目标文本，主要记录示例编号、长度、哈希和工具生成的内容。失败片段最多 200 个字符；未知实际内容只留摘要，不把可能非示例的文字写入报告。

## 手工部分

- 第 4 行微信、QQ、钉钉、飞书需要真实账号，漏回车可能真的发送消息。
- 第 5 行 Word/Excel 未安装；WPS 可能带着用户文档，编辑器的 Value/Text 接口也不稳定。默认保留手工，可参照 file 场景添加，但先在临时文件上验证读取接口。
- 第 6 行远程桌面需要远程主机和账号；当前在 RDP 中运行工具只会提示，不会代建远程会话。
- 第 7 行管理员程序需要 UAC 和权限隔离测试；工具不代点 UAC，也不自动提权。
- 第 8 行的独占全屏游戏不能用浏览器全屏替代；第二屏必须有硬件，跨 DPI 缩放也需要实测。

手工每格同样记录 30 次：目标程序/版本、模式、示例编号、按法、样机 session、预期与实际摘要、焦点是否恢复、是否提交、样机 paste_result/reason、最终分类和中文说明。请使用隔离账号或草稿，保留失败证据；自动格通过不能抵消手工格缺失。

## 实现与已知限制

代码分为纯逻辑的 CLI/场景/协议/日志/判定/报告/预检，以及 `win/` 中的观察、输入、中止、进程、UIA、目标、helper 和运行编排。示例通过 `#[path = "../../../src-tauri/src/samples.rs"]` 直接复用。依赖仅 serde、serde_json、Windows 目标上的 windows 0.62；没有另设 target 或 Cargo 配置。

Windows 真跑、所有 UIA 条件、浏览器书签窗口形态、记事本标签转交、Explorer 选区接口、全屏地址栏的可访问性、跨屏和焦点恢复均未实测。普通文件型应用没有可植入的按键/提交计数，证据主要来自内容和焦点；原生 helper 和测试网页的计数更完整。未知或不足证据不能当作通过。

浏览器的 DNS/代理/禁用后台网络参数用于阻断测试导航，不是系统级防火墙。系统通知、安全桌面、锁屏、其他注入软件和 UIA 提供者卡死仍可能打断运行。正常崩溃退出由 Job 收走所属进程，但无法对未认领的转交窗口或断电后的残留作出保证；遇到认领失败不会去关用户窗口。UIA 过慢、文本值不匹配或提供者行为偏离原生 F2 规则，都可能使本工具无法完成验收，应先调整/验证目标配置。

只读空跑不申请写权限、不试注册快捷键，所以无法预判 ACL、快捷键冲突、被安全软件拦截或启动后的前台争抢。请保留第一次冒烟的输出和临时目录检查；不要把本探路版的未实测代码当成 G0 已验收结论。
