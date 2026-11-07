use std::collections::HashSet;
use std::num::NonZeroU64;
use std::str::FromStr as _;

use codex_monitor_core::{
    AlertId, BuiltInRuleDefinition, BuiltInRuleId, BuiltInRuleRegistry, DataOrigin, ModelCall,
    NotificationThreshold, PatternMatchEvidence, RuleErrorCode, RuleEvaluation,
    RuleEvaluationOutcome, RuleNotificationPolicy, UtcTimestamp,
};
use rusqlite::{Connection, OptionalExtension as _, Transaction, TransactionBehavior, params};
use uuid::Uuid;

use super::action_outbox::persist_notification_action;
use super::codec::evidence_quality_text;
use super::projection::load_model_call;
use super::timestamp::timestamp_text;
use super::{RuleAlertDecision, RuleWorkClaim, RuleWorkCompletion, StorageError, StorageResult};

const MODEL_CALL_COMPLETED: &str = "model_call.completed";

pub(super) fn register_builtin_rules(
    connection: &mut Connection,
    definitions: &[BuiltInRuleDefinition],
) -> StorageResult<()> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let default_threshold = transaction.query_row(
        "SELECT notify_max_n, notify_all FROM app_settings WHERE singleton = 1",
        [],
        |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, bool>(1)?)),
    )?;
    notification_threshold(
        default_threshold.0,
        default_threshold.1,
        "app_settings.notify_threshold",
    )?;
    let mut keys = HashSet::new();
    for definition in definitions {
        if !keys.insert((definition.id, definition.version)) {
            return Err(StorageError::RuleDefinitionConflict);
        }
        let rule_kind = rule_kind(*definition);
        let inherited_settings = transaction
            .query_row(
                "SELECT enabled, notify_max_n, notify_all
                 FROM rules WHERE rule_id = ?1
                 ORDER BY rule_version DESC LIMIT 1",
                [definition.id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, bool>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, bool>(2)?,
                    ))
                },
            )
            .optional()?
            .unwrap_or((true, default_threshold.0, default_threshold.1));
        notification_threshold(
            inherited_settings.1,
            inherited_settings.2,
            "rules.notify_threshold",
        )?;
        let inserted_rule = transaction.execute(
            "INSERT INTO rules (
                rule_id, rule_version, rule_kind, enabled, notify_max_n, notify_all
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (rule_id, rule_version) DO NOTHING",
            params![
                definition.id.as_str(),
                definition.version.get(),
                rule_kind,
                inherited_settings.0,
                inherited_settings.1,
                inherited_settings.2,
            ],
        )?;
        let stored_kind = transaction.query_row(
            "SELECT rule_kind FROM rules WHERE rule_id = ?1 AND rule_version = ?2",
            params![definition.id.as_str(), definition.version.get()],
            |row| row.get::<_, String>(0),
        )?;
        if stored_kind != rule_kind {
            return Err(StorageError::RuleDefinitionConflict);
        }

        let semantic_fingerprint = definition.semantic_fingerprint();
        let stored_fingerprint = transaction
            .query_row(
                "SELECT semantic_sha256 FROM rule_definition_fingerprints
                 WHERE rule_id = ?1 AND rule_version = ?2",
                params![definition.id.as_str(), definition.version.get()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?;
        let stored_fingerprint = match stored_fingerprint {
            Some(stored_fingerprint) => stored_fingerprint,
            None if inserted_rule == 1 => {
                transaction.execute(
                    "INSERT INTO rule_definition_fingerprints (
                        rule_id, rule_version, semantic_sha256
                     ) VALUES (?1, ?2, ?3)",
                    params![
                        definition.id.as_str(),
                        definition.version.get(),
                        semantic_fingerprint.as_slice(),
                    ],
                )?;
                semantic_fingerprint.to_vec()
            }
            None => return Err(StorageError::RuleDefinitionConflict),
        };
        if stored_fingerprint.as_slice() != semantic_fingerprint {
            return Err(StorageError::RuleDefinitionConflict);
        }
    }
    transaction.commit()?;
    Ok(())
}

