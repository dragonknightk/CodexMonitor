# ADR 0005：SQLite writer actor 与 append-only migrations

[历史英文原件](0005-sqlite-writer-append-only-migrations.md)是不可变权威记录；本文件是中文伴随译本。

## 状态

2026-07-15 接受。

## 背景

Ingestion、settings、durable rules、action handoff 与 event publication 都修改同一个 current-user SQLite database。它们对 transaction 顺序和原子性有明确要求，由互不相关的 tasks 持有独立 write connection 时很难保证。Schema upgrade 还需要可审计历史，现有安装必须在 writer 打开前验证该历史。

## 决策

所有生产 mutation 通过 resident engine 拥有的一个 SQLite writer actor 串行执行。Read-only query 使用独立、有界 connection，不得产生 side effect。Schema 变化保存为连续、嵌入式、只向前的 SQL migrations。记录接受时 migration 位于 `crates/codex-monitor-engine/migrations`；[ADR 0007](0007-workspace-crate-boundaries.md) 已把同一字节权威迁至 `crates/codex-monitor-storage/migrations`。发布后的 migration version、name 与内容不可变，并以 checksum 验证；schema 变化只能追加下一项 migration。系统没有 down migration，未知、缺失或被修改的 migration history 会使数据库打开失败。

## 影响

- Transaction 顺序、Outbox insertion 与 post-commit event publication 具有单一 ownership boundary。
- Write throughput 有意串行，因此 command 与 shutdown 需要 bounded queue 和明确 transaction budget。
- Upgrade 只能向前；downgrade 与修改已接受 migration file 不受支持。
- 每次 schema 变化都要更新 migration authority，并重新生成 `docs/reference/database-schema.md`。
