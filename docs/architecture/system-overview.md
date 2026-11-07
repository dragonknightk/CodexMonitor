# 系统概览

CodexMonitor 是交互式桌面会话中的普通用户权限常驻进程。桌面产品只支持 Windows、macOS
和 Linux；其他操作系统不是受支持的 host 或打包目标。Tauri host 负责原生生命周期与操作
系统效果，Rust engine 负责两个独立 loopback HTTP listener、SQLite、durable workers 和
运行时组合。React Dashboard 由 Dashboard listener 提供，可以在普通浏览器中运行，不使用
Tauri JavaScript bridge。

## Workspace 边界

生产依赖方向如下：

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

codex-monitor-host          （无 Tauri）

codex-monitor-desktop
├── codex-monitor-engine
├── codex-monitor-host
└── 运行时 adapter 的公开契约
```

- `codex-monitor-core`：领域类型、ID、时间、provenance、规则/动作合同和 allowlisted observation。它不依赖 OpenTelemetry、SQLite、HTTP 或 Tauri。
- `codex-monitor-storage`：SQLite writer、repository、codec、projection、migrations、legacy import、retention、rule work、action Outbox、change feed、Dashboard 查询 SQL 与当前用户数据路径。
- `codex-monitor-telemetry`：OTLP protobuf/JSON 解码、gzip 与请求上限、类型化抽取、loopback receiver、source health、session index 与来源解析。Raw OTLP 永远留在该 crate 内。
- `codex-monitor-dashboard`：Axum listener、REST/SSE、session/bootstrap、CSRF/cookie、HTTP DTO、静态资源、settings 与 capability 展示语义。
- `codex-monitor-engine`：`ResidentEngine`、规则/action/retention workers、settings runtime、adapter bridges、启动组合和有序关闭。它不重导出 adapter 的旧模块路径。
- `codex-monitor-host`：启动参数、`HostCoordinator`、托盘视图模型和宿主生命周期状态机，只通过 `ResidentHostPort` 驱动具体 resident runtime。
- `codex-monitor-desktop`：唯一生产二进制，拥有 Tauri event loop、单实例、原生托盘、通知、登录自启动、浏览器打开和平台清理。

跨 adapter 使用消费方拥有的窄端口。Telemetry 定义 `ObservationSink`；Dashboard 定义查询、设置、capability 和 change-event 端口；host 定义 `ResidentHostPort`。端口只传闭合 DTO 与稳定错误码，engine 或 desktop 在组合根实现 bridge。Storage、telemetry 与 Dashboard 不互相依赖。

## 展示资源与 Dashboard

Desktop setup 只枚举一次 Tauri 构建期 `frontendDist`，转换为 engine 使用的有界 `DashboardAssets`。缺少 `index.html`、MIME 非法、路径重复或不规范、bundle 超限会在打开 storage 前失败。运行时不从安装目录、当前目录或 development server 读取展示文件。

Dashboard listener 在与 REST/SSE 相同的 loopback origin 上提供精确嵌入资源。未知 asset 和 API path 返回 `404`，不会 fallback 到 index。声明外的方法在认证之前返回 `405`。

`web/src/api` 包含 OpenAPI 生成 schema 和共享 same-origin session、CSRF、pagination 与稳定错误边界。业务代码位于 `web/src/features/calls`、`conversations`、`alerts`、`sources` 与 `settings`，通过顶部多工作区标签页（模型调用、推理告警、遥测来源、设置）和类型化 OpenAPI client 组织交互，不增加 Tauri IPC 或 untyped `fetch` 数据路径。

Model-call history 把可编辑 filter draft 与已应用查询分开。应用或清除 filter 会原子重置 cursor；summary 使用相同 conversation、model、source、time 和 alert filter，但不带分页与 sort。前进只使用服务端返回的 opaque `next_cursor`，后退重放客户端 cursor stack。Filter、page size 或 sort 改变时丢弃 stack。

Source diagnostics 在“遥测来源”工作区统一呈现有界 `SourceHealth` DTO 与接收器状态。安全错误码从闭合翻译目录选择解释与修复动作，不接收 backend 错误文本、路径或 payload 摘要。`127.0.0.1:4318` 冲突时显示释放端口的建议，并只在类型状态允许时显示 retry。

## 网络与会话

OTLP receiver 固定绑定 `127.0.0.1:4318`。Authenticated Dashboard listener 优先使用 `127.0.0.1:4319`，被占用时可以选择其他 loopback 端口。所有 listener 拒绝非 loopback 地址。CodexMonitor 没有 analytics、crash upload 或 monitoring-data forwarding。

每次浏览器打开获得一个 256-bit 单次 bootstrap credential，monotonic TTL 为 60 秒。成功消费后创建独立的 256-bit session ID 与 CSRF proof，固定 TTL 为 12 小时，然后设置 host-only `HttpOnly; SameSite=Strict` cookie 并重定向到 `/`。Credential map 只存在于进程内、有容量上限并以 SHA-256 digest 为键；重启、精确到期或确定性 eviction 都会失效。

最终 service boundary 先验证精确 active `Host`。受保护的 `/api/v1` read 需要当前 session；`DELETE`、`PATCH`、`POST` 和 `PUT` 还需要精确 `http://127.0.0.1:<active-port>` Origin 与该 session 的 CSRF proof。响应移除 CORS permission headers，敏感响应使用 `Cache-Control: no-store`。

