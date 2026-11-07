use std::num::{NonZeroU16, NonZeroU32, NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::Arc;

use codex_monitor_core::{
    ActionId, AlertId, BuiltInRuleId, CanonicalField, Conversation, ConversationId,
    ConversationSourceId, FieldId, FieldProvenance, ModelCallId, Observed, PatternN, RuleVersion,
    SafeErrorCode, SourceContractVersion, SourceHealthStatus, SourceId, SourceIdentity,
    SourcePriority, ThreadSourceId, TokenCount,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension as _, params};

use super::action_outbox::{load_action, load_action_attempts};
use super::codec::{parse_evidence_quality, parse_timestamp};
use super::migrations::configure_read_connection;
use super::projection::load_model_call;
use super::timestamp::timestamp_text;
use super::{
    AppLocale, AppSettings, MAX_MODEL_CALL_QUERY_LIMIT, ModelCallQuery, RetentionPolicy,
    SqlitePragmas, StorageError, StorageResult, StoredAction, StoredActionAttempt, StoredAlert,
    StoredModelCall, StoredSettingsDocument, StoredSourceHealth,
};

type RankedTextRaw = (
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<i64>,
);

/// Read repository that opens a fresh query-only connection per operation.
#[derive(Clone, Debug)]
pub struct StorageReader {
    database_path: Arc<PathBuf>,
}

impl StorageReader {
    #[must_use]
    pub fn new(database_path: PathBuf) -> Self {
        Self {
            database_path: Arc::new(database_path),
        }
    }

    #[must_use]
    pub fn database_path(&self) -> &Path {
        self.database_path.as_path()
    }

    /// Inspect safety pragmas on an independent read connection.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be opened or queried.
    pub fn pragmas(&self) -> StorageResult<SqlitePragmas> {
        let connection = self.open_connection()?;
        let journal_mode = connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
        let foreign_keys = connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
        let busy_timeout_millis: i64 =
            connection.query_row("PRAGMA busy_timeout", [], |row| row.get(0))?;
        let query_only = connection.query_row("PRAGMA query_only", [], |row| row.get(0))?;
        Ok(SqlitePragmas {
            journal_mode,
            foreign_keys,
            busy_timeout_millis: u64::try_from(busy_timeout_millis).map_err(|_| {
                StorageError::InvalidStoredValue {
                    field: "pragma.busy_timeout",
                }
            })?,
            query_only,
        })
    }

    /// Load one canonical model call and its selected provenance.
    ///
    /// # Errors
    ///
    /// Returns an error for database failure or invalid stored domain values.
    pub fn model_call(&self, id: ModelCallId) -> StorageResult<Option<StoredModelCall>> {
        self.read_snapshot(|connection| {
            let exists = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM model_calls WHERE id = ?1)",
                [id.to_string()],
                |row| row.get::<_, bool>(0),
            )?;
            exists
                .then(|| load_model_call(connection, id).map(StoredModelCall))
                .transpose()
        })
    }

    /// List recent canonical calls using a bounded stable cursor.
    ///
    /// # Errors
    ///
    /// Returns an error for database failure or invalid stored domain values.
    pub fn model_calls(&self, query: ModelCallQuery) -> StorageResult<Vec<StoredModelCall>> {
        self.read_snapshot(|connection| {
            let (before_event_at, before_id) = query.before().map_or((None, None), |cursor| {
                (
                    Some(timestamp_text(cursor.event_at())),
                    Some(cursor.id().to_string()),
                )
            });
            let limit = query_limit(query.limit())?;
            let ids = {
                let mut statement = connection.prepare(
                    "SELECT id FROM model_calls
                     WHERE (?1 IS NULL OR event_at < ?1 OR (event_at = ?1 AND id < ?2))
                     ORDER BY event_at DESC, id DESC
                     LIMIT ?3",
                )?;
                statement
                    .query_map(params![before_event_at, before_id, limit], |row| {
                        row.get::<_, String>(0)
                    })?
                    .collect::<Result<Vec<_>, _>>()?
            };
            ids.into_iter()
                .map(|id| {
                    let id = ModelCallId::from_str(&id).map_err(|_| {
                        StorageError::InvalidStoredValue {
                            field: "model_call.id",
                        }
                    })?;
                    load_model_call(connection, id).map(StoredModelCall)
                })
                .collect()
        })
    }

    /// Load one canonical conversation.
    ///
    /// # Errors
    ///
    /// Returns an error for database failure or invalid stored provenance.
    pub fn conversation(&self, id: ConversationId) -> StorageResult<Option<Conversation>> {
        self.read_snapshot(|connection| load_conversation(connection, id))
    }

    /// List recently observed conversations with a hard query cap.
    ///
    /// # Errors
    ///
    /// Returns an error when the limit exceeds the cap or stored rows are invalid.
    pub fn conversations(&self, limit: NonZeroUsize) -> StorageResult<Vec<Conversation>> {
        let limit = query_limit(limit)?;
        self.read_snapshot(|connection| {
            let ids = {
                let mut statement = connection.prepare(
                    "SELECT id FROM conversations
                     ORDER BY last_observed_at DESC, id DESC LIMIT ?1",
                )?;
                statement
                    .query_map([limit], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?
            };
            ids.into_iter()
                .map(|id| {
                    let id = ConversationId::from_str(&id).map_err(|_| {
                        StorageError::InvalidStoredValue {
                            field: "conversation.id",
                        }
                    })?;
                    load_conversation(connection, id)?.ok_or(StorageError::InvalidStoredValue {
                        field: "conversation.id",
                    })
                })
                .collect()
        })
    }

    /// Load every registered source-health row in stable source order.
    ///
    /// # Errors
    ///
    /// Returns an error for database failure or invalid stored capabilities.
    pub fn source_health(&self) -> StorageResult<Vec<StoredSourceHealth>> {
        self.read_snapshot(load_source_health)
    }

    /// Load recent alerts using a bounded limit.
    ///
    /// # Errors
    ///
    /// Returns an error when the limit exceeds the cap or a row is invalid.
    pub fn alerts(&self, limit: NonZeroUsize) -> StorageResult<Vec<StoredAlert>> {
        let limit = query_limit(limit)?;
        let connection = self.open_connection()?;
        let mut statement = connection.prepare(
            "SELECT id, model_call_id, rule_id, rule_version,
                    reasoning_output_tokens, pattern_n, event_at, source_quality, created_at
             FROM alerts ORDER BY event_at DESC, id DESC LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
            ))
        })?;
        rows.map(|row| {
            let (
                id,
                model_call_id,
                rule_id,
                rule_version,
                tokens,
                pattern_n,
                event_at,
                quality,
                created_at,
            ) = row?;
            Ok(StoredAlert {
                id: AlertId::from_str(&id)
                    .map_err(|_| StorageError::InvalidStoredValue { field: "alert.id" })?,
                model_call_id: ModelCallId::from_str(&model_call_id).map_err(|_| {
                    StorageError::InvalidStoredValue {
                        field: "model_call.id",
                    }
                })?,
                rule_id: BuiltInRuleId::from_str(&rule_id)
                    .map_err(|_| StorageError::InvalidStoredValue { field: "rule.id" })?,
                rule_version: RuleVersion::new(
                    NonZeroU16::new(u16::try_from(rule_version).map_err(|_| {
                        StorageError::InvalidStoredValue {
                            field: "rule.version",
                        }
                    })?)
                    .ok_or(StorageError::InvalidStoredValue {
                        field: "rule.version",
                    })?,
                ),
                reasoning_output_tokens: TokenCount::try_from(tokens)?,
                pattern_n: PatternN::new(
                    NonZeroU64::new(u64::try_from(pattern_n).map_err(|_| {
                        StorageError::InvalidStoredValue {
                            field: "alert.pattern_n",
                        }
                    })?)
                    .ok_or(StorageError::InvalidStoredValue {
                        field: "alert.pattern_n",
                    })?,
                ),
                event_at: parse_timestamp(&event_at, "model_call.event_at")?,
                source_quality: parse_evidence_quality(&quality)?,
                created_at: parse_timestamp(&created_at, "alert.created_at")?,
            })
        })
        .collect()
    }

    /// Load one durable action Outbox request.
    ///
    /// # Errors
    ///
    /// Returns an error for database failure or invalid stored action values.
    pub fn action(&self, id: ActionId) -> StorageResult<Option<StoredAction>> {
        self.read_snapshot(|connection| load_action(connection, id))
    }

    /// Load actions associated with one alert in stable creation order.
    ///
    /// # Errors
    ///
    /// Returns an error for database failure or invalid stored action values.
    pub fn actions_for_alert(&self, alert_id: AlertId) -> StorageResult<Vec<StoredAction>> {
        self.read_snapshot(|connection| {
            let ids = {
                let mut statement = connection.prepare(
                    "SELECT id FROM action_outbox
                     WHERE alert_id = ?1 ORDER BY created_at, id",
                )?;
                statement
                    .query_map([alert_id.to_string()], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?
            };
            ids.into_iter()
                .map(|id| {
                    let id =
                        ActionId::from_str(&id).map_err(|_| StorageError::InvalidStoredValue {
                            field: "action_outbox.id",
                        })?;
                    load_action(connection, id)?.ok_or(StorageError::InvalidStoredValue {
                        field: "action_outbox.id",
                    })
                })
                .collect()
        })
    }

    /// Load every execution attempt for one action in attempt order.
    ///
    /// # Errors
    ///
    /// Returns an error for database failure or invalid stored attempt values.
    pub fn action_attempts(&self, id: ActionId) -> StorageResult<Vec<StoredActionAttempt>> {
        self.read_snapshot(|connection| load_action_attempts(connection, id))
    }

    /// Load the singleton settings document and current revision.
    ///
    /// # Errors
    ///
    /// Returns an error when the row is missing, invalid, or cannot be queried.
    pub fn settings(&self) -> StorageResult<StoredSettingsDocument> {
        let connection = self.open_connection()?;
        let raw = connection.query_row(
            "SELECT settings.revision, settings.retention_days,
                    settings.notifications_enabled, settings.notify_max_n,
                    settings.notify_all, settings.locale,
                    settings.login_autostart, settings.updated_at,
                    locale_state.initialized
             FROM app_settings AS settings
             JOIN app_locale_initialization AS locale_state
               ON locale_state.singleton = settings.singleton
             WHERE settings.singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, bool>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, bool>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, bool>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, bool>(8)?,
                ))
            },
        )?;
        let (
            revision,
            retention_days,
            notifications_enabled,
            notify_max_n,
            notify_all,
            locale,
            login_autostart,
            updated_at,
            locale_initialized,
        ) = raw;
        let settings = AppSettings {
            retention: parse_retention(retention_days)?,
            notifications_enabled,
            notify_max_n: notify_max_n
                .map(|value| {
                    let value =
                        u32::try_from(value).map_err(|_| StorageError::InvalidStoredValue {
                            field: "app_settings.notify_max_n",
                        })?;
                    NonZeroU32::new(value).ok_or(StorageError::InvalidStoredValue {
                        field: "app_settings.notify_max_n",
                    })
                })
                .transpose()?,
            notify_all,
            locale: match locale.as_str() {
                "en-US" => AppLocale::EnUs,
                "zh-CN" => AppLocale::ZhCn,
                _ => {
                    return Err(StorageError::InvalidStoredValue {
                        field: "app_settings.locale",
                    });
                }
            },
            login_autostart,
            updated_at: parse_timestamp(&updated_at, "app_settings.updated_at")?,
        };
        settings.validate()?;
        Ok(StoredSettingsDocument {
            revision: u64::try_from(revision).map_err(|_| StorageError::InvalidStoredValue {
                field: "app_settings.revision",
            })?,
            settings,
            locale_initialized,
        })
    }

    pub(super) fn open_connection(&self) -> StorageResult<Connection> {
        let connection = Connection::open_with_flags(
            self.database_path.as_path(),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        configure_read_connection(&connection)?;
        Ok(connection)
    }

    pub(super) fn read_snapshot<T>(
        &self,
        operation: impl FnOnce(&Connection) -> StorageResult<T>,
    ) -> StorageResult<T> {
        let mut connection = self.open_connection()?;
        let transaction = connection.transaction()?;
        let result = operation(&transaction)?;
        transaction.commit()?;
        Ok(result)
    }
}

