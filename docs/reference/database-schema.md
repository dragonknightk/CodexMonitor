<!-- 由 scripts/generate-database-schema.mjs 生成。请勿直接编辑。 -->

# 数据库模式

本参考文档根据 `crates/codex-monitor-storage/migrations/` 中按顺序执行的 SQL
迁移生成。这些迁移是机器可读权威。使用 `pnpm generate:references` 重新生成；
使用 `node scripts/generate-database-schema.mjs --check` 进行不写文件的验证。

生成器采用与 Rust 迁移运行时相同的校验和规范化方式：计算 SHA-256 和字节长度前，
每个 CRLF 序列都会转换为 LF；单独出现的 CR 不会被改写。

隐私边界：本文档只描述存储结构。列的存在不代表允许采集。只有遥测字段目录中
已注册的字段可以跨越持久化边界。原始 OTLP 正文、提示词、响应、工具数据、
推理内容、路径、账户标识符和认证材料绝不能持久化或记录到日志。

## 迁移清单

<!-- prettier-ignore -->
| 版本 | 文件 | SHA-256 | 规范化字节数 |
| ---: | --- | --- | ---: |
| 1 | `0001_storage.sql` | `326bb984b1175f17cf72cd00921602b5b2cadbafac50ce0df90e66a6288e7853` | 17784 |
| 2 | `0002_rules_actions_settings.sql` | `d22cf1fc158d2602737ed87b862afc1d19a40ef63580d5fa38d44d208c79ffb4` | 6317 |
| 3 | `0003_rule_definition_fingerprints.sql` | `890c2313b5429c98ada2c415f3444ebd97190a1de3cea6ab8b60c714e8eb1ad1` | 972 |
| 4 | `0004_alert_evidence_and_threshold_invariants.sql` | `08e204655c183042181af5c2899f295a0ef64c9de1ec00f1584d86d905c00346` | 2273 |
| 5 | `0005_historical_action_isolation.sql` | `b61ddbafbf4ab885342b7bbb74e48f61ce937e5635c9f096c63773e171f256b7` | 3558 |
| 6 | `0006_transactional_action_outbox.sql` | `31c7b480ffea01454a7fd14e9836ea8022f200898df11f841ed2158aa0ac1c47` | 16935 |
| 7 | `0007_notification_coalescing.sql` | `8e640623e0843f971b1d4b46cb7133ac7a1bb7fd5356b90bb9ce48024b1d2bf2` | 7686 |
| 8 | `0008_dashboard_query_projection.sql` | `22bde9e2fdb9d2b48f2f68044f3bf7d2cca2b1c1c243bc2afee4905bc583a44b` | 9520 |
| 9 | `0009_locale_initialization.sql` | `255c292d723ac4004a7cb2badf4f5e54d7d4db0bc7377582c4576ab0a108de5d` | 440 |

## 模式概览

- 表：27
- 显式索引：33
- 触发器：31

下方各表按迁移创建顺序排列。列声明包含内联的可空性、唯一性、引用、默认值和检查。
单独的表级约束以规范化 SQL 形式原样列出。

## 表

### `schema_migrations`

迁移：1（`0001_storage.sql`）。表选项：`STRICT`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `version` | `INTEGER PRIMARY KEY CHECK (version > 0)` |
| `name` | `TEXT NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 128)` |
| `sha256` | `TEXT NOT NULL CHECK (length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*')` |
| `sql_bytes` | `INTEGER NOT NULL CHECK (sql_bytes > 0)` |
| `applied_at` | `TEXT NOT NULL` |

表级约束：

- 除列声明外没有其他约束。

### `telemetry_field_registry`

迁移：1（`0001_storage.sql`）。表选项：`STRICT, WITHOUT ROWID`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `field_id` | `TEXT PRIMARY KEY` |
| `persisted` | `INTEGER NOT NULL CHECK (persisted IN (0, 1))` |

表级约束：

- 除列声明外没有其他约束。

### `conversations`

迁移：1（`0001_storage.sql`）。表选项：`STRICT`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `id` | `TEXT PRIMARY KEY` |
| `source_id` | `TEXT` |
| `source_provenance_source_id` | `TEXT` |
| `source_provenance_contract_version` | `INTEGER` |
| `source_provenance_observed_at` | `TEXT` |
| `source_quality` | `TEXT CHECK (source_quality IN ('inferred', 'direct'))` |
| `source_priority` | `INTEGER CHECK (source_priority BETWEEN 0 AND 65535)` |
| `thread_id` | `TEXT` |
| `thread_provenance_source_id` | `TEXT` |
| `thread_provenance_contract_version` | `INTEGER` |
| `thread_provenance_observed_at` | `TEXT` |
| `thread_quality` | `TEXT CHECK (thread_quality IN ('inferred', 'direct'))` |
| `thread_priority` | `INTEGER CHECK (thread_priority BETWEEN 0 AND 65535)` |
| `first_observed_at` | `TEXT NOT NULL` |
| `last_observed_at` | `TEXT NOT NULL` |

表级约束：

- `CHECK (first_observed_at <= last_observed_at)`
- `CHECK ( (source_id IS NULL AND source_provenance_source_id IS NULL AND source_provenance_contract_version IS NULL AND source_provenance_observed_at IS NULL AND source_quality IS NULL AND source_priority IS NULL) OR (source_id IS NOT NULL AND source_provenance_source_id IS NOT NULL AND source_provenance_contract_version > 0 AND source_provenance_observed_at IS NOT NULL AND source_quality IS NOT NULL AND source_priority IS NOT NULL) )`
- `CHECK ( (thread_id IS NULL AND thread_provenance_source_id IS NULL AND thread_provenance_contract_version IS NULL AND thread_provenance_observed_at IS NULL AND thread_quality IS NULL AND thread_priority IS NULL) OR (thread_id IS NOT NULL AND thread_provenance_source_id IS NOT NULL AND thread_provenance_contract_version > 0 AND thread_provenance_observed_at IS NOT NULL AND thread_quality IS NOT NULL AND thread_priority IS NOT NULL) )`

### `conversation_source_keys`

