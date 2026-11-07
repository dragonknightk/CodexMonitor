use std::str::FromStr as _;

use codex_monitor_core::{
    ClientName, ConversationSourceId, DataOrigin, EventName, EvidenceQuality, FieldProvenance,
    IdentityAlgorithmVersion, IdentityKind, ModelCallId, ModelCallObservation,
    ModelCallObservationId, ModelCallRelationship, ModelName, ObservationFields, Observed,
    ReasoningEffort, SourceContractVersion, SourceEventIdentity, SourceId, SourceIdentity,
    SourcePriority, StreamEventKind, SurfaceName, ThreadSourceId, TokenCount, TurnSourceId,
    UtcTimestamp, ValidationError,
};
use rusqlite::{Connection, OptionalExtension as _, Row, Transaction, params};

use super::timestamp::timestamp_text;
use super::{StorageError, StorageResult};

pub(crate) const CATALOG_FIELD_IDS: &[&str] = &[
    "conversation.first_observed_at",
    "conversation.id",
    "conversation.last_observed_at",
    "conversation.resolved_display_name",
    "conversation.source_id",
    "conversation.thread_id",
    "model_call.cached_input_tokens",
    "model_call.canonical_identity",
    "model_call.client",
    "model_call.event_at",
    "model_call.event_name",
    "model_call.id",
    "model_call.input_tokens",
    "model_call.model",
    "model_call.output_tokens",
    "model_call.reasoning_effort",
    "model_call.reasoning_output_tokens",
    "model_call.stream_event_kind",
    "model_call.surface",
    "model_call.temporal_class",
    "model_call_observation.field_provenance",
    "model_call_observation.field_quality",
    "model_call_observation.id",
    "model_call_observation.observed_at",
    "model_call_observation.source_contract_version",
    "model_call_observation.source_event_identity",
    "model_call_observation.source_event_identity_version",
    "model_call_observation.source_id",
    "source_health.last_accepted_at",
    "source_health.last_error_code",
    "source_health.last_error_occurred_at",
    "source_health.source_contract_version",
    "source_health.source_id",
    "source_health.status",
    "source_health.supported_fields",
    "source_health.updated_at",
    "turn.event_at",
    "turn.id",
    "turn.source_id",
];

pub(crate) struct LoadedObservation {
    pub id: ModelCallObservationId,
    pub observation: ModelCallObservation,
}

pub(crate) fn identity_kind_text(kind: IdentityKind) -> &'static str {
    match kind {
        IdentityKind::Stable => "stable",
        IdentityKind::FallbackFingerprint => "fallback_fingerprint",
    }
}

pub(crate) fn data_origin_text(origin: DataOrigin) -> &'static str {
    match origin {
        DataOrigin::Realtime => "realtime",
        DataOrigin::Historical => "historical",
    }
}

pub(crate) fn evidence_quality_text(quality: EvidenceQuality) -> &'static str {
    match quality {
        EvidenceQuality::Inferred => "inferred",
        EvidenceQuality::Direct => "direct",
    }
}

pub(crate) fn parse_data_origin(value: &str) -> StorageResult<DataOrigin> {
    match value {
        "realtime" => Ok(DataOrigin::Realtime),
        "historical" => Ok(DataOrigin::Historical),
        _ => invalid("model_call.temporal_class"),
    }
}

pub(crate) fn parse_evidence_quality(value: &str) -> StorageResult<EvidenceQuality> {
    match value {
        "inferred" => Ok(EvidenceQuality::Inferred),
        "direct" => Ok(EvidenceQuality::Direct),
        _ => invalid("model_call_observation.field_quality"),
    }
}

pub(crate) fn parse_timestamp(value: &str, field: &'static str) -> StorageResult<UtcTimestamp> {
    UtcTimestamp::parse_rfc3339(value).map_err(|_| StorageError::InvalidStoredValue { field })
}

