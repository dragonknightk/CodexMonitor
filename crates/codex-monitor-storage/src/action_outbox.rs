use std::num::{NonZeroU16, NonZeroU32, NonZeroU64};
use std::str::FromStr as _;

use codex_monitor_core::{
    ActionErrorCode, ActionExecutionResult, ActionId, ActionLifecycleState, ActionParameters,
    ActionRegistryError, ActionRequest, ActionSuppressionReason, AlertId, ConversationId,
    ModelCall, ModelCallId, ModelName, NotificationCoalescingWindowId, NotificationEligibility,
    PatternMatchEvidence, PatternN, RegisteredActionRegistry, RegisteredActionType,
    SystemNotificationParams, TokenCount, UtcTimestamp,
};
use rusqlite::{Connection, OptionalExtension as _, Transaction, TransactionBehavior, params};
use time::Duration;

use super::codec::parse_timestamp;
use super::timestamp::timestamp_text;
use super::{
    ActionAttemptState, ActionClaimOutcome, ActionClaimToken, ActionFinalization, ActionLeaseOwner,
    ActionLeaseWindow, ActionRecoveryOutcome, ActionRecoveryRequest, ActionStartOutcome,
    ClaimedAction, NotificationCoalescingRole, RuleActionDecision, StartedAction, StorageError,
    StorageResult, StoredAction, StoredActionAttempt, StoredActionType,
    StoredNotificationCoalescing,
};

enum NotificationCoalescingPlan {
    None,
    Leader {
        window_id: NotificationCoalescingWindowId,
        conversation_id: ConversationId,
        closes_at: UtcTimestamp,
    },
    Follower {
        window_id: NotificationCoalescingWindowId,
    },
}

pub(super) struct ActionTransition<T> {
    outcome: T,
    changed: bool,
}

impl<T> ActionTransition<T> {
    const fn committed(outcome: T) -> Self {
        Self {
            outcome,
            changed: true,
        }
    }

    const fn replayed(outcome: T) -> Self {
        Self {
            outcome,
            changed: false,
        }
    }

    pub(super) const fn changed(&self) -> bool {
        self.changed
    }

    pub(super) fn into_outcome(self) -> T {
        self.outcome
    }
}

pub(super) fn persist_notification_action(
    transaction: &Transaction<'_>,
    model_call: &ModelCall,
    alert_id: AlertId,
    evidence: &PatternMatchEvidence,
    eligibility: NotificationEligibility,
    created_at: UtcTimestamp,
) -> StorageResult<Option<RuleActionDecision>> {
    if matches!(
        eligibility,
        NotificationEligibility::AboveThreshold | NotificationEligibility::HistoricalEvent
    ) {
        return Ok(None);
    }
    let coalescing = match eligibility {
        NotificationEligibility::Eligible => {
            notification_coalescing_plan(transaction, model_call, created_at)?
        }
        NotificationEligibility::NotificationsDisabled => NotificationCoalescingPlan::None,
        NotificationEligibility::AboveThreshold | NotificationEligibility::HistoricalEvent => {
            unreachable!("non-action eligibility returned before planning")
        }
    };
    let (state, not_before, completed_at, suppression_reason) = match &coalescing {
        NotificationCoalescingPlan::Follower { .. } => (
            ActionLifecycleState::Suppressed,
            created_at,
            Some(created_at),
            Some(ActionSuppressionReason::Coalesced),
        ),
        NotificationCoalescingPlan::Leader { closes_at, .. } => {
            (ActionLifecycleState::Pending, *closes_at, None, None)
        }
        NotificationCoalescingPlan::None if eligibility == NotificationEligibility::Eligible => {
            (ActionLifecycleState::Pending, created_at, None, None)
        }
        NotificationCoalescingPlan::None => (
            ActionLifecycleState::Suppressed,
            created_at,
            Some(created_at),
            Some(ActionSuppressionReason::NotificationsDisabled),
        ),
    };
    let model = model_call
        .fields()
        .model
        .as_ref()
        .map(|value| value.value().clone());
    let request = ActionRequest::system_notification(alert_id, model, evidence, created_at);
    let ActionParameters::SystemNotification(parameters) = &request.parameters;
    let inserted = transaction.execute(
        "INSERT INTO action_outbox (
            id, alert_id, action_type, parameter_schema_version,
            idempotency_key_version, idempotency_key_bytes, state, model,
            reasoning_output_tokens, pattern_n, created_at, not_before,
            lease_generation, completed_at, suppression_reason
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0, ?13, ?14
         )
         ON CONFLICT DO NOTHING",
        params![
            request.id.to_string(),
            request.alert_id.to_string(),
            request.parameters.action_type().as_str(),
            request.parameter_schema_version.get(),
            request.idempotency_key.version(),
            request.idempotency_key.digest().as_slice(),
            state.as_str(),
            parameters.model().map(ModelName::as_str),
            i64::try_from(parameters.reasoning_output_tokens().get())
                .expect("validated token count fits SQLite INTEGER"),
            i64::try_from(parameters.pattern_n().get())
                .expect("validated pattern number fits SQLite INTEGER"),
            timestamp_text(request.created_at),
            timestamp_text(not_before),
            completed_at.map(timestamp_text),
            suppression_reason.map(ActionSuppressionReason::as_str),
        ],
    )?;
    let action_id = if inserted == 1 {
        persist_notification_coalescing(transaction, &request, &coalescing)?;
        request.id
    } else {
        verify_existing_action(transaction, &request, state, not_before, suppression_reason)?
    };
    Ok(Some(RuleActionDecision { action_id, state }))
}