迁移：1（`0001_storage.sql`）。表选项：`STRICT, WITHOUT ROWID`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `telemetry_source_id` | `TEXT NOT NULL` |
| `source_contract_version` | `INTEGER NOT NULL CHECK (source_contract_version > 0)` |
| `key_kind` | `TEXT NOT NULL CHECK (key_kind IN ('conversation', 'thread'))` |
| `key_value` | `TEXT NOT NULL CHECK (length(key_value) BETWEEN 1 AND 512)` |
| `conversation_id` | `TEXT NOT NULL` |
| `first_observed_at` | `TEXT NOT NULL` |
| `last_observed_at` | `TEXT NOT NULL` |

表级约束：

- `PRIMARY KEY (telemetry_source_id, source_contract_version, key_kind, key_value)`
- `FOREIGN KEY (conversation_id) REFERENCES conversations (id) ON DELETE CASCADE`
- `CHECK (first_observed_at <= last_observed_at)`

### `turns`

迁移：1（`0001_storage.sql`）。表选项：`STRICT`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `id` | `TEXT PRIMARY KEY` |
| `conversation_id` | `TEXT NOT NULL` |
| `source_id` | `TEXT` |
| `source_provenance_source_id` | `TEXT` |
| `source_provenance_contract_version` | `INTEGER` |
| `source_provenance_observed_at` | `TEXT` |
| `source_quality` | `TEXT CHECK (source_quality IN ('inferred', 'direct'))` |
| `source_priority` | `INTEGER CHECK (source_priority BETWEEN 0 AND 65535)` |
| `event_at` | `TEXT` |
| `event_provenance_source_id` | `TEXT` |
| `event_provenance_contract_version` | `INTEGER` |
| `event_provenance_observed_at` | `TEXT` |
| `event_quality` | `TEXT CHECK (event_quality IN ('inferred', 'direct'))` |
| `event_priority` | `INTEGER CHECK (event_priority BETWEEN 0 AND 65535)` |

表级约束：

- `UNIQUE (id, conversation_id)`
- `FOREIGN KEY (conversation_id) REFERENCES conversations (id) ON DELETE CASCADE`
- `CHECK ( (source_id IS NULL AND source_provenance_source_id IS NULL AND source_provenance_contract_version IS NULL AND source_provenance_observed_at IS NULL AND source_quality IS NULL AND source_priority IS NULL) OR (source_id IS NOT NULL AND source_provenance_source_id IS NOT NULL AND source_provenance_contract_version > 0 AND source_provenance_observed_at IS NOT NULL AND source_quality IS NOT NULL AND source_priority IS NOT NULL) )`
- `CHECK ( (event_at IS NULL AND event_provenance_source_id IS NULL AND event_provenance_contract_version IS NULL AND event_provenance_observed_at IS NULL AND event_quality IS NULL AND event_priority IS NULL) OR (event_at IS NOT NULL AND event_provenance_source_id IS NOT NULL AND event_provenance_contract_version > 0 AND event_provenance_observed_at IS NOT NULL AND event_quality IS NOT NULL AND event_priority IS NOT NULL) )`

### `model_calls`

迁移：1（`0001_storage.sql`）。表选项：`STRICT`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `id` | `TEXT PRIMARY KEY` |
| `canonical_source_id` | `TEXT NOT NULL` |
| `canonical_source_contract_version` | `INTEGER NOT NULL CHECK (canonical_source_contract_version > 0)` |
| `canonical_identity_version` | `INTEGER NOT NULL CHECK (canonical_identity_version > 0)` |
| `canonical_identity_kind` | `TEXT NOT NULL CHECK (canonical_identity_kind IN ('stable', 'fallback_fingerprint'))` |
| `canonical_identity_digest` | `BLOB NOT NULL CHECK (length(canonical_identity_digest) = 32)` |
| `event_at` | `TEXT NOT NULL` |
| `event_time_observation_id` | `TEXT NOT NULL` |
| `relationship_kind` | `TEXT CHECK (relationship_kind IN ('conversation', 'turn'))` |
| `conversation_id` | `TEXT` |
| `turn_id` | `TEXT` |
| `relationship_observation_id` | `TEXT` |
| `origin` | `TEXT NOT NULL CHECK (origin IN ('realtime', 'historical'))` |

表级约束：

- `UNIQUE ( canonical_source_id, canonical_source_contract_version, canonical_identity_version, canonical_identity_kind, canonical_identity_digest )`
- `UNIQUE (id, event_time_observation_id)`
- `FOREIGN KEY (conversation_id) REFERENCES conversations (id)`
- `FOREIGN KEY (turn_id, conversation_id) REFERENCES turns (id, conversation_id)`
- `FOREIGN KEY (event_time_observation_id, id) REFERENCES model_call_observations (id, model_call_id) DEFERRABLE INITIALLY DEFERRED`
- `FOREIGN KEY (relationship_observation_id, id) REFERENCES model_call_observation_relationships (observation_id, model_call_id) DEFERRABLE INITIALLY DEFERRED`
- `CHECK ( (relationship_kind IS NULL AND conversation_id IS NULL AND turn_id IS NULL AND relationship_observation_id IS NULL) OR (relationship_kind = 'conversation' AND conversation_id IS NOT NULL AND turn_id IS NULL AND relationship_observation_id IS NOT NULL) OR (relationship_kind = 'turn' AND conversation_id IS NOT NULL AND turn_id IS NOT NULL AND relationship_observation_id IS NOT NULL) )`

### `model_call_observations`

迁移：1（`0001_storage.sql`）。表选项：`STRICT`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `id` | `TEXT PRIMARY KEY` |
| `model_call_id` | `TEXT NOT NULL` |
| `source_id` | `TEXT NOT NULL` |
| `source_contract_version` | `INTEGER NOT NULL CHECK (source_contract_version > 0)` |
| `identity_version` | `INTEGER NOT NULL CHECK (identity_version > 0)` |
| `identity_kind` | `TEXT NOT NULL CHECK (identity_kind IN ('stable', 'fallback_fingerprint'))` |
| `identity_digest` | `BLOB NOT NULL CHECK (length(identity_digest) = 32)` |
| `event_at` | `TEXT NOT NULL` |
| `event_provenance_observed_at` | `TEXT NOT NULL` |
| `event_quality` | `TEXT NOT NULL CHECK (event_quality IN ('inferred', 'direct'))` |
| `event_priority` | `INTEGER NOT NULL CHECK (event_priority BETWEEN 0 AND 65535)` |
| `observed_at` | `TEXT NOT NULL` |
| `origin` | `TEXT NOT NULL CHECK (origin IN ('realtime', 'historical'))` |

