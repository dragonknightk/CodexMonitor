# 升级 CodexMonitor

[English translation](upgrade.md)；本文件是中文权威版本。

CodexMonitor 采用覆盖安装方式升级。应用数据目录会保留；新版本必须先完成只向前的
SQLite 数据库迁移，才会开始接收遥测。

## 升级前

1. 确认新版本高于已安装版本。项目不支持也不允许降级。
2. 使用托盘“退出”停止常驻进程。没有托盘时，用已安装的可执行文件加 `--quit`，
   并等待进程结束。
3. 保持相同的操作系统架构；条件允许时也保持相同的安装包类型。
4. 如果需要备份，只在进程停止后复制应用数据目录。该副本属于本地敏感数据，不要上传。
5. 检查新构建产物的签名/开发标签，以及对应的原生工作流运行结果。
   `unsigned-development` 是开发构建，不是已签名的正式版本。

## 在 Windows 上升级

先退出常驻进程，解压新的 x64 便携 ZIP，再用其中唯一的 `codex-monitor.exe` 覆盖原来
相同路径的 EXE。随后正常启动一次。相同路径覆盖升级会保留
`%LOCALAPPDATA%\CodexMonitor` 中的数据和已保存设置，通知与登录自启也无需重新配置。

如果升级同时移动或重命名 EXE，先从新路径手动启动一次以恢复通知身份；此前开启了登录
自启时，再关闭并重新开启登录自启，使注册项改为新路径。正式 tag 发布要求 Windows
签名凭据；没有凭据的开发包会标记为 `unsigned-development`，并可能显示 Windows 信任
警告。

## 在 macOS 上升级

退出 CodexMonitor，再用相同架构的较新 x64 或 arm64 `CodexMonitor.app` 替换现有
应用。使用 DMG 时，把新应用覆盖复制到旧位置，再推出 DMG。随后正常启动替换后的应用。

正式 tag 发布要求 Developer ID 签名和 notarization。macOS 的
`unsigned-development` 构建只为本机验证使用 ad-hoc 签名，没有 Developer ID 签名，
也没有 notarize。

## 在 Linux 上升级

- 使用 deb 或 rpm 时，通过发行版软件包管理器把较新安装包覆盖安装到现有包，继续由
  软件包管理器解析 WebKitGTK 4.1 和 AppIndicator 依赖。
- 使用 AppImage 时，先退出进程，再替换稳定路径上的 AppImage，保留执行权限并正常启动。

AppImage 仍受 Ubuntu 22.04 glibc 基线和主机 WebKitGTK 运行库约束。如果动态加载器
无法加载 WebKitGTK，新进程不会启动，监控也没有运行。进程成功启动后，仅缺少托盘/
AppIndicator 或通知服务时，只会失去对应平台能力；引擎、OTLP 接收器、SQLite
和 Dashboard 会继续运行。

## 导入旧版历史

打包后的应用包含原生导入器，可以导入用户明确选择的旧 Python SQLite 数据库。该过程
不会启动、包装或依赖旧 Python 运行时。

用已安装的可执行文件和绝对源路径运行：

```text
codex-monitor --import-legacy <absolute-sqlite-path>
```

可以先退出 CodexMonitor 再运行该命令，也可以在常驻实例运行时直接调用。后一种情况会
通过单实例路由把经过校验的导入请求转发给现有进程。相对路径会被拒绝；Windows 使用
带盘符的绝对路径，macOS 或 Linux 使用以 `/` 开头的绝对路径。

导入器以只读方式打开源文件，校验 schema，并在一个原子事务中只复制白名单允许的
结构化字段。源文件保持不变。导入记录会标记为历史数据，不能产生实时告警或通知。
当导入成功，或检测到同一源文件此前已经导入时，程序会打开 Dashboard。

## 验证升级

第一次启动后：

1. 确认 Dashboard 在默认浏览器中打开，并且原有本机历史仍然可用。
2. 确认数据源状态显示 `127.0.0.1:4318/v1/logs` 上的 OTLP 接收器可用。
3. 发起一次普通 Codex 请求，确认出现新的模型调用。
4. 检查登录自启和系统通知设置；二者应保留用户已经保存的选择。
5. 关闭浏览器，确认监控进程仍然常驻。再次启动应用，确认现有实例重新打开 Dashboard。

重启后 Dashboard 可能使用另一个回环端口，因为 `4319` 只是首选端口。不要硬编码或
收藏 bootstrap URL。

## 降级策略

禁止降级。数据库迁移只能追加，没有反向迁移路径。不要用旧的 Windows 便携 EXE 覆盖
新版本，也不要强制安装旧 deb/rpm/AppImage，或让旧 macOS 应用打开已被新版本使用过的
数据库。

如果必须撤回升级，请先停止操作并收集版本号和安全状态码。只能使用项目认可的恢复流程，
不要编辑 SQLite schema，也不要删除迁移记录。

## 升级失败时

- 如果系统报告文件仍被占用，发送 `--quit`，等待有序退出后重试覆盖安装。
- 如果 Dashboard 可用但 OTLP 不可用，请解决本机 `4318` 端口冲突；重新安装不会释放
  该端口。
- 在 Linux 上，应区分托盘/通知能力缺失，以及因 WebKitGTK 缺失导致进程无法启动。
- 不要把删除应用数据目录作为第一项排障操作。

继续阅读[故障排查](troubleshooting.zh-CN.md)和完整的[隐私说明](../../PRIVACY.zh-CN.md)。