fn notification_coalescing_plan(
    transaction: &Transaction<'_>,
    model_call: &ModelCall,
    created_at: UtcTimestamp,
) -> StorageResult<NotificationCoalescingPlan> {
    let Some(conversation_id) = model_call
        .fields()
        .relationship
        .as_ref()
        .map(|relationship| relationship.value().conversation_id())
    else {
        return Ok(NotificationCoalescingPlan::None);
    };
    let created_at_text = timestamp_text(created_at);
    let raw_windows = {
        let mut statement = transaction.prepare(
            "SELECT id
             FROM notification_coalescing_windows
             WHERE conversation_id = ?1
               AND opened_at <= ?2 AND ?2 < closes_at
             ORDER BY opened_at, id
             LIMIT 2",
        )?;
        statement
            .query_map(
                params![conversation_id.to_string(), created_at_text],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?
    };
    match raw_windows.as_slice() {
        [] => {
            let closes_at = created_at
                .as_offset_datetime()
                .checked_add(Duration::seconds(
                    super::NOTIFICATION_COALESCING_WINDOW_SECONDS,
                ))
                .map(UtcTimestamp::new)
                .ok_or(StorageError::NotificationCoalescingWindowOverflow)?;
            Ok(NotificationCoalescingPlan::Leader {
                window_id: NotificationCoalescingWindowId::new(),
                conversation_id,
                closes_at,
            })
        }
        [window_id] => Ok(NotificationCoalescingPlan::Follower {
            window_id: window_id
                .parse()
                .map_err(|_| StorageError::InvalidStoredValue {
                    field: "notification_coalescing_windows.id",
                })?,
        }),
        _ => Err(StorageError::ActionRequestConflict),
    }
}

fn persist_notification_coalescing(
    transaction: &Transaction<'_>,
    request: &ActionRequest,
    plan: &NotificationCoalescingPlan,
) -> StorageResult<()> {
    match plan {
        NotificationCoalescingPlan::None => Ok(()),
        NotificationCoalescingPlan::Leader {
            window_id,
            conversation_id,
            closes_at,
        } => {
            transaction.execute(
                "INSERT INTO notification_coalescing_windows (
                    id, conversation_id, leader_action_id, opened_at, closes_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    window_id.to_string(),
                    conversation_id.to_string(),
                    request.id.to_string(),
                    timestamp_text(request.created_at),
                    timestamp_text(*closes_at),
                ],
            )?;
            insert_notification_coalescing_member(
                transaction,
                request.id,
                *window_id,
                "leader",
                request.created_at,
            )
        }
        NotificationCoalescingPlan::Follower { window_id } => {
            insert_notification_coalescing_member(
                transaction,
                request.id,
                *window_id,
                "follower",
                request.created_at,
            )
        }
    }
}

fn insert_notification_coalescing_member(
    transaction: &Transaction<'_>,
    action_id: ActionId,
    window_id: NotificationCoalescingWindowId,
    role: &'static str,
    joined_at: UtcTimestamp,
) -> StorageResult<()> {
    let inserted = transaction.execute(
        "INSERT INTO notification_coalescing_members (
            action_id, window_id, role, joined_at
         ) VALUES (?1, ?2, ?3, ?4)",
        params![
            action_id.to_string(),
            window_id.to_string(),
            role,
            timestamp_text(joined_at),
        ],
    )?;
    if inserted != 1 {
        return Err(StorageError::ActionRequestConflict);
    }
    Ok(())
}

fn verify_existing_action(
    transaction: &Transaction<'_>,
    expected: &ActionRequest,
    state: ActionLifecycleState,
    not_before: UtcTimestamp,
    suppression_reason: Option<ActionSuppressionReason>,
) -> StorageResult<ActionId> {
    let stored = transaction
        .query_row(
            "SELECT id, idempotency_key_version, idempotency_key_bytes, state,
                    model, reasoning_output_tokens, pattern_n, created_at,
                    not_before, completed_at, last_error_code, suppression_reason
             FROM action_outbox
             WHERE alert_id = ?1 AND action_type = ?2 AND parameter_schema_version = ?3",
            params![
                expected.alert_id.to_string(),
                expected.parameters.action_type().as_str(),
                expected.parameter_schema_version.get(),
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                ))
            },
        )
        .optional()?;
    let Some(stored) = stored else {
        return Err(StorageError::ActionRequestConflict);
    };
    let ActionParameters::SystemNotification(parameters) = &expected.parameters;
    let created_at = timestamp_text(expected.created_at);
    let lifecycle_matches = match state {
        ActionLifecycleState::Pending => matches!(
            stored.3.as_str(),
            "pending" | "started" | "completed" | "failed" | "suppressed" | "unavailable"
        ),
        ActionLifecycleState::Suppressed => {
            stored.3 == ActionLifecycleState::Suppressed.as_str()
                && stored.9.as_ref() == Some(&created_at)
                && stored.10.is_none()
                && stored.11.as_deref() == suppression_reason.map(ActionSuppressionReason::as_str)
        }
        _ => false,
    };
    let matches = stored.1 == i64::from(expected.idempotency_key.version())
        && stored.2.as_slice() == expected.idempotency_key.digest()
        && lifecycle_matches
        && stored.4.as_deref() == parameters.model().map(ModelName::as_str)
        && stored.5
            == i64::try_from(parameters.reasoning_output_tokens().get())
                .expect("validated token count fits SQLite INTEGER")
        && stored.6
            == i64::try_from(parameters.pattern_n().get())
                .expect("validated pattern number fits SQLite INTEGER")
        && stored.7 == created_at
        && stored.8 == timestamp_text(not_before);
    if !matches {
        return Err(StorageError::ActionRequestConflict);
    }
    ActionId::from_str(&stored.0).map_err(|_| StorageError::ActionRequestConflict)
}

struct RawPendingAction {
    id: String,
    alert_id: String,
    model_call_id: String,
    action_type: String,
    parameter_schema_version: i64,
    idempotency_key_version: i64,
    idempotency_key_bytes: Vec<u8>,
    model: Option<String>,
    reasoning_output_tokens: i64,
    pattern_n: i64,
    created_at: String,
    lease_generation: i64,
    alert_reasoning_output_tokens: i64,
    alert_pattern_n: i64,
    grouped: bool,
}