表级约束：

- `UNIQUE (id, model_call_id)`
- `UNIQUE ( source_id, source_contract_version, identity_version, identity_kind, identity_digest )`
- `FOREIGN KEY (model_call_id) REFERENCES model_calls (id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED`

### `model_call_observation_relationships`

迁移：1（`0001_storage.sql`）。表选项：`STRICT`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `observation_id` | `TEXT PRIMARY KEY` |
| `model_call_id` | `TEXT NOT NULL` |
| `relationship_kind` | `TEXT NOT NULL CHECK (relationship_kind IN ('conversation', 'turn'))` |
| `conversation_id` | `TEXT NOT NULL` |
| `turn_id` | `TEXT` |
| `provenance_observed_at` | `TEXT NOT NULL` |
| `quality` | `TEXT NOT NULL CHECK (quality IN ('inferred', 'direct'))` |
| `source_priority` | `INTEGER NOT NULL CHECK (source_priority BETWEEN 0 AND 65535)` |

表级约束：

- `UNIQUE (observation_id, model_call_id)`
- `FOREIGN KEY (observation_id, model_call_id) REFERENCES model_call_observations (id, model_call_id) ON DELETE CASCADE`
- `FOREIGN KEY (conversation_id) REFERENCES conversations (id)`
- `FOREIGN KEY (turn_id, conversation_id) REFERENCES turns (id, conversation_id)`
- `CHECK ( (relationship_kind = 'conversation' AND turn_id IS NULL) OR (relationship_kind = 'turn' AND turn_id IS NOT NULL) )`

### `model_call_observation_fields`

迁移：1（`0001_storage.sql`）。表选项：`STRICT, WITHOUT ROWID`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `observation_id` | `TEXT NOT NULL` |
| `field_id` | `TEXT NOT NULL` |
| `text_value` | `TEXT` |
| `integer_value` | `INTEGER` |
| `provenance_observed_at` | `TEXT NOT NULL` |
| `quality` | `TEXT NOT NULL CHECK (quality IN ('inferred', 'direct'))` |
| `source_priority` | `INTEGER NOT NULL CHECK (source_priority BETWEEN 0 AND 65535)` |

表级约束：

- `PRIMARY KEY (observation_id, field_id)`
- `FOREIGN KEY (observation_id) REFERENCES model_call_observations (id) ON DELETE CASCADE`
- `FOREIGN KEY (field_id) REFERENCES telemetry_field_registry (field_id)`
- `CHECK (field_id IN ( 'conversation.source_id', 'conversation.thread_id', 'turn.source_id', 'model_call.model', 'model_call.reasoning_effort', 'model_call.input_tokens', 'model_call.cached_input_tokens', 'model_call.output_tokens', 'model_call.reasoning_output_tokens', 'model_call.surface', 'model_call.client', 'model_call.event_name', 'model_call.stream_event_kind' ))`
- `CHECK ( (field_id IN ( 'model_call.input_tokens', 'model_call.cached_input_tokens', 'model_call.output_tokens', 'model_call.reasoning_output_tokens' ) AND integer_value BETWEEN 0 AND 1000000000000000 AND text_value IS NULL) OR (field_id NOT IN ( 'model_call.input_tokens', 'model_call.cached_input_tokens', 'model_call.output_tokens', 'model_call.reasoning_output_tokens' ) AND text_value IS NOT NULL AND integer_value IS NULL) )`

### `model_call_projection_fields`

迁移：1（`0001_storage.sql`）。表选项：`STRICT, WITHOUT ROWID`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `model_call_id` | `TEXT NOT NULL` |
| `field_id` | `TEXT NOT NULL` |
| `observation_id` | `TEXT NOT NULL` |

表级约束：

- `PRIMARY KEY (model_call_id, field_id)`
- `FOREIGN KEY (model_call_id) REFERENCES model_calls (id) ON DELETE CASCADE`
- `FOREIGN KEY (observation_id, model_call_id) REFERENCES model_call_observations (id, model_call_id) ON DELETE CASCADE`
- `FOREIGN KEY (observation_id, field_id) REFERENCES model_call_observation_fields (observation_id, field_id) ON DELETE CASCADE`

### `source_checkpoints`

迁移：1（`0001_storage.sql`）。表选项：`STRICT, WITHOUT ROWID`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `source_id` | `TEXT NOT NULL` |
| `source_contract_version` | `INTEGER NOT NULL CHECK (source_contract_version > 0)` |
| `checkpoint_kind` | `TEXT NOT NULL CHECK (checkpoint_kind IN ('otlp_receiver', 'sessions_file'))` |
| `generation` | `INTEGER NOT NULL CHECK (generation >= 0)` |
| `offset_bytes` | `INTEGER CHECK (offset_bytes >= 0)` |
| `fingerprint_digest` | `BLOB CHECK (fingerprint_digest IS NULL OR length(fingerprint_digest) = 32)` |
| `updated_at` | `TEXT NOT NULL` |

表级约束：

- `PRIMARY KEY (source_id, source_contract_version)`

### `source_health`

迁移：1（`0001_storage.sql`）。表选项：`STRICT, WITHOUT ROWID`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `source_id` | `TEXT NOT NULL` |
| `source_contract_version` | `INTEGER NOT NULL CHECK (source_contract_version > 0)` |
| `status` | `TEXT NOT NULL CHECK (status IN ('starting', 'healthy', 'degraded', 'unavailable', 'stopped'))` |
| `last_accepted_at` | `TEXT` |
| `last_error_code` | `TEXT CHECK (last_error_code IN ( 'otlp_bind_failed', 'otlp_invalid_content_type', 'otlp_compressed_body_too_large', 'otlp_decompressed_body_too_large', 'otlp_gzip_decode_failed', 'otlp_json_decode_failed', 'otlp_protobuf_decode_failed', 'otlp_decode_timed_out', 'persistence_commit_failed', 'source_checkpoint_invalid', 'source_unavailable', 'internal_queue_closed' ))` |
| `last_error_occurred_at` | `TEXT` |
| `updated_at` | `TEXT NOT NULL` |

表级约束：

- `PRIMARY KEY (source_id, source_contract_version)`
- `CHECK ( (last_error_code IS NULL AND last_error_occurred_at IS NULL) OR (last_error_code IS NOT NULL AND last_error_occurred_at IS NOT NULL) )`

