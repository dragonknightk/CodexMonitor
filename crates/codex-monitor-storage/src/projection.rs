use codex_monitor_core::{
    CanonicalField, DataOrigin, ModelCall, ModelCallId, ModelCallObservation,
    ModelCallObservationId, ModelCallRelationship, Observed, SourceEventIdentity,
};
use rusqlite::{Connection, Transaction, params};

use super::codec::{
    LoadedObservation, data_origin_text, evidence_quality_text, identity_kind_text,
    load_canonical_identity, load_observations, parse_data_origin,
};
use super::timestamp::timestamp_text;
use super::{StorageError, StorageResult};

pub(crate) fn insert_model_call(
    transaction: &Transaction<'_>,
    canonical_identity: &SourceEventIdentity,
    observation: &ModelCallObservation,
) -> StorageResult<()> {
    transaction.execute(
        "INSERT INTO model_calls (
            id, canonical_source_id, canonical_source_contract_version,
            canonical_identity_version, canonical_identity_kind, canonical_identity_digest,
            event_at, event_time_observation_id, origin
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            observation.model_call_id().to_string(),
            canonical_identity.source().id().as_str(),
            canonical_identity.source().contract_version().get(),
            canonical_identity.algorithm_version().get(),
            identity_kind_text(canonical_identity.kind()),
            canonical_identity.digest().as_slice(),
            timestamp_text(*observation.event_time().value()),
            observation.id().to_string(),
            data_origin_text(observation.origin()),
        ],
    )?;
    Ok(())
}

pub(crate) fn load_model_call(
    connection: &Connection,
    model_call_id: ModelCallId,
) -> StorageResult<ModelCall> {
    let (identity, stored_origin) = connection.query_row(
        "SELECT canonical_source_id, canonical_source_contract_version,
                canonical_identity_version, canonical_identity_kind,
                canonical_identity_digest, origin
         FROM model_calls WHERE id = ?1",
        [model_call_id.to_string()],
        |row| {
            let identity = load_canonical_identity(row, 0).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            Ok((identity, row.get::<_, String>(5)?))
        },
    )?;
    let observations = load_observations(connection, model_call_id)?;
    build_model_call(model_call_id, identity, &stored_origin, &observations)
}

fn build_model_call(
    model_call_id: ModelCallId,
    canonical_identity: SourceEventIdentity,
    stored_origin: &str,
    observations: &[LoadedObservation],
) -> StorageResult<ModelCall> {
    let first = observations
        .first()
        .ok_or(StorageError::InvalidStoredValue {
            field: "model_call.observations",
        })?;
    // Origin is fixed when the canonical call first enters storage. In
    // particular, a historical import stays historical if a later source
    // observation correlates with it.
    let origin = parse_data_origin(stored_origin)?;
    if origin == DataOrigin::Realtime
        && !observations
            .iter()
            .any(|row| row.observation.origin() == DataOrigin::Realtime)
    {
        return Err(StorageError::InvalidStoredValue {
            field: "model_call.temporal_class",
        });
    }
    let mut call = ModelCall::new(
        model_call_id,
        canonical_identity,
        CanonicalField::new(first.observation.event_time().clone()),
        first.observation.observed_at(),
        origin,
        codex_monitor_core::CanonicalModelCallFields::default(),
    );
    for row in observations {
        call.apply_observation(&row.observation)?;
    }
    Ok(call)
}