pub(super) fn claim_rule_work(
    connection: &mut Connection,
    claimed_at: UtcTimestamp,
    stale_before: UtcTimestamp,
) -> StorageResult<Option<RuleWorkClaim>> {
    if stale_before >= claimed_at {
        return Err(StorageError::InvalidRuleWorkClaimWindow);
    }
    let claimed_at_text = timestamp_text(claimed_at);
    let stale_before_text = timestamp_text(stale_before);
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let candidate = transaction
        .query_row(
            "SELECT work.id, work.model_call_id, work.state, work.started_at
             FROM domain_work AS work
             INNER JOIN model_calls AS model_call ON model_call.id = work.model_call_id
             WHERE work.work_kind = ?1
               AND model_call.origin = 'realtime'
               AND (
                    work.state = 'pending'
                    OR (work.state = 'started' AND work.started_at <= ?2)
               )
             ORDER BY work.created_at, work.id
             LIMIT 1",
            params![MODEL_CALL_COMPLETED, stale_before_text],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((work_id, model_call_id, state, previous_started_at)) = candidate else {
        transaction.commit()?;
        return Ok(None);
    };

    let changed = match (state.as_str(), previous_started_at.as_deref()) {
        ("pending", None) => transaction.execute(
            "UPDATE domain_work
             SET state = 'started', started_at = ?1
             WHERE id = ?2 AND work_kind = ?3
               AND state = 'pending' AND started_at IS NULL AND completed_at IS NULL",
            params![claimed_at_text, work_id, MODEL_CALL_COMPLETED],
        )?,
        ("started", Some(previous_started_at)) => transaction.execute(
            "UPDATE domain_work
             SET started_at = ?1
             WHERE id = ?2 AND work_kind = ?3
               AND state = 'started' AND started_at = ?4 AND completed_at IS NULL",
            params![
                claimed_at_text,
                work_id,
                MODEL_CALL_COMPLETED,
                previous_started_at,
            ],
        )?,
        _ => {
            return Err(StorageError::InvalidStoredValue {
                field: "domain_work.state",
            });
        }
    };
    if changed != 1 {
        return Err(StorageError::RuleWorkClaimLost);
    }

    let work_id = Uuid::from_str(&work_id).map_err(|_| StorageError::InvalidStoredValue {
        field: "domain_work.id",
    })?;
    let model_call_id = model_call_id
        .parse()
        .map_err(|_| StorageError::InvalidStoredValue {
            field: "model_call.id",
        })?;
    transaction.commit()?;
    Ok(Some(RuleWorkClaim::new(work_id, model_call_id, claimed_at)))
}

pub(super) fn complete_rule_work(
    connection: &mut Connection,
    claim: RuleWorkClaim,
    model_call: &ModelCall,
    evaluations: &[RuleEvaluation],
    completed_at: UtcTimestamp,
) -> StorageResult<RuleWorkCompletion> {
    validate_evaluation_batch(claim, model_call, evaluations)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let completed_replay = validate_claim(&transaction, claim)?;
    if !completed_replay && load_model_call(&transaction, claim.model_call_id())? != *model_call {
        return Err(StorageError::RuleEvaluationBatchInvalid);
    }
    let mut outcome = RuleWorkCompletion::default();
    for evaluation in evaluations {
        let row = evaluation_row(model_call, evaluation);
        let candidate_evaluation_id = Uuid::now_v7().to_string();
        let inserted = transaction.execute(
            "INSERT INTO rule_evaluations (
                id, rule_id, rule_version, model_call_id, matched,
                reasoning_output_tokens, pattern_n, event_at, source_quality, evaluated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT (rule_id, rule_version, model_call_id) DO NOTHING",
            params![
                candidate_evaluation_id,
                evaluation.key.rule_id.as_str(),
                evaluation.key.rule_version.get(),
                evaluation.key.model_call_id.to_string(),
                row.matched,
                row.reasoning_output_tokens,
                row.pattern_n,
                row.event_at,
                row.source_quality,
                timestamp_text(completed_at),
            ],
        )?;
        let evaluation_id = if inserted == 1 {
            outcome.inserted_evaluations += 1;
            candidate_evaluation_id
        } else {
            let evaluation_id = verify_existing_evaluation(&transaction, evaluation, &row)?;
            outcome.existing_evaluations += 1;
            evaluation_id
        };

        if let RuleEvaluationOutcome::Matched { evidence } = &evaluation.outcome {
            let decision = persist_matched_alert_decision(
                &transaction,
                model_call,
                evaluation,
                &evaluation_id,
                evidence,
                completed_at,
            )?;
            if let Some(decision) = decision {
                outcome.inserted_alerts.push(decision);
            } else {
                outcome.existing_alerts += 1;
            }
        }

        let evaluated_at = timestamp_text(completed_at);
        let updated = transaction.execute(
            "UPDATE rules
             SET last_evaluated_at = CASE
                    WHEN last_evaluated_at IS NULL OR last_evaluated_at < ?3 THEN ?3
                    ELSE last_evaluated_at
                 END
             WHERE rule_id = ?1 AND rule_version = ?2",
            params![
                evaluation.key.rule_id.as_str(),
                evaluation.key.rule_version.get(),
                evaluated_at,
            ],
        )?;
        if updated != 1 {
            return Err(StorageError::RuleDefinitionConflict);
        }
    }

    if !completed_replay {
        let changed = transaction.execute(
            "UPDATE domain_work
             SET state = 'completed', completed_at = ?1
             WHERE id = ?2 AND work_kind = ?3 AND model_call_id = ?4
               AND state = 'started' AND started_at = ?5 AND completed_at IS NULL",
            params![
                timestamp_text(completed_at),
                claim.work_id().to_string(),
                MODEL_CALL_COMPLETED,
                claim.model_call_id().to_string(),
                timestamp_text(claim.claimed_at()),
            ],
        )?;
        if changed != 1 {
            return Err(StorageError::RuleWorkClaimLost);
        }
    }

    transaction.commit()?;
    Ok(outcome)
}

fn persist_matched_alert_decision(
    transaction: &Transaction<'_>,
    model_call: &ModelCall,
    evaluation: &RuleEvaluation,
    evaluation_id: &str,
    evidence: &PatternMatchEvidence,
    completed_at: UtcTimestamp,
) -> StorageResult<Option<RuleAlertDecision>> {
    let persisted = persist_alert(
        transaction,
        evaluation_id,
        evidence,
        &timestamp_text(completed_at),
    )?;
    if !persisted.inserted {
        return Ok(None);
    }
    let policy = load_notification_policy(transaction, evaluation)?;
    let notification_eligibility = policy.eligibility(evaluation.origin, evidence);
    let action = persist_notification_action(
        transaction,
        model_call,
        persisted.id,
        evidence,
        notification_eligibility,
        completed_at,
    )?;
    Ok(Some(RuleAlertDecision {
        alert_id: persisted.id,
        evaluation_key: evaluation.key,
        notification_eligibility,
        action,
    }))
}

pub(super) fn release_rule_work(
    connection: &mut Connection,
    claim: RuleWorkClaim,
) -> StorageResult<()> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = transaction.execute(
        "UPDATE domain_work
         SET id = ?1, state = 'pending', started_at = NULL
         WHERE id = ?2 AND work_kind = ?3 AND model_call_id = ?4
           AND state = 'started' AND started_at = ?5 AND completed_at IS NULL",
        params![
            Uuid::now_v7().to_string(),
            claim.work_id().to_string(),
            MODEL_CALL_COMPLETED,
            claim.model_call_id().to_string(),
            timestamp_text(claim.claimed_at()),
        ],
    )?;
    if changed != 1 {
        return Err(StorageError::RuleWorkClaimLost);
    }
    transaction.commit()?;
    Ok(())
}