### `source_health_supported_fields`

迁移：1（`0001_storage.sql`）。表选项：`STRICT, WITHOUT ROWID`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `source_id` | `TEXT NOT NULL` |
| `source_contract_version` | `INTEGER NOT NULL` |
| `field_id` | `TEXT NOT NULL` |

表级约束：

- `PRIMARY KEY (source_id, source_contract_version, field_id)`
- `FOREIGN KEY (source_id, source_contract_version) REFERENCES source_health (source_id, source_contract_version) ON DELETE CASCADE`
- `FOREIGN KEY (field_id) REFERENCES telemetry_field_registry (field_id)`

### `domain_work`

迁移：1（`0001_storage.sql`）。表选项：`STRICT`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `id` | `TEXT PRIMARY KEY` |
| `work_kind` | `TEXT NOT NULL CHECK (work_kind = 'model_call.completed')` |
| `model_call_id` | `TEXT NOT NULL` |
| `state` | `TEXT NOT NULL CHECK (state IN ('pending', 'started', 'completed'))` |
| `created_at` | `TEXT NOT NULL` |
| `started_at` | `TEXT` |
| `completed_at` | `TEXT` |

表级约束：

- `UNIQUE (work_kind, model_call_id)`
- `FOREIGN KEY (model_call_id) REFERENCES model_calls (id) ON DELETE CASCADE`
- `CHECK ( (state = 'pending' AND started_at IS NULL AND completed_at IS NULL) OR (state = 'started' AND started_at IS NOT NULL AND completed_at IS NULL) OR (state = 'completed' AND started_at IS NOT NULL AND completed_at IS NOT NULL) )`

### `rules`

迁移：2（`0002_rules_actions_settings.sql`）。表选项：`STRICT, WITHOUT ROWID`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `rule_id` | `TEXT NOT NULL` |
| `rule_version` | `INTEGER NOT NULL CHECK (rule_version > 0)` |
| `rule_kind` | `TEXT NOT NULL CHECK (rule_kind = 'suspected_reasoning_limit_pattern')` |
| `enabled` | `INTEGER NOT NULL CHECK (enabled IN (0, 1))` |
| `notify_max_n` | `INTEGER CHECK (notify_max_n IS NULL OR notify_max_n > 0)` |
| `notify_all` | `INTEGER NOT NULL CHECK (notify_all IN (0, 1))` |
| `last_evaluated_at` | `TEXT` |
| `last_error_code` | `TEXT CHECK (last_error_code IS NULL OR last_error_code IN ( 'persistence_commit_failed', 'internal_queue_closed' ))` |

表级约束：

- `PRIMARY KEY (rule_id, rule_version)`
- `CHECK (NOT (notify_all = 1 AND notify_max_n IS NOT NULL))`

### `rule_evaluations`

迁移：2（`0002_rules_actions_settings.sql`）。表选项：`STRICT`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `id` | `TEXT PRIMARY KEY` |
| `rule_id` | `TEXT NOT NULL` |
| `rule_version` | `INTEGER NOT NULL` |
| `model_call_id` | `TEXT NOT NULL` |
| `matched` | `INTEGER NOT NULL CHECK (matched IN (0, 1))` |
| `reasoning_output_tokens` | `INTEGER CHECK (reasoning_output_tokens BETWEEN 0 AND 1000000000000000)` |
| `pattern_n` | `INTEGER CHECK (pattern_n IS NULL OR pattern_n > 0)` |
| `event_at` | `TEXT NOT NULL` |
| `source_quality` | `TEXT CHECK (source_quality IN ('inferred', 'direct'))` |
| `evaluated_at` | `TEXT NOT NULL` |

表级约束：

- `UNIQUE (rule_id, rule_version, model_call_id)`
- `UNIQUE (id, model_call_id, rule_id, rule_version)`
- `FOREIGN KEY (rule_id, rule_version) REFERENCES rules (rule_id, rule_version)`
- `FOREIGN KEY (model_call_id) REFERENCES model_calls (id) ON DELETE CASCADE`
- `CHECK ( (matched = 0 AND pattern_n IS NULL) OR (matched = 1 AND reasoning_output_tokens IS NOT NULL AND pattern_n IS NOT NULL) )`

### `alerts`

迁移：2（`0002_rules_actions_settings.sql`）。表选项：`STRICT`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `id` | `TEXT PRIMARY KEY` |
| `evaluation_id` | `TEXT NOT NULL UNIQUE` |
| `model_call_id` | `TEXT NOT NULL` |
| `rule_id` | `TEXT NOT NULL` |
| `rule_version` | `INTEGER NOT NULL` |
| `reasoning_output_tokens` | `INTEGER NOT NULL CHECK (reasoning_output_tokens BETWEEN 0 AND 1000000000000000)` |
| `pattern_n` | `INTEGER NOT NULL CHECK (pattern_n > 0)` |
| `event_at` | `TEXT NOT NULL` |
| `source_quality` | `TEXT NOT NULL CHECK (source_quality IN ('inferred', 'direct'))` |
| `created_at` | `TEXT NOT NULL` |

表级约束：

- `FOREIGN KEY (evaluation_id, model_call_id, rule_id, rule_version) REFERENCES rule_evaluations (id, model_call_id, rule_id, rule_version) ON DELETE CASCADE`

### `app_settings`

迁移：2（`0002_rules_actions_settings.sql`）。表选项：`STRICT`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `singleton` | `INTEGER PRIMARY KEY CHECK (singleton = 1)` |
| `revision` | `INTEGER NOT NULL CHECK (revision > 0)` |
| `retention_days` | `INTEGER CHECK (retention_days IS NULL OR retention_days IN (30, 90, 365))` |
| `notifications_enabled` | `INTEGER NOT NULL CHECK (notifications_enabled IN (0, 1))` |
| `notify_max_n` | `INTEGER CHECK (notify_max_n IS NULL OR notify_max_n > 0)` |
| `notify_all` | `INTEGER NOT NULL CHECK (notify_all IN (0, 1))` |
| `locale` | `TEXT NOT NULL CHECK (locale IN ('en-US', 'zh-CN'))` |
| `login_autostart` | `INTEGER NOT NULL CHECK (login_autostart IN (0, 1))` |
| `updated_at` | `TEXT NOT NULL` |

表级约束：

- `CHECK (NOT (notify_all = 1 AND notify_max_n IS NOT NULL))`

