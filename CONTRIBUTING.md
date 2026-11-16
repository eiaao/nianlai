# 参与开发

问题和建议欢迎提 [Issue](https://github.com/eiaao/nianlai/issues)。

## 从源码构建

技术栈：Tauri 2 + Rust，前端 Svelte 5 + TypeScript。

准备 rustup（Rust 版本由 `rust-toolchain.toml` 固定，首次构建时自动安装）、Node.js 20 以上、pnpm，以及 Visual Studio 生成工具和 WebView2。

```bash
pnpm install
pnpm tauri dev                 # 开发运行
pnpm tauri build --no-bundle   # 正式版构建
```

打便携版：`powershell -NoProfile -ExecutionPolicy Bypass -File tools\release.ps1`，输出到 `release\`。开发和验证用的工具见 [tools/README.md](tools/README.md)。

## 参与开发

从 `main` 新建分支，完成后提 PR。提交前在仓库根目录跑一遍 CI 用到的检查：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm check
node tools/test-panel-key-repeat.mjs
python -X utf8 -I tools/check_release_versions.py
python -X utf8 -I tools/check_ps1_encoding.py
python -X utf8 -I -m unittest discover -s tools -p "test_release*.py" -v
```

CI（`.github/workflows/ci.yml`）在 PR、推送到 `main` 和手动触发时运行：先做上面的检查，再在 Windows 上真实构建便携版，产物保留 30 天。

### 发版

1. 改版本号：`powershell -NoProfile -ExecutionPolicy Bypass -File tools\release.ps1 -Version 0.1.0 -AllowDirty`，会同步 `src-tauri/Cargo.toml`、`package.json` 和 `Cargo.lock`。
2. 提交版本号改动，推送到 `main`。
3. 打标签并推送：`git tag v0.1.0 && git push origin v0.1.0`。

推送 `v*` 标签后，`.github/workflows/release.yml` 会核对标签与版本一致 → 完整检查和构建 → 核对校验值和来源提交 → 创建 GitHub Release 并上传便携版。带 alpha、beta、rc 的版本标为预发布。