const CLAIM_ACTION_SQL: &str = "SELECT action.id, action.alert_id, alert.model_call_id,
            action.action_type, action.parameter_schema_version,
            action.idempotency_key_version, action.idempotency_key_bytes,
            action.model, action.reasoning_output_tokens, action.pattern_n,
            action.created_at, action.lease_generation,
            alert.reasoning_output_tokens, alert.pattern_n,
            EXISTS (
                SELECT 1
                FROM notification_coalescing_windows AS coalescing_window
                INNER JOIN notification_coalescing_members AS coalescing_member
                    ON coalescing_member.window_id = coalescing_window.id
                WHERE coalescing_window.leader_action_id = action.id
                  AND coalescing_member.role = 'follower'
            )
     FROM action_outbox AS action
     INNER JOIN alerts AS alert ON alert.id = action.alert_id
     INNER JOIN model_calls AS model_call ON model_call.id = alert.model_call_id
     WHERE action.state = 'pending'
       AND action.not_before <= ?1
       AND (action.lease_owner IS NULL OR action.lease_expires_at <= ?1)
       AND model_call.origin = 'realtime'
     ORDER BY action.not_before, action.created_at, action.id
     LIMIT 1";

pub(super) fn claim_action(
    connection: &mut Connection,
    lease_owner: ActionLeaseOwner,
    window: ActionLeaseWindow,
) -> StorageResult<ActionClaimOutcome> {
    let claimed_at = timestamp_text(window.claimed_at());
    let lease_expires_at = timestamp_text(window.expires_at());
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let raw = transaction
        .query_row(CLAIM_ACTION_SQL, [&claimed_at], |row| {
            Ok(RawPendingAction {
                id: row.get(0)?,
                alert_id: row.get(1)?,
                model_call_id: row.get(2)?,
                action_type: row.get(3)?,
                parameter_schema_version: row.get(4)?,
                idempotency_key_version: row.get(5)?,
                idempotency_key_bytes: row.get(6)?,
                model: row.get(7)?,
                reasoning_output_tokens: row.get(8)?,
                pattern_n: row.get(9)?,
                created_at: row.get(10)?,
                lease_generation: row.get(11)?,
                alert_reasoning_output_tokens: row.get(12)?,
                alert_pattern_n: row.get(13)?,
                grouped: row.get(14)?,
            })
        })
        .optional()?;
    let Some(raw) = raw else {
        transaction.commit()?;
        return Ok(ActionClaimOutcome::Idle);
    };
    let action_id = ActionId::from_str(&raw.id).map_err(|_| StorageError::InvalidStoredValue {
        field: "action_outbox.id",
    })?;
    let request = match build_action_request(&raw) {
        Ok(request) => request,
        Err(error_code) => {
            let finalization = reject_pending_action(
                &transaction,
                action_id,
                &raw.id,
                error_code,
                window.claimed_at(),
            )?;
            transaction.commit()?;
            return Ok(ActionClaimOutcome::RejectedUnavailable(finalization));
        }
    };
    let previous_generation =
        u64::try_from(raw.lease_generation).map_err(|_| StorageError::InvalidStoredValue {
            field: "action_outbox.lease_generation",
        })?;
    let lease_generation = previous_generation
        .checked_add(1)
        .and_then(NonZeroU64::new)
        .ok_or(StorageError::InvalidStoredValue {
            field: "action_outbox.lease_generation",
        })?;
    let changed = transaction.execute(
        "UPDATE action_outbox
         SET lease_owner = ?1, lease_expires_at = ?2, lease_generation = ?3
         WHERE id = ?4 AND state = 'pending' AND lease_generation = ?5
           AND (lease_owner IS NULL OR lease_expires_at <= ?6)",
        params![
            lease_owner.as_uuid().to_string(),
            lease_expires_at,
            lease_generation_integer(lease_generation)?,
            raw.id,
            raw.lease_generation,
            claimed_at,
        ],
    )?;
    if changed != 1 {
        return Err(StorageError::ActionClaimLost);
    }
    let token = ActionClaimToken::new(action_id, lease_owner, lease_generation);
    transaction.commit()?;
    Ok(ActionClaimOutcome::Claimed(ClaimedAction::new(
        token,
        request,
        raw.grouped,
        window.claimed_at(),
        window.expires_at(),
    )))
}

fn build_action_request(raw: &RawPendingAction) -> Result<ActionRequest, ActionErrorCode> {
    let schema_version = u16::try_from(raw.parameter_schema_version)
        .ok()
        .and_then(NonZeroU16::new)
        .ok_or(ActionErrorCode::InvalidParameters)?;
    RegisteredActionRegistry::resolve(&raw.action_type, schema_version.get()).map_err(|error| {
        match error {
            ActionRegistryError::UnknownActionType => ActionErrorCode::UnknownActionType,
            ActionRegistryError::SchemaVersionMismatch => ActionErrorCode::InvalidParameters,
        }
    })?;
    let action_id = ActionId::from_str(&raw.id).map_err(|_| ActionErrorCode::InvalidParameters)?;
    let alert_id =
        AlertId::from_str(&raw.alert_id).map_err(|_| ActionErrorCode::InvalidParameters)?;
    let model_call_id = ModelCallId::from_str(&raw.model_call_id)
        .map_err(|_| ActionErrorCode::InvalidParameters)?;
    let model = raw
        .model
        .as_ref()
        .map(|model| ModelName::new(model.clone()))
        .transpose()
        .map_err(|_| ActionErrorCode::InvalidParameters)?;
    let reasoning_output_tokens = TokenCount::try_from(raw.reasoning_output_tokens)
        .map_err(|_| ActionErrorCode::InvalidParameters)?;
    let pattern_n =
        parse_pattern_n(raw.pattern_n).map_err(|_| ActionErrorCode::InvalidParameters)?;
    if raw.reasoning_output_tokens != raw.alert_reasoning_output_tokens
        || raw.pattern_n != raw.alert_pattern_n
    {
        return Err(ActionErrorCode::InvalidParameters);
    }
    let created_at = UtcTimestamp::parse_rfc3339(&raw.created_at)
        .map_err(|_| ActionErrorCode::InvalidParameters)?;
    let request = ActionRequest::from_stored_system_notification(
        action_id,
        alert_id,
        model_call_id,
        model,
        reasoning_output_tokens,
        pattern_n,
        created_at,
    );
    if raw.action_type != request.parameters.action_type().as_str()
        || schema_version != request.parameter_schema_version
        || raw.idempotency_key_version != i64::from(request.idempotency_key.version())
        || raw.idempotency_key_bytes.as_slice() != request.idempotency_key.digest()
    {
        return Err(ActionErrorCode::InvalidParameters);
    }
    Ok(request)
}