### `legacy_import_audit`

迁移：2（`0002_rules_actions_settings.sql`）。表选项：`STRICT`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `id` | `TEXT PRIMARY KEY` |
| `source_fingerprint` | `BLOB NOT NULL UNIQUE CHECK (length(source_fingerprint) = 32)` |
| `detected_schema` | `TEXT NOT NULL CHECK (detected_schema IN ('structured_v1', 'raw_legacy'))` |
| `imported_model_calls` | `INTEGER NOT NULL CHECK (imported_model_calls >= 0)` |
| `imported_conversations` | `INTEGER NOT NULL CHECK (imported_conversations >= 0)` |
| `completed_at` | `TEXT NOT NULL` |

表级约束：

- 除列声明外没有其他约束。

### `rule_definition_fingerprints`

迁移：3（`0003_rule_definition_fingerprints.sql`）。表选项：`STRICT, WITHOUT ROWID`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `rule_id` | `TEXT NOT NULL` |
| `rule_version` | `INTEGER NOT NULL CHECK (rule_version > 0)` |
| `semantic_sha256` | `BLOB NOT NULL CHECK (length(semantic_sha256) = 32)` |

表级约束：

- `PRIMARY KEY (rule_id, rule_version)`
- `FOREIGN KEY (rule_id, rule_version) REFERENCES rules (rule_id, rule_version) ON DELETE CASCADE`

### `action_outbox`

迁移：6（`0006_transactional_action_outbox.sql`）。表选项：`STRICT`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `id` | `TEXT PRIMARY KEY` |
| `alert_id` | `TEXT NOT NULL` |
| `action_type` | `TEXT NOT NULL CHECK ( length(action_type) BETWEEN 1 AND 64 AND substr(action_type, 1, 1) GLOB '[a-z]' AND substr(action_type, -1, 1) GLOB '[a-z0-9]' AND action_type NOT GLOB '*[^a-z0-9_]*' AND action_type NOT GLOB '*__*' )` |
| `parameter_schema_version` | `INTEGER NOT NULL CHECK (parameter_schema_version BETWEEN 1 AND 65535)` |
| `idempotency_key_version` | `INTEGER NOT NULL CHECK (idempotency_key_version BETWEEN 0 AND 65535)` |
| `idempotency_key_bytes` | `BLOB NOT NULL` |
| `state` | `TEXT NOT NULL CHECK ( state IN ('pending', 'started', 'completed', 'failed', 'suppressed', 'unavailable') )` |
| `model` | `TEXT CHECK (model IS NULL OR length(model) BETWEEN 1 AND 128)` |
| `reasoning_output_tokens` | `INTEGER NOT NULL CHECK (reasoning_output_tokens BETWEEN 0 AND 1000000000000000)` |
| `pattern_n` | `INTEGER NOT NULL CHECK (pattern_n > 0)` |
| `created_at` | `TEXT NOT NULL` |
| `not_before` | `TEXT NOT NULL` |
| `lease_generation` | `INTEGER NOT NULL DEFAULT 0 CHECK (lease_generation >= 0)` |
| `lease_owner` | `TEXT CHECK (lease_owner IS NULL OR length(lease_owner) BETWEEN 1 AND 128)` |
| `lease_expires_at` | `TEXT` |
| `started_at` | `TEXT` |
| `completed_at` | `TEXT` |
| `last_error_code` | `TEXT CHECK (last_error_code IS NULL OR last_error_code IN ( 'invalid_parameters', 'permission_denied', 'notification_unavailable', 'notification_dispatch_failed', 'indeterminate_after_crash', 'unknown_action_type', 'internal_queue_closed' ))` |
| `suppression_reason` | `TEXT CHECK (suppression_reason IS NULL OR suppression_reason IN ( 'notifications_disabled', 'above_threshold', 'permission_denied', 'coalesced', 'historical_event', 'pre_outbox_upgrade' ))` |

表级约束：

- `UNIQUE (idempotency_key_version, idempotency_key_bytes)`
- `UNIQUE (alert_id, action_type, parameter_schema_version)`
- `UNIQUE (id, lease_generation)`
- `FOREIGN KEY (alert_id) REFERENCES alerts (id) ON DELETE CASCADE`
- `CHECK (created_at <= not_before)`
- `CHECK (started_at IS NULL OR started_at >= created_at)`
- `CHECK (completed_at IS NULL OR completed_at >= COALESCE(started_at, created_at))`
- `CHECK ( (idempotency_key_version = 1 AND length(idempotency_key_bytes) = 32) OR (idempotency_key_version = 0 AND length(idempotency_key_bytes) BETWEEN 1 AND 256 AND state IN ('completed', 'failed', 'suppressed', 'unavailable')) )`
- `CHECK ( (state = 'pending' AND started_at IS NULL AND completed_at IS NULL AND last_error_code IS NULL AND suppression_reason IS NULL) OR (state = 'started' AND started_at IS NOT NULL AND completed_at IS NULL AND last_error_code IS NULL AND suppression_reason IS NULL) OR (state = 'completed' AND started_at IS NOT NULL AND completed_at IS NOT NULL AND last_error_code IS NULL AND suppression_reason IS NULL) OR (state = 'failed' AND started_at IS NOT NULL AND completed_at IS NOT NULL AND last_error_code IS NOT NULL AND suppression_reason IS NULL) OR (state = 'suppressed' AND started_at IS NULL AND completed_at IS NOT NULL AND last_error_code IS NULL AND suppression_reason IS NOT NULL) OR (state = 'unavailable' AND completed_at IS NOT NULL AND last_error_code IS NOT NULL AND suppression_reason IS NULL) )`
- `CHECK ( (state = 'pending' AND ( (lease_owner IS NULL AND lease_expires_at IS NULL) OR (lease_generation > 0 AND lease_owner IS NOT NULL AND lease_expires_at IS NOT NULL AND lease_expires_at > not_before) )) OR (state = 'started' AND lease_generation > 0 AND lease_owner IS NOT NULL AND lease_expires_at IS NOT NULL AND lease_expires_at > not_before) OR (state IN ('completed', 'failed', 'suppressed', 'unavailable') AND lease_owner IS NULL AND lease_expires_at IS NULL) )`

### `action_attempts`

