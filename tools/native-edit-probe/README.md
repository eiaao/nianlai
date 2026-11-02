# NativeEditProbe：独立跨进程标准 Edit 夹具

一个 .NET Framework C# EXE 的 driver/child。它不接入产品，不读写系统剪贴板，不发送输入，不查询/修改用户窗口或焦点，不要求日常版退出。工具默认不执行 live；`--self-test` 仅做纯数据判断。**当前交付只编译和纯自测，`--run` 未运行。**

源码在本目录；编译产物和记录写到调用者指定的输出目录，例如：

`<输出目录>\probe-build`

```powershell
# 从本仓库根目录运行；输出目录由调用者显式指定。
pwsh -NoProfile -File tools/native-edit-probe/build.ps1 -OutputDirectory '<输出目录>\probe-build'
```

脚本用 Framework64 `csc.exe`、`/warnaserror /codepage:65001 /optimize+ /platform:x64 /target:exe` 和系统 WinForms/Drawing/Web.Extensions 依赖编译，再只执行 `--self-test`。manifest 绑定源码/EXE/编译和自测摘要；已有 manifest 时拒绝覆盖。没有安装依赖或修改 Cargo。

## 后续真实夹具入口（本轮不运行）

由父任务静态审查通过后才安排：

```powershell
& '<输出目录>\probe-build\NativeEditProbe.exe' --run --output '<输出目录>\probe-run'
```

输出须为绝对、规范、尚不存在的新目录。driver 顺序运行下列8个短固定用例，首个失败后停止；不要求任何键盘操作。

| 用例 | 独立检验 |
| --- | --- |
| raw-caret | 原生单行 Unicode Edit，中文/emoji caret插入与一次真实undo |
| forms-selection-crlf | WinForms 多行 TextBox，选区替换/CRLF、尾部caret与一次undo |
| raw-limit-exact | 5单元上限内恰好容纳“中😀”，无截断与undo |
| forms-limit-refused | 容量不足，预检拒绝且mutation_count=0 |
| forms-readonly-refused | 明确只读，预检拒绝且mutation_count=0 |
| raw-password-refused | 密码样式/掩码检查拒绝且mutation_count=0 |
| raw-timeout-after-entry | 已入WndProc后100ms超时，Unknown，释放门闩后迟到完成恰好一次 |
| forms-early-reply | 先ReplyMessage后等待；sender已返回并释放buffer后，原Edit过程仍正确处理恰好一次 |

容量查询仅适用于本夹具短文本，总长度不超过32767。EM_GETSEL 用无输出指针的16位打包返回值；不支持大范围，也不把它伪装成32位选区。

## 隔离与生命周期

- 每例是新的自有子进程、真实原生 Edit 或 WinForms TextBox。root Form 创建句柄但不显示，带 NOACTIVATE/TOOLWINDOW；Application.Run 使用无 MainForm 的 ApplicationContext，无 Show/Focus/Activate 调用。driver 在任何消息前绑定 ready 中的准确 root/target HWND、PID/TID，并确认两者均隐藏、同一自有子进程。子进程只查询自己的公开样本文字。
- ready 由消息循环实际分发的首个callback发布。raw单行Edit显式使用ES_AUTOHSCROLL，避免200px夹具宽度带来无关的限字；metadata记录该位，这不是产品新增资格或通用行为保证。
- 只有 driver 向自有子进程发一次 EM_REPLACESEL(TRUE)。Unicode buffer 持有至 SendMessageTimeout 返回，随后清零/释放；然后才释放延迟接收器。接收器在门闩前只观察控件已有文字，**不读取或复制lParam**，门闩后把原 Message/lParam 交给原Edit过程。无异步指针消息，无同进程跨线程payload测试冒充跨进程证明。
- Mutation没有重试或其他输入fallback。返回非零只记 `sent_unconfirmed`，超时记 `unknown`；夹具是否通过另看接收器前后正文SHA256/长度、selection、一次计数和真实退出码。undo仅作用于新建的自有控件；准备阶段可清空这些新控件的undo，不影响用户控件。
- stdio仅传协议/公开样本摘要。driver保留原Process句柄、释放门闩后请求child正常退出并收真实exit码；child有12秒自身watchdog，driver最多等15秒，不查找或终止任何其他进程。

这验证控件传输机制，不验证完整产品许可、Clipboard多格式发布或物理I2/R10；隐藏可见性与产品可见目标资格明确不同。没有 UOI_IO、无nianlai进程或日常版退出前提。完整策略差异见任务设计草案和官方研究。

probe-build根目录的首版编译记录保留为历史，未运行live；当前审查/后续执行只使用r2及其manifest。build脚本不硬编码仓库或产物路径，已有manifest时仍拒绝覆盖。
