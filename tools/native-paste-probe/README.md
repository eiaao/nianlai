# NativePasteProbe：隐藏 WinForms 接收端

这是诊断专用接收端，不是输入驱动。只创建自身隐藏 Form / WinForms TextBox，处理自己的 `WM_PASTE`，记录长度、SHA-256、次数和窗口元数据。不保存正文，不主动调用 Clipboard API，不发键、不设焦点、不激活或切换桌面。

本轮仅实现和编译了 v1/v2/v3，没有由本分任务启动任何 helper。私有 window station 的创建已在外部驱动实验中被系统拒绝，已停止该路线。v3 的当前站分支尚未获得本轮实际执行许可；父端只有在用户明确许可这一次短诊断后才能设置 opt-in 并启动。固定值是防误启动的显式开关，不是对用户授权的密码学证明；实际授权原文、时间与源码/产物哈希由执行方另存证据。

## 构建

```powershell
pwsh -File tools/native-paste-probe/build.ps1 -OutputDirectory <不存在exe的新构建目录>
```

脚本只运行本机 .NET Framework `csc.exe`，使用 x64 WinExe、`/warnaserror`，不执行产物。已有 exe 拒绝覆盖。本工具不提供在 WinSta0 上顺手运行的 self-test，也不要直接双击 exe。

## 两个明确分离的 scope

基础参数均为四组：`--expected-station NAME --expected-desktop NAME --output NEW_LOCAL_DIRECTORY --timeout-ms 15000`。超时允许 1000..30000ms；输出必须不存在，工具创建目录并用 `.owner` 独占。相同目录禁止复用。

- 默认 `private`：可省略 scope。station 必须是 `NianlaiNativeProbe-<32hex>`，desktop 必须是 `NianlaiNativeProbe-Desk-<相同32hex>`，不能全零。实际 GetProcessWindowStation / GetThreadDesktop 的 UOI_NAME 必须精确匹配（忽略大小写），实际桌面的 UOI_IO 必须为 false，明确拒绝 WinSta0。查询失败或未知结果即非零退出，不创建 Form，不访问剪贴板。外部驱动原计划通过 CreateProcessW 的 STARTUPINFO.lpDesktop 选择该私有站；创建失败不触发任何其他分支。
- 当前站诊断：必须额外同时提供 `--scope authorized-interactive --opt-in M0_CURRENT_STATION_TEST_CLIPBOARD_V1`。expected-station 必须为 WinSta0，expected-desktop 为外部驱动实际核对的当前桌面名。创建 UI 前独立确认 UOI_NAME 相等、UOI_IO=true、没有任何 nianlai.exe；每次 WM_PASTE 前和 UI timer tick 再核验，失活、产品出现或任何未知即停止并失败。它记录 `scope=authorized-interactive`、`private_context_verified=false`，绝不能写成私有隔离通过。没有自动 fallback，也没有 receipt/TTL/hash 运行许可框架。

所有准入检查都位于 `Main` 创建 ProbeHost 之前；ProbeHost 才拥有 Form/控件。watchdog 的后台线程不创建 UI。Form/Edit 创建后保持隐藏，使用没有 MainForm 的 ApplicationContext 消息循环；不调用 Show、Focus、ActiveControl、Activate 或前台 API。`ShowWithoutActivation`/NOACTIVATE 也设定在 Form 上，但当前版本没有显示模式。

授权分支的完整 argv 契约如下（仅供审查；当前不得执行；桌面名由驱动实际核对，常见 Default 不能靠猜）：

```text
NativePasteProbe.exe --expected-station WinSta0 --expected-desktop <CURRENT_INPUT_DESKTOP_UOI_NAME> --output <NEW_LOCAL_ABSOLUTE_DIRECTORY> --timeout-ms 15000 --scope authorized-interactive --opt-in M0_CURRENT_STATION_TEST_CLIPBOARD_V1
```

## 文件协议

消息循环实际执行首个 UI Timer 回调后才原子发布 `ready.json`，此前 `state.json` 已写好。不使用固定 sleep 冒充循环握手。驱动读取所有活跃文件时需共享 ReadWrite|Delete；JSONL 尾行不完整或任意 failure.json 都使本次证据无效。