迁移：6（`0006_transactional_action_outbox.sql`）。表选项：`STRICT, WITHOUT ROWID`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `action_id` | `TEXT NOT NULL` |
| `attempt_number` | `INTEGER NOT NULL CHECK (attempt_number > 0)` |
| `lease_generation` | `INTEGER NOT NULL CHECK (lease_generation >= 0)` |
| `state` | `TEXT NOT NULL CHECK ( state IN ('started', 'completed', 'failed', 'unavailable', 'indeterminate') )` |
| `started_at` | `TEXT NOT NULL` |
| `completed_at` | `TEXT` |
| `safe_error_code` | `TEXT CHECK (safe_error_code IS NULL OR safe_error_code IN ( 'invalid_parameters', 'permission_denied', 'notification_unavailable', 'notification_dispatch_failed', 'indeterminate_after_crash', 'unknown_action_type', 'internal_queue_closed' ))` |

表级约束：

- `PRIMARY KEY (action_id, attempt_number)`
- `FOREIGN KEY (action_id, lease_generation) REFERENCES action_outbox (id, lease_generation) ON DELETE CASCADE`
- `CHECK (completed_at IS NULL OR completed_at >= started_at)`
- `CHECK ( (state = 'started' AND lease_generation > 0 AND completed_at IS NULL AND safe_error_code IS NULL) OR (state = 'completed' AND completed_at IS NOT NULL AND safe_error_code IS NULL) OR (state IN ('failed', 'unavailable', 'indeterminate') AND completed_at IS NOT NULL AND safe_error_code IS NOT NULL) )`
- `CHECK (lease_generation > 0 OR state <> 'started')`

### `notification_coalescing_windows`

迁移：7（`0007_notification_coalescing.sql`）。表选项：`STRICT`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `id` | `TEXT PRIMARY KEY` |
| `conversation_id` | `TEXT NOT NULL` |
| `leader_action_id` | `TEXT UNIQUE` |
| `opened_at` | `TEXT NOT NULL` |
| `closes_at` | `TEXT NOT NULL` |

表级约束：

- `FOREIGN KEY (conversation_id) REFERENCES conversations (id) ON DELETE CASCADE`
- `FOREIGN KEY (leader_action_id) REFERENCES action_outbox (id) ON DELETE SET NULL`
- `CHECK ( unixepoch(opened_at) IS NOT NULL AND unixepoch(closes_at) IS NOT NULL AND unixepoch(closes_at) - unixepoch(opened_at) = 30 AND substr(closes_at, 20) = substr(opened_at, 20) )`

### `notification_coalescing_members`

迁移：7（`0007_notification_coalescing.sql`）。表选项：`STRICT`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `action_id` | `TEXT PRIMARY KEY` |
| `window_id` | `TEXT NOT NULL` |
| `role` | `TEXT NOT NULL CHECK (role IN ('leader', 'follower'))` |
| `joined_at` | `TEXT NOT NULL` |

表级约束：

- `FOREIGN KEY (action_id) REFERENCES action_outbox (id) ON DELETE CASCADE`
- `FOREIGN KEY (window_id) REFERENCES notification_coalescing_windows (id) ON DELETE CASCADE`

### `model_call_dashboard_projection`

迁移：8（`0008_dashboard_query_projection.sql`）。表选项：`STRICT`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `model_call_id` | `TEXT PRIMARY KEY` |
| `event_at` | `TEXT NOT NULL` |
| `conversation_id` | `TEXT` |
| `turn_id` | `TEXT` |
| `temporal_class` | `TEXT NOT NULL CHECK (temporal_class IN ('realtime', 'historical'))` |
| `conversation_source_id` | `TEXT CHECK ( conversation_source_id IS NULL OR length(conversation_source_id) BETWEEN 1 AND 256 )` |
| `thread_source_id` | `TEXT CHECK ( thread_source_id IS NULL OR length(thread_source_id) BETWEEN 1 AND 256 )` |
| `model` | `TEXT CHECK (model IS NULL OR length(model) BETWEEN 1 AND 128)` |
| `reasoning_effort` | `TEXT CHECK ( reasoning_effort IS NULL OR reasoning_effort IN ('none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max', 'ultra') )` |
| `input_tokens` | `INTEGER CHECK (input_tokens BETWEEN 0 AND 1000000000000000)` |
| `cached_input_tokens` | `INTEGER CHECK (cached_input_tokens BETWEEN 0 AND 1000000000000000)` |
| `output_tokens` | `INTEGER CHECK (output_tokens BETWEEN 0 AND 1000000000000000)` |
| `reasoning_output_tokens` | `INTEGER CHECK ( reasoning_output_tokens BETWEEN 0 AND 1000000000000000 )` |
| `client` | `TEXT CHECK (client IS NULL OR length(client) BETWEEN 1 AND 128)` |
| `surface` | `TEXT CHECK (surface IS NULL OR length(surface) BETWEEN 1 AND 64)` |

表级约束：

- `FOREIGN KEY (model_call_id) REFERENCES model_calls (id) ON DELETE CASCADE`
- `FOREIGN KEY (conversation_id) REFERENCES conversations (id)`
- `FOREIGN KEY (turn_id, conversation_id) REFERENCES turns (id, conversation_id)`
- `CHECK (turn_id IS NULL OR conversation_id IS NOT NULL)`

### `alert_evidence_provenance`

迁移：8（`0008_dashboard_query_projection.sql`）。表选项：`STRICT`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `alert_id` | `TEXT PRIMARY KEY` |
| `source_id` | `TEXT NOT NULL CHECK ( length(source_id) BETWEEN 1 AND 64 AND substr(source_id, 1, 1) GLOB '[a-z0-9]' AND substr(source_id, -1, 1) GLOB '[a-z0-9]' AND source_id NOT GLOB '*[^a-z0-9._-]*' AND source_id NOT GLOB '*[._-][._-]*' )` |
| `source_contract_version` | `INTEGER NOT NULL CHECK (source_contract_version BETWEEN 1 AND 65535)` |
| `observed_at` | `TEXT NOT NULL` |
| `quality` | `TEXT NOT NULL CHECK (quality IN ('inferred', 'direct'))` |

表级约束：

- `FOREIGN KEY (alert_id) REFERENCES alerts (id) ON DELETE CASCADE`

### `app_locale_initialization`

迁移：9（`0009_locale_initialization.sql`）。表选项：`STRICT`。

