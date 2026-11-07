use std::fmt::Write as _;
use std::path::Path;
use std::str::FromStr as _;

use codex_monitor_core::{
    ClientName, ConversationSourceId, DataOrigin, EventName, EvidenceQuality, FieldProvenance,
    ModelCallId, ModelCallObservationId, ModelName, ObservationFields, Observed, ReasoningEffort,
    SourceContractVersion, SourceEventIdentity, SourceId, SourceIdentity,
    SourceObservationCandidate, SourceObservationError, SourceObservationKind, SourcePriority,
    StableSourceEventId, StreamEventKind, SurfaceName, ThreadSourceId, TokenCount, UtcTimestamp,
    ValidationError,
};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension as _, Row, Transaction, TransactionBehavior, params};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use crate::codec::find_observation_by_identity;

use super::{
    LegacySchemaDetection, LegacySchemaDetectionError, LegacySchemaKind, inspect_source_schema,
    open_source_connection,
};
use crate::writer::{
    commit_observation_transaction, resolve_conversation, resolve_legacy_conversation_range,
};
use crate::{ObservationCommitOutcome, ObservationWrite, StorageError, StorageResult};

const LEGACY_SOURCE_ID: &str = "legacy-import";
const LEGACY_SOURCE_CONTRACT_VERSION: u16 = 1;
// Historical metadata must never outrank direct live OTLP evidence (priority 100).
const LEGACY_SOURCE_PRIORITY: u16 = 50;
const MAX_LEGACY_ROWS: usize = 1_000_000;
const FINGERPRINT_DOMAIN: &[u8] = b"codex-monitor/legacy-import-source/v1";
const ROW_IDENTITY_DOMAIN: &[u8] = b"codex-monitor/legacy-import-row/v1";

const TOKEN_ROWS_SQL: &str = "
    SELECT id, created_at, conversation_id, thread_id, model,
           reasoning_effort, input_tokens, cached_input_tokens, output_tokens,
           reasoning_output_tokens, raw_event_name, stream_event_kind, surface, client
    FROM codex_token_events NOT INDEXED
    ORDER BY id
    LIMIT 1000001
";

const CONVERSATION_ROWS_SQL: &str = "
    SELECT conversation_id, first_seen_at, last_seen_at
    FROM codex_conversations INDEXED BY sqlite_autoindex_codex_conversations_1
    ORDER BY conversation_id
    LIMIT 1000001
";

/// Whether one explicit import inserted rows or matched its durable audit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LegacyImportStatus {
    Imported,
    AlreadyImported,
}

/// Privacy-safe result of one all-or-nothing legacy import.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LegacyImportOutcome {
    status: LegacyImportStatus,
    schema: LegacySchemaKind,
    imported_model_calls: u64,
    imported_conversations: u64,
}

impl LegacyImportOutcome {
    /// Return whether this call inserted data or matched an earlier audit.
    #[must_use]
    pub const fn status(self) -> LegacyImportStatus {
        self.status
    }

    /// Return the recognized source schema.
    #[must_use]
    pub const fn schema(self) -> LegacySchemaKind {
        self.schema
    }

    /// Return the number of imported completed-call rows.
    #[must_use]
    pub const fn imported_model_calls(self) -> u64 {
        self.imported_model_calls
    }

    /// Return the number of newly established or expanded conversation ranges.
    #[must_use]
    pub const fn imported_conversations(self) -> u64 {
        self.imported_conversations
    }
}