SSE change stream 通过 storage change feed 提交 bounded typed event。Cursor 带版本、process generation 和认证 tag；过期、跨进程或缺失历史的 cursor 终结为 `stream-control`，要求客户端重新同步。订阅数、replay buffer、live buffer 和并行 materialization 都有固定上限，慢客户端不会造成无界内存。

## Storage 与 legacy import

所有 SQLite mutation 通过一个 bounded writer actor 串行执行；读查询使用独立只读连接。Embedded migrations 按连续版本和固定 checksum 校验，已应用 migration 的字节变化会拒绝打开数据库。Schema 权威位于 `crates/codex-monitor-storage/migrations`。

Legacy database 是独立的不可信文件边界。系统只接受 absolute、existing、non-symlink SQLite 文件，以 private read-only cache、`NOFOLLOW`、`query_only`、defensive mode、关闭 trigger/view、关闭 trusted schema、zero mmap 和缩减 limits 打开。Detection 在一个 read transaction 中运行固定 SQL，并受 instruction 与 wall-clock budget 约束。应用代码不会读取或执行 `sqlite_schema.sql` 中的存储文本。

Detector 区分当前 Rust schema、privacy-clean Python v1、旧 raw Python schema 和 unsupported 输入。当前 schema 通过 migration ledger 与字段 registry 识别；legacy 表、允许列与已知 index 逐项检查。普通额外列和旧 raw-record 表只影响分类，其值永远不读取。View、trigger、virtual/shadow table、generated column、foreign schema object 或超限 schema 会失败关闭。

Importer 只选择固定允许列。非法 token 字段逐个丢弃，剩余 token 全部未知的 row 忽略。无法区分 direct 与 backfilled 的旧 metadata 标记为 inferred。全源安全 fingerprint 提供 audit identity，版本化 per-row identity 允许追加 snapshot 只导入新调用。来源 row 删除不会删除已导入数据；同一 legacy row ID 的允许值改变会形成新的 historical call identity。

Conversation range、historical observation 与 privacy-safe audit 在一个 target transaction 中提交。Raw table、raw column、source path 和 stored SQL 不进入目标数据库，historical call 不创建 realtime rule work。

## Durable workers

Realtime completed-call observation 与唯一 `model_call.completed` 工作项在同一 writer transaction 中提交。规则 worker 以 fenced token claim 工作，在独立 reader 加载 canonical call，并在完成 transaction 中重新验证。崩溃前未完成的工作可重新 claim；相同结果重放幂等，不同结果失败关闭。

每个匹配 evaluation 在同一 transaction 创建 alert。Alert 保存确切规则版本、reasoning-token evidence、pattern number、event/observation time、source 与 quality。规则设置由版本化 `rules` row 管理，全局系统通知开关属于 singleton application settings。

Eligible alert 与 `system_notification` request 在同一 transaction 进入 action Outbox。通知关闭时创建 terminal `suppressed` audit；超过 rule threshold 时不创建 action。执行参数只允许可选 model、reasoning-token count 与 pattern number。

已知 canonical conversation 使用固定 30 秒半开 coalescing window。Leader 在窗口关闭时可执行，followers 各自以 `coalesced` 终态保留并链接 leader。Action worker 使用 opaque owner 与递增 generation lease `pending` row；调用 host 前再次检查通知设置，并原子写入 `started` 与 attempt。Executor 只在 transaction commit 后调用。

完成结果将 action 与 attempt 原子写为 `completed`、`failed` 或 `unavailable`。正常 claim 不会把 `started` 返回 `pending`，避免含糊 effect window 中重复通知。重启 recovery 在当前 worker 启动前把遗留 `started` attempt 标记为 `indeterminate`，action 标记为 `failed`，并记录 `indeterminate_after_crash`。

## Settings、平台能力与关闭

唯一 `SettingsRuntime` 对 full replacement 与 targeted mutation 串行化，使用 revision compare-and-swap。持久设置包括 retention、通知开关、rule threshold、locale 与 login autostart。Receiver 重试状态属于 runtime capability，不进入 atomic settings document。

平台能力是进程内 live state。Dashboard 与 tray 的 clones 观察同一 login-autostart、system-notification 与 tray snapshot；`/api/v1/capabilities` 只暴露闭合值。原生错误文本在进入 engine 之前折叠。

`ResidentEngine` 只有在 migrations、首次 locale、platform reconciliation、规则登记、started-action recovery、两个 listeners 和所有 workers 完成组合后才 ready。关闭顺序为 receiver、workers、Dashboard、已取得的 settings mutation、SQLite writer。所有生产者先封闭，共享 monotonic deadline，并保留 async abort/join 与最终 SQLite close 的固定尾部预算。

详细状态转换与平台退出行为见[常驻进程生命周期](process-lifecycle.md)，crate 边界决策见 [ADR 0007](../decisions/0007-workspace-crate-boundaries.md)。