pub(crate) fn invalid<T>(field: &'static str) -> StorageResult<T> {
    Err(StorageError::InvalidStoredValue { field })
}

pub(crate) fn rebuild_identity(
    source_id: String,
    contract_version: i64,
    algorithm_version: i64,
    kind: &str,
    digest: Vec<u8>,
) -> StorageResult<SourceEventIdentity> {
    let source_id = SourceId::new(source_id)
        .map_err(|_| StorageError::InvalidStoredValue { field: "source_id" })?;
    let contract_version = u16::try_from(contract_version)
        .ok()
        .and_then(|value| SourceContractVersion::new(value).ok())
        .ok_or(StorageError::InvalidStoredValue {
            field: "source_contract_version",
        })?;
    let algorithm_version = match algorithm_version {
        1 => IdentityAlgorithmVersion::V1,
        _ => {
            return invalid("source_event_identity.algorithm_version");
        }
    };
    let kind = match kind {
        "stable" => IdentityKind::Stable,
        "fallback_fingerprint" => IdentityKind::FallbackFingerprint,
        _ => return invalid("source_event_identity.kind"),
    };
    let digest: [u8; 32] = digest
        .try_into()
        .map_err(|_| StorageError::InvalidStoredValue {
            field: "source_event_identity.digest",
        })?;
    SourceEventIdentity::from_stored_digest(
        SourceIdentity::new(source_id, contract_version),
        algorithm_version,
        kind,
        digest,
    )
    .map_err(Into::into)
}