pub(crate) fn import_legacy_database(
    target: &mut Connection,
    source_path: &Path,
    imported_at: UtcTimestamp,
) -> StorageResult<LegacyImportOutcome> {
    let mut source = open_source_connection(source_path).map_err(map_detection_error)?;
    let source_transaction = source
        .transaction_with_behavior(TransactionBehavior::Deferred)
        .map_err(|_| StorageError::LegacySourceInvalid)?;
    let schema = match inspect_source_schema(&source_transaction).map_err(map_detection_error)? {
        LegacySchemaDetection::Legacy(schema) => schema,
        LegacySchemaDetection::CurrentDatabase | LegacySchemaDetection::Unsupported => {
            return Err(StorageError::LegacySchemaUnsupported);
        }
    };
    let source_identity = legacy_source_identity()?;
    let source_fingerprint = source_fingerprint(&source_transaction, schema)?;

    let target_transaction = target.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(outcome) = existing_audit(&target_transaction, source_fingerprint, schema)? {
        target_transaction.rollback()?;
        source_transaction
            .rollback()
            .map_err(|_| StorageError::LegacySourceInvalid)?;
        return Ok(outcome);
    }

    let imported_conversations = import_conversations(
        &source_transaction,
        &target_transaction,
        &source_identity,
        imported_at,
    )?;
    let imported_model_calls = import_model_calls(
        &source_transaction,
        &target_transaction,
        &source_identity,
        imported_at,
    )?;
    insert_audit(
        &target_transaction,
        source_fingerprint,
        schema,
        imported_model_calls,
        imported_conversations,
        imported_at,
    )?;

    source_transaction
        .rollback()
        .map_err(|_| StorageError::LegacySourceInvalid)?;
    target_transaction.commit()?;
    Ok(LegacyImportOutcome {
        status: LegacyImportStatus::Imported,
        schema,
        imported_model_calls,
        imported_conversations,
    })
}

fn map_detection_error(error: LegacySchemaDetectionError) -> StorageError {
    match error {
        LegacySchemaDetectionError::SourceUnavailable => StorageError::LegacySourceUnavailable,
        LegacySchemaDetectionError::InvalidDatabase => StorageError::LegacySourceInvalid,
        LegacySchemaDetectionError::SchemaTooComplex
        | LegacySchemaDetectionError::ExecutableSchema => StorageError::LegacySchemaUnsupported,
    }
}

fn legacy_source_identity() -> StorageResult<SourceIdentity> {
    Ok(SourceIdentity::new(
        SourceId::new(LEGACY_SOURCE_ID)?,
        SourceContractVersion::new(LEGACY_SOURCE_CONTRACT_VERSION)?,
    ))
}

fn source_fingerprint(source: &Connection, schema: LegacySchemaKind) -> StorageResult<[u8; 32]> {
    let mut encoder = FingerprintEncoder::new_source(schema);
    let token_count = for_each_token_row(source, |row| {
        encoder.token_row(&row);
        Ok(())
    })?;
    encoder.row_count(40, token_count);
    let conversation_count = for_each_conversation_row(source, |row| {
        encoder.conversation_row(&row);
        Ok(())
    })?;
    encoder.row_count(41, conversation_count);
    Ok(encoder.finish())
}