pub(super) fn fail_rule_work(
    connection: &mut Connection,
    claim: RuleWorkClaim,
    error_code: RuleErrorCode,
    failed_at: UtcTimestamp,
) -> StorageResult<()> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for definition in BuiltInRuleRegistry::active_definitions() {
        let changed = transaction.execute(
            "UPDATE rules SET last_error_code = ?3
             WHERE rule_id = ?1 AND rule_version = ?2",
            params![
                definition.id.as_str(),
                definition.version.get(),
                error_code.as_str(),
            ],
        )?;
        if changed != 1 {
            return Err(StorageError::RuleDefinitionConflict);
        }
    }
    let changed = transaction.execute(
        "UPDATE domain_work
         SET state = 'completed', completed_at = ?1
         WHERE id = ?2 AND work_kind = ?3 AND model_call_id = ?4
           AND state = 'started' AND started_at = ?5 AND completed_at IS NULL",
        params![
            timestamp_text(failed_at),
            claim.work_id().to_string(),
            MODEL_CALL_COMPLETED,
            claim.model_call_id().to_string(),
            timestamp_text(claim.claimed_at()),
        ],
    )?;
    if changed != 1 {
        return Err(StorageError::RuleWorkClaimLost);
    }
    transaction.commit()?;
    Ok(())
}

