# ADR 0002：单个普通用户权限常驻进程

[历史英文原件](0002-single-user-resident-process.md)是不可变权威记录；本文件是中文伴随译本。

## 状态

2026-07-15 接受。

## 背景

Telemetry ingestion、durable rule work、Dashboard 和原生 action 需要一个 current-user state owner。System service 或多个协作进程会增加权限边界、进程间协议和相互竞争的生命周期 owner，对 v1 单用户桌面场景没有收益。

## 决策

CodexMonitor 在交互式桌面会话中作为一个普通用户权限常驻进程运行。手动启动与 `--background` 使用相同 host/engine 组合。经过验证的第二次启动把用户可见的打开操作转发给常驻实例，不启动另一个 engine。进程只拥有当前用户的 storage 和操作系统注册项，不需要管理员或 system-service 权限。

## 影响

- 一个进程拥有 listeners、SQLite writer、workers、tray 与原生 action adapters，启动和关闭只有一个生命周期边界。
- 该用户进程退出后监控停止，不存在继续运行的机器级隐藏服务。
- 不同操作系统用户拥有独立进程、credentials 与 storage。
- 未来的多用户 daemon 或 split-process 架构需要新 ADR，说明 authentication、IPC、安装和升级语义。
