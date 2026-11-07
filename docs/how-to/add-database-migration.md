# 增加数据库 migration

在 `crates/codex-monitor-storage/migrations` 下创建下一项连续编号 SQL migration。已发布 migration 不可变；需要继续修改 schema 时追加 migration，不能编辑历史。SQL 嵌入生产二进制，version、name、bytes 与 checksum 必须一致。

Migration 在 telemetry listeners 启动前以 transaction 执行。至少测试空数据库、上一发布 schema、interruption rollback 和 checksum mismatch。字段相关 schema 变化还需要审查 telemetry catalog、repository、OpenAPI、生成参考与 retention。

不要把不可信 legacy SQL 放入 migration。Legacy importer 只检查 allowlisted schema，并通过 prepared statement 复制已验证值。

完成后运行：

```text
cargo test -p codex-monitor-storage migrations
node scripts/generate-database-schema.mjs
node scripts/generate-database-schema.mjs --check
pnpm check:privacy
pnpm docs:check
```
