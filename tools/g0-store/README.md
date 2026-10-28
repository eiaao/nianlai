# g0-store 离线工具

`paste-fixture` 用生产 Store API 创建一个独立的合成粘贴样本，不访问系统剪贴板，也不启动产品。

```powershell
cargo run -p g0-store -- paste-fixture --dir D:\test\nianlai-html-fixture --format html
```

`--dir` 必须显式提供，父目录必须存在，目标路径必须不存在。此命令不读取 `NIANLAI_G0_DATA_DIR`；即使是已有的空目录也会拒绝，不覆盖数据库。`--format` 必须为 `html`、`rtf` 或 `text`，不接受其他参数。

每次只写一个包含中文、英文和 emoji 的固定记录。HTML/RTF 样本同时保留 Unicode 文本与对应富文本格式。三种样本的 `expected_submission_method` 均为 `depends`：实际提交方式取决于目标资格、粘贴键规则及运行时检查；验收会话应另行固定并核对预期路由。

标准输出为 JSON 清单，包括记录 ID、各格式长度与 SHA256、内容指纹、数据库 schema 版本、预期 UTF-16 长度及 UTF-8/UTF-16LE 摘要。调用方应保存清单，并在真实产品验收时核对实际选择记录和格式；样本创建成功不代表粘贴验收通过。实际运行保持生产采集开启，出现记录漂移则该用例无效。

离线验证：`cargo test -p g0-store paste_fixture::tests`。
