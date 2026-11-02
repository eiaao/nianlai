# Security policy / 安全策略

## Reporting a vulnerability / 报告漏洞

Please **do not** open a public issue for security problems. Use GitHub's private
reporting instead: **Security → Report a vulnerability** on this repository.
We aim to reply within 7 days.

请不要在公开 issue 里报告安全问题，使用本仓库的 **Security → Report a vulnerability**
私下提交，我们会在 7 天内回复。

## Supported versions / 支持的版本

Only the latest release receives security fixes. 只有最新版本会收到安全修复。

## What Nianlai does on your machine / 程序会做什么

- Listens for clipboard changes and stores the copied content (text, rich text, images,
  file lists) in a local SQLite database under `%LOCALAPPDATA%\Nianlai\data`. It records
  the name of the process that owned the clipboard, never window titles.
  监听剪贴板变化，把复制的内容（文本、富文本、图片、文件列表）存进本机
  `%LOCALAPPDATA%\Nianlai\data` 下的 SQLite 数据库；只记剪贴板所属的进程名，不记窗口标题。
- Installs low-level keyboard and mouse hooks (`WH_KEYBOARD_LL`, `WH_MOUSE_LL`) while
  the panel is shown, so it can take keys without stealing focus from the app you are
  typing in. Key presses are not stored, and logs never contain clipboard content or
  search text.
  面板显示期间安装低级键盘和鼠标钩子，用来在不抢焦点的情况下接收按键；按键不会被保存，
  日志里不写剪贴板内容和搜索词。
- Pastes by writing the chosen item back to the clipboard and sending a paste key to the
  target window (or inserting text directly into standard edit controls).
  粘贴时把选中的内容写回剪贴板，再向目标窗口发送粘贴键（标准编辑框会直接插入文本）。
- Makes **no network requests**. There is no telemetry and no update check yet.
  程序**不联网**，没有遥测，目前也没有更新检查。
- Optional start at login uses the per-user `Run` registry key; settings live in
  `%APPDATA%\Nianlai\settings.json`, logs in `%LOCALAPPDATA%\Nianlai\logs`.
  开机自启（可选）使用当前用户的 `Run` 注册表项；设置在 `%APPDATA%\Nianlai\settings.json`，
  日志在 `%LOCALAPPDATA%\Nianlai\logs`。

Your clipboard history is stored unencrypted on disk. Anyone with access to your user
account can read it. 剪贴板历史在磁盘上未加密，能登录你账号的人都能读到。
