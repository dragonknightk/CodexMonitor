# ADR 0005: SQLite writer actor and append-only migrations

## Status

Accepted on 2026-07-15.

## Context

Ingestion, settings, durable rules, action handoff and event publication all mutate
one current-user SQLite database. Their transactions have ordering and atomicity
requirements that are difficult to preserve when unrelated tasks own independent
write connections. Schema upgrades also need an auditable history that existing
installations can verify before opening the writer.

## Decision

Serialize every production mutation through one SQLite writer actor owned by the
resident engine. Read-only queries use separately bounded connections and may not
perform side effects. Store schema changes as contiguous, embedded, forward-only
SQL migrations under `crates/codex-monitor-engine/migrations`. Once released, a
migration's version, name and contents are immutable and verified by checksum; a
schema change adds the next migration. There are no down migrations, and opening a
database with unknown, missing or changed migration history fails closed.

## Consequences

- Transaction order, outbox insertion and post-commit event publication have one
  ownership boundary.
- Write throughput is deliberately serialized, so commands and shutdown need
  bounded queues and explicit transaction budgets.
- Upgrades can move a database only forward; downgrades and edits to accepted
  migration files are unsupported.
- Every schema change must update the migration authority and regenerate
  `docs/reference/database-schema.md`.