fn load_source_health(connection: &Connection) -> StorageResult<Vec<StoredSourceHealth>> {
    let raw = {
        let mut statement = connection.prepare(
            "SELECT source_id, source_contract_version, status, last_accepted_at,
                    last_error_code, last_error_occurred_at, updated_at
             FROM source_health ORDER BY source_id, source_contract_version",
        )?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    raw.into_iter()
        .map(|row| build_source_health(connection, row))
        .collect()
}

fn build_source_health(
    connection: &Connection,
    row: (
        String,
        i64,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
    ),
) -> StorageResult<StoredSourceHealth> {
    let (
        source_id,
        contract_version,
        status,
        last_accepted_at,
        last_error_code,
        last_error_occurred_at,
        updated_at,
    ) = row;
    let source_id = SourceId::new(source_id).map_err(|_| StorageError::InvalidStoredValue {
        field: "source_health.source_id",
    })?;
    let contract_version = parse_contract_version(contract_version)?;
    Ok(StoredSourceHealth {
        supported_fields: load_supported_fields(connection, &source_id, contract_version)?,
        source_id,
        source_contract_version: contract_version,
        status: parse_source_status(&status)?,
        last_accepted_at: last_accepted_at
            .map(|value| parse_timestamp(&value, "source_health.last_accepted_at"))
            .transpose()?,
        last_error_code: last_error_code
            .map(|value| {
                SafeErrorCode::from_str(&value).map_err(|_| StorageError::InvalidStoredValue {
                    field: "source_health.last_error_code",
                })
            })
            .transpose()?,
        last_error_occurred_at: last_error_occurred_at
            .map(|value| parse_timestamp(&value, "source_health.last_error_occurred_at"))
            .transpose()?,
        updated_at: parse_timestamp(&updated_at, "source_health.updated_at")?,
    })
}

fn load_conversation(
    connection: &Connection,
    id: ConversationId,
) -> StorageResult<Option<Conversation>> {
    let raw = connection
        .query_row(
            "SELECT source_id, source_provenance_source_id,
                    source_provenance_contract_version, source_provenance_observed_at,
                    source_quality, source_priority,
                    thread_id, thread_provenance_source_id,
                    thread_provenance_contract_version, thread_provenance_observed_at,
                    thread_quality, thread_priority,
                    first_observed_at, last_observed_at
             FROM conversations WHERE id = ?1",
            [id.to_string()],
            |row| {
                Ok((
                    (
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ),
                    (
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                        row.get(11)?,
                    ),
                    row.get::<_, String>(12)?,
                    row.get::<_, String>(13)?,
                ))
            },
        )
        .optional()?;
    raw.map(|(source, thread, first, last)| {
        let first = parse_timestamp(&first, "conversation.first_observed_at")?;
        let last = parse_timestamp(&last, "conversation.last_observed_at")?;
        let source =
            parse_ranked_text(source, "conversation.source_id", ConversationSourceId::new)?;
        let thread = parse_ranked_text(thread, "conversation.thread_id", ThreadSourceId::new)?;
        let mut conversation = Conversation::new(id, source, thread, first);
        conversation.observe_at(last);
        Ok(conversation)
    })
    .transpose()
}

fn parse_ranked_text<T: Ord>(
    raw: RankedTextRaw,
    field: &'static str,
    constructor: impl FnOnce(String) -> Result<T, codex_monitor_core::ValidationError>,
) -> StorageResult<Option<CanonicalField<T>>> {
    let (value, source_id, version, observed_at, quality, priority) = raw;
    if value.is_none()
        && source_id.is_none()
        && version.is_none()
        && observed_at.is_none()
        && quality.is_none()
        && priority.is_none()
    {
        return Ok(None);
    }
    let value = constructor(value.ok_or(StorageError::InvalidStoredValue { field })?)?;
    let source_id = SourceId::new(source_id.ok_or(StorageError::InvalidStoredValue { field })?)?;
    let version =
        parse_contract_version(version.ok_or(StorageError::InvalidStoredValue { field })?)?;
    let observed_at = parse_timestamp(
        &observed_at.ok_or(StorageError::InvalidStoredValue { field })?,
        field,
    )?;
    let quality =
        parse_evidence_quality(&quality.ok_or(StorageError::InvalidStoredValue { field })?)?;
    let priority = u16::try_from(priority.ok_or(StorageError::InvalidStoredValue { field })?)
        .map_err(|_| StorageError::InvalidStoredValue { field })?;
    Ok(Some(CanonicalField::new(Observed::new(
        value,
        FieldProvenance::new(
            SourceIdentity::new(source_id, version),
            observed_at,
            quality,
            SourcePriority::new(priority),
        ),
    ))))
}

fn load_supported_fields(
    connection: &Connection,
    source_id: &SourceId,
    contract_version: SourceContractVersion,
) -> StorageResult<Vec<FieldId>> {
    let mut statement = connection.prepare(
        "SELECT field_id FROM source_health_supported_fields
         WHERE source_id = ?1 AND source_contract_version = ?2 ORDER BY field_id",
    )?;
    statement
        .query_map(params![source_id.as_str(), contract_version.get()], |row| {
            row.get::<_, String>(0)
        })?
        .map(|row| {
            FieldId::new(row?).map_err(|_| StorageError::InvalidStoredValue {
                field: "source_health.supported_fields",
            })
        })
        .collect()
}

fn parse_contract_version(value: i64) -> StorageResult<SourceContractVersion> {
    u16::try_from(value)
        .ok()
        .and_then(|value| SourceContractVersion::new(value).ok())
        .ok_or(StorageError::InvalidStoredValue {
            field: "source_contract_version",
        })
}

fn parse_source_status(value: &str) -> StorageResult<SourceHealthStatus> {
    match value {
        "starting" => Ok(SourceHealthStatus::Starting),
        "healthy" => Ok(SourceHealthStatus::Healthy),
        "degraded" => Ok(SourceHealthStatus::Degraded),
        "unavailable" => Ok(SourceHealthStatus::Unavailable),
        "stopped" => Ok(SourceHealthStatus::Stopped),
        _ => Err(StorageError::InvalidStoredValue {
            field: "source_health.status",
        }),
    }
}

fn parse_retention(value: Option<i64>) -> StorageResult<RetentionPolicy> {
    match value {
        None => Ok(RetentionPolicy::Unlimited),
        Some(30) => Ok(RetentionPolicy::Days30),
        Some(90) => Ok(RetentionPolicy::Days90),
        Some(365) => Ok(RetentionPolicy::Days365),
        Some(_) => Err(StorageError::InvalidStoredValue {
            field: "app_settings.retention_days",
        }),
    }
}

fn validate_query_limit(limit: NonZeroUsize) -> StorageResult<()> {
    if limit.get() > MAX_MODEL_CALL_QUERY_LIMIT {
        return Err(StorageError::InvalidQueryLimit {
            max: MAX_MODEL_CALL_QUERY_LIMIT,
        });
    }
    Ok(())
}

fn query_limit(limit: NonZeroUsize) -> StorageResult<i64> {
    validate_query_limit(limit)?;
    i64::try_from(limit.get()).map_err(|_| StorageError::InvalidQueryLimit {
        max: MAX_MODEL_CALL_QUERY_LIMIT,
    })
}