pub(crate) fn project_model_call(
    transaction: &Transaction<'_>,
    model_call_id: ModelCallId,
    candidate_observation_id: ModelCallObservationId,
) -> StorageResult<bool> {
    let (canonical_identity, stored_origin) = transaction.query_row(
        "SELECT canonical_source_id, canonical_source_contract_version,
                canonical_identity_version, canonical_identity_kind,
                canonical_identity_digest, origin
         FROM model_calls WHERE id = ?1",
        [model_call_id.to_string()],
        |row| {
            let identity = load_canonical_identity(row, 0).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            Ok((identity, row.get::<_, String>(5)?))
        },
    )?;
    let observations = load_observations(transaction, model_call_id)?;
    let call = build_model_call(
        model_call_id,
        canonical_identity,
        &stored_origin,
        &observations,
    )?;

    let event_selected = call.event_time().clone().into_selected();
    let event_observation_id = selected_event_observation(&observations, &event_selected)?;
    let relationship = call
        .fields()
        .relationship
        .as_ref()
        .map(|field| field.clone().into_selected());
    let relationship_observation_id = relationship
        .as_ref()
        .map(|selected| selected_relationship_observation(&observations, selected))
        .transpose()?;

    let (relationship_kind, conversation_id, turn_id) =
        match relationship.as_ref().map(Observed::value) {
            None => (None, None, None),
            Some(ModelCallRelationship::Conversation { conversation_id }) => (
                Some("conversation"),
                Some(conversation_id.to_string()),
                None,
            ),
            Some(ModelCallRelationship::Turn {
                conversation_id,
                turn_id,
            }) => (
                Some("turn"),
                Some(conversation_id.to_string()),
                Some(turn_id.to_string()),
            ),
        };

    transaction.execute(
        "UPDATE model_calls
         SET event_at = ?2,
             event_time_observation_id = ?3,
             relationship_kind = ?4,
             conversation_id = ?5,
             turn_id = ?6,
             relationship_observation_id = ?7
         WHERE id = ?1",
        params![
            model_call_id.to_string(),
            timestamp_text(*call.event_time().value()),
            event_observation_id.to_string(),
            relationship_kind,
            conversation_id,
            turn_id,
            relationship_observation_id.map(|value| value.to_string()),
        ],
    )?;

    let selected_projection_field = persist_projection_fields(
        transaction,
        model_call_id,
        candidate_observation_id,
        &call,
        &observations,
    )?;
    let selected_candidate = event_observation_id == candidate_observation_id
        || relationship_observation_id == Some(candidate_observation_id)
        || selected_projection_field;

    upsert_dashboard_projection(transaction, &call)?;
    update_related_entities(transaction, &call)?;
    Ok(selected_candidate)
}

fn upsert_dashboard_projection(
    transaction: &Transaction<'_>,
    call: &ModelCall,
) -> StorageResult<()> {
    let (conversation_id, turn_id) = match call
        .fields()
        .relationship
        .as_ref()
        .map(CanonicalField::value)
    {
        None => (None, None),
        Some(relationship) => (
            Some(relationship.conversation_id().to_string()),
            relationship.turn_id().map(|value| value.to_string()),
        ),
    };
    let fields = call.fields();
    let input_tokens = fields
        .input_tokens
        .as_ref()
        .map(|field| sqlite_token(*field.value()));
    let output_tokens = fields
        .output_tokens
        .as_ref()
        .map(|field| sqlite_token(*field.value()));
    transaction.execute(
        "INSERT INTO model_call_dashboard_projection (
            model_call_id, event_at, conversation_id, turn_id, temporal_class,
            conversation_source_id, thread_source_id, model, reasoning_effort,
            input_tokens, cached_input_tokens, output_tokens,
            reasoning_output_tokens, client, surface
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
            ?9, ?10, ?11, ?12, ?13, ?14, ?15
         )
         ON CONFLICT (model_call_id) DO UPDATE SET
            event_at = excluded.event_at,
            conversation_id = excluded.conversation_id,
            turn_id = excluded.turn_id,
            temporal_class = excluded.temporal_class,
            conversation_source_id = excluded.conversation_source_id,
            thread_source_id = excluded.thread_source_id,
            model = excluded.model,
            reasoning_effort = excluded.reasoning_effort,
            input_tokens = excluded.input_tokens,
            cached_input_tokens = excluded.cached_input_tokens,
            output_tokens = excluded.output_tokens,
            reasoning_output_tokens = excluded.reasoning_output_tokens,
            client = excluded.client,
            surface = excluded.surface",
        params![
            call.id().to_string(),
            timestamp_text(*call.event_time().value()),
            conversation_id,
            turn_id,
            data_origin_text(call.origin()),
            fields
                .conversation_source_id
                .as_ref()
                .map(|field| field.value().as_str()),
            fields
                .thread_source_id
                .as_ref()
                .map(|field| field.value().as_str()),
            fields.model.as_ref().map(|field| field.value().as_str()),
            fields
                .reasoning_effort
                .as_ref()
                .map(|field| field.value().as_str()),
            input_tokens,
            fields
                .cached_input_tokens
                .as_ref()
                .map(|field| sqlite_token(*field.value())),
            output_tokens,
            fields
                .reasoning_output_tokens
                .as_ref()
                .map(|field| sqlite_token(*field.value())),
            fields.client.as_ref().map(|field| field.value().as_str()),
            fields.surface.as_ref().map(|field| field.value().as_str()),
        ],
    )?;
    Ok(())
}