ready 顶层：`schema=1`、`run_id`、`pid`、`thread`、`station`、`desktop`、`scope`、`private_context_verified`、最近检查的 `desktop_receives_input/context_verified_utc`、`message_loop_running=true`、`hidden_controls=true`、`hwnd`、`edit`、`read_only=false`、`multiline=true`、`accepts_return=true`、`accepts_tab=true`、`max_length=32768`、`observer_clipboard_api_calls=0`、`utc`。焦点元数据为 `focus.focus_query_ok/focus_hwnd/focus_is_edit/focus_error`，来自对自身 UI 线程的 GetGUIThreadInfo，只读，不主动改变焦点。

每次自己的 WndProc 收到 WM_PASTE，先复核 context，记录调用前长度/hash，随后恰好调用一次 `base.WndProc`，返回后记录调用后长度/hash。每次完整观察都追加 `paste-events.jsonl` 并原子发布 state：

- `paste_calls`：本控件已进入的 WM_PASTE 次数；`wm_paste_completed` 表示至少有一次完整观察。
- `before_length_utf16/before_sha256_utf8` 与 `after_length_utf16/after_sha256_utf8`：最近一次 WM_PASTE 的同步前后值。
- `length_utf16/sha256_utf8`：普通 paste_observed 状态等于该次 after；正常停止时改为 UI 线程独立读取的当前值。
- 正常 STOP/timeout 状态额外带 `final_current_length_utf16/final_current_sha256_utf8`；`digest_origin=ui_final_current_text`。此读数不是简单复制 last.After。其他阶段 origin 为 known_initial_empty 或 wm_paste_after。
- scope、PID/线程、station/desktop、HWND、最近 context 检查与焦点元数据也随状态保存。默认 private 的 run_id 是名字中的 32hex；授权诊断由接收端产生独立新 GUID，外部执行证据以其目录/PID/ready 关联。

`observer_clipboard_api_calls=0` 只说明**观察器没有显式 Clipboard API 调用**。`base.WndProc(WM_PASTE)` 的原生 Edit 实现会按 Windows 规则消费所在 window station 的剪贴板；当前站分支因此会消费 WinSta0 的测试剪贴板，必须先有这次用户许可。这个字段不能解释为“WM_PASTE 没有访问剪贴板”。外部驱动负责授权后写入及核对测试剪贴板；本接收端不执行这些动作。本轮诊断不备份或恢复原剪贴板，结束后留下测试文字。

写入/追加失败不重试，记录 failure.json、停止接收后续粘贴并退出 1。替换即使可能已经部分完成，也按失败处理；驱动不能因看到一个貌似完整的 state 就忽略 failure 或非零退出。failure 标记自身写不了时仍返回非零，外部必须持有原进程句柄收取真实退出码。

## 退出和边界

UI timer 50ms 检查 STOP、scope 和正常超时。正常路径重新读取自身控件摘要后退出 0，phase 为 stopped / timeout；退出 0 本身不是粘贴通过，驱动仍需核对次数、before/after/final 摘要和无 failure。

最外层独立 System.Threading.Timer 在 timeout-ms+1000 的硬期限直接调用 `Environment.Exit(124)`，不等 UI、日志或控件；正常结束会 Dispose。`watchdog.json` 在 UI 创建前保存 armed_utc、ui_timeout_ms、hard_timeout_ms、hard_timeout_exit_code=124。强制超时不会伪造正常 final 摘要，必须按退出 124 判无效。驱动仍应保留子进程句柄并有外层限时回收保障。

目前是**隐藏** WinForms TextBox，虽然 Multiline/AcceptsReturn/AcceptsTab/MaxLength/ReadOnly/字体与 HintTarget 的接收控件配置一致，但没有复现可见目标的前后台、输入法、真实按键或用户焦点环境。它不能证明产品的可见性守卫或活动 IME 分支。原生插入/长度/hash 只能支持对应控件和调用路径的诊断；不能扩写为实机输入验收通过。当前 scope 检查与粘贴不是 OS 提供的原子事务，活跃桌面和产品状态改变由调用前及周期复核发现，不能把轮询说成全程锁定。

v1/v2 产物保留在独立构建目录；v1 的 clipboard_access=false 字段有歧义，v2 已移除。v3 新增上述显式当前站分支、独立 watchdog、循环握手和 fresh final 摘要；不同版本不得混用哈希和协议解释。