fn reject_pending_action(
    transaction: &Transaction<'_>,
    action_id: ActionId,
    raw_action_id: &str,
    error_code: ActionErrorCode,
    completed_at: UtcTimestamp,
) -> StorageResult<ActionFinalization> {
    let changed = transaction.execute(
        "UPDATE action_outbox
         SET state = 'unavailable', lease_owner = NULL, lease_expires_at = NULL,
             completed_at = ?1, last_error_code = ?2, suppression_reason = NULL
         WHERE id = ?3 AND state = 'pending'",
        params![
            timestamp_text(completed_at),
            error_code.as_str(),
            raw_action_id,
        ],
    )?;
    if changed != 1 {
        return Err(StorageError::ActionClaimLost);
    }
    Ok(ActionFinalization {
        action_id,
        state: ActionLifecycleState::Unavailable,
        completed_at,
        error_code: Some(error_code),
        suppression_reason: None,
    })
}

pub(super) fn release_action_claim(
    connection: &mut Connection,
    claim: ActionClaimToken,
) -> StorageResult<()> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = transaction.execute(
        "UPDATE action_outbox
         SET lease_owner = NULL, lease_expires_at = NULL
         WHERE id = ?1 AND state = 'pending' AND lease_owner = ?2
           AND lease_generation = ?3",
        params![
            claim.action_id().to_string(),
            claim.lease_owner().as_uuid().to_string(),
            lease_generation_integer(claim.lease_generation())?,
        ],
    )?;
    if changed != 1 {
        return Err(StorageError::ActionClaimLost);
    }
    transaction.commit()?;
    Ok(())
}