pub(crate) fn find_observation_by_identity(
    connection: &Connection,
    identity: &SourceEventIdentity,
) -> StorageResult<Option<(ModelCallObservationId, ModelCallId)>> {
    let stored = connection
        .query_row(
            "SELECT id, model_call_id FROM model_call_observations
             WHERE source_id = ?1
               AND source_contract_version = ?2
               AND identity_version = ?3
               AND identity_kind = ?4
               AND identity_digest = ?5",
            params![
                identity.source().id().as_str(),
                identity.source().contract_version().get(),
                identity.algorithm_version().get(),
                identity_kind_text(identity.kind()),
                identity.digest().as_slice(),
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    stored
        .map(|(observation_id, model_call_id)| {
            Ok((
                ModelCallObservationId::from_str(&observation_id).map_err(|_| {
                    StorageError::InvalidStoredValue {
                        field: "model_call_observation.id",
                    }
                })?,
                ModelCallId::from_str(&model_call_id).map_err(|_| {
                    StorageError::InvalidStoredValue {
                        field: "model_call.id",
                    }
                })?,
            ))
        })
        .transpose()
}

pub(crate) fn find_model_call_by_identity(
    connection: &Connection,
    identity: &SourceEventIdentity,
) -> StorageResult<Option<ModelCallId>> {
    let stored = connection
        .query_row(
            "SELECT id FROM model_calls
             WHERE canonical_source_id = ?1
               AND canonical_source_contract_version = ?2
               AND canonical_identity_version = ?3
               AND canonical_identity_kind = ?4
               AND canonical_identity_digest = ?5",
            params![
                identity.source().id().as_str(),
                identity.source().contract_version().get(),
                identity.algorithm_version().get(),
                identity_kind_text(identity.kind()),
                identity.digest().as_slice(),
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    stored
        .map(|value| {
            ModelCallId::from_str(&value).map_err(|_| StorageError::InvalidStoredValue {
                field: "model_call.id",
            })
        })
        .transpose()
}

pub(crate) fn insert_observation(
    transaction: &Transaction<'_>,
    observation: &ModelCallObservation,
) -> StorageResult<()> {
    let identity = observation.source_event_identity();
    let event = observation.event_time();
    let event_provenance = event.provenance();
    transaction.execute(
        "INSERT INTO model_call_observations (
            id, model_call_id, source_id, source_contract_version,
            identity_version, identity_kind, identity_digest,
            event_at, event_provenance_observed_at, event_quality, event_priority,
            observed_at, origin
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            observation.id().to_string(),
            observation.model_call_id().to_string(),
            identity.source().id().as_str(),
            identity.source().contract_version().get(),
            identity.algorithm_version().get(),
            identity_kind_text(identity.kind()),
            identity.digest().as_slice(),
            timestamp_text(*event.value()),
            timestamp_text(event_provenance.observed_at()),
            evidence_quality_text(event_provenance.quality()),
            event_provenance.source_priority().get(),
            timestamp_text(observation.observed_at()),
            data_origin_text(observation.origin()),
        ],
    )?;

    if let Some(relationship) = observation.fields().relationship.as_ref() {
        insert_relationship(transaction, observation, relationship)?;
    }
    insert_observation_fields(transaction, observation)?;
    Ok(())
}

fn insert_relationship(
    transaction: &Transaction<'_>,
    observation: &ModelCallObservation,
    relationship: &Observed<ModelCallRelationship>,
) -> StorageResult<()> {
    let (kind, conversation_id, turn_id) = match relationship.value() {
        ModelCallRelationship::Conversation { conversation_id } => {
            ("conversation", *conversation_id, None)
        }
        ModelCallRelationship::Turn {
            conversation_id,
            turn_id,
        } => ("turn", *conversation_id, Some(*turn_id)),
    };
    transaction.execute(
        "INSERT INTO conversations (id, first_observed_at, last_observed_at)
         VALUES (?1, ?2, ?2)
         ON CONFLICT (id) DO UPDATE SET
             first_observed_at = min(first_observed_at, excluded.first_observed_at),
             last_observed_at = max(last_observed_at, excluded.last_observed_at)",
        params![
            conversation_id.to_string(),
            timestamp_text(observation.observed_at())
        ],
    )?;
    if let Some(turn_id) = turn_id {
        transaction.execute(
            "INSERT INTO turns (id, conversation_id) VALUES (?1, ?2)
             ON CONFLICT (id) DO NOTHING",
            params![turn_id.to_string(), conversation_id.to_string()],
        )?;
    }
    let provenance = relationship.provenance();
    transaction.execute(
        "INSERT INTO model_call_observation_relationships (
            observation_id, model_call_id, relationship_kind, conversation_id, turn_id,
            provenance_observed_at, quality, source_priority
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            observation.id().to_string(),
            observation.model_call_id().to_string(),
            kind,
            conversation_id.to_string(),
            turn_id.map(|value| value.to_string()),
            timestamp_text(provenance.observed_at()),
            evidence_quality_text(provenance.quality()),
            provenance.source_priority().get(),
        ],
    )?;
    Ok(())
}

fn insert_observation_fields(
    transaction: &Transaction<'_>,
    observation: &ModelCallObservation,
) -> StorageResult<()> {
    let fields = observation.fields();
    insert_optional_text(
        transaction,
        observation,
        "conversation.source_id",
        fields.conversation_source_id.as_ref(),
        ConversationSourceId::as_str,
    )?;
    insert_optional_text(
        transaction,
        observation,
        "conversation.thread_id",
        fields.thread_source_id.as_ref(),
        ThreadSourceId::as_str,
    )?;
    insert_optional_text(
        transaction,
        observation,
        "turn.source_id",
        fields.turn_source_id.as_ref(),
        TurnSourceId::as_str,
    )?;
    insert_optional_text(
        transaction,
        observation,
        "model_call.model",
        fields.model.as_ref(),
        ModelName::as_str,
    )?;
    insert_optional_text(
        transaction,
        observation,
        "model_call.reasoning_effort",
        fields.reasoning_effort.as_ref(),
        |value| value.as_str(),
    )?;
    insert_optional_token(
        transaction,
        observation,
        "model_call.input_tokens",
        fields.input_tokens.as_ref(),
    )?;
    insert_optional_token(
        transaction,
        observation,
        "model_call.cached_input_tokens",
        fields.cached_input_tokens.as_ref(),
    )?;
    insert_optional_token(
        transaction,
        observation,
        "model_call.output_tokens",
        fields.output_tokens.as_ref(),
    )?;
    insert_optional_token(
        transaction,
        observation,
        "model_call.reasoning_output_tokens",
        fields.reasoning_output_tokens.as_ref(),
    )?;
    insert_optional_text(
        transaction,
        observation,
        "model_call.surface",
        fields.surface.as_ref(),
        SurfaceName::as_str,
    )?;
    insert_optional_text(
        transaction,
        observation,
        "model_call.client",
        fields.client.as_ref(),
        ClientName::as_str,
    )?;
    insert_optional_text(
        transaction,
        observation,
        "model_call.event_name",
        fields.event_name.as_ref(),
        EventName::as_str,
    )?;
    insert_optional_text(
        transaction,
        observation,
        "model_call.stream_event_kind",
        fields.stream_event_kind.as_ref(),
        StreamEventKind::as_str,
    )
}

fn insert_optional_text<T>(
    transaction: &Transaction<'_>,
    observation: &ModelCallObservation,
    field_id: &'static str,
    value: Option<&Observed<T>>,
    text: impl FnOnce(&T) -> &str,
) -> StorageResult<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let provenance = value.provenance();
    transaction.execute(
        "INSERT INTO model_call_observation_fields (
            observation_id, field_id, text_value, integer_value,
            provenance_observed_at, quality, source_priority
        ) VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6)",
        params![
            observation.id().to_string(),
            field_id,
            text(value.value()),
            timestamp_text(provenance.observed_at()),
            evidence_quality_text(provenance.quality()),
            provenance.source_priority().get(),
        ],
    )?;
    Ok(())
}