fn sqlite_token(value: codex_monitor_core::TokenCount) -> i64 {
    i64::try_from(value.get()).expect("validated token count fits SQLite INTEGER")
}

fn persist_projection_fields(
    transaction: &Transaction<'_>,
    model_call_id: ModelCallId,
    candidate_observation_id: ModelCallObservationId,
    call: &ModelCall,
    observations: &[LoadedObservation],
) -> StorageResult<bool> {
    transaction.execute(
        "DELETE FROM model_call_projection_fields WHERE model_call_id = ?1",
        [model_call_id.to_string()],
    )?;
    let mut selected_candidate = false;

    macro_rules! persist_projection {
        ($field:ident, $field_id:literal) => {
            if let Some(canonical) = call.fields().$field.as_ref() {
                let selected = canonical.clone().into_selected();
                let observation_id = selected_field_observation(
                    &observations,
                    &selected,
                    |fields| fields.$field.as_ref(),
                    $field_id,
                )?;
                transaction.execute(
                    "INSERT INTO model_call_projection_fields (model_call_id, field_id, observation_id)
                     VALUES (?1, ?2, ?3)",
                    params![
                        model_call_id.to_string(),
                        $field_id,
                        observation_id.to_string()
                    ],
                )?;
                selected_candidate |= observation_id == candidate_observation_id;
            }
        };
    }

    persist_projection!(conversation_source_id, "conversation.source_id");
    persist_projection!(thread_source_id, "conversation.thread_id");
    persist_projection!(turn_source_id, "turn.source_id");
    persist_projection!(model, "model_call.model");
    persist_projection!(reasoning_effort, "model_call.reasoning_effort");
    persist_projection!(input_tokens, "model_call.input_tokens");
    persist_projection!(cached_input_tokens, "model_call.cached_input_tokens");
    persist_projection!(output_tokens, "model_call.output_tokens");
    persist_projection!(
        reasoning_output_tokens,
        "model_call.reasoning_output_tokens"
    );
    persist_projection!(surface, "model_call.surface");
    persist_projection!(client, "model_call.client");
    persist_projection!(event_name, "model_call.event_name");
    persist_projection!(stream_event_kind, "model_call.stream_event_kind");
    Ok(selected_candidate)
}

fn selected_event_observation(
    observations: &[LoadedObservation],
    selected: &Observed<codex_monitor_core::UtcTimestamp>,
) -> StorageResult<ModelCallObservationId> {
    observations
        .iter()
        .find(|row| row.observation.event_time() == selected)
        .map(|row| row.id)
        .ok_or(StorageError::InvalidStoredValue {
            field: "model_call.event_at",
        })
}

fn selected_relationship_observation(
    observations: &[LoadedObservation],
    selected: &Observed<ModelCallRelationship>,
) -> StorageResult<ModelCallObservationId> {
    observations
        .iter()
        .find(|row| row.observation.fields().relationship.as_ref() == Some(selected))
        .map(|row| row.id)
        .ok_or(StorageError::InvalidStoredValue {
            field: "model_call.relationship",
        })
}

fn selected_field_observation<T: Eq>(
    observations: &[LoadedObservation],
    selected: &Observed<T>,
    field: impl for<'a> Fn(&'a codex_monitor_core::ObservationFields) -> Option<&'a Observed<T>>,
    field_id: &'static str,
) -> StorageResult<ModelCallObservationId> {
    observations
        .iter()
        .find(|row| field(row.observation.fields()) == Some(selected))
        .map(|row| row.id)
        .ok_or(StorageError::InvalidStoredValue { field: field_id })
}