pub(super) fn start_action(
    connection: &mut Connection,
    claimed: ClaimedAction,
    started_at: UtcTimestamp,
) -> StorageResult<ActionTransition<ActionStartOutcome>> {
    let (claim, request, grouped, claimed_at, lease_expires_at) = claimed.into_parts();
    if started_at < claimed_at {
        return Err(StorageError::InvalidActionTransition);
    }
    if started_at >= lease_expires_at {
        return Err(StorageError::ActionClaimLost);
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(replayed) = replayed_action_start(
        &transaction,
        claim,
        &request,
        grouped,
        lease_expires_at,
        started_at,
    )? {
        transaction.commit()?;
        return Ok(ActionTransition::replayed(replayed));
    }
    validate_pending_claim(&transaction, claim, lease_expires_at, started_at)?;
    let notifications_enabled = transaction.query_row(
        "SELECT notifications_enabled FROM app_settings WHERE singleton = 1",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if !notifications_enabled {
        let changed = transaction.execute(
            "UPDATE action_outbox
             SET state = 'suppressed', lease_owner = NULL, lease_expires_at = NULL,
                 completed_at = ?1, suppression_reason = ?2
             WHERE id = ?3 AND state = 'pending' AND lease_owner = ?4
               AND lease_generation = ?5 AND lease_expires_at > ?1",
            params![
                timestamp_text(started_at),
                ActionSuppressionReason::NotificationsDisabled.as_str(),
                claim.action_id().to_string(),
                claim.lease_owner().as_uuid().to_string(),
                lease_generation_integer(claim.lease_generation())?,
            ],
        )?;
        if changed != 1 {
            return Err(StorageError::ActionClaimLost);
        }
        transaction.commit()?;
        return Ok(ActionTransition::committed(ActionStartOutcome::Suppressed(
            ActionFinalization {
                action_id: claim.action_id(),
                state: ActionLifecycleState::Suppressed,
                completed_at: started_at,
                error_code: None,
                suppression_reason: Some(ActionSuppressionReason::NotificationsDisabled),
            },
        )));
    }

    let attempt_number = next_attempt_number(&transaction, claim.action_id())?;
    let changed = transaction.execute(
        "UPDATE action_outbox
         SET state = 'started', started_at = ?1
         WHERE id = ?2 AND state = 'pending' AND lease_owner = ?3
           AND lease_generation = ?4 AND lease_expires_at > ?1",
        params![
            timestamp_text(started_at),
            claim.action_id().to_string(),
            claim.lease_owner().as_uuid().to_string(),
            lease_generation_integer(claim.lease_generation())?,
        ],
    )?;
    if changed != 1 {
        return Err(StorageError::ActionClaimLost);
    }
    transaction.execute(
        "INSERT INTO action_attempts (
            action_id, attempt_number, lease_generation, state, started_at
         ) VALUES (?1, ?2, ?3, 'started', ?4)",
        params![
            claim.action_id().to_string(),
            attempt_number.get(),
            lease_generation_integer(claim.lease_generation())?,
            timestamp_text(started_at),
        ],
    )?;
    transaction.commit()?;
    Ok(ActionTransition::committed(ActionStartOutcome::Started(
        StartedAction::new(claim, request, grouped, attempt_number, started_at),
    )))
}

fn replayed_action_start(
    transaction: &Transaction<'_>,
    claim: ActionClaimToken,
    request: &ActionRequest,
    grouped: bool,
    lease_expires_at: UtcTimestamp,
    started_at: UtcTimestamp,
) -> StorageResult<Option<ActionStartOutcome>> {
    let raw = transaction
        .query_row(
            "SELECT state, lease_generation, lease_owner, lease_expires_at,
                    started_at, completed_at, suppression_reason
             FROM action_outbox WHERE id = ?1",
            [claim.action_id().to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .optional()?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    let generation = lease_generation_integer(claim.lease_generation())?;
    let started_at_text = timestamp_text(started_at);
    if raw.0 == ActionLifecycleState::Started.as_str()
        && raw.1 == generation
        && raw.2.as_deref() == Some(claim.lease_owner().as_uuid().to_string().as_str())
        && raw.3.as_deref() == Some(timestamp_text(lease_expires_at).as_str())
        && raw.4.as_deref() == Some(started_at_text.as_str())
        && raw.5.is_none()
        && raw.6.is_none()
    {
        let attempt_number = transaction
            .query_row(
                "SELECT attempt_number FROM action_attempts
                 WHERE action_id = ?1 AND lease_generation = ?2
                   AND state = 'started' AND started_at = ?3",
                params![claim.action_id().to_string(), generation, started_at_text],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .and_then(|value| u32::try_from(value).ok())
            .and_then(NonZeroU32::new)
            .ok_or(StorageError::ActionAttemptConflict)?;
        return Ok(Some(ActionStartOutcome::Started(StartedAction::new(
            claim,
            request.clone(),
            grouped,
            attempt_number,
            started_at,
        ))));
    }
    if raw.0 == ActionLifecycleState::Suppressed.as_str()
        && raw.1 == generation
        && raw.2.is_none()
        && raw.3.is_none()
        && raw.4.is_none()
        && raw.5.as_deref() == Some(started_at_text.as_str())
        && raw.6.as_deref() == Some(ActionSuppressionReason::NotificationsDisabled.as_str())
    {
        let attempt_count = transaction.query_row(
            "SELECT count(*) FROM action_attempts WHERE action_id = ?1",
            [claim.action_id().to_string()],
            |row| row.get::<_, i64>(0),
        )?;
        if attempt_count != 0 {
            return Err(StorageError::ActionAttemptConflict);
        }
        return Ok(Some(ActionStartOutcome::Suppressed(ActionFinalization {
            action_id: claim.action_id(),
            state: ActionLifecycleState::Suppressed,
            completed_at: started_at,
            error_code: None,
            suppression_reason: Some(ActionSuppressionReason::NotificationsDisabled),
        })));
    }
    Ok(None)
}

fn validate_pending_claim(
    transaction: &Transaction<'_>,
    claim: ActionClaimToken,
    lease_expires_at: UtcTimestamp,
    started_at: UtcTimestamp,
) -> StorageResult<()> {
    let valid = transaction.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM action_outbox
            WHERE id = ?1 AND state = 'pending' AND lease_owner = ?2
              AND lease_generation = ?3 AND lease_expires_at = ?4
              AND lease_expires_at > ?5
         )",
        params![
            claim.action_id().to_string(),
            claim.lease_owner().as_uuid().to_string(),
            lease_generation_integer(claim.lease_generation())?,
            timestamp_text(lease_expires_at),
            timestamp_text(started_at),
        ],
        |row| row.get::<_, bool>(0),
    )?;
    if !valid {
        return Err(StorageError::ActionClaimLost);
    }
    Ok(())
}

fn next_attempt_number(
    transaction: &Transaction<'_>,
    action_id: ActionId,
) -> StorageResult<NonZeroU32> {
    let maximum = transaction.query_row(
        "SELECT max(attempt_number) FROM action_attempts WHERE action_id = ?1",
        [action_id.to_string()],
        |row| row.get::<_, Option<i64>>(0),
    )?;
    let next = maximum
        .unwrap_or(0)
        .checked_add(1)
        .ok_or(StorageError::InvalidStoredValue {
            field: "action_attempt.attempt_number",
        })?;
    u32::try_from(next)
        .ok()
        .and_then(NonZeroU32::new)
        .ok_or(StorageError::InvalidStoredValue {
            field: "action_attempt.attempt_number",
        })
}

pub(super) fn finish_action(
    connection: &mut Connection,
    started: &StartedAction,
    result: ActionExecutionResult,
    completed_at: UtcTimestamp,
) -> StorageResult<ActionTransition<ActionFinalization>> {
    if completed_at < started.started_at() {
        return Err(StorageError::InvalidActionTransition);
    }
    let (state, attempt_state, error_code) = match result {
        ActionExecutionResult::Completed => (ActionLifecycleState::Completed, "completed", None),
        ActionExecutionResult::Failed(error_code) => {
            (ActionLifecycleState::Failed, "failed", Some(error_code))
        }
        ActionExecutionResult::Unavailable(error_code) => (
            ActionLifecycleState::Unavailable,
            "unavailable",
            Some(error_code),
        ),
    };
    let claim = started.token();
    let completed_at_text = timestamp_text(completed_at);
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let attempt_changed = transaction.execute(
        "UPDATE action_attempts
         SET state = ?1, completed_at = ?2, safe_error_code = ?3
         WHERE action_id = ?4 AND attempt_number = ?5 AND lease_generation = ?6
           AND state = 'started' AND completed_at IS NULL AND safe_error_code IS NULL",
        params![
            attempt_state,
            completed_at_text,
            error_code.map(ActionErrorCode::as_str),
            claim.action_id().to_string(),
            started.attempt_number().get(),
            lease_generation_integer(claim.lease_generation())?,
        ],
    )?;
    if attempt_changed != 1 {
        if terminal_replay_matches(
            &transaction,
            started,
            state,
            attempt_state,
            error_code,
            &completed_at_text,
        )? {
            transaction.commit()?;
            return Ok(ActionTransition::replayed(ActionFinalization {
                action_id: claim.action_id(),
                state,
                completed_at,
                error_code,
                suppression_reason: None,
            }));
        }
        return Err(StorageError::ActionAttemptConflict);
    }
    let action_changed = transaction.execute(
        "UPDATE action_outbox
         SET state = ?1, lease_owner = NULL, lease_expires_at = NULL,
             completed_at = ?2, last_error_code = ?3
         WHERE id = ?4 AND state = 'started' AND lease_owner = ?5
           AND lease_generation = ?6 AND started_at = ?7",
        params![
            state.as_str(),
            completed_at_text,
            error_code.map(ActionErrorCode::as_str),
            claim.action_id().to_string(),
            claim.lease_owner().as_uuid().to_string(),
            lease_generation_integer(claim.lease_generation())?,
            timestamp_text(started.started_at()),
        ],
    )?;
    if action_changed != 1 {
        return Err(StorageError::ActionClaimLost);
    }
    transaction.commit()?;
    Ok(ActionTransition::committed(ActionFinalization {
        action_id: claim.action_id(),
        state,
        completed_at,
        error_code,
        suppression_reason: None,
    }))
}

fn terminal_replay_matches(
    transaction: &Transaction<'_>,
    started: &StartedAction,
    state: ActionLifecycleState,
    attempt_state: &str,
    error_code: Option<ActionErrorCode>,
    completed_at: &str,
) -> StorageResult<bool> {
    let claim = started.token();
    transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1
                FROM action_outbox AS action
                INNER JOIN action_attempts AS attempt
                    ON attempt.action_id = action.id
                WHERE action.id = ?1
                  AND action.state = ?2
                  AND action.lease_generation = ?3
                  AND action.lease_owner IS NULL
                  AND action.lease_expires_at IS NULL
                  AND action.started_at = ?4
                  AND action.completed_at = ?5
                  AND action.last_error_code IS ?6
                  AND attempt.attempt_number = ?7
                  AND attempt.lease_generation = ?3
                  AND attempt.state = ?8
                  AND attempt.started_at = ?4
                  AND attempt.completed_at = ?5
                  AND attempt.safe_error_code IS ?6
             )",
            params![
                claim.action_id().to_string(),
                state.as_str(),
                lease_generation_integer(claim.lease_generation())?,
                timestamp_text(started.started_at()),
                completed_at,
                error_code.map(ActionErrorCode::as_str),
                started.attempt_number().get(),
                attempt_state,
            ],
            |row| row.get(0),
        )
        .map_err(StorageError::from)
}