fn insert_optional_token(
    transaction: &Transaction<'_>,
    observation: &ModelCallObservation,
    field_id: &'static str,
    value: Option<&Observed<TokenCount>>,
) -> StorageResult<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let provenance = value.provenance();
    let integer_value = i64::try_from(value.value().get()).expect("validated token count fits i64");
    transaction.execute(
        "INSERT INTO model_call_observation_fields (
            observation_id, field_id, text_value, integer_value,
            provenance_observed_at, quality, source_priority
        ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6)",
        params![
            observation.id().to_string(),
            field_id,
            integer_value,
            timestamp_text(provenance.observed_at()),
            evidence_quality_text(provenance.quality()),
            provenance.source_priority().get(),
        ],
    )?;
    Ok(())
}

pub(crate) fn load_observations(
    connection: &Connection,
    model_call_id: ModelCallId,
) -> StorageResult<Vec<LoadedObservation>> {
    let mut statement = connection.prepare(
        "SELECT id, source_id, source_contract_version, identity_version, identity_kind,
                identity_digest, event_at, event_provenance_observed_at, event_quality,
                event_priority, observed_at, origin
         FROM model_call_observations
         WHERE model_call_id = ?1
         ORDER BY observed_at, id",
    )?;
    let raw_rows = statement.query_map([model_call_id.to_string()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Vec<u8>>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, i64>(9)?,
            row.get::<_, String>(10)?,
            row.get::<_, String>(11)?,
        ))
    })?;

    let mut observations = Vec::new();
    for raw in raw_rows {
        let (
            id,
            source_id,
            contract_version,
            identity_version,
            identity_kind,
            identity_digest,
            event_at,
            event_provenance_observed_at,
            event_quality,
            event_priority,
            observed_at,
            origin,
        ) = raw?;
        let id = ModelCallObservationId::from_str(&id).map_err(|_| {
            StorageError::InvalidStoredValue {
                field: "model_call_observation.id",
            }
        })?;
        let identity = rebuild_identity(
            source_id,
            contract_version,
            identity_version,
            &identity_kind,
            identity_digest,
        )?;
        let event_at = parse_timestamp(&event_at, "model_call.event_at")?;
        let event_provenance = FieldProvenance::new(
            identity.source().clone(),
            parse_timestamp(
                &event_provenance_observed_at,
                "model_call_observation.field_provenance",
            )?,
            parse_evidence_quality(&event_quality)?,
            SourcePriority::new(parse_u16(event_priority, "source_priority")?),
        );
        let fields = load_observation_fields(connection, id, identity.source())?;
        let observation = ModelCallObservation::new(
            id,
            model_call_id,
            identity,
            Observed::new(event_at, event_provenance),
            parse_timestamp(&observed_at, "model_call_observation.observed_at")?,
            parse_data_origin(&origin)?,
            fields,
        )?;
        observations.push(LoadedObservation { id, observation });
    }
    Ok(observations)
}

