# 合成 OTLP fixture

`fixtures/otlp/` 中的每个值都由人工编写且完全合成。任何源文件都不得从 Codex、本机 OTel exporter、用户 profile 或 SQLite 数据库捕获。

`response_completed.json`、`metadata_only.json` 和 `unknown_fields.json` 使用 OTLP Protobuf JSON 字段名。Rust 原生合同测试会规范化有效 JSON，按需转换为确定性的 binary Protobuf，追加已审阅的 unknown wire field，并生成 `mtime = 0` 的确定性 stored-block gzip stream。binary 与 gzip body 只在内存中生成，仓库不提交不透明捕获 payload。

`malformed.protobuf.hex`、`malformed.json` 和 `malformed.gzip.txt` 是显式无效输入。`malformed.gzip.txt` 会在未压缩的情况下故意以 `Content-Encoding: gzip` 发送。

冻结的基线 case matrix 与 request hash 位于 `contracts/legacy-otlp-v1.json`。机器可读的 Rust cutover disposition 位于 `contracts/legacy-otlp-v1-disposition.json`；两个 authority 均由 `codex-monitor-telemetry::legacy_contract` 强制验证。

fixture 合同只允许冻结合同登记的源文件。`pnpm check:repository-fixtures` 从 Git index 检查 staged candidate；`pnpm check:repository-fixtures:worktree` 检查当前工作区候选文件，包含未跟踪且未忽略的文件，并排除已删除路径。两者都会检查文件类型、合成标识符、敏感 marker、绝对路径、外部 URL、真实邮箱、UUID、数据库、日志和认证材料，且不会读取被 Git 忽略的本机缓存。