struct StartedRecoveryRow {
    raw_action_id: String,
    lease_generation: i64,
    action_started_at: String,
    attempt_number: i64,
    completed_at: String,
}

pub(super) fn recover_started_actions(
    connection: &mut Connection,
    request: ActionRecoveryRequest,
) -> StorageResult<ActionRecoveryOutcome> {
    let batch_size = usize::try_from(request.batch_size().get()).map_err(|_| {
        StorageError::InvalidActionRecoveryBatch {
            max: super::MAX_ACTION_RECOVERY_BATCH_SIZE,
        }
    })?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut rows = load_started_recovery_rows(
        &transaction,
        request.recovered_at(),
        i64::from(request.batch_size().get()) + 1,
    )?;
    let more_remaining = rows.len() > batch_size;
    rows.truncate(batch_size);
    for row in &rows {
        recover_started_action(&transaction, row)?;
    }
    let recovered_actions =
        u32::try_from(rows.len()).map_err(|_| StorageError::InvalidActionRecoveryBatch {
            max: super::MAX_ACTION_RECOVERY_BATCH_SIZE,
        })?;
    transaction.commit()?;
    Ok(ActionRecoveryOutcome {
        recovered_actions,
        more_remaining,
    })
}

fn load_started_recovery_rows(
    transaction: &Transaction<'_>,
    recovered_at: UtcTimestamp,
    limit: i64,
) -> StorageResult<Vec<StartedRecoveryRow>> {
    let raw = {
        let mut statement = transaction.prepare(
            "SELECT action.id, action.lease_generation, action.started_at,
                    attempt.attempt_number, attempt.state, attempt.started_at
             FROM action_outbox AS action
             LEFT JOIN action_attempts AS attempt
               ON attempt.action_id = action.id
              AND attempt.lease_generation = action.lease_generation
             WHERE action.state = 'started'
             ORDER BY action.started_at, action.id
             LIMIT ?1",
        )?;
        statement
            .query_map([limit], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    raw.into_iter()
        .map(|row| validate_started_recovery_row(row, recovered_at))
        .collect()
}

fn validate_started_recovery_row(
    row: (
        String,
        i64,
        String,
        Option<i64>,
        Option<String>,
        Option<String>,
    ),
    recovered_at: UtcTimestamp,
) -> StorageResult<StartedRecoveryRow> {
    ActionId::from_str(&row.0).map_err(|_| StorageError::InvalidStoredValue {
        field: "action_outbox.id",
    })?;
    let lease_generation = u64::try_from(row.1)
        .ok()
        .and_then(NonZeroU64::new)
        .ok_or(StorageError::ActionRecoveryConflict)?;
    let action_started_at = parse_timestamp(&row.2, "action_outbox.started_at")?;
    if row.4.as_deref() != Some("started") || row.5.as_deref() != Some(row.2.as_str()) {
        return Err(StorageError::ActionRecoveryConflict);
    }
    let attempt_number = row
        .3
        .and_then(|value| u32::try_from(value).ok())
        .and_then(NonZeroU32::new)
        .ok_or(StorageError::ActionRecoveryConflict)?;
    Ok(StartedRecoveryRow {
        raw_action_id: row.0,
        lease_generation: lease_generation_integer(lease_generation)?,
        action_started_at: row.2,
        attempt_number: i64::from(attempt_number.get()),
        completed_at: timestamp_text(recovered_at.max(action_started_at)),
    })
}

fn recover_started_action(
    transaction: &Transaction<'_>,
    row: &StartedRecoveryRow,
) -> StorageResult<()> {
    let error_code = ActionErrorCode::IndeterminateAfterCrash.as_str();
    let attempt_changed = transaction.execute(
        "UPDATE action_attempts
         SET state = 'indeterminate', completed_at = ?1, safe_error_code = ?2
         WHERE action_id = ?3 AND attempt_number = ?4 AND lease_generation = ?5
           AND state = 'started' AND started_at = ?6
           AND completed_at IS NULL AND safe_error_code IS NULL",
        params![
            row.completed_at,
            error_code,
            row.raw_action_id,
            row.attempt_number,
            row.lease_generation,
            row.action_started_at,
        ],
    )?;
    if attempt_changed != 1 {
        return Err(StorageError::ActionRecoveryConflict);
    }
    let action_changed = transaction.execute(
        "UPDATE action_outbox
         SET state = 'failed', lease_owner = NULL, lease_expires_at = NULL,
             completed_at = ?1, last_error_code = ?2
         WHERE id = ?3 AND state = 'started' AND lease_generation = ?4
           AND started_at = ?5 AND completed_at IS NULL",
        params![
            row.completed_at,
            error_code,
            row.raw_action_id,
            row.lease_generation,
            row.action_started_at,
        ],
    )?;
    if action_changed != 1 {
        return Err(StorageError::ActionRecoveryConflict);
    }
    Ok(())
}

pub(super) fn load_action(
    connection: &Connection,
    action_id: ActionId,
) -> StorageResult<Option<StoredAction>> {
    let raw = connection
        .query_row(
            "SELECT alert_id, action_type, parameter_schema_version, state,
                    model, reasoning_output_tokens, pattern_n, created_at, not_before,
                    lease_generation, lease_expires_at, started_at, completed_at,
                    last_error_code, suppression_reason
             FROM action_outbox WHERE id = ?1",
            [action_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, Option<String>>(14)?,
                ))
            },
        )
        .optional()?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    let schema_version = parse_schema_version(raw.2)?;
    let registered_action_type = RegisteredActionType::from_str(&raw.1).ok();
    let action_type = registered_action_type.map_or_else(
        || StoredActionType::Unregistered(raw.1.clone()),
        StoredActionType::Registered,
    );
    let parameters = RegisteredActionRegistry::resolve(&raw.1, schema_version.get())
        .ok()
        .and_then(|_| parse_stored_notification_parameters(raw.4, raw.5, raw.6));
    let notification_coalescing = load_notification_coalescing(connection, action_id)?;
    Ok(Some(StoredAction {
        id: action_id,
        alert_id: AlertId::from_str(&raw.0).map_err(|_| StorageError::InvalidStoredValue {
            field: "action_outbox.alert_id",
        })?,
        action_type,
        parameter_schema_version: schema_version,
        state: parse_action_state(&raw.3)?,
        parameters,
        created_at: parse_timestamp(&raw.7, "action_outbox.created_at")?,
        not_before: parse_timestamp(&raw.8, "action_outbox.not_before")?,
        lease_generation: u64::try_from(raw.9).map_err(|_| StorageError::InvalidStoredValue {
            field: "action_outbox.lease_generation",
        })?,
        lease_expires_at: parse_optional_timestamp(raw.10, "action_outbox.lease_expires_at")?,
        started_at: parse_optional_timestamp(raw.11, "action_outbox.started_at")?,
        completed_at: parse_optional_timestamp(raw.12, "action_outbox.completed_at")?,
        error_code: raw
            .13
            .map(|value| parse_action_error_code(&value))
            .transpose()?,
        suppression_reason: raw
            .14
            .map(|value| parse_suppression_reason(&value))
            .transpose()?,
        notification_coalescing,
    }))
}