fn existing_audit(
    target: &Transaction<'_>,
    fingerprint: [u8; 32],
    schema: LegacySchemaKind,
) -> StorageResult<Option<LegacyImportOutcome>> {
    let stored = target
        .query_row(
            "SELECT detected_schema, imported_model_calls, imported_conversations
             FROM legacy_import_audit WHERE source_fingerprint = ?1",
            [fingerprint.as_slice()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    stored
        .map(|(stored_schema, model_calls, conversations)| {
            if stored_schema != schema.audit_code() {
                return Err(StorageError::LegacyImportAuditConflict);
            }
            Ok(LegacyImportOutcome {
                status: LegacyImportStatus::AlreadyImported,
                schema,
                imported_model_calls: u64::try_from(model_calls)
                    .map_err(|_| StorageError::LegacyImportAuditConflict)?,
                imported_conversations: u64::try_from(conversations)
                    .map_err(|_| StorageError::LegacyImportAuditConflict)?,
            })
        })
        .transpose()
}

fn insert_audit(
    target: &Transaction<'_>,
    fingerprint: [u8; 32],
    schema: LegacySchemaKind,
    imported_model_calls: u64,
    imported_conversations: u64,
    imported_at: UtcTimestamp,
) -> StorageResult<()> {
    target.execute(
        "INSERT INTO legacy_import_audit (
            id, source_fingerprint, detected_schema, imported_model_calls,
            imported_conversations, completed_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            Uuid::now_v7().to_string(),
            fingerprint.as_slice(),
            schema.audit_code(),
            sqlite_count(imported_model_calls),
            sqlite_count(imported_conversations),
            super::super::timestamp::timestamp_text(imported_at),
        ],
    )?;
    Ok(())
}

fn import_conversations(
    source: &Connection,
    target: &Transaction<'_>,
    source_identity: &SourceIdentity,
    imported_at: UtcTimestamp,
) -> StorageResult<u64> {
    let mut imported = 0_u64;
    for_each_conversation_row(source, |row| {
        if legacy_conversation_range_is_covered(target, source_identity, &row)? {
            return Ok(());
        }
        let candidate = conversation_candidate(
            &row,
            source_identity,
            conversation_row_identity(source_identity, &row)?,
            row.first_seen_at,
            imported_at,
        )?;
        resolve_legacy_conversation_range(target, &candidate, row.first_seen_at, row.last_seen_at)?;
        imported = imported
            .checked_add(1)
            .ok_or(StorageError::LegacyImportTooLarge {
                max: MAX_LEGACY_ROWS,
            })?;
        Ok(())
    })?;
    Ok(imported)
}

fn legacy_conversation_range_is_covered(
    target: &Transaction<'_>,
    source: &SourceIdentity,
    row: &LegacyConversationRow,
) -> StorageResult<bool> {
    target
        .query_row(
            "SELECT first_observed_at <= ?4 AND last_observed_at >= ?5
             FROM conversation_source_keys
             WHERE telemetry_source_id = ?1
               AND source_contract_version = ?2
               AND key_kind = 'conversation'
               AND key_value = ?3",
            params![
                source.id().as_str(),
                source.contract_version().get(),
                row.conversation_source_id.as_str(),
                super::super::timestamp::timestamp_text(row.first_seen_at),
                super::super::timestamp::timestamp_text(row.last_seen_at),
            ],
            |stored| stored.get::<_, bool>(0),
        )
        .optional()
        .map(Option::unwrap_or_default)
        .map_err(Into::into)
}

fn conversation_candidate(
    row: &LegacyConversationRow,
    source_identity: &SourceIdentity,
    identity: SourceEventIdentity,
    event_at: UtcTimestamp,
    imported_at: UtcTimestamp,
) -> StorageResult<SourceObservationCandidate> {
    let provenance = legacy_provenance(source_identity, imported_at, EvidenceQuality::Direct);
    SourceObservationCandidate::new(
        SourceObservationKind::ConversationMetadata,
        identity,
        Observed::new(event_at, provenance.clone()),
        imported_at,
        DataOrigin::Historical,
        ObservationFields {
            conversation_source_id: Some(Observed::new(
                row.conversation_source_id.clone(),
                provenance,
            )),
            ..ObservationFields::default()
        },
    )
    .map_err(|error| match error {
        SourceObservationError::InvalidDomain(error) => StorageError::Domain(error),
        SourceObservationError::NotCompletedModelCall => StorageError::LegacyImportAuditConflict,
    })
}

fn import_model_calls(
    source: &Connection,
    target: &Transaction<'_>,
    source_identity: &SourceIdentity,
    imported_at: UtcTimestamp,
) -> StorageResult<u64> {
    let mut imported = 0_u64;
    for_each_token_row(source, |row| {
        let canonical_identity = token_row_identity(source_identity, &row)?;
        if find_observation_by_identity(target, &canonical_identity)?.is_some() {
            return Ok(());
        }
        let candidate = token_candidate(
            row,
            source_identity,
            canonical_identity.clone(),
            imported_at,
        )?;
        let relationship = resolve_conversation(target, &candidate)?.map(|conversation_id| {
            codex_monitor_core::ModelCallRelationship::Conversation { conversation_id }
        });
        let observation = candidate
            .into_model_call_observation(
                ModelCallId::new(),
                ModelCallObservationId::new(),
                relationship,
            )
            .map_err(|error| match error {
                SourceObservationError::InvalidDomain(error) => StorageError::Domain(error),
                SourceObservationError::NotCompletedModelCall => {
                    StorageError::LegacyImportAuditConflict
                }
            })?;
        match commit_observation_transaction(
            target,
            ObservationWrite::new(canonical_identity, observation),
        )? {
            ObservationCommitOutcome::Inserted {
                downstream_work_enqueued,
                ..
            } => {
                if downstream_work_enqueued {
                    return Err(StorageError::LegacyHistoricalIsolationViolation);
                }
                imported = imported
                    .checked_add(1)
                    .ok_or(StorageError::LegacyImportTooLarge {
                        max: MAX_LEGACY_ROWS,
                    })?;
            }
            ObservationCommitOutcome::DuplicateSourceObservation { .. } => {}
        }
        Ok(())
    })?;
    Ok(imported)
}

fn token_candidate(
    row: LegacyTokenRow,
    source_identity: &SourceIdentity,
    identity: SourceEventIdentity,
    imported_at: UtcTimestamp,
) -> StorageResult<SourceObservationCandidate> {
    let direct = legacy_provenance(source_identity, imported_at, EvidenceQuality::Direct);
    let inferred = legacy_provenance(source_identity, imported_at, EvidenceQuality::Inferred);
    let fields = ObservationFields {
        conversation_source_id: observed(row.conversation_source_id, &direct),
        thread_source_id: observed(row.thread_source_id, &direct),
        model: observed(row.model, &inferred),
        reasoning_effort: observed(row.reasoning_effort, &inferred),
        input_tokens: observed(row.input_tokens, &direct),
        cached_input_tokens: observed(row.cached_input_tokens, &direct),
        output_tokens: observed(row.output_tokens, &direct),
        reasoning_output_tokens: observed(row.reasoning_output_tokens, &direct),
        surface: observed(row.surface, &inferred),
        client: observed(row.client, &inferred),
        event_name: observed(row.event_name, &direct),
        stream_event_kind: observed(row.stream_event_kind, &direct),
        ..ObservationFields::default()
    };
    SourceObservationCandidate::new(
        SourceObservationKind::CompletedModelCall,
        identity,
        Observed::new(row.event_at, direct),
        imported_at,
        DataOrigin::Historical,
        fields,
    )
    .map_err(|error| match error {
        SourceObservationError::InvalidDomain(error) => StorageError::Domain(error),
        SourceObservationError::NotCompletedModelCall => StorageError::LegacyImportAuditConflict,
    })
}

fn legacy_provenance(
    source: &SourceIdentity,
    imported_at: UtcTimestamp,
    quality: EvidenceQuality,
) -> FieldProvenance {
    FieldProvenance::new(
        source.clone(),
        imported_at,
        quality,
        SourcePriority::new(LEGACY_SOURCE_PRIORITY),
    )
}

fn observed<T>(value: Option<T>, provenance: &FieldProvenance) -> Option<Observed<T>> {
    value.map(|value| Observed::new(value, provenance.clone()))
}

fn token_row_identity(
    source: &SourceIdentity,
    row: &LegacyTokenRow,
) -> StorageResult<SourceEventIdentity> {
    let stable = StableSourceEventId::new(format!(
        "token:{}:{}",
        row.id,
        fingerprint_hex(row_fingerprint(|encoder| encoder.token_row(row)))
    ))?;
    Ok(SourceEventIdentity::from_stable_id(source.clone(), &stable))
}

fn conversation_row_identity(
    source: &SourceIdentity,
    row: &LegacyConversationRow,
) -> StorageResult<SourceEventIdentity> {
    let stable = StableSourceEventId::new(format!(
        "conversation:{}",
        fingerprint_hex(row_fingerprint(|encoder| encoder.conversation_row(row)))
    ))?;
    Ok(SourceEventIdentity::from_stable_id(source.clone(), &stable))
}

fn row_fingerprint(encode: impl FnOnce(&mut FingerprintEncoder)) -> [u8; 32] {
    let mut encoder = FingerprintEncoder::new_row();
    encode(&mut encoder);
    encoder.finish()
}

fn fingerprint_hex(fingerprint: [u8; 32]) -> String {
    let mut value = String::with_capacity(64);
    for byte in fingerprint {
        write!(&mut value, "{byte:02x}").expect("writing to String cannot fail");
    }
    value
}

fn for_each_token_row(
    source: &Connection,
    mut visit: impl FnMut(LegacyTokenRow) -> StorageResult<()>,
) -> StorageResult<u64> {
    let mut statement = source
        .prepare(TOKEN_ROWS_SQL)
        .map_err(source_database_error)?;
    let mut rows = statement.query([]).map_err(source_database_error)?;
    let mut scanned = 0_u64;
    let mut accepted = 0_u64;
    while let Some(row) = rows.next().map_err(source_database_error)? {
        scanned = scanned
            .checked_add(1)
            .ok_or(StorageError::LegacyImportTooLarge {
                max: MAX_LEGACY_ROWS,
            })?;
        if scanned > u64::try_from(MAX_LEGACY_ROWS).expect("legacy row limit fits u64") {
            return Err(StorageError::LegacyImportTooLarge {
                max: MAX_LEGACY_ROWS,
            });
        }
        if let Some(row) = LegacyTokenRow::read(row)? {
            visit(row)?;
            accepted = accepted
                .checked_add(1)
                .ok_or(StorageError::LegacyImportTooLarge {
                    max: MAX_LEGACY_ROWS,
                })?;
        }
    }
    Ok(accepted)
}

fn for_each_conversation_row(
    source: &Connection,
    mut visit: impl FnMut(LegacyConversationRow) -> StorageResult<()>,
) -> StorageResult<u64> {
    let mut statement = source
        .prepare(CONVERSATION_ROWS_SQL)
        .map_err(source_database_error)?;
    let mut rows = statement.query([]).map_err(source_database_error)?;
    let mut count = 0_u64;
    while let Some(row) = rows.next().map_err(source_database_error)? {
        count = count
            .checked_add(1)
            .ok_or(StorageError::LegacyImportTooLarge {
                max: MAX_LEGACY_ROWS,
            })?;
        if count > u64::try_from(MAX_LEGACY_ROWS).expect("legacy row limit fits u64") {
            return Err(StorageError::LegacyImportTooLarge {
                max: MAX_LEGACY_ROWS,
            });
        }
        visit(LegacyConversationRow::read(row)?)?;
    }
    Ok(count)
}

fn source_database_error(_error: rusqlite::Error) -> StorageError {
    StorageError::LegacySourceInvalid
}

struct LegacyTokenRow {
    id: i64,
    event_at: UtcTimestamp,
    conversation_source_id: Option<ConversationSourceId>,
    thread_source_id: Option<ThreadSourceId>,
    model: Option<ModelName>,
    reasoning_effort: Option<ReasoningEffort>,
    input_tokens: Option<TokenCount>,
    cached_input_tokens: Option<TokenCount>,
    output_tokens: Option<TokenCount>,
    reasoning_output_tokens: Option<TokenCount>,
    event_name: Option<EventName>,
    stream_event_kind: Option<StreamEventKind>,
    surface: Option<SurfaceName>,
    client: Option<ClientName>,
}

impl LegacyTokenRow {
    fn read(row: &Row<'_>) -> StorageResult<Option<Self>> {
        let event_at = row
            .get::<_, String>(1)
            .map_err(source_database_error)
            .and_then(|value| UtcTimestamp::parse_rfc3339(&value).map_err(StorageError::from))?;
        let input_tokens = optional_token(row, 6)?;
        let cached_input_tokens = optional_token(row, 7)?;
        let output_tokens = optional_token(row, 8)?;
        let reasoning_output_tokens = optional_token(row, 9)?;
        if input_tokens.is_none()
            && cached_input_tokens.is_none()
            && output_tokens.is_none()
            && reasoning_output_tokens.is_none()
        {
            return Ok(None);
        }
        Ok(Some(Self {
            id: row.get(0).map_err(source_database_error)?,
            event_at,
            conversation_source_id: optional_label(
                row.get(2).map_err(source_database_error)?,
                ConversationSourceId::new,
            ),
            thread_source_id: optional_label(
                row.get(3).map_err(source_database_error)?,
                ThreadSourceId::new,
            ),
            model: optional_label(row.get(4).map_err(source_database_error)?, ModelName::new),
            reasoning_effort: optional_reasoning_effort(
                row.get::<_, Option<String>>(5)
                    .map_err(source_database_error)?,
            ),
            input_tokens,
            cached_input_tokens,
            output_tokens,
            reasoning_output_tokens,
            event_name: optional_event_name(
                row.get::<_, Option<String>>(10)
                    .map_err(source_database_error)?,
            ),
            stream_event_kind: optional_label(
                row.get(11).map_err(source_database_error)?,
                StreamEventKind::new,
            ),
            surface: optional_label(
                row.get(12).map_err(source_database_error)?,
                SurfaceName::new,
            ),
            client: optional_label(row.get(13).map_err(source_database_error)?, ClientName::new),
        }))
    }
}

struct LegacyConversationRow {
    conversation_source_id: ConversationSourceId,
    first_seen_at: UtcTimestamp,
    last_seen_at: UtcTimestamp,
}

impl LegacyConversationRow {
    fn read(row: &Row<'_>) -> StorageResult<Self> {
        let conversation_source_id = ConversationSourceId::new(
            row.get::<_, String>(0)
                .map_err(source_database_error)?
                .trim(),
        )?;
        let first_seen_at =
            UtcTimestamp::parse_rfc3339(&row.get::<_, String>(1).map_err(source_database_error)?)?;
        let last_seen_at =
            UtcTimestamp::parse_rfc3339(&row.get::<_, String>(2).map_err(source_database_error)?)?;
        // The importer is all-or-nothing: an inverted source range is corruption,
        // not an instruction to guess or silently rewrite source history.
        if first_seen_at > last_seen_at {
            return Err(StorageError::LegacyConversationTimeRangeInvalid);
        }
        Ok(Self {
            conversation_source_id,
            first_seen_at,
            last_seen_at,
        })
    }
}

fn optional_label<T>(
    value: Option<String>,
    validate: impl FnOnce(String) -> Result<T, ValidationError>,
) -> Option<T> {
    value.and_then(|value| validate(value.trim().to_owned()).ok())
}

fn optional_reasoning_effort(value: Option<String>) -> Option<ReasoningEffort> {
    value.and_then(|value| {
        let normalized = value.trim().to_ascii_lowercase();
        let normalized = normalized.strip_suffix(" (config)").unwrap_or(&normalized);
        ReasoningEffort::from_str(normalized).ok()
    })
}

fn optional_event_name(value: Option<String>) -> Option<EventName> {
    value.and_then(|value| {
        let value = value.trim();
        value
            .starts_with("codex.")
            .then(|| EventName::new(value.to_owned()).ok())
            .flatten()
    })
}

fn optional_token(row: &Row<'_>, index: usize) -> StorageResult<Option<TokenCount>> {
    let value = row.get_ref(index).map_err(source_database_error)?;
    Ok(match value {
        ValueRef::Integer(value) => TokenCount::try_from(value).ok(),
        ValueRef::Null | ValueRef::Real(_) | ValueRef::Text(_) | ValueRef::Blob(_) => None,
    })
}

struct FingerprintEncoder(Sha256);

impl FingerprintEncoder {
    fn new_source(schema: LegacySchemaKind) -> Self {
        let mut encoder = Self(Sha256::new());
        encoder.bytes(0, FINGERPRINT_DOMAIN);
        encoder.text(1, schema.audit_code());
        encoder
    }

    fn new_row() -> Self {
        let mut encoder = Self(Sha256::new());
        encoder.bytes(0, ROW_IDENTITY_DOMAIN);
        encoder
    }

    fn token_row(&mut self, row: &LegacyTokenRow) {
        self.bytes(10, b"token");
        self.i64(11, row.id);
        self.i128(12, row.event_at.unix_timestamp_nanos());
        self.optional_text(
            13,
            row.conversation_source_id
                .as_ref()
                .map(ConversationSourceId::as_str),
        );
        self.optional_text(
            14,
            row.thread_source_id.as_ref().map(ThreadSourceId::as_str),
        );
        self.optional_text(15, row.model.as_ref().map(ModelName::as_str));
        self.optional_text(
            16,
            row.reasoning_effort.as_ref().map(|value| value.as_str()),
        );
        self.optional_u64(17, row.input_tokens.map(TokenCount::get));
        self.optional_u64(18, row.cached_input_tokens.map(TokenCount::get));
        self.optional_u64(19, row.output_tokens.map(TokenCount::get));
        self.optional_u64(20, row.reasoning_output_tokens.map(TokenCount::get));
        self.optional_text(21, row.event_name.as_ref().map(EventName::as_str));
        self.optional_text(
            22,
            row.stream_event_kind.as_ref().map(StreamEventKind::as_str),
        );
        self.optional_text(23, row.surface.as_ref().map(SurfaceName::as_str));
        self.optional_text(24, row.client.as_ref().map(ClientName::as_str));
    }

    fn conversation_row(&mut self, row: &LegacyConversationRow) {
        self.bytes(30, b"conversation");
        self.text(31, row.conversation_source_id.as_str());
        self.i128(32, row.first_seen_at.unix_timestamp_nanos());
        self.i128(33, row.last_seen_at.unix_timestamp_nanos());
    }

    fn row_count(&mut self, tag: u8, count: u64) {
        self.u64(tag, count);
    }

    fn field_header(&mut self, tag: u8, length: usize) {
        self.0.update([tag]);
        self.0.update(
            u64::try_from(length)
                .expect("fingerprint field length fits u64")
                .to_be_bytes(),
        );
    }

    fn bytes(&mut self, tag: u8, value: &[u8]) {
        self.field_header(tag, value.len());
        self.0.update(value);
    }

    fn text(&mut self, tag: u8, value: &str) {
        self.bytes(tag, value.as_bytes());
    }

    fn i64(&mut self, tag: u8, value: i64) {
        self.bytes(tag, &value.to_be_bytes());
    }

    fn u64(&mut self, tag: u8, value: u64) {
        self.bytes(tag, &value.to_be_bytes());
    }

    fn i128(&mut self, tag: u8, value: i128) {
        self.bytes(tag, &value.to_be_bytes());
    }

    fn optional_text(&mut self, tag: u8, value: Option<&str>) {
        match value {
            Some(value) => {
                self.bytes(tag, &[1]);
                self.text(tag, value);
            }
            None => self.bytes(tag, &[0]),
        }
    }

    fn optional_u64(&mut self, tag: u8, value: Option<u64>) {
        match value {
            Some(value) => {
                self.bytes(tag, &[1]);
                self.u64(tag, value);
            }
            None => self.bytes(tag, &[0]),
        }
    }

    fn finish(self) -> [u8; 32] {
        self.0.finalize().into()
    }
}

fn sqlite_count(value: u64) -> i64 {
    i64::try_from(value).expect("bounded legacy row count fits SQLite INTEGER")
}
