use std::path::Path;
use std::time::Duration;

use codex_monitor_core::UtcTimestamp;
use rusqlite::{Connection, OptionalExtension as _, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use super::codec::CATALOG_FIELD_IDS;
use super::timestamp::timestamp_text;
use super::{StorageError, StorageResult};

pub(crate) const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug)]
pub(crate) struct EmbeddedMigration {
    pub version: u32,
    pub name: &'static str,
    pub sql: &'static str,
}

pub(crate) const MIGRATIONS: &[EmbeddedMigration] = &[
    EmbeddedMigration {
        version: 1,
        name: "0001_storage.sql",
        sql: include_str!("../migrations/0001_storage.sql"),
    },
    EmbeddedMigration {
        version: 2,
        name: "0002_rules_actions_settings.sql",
        sql: include_str!("../migrations/0002_rules_actions_settings.sql"),
    },
    EmbeddedMigration {
        version: 3,
        name: "0003_rule_definition_fingerprints.sql",
        sql: include_str!("../migrations/0003_rule_definition_fingerprints.sql"),
    },
    EmbeddedMigration {
        version: 4,
        name: "0004_alert_evidence_and_threshold_invariants.sql",
        sql: include_str!("../migrations/0004_alert_evidence_and_threshold_invariants.sql"),
    },
    EmbeddedMigration {
        version: 5,
        name: "0005_historical_action_isolation.sql",
        sql: include_str!("../migrations/0005_historical_action_isolation.sql"),
    },
    EmbeddedMigration {
        version: 6,
        name: "0006_transactional_action_outbox.sql",
        sql: include_str!("../migrations/0006_transactional_action_outbox.sql"),
    },
    EmbeddedMigration {
        version: 7,
        name: "0007_notification_coalescing.sql",
        sql: include_str!("../migrations/0007_notification_coalescing.sql"),
    },
    EmbeddedMigration {
        version: 8,
        name: "0008_dashboard_query_projection.sql",
        sql: include_str!("../migrations/0008_dashboard_query_projection.sql"),
    },
    EmbeddedMigration {
        version: 9,
        name: "0009_locale_initialization.sql",
        sql: include_str!("../migrations/0009_locale_initialization.sql"),
    },
];

#[derive(Debug)]
struct AppliedMigration {
    version: u32,
    name: String,
    checksum: String,
    sql_bytes: i64,
}

fn canonical_sql(sql: &str) -> String {
    sql.replace("\r\n", "\n")
}

pub(crate) fn migration_checksum(sql: &str) -> String {
    let digest = Sha256::digest(canonical_sql(sql).as_bytes());
    format!("{digest:x}")
}

fn canonical_sql_size(sql: &str) -> i64 {
    i64::try_from(canonical_sql(sql).len()).expect("embedded migration length fits i64")
}

fn validate_embedded_order() -> StorageResult<()> {
    for (index, migration) in MIGRATIONS.iter().enumerate() {
        let expected = u32::try_from(index + 1).expect("migration count fits u32");
        if migration.version != expected {
            return Err(StorageError::MigrationOrder {
                version: migration.version,
            });
        }
        let expected_prefix = format!("{:04}_", migration.version);
        if !migration.name.starts_with(&expected_prefix) {
            return Err(StorageError::MigrationOrder {
                version: migration.version,
            });
        }
    }
    Ok(())
}

pub(crate) fn configure_writer_connection(connection: &Connection) -> StorageResult<()> {
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}

pub(crate) fn configure_read_connection(connection: &Connection) -> StorageResult<()> {
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.pragma_update(None, "query_only", true)?;
    Ok(())
}

fn migration_table_exists(connection: &Connection) -> StorageResult<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'schema_migrations')",
        [],
        |row| row.get::<_, bool>(0),
    )?)
}