fn update_related_entities(transaction: &Transaction<'_>, call: &ModelCall) -> StorageResult<()> {
    let Some(relationship) = call
        .fields()
        .relationship
        .as_ref()
        .map(CanonicalField::value)
    else {
        return Ok(());
    };
    let conversation_id = relationship.conversation_id();
    if let Some(source_id) = call.fields().conversation_source_id.as_ref() {
        update_ranked_text(
            transaction,
            "conversations",
            "source",
            &conversation_id.to_string(),
            source_id.value().as_str(),
            source_id.provenance(),
        )?;
    }
    if let Some(thread_id) = call.fields().thread_source_id.as_ref() {
        update_ranked_text(
            transaction,
            "conversations",
            "thread",
            &conversation_id.to_string(),
            thread_id.value().as_str(),
            thread_id.provenance(),
        )?;
    }
    if let (Some(turn_id), Some(source_id)) = (
        relationship.turn_id(),
        call.fields().turn_source_id.as_ref(),
    ) {
        update_ranked_text(
            transaction,
            "turns",
            "source",
            &turn_id.to_string(),
            source_id.value().as_str(),
            source_id.provenance(),
        )?;
    }
    Ok(())
}

pub(crate) fn update_ranked_text(
    transaction: &Transaction<'_>,
    table: &'static str,
    prefix: &'static str,
    id: &str,
    value: &str,
    provenance: &codex_monitor_core::FieldProvenance,
) -> StorageResult<()> {
    let quality_rank = match provenance.quality() {
        codex_monitor_core::EvidenceQuality::Inferred => 0,
        codex_monitor_core::EvidenceQuality::Direct => 1,
    };
    let sql = match (table, prefix) {
        ("conversations", "source") => {
            "UPDATE conversations SET
                source_id = ?2,
                source_provenance_source_id = ?3,
                source_provenance_contract_version = ?4,
                source_provenance_observed_at = ?5,
                source_quality = ?6,
                source_priority = ?7
             WHERE id = ?1 AND (
                source_id IS NULL OR
                (CASE source_quality WHEN 'direct' THEN 1 ELSE 0 END,
                 source_priority, source_provenance_source_id,
                 source_provenance_contract_version, source_id)
                < (?8, ?7, ?3, ?4, ?2)
             )"
        }
        ("conversations", "thread") => {
            "UPDATE conversations SET
                thread_id = ?2,
                thread_provenance_source_id = ?3,
                thread_provenance_contract_version = ?4,
                thread_provenance_observed_at = ?5,
                thread_quality = ?6,
                thread_priority = ?7
             WHERE id = ?1 AND (
                thread_id IS NULL OR
                (CASE thread_quality WHEN 'direct' THEN 1 ELSE 0 END,
                 thread_priority, thread_provenance_source_id,
                 thread_provenance_contract_version, thread_id)
                < (?8, ?7, ?3, ?4, ?2)
             )"
        }
        ("turns", "source") => {
            "UPDATE turns SET
                source_id = ?2,
                source_provenance_source_id = ?3,
                source_provenance_contract_version = ?4,
                source_provenance_observed_at = ?5,
                source_quality = ?6,
                source_priority = ?7
             WHERE id = ?1 AND (
                source_id IS NULL OR
                (CASE source_quality WHEN 'direct' THEN 1 ELSE 0 END,
                 source_priority, source_provenance_source_id,
                 source_provenance_contract_version, source_id)
                < (?8, ?7, ?3, ?4, ?2)
             )"
        }
        _ => unreachable!("closed projection target"),
    };
    transaction.execute(
        sql,
        params![
            id,
            value,
            provenance.source().id().as_str(),
            provenance.source().contract_version().get(),
            timestamp_text(provenance.observed_at()),
            evidence_quality_text(provenance.quality()),
            provenance.source_priority().get(),
            quality_rank,
        ],
    )?;
    Ok(())
}
