# 管理员提示验收目标

本工具是被动 WinForms 文本目标，不是输入驱动。它不调用 SendInput，不读取剪贴板，不激活其他窗口，只记录自身文本框收到的按下/重复/抬起，以及自身文本的长度和 SHA-256。可以用真实键盘完成管理员和长按场景，并从文件取证。

## 构建与无窗口自测

```powershell
pwsh -File tools/hint-target/build.ps1
```

输出在 `release/hint-target/HintTarget.exe`。脚本只编译并运行 `--self-test`，不弹窗口、不发键。可以通过 `-OutputDirectory` 选择独立产物目录，保留旧候选。

自测包含 7 项事件配对记账/文本校验、7 项原生编辑消息观测内存自测及 12 项文件测试。内存自测检查前后摘要、调用计数、WM_PASTE 与 EM_REPLACESEL 两类记录分离、不保存正文及双记录用例重置，不构造控件或真实调用这两种消息。文件测试用真实 Windows 句柄检查未共享 DELETE 的短暂/永久占用、共享 DELETE 的读取者、只读目标和追加失败；另用注入错误检查替换已经提交却报错、部分替换丢失路径、目标无法确认等结果不确定性。自测不能代替真实输入验收，也没有实测 GUI 故障退出流程。

## 文件读取与故障判定

快照先写同目录独立临时文件，再用 `File.Move` / `File.Replace` 原子发布；替换后核对目标完整字节。文件操作失败时最多尝试 9 次，每次间隔 25ms（总等待最多 200ms，文件操作自身耗时另计）。替换报错也先检查目标是否实际完成；只有目标完整内容可确认才算成功。不会退回到直接覆盖现有快照。

读取 `ready.json`、`state.json` 和 `events.jsonl` 的文件流必须使用 `FileShare.ReadWrite | FileShare.Delete`，并及时释放。普通 `File.ReadAllText` 的默认共享方式缺少 DELETE，会在读取期间阻止 Windows 替换。事件文件是追加格式，最后一行尚未以换行结束时不能作为完整事件集验收。

**输出目录中只要存在 `failure.json`，整轮就无效，即使该文件无法解析。** 读取证据前后都检查该标记。任何快照或事件追加失败后，目标停止后续记录和计时，不再主动写 `phase=closed`，安排退出并返回退出码 1；事件追加不盲重试，避免部分写入后产生重复。故障标记包含 `schema=1`、`phase=failed`、`run_id`、`case_id`、时间、PID、`operation`、异常类型和 HRESULT，以及底层 `cause_type` / `cause_hresult`，不含文本正文。若目录权限等原因使标记也无法写入，仍保持失败状态并向标准错误报告；不能仅凭旧的 running 快照判定成功，还要检查状态新鲜度、进程存活和结束结果。`closed` 本身的发布也可能处于无法确认的失败中，因此最终验收需要确认正常 closed、PID 已结束及退出码 0；无法取得正常退出结果的轮次不能仅凭文件判为通过。

记录故障通过代码守卫封闭并延后关闭窗口；未处理回调异常配置为向 Main 传播，避免 WinForms 默认异常对话框。此路径目前只有源码审查和无窗口记录层测试证据。`elapsed_ms` 是消息处理时刻；快照重试可能短暂延后消息处理，不能把它当成物理按键计时器。

## 实机时使用

每个实例的 `--output` 必须是**不存在的新目录**，工具负责创建它。不要重复使用旧目录。普通目标正常启动，管理员目标用 Windows 的“以管理员身份运行”启动；产品保持普通权限。需要看见和操作的测试窗口以正常窗口方式启动，不使用隐藏方式。

```text
HintTarget.exe --output D:\test-runs\新一轮\normal-target
HintTarget.exe --output D:\test-runs\新一轮\admin-target
```

启动后核对 `ready.json` 的新 `run_id`、PID、`elevated` 和时间，随后确认 `state.json` 的时间持续更新。每个用例先点击“开始新用例”，记录新的 `case_id`；它清空本目标文本、配对记账和两类原生编辑消息记录。用例中间不要重置。