fn load_observation_fields(
    connection: &Connection,
    observation_id: ModelCallObservationId,
    source: &SourceIdentity,
) -> StorageResult<ObservationFields> {
    let mut fields = ObservationFields {
        relationship: load_relationship(connection, observation_id, source)?,
        ..ObservationFields::default()
    };
    let mut statement = connection.prepare(
        "SELECT field_id, text_value, integer_value, provenance_observed_at, quality, source_priority
         FROM model_call_observation_fields
         WHERE observation_id = ?1
         ORDER BY field_id",
    )?;
    let rows = statement.query_map([observation_id.to_string()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, i64>(5)?,
        ))
    })?;
    for row in rows {
        let (field_id, text, integer, observed_at, quality, priority) = row?;
        let provenance = FieldProvenance::new(
            source.clone(),
            parse_timestamp(&observed_at, "model_call_observation.field_provenance")?,
            parse_evidence_quality(&quality)?,
            SourcePriority::new(parse_u16(priority, "source_priority")?),
        );
        assign_observation_field(&mut fields, &field_id, text, integer, provenance)?;
    }
    Ok(fields)
}

fn assign_observation_field(
    fields: &mut ObservationFields,
    field_id: &str,
    text: Option<String>,
    integer: Option<i64>,
    provenance: FieldProvenance,
) -> StorageResult<()> {
    macro_rules! set_text {
        ($target:expr, $field:literal, $constructor:expr) => {
            set_loaded_text($target, text, integer, provenance, $field, $constructor)
        };
    }
    macro_rules! set_token {
        ($target:expr, $field:literal) => {
            set_loaded_token($target, text.is_some(), integer, provenance, $field)
        };
    }
    match field_id {
        "conversation.source_id" => set_text!(
            &mut fields.conversation_source_id,
            "conversation.source_id",
            ConversationSourceId::new
        ),
        "conversation.thread_id" => set_text!(
            &mut fields.thread_source_id,
            "conversation.thread_id",
            ThreadSourceId::new
        ),
        "turn.source_id" => set_text!(
            &mut fields.turn_source_id,
            "turn.source_id",
            TurnSourceId::new
        ),
        "model_call.model" => set_text!(&mut fields.model, "model_call.model", ModelName::new),
        "model_call.reasoning_effort" => set_text!(
            &mut fields.reasoning_effort,
            "model_call.reasoning_effort",
            |value| ReasoningEffort::from_str(&value)
        ),
        "model_call.input_tokens" => {
            set_token!(&mut fields.input_tokens, "model_call.input_tokens")
        }
        "model_call.cached_input_tokens" => set_token!(
            &mut fields.cached_input_tokens,
            "model_call.cached_input_tokens"
        ),
        "model_call.output_tokens" => {
            set_token!(&mut fields.output_tokens, "model_call.output_tokens")
        }
        "model_call.reasoning_output_tokens" => {
            set_token!(
                &mut fields.reasoning_output_tokens,
                "model_call.reasoning_output_tokens"
            )
        }
        "model_call.surface" => {
            set_text!(&mut fields.surface, "model_call.surface", SurfaceName::new)
        }
        "model_call.client" => set_text!(&mut fields.client, "model_call.client", ClientName::new),
        "model_call.event_name" => set_text!(
            &mut fields.event_name,
            "model_call.event_name",
            EventName::new
        ),
        "model_call.stream_event_kind" => set_text!(
            &mut fields.stream_event_kind,
            "model_call.stream_event_kind",
            StreamEventKind::new
        ),
        _ => Err(StorageError::UnregisteredTelemetryField),
    }
}