<!-- prettier-ignore -->
| 列 | 声明 |
| --- | --- |
| `singleton` | `INTEGER PRIMARY KEY CHECK (singleton = 1)` |
| `initialized` | `INTEGER NOT NULL CHECK (initialized IN (0, 1))` |

表级约束：

- 除列声明外没有其他约束。

## 显式索引

`PRIMARY KEY` 或 `UNIQUE` 声明隐含的 SQLite 索引保留在上方表声明中。
本节列出显式的 `CREATE INDEX` 语句。

<!-- prettier-ignore -->
| 索引 | 表 | 唯一 | 列或表达式 | 谓词 | 迁移 |
| --- | --- | --- | --- | --- | --- |
| `model_calls_event_at_idx` | `model_calls` | 否 | `event_at DESC, id DESC` | `none` | 1（`0001_storage.sql`） |
| `model_calls_conversation_idx` | `model_calls` | 否 | `conversation_id, event_at DESC, id DESC` | `none` | 1（`0001_storage.sql`） |
| `observations_model_call_idx` | `model_call_observations` | 否 | `model_call_id, observed_at, id` | `none` | 1（`0001_storage.sql`） |
| `domain_work_state_idx` | `domain_work` | 否 | `state, created_at, id` | `none` | 1（`0001_storage.sql`） |
| `conversation_source_keys_conversation_idx` | `conversation_source_keys` | 否 | `conversation_id, key_kind` | `none` | 1（`0001_storage.sql`） |
| `rule_evaluations_model_call_idx` | `rule_evaluations` | 否 | `model_call_id, evaluated_at` | `none` | 2（`0002_rules_actions_settings.sql`） |
| `alerts_event_at_idx` | `alerts` | 否 | `event_at DESC, id DESC` | `none` | 2（`0002_rules_actions_settings.sql`） |
| `action_outbox_claim_idx` | `action_outbox` | 否 | `state, not_before, created_at, id` | `none` | 6（`0006_transactional_action_outbox.sql`） |
| `action_outbox_lease_expiry_idx` | `action_outbox` | 否 | `state, lease_expires_at, id` | `none` | 6（`0006_transactional_action_outbox.sql`） |
| `action_outbox_started_recovery_idx` | `action_outbox` | 否 | `started_at, id` | `WHERE state = 'started'` | 6（`0006_transactional_action_outbox.sql`） |
| `action_attempts_live_generation_idx` | `action_attempts` | 是 | `action_id, lease_generation` | `WHERE lease_generation > 0` | 6（`0006_transactional_action_outbox.sql`） |
| `notification_coalescing_windows_conversation_interval_idx` | `notification_coalescing_windows` | 否 | `conversation_id, opened_at, closes_at, id` | `none` | 7（`0007_notification_coalescing.sql`） |
| `notification_coalescing_windows_closes_at_idx` | `notification_coalescing_windows` | 否 | `closes_at, id` | `none` | 7（`0007_notification_coalescing.sql`） |
| `notification_coalescing_members_window_joined_idx` | `notification_coalescing_members` | 否 | `window_id, joined_at, action_id` | `none` | 7（`0007_notification_coalescing.sql`） |
| `notification_coalescing_members_one_leader_idx` | `notification_coalescing_members` | 是 | `window_id` | `WHERE role = 'leader'` | 7（`0007_notification_coalescing.sql`） |
| `model_call_dashboard_event_desc_idx` | `model_call_dashboard_projection` | 否 | `event_at DESC, model_call_id DESC` | `none` | 8（`0008_dashboard_query_projection.sql`） |
| `model_call_dashboard_event_asc_idx` | `model_call_dashboard_projection` | 否 | `event_at ASC, model_call_id ASC` | `none` | 8（`0008_dashboard_query_projection.sql`） |
| `model_call_dashboard_input_tokens_idx` | `model_call_dashboard_projection` | 否 | `input_tokens IS NULL, input_tokens DESC, event_at DESC, model_call_id DESC` | `none` | 8（`0008_dashboard_query_projection.sql`） |
| `model_call_dashboard_output_tokens_idx` | `model_call_dashboard_projection` | 否 | `output_tokens IS NULL, output_tokens DESC, event_at DESC, model_call_id DESC` | `none` | 8（`0008_dashboard_query_projection.sql`） |
| `model_call_dashboard_reasoning_tokens_idx` | `model_call_dashboard_projection` | 否 | `reasoning_output_tokens IS NULL, reasoning_output_tokens DESC, event_at DESC, model_call_id DESC` | `none` | 8（`0008_dashboard_query_projection.sql`） |
| `model_call_dashboard_conversation_idx` | `model_call_dashboard_projection` | 否 | `conversation_id, event_at DESC, model_call_id DESC` | `none` | 8（`0008_dashboard_query_projection.sql`） |
| `model_call_dashboard_conversation_source_idx` | `model_call_dashboard_projection` | 否 | `conversation_source_id, event_at DESC, model_call_id DESC` | `none` | 8（`0008_dashboard_query_projection.sql`） |
| `model_call_dashboard_thread_source_idx` | `model_call_dashboard_projection` | 否 | `thread_source_id, event_at DESC, model_call_id DESC` | `none` | 8（`0008_dashboard_query_projection.sql`） |
| `model_call_dashboard_model_idx` | `model_call_dashboard_projection` | 否 | `model, event_at DESC, model_call_id DESC` | `none` | 8（`0008_dashboard_query_projection.sql`） |
| `model_call_dashboard_client_idx` | `model_call_dashboard_projection` | 否 | `client, event_at DESC, model_call_id DESC` | `none` | 8（`0008_dashboard_query_projection.sql`） |
| `model_call_dashboard_temporal_idx` | `model_call_dashboard_projection` | 否 | `temporal_class, event_at DESC, model_call_id DESC` | `none` | 8（`0008_dashboard_query_projection.sql`） |
| `conversations_last_observed_idx` | `conversations` | 否 | `last_observed_at DESC, id DESC` | `none` | 8（`0008_dashboard_query_projection.sql`） |
| `conversation_source_keys_value_idx` | `conversation_source_keys` | 否 | `key_value, conversation_id` | `none` | 8（`0008_dashboard_query_projection.sql`） |
| `alerts_created_at_idx` | `alerts` | 否 | `created_at DESC, id DESC` | `none` | 8（`0008_dashboard_query_projection.sql`） |
| `alerts_model_call_created_idx` | `alerts` | 否 | `model_call_id, created_at DESC, id DESC` | `none` | 8（`0008_dashboard_query_projection.sql`） |
| `observations_source_call_idx` | `model_call_observations` | 否 | `source_id, model_call_id` | `none` | 8（`0008_dashboard_query_projection.sql`） |
| `action_outbox_state_alert_idx` | `action_outbox` | 否 | `state, alert_id` | `none` | 8（`0008_dashboard_query_projection.sql`） |
| `action_outbox_alert_notification_idx` | `action_outbox` | 否 | `alert_id, action_type, parameter_schema_version DESC, id DESC` | `none` | 8（`0008_dashboard_query_projection.sql`） |

