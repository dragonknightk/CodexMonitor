# 增加 telemetry source

1. 先完成架构设计说明，明确 source identity、safe fields、checkpoint behavior、failure modes 与 privacy review。
2. 在 `codex-monitor-telemetry` 实现带版本的 `TelemetrySource` contract。先解码为该 crate 内部 protocol types，再构造类型化 core observations。Raw payload 与任意 attribute map 不能暴露给 storage。
3. 在 telemetry catalog 登记每个 emitted field，并定义 direct/inferred quality 及与现有来源的 precedence。
4. 定义稳定 event identity。来源没有 identifier 时，只用 allowlisted values 创建带版本 fingerprint test vectors。
5. 增加 success、retransmission、malformed input、unknown fields、limits 与 restart checkpoint 的 synthetic fixtures。加入 forbidden-content sentinels，并证明 SQLite、logs、API 与 notification 都不包含它们。
6. 把 source health 和 capability projection 加入本机 API 与生成 client，更新[已登记来源](../reference/telemetry-sources.md)。

实现完成后运行 telemetry 局部测试、privacy tests、generation checks 与 `pnpm docs:check`。跨 storage 的提交通过 telemetry 消费方拥有的 `ObservationSink` 和 engine bridge 完成，telemetry 不得依赖 `StorageWriter` 或 `rusqlite`。
