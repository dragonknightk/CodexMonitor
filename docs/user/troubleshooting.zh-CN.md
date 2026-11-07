# CodexMonitor 故障排查

[English translation](troubleshooting.md)；本文件是中文权威版本。

CodexMonitor 有意避免记录原始载荷诊断信息。排查时先使用可观察状态和合成复现步骤，
不要附带个人 Codex 数据。

## 先收集安全诊断信息

只记录以下内容：

- CodexMonitor 版本、操作系统版本和 CPU 架构；
- 安装包类型：Windows 便携 ZIP、`.app`、DMG、AppImage、deb 或 rpm；
- 构建产物标签，包括存在时的 `unsigned-development`；
- 进程是否启动、Dashboard 是否打开、OTLP 数据源是否可用；
- Dashboard 或托盘显示的封闭安全状态码；
- 大致发生时间，以及只使用合成内容的复现步骤。

不要在报告中粘贴 bootstrap URL、数据库行、原始 OTLP body、本机路径、账户或
conversation 标识符、Codex 日志、session index 或原生错误文本。

## 应用无法启动

1. 确认安装包与操作系统和架构一致。macOS 要求 12.0 或更高版本；Linux v1 安装包
   仅支持 x64。
2. 确认常驻进程是否已经运行。再次正常启动时，第二个进程会把“打开 Dashboard”请求
   转发给现有实例，然后退出。
3. 如果启动时显示 migration 或数据库完整性状态码，不要降级、编辑 SQLite schema 或
   删除 migration record。保留本机数据目录并报告安全状态码。
4. 在 Windows 上，确认 ZIP 已解压，且根目录只含 `codex-monitor.exe`；再检查
   `unsigned-development` 构建是否被本机应用信任策略阻止。正式 tag 发布应带有签名。
5. 在 macOS 上，检查版本、架构和构建产物标签。`unsigned-development` 应用只为本机
   验证使用 ad-hoc 签名，没有 Developer ID 签名，也没有 notarize。
6. 在 Linux 上，确认 AppImage 有执行权限，且动态加载器可以找到 WebKitGTK 4.1。
   deb/rpm 应由软件包管理器解析该依赖。

如果 WebKitGTK 无法加载，Linux 进程会在引擎启动前失败。此时没有可继续运行的监控、
OTLP 接收器、SQLite 写入器或 Dashboard。

## Dashboard 没有打开

v1 没有可见的桌面 WebView。正常手动启动会请求系统默认浏览器打开 Dashboard。可以
再次手动启动，或在托盘中选择“打开 Dashboard”；两种方式都会让现有实例创建新的一次性
bootstrap URL。

Dashboard 优先使用 `127.0.0.1:4319`，必要时选择其他可用回环端口。不要假定端口一定
是 `4319`，不要硬编码 URL，也不要收藏 bootstrap 地址。bootstrap 成功后会重定向到
普通 URL，并保存 `HttpOnly` 本机会话 cookie。

如果浏览器已经到达这个普通回环 URL，却显示 `HTTP ERROR 404`，说明常驻 API 正在运行，
但已安装的可执行文件没有提供随包 Dashboard 页面。只记录版本和安装包类型，从托盘退出，
再安装当前版本的安装包。反复打开同一安装或更换浏览器无法修复缺失的随包页面路由。

关闭浏览器不会停止监控。如果浏览器没有打开，请检查操作系统的默认浏览器关联和本机
策略。不要通过局域网代理暴露 Dashboard，也不要把监听器改为非回环地址。

## 没有收到遥测

确认 Codex 配置使用准确的本机 logs endpoint，并关闭提示词、metrics 和 trace export：

```toml
[analytics]
enabled = false

[otel]
environment = "local"
log_user_prompt = false
metrics_exporter = "none"
trace_exporter = "none"
exporter = { otlp-http = { endpoint = "http://127.0.0.1:4318/v1/logs", protocol = "binary" } }
```

修改配置后重启 Codex，发起一次普通请求，再检查 Dashboard 的数据源状态。Dashboard
端口与固定 OTLP 接收器端口无关；不要把 OTel 日志发送到 `4319` 或随机 Dashboard
端口。

