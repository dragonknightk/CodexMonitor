# 安装 CodexMonitor

[English translation](installation.md)；本文件是中文权威版本。

CodexMonitor 是普通用户权限的常驻应用。它不会安装系统服务，不会监听局域网接口，
v1 也不会创建可见的桌面 WebView。

## 支持的安装包

| 平台    | 架构       | 安装包             | 额外要求                                 |
| ------- | ---------- | ------------------ | ---------------------------------------- |
| Windows | x64        | 便携 ZIP           | 受支持的 Windows 桌面会话                |
| macOS   | x64、arm64 | `.app`、DMG        | macOS 12.0 或更高版本                    |
| Linux   | x64        | AppImage、deb、rpm | WebKitGTK 4.1；托盘集成需要 AppIndicator |

Linux AppImage 以 Ubuntu 22.04 作为 glibc 兼容基线。Linux arm64 不在 v1 安装包
矩阵内。

## 安装前

1. 从项目发布页面下载与操作系统和 CPU 架构一致的安装包。
2. 检查构建产物标签。`unsigned-development` 表示开发构建，其信任保证与正式 tag
   发布不同。
3. 不要复制个人 Codex 数据库或日志来安装或测试。应用会创建自己的当前用户数据目录。
4. 如果要替换现有安装，先阅读[升级](upgrade.zh-CN.md)，并在覆盖文件前退出常驻进程。

## 在 Windows 上安装

Windows x64 发布物是一个便携 ZIP。解压后，其根目录只包含
`codex-monitor.exe`；把这个 EXE 放在准备长期使用的本机目录，再正常启动。它没有
NSIS、MSI 或系统级安装步骤，也不要把它配置为 Windows Service。

每次手动启动都会按当前 EXE 路径自校准 Windows 通知身份。首次启动不需要额外配置。
正式 tag 发布中的 EXE 应带有签名；`unsigned-development` 构建可能触发 Windows
信任警告，只适合受控的开发验证。

## 在 macOS 上安装

CodexMonitor 要求 macOS 12.0 或更高版本。Intel Mac 选择 x64 构建产物，Apple
silicon 选择 arm64 构建产物。

- 使用 DMG 时，打开它并把 `CodexMonitor.app` 复制到 Applications。
- 使用独立 `.app` 时，把它放到 Applications 或其他稳定的本机应用目录。

正常启动复制后的应用。正式 tag 发布要求 Developer ID 签名和 notarization。
macOS 的 `unsigned-development` 构建产物只使用本地验证所需的 ad-hoc 签名；它没有
Developer ID 签名，也没有 notarize。请遵守组织的本地应用信任策略，不要把该构建
视为正式版本。

## 在 Linux 上安装

使用与发行版一致的 x64 安装包：

- 通过发行版软件包管理器安装 deb 或 rpm，使其可以解析 WebKitGTK 4.1 和
  AppIndicator 依赖。
- 为 AppImage 添加执行权限，把它放到稳定路径，再以桌面用户身份启动。AppImage 以
  Ubuntu 22.04 作为 glibc 构建基线。

应用仍要求主机动态加载器能找到所需 WebKitGTK 运行库。正确的 deb/rpm 依赖元数据
应阻止依赖不完整的安装。AppImage 所在主机缺少所需 WebKitGTK 运行库时，进程会在
启动阶段失败；此时引擎尚未启动，监控不会继续。

进程成功启动后，即使桌面缺少托盘/AppIndicator 支持或用户会话通知服务，引擎、
OTLP 接收器、SQLite 和 Dashboard 仍可运行。没有托盘时，再次手动启动应用即可
打开 Dashboard。

## 首次启动

正常启动 CodexMonitor。唯一的常驻进程开始运行，系统默认浏览器会打开经过身份验证的
Dashboard。最终地址只使用回环网络：优先选择 `127.0.0.1:4319`，必要时改用其他
可用的回环端口。

首次安装不会开启登录自启。需要时在托盘或 Dashboard 设置中明确开启。自启注册使用
`--background`，后续登录启动不会打开浏览器。系统通知也可以在设置中关闭；告警仍会
显示在 Dashboard 中。Windows 便携 EXE 移动或重命名后，先从新路径手动启动一次以恢复
通知身份；若此前开启了登录自启，再关闭并重新开启登录自启，使其指向新路径。

关闭浏览器不会停止 CodexMonitor。请使用托盘“退出”，或用已安装的可执行文件加
`--quit` 请求有序退出。

## 配置 Codex

让 Codex 把 OTel 日志发送到固定的本机接收器，同时关闭提示词日志、metrics 和
traces：

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

保存配置后重启 Codex。发起一次普通 Codex 请求，再检查 Dashboard 中的数据源状态和
最近调用。CodexMonitor 的 `/v1/logs` 支持 OTLP binary Protobuf、OTLP Protobuf
JSON 和 gzip；上面的 binary 配置是常规使用方式。

## 数据位置

CodexMonitor 在当前用户目录中创建以下本机文件：

| 平台    | 数据目录                                                                                     |
| ------- | -------------------------------------------------------------------------------------------- |
| Windows | `%LOCALAPPDATA%\CodexMonitor`                                                                |
| macOS   | `~/Library/Application Support/CodexMonitor`                                                 |
| Linux   | `$XDG_DATA_HOME/codex-monitor`；未设置 `XDG_DATA_HOME` 时使用 `~/.local/share/codex-monitor` |

该目录包含 SQLite 数据库、设置和经过净化的结构化日志。卸载时默认保留。详情见
[隐私说明](../../PRIVACY.zh-CN.md)和[卸载与删除数据](uninstall.zh-CN.md)。

## 构建信任与发布状态

发布工作流以四个原生运行器任务为目标：Windows x64、macOS x64、macOS arm64 和
Linux x64。文档或源代码副本不能证明这些任务已经通过；请查看与构建产物对应的工作流
运行记录。

正式 tag 发布缺少 Windows 签名凭据，或缺少 macOS Developer ID 签名/notarization
凭据时，发布会失败。没有凭据的开发包统一标记为
`unsigned-development`；macOS 包只使用本机 ad-hoc 签名。不要把这类构建产物描述为
已经签名或 notarize 的正式版本。

## 后续操作

- [升级](upgrade.zh-CN.md)
- [卸载与删除数据](uninstall.zh-CN.md)
- [故障排查](troubleshooting.zh-CN.md)
- [隐私说明](../../PRIVACY.zh-CN.md)