fn rule_kind(definition: BuiltInRuleDefinition) -> &'static str {
    match definition.id {
        BuiltInRuleId::SuspectedReasoningLimitPattern => "suspected_reasoning_limit_pattern",
    }
}

fn validate_evaluation_batch(
    claim: RuleWorkClaim,
    model_call: &ModelCall,
    evaluations: &[RuleEvaluation],
) -> StorageResult<()> {
    if model_call.id() != claim.model_call_id() || model_call.origin() != DataOrigin::Realtime {
        return Err(StorageError::RuleEvaluationBatchInvalid);
    }
    let definitions = BuiltInRuleRegistry::active_definitions();
    if evaluations.len() != definitions.len() {
        return Err(StorageError::RuleEvaluationBatchInvalid);
    }
    let mut seen = HashSet::new();
    for evaluation in evaluations {
        if !seen.insert((evaluation.key.rule_id, evaluation.key.rule_version)) {
            return Err(StorageError::RuleEvaluationBatchInvalid);
        }
        let Some(definition) = definitions.iter().find(|definition| {
            definition.id == evaluation.key.rule_id
                && definition.version == evaluation.key.rule_version
        }) else {
            return Err(StorageError::RuleEvaluationBatchInvalid);
        };
        if definition.evaluate(model_call) != *evaluation {
            return Err(StorageError::RuleEvaluationBatchInvalid);
        }
    }
    Ok(())
}