## 触发器

<!-- prettier-ignore -->
| 触发器 | 表 | 事件 | 迁移 |
| --- | --- | --- | --- |
| `model_calls_projection_reference_guard` | `model_calls` | `BEFORE UPDATE OF event_time_observation_id, relationship_observation_id` | 1（`0001_storage.sql`） |
| `rules_notification_threshold_insert_guard` | `rules` | `BEFORE INSERT` | 4（`0004_alert_evidence_and_threshold_invariants.sql`） |
| `rules_notification_threshold_update_guard` | `rules` | `BEFORE UPDATE OF notify_max_n, notify_all` | 4（`0004_alert_evidence_and_threshold_invariants.sql`） |
| `app_settings_notification_threshold_insert_guard` | `app_settings` | `BEFORE INSERT` | 4（`0004_alert_evidence_and_threshold_invariants.sql`） |
| `app_settings_notification_threshold_update_guard` | `app_settings` | `BEFORE UPDATE OF notify_max_n, notify_all` | 4（`0004_alert_evidence_and_threshold_invariants.sql`） |
| `model_calls_origin_immutable_guard` | `model_calls` | `BEFORE UPDATE OF origin` | 5（`0005_historical_action_isolation.sql`） |
| `domain_work_historical_insert_guard` | `domain_work` | `BEFORE INSERT` | 5（`0005_historical_action_isolation.sql`） |
| `domain_work_historical_update_guard` | `domain_work` | `BEFORE UPDATE OF model_call_id, work_kind` | 5（`0005_historical_action_isolation.sql`） |
| `action_outbox_initial_state_guard` | `action_outbox` | `BEFORE INSERT` | 6（`0006_transactional_action_outbox.sql`） |
| `action_outbox_request_immutable_guard` | `action_outbox` | `BEFORE UPDATE OF id, alert_id, action_type, parameter_schema_version, idempotency_key_version, idempotency_key_bytes, model, reasoning_output_tokens, pattern_n, created_at, not_before` | 6（`0006_transactional_action_outbox.sql`） |
| `action_outbox_terminal_immutable_guard` | `action_outbox` | `BEFORE UPDATE` | 6（`0006_transactional_action_outbox.sql`） |
| `action_outbox_started_at_immutable_guard` | `action_outbox` | `BEFORE UPDATE OF started_at` | 6（`0006_transactional_action_outbox.sql`） |
| `action_outbox_state_transition_guard` | `action_outbox` | `BEFORE UPDATE OF state` | 6（`0006_transactional_action_outbox.sql`） |
| `action_outbox_lease_transition_guard` | `action_outbox` | `BEFORE UPDATE OF lease_generation, lease_owner, lease_expires_at` | 6（`0006_transactional_action_outbox.sql`） |
| `action_attempts_insert_guard` | `action_attempts` | `BEFORE INSERT` | 6（`0006_transactional_action_outbox.sql`） |
| `action_attempts_identity_immutable_guard` | `action_attempts` | `BEFORE UPDATE OF action_id, attempt_number, lease_generation, started_at` | 6（`0006_transactional_action_outbox.sql`） |
| `action_attempts_terminal_immutable_guard` | `action_attempts` | `BEFORE UPDATE` | 6（`0006_transactional_action_outbox.sql`） |
| `action_attempts_state_transition_guard` | `action_attempts` | `BEFORE UPDATE OF state` | 6（`0006_transactional_action_outbox.sql`） |
| `action_outbox_terminal_attempt_guard` | `action_outbox` | `BEFORE UPDATE OF state` | 6（`0006_transactional_action_outbox.sql`） |
| `action_outbox_historical_insert_guard` | `action_outbox` | `BEFORE INSERT` | 6（`0006_transactional_action_outbox.sql`） |
| `action_outbox_historical_update_guard` | `action_outbox` | `BEFORE UPDATE OF alert_id` | 6（`0006_transactional_action_outbox.sql`） |
| `notification_coalescing_windows_leader_insert_guard` | `notification_coalescing_windows` | `BEFORE INSERT` | 7（`0007_notification_coalescing.sql`） |
| `notification_coalescing_windows_overlap_insert_guard` | `notification_coalescing_windows` | `BEFORE INSERT` | 7（`0007_notification_coalescing.sql`） |
| `notification_coalescing_windows_identity_immutable_guard` | `notification_coalescing_windows` | `BEFORE UPDATE OF id, conversation_id, opened_at, closes_at` | 7（`0007_notification_coalescing.sql`） |
| `notification_coalescing_windows_leader_immutable_guard` | `notification_coalescing_windows` | `BEFORE UPDATE OF leader_action_id` | 7（`0007_notification_coalescing.sql`） |
| `notification_coalescing_windows_nonempty_delete_guard` | `notification_coalescing_windows` | `BEFORE DELETE` | 7（`0007_notification_coalescing.sql`） |
| `notification_coalescing_members_insert_guard` | `notification_coalescing_members` | `BEFORE INSERT` | 7（`0007_notification_coalescing.sql`） |
| `notification_coalescing_members_immutable_guard` | `notification_coalescing_members` | `BEFORE UPDATE` | 7（`0007_notification_coalescing.sql`） |
| `notification_coalescing_members_retention_delete_guard` | `notification_coalescing_members` | `BEFORE DELETE` | 7（`0007_notification_coalescing.sql`） |
| `notification_coalescing_members_empty_window_cleanup` | `notification_coalescing_members` | `AFTER DELETE` | 7（`0007_notification_coalescing.sql`） |
| `alert_evidence_provenance_immutable_guard` | `alert_evidence_provenance` | `BEFORE UPDATE` | 8（`0008_dashboard_query_projection.sql`） |