fn load_notification_coalescing(
    connection: &Connection,
    action_id: ActionId,
) -> StorageResult<Option<StoredNotificationCoalescing>> {
    let raw = connection
        .query_row(
            "SELECT member.window_id, window.conversation_id, window.leader_action_id,
                    member.role, window.opened_at, window.closes_at
             FROM notification_coalescing_members AS member
             INNER JOIN notification_coalescing_windows AS window
                ON window.id = member.window_id
             WHERE member.action_id = ?1",
            [action_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .optional()?;
    let Some((window_id, conversation_id, leader_action_id, role, opened_at, closes_at)) = raw
    else {
        return Ok(None);
    };
    let role = match role.as_str() {
        "leader" => NotificationCoalescingRole::Leader,
        "follower" => NotificationCoalescingRole::Follower,
        _ => {
            return Err(StorageError::InvalidStoredValue {
                field: "notification_coalescing_members.role",
            });
        }
    };
    Ok(Some(StoredNotificationCoalescing {
        window_id: window_id
            .parse()
            .map_err(|_| StorageError::InvalidStoredValue {
                field: "notification_coalescing_windows.id",
            })?,
        conversation_id: conversation_id
            .parse()
            .map_err(|_| StorageError::InvalidStoredValue {
                field: "notification_coalescing_windows.conversation_id",
            })?,
        leader_action_id: leader_action_id
            .map(|value| {
                value.parse().map_err(|_| StorageError::InvalidStoredValue {
                    field: "notification_coalescing_windows.leader_action_id",
                })
            })
            .transpose()?,
        role,
        opened_at: parse_timestamp(&opened_at, "notification_coalescing_windows.opened_at")?,
        closes_at: parse_timestamp(&closes_at, "notification_coalescing_windows.closes_at")?,
    }))
}

fn parse_stored_notification_parameters(
    model: Option<String>,
    reasoning_output_tokens: i64,
    pattern_n: i64,
) -> Option<ActionParameters> {
    let model = match model {
        Some(model) => Some(ModelName::new(model).ok()?),
        None => None,
    };
    Some(ActionParameters::SystemNotification(
        SystemNotificationParams::new(
            model,
            TokenCount::try_from(reasoning_output_tokens).ok()?,
            parse_pattern_n(pattern_n).ok()?,
        ),
    ))
}

pub(super) fn load_action_attempts(
    connection: &Connection,
    action_id: ActionId,
) -> StorageResult<Vec<StoredActionAttempt>> {
    let raw = {
        let mut statement = connection.prepare(
            "SELECT attempt_number, lease_generation, state, started_at,
                    completed_at, safe_error_code
             FROM action_attempts WHERE action_id = ?1 ORDER BY attempt_number",
        )?;
        statement
            .query_map([action_id.to_string()], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    raw.into_iter()
        .map(|row| {
            Ok(StoredActionAttempt {
                action_id,
                attempt_number: u32::try_from(row.0).ok().and_then(NonZeroU32::new).ok_or(
                    StorageError::InvalidStoredValue {
                        field: "action_attempt.attempt_number",
                    },
                )?,
                lease_generation: u64::try_from(row.1).map_err(|_| {
                    StorageError::InvalidStoredValue {
                        field: "action_attempt.lease_generation",
                    }
                })?,
                state: parse_attempt_state(&row.2)?,
                started_at: parse_timestamp(&row.3, "action_attempt.started_at")?,
                completed_at: parse_optional_timestamp(row.4, "action_attempt.completed_at")?,
                error_code: row
                    .5
                    .map(|value| parse_action_error_code(&value))
                    .transpose()?,
            })
        })
        .collect()
}

fn parse_schema_version(value: i64) -> StorageResult<NonZeroU16> {
    u16::try_from(value)
        .ok()
        .and_then(NonZeroU16::new)
        .ok_or(StorageError::InvalidStoredValue {
            field: "action_outbox.parameter_schema_version",
        })
}

fn lease_generation_integer(generation: NonZeroU64) -> StorageResult<i64> {
    i64::try_from(generation.get()).map_err(|_| StorageError::InvalidStoredValue {
        field: "action_outbox.lease_generation",
    })
}

fn parse_pattern_n(value: i64) -> StorageResult<PatternN> {
    let value = u64::try_from(value).ok().and_then(NonZeroU64::new).ok_or(
        StorageError::InvalidStoredValue {
            field: "action_outbox.pattern_n",
        },
    )?;
    Ok(PatternN::new(value))
}

fn parse_optional_timestamp(
    value: Option<String>,
    field: &'static str,
) -> StorageResult<Option<UtcTimestamp>> {
    value
        .map(|value| parse_timestamp(&value, field))
        .transpose()
}

fn parse_action_state(value: &str) -> StorageResult<ActionLifecycleState> {
    match value {
        "pending" => Ok(ActionLifecycleState::Pending),
        "started" => Ok(ActionLifecycleState::Started),
        "completed" => Ok(ActionLifecycleState::Completed),
        "failed" => Ok(ActionLifecycleState::Failed),
        "suppressed" => Ok(ActionLifecycleState::Suppressed),
        "unavailable" => Ok(ActionLifecycleState::Unavailable),
        _ => Err(StorageError::InvalidStoredValue {
            field: "action_outbox.state",
        }),
    }
}

fn parse_action_error_code(value: &str) -> StorageResult<ActionErrorCode> {
    match value {
        "invalid_parameters" => Ok(ActionErrorCode::InvalidParameters),
        "permission_denied" => Ok(ActionErrorCode::PermissionDenied),
        "notification_unavailable" => Ok(ActionErrorCode::NotificationUnavailable),
        "notification_dispatch_failed" => Ok(ActionErrorCode::NotificationDispatchFailed),
        "indeterminate_after_crash" => Ok(ActionErrorCode::IndeterminateAfterCrash),
        "unknown_action_type" => Ok(ActionErrorCode::UnknownActionType),
        "internal_queue_closed" => Ok(ActionErrorCode::InternalQueueClosed),
        _ => Err(StorageError::InvalidStoredValue {
            field: "action_outbox.last_error_code",
        }),
    }
}

fn parse_suppression_reason(value: &str) -> StorageResult<ActionSuppressionReason> {
    match value {
        "notifications_disabled" => Ok(ActionSuppressionReason::NotificationsDisabled),
        "above_threshold" => Ok(ActionSuppressionReason::AboveThreshold),
        "permission_denied" => Ok(ActionSuppressionReason::PermissionDenied),
        "coalesced" => Ok(ActionSuppressionReason::Coalesced),
        "historical_event" => Ok(ActionSuppressionReason::HistoricalEvent),
        "pre_outbox_upgrade" => Ok(ActionSuppressionReason::PreOutboxUpgrade),
        _ => Err(StorageError::InvalidStoredValue {
            field: "action_outbox.suppression_reason",
        }),
    }
}

fn parse_attempt_state(value: &str) -> StorageResult<ActionAttemptState> {
    match value {
        "started" => Ok(ActionAttemptState::Started),
        "completed" => Ok(ActionAttemptState::Completed),
        "failed" => Ok(ActionAttemptState::Failed),
        "unavailable" => Ok(ActionAttemptState::Unavailable),
        "indeterminate" => Ok(ActionAttemptState::Indeterminate),
        _ => Err(StorageError::InvalidStoredValue {
            field: "action_attempt.state",
        }),
    }
}
