# 卸载 CodexMonitor

[English translation](uninstall.md)；本文件是中文权威版本。

卸载操作会删除应用，但默认保留当前用户的数据库、设置和结构化日志。删除数据是独立的
明确操作。

## 卸载前

1. 如果开启了登录自启，先在托盘或 Dashboard 设置中关闭，让 CodexMonitor 删除当前
   用户的启动注册。
2. 在托盘中选择“退出”。没有托盘时，用当前的可执行文件加 `--quit`。
3. 等待有序退出完成，再删除应用文件或数据。只关闭浏览器并不够，因为没有 Dashboard
   标签页时监控仍会继续。

如果应用无法启动，请在卸载后通过操作系统的当前用户启动项设置删除对应条目。

## 删除应用

### Windows

Windows 没有安装程序可卸载。退出后，在准备删除的便携 EXE 所在目录运行：

```powershell
.\codex-monitor.exe --cleanup-platform-registration
```

该命令清理当前 EXE 的 Windows 平台注册。确认命令成功后再删除
`codex-monitor.exe` 和解压 ZIP 时创建的目录；不要先删除 EXE，否则无法由它完成清理。

### macOS

CodexMonitor 退出后，从 Applications 或实际安装目录删除 `CodexMonitor.app`。另行
推出并删除下载的 DMG。

### Linux

- 使用 deb 或 rpm 时，通过最初安装它的发行版软件包管理器删除 CodexMonitor。
- 使用 AppImage 时，等待常驻进程退出后删除 AppImage 文件。

删除 Linux 软件包会移除打包文件，但不会删除当前用户的应用数据目录。

## 卸载后保留的数据

下列目录会有意保留：

| 平台    | 数据目录                                                                                     |
| ------- | -------------------------------------------------------------------------------------------- |
| Windows | `%LOCALAPPDATA%\CodexMonitor`                                                                |
| macOS   | `~/Library/Application Support/CodexMonitor`                                                 |
| Linux   | `$XDG_DATA_HOME/codex-monitor`；未设置 `XDG_DATA_HOME` 时使用 `~/.local/share/codex-monitor` |

以后重新安装或升级时，可以继续使用 SQLite 历史和已保存设置。目录中可能包含假名化的
conversation/thread 标识符，应把它视为本地敏感数据。

## 删除全部本机数据

以下命令不可撤销。运行前确认 CodexMonitor 已经退出，检查目标路径，并在需要时先保留
本机备份。不要上传备份。

### Windows PowerShell

```powershell
Remove-Item -LiteralPath "$env:LOCALAPPDATA\CodexMonitor" -Recurse -Force
```

### macOS 终端

```sh
rm -rf -- "$HOME/Library/Application Support/CodexMonitor"
```

### Linux 终端

```sh
rm -rf -- "${XDG_DATA_HOME:-$HOME/.local/share}/codex-monitor"
```

这些命令会删除应用数据目录内的本机 SQLite 数据库、设置和经过净化的结构化日志，
不能删除保存在其他位置的副本、备份、文件系统快照或附件。

## 验证卸载

1. 确认没有 CodexMonitor 进程。如果进程仍在运行，先发送 `--quit`，再重试删除。
2. 确认 Windows 便携 EXE、应用入口或 AppImage 已删除；Windows 删除前应已运行
   `--cleanup-platform-registration`。
3. 如果要求完整删除，确认对应平台的数据目录已经不存在。
4. 如果没有其他本机接收器使用 `http://127.0.0.1:4318/v1/logs`，请删除或关闭
   Codex 的 OTel exporter 配置，否则 Codex 可能继续尝试向已经不存在的接收器
   发送 logs。

关于 CodexMonitor 从不持久化的数据，以及本机删除的边界，参见[隐私说明](../../PRIVACY.zh-CN.md)。