fn validate_claim(transaction: &Transaction<'_>, claim: RuleWorkClaim) -> StorageResult<bool> {
    let stored = transaction
        .query_row(
            "SELECT model_call_id, state, started_at
             FROM domain_work WHERE id = ?1 AND work_kind = ?2",
            params![claim.work_id().to_string(), MODEL_CALL_COMPLETED],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((model_call_id, state, started_at)) = stored else {
        return Err(StorageError::RuleWorkClaimLost);
    };
    if model_call_id != claim.model_call_id().to_string()
        || started_at.as_deref() != Some(timestamp_text(claim.claimed_at()).as_str())
    {
        return Err(StorageError::RuleWorkClaimLost);
    }
    match state.as_str() {
        "started" => Ok(false),
        "completed" => Ok(true),
        _ => Err(StorageError::RuleWorkClaimLost),
    }
}

struct EvaluationRow {
    matched: i64,
    reasoning_output_tokens: Option<i64>,
    pattern_n: Option<i64>,
    event_at: String,
    source_quality: Option<&'static str>,
}

fn evaluation_row(model_call: &ModelCall, evaluation: &RuleEvaluation) -> EvaluationRow {
    let reasoning = model_call.fields().reasoning_output_tokens.as_ref();
    let reasoning_output_tokens = reasoning.map(|value| {
        i64::try_from(value.value().get()).expect("validated token count fits SQLite INTEGER")
    });
    let source_quality = reasoning.map(|value| evidence_quality_text(value.provenance().quality()));
    let (matched, pattern_n) = match &evaluation.outcome {
        RuleEvaluationOutcome::Matched { evidence } => (
            1,
            Some(
                i64::try_from(evidence.pattern_n.get())
                    .expect("validated pattern number fits SQLite INTEGER"),
            ),
        ),
        RuleEvaluationOutcome::MissingValue
        | RuleEvaluationOutcome::NonPositiveValue
        | RuleEvaluationOutcome::NoMatch => (0, None),
    };
    EvaluationRow {
        matched,
        reasoning_output_tokens,
        pattern_n,
        event_at: timestamp_text(*model_call.event_time().value()),
        source_quality,
    }
}

fn verify_existing_evaluation(
    transaction: &Transaction<'_>,
    evaluation: &RuleEvaluation,
    expected: &EvaluationRow,
) -> StorageResult<String> {
    let stored = transaction.query_row(
        "SELECT id, matched, reasoning_output_tokens, pattern_n, event_at, source_quality
         FROM rule_evaluations
         WHERE rule_id = ?1 AND rule_version = ?2 AND model_call_id = ?3",
        params![
            evaluation.key.rule_id.as_str(),
            evaluation.key.rule_version.get(),
            evaluation.key.model_call_id.to_string(),
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        },
    )?;
    if stored.1 != expected.matched
        || stored.2 != expected.reasoning_output_tokens
        || stored.3 != expected.pattern_n
        || stored.4 != expected.event_at
        || stored.5.as_deref() != expected.source_quality
    {
        return Err(StorageError::RuleEvaluationConflict);
    }
    Ok(stored.0)
}

struct PersistedAlert {
    id: AlertId,
    inserted: bool,
}

struct AlertRow {
    model_call_id: String,
    rule_id: &'static str,
    rule_version: u16,
    reasoning_output_tokens: i64,
    pattern_n: i64,
    event_at: String,
    source_quality: &'static str,
}

fn persist_alert(
    transaction: &Transaction<'_>,
    evaluation_id: &str,
    evidence: &PatternMatchEvidence,
    created_at: &str,
) -> StorageResult<PersistedAlert> {
    let row = AlertRow {
        model_call_id: evidence.model_call_id.to_string(),
        rule_id: evidence.rule_id.as_str(),
        rule_version: evidence.rule_version.get(),
        reasoning_output_tokens: i64::try_from(evidence.reasoning_output_tokens.get())
            .expect("validated token count fits SQLite INTEGER"),
        pattern_n: i64::try_from(evidence.pattern_n.get())
            .expect("validated pattern number fits SQLite INTEGER"),
        event_at: timestamp_text(evidence.event_time),
        source_quality: evidence_quality_text(evidence.token_provenance.quality()),
    };
    let candidate_id = AlertId::new();
    let inserted = transaction.execute(
        "INSERT INTO alerts (
            id, evaluation_id, model_call_id, rule_id, rule_version,
            reasoning_output_tokens, pattern_n, event_at, source_quality, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT (evaluation_id) DO NOTHING",
        params![
            candidate_id.to_string(),
            evaluation_id,
            row.model_call_id,
            row.rule_id,
            row.rule_version,
            row.reasoning_output_tokens,
            row.pattern_n,
            row.event_at,
            row.source_quality,
            created_at,
        ],
    )?;
    if inserted == 1 {
        persist_alert_provenance(transaction, candidate_id, evidence)?;
        return Ok(PersistedAlert {
            id: candidate_id,
            inserted: true,
        });
    }
    let existing_id = verify_existing_alert(transaction, evaluation_id, &row)?;
    persist_alert_provenance(transaction, existing_id, evidence)?;
    Ok(PersistedAlert {
        id: existing_id,
        inserted: false,
    })
}

fn persist_alert_provenance(
    transaction: &Transaction<'_>,
    alert_id: AlertId,
    evidence: &PatternMatchEvidence,
) -> StorageResult<()> {
    let provenance = &evidence.token_provenance;
    let expected = (
        provenance.source().id().as_str(),
        provenance.source().contract_version().get(),
        timestamp_text(provenance.observed_at()),
        evidence_quality_text(provenance.quality()),
    );
    transaction.execute(
        "INSERT INTO alert_evidence_provenance (
            alert_id, source_id, source_contract_version, observed_at, quality
         ) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (alert_id) DO NOTHING",
        params![
            alert_id.to_string(),
            expected.0,
            expected.1,
            expected.2,
            expected.3,
        ],
    )?;
    let stored = transaction.query_row(
        "SELECT source_id, source_contract_version, observed_at, quality
         FROM alert_evidence_provenance WHERE alert_id = ?1",
        [alert_id.to_string()],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        },
    )?;
    if stored.0 != expected.0
        || stored.1 != i64::from(expected.1)
        || stored.2 != expected.2
        || stored.3 != expected.3
    {
        return Err(StorageError::AlertEvidenceConflict);
    }
    Ok(())
}

