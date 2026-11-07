# 增加 action

新增 action executor 必须执行显式架构与权限审查。定义稳定 action type、版本化 parameter schema、平台 capability、timeout、retry policy 和幂等行为。参数只能使用结构化闭合类型，不能把来源文本作为 shell、script、executable path 或 unrestricted URL。

Outbox row 必须与 alert 或状态转换在同一 transaction 创建。持久状态包含 `pending`、`started` 和且仅一个 terminal state。External transmission、process control、额外文件访问或控制其他应用默认关闭，启用时需要按 capability 范围保存用户授权。

`system_notification` 是 v1 唯一产生 effect 的 action。未来 executor 登记到生产前必须增加 permission、crash recovery、unavailable platform 与 privacy 测试。除非自身 specification 明确定义等价 grouping 语义，否则不能继承 notification coalescing window。

实现位于 `codex-monitor-core` 的 action registry、`codex-monitor-engine` 的 durable worker/bridge 和 `codex-monitor-desktop` 的原生 effect 边界；不要增加字符串 dispatch 或 fallback executor。
