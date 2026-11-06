# M1-H 呼出快捷键验收

## 安全边界

本说明中的实机步骤**尚未在本实现会话执行**。只能由操作员在独立测试机或**没有日常样机的 Windows 登录会话**执行。

- 不要为测试启动第二个拈来去干扰正在使用的实例。单实例转发不会因为换了数据目录就隔离。
- 只设置 `NIANLAI_G0_DATA_DIR` **不能**隔离 `settings.json`；必须同时隔离子进程的 `APPDATA`、`LOCALAPPDATA`。
- 不运行 `pnpm tauri dev`，不覆盖日常安装目录，不修改系统级环境变量，不按进程名结束进程。
- 测试应用从托盘正常退出。占用工具是前台命令，按 Enter 释放，不需要结束进程。

## 离线验收

在仓库根目录依次运行；这些命令不会启动应用或注册真实全局键：

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm check
pnpm build
node tools/test-panel-key-repeat.mjs
```

预期：Rust 格式、clippy、全部测试通过；`pnpm check` 为 0 错误、0 警告；前端构建和面板按键回归全部通过。

新增 Rust 测试使用替身注册器，覆盖：组合校验、保留键、常见冲突仅警告、先注销后注册、等价键不重复注册、注册失败不回退/不 panic、占用释放后同值重试、注销失败不注册第二个键、保存/广播/重载、非法输入和写盘失败不动旧键、不广播、测试环境覆盖、自动循环不注册呼出键、守护者不因此消耗重启额度。Windows 钩子测试另覆盖无呼出键及就地更新不清空吞键配对。

## 构建及启动隔离实例（仅操作员实机手测）

先完成上面的前端构建，再构建测试二进制：

```text
cargo build -p nianlai
```

在仓库根目录的 PowerShell 中，确认当前 Windows 会话没有日常样机后执行：

```powershell
$root = Join-Path (Get-Location) ('target/m1-h-manual-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path "$root/profile/Nianlai", "$root/local", "$root/data" | Out-Null
'{"hotkey":"Ctrl+Shift+F10"}' | Set-Content -LiteralPath "$root/profile/Nianlai/settings.json" -Encoding ascii
$psi = [Diagnostics.ProcessStartInfo]::new()
$psi.FileName = (Resolve-Path 'target/debug/nianlai.exe').Path
$psi.WorkingDirectory = (Get-Location).Path
$psi.UseShellExecute = $false
$psi.EnvironmentVariables['APPDATA'] = "$root/profile"
$psi.EnvironmentVariables['LOCALAPPDATA'] = "$root/local"
$psi.EnvironmentVariables['NIANLAI_G0_DATA_DIR'] = "$root/data"
$psi.EnvironmentVariables['NIANLAI_GUARD'] = '1'
$psi.EnvironmentVariables.Remove('NIANLAI_HOTKEY')
$psi.EnvironmentVariables.Remove('NIANLAI_G0_AUTOCYCLE')
$psi.EnvironmentVariables.Remove('NIANLAI_G0_RESUSPEND_MIN')
$startTest = { $p = [Diagnostics.Process]::Start($psi); Write-Host "守护者 PID=$($p.Id)；隔离根目录=$root"; $p }
$guardian = & $startTest
```

这只把环境传给新进程及其守护子进程，不改当前会话或日常配置。保留 `$psi`、`$root`、`$guardian`，后续用同一配置重启。

## 实机检查清单

### 1. 编辑、即时生效及重启保持

1. 在记事本等普通权限目标输入框保持焦点，按初始键 **Ctrl+Shift+F10**。
2. 确认面板仍按原快速模式显示，不抢前台；按 Space / Esc 验证浮层分层，方向键、数字、Enter、Ctrl+Enter、中文搜索沿用原行为。
3. 从面板或托盘的“设置…”打开设置。在“通用 → 呼出快捷键”选择 **Ctrl+Shift+F9** 并保存。
4. 设置窗口显示“已保存”和“当前生效”均为新组合；切回目标窗口，新键立即可呼出，旧键不再呼出。
5. 让面板保持可见，通过托盘打开设置并再次改键；新键仍能收起面板，导航和已经按下按键的抬起配对不被重建钩子打断。
6. 在设置切换 A/B/C 布局、主题、动效，验证已打开面板仍收到后端 `settings:changed`，不重置会话或强行聚焦。
7. 从托盘退出，确认守护者正常结束；检查 `$root/profile/Nianlai/settings.json` 的 `hotkey` 已保存。
8. 执行 `$guardian = & $startTest`，验证启动按保存的新键注册，旧键不生效。

### 2. 被占用：运行时失败、同值重试及启动降级

另开一个仓库根目录的终端，前台运行：

```powershell
powershell -NoProfile -File tools/hold-test-hotkey.ps1
```

等待看到“测试占用已就绪”和 PID。默认只占用 **Ctrl+Shift+F11**，不模拟键盘输入。

1. 在运行中实例的设置选择 **Ctrl+Shift+F11** 并保存。
2. 预期：旧键已注销，新键注册失败；设置明确显示“呼出键被占用或无效，请换一个”、系统错误和“当前生效：无”；没有偷偷注册备用键，没有 panic 或程序退出。
3. 从托盘打开面板，验证导航、Space/Esc、搜索和取用仍可使用；托盘“设置…”与“退出”仍可用。
4. 日志 `$root/data/panel-log.jsonl` 应先有旧键 `hotkey_unregistered`，再有 `hotkey_registration_failed` 且 `fatal:false`；不应出现新的 `guardian_child_exit` / restart。子进程 PID 保持不变。
5. 保持占用工具运行，从托盘退出应用，再执行 `$guardian = & $startTest`。保存的 F11 在启动时注册失败，应用仍常驻；从托盘打开设置仍能看到错误。等待至少 10 秒确认没有 2 秒间隔的重启循环或守护者“连续异常退出”弹窗。
6. 在占用工具终端按 Enter 释放。回设置点“保存并重试注册”（组合不变），预期立即注册成功、清除错误，新键正常工作。
7. 再次占用 F11 前先把应用改回 F9。也可保持占用并直接改成空闲键，验证错误立即清除。

可选：在另一个新终端运行下列命令，占用固定退出键，再启动测试实例；固定退出键失败也不得阻止启动，托盘退出仍应正常：

```powershell
powershell -NoProfile -File tools/hold-test-hotkey.ps1 -Modifiers 7 -VirtualKey 0x51
```

### 3. 非法组合与软冲突

在选择控件中编辑，**不要在键盘上实际敲系统保留组合**：

- 清空主键：提示“主键不能为空”，保存按钮禁用。
- 只勾修饰键而不选主键：不能保存；保留主键但取消全部修饰键：提示至少一个修饰键，不能保存。
- Win+L、Ctrl+Alt+Delete、Alt+Tab、Ctrl+Shift+Escape、Ctrl+F12：显示具体禁用原因，不能保存。
- Ctrl+Alt+Shift+Q / L：提示不能占用退出键或采集调试键。
- Ctrl+V、Ctrl+Shift+P、Ctrl+Space：有常见应用冲突提示，但允许保存；若系统实际拒绝注册则走非致命降级路径。测试后立刻改回测试键，避免影响普通编辑。
- Win+F8：可以尝试保存；若系统允许，应能即时呼出；若系统拒绝，明确显示失败且保留托盘入口。
- 快速连续切换合法/非法草稿：较早的合法校验结果不得错误启用当前非法草稿的保存按钮。

### 4. 完成与清理

从托盘退出测试应用，在所有占用工具窗口按 Enter。保留本次新建的 `$root` 作为验收证据；确认不再需要后由操作员只清理该确切目录。不要删除日常数据或按进程名清场。

## 行为取舍

- 被占用或系统拒绝的组合在**通过静态校验且写盘成功**后仍保留为用户期望值；实际注册状态独立显示。失败不回滚旧键、不隐式换备用键，避免界面与真实键位不一致。释放占用后同值保存即可重试。
- 写盘在换键之前；写盘失败保持旧设置和旧注册。注销失败时不注册新键，保留最后已知的旧注册并显示错误。
- 系统保留清单是保守的已知清单，不宣称穷尽 Windows 版本、第三方全局热键、输入法和键盘布局的组合；常见应用清单仅作提醒并写入日志，不探测其他应用的私有热键配置。
- 支持 Ctrl / Alt / Shift / Win；Win 在底层规范为 Super。拦截已知系统组合，未列出的 Win 组合仍由 Windows 决定是否允许注册；键名使用物理 Code，字符键帽可能随布局而异。F12 按 Windows 调试器保留规则拒绝。
- `NIANLAI_HOTKEY` 继续优先于设置，仅覆盖当前测试进程；设置页明确显示覆盖状态，保存值仍会持久化，移除覆盖后启动使用。自动循环继续不注册呼出键。
- 注册失败不再是启动错误；无需放宽守护者对真正崩溃、被杀或其他初始化失败的重启策略。