如果数据源健康但没有出现调用，请用合成内容复现并报告安全的数据源状态码。不要捕获或
上传真实 OTLP body。

## 4318 端口被占用

其他进程占用 `127.0.0.1:4318` 时，CodexMonitor 的 OTLP 接收器无法启动。
CodexMonitor 会保留常驻主机和 Dashboard，并报告接收器冲突；它不会把 OTLP 接收器
移到其他端口，因为 Codex endpoint 是固定的。

找出并停止或重新配置另一个本机接收器。如果界面提供接收器重试操作，可使用该操作；
否则重启 CodexMonitor。重新安装 CodexMonitor 不会释放由其他进程占用的端口。

## Linux 托盘或通知不可用

进程成功启动后，缺少托盘/AppIndicator 支持或用户会话通知服务属于受支持的降级状态。
引擎、`127.0.0.1:4318` OTLP 接收器、SQLite 存储和回环 Dashboard 会继续运行。
没有可见托盘时，再次手动启动 CodexMonitor 即可打开 Dashboard。通知发送不可用时，
告警仍保留在 Dashboard 历史中。

这一降级不适用于缺少 WebKitGTK 动态加载运行库的情况。如果该依赖阻止启动，进程和
监控都不存在。

## 系统通知没有出现

1. 检查 CodexMonitor 设置中是否开启推理通知。
2. 检查操作系统为该应用设置的通知权限，以及专注/勿扰设置。
3. 检查告警历史。告警可以存在，但对应动作可能是 suppressed、unavailable、failed，
   或已经与另一条通知合并。
4. 默认推理模式阈值只通知符合条件的低 `n` 匹配。更高的匹配可以保留在历史中，
   但不触发通知。

通知开始发送后，如果进程崩溃导致结果不确定，CodexMonitor 不会自动重试，避免用户看到
重复通知。对应告警仍会保留。

Windows 便携 EXE 移动或重命名后，先从新路径正常手动启动一次。该启动会自校准当前路径
的通知身份并恢复通知；无需手动修改通知注册。

## 登录自启没有运行

登录自启默认关闭。请在托盘或 Dashboard 设置中明确开启，并确认显示状态保持为开启。
注册项属于当前用户，使用 `--background` 启动同一个应用，因此登录时不会打开浏览器。

请把登录自启所指向的可执行文件保持在稳定路径。Windows 便携 EXE、AppImage 或其他
已登记路径的文件移动或重命名后，在运行中的应用里关闭再重新开启登录自启，以刷新注册项。

## 旧版导入被拒绝

使用已安装的原生命令，并提供绝对 SQLite 路径：

```text
codex-monitor --import-legacy <absolute-sqlite-path>
```

相对路径会被拒绝。可以先明确退出再运行命令，也可以在常驻实例运行时直接调用；已有实例
会通过单实例路由接收请求。导入器不依赖旧 Python 运行时。

不要修改源文件 schema，也不要把原始数据表复制到当前数据库。当导入成功，或检测到
同一源文件此前已经导入时，程序会打开 Dashboard。如果没有打开，请保持源文件不变，
只报告安全导入状态码、安装包类型和合成复现步骤。

## 升级或卸载被阻止

关闭 Dashboard 标签页后，常驻进程仍在运行，应用文件可能继续被占用。请使用托盘“退出”，
或用当前可执行文件加 `--quit`，等待有序退出后重试。Windows 删除便携 EXE 前必须先运行
`--cleanup-platform-registration`。

不要强制降级。升级会覆盖应用并保留数据；卸载也默认保留数据。请按[升级](upgrade.zh-CN.md)
或[卸载与删除数据](uninstall.zh-CN.md)操作，不要在进程运行时删除 SQLite 文件。

## 安全报告问题

请提供本文开头列出的安全信息和最小合成复现。如果问题涉及构建产物，请链接公开的原生
工作流运行记录，不要自行声称四个平台任务已经通过。

不要上传应用数据目录、数据库、WAL/SHM 文件、设置、Codex 日志、
`session_index.jsonl`、原始遥测、bootstrap URL、签名材料或包含标识符的截图。
发送报告前请阅读[隐私说明](../../PRIVACY.zh-CN.md)。