fn verify_existing_alert(
    transaction: &Transaction<'_>,
    evaluation_id: &str,
    expected: &AlertRow,
) -> StorageResult<AlertId> {
    let stored = transaction.query_row(
        "SELECT id, model_call_id, rule_id, rule_version,
                reasoning_output_tokens, pattern_n, event_at, source_quality
         FROM alerts WHERE evaluation_id = ?1",
        [evaluation_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        },
    )?;
    if stored.1 != expected.model_call_id
        || stored.2 != expected.rule_id
        || stored.3 != i64::from(expected.rule_version)
        || stored.4 != expected.reasoning_output_tokens
        || stored.5 != expected.pattern_n
        || stored.6 != expected.event_at
        || stored.7 != expected.source_quality
    {
        return Err(StorageError::AlertEvidenceConflict);
    }
    AlertId::from_str(&stored.0).map_err(|_| StorageError::AlertEvidenceConflict)
}

fn load_notification_policy(
    transaction: &Transaction<'_>,
    evaluation: &RuleEvaluation,
) -> StorageResult<RuleNotificationPolicy> {
    let (rule_enabled, notify_max_n, notify_all, notifications_enabled) = transaction.query_row(
        "SELECT rules.enabled, rules.notify_max_n, rules.notify_all,
                app_settings.notifications_enabled
         FROM rules CROSS JOIN app_settings
         WHERE rules.rule_id = ?1 AND rules.rule_version = ?2
           AND app_settings.singleton = 1",
        params![
            evaluation.key.rule_id.as_str(),
            evaluation.key.rule_version.get(),
        ],
        |row| {
            Ok((
                row.get::<_, bool>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, bool>(3)?,
            ))
        },
    )?;
    Ok(RuleNotificationPolicy {
        enabled: rule_enabled && notifications_enabled,
        threshold: notification_threshold(notify_max_n, notify_all, "rules.notify_threshold")?,
    })
}

