# ADR 0003：分离 OTLP 与 Dashboard listeners

[历史英文原件](0003-separate-otlp-dashboard-listeners.md)是不可变权威记录；本文件是中文伴随译本。

## 状态

2026-07-15 接受。

## 背景

OTLP ingestion 与 Dashboard 的协议、authentication 规则、失败方式和端口要求不同。放入同一个 HTTP listener 会把 receiver 可用性与 browser-session security 耦合，也会让固定 telemetry endpoint 与面向用户的 API 共用生命周期。

## 决策

Engine 拥有两个独立 loopback HTTP listeners。OTLP receiver 精确绑定 `127.0.0.1:4318`。Authenticated Dashboard listener 优先使用 `127.0.0.1:4319`，被占用时可以选择另一个 loopback 端口。两者不共享 router、socket 或可从外部访问的 interface，各自拥有 readiness、error state 和 shutdown stage。CodexMonitor 默认没有 outbound telemetry path。

## 影响

- OTLP bind conflict 可以单独报告和重试，不改变 Dashboard authentication boundary。
- Browser routes 不会继承 unauthenticated ingestion 行为，receiver requests 也不能消费 Dashboard sessions。
- 启动与关闭必须组合、监督并终结两个 listener 生命周期。
- 本机集成使用固定 OTLP address，browser client 使用常驻进程签发的 active Dashboard origin。