fn load_applied(connection: &Connection) -> StorageResult<Vec<AppliedMigration>> {
    if !migration_table_exists(connection)? {
        return Ok(Vec::new());
    }

    let mut statement = connection.prepare(
        "SELECT version, name, sha256, sql_bytes FROM schema_migrations ORDER BY version",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(AppliedMigration {
            version: row.get(0)?,
            name: row.get(1)?,
            checksum: row.get(2)?,
            sql_bytes: row.get(3)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn verify_applied(applied: &[AppliedMigration]) -> StorageResult<()> {
    for (index, stored) in applied.iter().enumerate() {
        let expected_version = u32::try_from(index + 1).expect("migration count fits u32");
        if stored.version != expected_version {
            return Err(StorageError::MigrationAuditGap {
                version: expected_version,
            });
        }
        let Some(embedded) = MIGRATIONS.get(index) else {
            return Err(StorageError::UnknownMigration {
                version: stored.version,
            });
        };
        if stored.name != embedded.name {
            return Err(StorageError::MigrationNameMismatch {
                version: stored.version,
            });
        }
        if stored.checksum != migration_checksum(embedded.sql) {
            return Err(StorageError::MigrationChecksumMismatch {
                version: stored.version,
            });
        }
        if stored.sql_bytes != canonical_sql_size(embedded.sql) {
            return Err(StorageError::MigrationSizeMismatch {
                version: stored.version,
            });
        }
    }
    Ok(())
}

fn verify_field_registry(connection: &Connection) -> StorageResult<()> {
    if field_registry_matches(connection)? {
        return Ok(());
    }
    Err(StorageError::UnregisteredTelemetryField)
}

fn field_registry_matches(connection: &Connection) -> StorageResult<bool> {
    let mut statement = connection
        .prepare("SELECT field_id, persisted FROM telemetry_field_registry ORDER BY field_id")?;
    let stored = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if stored.len() != CATALOG_FIELD_IDS.len() {
        return Ok(false);
    }
    for ((stored_id, persisted), expected_id) in stored.iter().zip(CATALOG_FIELD_IDS) {
        let expected_persisted = *expected_id != "conversation.resolved_display_name";
        if stored_id != expected_id || *persisted != expected_persisted {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn matches_current_schema_bounded(connection: &Connection) -> StorageResult<bool> {
    validate_embedded_order()?;
    let migration_limit = i64::try_from(MIGRATIONS.len() + 1).expect("migration limit fits i64");
    let migration_count = connection.query_row(
        "SELECT count(*) FROM (SELECT 1 FROM schema_migrations LIMIT ?1)",
        [migration_limit],
        |row| row.get::<_, i64>(0),
    )?;
    if migration_count != i64::try_from(MIGRATIONS.len()).expect("migration count fits i64") {
        return Ok(false);
    }
    for migration in MIGRATIONS {
        let stored = connection
            .query_row(
                "SELECT version, name, sha256, sql_bytes
                 FROM schema_migrations WHERE version = ?1 LIMIT 2",
                [migration.version],
                |row| {
                    Ok(AppliedMigration {
                        version: row.get(0)?,
                        name: row.get(1)?,
                        checksum: row.get(2)?,
                        sql_bytes: row.get(3)?,
                    })
                },
            )
            .optional()?;
        let Some(stored) = stored else {
            return Ok(false);
        };
        if stored.version != migration.version
            || stored.name != migration.name
            || stored.checksum != migration_checksum(migration.sql)
            || stored.sql_bytes != canonical_sql_size(migration.sql)
        {
            return Ok(false);
        }
    }

    let registry_limit =
        i64::try_from(CATALOG_FIELD_IDS.len() + 1).expect("field registry limit fits i64");
    let registry_count = connection.query_row(
        "SELECT count(*) FROM (SELECT 1 FROM telemetry_field_registry LIMIT ?1)",
        [registry_limit],
        |row| row.get::<_, i64>(0),
    )?;
    if registry_count
        != i64::try_from(CATALOG_FIELD_IDS.len()).expect("field registry count fits i64")
    {
        return Ok(false);
    }
    for field_id in CATALOG_FIELD_IDS {
        let persisted = connection
            .query_row(
                "SELECT persisted FROM telemetry_field_registry WHERE field_id = ?1 LIMIT 2",
                [field_id],
                |row| row.get::<_, bool>(0),
            )
            .optional()?;
        if persisted != Some(*field_id != "conversation.resolved_display_name") {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn open_and_migrate(path: &Path) -> StorageResult<Connection> {
    validate_embedded_order()?;
    let mut connection = Connection::open(path)?;
    configure_writer_connection(&connection)?;

    let applied = load_applied(&connection)?;
    verify_applied(&applied)?;

    for migration in MIGRATIONS.iter().skip(applied.len()) {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(migration.sql)?;
        transaction.execute(
            "INSERT INTO schema_migrations (version, name, sha256, sql_bytes, applied_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                migration.version,
                migration.name,
                migration_checksum(migration.sql),
                canonical_sql_size(migration.sql),
                timestamp_text(UtcTimestamp::now()),
            ],
        )?;
        transaction.commit()?;
    }

    verify_field_registry(&connection)?;

    Ok(connection)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    const SYNTHETIC_RULE_ID: &str = "synthetic-historical-isolation-rule";
    const CREATED_AT: &str = "2026-01-01T00:00:00.000000000Z";
    const STARTED_AT: &str = "2026-01-01T00:00:01.000000000Z";
    const COMPLETED_AT: &str = "2026-01-01T00:00:02.000000000Z";
    type StoredAction = (
        String,
        i64,
        Vec<u8>,
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );

    fn database_before_historical_isolation() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        for migration in &MIGRATIONS[..4] {
            connection.execute_batch(migration.sql).unwrap();
        }
        connection
            .execute(
                "INSERT INTO rules (
                    rule_id, rule_version, rule_kind, enabled, notify_max_n, notify_all
                 ) VALUES (?1, 1, 'suspected_reasoning_limit_pattern', 1, 1, 0)",
                [SYNTHETIC_RULE_ID],
            )
            .unwrap();
        connection
    }

    fn insert_synthetic_model_call(connection: &Connection, id: &str, origin: &str) {
        let observation_id = format!("observation-{id}");
        let canonical_digest = Sha256::digest(format!("model-call:{id}")).to_vec();
        let observation_digest = Sha256::digest(format!("observation:{id}")).to_vec();

        connection.execute_batch("BEGIN DEFERRED").unwrap();
        connection
            .execute(
                "INSERT INTO model_calls (
                    id, canonical_source_id, canonical_source_contract_version,
                    canonical_identity_version, canonical_identity_kind,
                    canonical_identity_digest, event_at, event_time_observation_id, origin
                 ) VALUES (?1, 'synthetic-source', 1, 1, 'stable', ?2, ?3, ?4, ?5)",
                params![id, canonical_digest, CREATED_AT, observation_id, origin],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO model_call_observations (
                    id, model_call_id, source_id, source_contract_version,
                    identity_version, identity_kind, identity_digest, event_at,
                    event_provenance_observed_at, event_quality, event_priority,
                    observed_at, origin
                 ) VALUES (
                    ?1, ?2, 'synthetic-source', 1, 1, 'stable', ?3, ?4,
                    ?4, 'direct', 100, ?4, ?5
                 )",
                params![observation_id, id, observation_digest, CREATED_AT, origin],
            )
            .unwrap();
        connection.execute_batch("COMMIT").unwrap();
    }

    fn insert_reasoning_observation(
        connection: &Connection,
        model_call_id: &str,
        observation_id: &str,
        source_id: &str,
        tokens: i64,
        observed_at: &str,
    ) {
        let observation_digest = Sha256::digest(format!("observation:{observation_id}")).to_vec();
        connection
            .execute(
                "INSERT INTO model_call_observations (
                    id, model_call_id, source_id, source_contract_version,
                    identity_version, identity_kind, identity_digest, event_at,
                    event_provenance_observed_at, event_quality, event_priority,
                    observed_at, origin
                 ) VALUES (
                    ?1, ?2, ?3, 1, 1, 'stable', ?4, ?5,
                    ?5, 'direct', 100, ?5, 'realtime'
                 )",
                params![
                    observation_id,
                    model_call_id,
                    source_id,
                    observation_digest,
                    observed_at
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO model_call_observation_fields (
                    observation_id, field_id, integer_value,
                    provenance_observed_at, quality, source_priority
                 ) VALUES (
                    ?1, 'model_call.reasoning_output_tokens', ?2, ?3, 'direct', 100
                 )",
                params![observation_id, tokens, observed_at],
            )
            .unwrap();
    }

    fn insert_synthetic_conversation_model_call(
        connection: &Connection,
        id: &str,
        conversation_id: &str,
    ) {
        connection
            .execute(
                "INSERT INTO conversations (id, first_observed_at, last_observed_at)
                 VALUES (?1, ?2, ?2)
                 ON CONFLICT (id) DO NOTHING",
                params![conversation_id, CREATED_AT],
            )
            .unwrap();
        insert_synthetic_model_call(connection, id, "realtime");

        let observation_id = format!("observation-{id}");
        connection
            .execute(
                "INSERT INTO model_call_observation_relationships (
                    observation_id, model_call_id, relationship_kind, conversation_id,
                    provenance_observed_at, quality, source_priority
                 ) VALUES (?1, ?2, 'conversation', ?3, ?4, 'direct', 100)",
                params![observation_id, id, conversation_id, CREATED_AT],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE model_calls
                 SET relationship_kind = 'conversation', conversation_id = ?1,
                     relationship_observation_id = ?2
                 WHERE id = ?3",
                params![conversation_id, observation_id, id],
            )
            .unwrap();
    }

    fn insert_domain_work(connection: &Connection, id: &str, model_call_id: &str, state: &str) {
        let (started_at, completed_at) = match state {
            "pending" => (None, None),
            "started" => (Some(STARTED_AT), None),
            "completed" => (Some(STARTED_AT), Some(COMPLETED_AT)),
            _ => panic!("unsupported synthetic work state: {state}"),
        };
        connection
            .execute(
                "INSERT INTO domain_work (
                    id, work_kind, model_call_id, state, created_at, started_at, completed_at
                 ) VALUES (?1, 'model_call.completed', ?2, ?3, ?4, ?5, ?6)",
                params![
                    id,
                    model_call_id,
                    state,
                    CREATED_AT,
                    started_at,
                    completed_at
                ],
            )
            .unwrap();
    }

    fn insert_rule_evaluation(
        connection: &Connection,
        model_call_id: &str,
        matched: bool,
    ) -> String {
        let evaluation_id = format!("evaluation-{model_call_id}");
        let (reasoning_output_tokens, pattern_n) = if matched {
            (516_i64, Some(1_i64))
        } else {
            (517_i64, None)
        };
        connection
            .execute(
                "INSERT INTO rule_evaluations (
                    id, rule_id, rule_version, model_call_id, matched,
                    reasoning_output_tokens, pattern_n, event_at, source_quality, evaluated_at
                 ) VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6, ?7, 'direct', ?7)",
                params![
                    evaluation_id,
                    SYNTHETIC_RULE_ID,
                    model_call_id,
                    matched,
                    reasoning_output_tokens,
                    pattern_n,
                    CREATED_AT
                ],
            )
            .unwrap();
        evaluation_id
    }

    fn insert_matched_alert(connection: &Connection, model_call_id: &str) -> String {
        let evaluation_id = insert_rule_evaluation(connection, model_call_id, true);
        let alert_id = format!("alert-{model_call_id}");
        connection
            .execute(
                "INSERT INTO alerts (
                    id, evaluation_id, model_call_id, rule_id, rule_version,
                    reasoning_output_tokens, pattern_n, event_at, source_quality, created_at
                 ) VALUES (?1, ?2, ?3, ?4, 1, 516, 1, ?5, 'direct', ?5)",
                params![
                    alert_id,
                    evaluation_id,
                    model_call_id,
                    SYNTHETIC_RULE_ID,
                    CREATED_AT
                ],
            )
            .unwrap();
        alert_id
    }

    fn insert_coalescing_action(
        connection: &Connection,
        id: &str,
        alert_id: &str,
        state: &str,
        created_at: &str,
        not_before: &str,
        idempotency_byte: u8,
    ) {
        let (completed_at, suppression_reason) = match state {
            "pending" => (None, None),
            "suppressed" => (Some(created_at), Some("coalesced")),
            _ => panic!("unsupported synthetic coalescing action state: {state}"),
        };
        connection
            .execute(
                "INSERT INTO action_outbox (
                    id, alert_id, action_type, parameter_schema_version,
                    idempotency_key_version, idempotency_key_bytes, state,
                    reasoning_output_tokens, pattern_n, created_at, not_before,
                    completed_at, suppression_reason
                 ) VALUES (
                    ?1, ?2, 'system_notification', 1, 1, ?3, ?4,
                    516, 1, ?5, ?6, ?7, ?8
                 )",
                params![
                    id,
                    alert_id,
                    vec![idempotency_byte; 32],
                    state,
                    created_at,
                    not_before,
                    completed_at,
                    suppression_reason,
                ],
            )
            .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_action(
        connection: &Connection,
        id: &str,
        alert_id: &str,
        state: &str,
        lease_owner: Option<&str>,
        lease_expires_at: Option<&str>,
        started_at: Option<&str>,
        completed_at: Option<&str>,
        last_error_code: Option<&str>,
    ) {
        let idempotency_key = format!("synthetic:{id}");
        connection
            .execute(
                "INSERT INTO action_outbox (
                    id, alert_id, action_type, idempotency_key, state, model,
                    reasoning_output_tokens, pattern_n, created_at, not_before,
                    lease_owner, lease_expires_at, started_at, completed_at, last_error_code
                 ) VALUES (
                    ?1, ?2, 'system_notification', ?3, ?4, 'synthetic-model',
                    516, 1, ?5, ?5, ?6, ?7, ?8, ?9, ?10
                 )",
                params![
                    id,
                    alert_id,
                    idempotency_key,
                    state,
                    CREATED_AT,
                    lease_owner,
                    lease_expires_at,
                    started_at,
                    completed_at,
                    last_error_code
                ],
            )
            .unwrap();
    }

    fn assert_guard_abort(connection: &Connection, statement: &str, expected_message: &str) {
        let error = connection.execute(statement, []).unwrap_err();
        let rendered = error.to_string();
        assert!(
            rendered.contains(expected_message),
            "expected {expected_message:?} from {statement:?}, got {rendered:?}"
        );
    }

    #[test]
    fn embedded_migrations_are_contiguous_and_stable() {
        validate_embedded_order().unwrap();
        assert_eq!(MIGRATIONS.len(), 9);
        for migration in MIGRATIONS {
            assert_eq!(migration_checksum(migration.sql).len(), 64);
            assert!(canonical_sql_size(migration.sql) > 0);
        }
    }

    #[test]
    fn applies_all_migrations_to_a_new_database() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("storage.sqlite3");
        let connection = open_and_migrate(&database).unwrap();

        let migration_count: i64 = connection
            .query_row("SELECT count(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(migration_count, i64::try_from(MIGRATIONS.len()).unwrap());
        let integrity: String = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn dashboard_projection_migration_backfills_calls_and_alert_provenance() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        for migration in &MIGRATIONS[..7] {
            connection.execute_batch(migration.sql).unwrap();
        }
        connection
            .execute(
                "INSERT INTO rules (
                    rule_id, rule_version, rule_kind, enabled, notify_max_n, notify_all
                 ) VALUES (?1, 1, 'suspected_reasoning_limit_pattern', 1, 1, 0)",
                [SYNTHETIC_RULE_ID],
            )
            .unwrap();
        insert_synthetic_conversation_model_call(
            &connection,
            "call-dashboard-backfill",
            "conversation-dashboard-backfill",
        );
        let observation_id = "observation-call-dashboard-backfill";
        for (field_id, text_value, integer_value) in [
            ("model_call.model", Some("gpt-test"), None),
            ("model_call.input_tokens", None, Some(100_i64)),
            ("model_call.output_tokens", None, Some(50_i64)),
            ("model_call.reasoning_output_tokens", None, Some(516_i64)),
            ("model_call.client", Some("codex-cli"), None),
        ] {
            connection
                .execute(
                    "INSERT INTO model_call_observation_fields (
                        observation_id, field_id, text_value, integer_value,
                        provenance_observed_at, quality, source_priority
                     ) VALUES (?1, ?2, ?3, ?4, ?5, 'direct', 100)",
                    params![
                        observation_id,
                        field_id,
                        text_value,
                        integer_value,
                        CREATED_AT
                    ],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO model_call_projection_fields (
                        model_call_id, field_id, observation_id
                     ) VALUES ('call-dashboard-backfill', ?1, ?2)",
                    params![field_id, observation_id],
                )
                .unwrap();
        }
        let alert_id = insert_matched_alert(&connection, "call-dashboard-backfill");

        connection.execute_batch(MIGRATIONS[7].sql).unwrap();

        let projection: (i64, i64, String, String) = connection
            .query_row(
                "SELECT input_tokens, output_tokens, model, client
                 FROM model_call_dashboard_projection
                 WHERE model_call_id = 'call-dashboard-backfill'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(projection, (100, 50, "gpt-test".into(), "codex-cli".into()));
        let persisted_total_columns: i64 = connection
            .query_row(
                "SELECT count(*) FROM pragma_table_info('model_call_dashboard_projection')
                 WHERE name = 'total_tokens'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(persisted_total_columns, 0);
        let provenance: (String, i64, String, String) = connection
            .query_row(
                "SELECT source_id, source_contract_version, observed_at, quality
                 FROM alert_evidence_provenance WHERE alert_id = ?1",
                [&alert_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            provenance,
            (
                "synthetic-source".into(),
                1,
                CREATED_AT.into(),
                "direct".into()
            )
        );

        assert!(
            connection
                .execute(
                    "UPDATE model_call_dashboard_projection
                     SET input_tokens = 1000000000000001
                     WHERE model_call_id = 'call-dashboard-backfill'",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE alert_evidence_provenance
                     SET source_id = 'Synthetic-Source'
                     WHERE alert_id = ?1",
                    [&alert_id],
                )
                .is_err()
        );
        let immutable_error = connection
            .execute(
                "UPDATE alert_evidence_provenance
                 SET observed_at = '2026-07-15T01:00:00Z'
                 WHERE alert_id = ?1",
                [&alert_id],
            )
            .unwrap_err();
        assert!(
            immutable_error
                .to_string()
                .contains("alert evidence provenance is immutable")
        );
    }

    #[test]
    fn dashboard_projection_migration_refuses_alert_without_selected_evidence() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        for migration in &MIGRATIONS[..7] {
            connection.execute_batch(migration.sql).unwrap();
        }
        connection
            .execute(
                "INSERT INTO rules (
                    rule_id, rule_version, rule_kind, enabled, notify_max_n, notify_all
                 ) VALUES (?1, 1, 'suspected_reasoning_limit_pattern', 1, 1, 0)",
                [SYNTHETIC_RULE_ID],
            )
            .unwrap();
        insert_synthetic_model_call(&connection, "call-missing-evidence", "realtime");
        insert_matched_alert(&connection, "call-missing-evidence");

        let error = connection.execute_batch(MIGRATIONS[7].sql).unwrap_err();
        assert!(error.to_string().contains("CHECK constraint failed"));
    }

    #[test]
    fn dashboard_projection_migration_refuses_changed_alert_evidence() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        for migration in &MIGRATIONS[..7] {
            connection.execute_batch(migration.sql).unwrap();
        }
        connection
            .execute(
                "INSERT INTO rules (
                    rule_id, rule_version, rule_kind, enabled, notify_max_n, notify_all
                 ) VALUES (?1, 1, 'suspected_reasoning_limit_pattern', 1, 1, 0)",
                [SYNTHETIC_RULE_ID],
            )
            .unwrap();
        insert_synthetic_model_call(&connection, "call-changed-evidence", "realtime");
        connection
            .execute(
                "INSERT INTO model_call_observation_fields (
                    observation_id, field_id, integer_value,
                    provenance_observed_at, quality, source_priority
                 ) VALUES (
                    'observation-call-changed-evidence',
                    'model_call.reasoning_output_tokens', 1034, ?1, 'direct', 100
                 )",
                [CREATED_AT],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO model_call_projection_fields (
                    model_call_id, field_id, observation_id
                 ) VALUES (
                    'call-changed-evidence', 'model_call.reasoning_output_tokens',
                    'observation-call-changed-evidence'
                 )",
                [],
            )
            .unwrap();
        insert_matched_alert(&connection, "call-changed-evidence");

        let error = connection.execute_batch(MIGRATIONS[7].sql).unwrap_err();
        assert!(error.to_string().contains("CHECK constraint failed"));
    }

    #[test]
    fn dashboard_projection_migration_recovers_unique_historical_alert_evidence() {
        let connection = dashboard_projection_migration_database();
        insert_synthetic_model_call(&connection, "call-historical-evidence", "realtime");
        connection
            .execute(
                "INSERT INTO model_call_observation_fields (
                    observation_id, field_id, integer_value,
                    provenance_observed_at, quality, source_priority
                 ) VALUES (
                    'observation-call-historical-evidence',
                    'model_call.reasoning_output_tokens', 516, ?1, 'direct', 100
                 )",
                [CREATED_AT],
            )
            .unwrap();
        insert_reasoning_observation(
            &connection,
            "call-historical-evidence",
            "observation-current-evidence",
            "source-b",
            1034,
            STARTED_AT,
        );
        connection
            .execute(
                "INSERT INTO model_call_projection_fields (
                    model_call_id, field_id, observation_id
                 ) VALUES (
                    'call-historical-evidence', 'model_call.reasoning_output_tokens',
                    'observation-current-evidence'
                 )",
                [],
            )
            .unwrap();
        let alert_id = insert_matched_alert(&connection, "call-historical-evidence");

        connection.execute_batch(MIGRATIONS[7].sql).unwrap();

        let provenance: (String, String) = connection
            .query_row(
                "SELECT source_id, observed_at
                 FROM alert_evidence_provenance WHERE alert_id = ?1",
                [alert_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(provenance, ("synthetic-source".into(), CREATED_AT.into()));
    }

    #[test]
    fn dashboard_projection_migration_refuses_ambiguous_historical_alert_evidence() {
        let connection = dashboard_projection_migration_database();
        insert_synthetic_model_call(&connection, "call-ambiguous-evidence", "realtime");
        connection
            .execute(
                "INSERT INTO model_call_observation_fields (
                    observation_id, field_id, integer_value,
                    provenance_observed_at, quality, source_priority
                 ) VALUES (
                    'observation-call-ambiguous-evidence',
                    'model_call.reasoning_output_tokens', 516, ?1, 'direct', 100
                 )",
                [CREATED_AT],
            )
            .unwrap();
        insert_reasoning_observation(
            &connection,
            "call-ambiguous-evidence",
            "observation-later-evidence",
            "source-b",
            516,
            STARTED_AT,
        );
        connection
            .execute(
                "INSERT INTO model_call_projection_fields (
                    model_call_id, field_id, observation_id
                 ) VALUES (
                    'call-ambiguous-evidence', 'model_call.reasoning_output_tokens',
                    'observation-later-evidence'
                 )",
                [],
            )
            .unwrap();
        insert_matched_alert(&connection, "call-ambiguous-evidence");

        let error = connection.execute_batch(MIGRATIONS[7].sql).unwrap_err();
        assert!(error.to_string().contains("CHECK constraint failed"));
    }

    fn dashboard_projection_migration_database() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        for migration in &MIGRATIONS[..7] {
            connection.execute_batch(migration.sql).unwrap();
        }
        connection
            .execute(
                "INSERT INTO rules (
                    rule_id, rule_version, rule_kind, enabled, notify_max_n, notify_all
                 ) VALUES (?1, 1, 'suspected_reasoning_limit_pattern', 1, 1, 0)",
                [SYNTHETIC_RULE_ID],
            )
            .unwrap();
        connection
    }

    #[test]
    fn fingerprint_migration_backfills_the_pinned_v1_semantics() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(MIGRATIONS[0].sql).unwrap();
        connection.execute_batch(MIGRATIONS[1].sql).unwrap();
        connection
            .execute(
                "INSERT INTO rules (
                    rule_id, rule_version, rule_kind, enabled, notify_max_n, notify_all
                 ) VALUES (
                    'suspected-reasoning-limit-pattern', 1,
                    'suspected_reasoning_limit_pattern', 1, 1, 0
                 )",
                [],
            )
            .unwrap();

        connection.execute_batch(MIGRATIONS[2].sql).unwrap();
        let stored: Vec<u8> = connection
            .query_row(
                "SELECT semantic_sha256 FROM rule_definition_fingerprints
                 WHERE rule_id = 'suspected-reasoning-limit-pattern' AND rule_version = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            stored.as_slice(),
            codex_monitor_core::SUSPECTED_REASONING_LIMIT_PATTERN.semantic_fingerprint()
        );
    }

    #[test]
    fn alert_evidence_migration_backfills_only_matches_without_outbox() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(MIGRATIONS[0].sql).unwrap();
        connection.execute_batch(MIGRATIONS[1].sql).unwrap();
        connection
            .pragma_update(None, "foreign_keys", false)
            .unwrap();
        connection
            .execute_batch(
                "INSERT INTO model_calls (
                    id, canonical_source_id, canonical_source_contract_version,
                    canonical_identity_version, canonical_identity_kind,
                    canonical_identity_digest, event_at, event_time_observation_id, origin
                 ) VALUES
                    ('call-matched', 'test', 1, 1, 'stable', randomblob(32),
                     '2026-01-01T00:00:00.000000000Z', 'observation-matched', 'realtime'),
                    ('call-unmatched', 'test', 1, 1, 'stable', randomblob(32),
                     '2026-01-01T00:00:01.000000000Z', 'observation-unmatched', 'realtime');",
            )
            .unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        connection
            .execute_batch(
                "INSERT INTO rules (
                    rule_id, rule_version, rule_kind, enabled, notify_max_n, notify_all
                 ) VALUES (
                    'suspected-reasoning-limit-pattern', 1,
                    'suspected_reasoning_limit_pattern', 1, 1, 0
                 );",
            )
            .unwrap();
        connection.execute_batch(MIGRATIONS[2].sql).unwrap();
        connection
            .execute_batch(
                "INSERT INTO rule_evaluations (
                    id, rule_id, rule_version, model_call_id, matched,
                    reasoning_output_tokens, pattern_n, event_at, source_quality, evaluated_at
                 ) VALUES
                    (
                        'evaluation-matched', 'suspected-reasoning-limit-pattern', 1,
                        'call-matched', 1, 1034, 2,
                        '2026-01-01T00:00:00.000000000Z', 'direct',
                        '2026-01-01T00:00:02.000000000Z'
                    ),
                    (
                        'evaluation-unmatched', 'suspected-reasoning-limit-pattern', 1,
                        'call-unmatched', 0, 517, NULL,
                        '2026-01-01T00:00:01.000000000Z', 'inferred',
                        '2026-01-01T00:00:03.000000000Z'
                    );",
            )
            .unwrap();

        connection.execute_batch(MIGRATIONS[3].sql).unwrap();

        let alerts = connection
            .prepare(
                "SELECT id, evaluation_id, model_call_id, rule_id, rule_version,
                        reasoning_output_tokens, pattern_n, event_at, source_quality, created_at
                 FROM alerts ORDER BY id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            alerts,
            [(
                "evaluation-matched".to_owned(),
                "evaluation-matched".to_owned(),
                "call-matched".to_owned(),
                "suspected-reasoning-limit-pattern".to_owned(),
                1,
                1034,
                2,
                "2026-01-01T00:00:00.000000000Z".to_owned(),
                "direct".to_owned(),
                "2026-01-01T00:00:02.000000000Z".to_owned(),
            )]
        );
        let outbox_count: i64 = connection
            .query_row("SELECT count(*) FROM action_outbox", [], |row| row.get(0))
            .unwrap();
        assert_eq!(outbox_count, 0);
    }

    #[test]
    fn threshold_migration_repairs_empty_modes_and_rejects_both_or_neither() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(MIGRATIONS[0].sql).unwrap();
        connection.execute_batch(MIGRATIONS[1].sql).unwrap();
        connection
            .execute_batch(
                "UPDATE app_settings SET notify_max_n = NULL, notify_all = 0;
                 INSERT INTO rules (
                    rule_id, rule_version, rule_kind, enabled, notify_max_n, notify_all
                 ) VALUES (
                    'suspected-reasoning-limit-pattern', 1,
                    'suspected_reasoning_limit_pattern', 1, NULL, 0
                 );",
            )
            .unwrap();
        connection.execute_batch(MIGRATIONS[2].sql).unwrap();

        connection.execute_batch(MIGRATIONS[3].sql).unwrap();

        let application_threshold = connection
            .query_row(
                "SELECT notify_max_n, notify_all FROM app_settings WHERE singleton = 1",
                [],
                |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, bool>(1)?)),
            )
            .unwrap();
        let rule_threshold = connection
            .query_row(
                "SELECT notify_max_n, notify_all FROM rules
                 WHERE rule_id = 'suspected-reasoning-limit-pattern' AND rule_version = 1",
                [],
                |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, bool>(1)?)),
            )
            .unwrap();
        assert_eq!(application_threshold, (Some(1), false));
        assert_eq!(rule_threshold, (Some(1), false));

        for statement in [
            "UPDATE app_settings SET notify_max_n = NULL, notify_all = 0",
            "UPDATE app_settings SET notify_max_n = 1, notify_all = 1",
            "UPDATE rules SET notify_max_n = NULL, notify_all = 0
             WHERE rule_id = 'suspected-reasoning-limit-pattern' AND rule_version = 1",
            "UPDATE rules SET notify_max_n = 1, notify_all = 1
             WHERE rule_id = 'suspected-reasoning-limit-pattern' AND rule_version = 1",
            "INSERT INTO rules (
                rule_id, rule_version, rule_kind, enabled, notify_max_n, notify_all
             ) VALUES (
                'invalid-empty-threshold', 1,
                'suspected_reasoning_limit_pattern', 1, NULL, 0
             )",
            "INSERT INTO rules (
                rule_id, rule_version, rule_kind, enabled, notify_max_n, notify_all
             ) VALUES (
                'invalid-double-threshold', 1,
                'suspected_reasoning_limit_pattern', 1, 1, 1
             )",
        ] {
            assert!(connection.execute(statement, []).is_err(), "{statement}");
        }

        connection
            .execute("DELETE FROM app_settings WHERE singleton = 1", [])
            .unwrap();
        for threshold in ["NULL, 0", "1, 1"] {
            let statement = format!(
                "INSERT INTO app_settings (
                    singleton, revision, retention_days, notifications_enabled,
                    notify_max_n, notify_all, locale, login_autostart, updated_at
                 ) VALUES (
                    1, 2, 90, 1, {threshold}, 'en-US', 0,
                    '2026-01-01T00:00:00.000000000Z'
                 )"
            );
            assert!(connection.execute(&statement, []).is_err(), "{statement}");
        }
    }

    #[test]
    fn alert_must_reference_one_coherent_evaluation() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("alert-integrity.sqlite3");
        let connection = open_and_migrate(&database).unwrap();
        connection
            .pragma_update(None, "foreign_keys", false)
            .unwrap();
        connection
            .execute_batch(
                "INSERT INTO model_calls (
                    id, canonical_source_id, canonical_source_contract_version,
                    canonical_identity_version, canonical_identity_kind,
                    canonical_identity_digest, event_at, event_time_observation_id, origin
                 ) VALUES
                    ('call-a', 'test', 1, 1, 'stable', randomblob(32),
                     '2026-01-01T00:00:00.000000000Z', 'observation-a', 'historical'),
                    ('call-b', 'test', 1, 1, 'stable', randomblob(32),
                     '2026-01-01T00:00:00.000000000Z', 'observation-b', 'historical');",
            )
            .unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        connection
            .execute_batch(
                "INSERT INTO rules (
                    rule_id, rule_version, rule_kind, enabled, notify_max_n, notify_all
                 ) VALUES
                    ('rule-a', 1, 'suspected_reasoning_limit_pattern', 1, 1, 0),
                    ('rule-b', 1, 'suspected_reasoning_limit_pattern', 1, 1, 0);
                 INSERT INTO rule_evaluations (
                    id, rule_id, rule_version, model_call_id, matched,
                    reasoning_output_tokens, pattern_n, event_at, source_quality, evaluated_at
                 ) VALUES (
                    'evaluation-a', 'rule-a', 1, 'call-a', 1,
                    516, 1, '2026-01-01T00:00:00.000000000Z', 'direct',
                    '2026-01-01T00:00:00.000000000Z'
                 );",
            )
            .unwrap();

        let mismatched = connection.execute(
            "INSERT INTO alerts (
                id, evaluation_id, model_call_id, rule_id, rule_version,
                reasoning_output_tokens, pattern_n, event_at, source_quality, created_at
             ) VALUES (
                'alert-invalid', 'evaluation-a', 'call-b', 'rule-b', 1,
                516, 1, '2026-01-01T00:00:00.000000000Z', 'direct',
                '2026-01-01T00:00:00.000000000Z'
             )",
            [],
        );
        assert!(mismatched.is_err());
        connection
            .execute(
                "INSERT INTO alerts (
                    id, evaluation_id, model_call_id, rule_id, rule_version,
                    reasoning_output_tokens, pattern_n, event_at, source_quality, created_at
                 ) VALUES (
                    'alert-valid', 'evaluation-a', 'call-a', 'rule-a', 1,
                    516, 1, '2026-01-01T00:00:00.000000000Z', 'direct',
                    '2026-01-01T00:00:00.000000000Z'
                 )",
                [],
            )
            .unwrap();
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn historical_isolation_migration_classifies_cleans_and_guards_action_work() {
        let connection = database_before_historical_isolation();
        for (id, origin) in [
            ("call-orphan", "realtime"),
            ("call-realtime-work", "realtime"),
            ("call-realtime-evaluation", "realtime"),
            ("call-realtime-action", "realtime"),
            ("call-historical-pending-work", "historical"),
            ("call-historical-started-work", "historical"),
            ("call-historical-actions", "historical"),
        ] {
            insert_synthetic_model_call(&connection, id, origin);
        }

        insert_domain_work(
            &connection,
            "work-realtime",
            "call-realtime-work",
            "pending",
        );
        insert_domain_work(
            &connection,
            "work-historical-pending",
            "call-historical-pending-work",
            "pending",
        );
        insert_domain_work(
            &connection,
            "work-historical-started",
            "call-historical-started-work",
            "started",
        );
        insert_rule_evaluation(&connection, "call-realtime-evaluation", false);

        let historical_alert = insert_matched_alert(&connection, "call-historical-actions");
        let realtime_alert = insert_matched_alert(&connection, "call-realtime-action");
        insert_action(
            &connection,
            "action-historical-pending",
            &historical_alert,
            "pending",
            None,
            None,
            None,
            None,
            Some("source_unavailable"),
        );
        insert_action(
            &connection,
            "action-historical-started",
            &historical_alert,
            "started",
            Some("synthetic-worker"),
            Some("2026-01-01T00:01:00.000000000Z"),
            Some(STARTED_AT),
            None,
            Some("persistence_commit_failed"),
        );
        for (id, state, error_code) in [
            ("action-historical-completed", "completed", None),
            (
                "action-historical-failed",
                "failed",
                Some("source_unavailable"),
            ),
            ("action-historical-suppressed", "suppressed", None),
            (
                "action-historical-unavailable",
                "unavailable",
                Some("source_unavailable"),
            ),
        ] {
            insert_action(
                &connection,
                id,
                &historical_alert,
                state,
                None,
                None,
                Some(STARTED_AT),
                Some(COMPLETED_AT),
                error_code,
            );
        }
        insert_action(
            &connection,
            "action-realtime",
            &realtime_alert,
            "pending",
            None,
            None,
            None,
            None,
            None,
        );
        connection
            .execute(
                "INSERT INTO action_attempts (
                    action_id, attempt_number, state, started_at, completed_at, safe_error_code
                 ) VALUES
                    ('action-historical-started', 1, 'started', ?1, NULL, NULL),
                    ('action-historical-completed', 1, 'completed', ?1, ?2, NULL)",
                params![STARTED_AT, COMPLETED_AT],
            )
            .unwrap();

        connection.execute_batch(MIGRATIONS[4].sql).unwrap();

        let origins = connection
            .prepare(
                "SELECT id, origin FROM model_calls
                 WHERE id IN ('call-orphan', 'call-realtime-work', 'call-realtime-evaluation')
                 ORDER BY id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            origins,
            [
                ("call-orphan".to_owned(), "historical".to_owned()),
                ("call-realtime-evaluation".to_owned(), "realtime".to_owned()),
                ("call-realtime-work".to_owned(), "realtime".to_owned()),
            ]
        );

        let historical_work_count: i64 = connection
            .query_row(
                "SELECT count(*)
                 FROM domain_work AS work
                 INNER JOIN model_calls AS model_call ON model_call.id = work.model_call_id
                 WHERE model_call.origin = 'historical'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(historical_work_count, 0);
        let realtime_work = connection
            .query_row(
                "SELECT model_call_id, state, started_at, completed_at
                 FROM domain_work WHERE id = 'work-realtime'",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            realtime_work,
            (
                "call-realtime-work".to_owned(),
                "pending".to_owned(),
                None,
                None
            )
        );

        let pending_action = connection
            .query_row(
                "SELECT state, lease_owner, lease_expires_at, started_at,
                        completed_at, last_error_code
                 FROM action_outbox WHERE id = 'action-historical-pending'",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            pending_action,
            (
                "suppressed".to_owned(),
                None,
                None,
                None,
                Some(CREATED_AT.to_owned()),
                None
            )
        );
        let started_action = connection
            .query_row(
                "SELECT state, lease_owner, lease_expires_at, started_at,
                        completed_at, last_error_code
                 FROM action_outbox WHERE id = 'action-historical-started'",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            started_action,
            (
                "suppressed".to_owned(),
                None,
                None,
                Some(STARTED_AT.to_owned()),
                Some(STARTED_AT.to_owned()),
                None
            )
        );
        let started_attempt = connection
            .query_row(
                "SELECT state, started_at, completed_at, safe_error_code
                 FROM action_attempts
                 WHERE action_id = 'action-historical-started' AND attempt_number = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            started_attempt,
            (
                "indeterminate".to_owned(),
                STARTED_AT.to_owned(),
                Some(STARTED_AT.to_owned()),
                None
            )
        );

        let terminal_actions = connection
            .prepare(
                "SELECT id, state, started_at, completed_at, last_error_code
                 FROM action_outbox
                 WHERE id IN (
                    'action-historical-completed', 'action-historical-failed',
                    'action-historical-suppressed', 'action-historical-unavailable'
                 ) ORDER BY id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            terminal_actions,
            [
                (
                    "action-historical-completed".to_owned(),
                    "completed".to_owned(),
                    Some(STARTED_AT.to_owned()),
                    Some(COMPLETED_AT.to_owned()),
                    None
                ),
                (
                    "action-historical-failed".to_owned(),
                    "failed".to_owned(),
                    Some(STARTED_AT.to_owned()),
                    Some(COMPLETED_AT.to_owned()),
                    Some("source_unavailable".to_owned())
                ),
                (
                    "action-historical-suppressed".to_owned(),
                    "suppressed".to_owned(),
                    Some(STARTED_AT.to_owned()),
                    Some(COMPLETED_AT.to_owned()),
                    None
                ),
                (
                    "action-historical-unavailable".to_owned(),
                    "unavailable".to_owned(),
                    Some(STARTED_AT.to_owned()),
                    Some(COMPLETED_AT.to_owned()),
                    Some("source_unavailable".to_owned())
                ),
            ]
        );
        let completed_attempt = connection
            .query_row(
                "SELECT state, started_at, completed_at
                 FROM action_attempts
                 WHERE action_id = 'action-historical-completed' AND attempt_number = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            completed_attempt,
            (
                "completed".to_owned(),
                STARTED_AT.to_owned(),
                Some(COMPLETED_AT.to_owned())
            )
        );
        let historical_replayable_actions: i64 = connection
            .query_row(
                "SELECT count(*)
                 FROM action_outbox AS action
                 INNER JOIN alerts AS alert ON alert.id = action.alert_id
                 INNER JOIN model_calls AS model_call ON model_call.id = alert.model_call_id
                 WHERE model_call.origin = 'historical'
                   AND action.state IN ('pending', 'started')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(historical_replayable_actions, 0);
        let realtime_action_state: String = connection
            .query_row(
                "SELECT state FROM action_outbox WHERE id = 'action-realtime'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(realtime_action_state, "pending");

        for (statement, expected_message) in [
            (
                "UPDATE model_calls SET origin = 'realtime' WHERE id = 'call-orphan'",
                "model call origin is immutable",
            ),
            (
                "INSERT INTO domain_work (
                    id, work_kind, model_call_id, state, created_at
                 ) VALUES (
                    'work-invalid-insert', 'model_call.completed',
                    'call-orphan', 'pending',
                    '2026-01-01T00:00:00.000000000Z'
                 )",
                "historical model call cannot enter realtime work",
            ),
            (
                "UPDATE domain_work SET model_call_id = 'call-orphan'
                 WHERE id = 'work-realtime'",
                "historical model call cannot enter realtime work",
            ),
            (
                "INSERT INTO action_outbox (
                    id, alert_id, action_type, idempotency_key, state, model,
                    reasoning_output_tokens, pattern_n, created_at, not_before
                 ) VALUES (
                    'action-invalid-insert', 'alert-call-historical-actions',
                    'system_notification', 'synthetic:action-invalid-insert',
                    'pending', 'synthetic-model', 516, 1,
                    '2026-01-01T00:00:00.000000000Z',
                    '2026-01-01T00:00:00.000000000Z'
                 )",
                "historical alert cannot enter the action outbox",
            ),
            (
                "UPDATE action_outbox
                 SET alert_id = 'alert-call-historical-actions'
                 WHERE id = 'action-realtime'",
                "historical alert cannot enter the action outbox",
            ),
        ] {
            assert_guard_abort(&connection, statement, expected_message);
        }

        let foreign_key_violations = connection
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query_map([], |_| Ok(()))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(foreign_key_violations.is_empty());
        let integrity: String = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn transactional_outbox_migration_terminalizes_pre_v6_work() {
        let connection = database_before_historical_isolation();
        for (suffix, origin) in [
            ("pending", "realtime"),
            ("started", "realtime"),
            ("completed", "realtime"),
            ("failed", "realtime"),
            ("suppressed", "realtime"),
            ("unavailable", "realtime"),
            ("historical", "historical"),
        ] {
            let model_call_id = format!("call-v6-{suffix}");
            insert_synthetic_model_call(&connection, &model_call_id, origin);
            let alert_id = insert_matched_alert(&connection, &model_call_id);
            let action_id = format!("action-v6-{suffix}");
            let (state, lease_owner, lease_expires_at, started_at, completed_at, error) =
                match suffix {
                    "pending" | "historical" => (
                        "pending",
                        None,
                        None,
                        None,
                        None,
                        Some("source_unavailable"),
                    ),
                    "started" => (
                        "started",
                        Some("legacy-worker"),
                        Some("2026-01-01T00:01:00.000000000Z"),
                        Some(STARTED_AT),
                        None,
                        Some("persistence_commit_failed"),
                    ),
                    "completed" => (
                        "completed",
                        None,
                        None,
                        Some(STARTED_AT),
                        Some(COMPLETED_AT),
                        Some("source_unavailable"),
                    ),
                    "failed" => (
                        "failed",
                        None,
                        None,
                        Some(STARTED_AT),
                        Some(COMPLETED_AT),
                        Some("persistence_commit_failed"),
                    ),
                    "suppressed" => (
                        "suppressed",
                        None,
                        None,
                        Some(STARTED_AT),
                        Some(COMPLETED_AT),
                        None,
                    ),
                    "unavailable" => (
                        "unavailable",
                        None,
                        None,
                        None,
                        Some(COMPLETED_AT),
                        Some("source_unavailable"),
                    ),
                    _ => unreachable!(),
                };
            insert_action(
                &connection,
                &action_id,
                &alert_id,
                state,
                lease_owner,
                lease_expires_at,
                started_at,
                completed_at,
                error,
            );
        }
        connection
            .execute(
                "INSERT INTO action_attempts (
                    action_id, attempt_number, state, started_at, completed_at, safe_error_code
                 ) VALUES
                    ('action-v6-started', 1, 'started', ?1, NULL, NULL),
                    ('action-v6-completed', 1, 'completed', ?1, ?2, NULL),
                    ('action-v6-failed', 1, 'failed', ?1, ?2, 'persistence_commit_failed'),
                    ('action-v6-unavailable', 1, 'unavailable', ?1, ?2, 'source_unavailable')",
                params![STARTED_AT, COMPLETED_AT],
            )
            .unwrap();

        connection.execute_batch(MIGRATIONS[4].sql).unwrap();
        connection.execute_batch(MIGRATIONS[5].sql).unwrap();

        let load_action = |id: &str| -> StoredAction {
            connection
                .query_row(
                    "SELECT state, idempotency_key_version, idempotency_key_bytes,
                            lease_generation, started_at, completed_at,
                            last_error_code, suppression_reason
                     FROM action_outbox WHERE id = ?1",
                    [id],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                            row.get(7)?,
                        ))
                    },
                )
                .unwrap()
        };
        let legacy_key = |id: &str| format!("synthetic:{id}").into_bytes();

        assert_eq!(
            load_action("action-v6-pending"),
            (
                "suppressed".to_owned(),
                0,
                legacy_key("action-v6-pending"),
                0,
                None,
                Some(CREATED_AT.to_owned()),
                None,
                Some("pre_outbox_upgrade".to_owned()),
            )
        );
        assert_eq!(
            load_action("action-v6-started"),
            (
                "failed".to_owned(),
                0,
                legacy_key("action-v6-started"),
                0,
                Some(STARTED_AT.to_owned()),
                Some(STARTED_AT.to_owned()),
                Some("indeterminate_after_crash".to_owned()),
                None,
            )
        );
        assert_eq!(
            load_action("action-v6-completed"),
            (
                "completed".to_owned(),
                0,
                legacy_key("action-v6-completed"),
                0,
                Some(STARTED_AT.to_owned()),
                Some(COMPLETED_AT.to_owned()),
                None,
                None,
            )
        );
        assert_eq!(
            load_action("action-v6-failed"),
            (
                "failed".to_owned(),
                0,
                legacy_key("action-v6-failed"),
                0,
                Some(STARTED_AT.to_owned()),
                Some(COMPLETED_AT.to_owned()),
                Some("notification_dispatch_failed".to_owned()),
                None,
            )
        );
        assert_eq!(
            load_action("action-v6-suppressed"),
            (
                "suppressed".to_owned(),
                0,
                legacy_key("action-v6-suppressed"),
                0,
                None,
                Some(COMPLETED_AT.to_owned()),
                None,
                Some("pre_outbox_upgrade".to_owned()),
            )
        );
        assert_eq!(
            load_action("action-v6-unavailable"),
            (
                "unavailable".to_owned(),
                0,
                legacy_key("action-v6-unavailable"),
                0,
                None,
                Some(COMPLETED_AT.to_owned()),
                Some("notification_unavailable".to_owned()),
                None,
            )
        );
        assert_eq!(
            load_action("action-v6-historical"),
            (
                "suppressed".to_owned(),
                0,
                legacy_key("action-v6-historical"),
                0,
                None,
                Some(CREATED_AT.to_owned()),
                None,
                Some("historical_event".to_owned()),
            )
        );

        let replayable_legacy_count: i64 = connection
            .query_row(
                "SELECT count(*) FROM action_outbox
                 WHERE idempotency_key_version = 0 AND state IN ('pending', 'started')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(replayable_legacy_count, 0);
        let lingering_lease_count: i64 = connection
            .query_row(
                "SELECT count(*) FROM action_outbox
                 WHERE lease_owner IS NOT NULL OR lease_expires_at IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(lingering_lease_count, 0);

        let attempts = connection
            .prepare(
                "SELECT action_id, state, lease_generation, completed_at, safe_error_code
                 FROM action_attempts ORDER BY action_id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            attempts,
            [
                (
                    "action-v6-completed".to_owned(),
                    "completed".to_owned(),
                    0,
                    Some(COMPLETED_AT.to_owned()),
                    None,
                ),
                (
                    "action-v6-failed".to_owned(),
                    "failed".to_owned(),
                    0,
                    Some(COMPLETED_AT.to_owned()),
                    Some("notification_dispatch_failed".to_owned()),
                ),
                (
                    "action-v6-started".to_owned(),
                    "indeterminate".to_owned(),
                    0,
                    Some(STARTED_AT.to_owned()),
                    Some("indeterminate_after_crash".to_owned()),
                ),
                (
                    "action-v6-unavailable".to_owned(),
                    "unavailable".to_owned(),
                    0,
                    Some(COMPLETED_AT.to_owned()),
                    Some("notification_unavailable".to_owned()),
                ),
            ]
        );

        let foreign_key_violations = connection
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query_map([], |_| Ok(()))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(foreign_key_violations.is_empty());
        let integrity: String = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn transactional_outbox_guards_requests_fencing_attempts_and_history() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("transactional-outbox.sqlite3");
        let connection = open_and_migrate(&database).unwrap();
        connection
            .execute(
                "INSERT INTO rules (
                    rule_id, rule_version, rule_kind, enabled, notify_max_n, notify_all
                 ) VALUES (?1, 1, 'suspected_reasoning_limit_pattern', 1, 1, 0)",
                [SYNTHETIC_RULE_ID],
            )
            .unwrap();
        insert_synthetic_model_call(&connection, "call-v6-live", "realtime");
        insert_synthetic_model_call(&connection, "call-v6-history", "historical");
        let live_alert = insert_matched_alert(&connection, "call-v6-live");
        let historical_alert = insert_matched_alert(&connection, "call-v6-history");

        let columns = connection
            .prepare("SELECT name FROM pragma_table_info('action_outbox') ORDER BY cid")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            columns,
            [
                "id",
                "alert_id",
                "action_type",
                "parameter_schema_version",
                "idempotency_key_version",
                "idempotency_key_bytes",
                "state",
                "model",
                "reasoning_output_tokens",
                "pattern_n",
                "created_at",
                "not_before",
                "lease_generation",
                "lease_owner",
                "lease_expires_at",
                "started_at",
                "completed_at",
                "last_error_code",
                "suppression_reason",
            ]
        );

        connection
            .execute(
                "INSERT INTO action_outbox (
                    id, alert_id, action_type, parameter_schema_version,
                    idempotency_key_version, idempotency_key_bytes, state, model,
                    reasoning_output_tokens, pattern_n, created_at, not_before
                 ) VALUES (?1, ?2, 'system_notification', 1, 1, ?3, 'pending',
                           'synthetic-model', 516, 1, ?4, ?4)",
                params!["action-v6-live", live_alert, vec![1_u8; 32], CREATED_AT],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO action_outbox (
                    id, alert_id, action_type, parameter_schema_version,
                    idempotency_key_version, idempotency_key_bytes, state,
                    reasoning_output_tokens, pattern_n, created_at, not_before
                 ) VALUES (?1, ?2, 'future_action_2', 2, 1, ?3, 'pending',
                           516, 1, ?4, ?4)",
                params!["action-v6-future", live_alert, vec![2_u8; 32], CREATED_AT],
            )
            .unwrap();

        let invalid_action_type = connection.execute(
            "INSERT INTO action_outbox (
                id, alert_id, action_type, parameter_schema_version,
                idempotency_key_version, idempotency_key_bytes, state,
                reasoning_output_tokens, pattern_n, created_at, not_before
             ) VALUES ('action-v6-invalid-type', ?1, 'Future__Action', 3, 1,
                       ?2, 'pending', 516, 1, ?3, ?3)",
            params![live_alert, vec![3_u8; 32], CREATED_AT],
        );
        assert!(invalid_action_type.is_err());
        let executable_v0 = connection.execute(
            "INSERT INTO action_outbox (
                id, alert_id, action_type, parameter_schema_version,
                idempotency_key_version, idempotency_key_bytes, state,
                reasoning_output_tokens, pattern_n, created_at, not_before
             ) VALUES ('action-v6-invalid-v0', ?1, 'future_action_3', 3, 0,
                       CAST('legacy-key' AS BLOB), 'pending', 516, 1, ?2, ?2)",
            params![live_alert, CREATED_AT],
        );
        assert!(executable_v0.is_err());
        assert_guard_abort(
            &connection,
            "INSERT INTO action_outbox (
                id, alert_id, action_type, parameter_schema_version,
                idempotency_key_version, idempotency_key_bytes, state,
                reasoning_output_tokens, pattern_n, created_at, not_before
             ) VALUES (
                'action-v6-historical', 'alert-call-v6-history',
                'system_notification', 1, 1, zeroblob(32), 'pending',
                516, 1, '2026-01-01T00:00:00.000000000Z',
                '2026-01-01T00:00:00.000000000Z'
             )",
            "historical alert cannot enter the action outbox",
        );
        assert_eq!(historical_alert, "alert-call-v6-history");
        assert_guard_abort(
            &connection,
            "UPDATE action_outbox SET model = 'rewritten' WHERE id = 'action-v6-live'",
            "action request is immutable",
        );
        assert_guard_abort(
            &connection,
            "UPDATE action_outbox
             SET lease_generation = 2, lease_owner = 'worker-a',
                 lease_expires_at = '2026-01-01T00:01:00.000000000Z'
             WHERE id = 'action-v6-live'",
            "invalid action lease transition",
        );
        assert_guard_abort(
            &connection,
            "UPDATE action_outbox
             SET state = 'completed', started_at = '2026-01-01T00:00:01.000000000Z',
                 completed_at = '2026-01-01T00:00:02.000000000Z'
             WHERE id = 'action-v6-live'",
            "invalid action state transition",
        );

        connection
            .execute(
                "UPDATE action_outbox
                 SET lease_generation = 1, lease_owner = 'worker-a', lease_expires_at = ?1
                 WHERE id = 'action-v6-live'",
                ["2026-01-01T00:01:00.000000000Z"],
            )
            .unwrap();
        assert_guard_abort(
            &connection,
            "INSERT INTO action_attempts (
                action_id, attempt_number, lease_generation, state, started_at
             ) VALUES (
                'action-v6-live', 1, 1, 'started',
                '2026-01-01T00:00:01.000000000Z'
             )",
            "invalid initial action attempt",
        );
        connection
            .execute(
                "UPDATE action_outbox SET state = 'started', started_at = ?1
                 WHERE id = 'action-v6-live'",
                [STARTED_AT],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO action_attempts (
                    action_id, attempt_number, lease_generation, state, started_at
                 ) VALUES ('action-v6-live', 1, 1, 'started', ?1)",
                [STARTED_AT],
            )
            .unwrap();
        assert_guard_abort(
            &connection,
            "UPDATE action_attempts SET attempt_number = 2
             WHERE action_id = 'action-v6-live' AND attempt_number = 1",
            "action attempt identity is immutable",
        );
        let duplicate_generation = connection.execute(
            "INSERT INTO action_attempts (
                action_id, attempt_number, lease_generation, state, started_at
             ) VALUES ('action-v6-live', 2, 1, 'started', ?1)",
            [STARTED_AT],
        );
        assert!(duplicate_generation.is_err());
        connection
            .execute(
                "UPDATE action_attempts
                 SET state = 'completed', completed_at = ?1
                 WHERE action_id = 'action-v6-live' AND attempt_number = 1",
                [COMPLETED_AT],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE action_outbox
                 SET state = 'completed', completed_at = ?1,
                     lease_owner = NULL, lease_expires_at = NULL
                 WHERE id = 'action-v6-live'",
                [COMPLETED_AT],
            )
            .unwrap();
        assert_guard_abort(
            &connection,
            "UPDATE action_outbox SET completed_at = '2026-01-01T00:00:03.000000000Z'
             WHERE id = 'action-v6-live'",
            "terminal action is immutable",
        );
        assert_guard_abort(
            &connection,
            "UPDATE action_attempts
             SET completed_at = '2026-01-01T00:00:03.000000000Z'
             WHERE action_id = 'action-v6-live' AND attempt_number = 1",
            "terminal action attempt is immutable",
        );

        let historical_trigger_count: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_schema
                 WHERE type = 'trigger'
                   AND name IN (
                       'action_outbox_historical_insert_guard',
                       'action_outbox_historical_update_guard'
                   )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(historical_trigger_count, 2);
        let foreign_key_violations = connection
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query_map([], |_| Ok(()))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(foreign_key_violations.is_empty());
        let integrity: String = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
    }

    #[test]
    fn notification_coalescing_migration_does_not_regroup_existing_actions() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        for migration in &MIGRATIONS[..6] {
            connection.execute_batch(migration.sql).unwrap();
        }
        connection
            .execute(
                "INSERT INTO rules (
                    rule_id, rule_version, rule_kind, enabled, notify_max_n, notify_all
                 ) VALUES (?1, 1, 'suspected_reasoning_limit_pattern', 1, 1, 0)",
                [SYNTHETIC_RULE_ID],
            )
            .unwrap();
        insert_synthetic_conversation_model_call(
            &connection,
            "call-v6-direct-audit",
            "conversation-v6-direct-audit",
        );
        let alert_id = insert_matched_alert(&connection, "call-v6-direct-audit");
        insert_coalescing_action(
            &connection,
            "action-v6-direct-audit",
            &alert_id,
            "pending",
            CREATED_AT,
            CREATED_AT,
            1,
        );

        connection.execute_batch(MIGRATIONS[6].sql).unwrap();

        let action_count: i64 = connection
            .query_row(
                "SELECT count(*) FROM action_outbox WHERE id = 'action-v6-direct-audit'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let window_count: i64 = connection
            .query_row(
                "SELECT count(*) FROM notification_coalescing_windows",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let member_count: i64 = connection
            .query_row(
                "SELECT count(*) FROM notification_coalescing_members",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(action_count, 1);
        assert_eq!(window_count, 0);
        assert_eq!(member_count, 0);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn notification_coalescing_guards_membership_intervals_and_retention_cleanup() {
        const WINDOW_ID: &str = "00000000-0000-4000-8000-000000000001";
        const WINDOW_CLOSES_AT: &str = "2026-01-01T00:00:30.000000000Z";
        const FOLLOWER_JOINED_AT: &str = "2026-01-01T00:00:10.000000000Z";
        const OTHER_JOINED_AT: &str = "2026-01-01T00:00:12.000000000Z";
        const WRONG_STATE_JOINED_AT: &str = "2026-01-01T00:00:15.000000000Z";
        const WRONG_NOT_BEFORE_JOINED_AT: &str = "2026-01-01T00:00:16.000000000Z";
        const WRONG_NOT_BEFORE_AT: &str = "2026-01-01T00:00:17.000000000Z";
        const WRONG_COMPLETED_JOINED_AT: &str = "2026-01-01T00:00:18.000000000Z";
        const WRONG_COMPLETED_AT: &str = "2026-01-01T00:00:19.000000000Z";
        const OVERLAP_OPENED_AT: &str = "2026-01-01T00:00:20.000000000Z";
        const OVERLAP_CLOSES_AT: &str = "2026-01-01T00:00:50.000000000Z";
        const LATER_OPENED_AT: &str = "2026-01-01T00:01:00.000000000Z";
        const LATER_CLOSES_AT: &str = "2026-01-01T00:01:30.000000000Z";
        const DURATION_29_OPENED_AT: &str = "2026-01-01T00:02:00.000000000Z";
        const DURATION_29_CLOSES_AT: &str = "2026-01-01T00:02:29.000000000Z";
        const DURATION_31_OPENED_AT: &str = "2026-01-01T00:03:00.000000000Z";
        const DURATION_31_CLOSES_AT: &str = "2026-01-01T00:03:31.000000000Z";

        let directory = tempdir().unwrap();
        let database = directory.path().join("notification-coalescing.sqlite3");
        let connection = open_and_migrate(&database).unwrap();

        let tables = connection
            .prepare(
                "SELECT name FROM sqlite_schema
                 WHERE type = 'table' AND name GLOB 'notification_coalescing_*'
                 ORDER BY name",
            )
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            tables,
            [
                "notification_coalescing_members",
                "notification_coalescing_windows",
            ]
        );
        let indexes = connection
            .prepare(
                "SELECT name FROM sqlite_schema
                 WHERE type = 'index'
                   AND name GLOB 'notification_coalescing_*'
                   AND sql IS NOT NULL
                 ORDER BY name",
            )
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            indexes,
            [
                "notification_coalescing_members_one_leader_idx",
                "notification_coalescing_members_window_joined_idx",
                "notification_coalescing_windows_closes_at_idx",
                "notification_coalescing_windows_conversation_interval_idx",
            ]
        );
        let triggers = connection
            .prepare(
                "SELECT name FROM sqlite_schema
                 WHERE type = 'trigger' AND name GLOB 'notification_coalescing_*'
                 ORDER BY name",
            )
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            triggers,
            [
                "notification_coalescing_members_empty_window_cleanup",
                "notification_coalescing_members_immutable_guard",
                "notification_coalescing_members_insert_guard",
                "notification_coalescing_members_retention_delete_guard",
                "notification_coalescing_windows_identity_immutable_guard",
                "notification_coalescing_windows_leader_immutable_guard",
                "notification_coalescing_windows_leader_insert_guard",
                "notification_coalescing_windows_nonempty_delete_guard",
                "notification_coalescing_windows_overlap_insert_guard",
            ]
        );

        connection
            .execute(
                "INSERT INTO rules (
                    rule_id, rule_version, rule_kind, enabled, notify_max_n, notify_all
                 ) VALUES (?1, 1, 'suspected_reasoning_limit_pattern', 1, 1, 0)",
                [SYNTHETIC_RULE_ID],
            )
            .unwrap();

        insert_synthetic_conversation_model_call(
            &connection,
            "call-coalescing-leader",
            "conversation-a",
        );
        let leader_alert = insert_matched_alert(&connection, "call-coalescing-leader");
        insert_coalescing_action(
            &connection,
            "action-coalescing-leader",
            &leader_alert,
            "pending",
            CREATED_AT,
            WINDOW_CLOSES_AT,
            1,
        );
        connection
            .execute(
                "INSERT INTO notification_coalescing_windows (
                    id, conversation_id, leader_action_id, opened_at, closes_at
                 ) VALUES (?1, 'conversation-a', 'action-coalescing-leader', ?2, ?3)",
                params![WINDOW_ID, CREATED_AT, WINDOW_CLOSES_AT],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO notification_coalescing_members (
                    action_id, window_id, role, joined_at
                 ) VALUES ('action-coalescing-leader', ?1, 'leader', ?2)",
                params![WINDOW_ID, CREATED_AT],
            )
            .unwrap();

        insert_synthetic_conversation_model_call(
            &connection,
            "call-coalescing-follower",
            "conversation-a",
        );
        let follower_alert = insert_matched_alert(&connection, "call-coalescing-follower");
        insert_coalescing_action(
            &connection,
            "action-coalescing-follower",
            &follower_alert,
            "suppressed",
            FOLLOWER_JOINED_AT,
            FOLLOWER_JOINED_AT,
            2,
        );
        connection
            .execute(
                "INSERT INTO notification_coalescing_members (
                    action_id, window_id, role, joined_at
                 ) VALUES ('action-coalescing-follower', ?1, 'follower', ?2)",
                params![WINDOW_ID, FOLLOWER_JOINED_AT],
            )
            .unwrap();

        let stored_members = connection
            .prepare(
                "SELECT action_id, role, joined_at
                 FROM notification_coalescing_members
                 WHERE window_id = ?1 ORDER BY joined_at",
            )
            .unwrap()
            .query_map([WINDOW_ID], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            stored_members,
            [
                (
                    "action-coalescing-leader".to_owned(),
                    "leader".to_owned(),
                    CREATED_AT.to_owned(),
                ),
                (
                    "action-coalescing-follower".to_owned(),
                    "follower".to_owned(),
                    FOLLOWER_JOINED_AT.to_owned(),
                ),
            ]
        );

        insert_synthetic_conversation_model_call(
            &connection,
            "call-coalescing-other-conversation",
            "conversation-b",
        );
        let other_alert = insert_matched_alert(&connection, "call-coalescing-other-conversation");
        insert_coalescing_action(
            &connection,
            "action-coalescing-other-conversation",
            &other_alert,
            "suppressed",
            OTHER_JOINED_AT,
            OTHER_JOINED_AT,
            3,
        );
        assert_guard_abort(
            &connection,
            "INSERT INTO notification_coalescing_members (
                action_id, window_id, role, joined_at
             ) VALUES (
                'action-coalescing-other-conversation',
                '00000000-0000-4000-8000-000000000001', 'follower',
                '2026-01-01T00:00:12.000000000Z'
             )",
            "invalid notification coalescing member",
        );

        insert_synthetic_conversation_model_call(
            &connection,
            "call-coalescing-boundary",
            "conversation-a",
        );
        let boundary_alert = insert_matched_alert(&connection, "call-coalescing-boundary");
        insert_coalescing_action(
            &connection,
            "action-coalescing-boundary",
            &boundary_alert,
            "suppressed",
            WINDOW_CLOSES_AT,
            WINDOW_CLOSES_AT,
            4,
        );
        assert_guard_abort(
            &connection,
            "INSERT INTO notification_coalescing_members (
                action_id, window_id, role, joined_at
             ) VALUES (
                'action-coalescing-boundary',
                '00000000-0000-4000-8000-000000000001', 'follower',
                '2026-01-01T00:00:30.000000000Z'
             )",
            "invalid notification coalescing member",
        );

        insert_synthetic_conversation_model_call(
            &connection,
            "call-coalescing-wrong-state",
            "conversation-a",
        );
        let wrong_state_alert = insert_matched_alert(&connection, "call-coalescing-wrong-state");
        insert_coalescing_action(
            &connection,
            "action-coalescing-wrong-state",
            &wrong_state_alert,
            "pending",
            WRONG_STATE_JOINED_AT,
            WRONG_STATE_JOINED_AT,
            5,
        );
        assert_guard_abort(
            &connection,
            "INSERT INTO notification_coalescing_members (
                action_id, window_id, role, joined_at
             ) VALUES (
                'action-coalescing-wrong-state',
                '00000000-0000-4000-8000-000000000001', 'follower',
                '2026-01-01T00:00:15.000000000Z'
            )",
            "invalid notification coalescing member",
        );

        insert_synthetic_conversation_model_call(
            &connection,
            "call-coalescing-wrong-not-before",
            "conversation-a",
        );
        let wrong_not_before_alert =
            insert_matched_alert(&connection, "call-coalescing-wrong-not-before");
        insert_coalescing_action(
            &connection,
            "action-coalescing-wrong-not-before",
            &wrong_not_before_alert,
            "suppressed",
            WRONG_NOT_BEFORE_JOINED_AT,
            WRONG_NOT_BEFORE_AT,
            6,
        );
        assert_guard_abort(
            &connection,
            "INSERT INTO notification_coalescing_members (
                action_id, window_id, role, joined_at
             ) VALUES (
                'action-coalescing-wrong-not-before',
                '00000000-0000-4000-8000-000000000001', 'follower',
                '2026-01-01T00:00:16.000000000Z'
             )",
            "invalid notification coalescing member",
        );

        insert_synthetic_conversation_model_call(
            &connection,
            "call-coalescing-wrong-completed",
            "conversation-a",
        );
        let wrong_completed_alert =
            insert_matched_alert(&connection, "call-coalescing-wrong-completed");
        connection
            .execute(
                "INSERT INTO action_outbox (
                    id, alert_id, action_type, parameter_schema_version,
                    idempotency_key_version, idempotency_key_bytes, state,
                    reasoning_output_tokens, pattern_n, created_at, not_before,
                    completed_at, suppression_reason
                 ) VALUES (
                    'action-coalescing-wrong-completed', ?1,
                    'system_notification', 1, 1, ?2, 'suppressed',
                    516, 1, ?3, ?3, ?4, 'coalesced'
                 )",
                params![
                    wrong_completed_alert,
                    vec![7_u8; 32],
                    WRONG_COMPLETED_JOINED_AT,
                    WRONG_COMPLETED_AT,
                ],
            )
            .unwrap();
        assert_guard_abort(
            &connection,
            "INSERT INTO notification_coalescing_members (
                action_id, window_id, role, joined_at
             ) VALUES (
                'action-coalescing-wrong-completed',
                '00000000-0000-4000-8000-000000000001', 'follower',
                '2026-01-01T00:00:18.000000000Z'
             )",
            "invalid notification coalescing member",
        );

        insert_synthetic_conversation_model_call(
            &connection,
            "call-coalescing-overlap",
            "conversation-a",
        );
        let overlap_alert = insert_matched_alert(&connection, "call-coalescing-overlap");
        insert_coalescing_action(
            &connection,
            "action-coalescing-overlap",
            &overlap_alert,
            "pending",
            OVERLAP_OPENED_AT,
            OVERLAP_CLOSES_AT,
            8,
        );
        assert_guard_abort(
            &connection,
            "INSERT INTO notification_coalescing_windows (
                id, conversation_id, leader_action_id, opened_at, closes_at
             ) VALUES (
                '00000000-0000-4000-8000-000000000002', 'conversation-a',
                'action-coalescing-overlap',
                '2026-01-01T00:00:20.000000000Z',
                '2026-01-01T00:00:50.000000000Z'
             )",
            "notification coalescing windows overlap",
        );

        insert_synthetic_conversation_model_call(
            &connection,
            "call-coalescing-invalid-leader",
            "conversation-a",
        );
        let invalid_leader_alert =
            insert_matched_alert(&connection, "call-coalescing-invalid-leader");
        insert_coalescing_action(
            &connection,
            "action-coalescing-invalid-leader",
            &invalid_leader_alert,
            "suppressed",
            LATER_OPENED_AT,
            LATER_CLOSES_AT,
            9,
        );
        assert_guard_abort(
            &connection,
            "INSERT INTO notification_coalescing_windows (
                id, conversation_id, leader_action_id, opened_at, closes_at
             ) VALUES (
                '00000000-0000-4000-8000-000000000003', 'conversation-a',
                'action-coalescing-invalid-leader',
                '2026-01-01T00:01:00.000000000Z',
                '2026-01-01T00:01:30.000000000Z'
            )",
            "invalid notification coalescing leader",
        );

        insert_synthetic_conversation_model_call(
            &connection,
            "call-coalescing-duration-29",
            "conversation-a",
        );
        let duration_29_alert = insert_matched_alert(&connection, "call-coalescing-duration-29");
        insert_coalescing_action(
            &connection,
            "action-coalescing-duration-29",
            &duration_29_alert,
            "pending",
            DURATION_29_OPENED_AT,
            DURATION_29_CLOSES_AT,
            10,
        );
        assert_guard_abort(
            &connection,
            "INSERT INTO notification_coalescing_windows (
                id, conversation_id, leader_action_id, opened_at, closes_at
             ) VALUES (
                '00000000-0000-4000-8000-000000000004', 'conversation-a',
                'action-coalescing-duration-29',
                '2026-01-01T00:02:00.000000000Z',
                '2026-01-01T00:02:29.000000000Z'
             )",
            "CHECK constraint failed",
        );

        insert_synthetic_conversation_model_call(
            &connection,
            "call-coalescing-duration-31",
            "conversation-a",
        );
        let duration_31_alert = insert_matched_alert(&connection, "call-coalescing-duration-31");
        insert_coalescing_action(
            &connection,
            "action-coalescing-duration-31",
            &duration_31_alert,
            "pending",
            DURATION_31_OPENED_AT,
            DURATION_31_CLOSES_AT,
            11,
        );
        assert_guard_abort(
            &connection,
            "INSERT INTO notification_coalescing_windows (
                id, conversation_id, leader_action_id, opened_at, closes_at
             ) VALUES (
                '00000000-0000-4000-8000-000000000005', 'conversation-a',
                'action-coalescing-duration-31',
                '2026-01-01T00:03:00.000000000Z',
                '2026-01-01T00:03:31.000000000Z'
             )",
            "CHECK constraint failed",
        );

        assert_guard_abort(
            &connection,
            "UPDATE notification_coalescing_windows
             SET opened_at = '2025-12-31T23:59:59.000000000Z'
             WHERE id = '00000000-0000-4000-8000-000000000001'",
            "notification coalescing window is immutable",
        );
        assert_guard_abort(
            &connection,
            "UPDATE notification_coalescing_windows SET leader_action_id = NULL
             WHERE id = '00000000-0000-4000-8000-000000000001'",
            "notification coalescing leader is immutable",
        );
        assert_guard_abort(
            &connection,
            "UPDATE notification_coalescing_members
             SET joined_at = '2026-01-01T00:00:11.000000000Z'
             WHERE action_id = 'action-coalescing-follower'",
            "notification coalescing member is immutable",
        );
        assert_guard_abort(
            &connection,
            "DELETE FROM notification_coalescing_members
             WHERE action_id = 'action-coalescing-follower'",
            "notification coalescing member requires its action",
        );
        assert_guard_abort(
            &connection,
            "DELETE FROM notification_coalescing_windows
             WHERE id = '00000000-0000-4000-8000-000000000001'",
            "notification coalescing window still has members",
        );

        connection
            .execute(
                "UPDATE action_outbox
                 SET state = 'suppressed', completed_at = ?1,
                     suppression_reason = 'notifications_disabled'
                 WHERE id = 'action-coalescing-leader'",
                [WINDOW_CLOSES_AT],
            )
            .unwrap();
        connection
            .execute(
                "DELETE FROM action_outbox WHERE id = 'action-coalescing-leader'",
                [],
            )
            .unwrap();
        let retained_window = connection
            .query_row(
                "SELECT leader_action_id,
                        (SELECT action_id FROM notification_coalescing_members
                         WHERE window_id = notification_coalescing_windows.id),
                        (SELECT count(*) FROM notification_coalescing_members
                         WHERE window_id = notification_coalescing_windows.id)
                 FROM notification_coalescing_windows WHERE id = ?1",
                [WINDOW_ID],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            retained_window,
            (None, Some("action-coalescing-follower".to_owned()), 1,)
        );

        connection
            .execute(
                "DELETE FROM action_outbox WHERE id = 'action-coalescing-follower'",
                [],
            )
            .unwrap();
        let remaining_window_count: i64 = connection
            .query_row(
                "SELECT count(*) FROM notification_coalescing_windows WHERE id = ?1",
                [WINDOW_ID],
                |row| row.get(0),
            )
            .unwrap();
        let remaining_member_count: i64 = connection
            .query_row(
                "SELECT count(*) FROM notification_coalescing_members WHERE window_id = ?1",
                [WINDOW_ID],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining_window_count, 0);
        assert_eq!(remaining_member_count, 0);

        let foreign_key_violations = connection
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query_map([], |_| Ok(()))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(foreign_key_violations.is_empty());
        let integrity: String = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
    }

    #[test]
    fn refuses_a_modified_applied_migration_checksum() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("tampered.sqlite3");
        let connection = open_and_migrate(&database).unwrap();
        connection
            .execute(
                "UPDATE schema_migrations SET sha256 = ?1 WHERE version = 1",
                ["0".repeat(64)],
            )
            .unwrap();
        drop(connection);

        let error = open_and_migrate(&database).unwrap_err();
        assert!(matches!(
            error,
            StorageError::MigrationChecksumMismatch { version: 1 }
        ));
    }

    #[test]
    fn refuses_registry_drift_before_returning_a_writer_connection() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("registry-drift.sqlite3");
        let connection = open_and_migrate(&database).unwrap();
        connection
            .execute(
                "DELETE FROM telemetry_field_registry WHERE field_id = 'model_call.model'",
                [],
            )
            .unwrap();
        drop(connection);

        let error = open_and_migrate(&database).unwrap_err();
        assert!(matches!(error, StorageError::UnregisteredTelemetryField));
    }
}
