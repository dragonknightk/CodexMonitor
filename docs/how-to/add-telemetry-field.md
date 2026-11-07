# 增加监控字段

新增字段会改变 persistence allowlist，必须做显式隐私决策。

1. 在 `docs/_meta/telemetry-fields.yaml` 增加 field type、safe bounds、source paths、classification、retention、display purpose 与 safety boundary。
2. 在 `codex-monitor-core` 增加类型化可选 domain value 和 validation。禁止 arbitrary map 或 generic JSON escape hatch。
3. 需要持久化时追加 storage migration 并更新 projection query。已发布 migration 不得修改。
4. 更新 OpenAPI authority，重新生成 TypeScript types；可见字段增加本地化 label；notification exposure 单独审查。
5. 增加 valid value、invalid bound、precedence conflict、absent value、retransmission 与 forbidden neighboring content 测试。
6. 重新生成 telemetry 与 database references，运行 privacy suite。

字段只有同时出现在 telemetry catalog 与 `crates/codex-monitor-core/telemetry-persistence-fields.yaml` 时才能跨入 persistence boundary。
