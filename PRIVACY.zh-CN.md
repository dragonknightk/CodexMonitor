# 隐私说明

[English translation](PRIVACY.md)；本文件是中文权威版本。

CodexMonitor 在持久化之前执行数据最小化。本说明描述打包后的 Rust/Tauri 应用所遵守
的本机数据边界。

## 网络边界

- OTLP 接收器固定监听 `127.0.0.1:4318`，并通过 `/v1/logs` 接收 Codex 的
  本机遥测。
- Dashboard 和 `/api/v1` 只监听回环地址。Dashboard 优先使用 `4319`，也可能使用
  其他可用的回环端口。
- CodexMonitor 默认没有远程遥测、分析、崩溃上传、监控数据转发或更新检查
  请求。
- 使用 OpenAI 托管模型时，模型请求仍会发送给 OpenAI。这条数据路径与 Codex 的
  本机 OTel exporter 相互独立。

回环地址可以减少网络暴露，但不能隔离同一个操作系统用户下运行的不同进程。以当前
用户身份运行的恶意程序通常也能直接读取或修改本地数据库和设置。

## 收到但不保存的数据

Codex OTel 事件可能包含提示词、回答、工具参数、命令、路径、工具输出、账户信息、
主机信息和后续版本新增的字段。CodexMonitor 不保存通用 attribute map，也不保存
经过过滤的原始事件副本。

原始请求字节和解码后的通用 OTLP message 只在有边界的内存请求处理期间存在。类型化
提取器只读取已登记的事件、属性和 `response.usage` 路径。未登记在评审后字段目录中的
内容会在进入存储边界前丢弃，包括：

- 原始 OTLP body、原始 JSON 和 payload 摘录；
- 提示词、消息、回答正文和会话名称；
- 工具参数、命令、工具输出和不受限制的 URL；
- reasoning summary、加密 reasoning 和其他 reasoning 内容；
- 工作区、sandbox、approval、可执行文件和普通文件路径；
- 邮箱、账户标识符、API key、cookie 和认证 token。

解码或处理失败时，诊断信息只记录安全状态码，以及编码方式、大小等级等有限事实，
不会包含载荷摘录。

## 保存到本机的数据

只有登记在遥测字段目录中的字段可以持久化。存在相应数据时，保存内容
包括通过校验的 token 计数、模型、推理强度、时间、事件/来源/客户端/surface 元数据、
字段来源、数据源健康状态、规则证据、动作状态、设置和迁移审计事实。

程序可以保存 conversation/thread 标识符，用于本机关联和筛选。它们属于假名标识符，
仍应视为本地敏感数据。普通诊断日志和系统通知不会包含这些标识符。

程序可以在查询时读取 `session_index.jsonl` 解析显示名称。名称可以显示在本机
Dashboard 中，但不会复制到监控数据表。索引缺失或无效时，Dashboard 使用缩短后的
标识符。

数据库、设置和结构化日志保存在当前用户的应用数据目录中：

| 平台    | 数据目录                                                                                     |
| ------- | -------------------------------------------------------------------------------------------- |
| Windows | `%LOCALAPPDATA%\CodexMonitor`                                                                |
| macOS   | `~/Library/Application Support/CodexMonitor`                                                 |
| Linux   | `$XDG_DATA_HOME/codex-monitor`；未设置 `XDG_DATA_HOME` 时使用 `~/.local/share/codex-monitor` |

程序会在各操作系统允许的范围内把权限限制到当前用户。

## Dashboard 安全

打开 Dashboard 时，程序会生成一个新的一次性 bootstrap URL。服务端用其中的 token
建立仅当前进程有效的 `HttpOnly`、`SameSite=Strict` session，再重定向到不含 token
的普通 URL。重复使用或已经过期的 bootstrap token 会被拒绝。

API 校验回环 `Host`，不启用宽松 CORS；修改设置时必须同时提供当前会话和 CSRF
保护。即使 token 有效期很短，也不要在问题报告中分享或粘贴 bootstrap URL。

## 系统通知

系统通知使用固定的本地化模板，只允许包含告警类型、可选模型名、推理 token 数、
计算得到的模式编号和检查结果的建议。缺少模型名时不会用会话名或其他字段补全。
合并通知也不会包含 conversation/thread 标识符。

通知不会包含提示词、回答、reasoning 内容、会话名称、工具数据、命令、路径、账户
标识符或认证材料。操作系统可能在通知中心或锁屏显示允许的标题和正文。如果旁人可以
看到屏幕，请在 CodexMonitor 中关闭推理通知，或在操作系统中隐藏通知预览。

权限、通知服务和发送失败只保存封闭的安全状态码。原生错误文本不会复制到动作审计或
诊断信息中。通知关闭或平台不可用时，告警仍保留在 Dashboard 中。

## 旧数据库导入

原生导入器把用户选择的旧数据库视为不可信的只读输入。它先校验 schema，再只查询
固定白名单中的结构化字段。非法 token 值会逐项丢弃。原始记录表、`raw_json`、提示词、
工具数据和额外私有列不会被查询或复制。

导入必须由用户通过 `codex-monitor --import-legacy <absolute-sqlite-path>` 明确请求，
相对路径会被拒绝。已有常驻实例时，单实例路由会把请求转发给该实例；应用不会启动或
保留旧 Python 运行时。

允许字段和不包含路径的导入审计会原子提交到新数据库，源文件保持不变。经过净化的
审计指纹和逐行身份保证重复导入具有幂等性。导入记录会标记为历史数据，不能产生实时
告警或通知，也不能进入自动化动作管线。当导入成功，或检测到同一源文件此前已经导入
时，应用会打开本机 Dashboard。

## 保留与删除

结构化监控历史默认保留 90 天。可选策略为 30 天、90 天、365 天或无限保留。清理过程
会保留完整性所需的待处理动作和迁移审计记录。

卸载 CodexMonitor 默认保留用户数据目录，因此升级或重新安装不会擦除历史。需要删除
全部本机数据时，先通过托盘“退出”或 `--quit` 停止常驻进程，再按[卸载与删除数据](docs/user/uninstall.zh-CN.md)
中的平台步骤操作。删除操作无法清除用户另行创建的副本、备份、文件系统快照或已经
上传到其他位置的附件。

## Codex 配置

建议保持以下隐私基线：

```toml
[analytics]
enabled = false

[otel]
log_user_prompt = false
metrics_exporter = "none"
trace_exporter = "none"
```

`log_user_prompt = false` 只控制 Codex 暴露的提示词字段，不能保证后续版本的每个 OTel
attribute 都不含敏感内容，因此 CodexMonitor 仍会执行自己的持久化白名单。不要为了
使用 CodexMonitor 把这个 exporter 配置为远程地址。

## 本机文件与问题报告

仓库忽略规则会排除 SQLite 数据库、WAL/SHM 文件、JSONL 文件、日志、本地设置、
环境文件和本机构建状态。不要用 `git add -f` 绕过这些规则。

不要把生产数据库、本机 Codex 日志、session index、bootstrap URL 或完整应用数据目录
作为复现附件上传。安全的问题报告可以包含 CodexMonitor 版本、操作系统、安装包类型、
架构、开发/签名标签、安全状态码，以及只使用合成数据的复现步骤。参见
[故障排查](docs/user/troubleshooting.zh-CN.md)。