fn set_loaded_text<T>(
    target: &mut Option<Observed<T>>,
    text: Option<String>,
    integer: Option<i64>,
    provenance: FieldProvenance,
    field: &'static str,
    constructor: impl FnOnce(String) -> Result<T, ValidationError>,
) -> StorageResult<()> {
    if integer.is_some() {
        return invalid(field);
    }
    let value = text.ok_or(StorageError::InvalidStoredValue { field })?;
    *target = Some(Observed::new(constructor(value)?, provenance));
    Ok(())
}

fn set_loaded_token(
    target: &mut Option<Observed<TokenCount>>,
    has_text: bool,
    integer: Option<i64>,
    provenance: FieldProvenance,
    field: &'static str,
) -> StorageResult<()> {
    if has_text {
        return invalid(field);
    }
    let value = integer.ok_or(StorageError::InvalidStoredValue { field })?;
    *target = Some(Observed::new(TokenCount::try_from(value)?, provenance));
    Ok(())
}

fn load_relationship(
    connection: &Connection,
    observation_id: ModelCallObservationId,
    source: &SourceIdentity,
) -> StorageResult<Option<Observed<ModelCallRelationship>>> {
    let raw = connection
        .query_row(
            "SELECT relationship_kind, conversation_id, turn_id,
                    provenance_observed_at, quality, source_priority
             FROM model_call_observation_relationships WHERE observation_id = ?1",
            [observation_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .optional()?;
    let Some((kind, conversation_id, turn_id, observed_at, quality, priority)) = raw else {
        return Ok(None);
    };
    let conversation_id =
        conversation_id
            .parse()
            .map_err(|_| StorageError::InvalidStoredValue {
                field: "conversation.id",
            })?;
    let turn_id = turn_id
        .map(|value| {
            value
                .parse()
                .map_err(|_| StorageError::InvalidStoredValue { field: "turn.id" })
        })
        .transpose()?;
    let relationship = ModelCallRelationship::from_parts(Some(conversation_id), turn_id)?.ok_or(
        StorageError::InvalidStoredValue {
            field: "model_call.relationship",
        },
    )?;
    if (kind == "conversation" && turn_id.is_some()) || (kind == "turn" && turn_id.is_none()) {
        return invalid("model_call.relationship");
    }
    if !matches!(kind.as_str(), "conversation" | "turn") {
        return invalid("model_call.relationship");
    }
    Ok(Some(Observed::new(
        relationship,
        FieldProvenance::new(
            source.clone(),
            parse_timestamp(&observed_at, "model_call_observation.field_provenance")?,
            parse_evidence_quality(&quality)?,
            SourcePriority::new(parse_u16(priority, "source_priority")?),
        ),
    )))
}

fn parse_u16(value: i64, field: &'static str) -> StorageResult<u16> {
    u16::try_from(value).map_err(|_| StorageError::InvalidStoredValue { field })
}

pub(crate) fn load_canonical_identity(
    row: &Row<'_>,
    start: usize,
) -> StorageResult<SourceEventIdentity> {
    let source_id = row.get(start)?;
    let contract_version = row.get(start + 1)?;
    let algorithm_version = row.get(start + 2)?;
    let kind: String = row.get(start + 3)?;
    let digest = row.get(start + 4)?;
    rebuild_identity(
        source_id,
        contract_version,
        algorithm_version,
        &kind,
        digest,
    )
}