- `events.jsonl`：本目标收到的按键和时间，`repeat` 对应 Windows 消息中的上一状态位；`matching_down=false` 表示本用例未观察到对应按下，不自动等同产品错误，呼出快捷键、焦点变化也需结合判断。
- `state.json`：当前文本 UTF-16 长度、UTF-8 SHA-256、前台是否本目标、收到但尚未配对的键、事件计数。`observed_held` 不是物理键盘状态，被产品吞掉的键不会出现在这里。
- `native_paste`：本 case 内的 WM_PASTE 次数 `count`、单调时钟 `timestamp_frequency_hz` 及最后一次 `last`。last 包括 `call_index/completed`、紧邻 base.WndProc 前后的 `before_timestamp_ticks/after_timestamp_ticks`，以及 `before_text_utf16_length/before_text_sha256_utf8/after_text_utf16_length/after_text_sha256_utf8`。尚未收到消息时 count=0、last=null；开始新用例会清零，计数不影响键盘 events 协议。
- `native_replace`：本 case 内的 EM_REPLACESEL（`0x00C2`）观测，字段与 `native_paste` 相同，但独立计数和保存最后一次摘要。`native_paste` 仍只统计 WM_PASTE；一次替换不会增加其次数或覆盖其摘要。开始新用例会同时清零两类记录。
- 单次实例最多记录 4096 个按键事件，超出后标记 `truncated`，不能将截断证据判为通过。30 分钟后自动关闭。
- 关窗口或在输出目录创建 `STOP` 文件可结束；`phase=closed` 表示正常关闭。读取方还应确认 PID 已结束，不能只依赖文件。
- 默认仅记录文本校验，不保存文本正文。测试只粘贴产品自带的虚构示例。

WM_PASTE 与 EM_REPLACESEL 的前后观测只在自有 TextBox.WndProc 中更新各自内存记录，每次消息恰好调用一次 base.WndProc，不读取或解引用 EM_REPLACESEL 的 lParam，不新增文件 IO、不显式访问剪贴板或改变焦点。由原有定时/用例/关闭 Snapshot 经 EvidenceOutput 发布。顶层 `text_utf16_length/text_sha256_utf8` 来自 Snapshot 当次一次读取的当前 Text，其 `text_observed_timestamp_ticks` 与两类记录使用同一 Stopwatch 时钟；它们不能代替各自 last.after。若同 case 的 completed=true 且 after 长度非零，而后续当前长度变零，说明该消息返回后到快照间文本又改变了，不能再推断为“从未插入”。若 after=0，只能说该消息返回时未观察到文本；无法排除调用内部瞬时变化。每类 count=0 仅表示未观察到该类消息，不排除其他编辑路径；各自仅保留最后一次，不能从它还原全部中间变化。completed=false 表示观测尚未完整，不能用于插入结论。时间戳用于单调排序/差值，除以 frequency 得到秒，不是 UTC 或物理按键时刻。

此目标没有请求文件或外部发键接口，解决了前一版夹具复用旧请求、旧响应的问题；它**没有**解决当前 computer-use 接口无法操作管理员窗口和无法分离按下/松开的限制。补测需要真实键盘参与，或另一个经验证确实具备这些能力的受支持输入接口。不得把构建成功或普通窗口按键成功当成管理员输入能力已验证。

## 补测顺序

按本轮验收计划执行指定尚未通过的用例，无需单独按 V。已通过格按对应产品版本、目标工具版本和原始证据保留，不默认重测。

1. 使用本轮计划指定的构建和配置，记录产品版本、目标工具版本、目标 `run_id/case_id`、产品 session 和输入方式。
2. 重置前保存完整原始 `state.json`（含 `native_paste` 和 `native_replace`）、目标事件、产品日志和用户报告，核对对应消息返回时的 after 与快照当前文本。
3. 仅执行本轮计划指定的未通过用例；未执行的格保持待验，工具更新本身不改变已通过格的结论。

工具不会替人确认“确实物理按住了 3 秒”，也不会把目标记录为空自动判为吞键成功。
