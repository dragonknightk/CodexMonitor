# ADR 0007：Workspace crate 边界与消费方端口

## 状态

2026-07-18 接受。

## 背景

早期 `codex-monitor-engine` 同时包含 SQLite 持久化、OTLP 接收、Dashboard
HTTP 服务、规则与动作 worker，以及部分宿主协调逻辑。这个结构扩大了局部改动的编译和测试范围，也让传输层、持久化层与操作系统边界能够通过具体类型相互依赖。

这次迁移需要保持既有隐私边界、loopback listener、数据库迁移字节、REST/SSE
契约和常驻进程生命周期，同时让各职责可以独立编译和验证。旧模块路径不能通过重导出或兼容包装继续存在。

## 决策

Workspace 使用以下目标依赖图：

```text
codex-monitor-core
├── codex-monitor-storage
├── codex-monitor-telemetry
└── codex-monitor-dashboard

codex-monitor-engine
├── codex-monitor-core
├── codex-monitor-storage
├── codex-monitor-telemetry
└── codex-monitor-dashboard

codex-monitor-host       （不依赖 Tauri 或 engine）

codex-monitor-desktop
├── codex-monitor-engine
├── codex-monitor-host
└── 各运行时适配器 crate 的公开契约
```

`codex-monitor-core` 只拥有与 OTLP、SQLite、HTTP 和 Tauri 无关的领域契约。
`codex-monitor-storage` 拥有 SQLite、迁移、查询和持久工作队列；
`codex-monitor-telemetry` 拥有原始 OTLP 解码、类型化抽取、来源解析和接收器；
`codex-monitor-dashboard` 拥有 Dashboard HTTP、REST/SSE、会话与展示语义；
`codex-monitor-engine` 只负责组合适配器以及规则、动作和保留 worker；
`codex-monitor-host` 拥有可脱离 Tauri 测试的启动、协调、托盘视图模型和生命周期状态机；操作系统副作用只留在 `codex-monitor-desktop`。

跨边界能力由消费方拥有窄端口：telemetry 定义 `ObservationSink`，Dashboard
定义查询、设置、能力和事件端口，host 定义 `ResidentHostPort`。端口只传递闭合
DTO 与稳定错误代码；engine 或 desktop 在组合根实现适配器。上游 crate 不依赖消费方的具体实现。

迁移采用一次性替换：删除 engine 中原 storage、ingestion、Dashboard 和 host
adapter 路径，调用方直接依赖新 crate 的公开契约。engine 不重导出旧模块，也不提供 façade、shim 或 test-support 转发。

除完整 workspace 验证外，以下局部编译是架构验收条件：

- `cargo check -p codex-monitor-storage` 不编译 telemetry、Dashboard 或 Tauri；
- `cargo check -p codex-monitor-telemetry` 不依赖 `rusqlite` 或 Tauri；
- `cargo check -p codex-monitor-dashboard` 不依赖 `rusqlite`、OpenTelemetry 或 Tauri；
- `cargo check -p codex-monitor-host` 不依赖 Tauri 或 engine；
- 自动边界检查验证依赖方向、禁止依赖和无环图，并在本地验证与 CI 中执行。

## 影响

- storage、telemetry、Dashboard 和 host 可以独立编译和运行局部测试，日常反馈不再必然穿过完整产品组合。
- engine bridge 与 desktop 组合根成为具体适配器相遇的明确位置，需要产品集成测试覆盖真实 SQLite、loopback listeners、workers、宿主协调和关闭顺序。
- 增加跨 crate 能力时，应先判断契约由哪个消费方拥有；不能为减少调用方改动而恢复 engine façade。
- Cargo 依赖边界成为可执行契约，违反目标图的改动会在提交前验证和 CI 中失败。