fn notification_threshold(
    maximum: Option<i64>,
    all: bool,
    field: &'static str,
) -> StorageResult<NotificationThreshold> {
    match (maximum, all) {
        (None, true) => Ok(NotificationThreshold::All),
        (Some(maximum), false) => {
            let maximum = u64::try_from(maximum)
                .ok()
                .and_then(NonZeroU64::new)
                .ok_or(StorageError::InvalidStoredValue { field })?;
            Ok(NotificationThreshold::AtMost(maximum))
        }
        _ => Err(StorageError::InvalidStoredValue { field }),
    }
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU16, NonZeroU64};

    use codex_monitor_core::{
        EvidenceQuality, FieldProvenance, ModelCallId, PatternN, RuleVersion,
        SUSPECTED_REASONING_LIMIT_PATTERN, SourceContractVersion, SourceId, SourceIdentity,
        SourcePriority, TokenCount,
    };
    use tempfile::tempdir;

    use super::*;
    use crate::migrations::open_and_migrate;

    fn at(seconds: i128) -> UtcTimestamp {
        UtcTimestamp::from_unix_timestamp_nanos(seconds * 1_000_000_000).unwrap()
    }

    #[test]
    fn new_rule_version_inherits_settings_and_rejects_semantic_drift() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("rule-definitions.sqlite3");
        let mut connection = open_and_migrate(&database).unwrap();
        register_builtin_rules(&mut connection, &[SUSPECTED_REASONING_LIMIT_PATTERN]).unwrap();
        connection
            .execute(
                "UPDATE rules
                 SET enabled = 0, notify_max_n = 7, notify_all = 0
                 WHERE rule_id = 'suspected-reasoning-limit-pattern' AND rule_version = 1",
                [],
            )
            .unwrap();

        let version_two = BuiltInRuleDefinition {
            version: RuleVersion::new(NonZeroU16::new(2).unwrap()),
            ..SUSPECTED_REASONING_LIMIT_PATTERN
        };
        register_builtin_rules(&mut connection, &[version_two]).unwrap();
        let settings = connection
            .prepare(
                "SELECT rule_version, enabled, notify_max_n, notify_all
                 FROM rules WHERE rule_id = 'suspected-reasoning-limit-pattern'
                 ORDER BY rule_version",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, bool>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, bool>(3)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            settings,
            [(1, false, Some(7), false), (2, false, Some(7), false)]
        );

        let fingerprints = connection
            .prepare(
                "SELECT semantic_sha256 FROM rule_definition_fingerprints
                 WHERE rule_id = 'suspected-reasoning-limit-pattern'
                 ORDER BY rule_version",
            )
            .unwrap()
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(fingerprints.len(), 2);
        assert_ne!(fingerprints[0], fingerprints[1]);

        drop(connection);
        let mut connection = open_and_migrate(&database).unwrap();
        register_builtin_rules(&mut connection, &[version_two]).unwrap();
        let preserved_versions = connection
            .prepare(
                "SELECT rule_version, enabled, notify_max_n, notify_all
                 FROM rules WHERE rule_id = 'suspected-reasoning-limit-pattern'
                 ORDER BY rule_version",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, bool>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, bool>(3)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            preserved_versions,
            [(1, false, Some(7), false), (2, false, Some(7), false)]
        );

        connection
            .execute(
                "DELETE FROM rule_definition_fingerprints
                 WHERE rule_id = 'suspected-reasoning-limit-pattern' AND rule_version = 2",
                [],
            )
            .unwrap();
        assert!(matches!(
            register_builtin_rules(&mut connection, &[version_two]).unwrap_err(),
            StorageError::RuleDefinitionConflict
        ));
        connection
            .execute(
                "INSERT INTO rule_definition_fingerprints (
                    rule_id, rule_version, semantic_sha256
                 ) VALUES ('suspected-reasoning-limit-pattern', 2, zeroblob(32))",
                [],
            )
            .unwrap();
        assert!(matches!(
            register_builtin_rules(&mut connection, &[version_two]).unwrap_err(),
            StorageError::RuleDefinitionConflict
        ));
    }

    #[test]
    fn claim_skips_historical_work_even_if_database_guards_are_bypassed() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("historical-claim-filter.sqlite3");
        let mut connection = open_and_migrate(&database).unwrap();
        let historical_call = ModelCallId::new();
        let realtime_call = ModelCallId::new();
        connection
            .pragma_update(None, "foreign_keys", false)
            .unwrap();
        for (id, origin, event_observation) in [
            (historical_call, "historical", "historical-observation"),
            (realtime_call, "realtime", "realtime-observation"),
        ] {
            connection
                .execute(
                    "INSERT INTO model_calls (
                        id, canonical_source_id, canonical_source_contract_version,
                        canonical_identity_version, canonical_identity_kind,
                        canonical_identity_digest, event_at, event_time_observation_id, origin
                     ) VALUES (?1, 'test', 1, 1, 'stable', randomblob(32), ?2, ?3, ?4)",
                    params![
                        id.to_string(),
                        timestamp_text(at(1)),
                        event_observation,
                        origin,
                    ],
                )
                .unwrap();
        }
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        connection
            .execute_batch("DROP TRIGGER domain_work_historical_insert_guard;")
            .unwrap();
        connection
            .execute(
                "INSERT INTO domain_work (
                    id, work_kind, model_call_id, state, created_at
                 ) VALUES (?1, ?2, ?3, 'pending', ?4)",
                params![
                    Uuid::now_v7().to_string(),
                    MODEL_CALL_COMPLETED,
                    historical_call.to_string(),
                    timestamp_text(at(1)),
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO domain_work (
                    id, work_kind, model_call_id, state, created_at
                 ) VALUES (?1, ?2, ?3, 'pending', ?4)",
                params![
                    Uuid::now_v7().to_string(),
                    MODEL_CALL_COMPLETED,
                    realtime_call.to_string(),
                    timestamp_text(at(2)),
                ],
            )
            .unwrap();

        let claim = claim_rule_work(&mut connection, at(10), at(9))
            .unwrap()
            .unwrap();
        assert_eq!(claim.model_call_id(), realtime_call);
        let historical_state: String = connection
            .query_row(
                "SELECT state FROM domain_work WHERE model_call_id = ?1",
                [historical_call.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(historical_state, "pending");
        release_rule_work(&mut connection, claim).unwrap();
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn alert_provenance_is_immutable_and_replay_verifies_the_full_tuple() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("alert-provenance.sqlite3");
        let mut connection = open_and_migrate(&database).unwrap();
        register_builtin_rules(&mut connection, &[SUSPECTED_REASONING_LIMIT_PATTERN]).unwrap();
        let model_call_id = ModelCallId::new();
        let evaluation_id = Uuid::now_v7().to_string();
        connection
            .pragma_update(None, "foreign_keys", false)
            .unwrap();
        connection
            .execute(
                "INSERT INTO model_calls (
                    id, canonical_source_id, canonical_source_contract_version,
                    canonical_identity_version, canonical_identity_kind,
                    canonical_identity_digest, event_at, event_time_observation_id, origin
                 ) VALUES (?1, 'source-one', 1, 1, 'stable', randomblob(32), ?2,
                           'synthetic-observation', 'realtime')",
                params![model_call_id.to_string(), timestamp_text(at(1))],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO model_call_dashboard_projection (
                    model_call_id, event_at, temporal_class, reasoning_output_tokens
                 ) VALUES (?1, ?2, 'realtime', 516)",
                params![model_call_id.to_string(), timestamp_text(at(1))],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO rule_evaluations (
                    id, rule_id, rule_version, model_call_id, matched,
                    reasoning_output_tokens, pattern_n, event_at,
                    source_quality, evaluated_at
                 ) VALUES (?1, ?2, 1, ?3, 1, 516, 1, ?4, 'direct', ?4)",
                params![
                    evaluation_id,
                    BuiltInRuleId::SuspectedReasoningLimitPattern.as_str(),
                    model_call_id.to_string(),
                    timestamp_text(at(1)),
                ],
            )
            .unwrap();
        let provenance = FieldProvenance::new(
            SourceIdentity::new(
                SourceId::new("source-one").unwrap(),
                SourceContractVersion::new(1).unwrap(),
            ),
            at(2),
            EvidenceQuality::Direct,
            SourcePriority::new(100),
        );
        let evidence = PatternMatchEvidence {
            model_call_id,
            rule_id: BuiltInRuleId::SuspectedReasoningLimitPattern,
            rule_version: RuleVersion::new(NonZeroU16::MIN),
            reasoning_output_tokens: TokenCount::new(516).unwrap(),
            pattern_n: PatternN::new(NonZeroU64::MIN),
            event_time: at(1),
            token_provenance: provenance,
        };
        let created_at = timestamp_text(at(3));
        let first = {
            let transaction = connection.transaction().unwrap();
            let persisted =
                persist_alert(&transaction, &evaluation_id, &evidence, &created_at).unwrap();
            transaction.commit().unwrap();
            persisted
        };
        assert!(first.inserted);
        let replay = {
            let transaction = connection.transaction().unwrap();
            let persisted =
                persist_alert(&transaction, &evaluation_id, &evidence, &created_at).unwrap();
            transaction.commit().unwrap();
            persisted
        };
        assert!(!replay.inserted);
        assert_eq!(replay.id, first.id);

        connection
            .execute(
                "UPDATE model_call_dashboard_projection
                 SET reasoning_output_tokens = 517
                 WHERE model_call_id = ?1",
                [model_call_id.to_string()],
            )
            .unwrap();
        let stored: (String, i64, String, String) = connection
            .query_row(
                "SELECT source_id, source_contract_version, observed_at, quality
                 FROM alert_evidence_provenance WHERE alert_id = ?1",
                [first.id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            stored,
            (
                "source-one".into(),
                1,
                timestamp_text(at(2)),
                "direct".into()
            )
        );

        let mut conflicting = evidence.clone();
        conflicting.token_provenance = FieldProvenance::new(
            SourceIdentity::new(
                SourceId::new("source-two").unwrap(),
                SourceContractVersion::new(2).unwrap(),
            ),
            at(4),
            EvidenceQuality::Inferred,
            SourcePriority::new(1),
        );
        let transaction = connection.transaction().unwrap();
        assert!(matches!(
            persist_alert(&transaction, &evaluation_id, &conflicting, &created_at),
            Err(StorageError::AlertEvidenceConflict)
        ));
    }
}
