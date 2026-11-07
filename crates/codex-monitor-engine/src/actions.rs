//! Durable processing for registered action Outbox requests.

use std::sync::Arc;

use codex_monitor_core::{
    ActionExecutionContext, ActionExecutor, ActionLocale, RegisteredActionType,
    SYSTEM_NOTIFICATION_DEFINITION, UtcTimestamp,
};
use thiserror::Error;

use codex_monitor_storage::{
    ActionClaimOutcome, ActionFinalization, ActionLeaseOwner, ActionLeaseWindow,
    ActionStartOutcome, AppLocale, StorageResult, StorageWriter,
};

/// A terminal or idle result from processing at most one action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionProcessOutcome {
    Idle,
    RejectedUnavailable(ActionFinalization),
    Suppressed(ActionFinalization),
    Finished(ActionFinalization),
}

/// Failure to register a host executor against the closed action definition.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ActionProcessorBuildError {
    #[error("action executor does not exactly match its registered capability contract")]
    ExecutorContractMismatch,
}

/// Platform-independent processor for the transactional action Outbox.
///
/// The processor commits `started` and its attempt before invoking the host
/// executor. If finalization fails after the effect, the durable action remains
/// `started` and is never reclaimed by the normal pending-action path.
#[derive(Clone)]
pub struct ActionProcessor {
    writer: StorageWriter,
    executor: Arc<dyn ActionExecutor>,
    lease_owner: ActionLeaseOwner,
}

impl ActionProcessor {
    /// Bind the one v1 executor after checking its complete registered contract.
    ///
    /// # Errors
    ///
    /// Returns [`ActionProcessorBuildError::ExecutorContractMismatch`] when the
    /// action type, capabilities, or permissions differ from the typed registry.
    pub fn new(
        writer: StorageWriter,
        executor: Arc<dyn ActionExecutor>,
    ) -> Result<Self, ActionProcessorBuildError> {
        let definition = match executor.action_type() {
            RegisteredActionType::SystemNotification => &SYSTEM_NOTIFICATION_DEFINITION,
        };
        if !definition.effectful
            || executor.capabilities() != definition.capabilities
            || executor.permissions() != definition.permissions
        {
            return Err(ActionProcessorBuildError::ExecutorContractMismatch);
        }
        Ok(Self {
            writer,
            executor,
            lease_owner: ActionLeaseOwner::new(),
        })
    }

    /// Return the stable lease-owner identity used by this processor instance.
    #[must_use]
    pub const fn lease_owner(&self) -> ActionLeaseOwner {
        self.lease_owner
    }

    /// Claim, durably start, execute, and durably finish at most one action.
    ///
    /// The supplied window fixes the claim instant and deadline. Actual start
    /// and completion timestamps use the current UTC clock without moving
    /// backwards before the claim or start instant.
    ///
    /// # Errors
    ///
    /// Returns a storage error when claiming, starting, or finalizing cannot be
    /// committed. An error after executor completion intentionally leaves the
    /// action in `started` for the crash-recovery policy.
    pub async fn process_next(
        &self,
        window: ActionLeaseWindow,
        locale: AppLocale,
    ) -> StorageResult<ActionProcessOutcome> {
        let claimed = match self.writer.claim_action(self.lease_owner, window).await? {
            ActionClaimOutcome::Idle => return Ok(ActionProcessOutcome::Idle),
            ActionClaimOutcome::RejectedUnavailable(finalization) => {
                return Ok(ActionProcessOutcome::RejectedUnavailable(finalization));
            }
            ActionClaimOutcome::Claimed(claimed) => claimed,
        };

        let started_at = UtcTimestamp::now().max(window.claimed_at());
        let started = match self.writer.start_action(claimed, started_at).await? {
            ActionStartOutcome::Suppressed(finalization) => {
                return Ok(ActionProcessOutcome::Suppressed(finalization));
            }
            ActionStartOutcome::Started(started) => started,
        };

        let context = ActionExecutionContext::new(
            match locale {
                AppLocale::EnUs => ActionLocale::EnUs,
                AppLocale::ZhCn => ActionLocale::ZhCn,
            },
            started.grouped(),
        );
        let result = self
            .executor
            .execute(&started.request().parameters, context)
            .await;
        let completed_at = UtcTimestamp::now().max(started_at);
        let finalization = self
            .writer
            .finish_action(started, result, completed_at)
            .await?;
        Ok(ActionProcessOutcome::Finished(finalization))
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use codex_monitor_core::{
        ActionCapability, ActionErrorCode, ActionExecutionContext, ActionExecutionResult,
        ActionExecutor, ActionFuture, ActionId, ActionLifecycleState, ActionParameters,
        ActionPermission, ActionSuppressionReason, AlertId, ModelCallId, RegisteredActionType,
        SYSTEM_NOTIFICATION_DEFINITION,
    };
    use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
    use rusqlite::{Connection, params};
    use tempfile::tempdir;

    use crate::rules::{RuleWorkProcessOutcome, RuleWorkProcessor};
    use codex_monitor_storage::{
        ActionAttemptState, ActionClaimOutcome, ActionLeaseOwner, ActionLeaseWindow,
        ActionRecoveryRequest, ActionStartOutcome, MAX_ACTION_RECOVERY_BATCH_SIZE, ModelCallQuery,
        StartedAction, StorageChange, StorageConfig, StorageError, StorageReader,
        StoredSourceHealth, StreamReceive, StreamSubscriptionStart,
    };
    use codex_monitor_telemetry::{OtlpLogSource, TelemetrySource as _};

    use super::*;

    fn at(seconds: i128) -> UtcTimestamp {
        UtcTimestamp::from_unix_timestamp_nanos(seconds * 1_000_000_000).unwrap()
    }

    fn seconds_after(timestamp: UtcTimestamp, seconds: i128) -> UtcTimestamp {
        UtcTimestamp::from_unix_timestamp_nanos(
            timestamp.unix_timestamp_nanos() + seconds * 1_000_000_000,
        )
        .unwrap()
    }

    fn fixture_with_reasoning_tokens_and_response_id(
        tokens: u64,
        response_id: &str,
    ) -> ExportLogsServiceRequest {
        let fixture = include_str!("../../../fixtures/otlp/response_completed.json");
        let needle = "\\\"reasoning_tokens\\\":516";
        assert!(fixture.contains(needle));
        assert!(fixture.contains("synthetic-response-0001"));
        let fixture = fixture
            .replace(needle, &format!("\\\"reasoning_tokens\\\":{tokens}"))
            .replace("synthetic-response-0001", response_id)
            .replace("\"conversation.id\"", "\"synthetic.unknown_conversation\"")
            .replace("\"thread.id\"", "\"synthetic.unknown_thread\"");
        serde_json::from_str(&fixture).unwrap()
    }

    fn healthy_snapshot(source: &OtlpLogSource, accepted_at: UtcTimestamp) -> StoredSourceHealth {
        let mut accepted = source.clone();
        accepted.mark_committed(accepted_at);
        StoredSourceHealth::from(accepted.health())
    }

    async fn create_alert(
        writer: &StorageWriter,
        database: &Path,
        reasoning_tokens: u64,
    ) -> (ModelCallId, AlertId, Option<ActionId>) {
        create_alert_with_response_id(
            writer,
            database,
            reasoning_tokens,
            "synthetic-response-0001",
        )
        .await
    }

    async fn create_alert_with_response_id(
        writer: &StorageWriter,
        database: &Path,
        reasoning_tokens: u64,
        response_id: &str,
    ) -> (ModelCallId, AlertId, Option<ActionId>) {
        let processor = RuleWorkProcessor::new(writer.clone()).await.unwrap();
        let source = OtlpLogSource::new(at(1)).unwrap();
        let committed = writer
            .commit_source_batch(
                source.extract(
                    fixture_with_reasoning_tokens_and_response_id(reasoning_tokens, response_id),
                    at(2),
                ),
                healthy_snapshot(&source, at(2)),
            )
            .await
            .unwrap();
        assert_eq!(committed.inserted_observations, 1);
        assert_eq!(committed.downstream_work_enqueued, 1);
        let model_call_id = StorageReader::new(database.to_path_buf())
            .model_calls(ModelCallQuery::default())
            .unwrap()[0]
            .0
            .id();
        let RuleWorkProcessOutcome::Completed { completion, .. } =
            processor.process_next(at(10), at(9)).await.unwrap()
        else {
            panic!("synthetic rule work must complete");
        };
        assert_eq!(completion.inserted_alerts.len(), 1);
        let decision = completion.inserted_alerts[0];
        (
            model_call_id,
            decision.alert_id,
            decision.action.map(|action| action.action_id),
        )
    }

    async fn create_pending_action(
        writer: &StorageWriter,
        database: &Path,
    ) -> (ModelCallId, AlertId, ActionId) {
        let (model_call_id, alert_id, action_id) = create_alert(writer, database, 516).await;
        (model_call_id, alert_id, action_id.unwrap())
    }

    async fn create_pending_action_with_response_id(
        writer: &StorageWriter,
        database: &Path,
        response_id: &str,
    ) -> (ModelCallId, AlertId, ActionId) {
        let (model_call_id, alert_id, action_id) =
            create_alert_with_response_id(writer, database, 516, response_id).await;
        (model_call_id, alert_id, action_id.unwrap())
    }

    async fn claim(
        writer: &StorageWriter,
        owner: ActionLeaseOwner,
        claimed_at: UtcTimestamp,
        expires_at: UtcTimestamp,
    ) -> codex_monitor_storage::ClaimedAction {
        let ActionClaimOutcome::Claimed(claimed) = writer
            .claim_action(
                owner,
                ActionLeaseWindow::new(claimed_at, expires_at).unwrap(),
            )
            .await
            .unwrap()
        else {
            panic!("one pending action must be claimable");
        };
        claimed
    }

    async fn create_started_action_with_response_id(
        writer: &StorageWriter,
        database: &Path,
        response_id: &str,
        claimed_at: UtcTimestamp,
        expires_at: UtcTimestamp,
        started_at: UtcTimestamp,
    ) -> (ActionId, StartedAction) {
        let (_, _, action_id) =
            create_pending_action_with_response_id(writer, database, response_id).await;
        let claimed = claim(writer, ActionLeaseOwner::new(), claimed_at, expires_at).await;
        let ActionStartOutcome::Started(started) =
            writer.start_action(claimed, started_at).await.unwrap()
        else {
            panic!("enabled action must start");
        };
        (action_id, started)
    }

    struct FakeExecutor {
        result: ActionExecutionResult,
        capabilities: &'static [ActionCapability],
        permissions: &'static [ActionPermission],
        effect_count: AtomicUsize,
        contexts: Mutex<Vec<ActionExecutionContext>>,
    }

    impl FakeExecutor {
        fn valid(result: ActionExecutionResult) -> Self {
            Self {
                result,
                capabilities: SYSTEM_NOTIFICATION_DEFINITION.capabilities,
                permissions: SYSTEM_NOTIFICATION_DEFINITION.permissions,
                effect_count: AtomicUsize::new(0),
                contexts: Mutex::new(Vec::new()),
            }
        }

        fn effect_count(&self) -> usize {
            self.effect_count.load(Ordering::SeqCst)
        }

        fn contexts(&self) -> Vec<ActionExecutionContext> {
            self.contexts.lock().unwrap().clone()
        }
    }

    impl ActionExecutor for FakeExecutor {
        fn action_type(&self) -> RegisteredActionType {
            RegisteredActionType::SystemNotification
        }

        fn capabilities(&self) -> &'static [ActionCapability] {
            self.capabilities
        }

        fn permissions(&self) -> &'static [ActionPermission] {
            self.permissions
        }

        fn execute<'a>(
            &'a self,
            parameters: &'a ActionParameters,
            context: ActionExecutionContext,
        ) -> ActionFuture<'a> {
            assert_eq!(
                parameters.action_type(),
                RegisteredActionType::SystemNotification
            );
            self.effect_count.fetch_add(1, Ordering::SeqCst);
            self.contexts.lock().unwrap().push(context);
            Box::pin(async move { self.result })
        }
    }

    #[tokio::test]
    async fn pending_action_survives_writer_restart_and_is_claimable() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("action-restart.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let (_, _, action_id) = create_pending_action(&writer, &database).await;
        writer.shutdown().await.unwrap();

        let reopened = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let claimed = claim(&reopened, ActionLeaseOwner::new(), at(11), at(20)).await;
        assert_eq!(claimed.request().id, action_id);
        assert_eq!(claimed.token().lease_generation(), NonZeroU64::MIN);
        let stored = StorageReader::new(database)
            .action(action_id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.state, ActionLifecycleState::Pending);
        assert_eq!(stored.lease_generation, 1);
        assert_eq!(stored.lease_expires_at, Some(at(20)));
        reopened.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn started_action_recovery_after_restart_is_indeterminate_and_idempotent() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("action-started-recovery.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let (action_id, _started) = create_started_action_with_response_id(
            &writer,
            &database,
            "synthetic-response-recovery-restart",
            at(11),
            at(100),
            at(12),
        )
        .await;
        writer.shutdown().await.unwrap();

        let reopened = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let request = ActionRecoveryRequest::new(at(13), 10).unwrap();
        let recovered = reopened.recover_started_actions(request).await.unwrap();
        assert_eq!(recovered.recovered_actions, 1);
        assert!(!recovered.more_remaining);
        let replay = reopened.recover_started_actions(request).await.unwrap();
        assert_eq!(replay.recovered_actions, 0);
        assert!(!replay.more_remaining);

        let reader = StorageReader::new(database.clone());
        let action = reader.action(action_id).unwrap().unwrap();
        assert_eq!(action.state, ActionLifecycleState::Failed);
        assert_eq!(action.started_at, Some(at(12)));
        assert_eq!(action.completed_at, Some(at(13)));
        assert_eq!(action.lease_generation, 1);
        assert_eq!(
            action.error_code,
            Some(ActionErrorCode::IndeterminateAfterCrash)
        );
        assert_eq!(action.lease_expires_at, None);
        let attempts = reader.action_attempts(action_id).unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].state, ActionAttemptState::Indeterminate);
        assert_eq!(attempts[0].lease_generation, 1);
        assert_eq!(attempts[0].started_at, at(12));
        assert_eq!(attempts[0].completed_at, Some(at(13)));
        assert_eq!(
            attempts[0].error_code,
            Some(ActionErrorCode::IndeterminateAfterCrash)
        );
        let lease: (Option<String>, Option<String>) = Connection::open(&database)
            .unwrap()
            .query_row(
                "SELECT lease_owner, lease_expires_at FROM action_outbox WHERE id = ?1",
                [action_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(lease, (None, None));

        let executor = Arc::new(FakeExecutor::valid(ActionExecutionResult::Completed));
        let processor = ActionProcessor::new(reopened.clone(), executor.clone()).unwrap();
        let outcome = processor
            .process_next(
                ActionLeaseWindow::new(at(14), at(20)).unwrap(),
                AppLocale::EnUs,
            )
            .await
            .unwrap();
        assert_eq!(outcome, ActionProcessOutcome::Idle);
        assert_eq!(executor.effect_count(), 0);
        drop(processor);
        reopened.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn recovery_handles_equal_and_future_started_timestamps_without_time_regression() {
        let cases = [
            ("equal", at(12), at(12), at(12), at(100)),
            ("clock-rollback", at(13), at(12), at(13), at(100)),
            ("expired-lease", at(12), at(20), at(20), at(13)),
        ];

        for (name, started_at, restarted_at, expected_completed_at, expires_at) in cases {
            let directory = tempdir().unwrap();
            let database = directory.path().join(format!("recovery-{name}.sqlite3"));
            let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
            let (action_id, _started) = create_started_action_with_response_id(
                &writer,
                &database,
                &format!("synthetic-response-recovery-{name}"),
                at(11),
                expires_at,
                started_at,
            )
            .await;
            writer.shutdown().await.unwrap();

            let reopened = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
            let recovered = reopened
                .recover_started_actions(ActionRecoveryRequest::new(restarted_at, 10).unwrap())
                .await
                .unwrap();
            assert_eq!(recovered.recovered_actions, 1, "{name}");
            assert!(!recovered.more_remaining, "{name}");

            let reader = StorageReader::new(database);
            let action = reader.action(action_id).unwrap().unwrap();
            assert_eq!(action.state, ActionLifecycleState::Failed, "{name}");
            assert_eq!(action.completed_at, Some(expected_completed_at), "{name}");
            assert_eq!(
                action.error_code,
                Some(ActionErrorCode::IndeterminateAfterCrash),
                "{name}"
            );
            let attempts = reader.action_attempts(action_id).unwrap();
            assert_eq!(attempts.len(), 1, "{name}");
            assert_eq!(
                attempts[0].state,
                ActionAttemptState::Indeterminate,
                "{name}"
            );
            assert_eq!(
                attempts[0].completed_at,
                Some(expected_completed_at),
                "{name}"
            );
            reopened.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn recovery_and_finish_are_serialized_without_overwriting_the_winner() {
        let directory = tempdir().unwrap();
        let recovery_database = directory.path().join("recovery-wins.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(recovery_database.clone())).unwrap();
        let (recovered_action_id, stale_started) = create_started_action_with_response_id(
            &writer,
            &recovery_database,
            "synthetic-response-recovery-wins",
            at(11),
            at(100),
            at(12),
        )
        .await;
        writer.shutdown().await.unwrap();

        let reopened = StorageWriter::open(StorageConfig::new(recovery_database.clone())).unwrap();
        let recovered = reopened
            .recover_started_actions(ActionRecoveryRequest::new(at(13), 10).unwrap())
            .await
            .unwrap();
        assert_eq!(recovered.recovered_actions, 1);
        assert!(matches!(
            reopened
                .finish_action(stale_started, ActionExecutionResult::Completed, at(14))
                .await,
            Err(StorageError::ActionAttemptConflict)
        ));
        let reader = StorageReader::new(recovery_database);
        let action = reader.action(recovered_action_id).unwrap().unwrap();
        assert_eq!(action.state, ActionLifecycleState::Failed);
        assert_eq!(action.completed_at, Some(at(13)));
        assert_eq!(
            action.error_code,
            Some(ActionErrorCode::IndeterminateAfterCrash)
        );
        let attempts = reader.action_attempts(recovered_action_id).unwrap();
        assert_eq!(attempts[0].state, ActionAttemptState::Indeterminate);
        assert_eq!(attempts[0].completed_at, Some(at(13)));
        reopened.shutdown().await.unwrap();

        let finish_database = directory.path().join("finish-wins.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(finish_database.clone())).unwrap();
        let (completed_action_id, started) = create_started_action_with_response_id(
            &writer,
            &finish_database,
            "synthetic-response-finish-wins",
            at(11),
            at(100),
            at(12),
        )
        .await;
        writer
            .finish_action(started, ActionExecutionResult::Completed, at(13))
            .await
            .unwrap();
        writer.shutdown().await.unwrap();

        let reopened = StorageWriter::open(StorageConfig::new(finish_database.clone())).unwrap();
        let skipped = reopened
            .recover_started_actions(ActionRecoveryRequest::new(at(14), 10).unwrap())
            .await
            .unwrap();
        assert_eq!(skipped.recovered_actions, 0);
        assert!(!skipped.more_remaining);
        let reader = StorageReader::new(finish_database);
        let action = reader.action(completed_action_id).unwrap().unwrap();
        assert_eq!(action.state, ActionLifecycleState::Completed);
        assert_eq!(action.completed_at, Some(at(13)));
        assert_eq!(action.error_code, None);
        let attempts = reader.action_attempts(completed_action_id).unwrap();
        assert_eq!(attempts[0].state, ActionAttemptState::Completed);
        assert_eq!(attempts[0].completed_at, Some(at(13)));
        reopened.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn recovery_action_failure_rolls_back_attempt_and_can_be_retried() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("recovery-rollback.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let (action_id, _started) = create_started_action_with_response_id(
            &writer,
            &database,
            "synthetic-response-recovery-rollback",
            at(11),
            at(100),
            at(12),
        )
        .await;
        writer.shutdown().await.unwrap();

        let failure_injector = Connection::open(&database).unwrap();
        failure_injector
            .execute_batch(
                "CREATE TRIGGER test_abort_started_action_recovery
                 BEFORE UPDATE OF state ON action_outbox
                 WHEN OLD.state = 'started'
                  AND NEW.state = 'failed'
                  AND NEW.last_error_code = 'indeterminate_after_crash'
                 BEGIN
                     SELECT RAISE(ABORT, 'injected started action recovery failure');
                 END;",
            )
            .unwrap();
        let reopened = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let request = ActionRecoveryRequest::new(at(13), 10).unwrap();

        assert!(matches!(
            reopened.recover_started_actions(request).await,
            Err(StorageError::Database(_))
        ));
        let reader = StorageReader::new(database.clone());
        let action = reader.action(action_id).unwrap().unwrap();
        assert_eq!(action.state, ActionLifecycleState::Started);
        assert_eq!(action.completed_at, None);
        assert_eq!(action.error_code, None);
        assert_eq!(action.lease_expires_at, Some(at(100)));
        let attempts = reader.action_attempts(action_id).unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].state, ActionAttemptState::Started);
        assert_eq!(attempts[0].completed_at, None);
        assert_eq!(attempts[0].error_code, None);

        failure_injector
            .execute_batch("DROP TRIGGER test_abort_started_action_recovery;")
            .unwrap();
        let recovered = reopened.recover_started_actions(request).await.unwrap();
        assert_eq!(recovered.recovered_actions, 1);
        assert!(!recovered.more_remaining);
        let action = reader.action(action_id).unwrap().unwrap();
        assert_eq!(action.state, ActionLifecycleState::Failed);
        assert_eq!(action.completed_at, Some(at(13)));
        let attempts = reader.action_attempts(action_id).unwrap();
        assert_eq!(attempts[0].state, ActionAttemptState::Indeterminate);
        assert_eq!(attempts[0].completed_at, Some(at(13)));
        drop(failure_injector);
        reopened.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn invalid_started_attempt_aborts_the_whole_recovery_batch() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("recovery-invalid-attempt.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let (valid_action_id, _valid_started) = create_started_action_with_response_id(
            &writer,
            &database,
            "synthetic-response-recovery-valid-attempt",
            at(11),
            at(100),
            at(12),
        )
        .await;
        let (invalid_action_id, _invalid_started) = create_started_action_with_response_id(
            &writer,
            &database,
            "synthetic-response-recovery-invalid-attempt",
            at(13),
            at(100),
            at(14),
        )
        .await;
        writer.shutdown().await.unwrap();
        Connection::open(&database)
            .unwrap()
            .execute(
                "DELETE FROM action_attempts WHERE action_id = ?1",
                [invalid_action_id.to_string()],
            )
            .unwrap();

        let reopened = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        assert!(matches!(
            reopened
                .recover_started_actions(ActionRecoveryRequest::new(at(20), 10).unwrap())
                .await,
            Err(StorageError::ActionRecoveryConflict)
        ));
        let reader = StorageReader::new(database);
        let valid_action = reader.action(valid_action_id).unwrap().unwrap();
        assert_eq!(valid_action.state, ActionLifecycleState::Started);
        assert_eq!(valid_action.completed_at, None);
        let valid_attempts = reader.action_attempts(valid_action_id).unwrap();
        assert_eq!(valid_attempts.len(), 1);
        assert_eq!(valid_attempts[0].state, ActionAttemptState::Started);
        assert_eq!(valid_attempts[0].completed_at, None);
        let invalid_action = reader.action(invalid_action_id).unwrap().unwrap();
        assert_eq!(invalid_action.state, ActionLifecycleState::Started);
        assert_eq!(invalid_action.completed_at, None);
        assert!(
            reader
                .action_attempts(invalid_action_id)
                .unwrap()
                .is_empty()
        );
        reopened.shutdown().await.unwrap();
    }

    #[test]
    fn action_recovery_batch_size_is_strictly_bounded() {
        for batch_size in [0, MAX_ACTION_RECOVERY_BATCH_SIZE + 1] {
            assert!(matches!(
                ActionRecoveryRequest::new(at(1), batch_size),
                Err(StorageError::InvalidActionRecoveryBatch {
                    max: MAX_ACTION_RECOVERY_BATCH_SIZE
                })
            ));
        }
        assert!(ActionRecoveryRequest::new(at(1), 1).is_ok());
        assert!(ActionRecoveryRequest::new(at(1), MAX_ACTION_RECOVERY_BATCH_SIZE).is_ok());
    }

    #[tokio::test]
    async fn recovery_batch_reports_more_remaining_and_drains_started_actions() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("recovery-batch.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let (first_action_id, _first_started) = create_started_action_with_response_id(
            &writer,
            &database,
            "synthetic-response-recovery-batch-first",
            at(11),
            at(100),
            at(12),
        )
        .await;
        let (second_action_id, _second_started) = create_started_action_with_response_id(
            &writer,
            &database,
            "synthetic-response-recovery-batch-second",
            at(13),
            at(100),
            at(14),
        )
        .await;
        let (_, _, pending_action_id) = create_pending_action_with_response_id(
            &writer,
            &database,
            "synthetic-response-recovery-batch-pending",
        )
        .await;
        writer.shutdown().await.unwrap();

        let reopened = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let request = ActionRecoveryRequest::new(at(20), 1).unwrap();
        let first_batch = reopened.recover_started_actions(request).await.unwrap();
        assert_eq!(first_batch.recovered_actions, 1);
        assert!(first_batch.more_remaining);
        let reader = StorageReader::new(database.clone());
        assert_eq!(
            reader.action(first_action_id).unwrap().unwrap().state,
            ActionLifecycleState::Failed
        );
        assert_eq!(
            reader.action(second_action_id).unwrap().unwrap().state,
            ActionLifecycleState::Started
        );
        assert_eq!(
            reader.action(pending_action_id).unwrap().unwrap().state,
            ActionLifecycleState::Pending
        );
        assert_eq!(
            reader.action_attempts(first_action_id).unwrap()[0].state,
            ActionAttemptState::Indeterminate
        );
        assert_eq!(
            reader.action_attempts(second_action_id).unwrap()[0].state,
            ActionAttemptState::Started
        );

        let second_batch = reopened.recover_started_actions(request).await.unwrap();
        assert_eq!(second_batch.recovered_actions, 1);
        assert!(!second_batch.more_remaining);
        assert_eq!(
            reader.action(second_action_id).unwrap().unwrap().state,
            ActionLifecycleState::Failed
        );
        assert_eq!(
            reader.action_attempts(second_action_id).unwrap()[0].state,
            ActionAttemptState::Indeterminate
        );

        let drained = reopened.recover_started_actions(request).await.unwrap();
        assert_eq!(drained.recovered_actions, 0);
        assert!(!drained.more_remaining);
        assert_eq!(
            reader.action(pending_action_id).unwrap().unwrap().state,
            ActionLifecycleState::Pending
        );
        assert!(
            reader
                .action_attempts(pending_action_id)
                .unwrap()
                .is_empty()
        );
        reopened.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn pending_lease_is_exclusive_then_reclaimed_with_a_fencing_generation() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("action-lease.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let (_, _, action_id) = create_pending_action(&writer, &database).await;
        let first = claim(&writer, ActionLeaseOwner::new(), at(11), at(20)).await;

        assert_eq!(
            writer
                .claim_action(
                    ActionLeaseOwner::new(),
                    ActionLeaseWindow::new(at(12), at(21)).unwrap(),
                )
                .await
                .unwrap(),
            ActionClaimOutcome::Idle
        );

        let second = claim(&writer, ActionLeaseOwner::new(), at(21), at(30)).await;
        assert_eq!(second.request().id, action_id);
        assert_eq!(first.token().lease_generation().get(), 1);
        assert_eq!(second.token().lease_generation().get(), 2);
        assert!(matches!(
            writer.release_action_claim(first.token()).await,
            Err(StorageError::ActionClaimLost)
        ));
        assert!(matches!(
            writer.start_action(first, at(12)).await,
            Err(StorageError::ActionClaimLost)
        ));

        let reader = StorageReader::new(database);
        let stored = reader.action(action_id).unwrap().unwrap();
        assert_eq!(stored.state, ActionLifecycleState::Pending);
        assert_eq!(stored.lease_generation, 2);
        assert_eq!(stored.lease_expires_at, Some(at(30)));
        assert!(reader.action_attempts(action_id).unwrap().is_empty());
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn started_attempt_and_completed_terminal_state_are_auditable() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("action-completed.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let (model_call_id, alert_id, action_id) = create_pending_action(&writer, &database).await;
        let reader = StorageReader::new(database);

        let pending = reader.action(action_id).unwrap().unwrap();
        assert_eq!(pending.id, action_id);
        assert_eq!(pending.alert_id, alert_id);
        assert_eq!(
            pending.action_type,
            RegisteredActionType::SystemNotification
        );
        assert_eq!(
            pending.parameter_schema_version,
            SYSTEM_NOTIFICATION_DEFINITION.schema_version
        );
        assert_eq!(pending.created_at, at(10));
        assert_eq!(pending.not_before, at(10));
        assert_eq!(pending.state, ActionLifecycleState::Pending);
        let Some(ActionParameters::SystemNotification(parameters)) = &pending.parameters else {
            panic!("registered action parameters must remain readable");
        };
        assert_eq!(parameters.reasoning_output_tokens().get(), 516);
        assert_eq!(parameters.pattern_n().get(), 1);
        let by_alert = reader.actions_for_alert(alert_id).unwrap();
        assert_eq!(by_alert, [pending]);

        let claimed = claim(&writer, ActionLeaseOwner::new(), at(11), at(20)).await;
        assert_eq!(claimed.request().model_call_id, model_call_id);
        let ActionStartOutcome::Started(started) =
            writer.start_action(claimed, at(12)).await.unwrap()
        else {
            panic!("enabled action must start");
        };
        let durable_started = reader.action(action_id).unwrap().unwrap();
        assert_eq!(durable_started.state, ActionLifecycleState::Started);
        assert_eq!(durable_started.started_at, Some(at(12)));
        assert_eq!(durable_started.completed_at, None);
        assert_eq!(durable_started.lease_generation, 1);
        let started_attempts = reader.action_attempts(action_id).unwrap();
        assert_eq!(started_attempts.len(), 1);
        assert_eq!(started_attempts[0].action_id, action_id);
        assert_eq!(started_attempts[0].attempt_number.get(), 1);
        assert_eq!(started_attempts[0].lease_generation, 1);
        assert_eq!(started_attempts[0].state, ActionAttemptState::Started);
        assert_eq!(started_attempts[0].started_at, at(12));
        assert_eq!(started_attempts[0].completed_at, None);

        let finalization = writer
            .finish_action(started, ActionExecutionResult::Completed, at(13))
            .await
            .unwrap();
        assert_eq!(finalization.action_id, action_id);
        assert_eq!(finalization.state, ActionLifecycleState::Completed);
        assert_eq!(finalization.completed_at, at(13));
        assert_eq!(finalization.error_code, None);
        let durable_completed = reader.action(action_id).unwrap().unwrap();
        assert_eq!(durable_completed.state, ActionLifecycleState::Completed);
        assert_eq!(durable_completed.started_at, Some(at(12)));
        assert_eq!(durable_completed.completed_at, Some(at(13)));
        assert_eq!(durable_completed.error_code, None);
        assert_eq!(durable_completed.lease_expires_at, None);
        let completed_attempts = reader.action_attempts(action_id).unwrap();
        assert_eq!(completed_attempts.len(), 1);
        assert_eq!(completed_attempts[0].state, ActionAttemptState::Completed);
        assert_eq!(completed_attempts[0].completed_at, Some(at(13)));
        assert_eq!(completed_attempts[0].error_code, None);
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn terminal_finish_replays_only_the_identical_tuple() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("action-finish-replay.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let (_, alert_id, action_id) = create_pending_action(&writer, &database).await;
        let claimed = claim(&writer, ActionLeaseOwner::new(), at(11), at(20)).await;
        let ActionStartOutcome::Started(started) =
            writer.start_action(claimed, at(12)).await.unwrap()
        else {
            panic!("enabled action must start");
        };

        let event_boundary = writer.change_feed().current_cursor();
        let first = writer
            .finish_action(started.clone(), ActionExecutionResult::Completed, at(13))
            .await
            .unwrap();
        let replay = writer
            .finish_action(started.clone(), ActionExecutionResult::Completed, at(13))
            .await
            .unwrap();
        assert_eq!(replay, first);
        assert!(matches!(
            writer
                .finish_action(
                    started,
                    ActionExecutionResult::Failed(ActionErrorCode::NotificationDispatchFailed),
                    at(13),
                )
                .await,
            Err(StorageError::ActionAttemptConflict)
        ));
        let StreamSubscriptionStart::Active(mut events) = writer
            .change_feed()
            .subscribe(Some(&event_boundary))
            .unwrap()
        else {
            panic!("action finish boundary must remain replayable");
        };
        let StreamReceive::Change(event) = events.recv().await else {
            panic!("the committed finish must refresh its alert");
        };
        assert_eq!(event.change, StorageChange::Alert(alert_id));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), events.recv())
                .await
                .is_err(),
            "an idempotent finish replay must not publish another alert"
        );
        let reader = StorageReader::new(database);
        assert_eq!(
            reader.action(action_id).unwrap().unwrap().state,
            ActionLifecycleState::Completed
        );
        assert_eq!(reader.action_attempts(action_id).unwrap().len(), 1);
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn start_replays_only_the_identical_claim_and_timestamp() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("action-start-replay.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let (_, alert_id, action_id) = create_pending_action(&writer, &database).await;
        let claimed = claim(&writer, ActionLeaseOwner::new(), at(11), at(20)).await;

        let event_boundary = writer.change_feed().current_cursor();
        let ActionStartOutcome::Started(first) =
            writer.start_action(claimed.clone(), at(12)).await.unwrap()
        else {
            panic!("enabled action must start");
        };
        let ActionStartOutcome::Started(replay) =
            writer.start_action(claimed.clone(), at(12)).await.unwrap()
        else {
            panic!("identical start must replay the durable attempt");
        };
        assert_eq!(replay, first);
        assert!(matches!(
            writer.start_action(claimed, at(13)).await,
            Err(StorageError::ActionClaimLost)
        ));
        let StreamSubscriptionStart::Active(mut events) = writer
            .change_feed()
            .subscribe(Some(&event_boundary))
            .unwrap()
        else {
            panic!("action start boundary must remain replayable");
        };
        let StreamReceive::Change(event) = events.recv().await else {
            panic!("the committed start must refresh its alert");
        };
        assert_eq!(event.change, StorageChange::Alert(alert_id));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), events.recv())
                .await
                .is_err(),
            "an idempotent start replay must not publish another alert"
        );
        assert_eq!(
            StorageReader::new(database.clone())
                .action_attempts(action_id)
                .unwrap()
                .len(),
            1
        );

        writer
            .finish_action(first, ActionExecutionResult::Completed, at(14))
            .await
            .unwrap();
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn attempt_insert_failure_rolls_back_the_started_transition() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("action-start-rollback.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let (_, _, action_id) = create_pending_action(&writer, &database).await;
        let claimed = claim(&writer, ActionLeaseOwner::new(), at(11), at(20)).await;
        let failure_injector = Connection::open(&database).unwrap();
        failure_injector
            .execute_batch(
                "CREATE TRIGGER test_abort_action_attempt
                 BEFORE INSERT ON action_attempts
                 BEGIN
                     SELECT RAISE(ABORT, 'injected action attempt failure');
                 END;",
            )
            .unwrap();

        assert!(matches!(
            writer.start_action(claimed.clone(), at(12)).await,
            Err(StorageError::Database(_))
        ));
        let reader = StorageReader::new(database.clone());
        let rolled_back = reader.action(action_id).unwrap().unwrap();
        assert_eq!(rolled_back.state, ActionLifecycleState::Pending);
        assert_eq!(rolled_back.started_at, None);
        assert_eq!(rolled_back.lease_generation, 1);
        assert!(reader.action_attempts(action_id).unwrap().is_empty());

        failure_injector
            .execute_batch("DROP TRIGGER test_abort_action_attempt;")
            .unwrap();
        let ActionStartOutcome::Started(started) =
            writer.start_action(claimed, at(12)).await.unwrap()
        else {
            panic!("claim must remain usable after the rolled-back start");
        };
        writer
            .finish_action(started, ActionExecutionResult::Completed, at(13))
            .await
            .unwrap();
        drop(failure_injector);
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn action_terminal_failure_rolls_back_the_attempt_terminal_state() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("action-finish-rollback.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let (_, _, action_id) = create_pending_action(&writer, &database).await;
        let claimed = claim(&writer, ActionLeaseOwner::new(), at(11), at(20)).await;
        let ActionStartOutcome::Started(started) =
            writer.start_action(claimed, at(12)).await.unwrap()
        else {
            panic!("enabled action must start");
        };
        let failure_injector = Connection::open(&database).unwrap();
        failure_injector
            .execute_batch(
                "CREATE TRIGGER test_abort_action_terminal
                 BEFORE UPDATE OF state ON action_outbox
                 WHEN NEW.state = 'completed'
                 BEGIN
                     SELECT RAISE(ABORT, 'injected action terminal failure');
                 END;",
            )
            .unwrap();

        assert!(matches!(
            writer
                .finish_action(started.clone(), ActionExecutionResult::Completed, at(13))
                .await,
            Err(StorageError::Database(_))
        ));
        let reader = StorageReader::new(database.clone());
        assert_eq!(
            reader.action(action_id).unwrap().unwrap().state,
            ActionLifecycleState::Started
        );
        let attempts = reader.action_attempts(action_id).unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].state, ActionAttemptState::Started);
        assert_eq!(attempts[0].completed_at, None);

        failure_injector
            .execute_batch("DROP TRIGGER test_abort_action_terminal;")
            .unwrap();
        writer
            .finish_action(started, ActionExecutionResult::Completed, at(13))
            .await
            .unwrap();
        drop(failure_injector);
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn disabled_notifications_suppress_before_an_attempt() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("action-permission.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let (_, _, action_id) = create_pending_action(&writer, &database).await;
        let claimed = claim(&writer, ActionLeaseOwner::new(), at(11), at(20)).await;
        let reader = StorageReader::new(database);
        let current = reader.settings().unwrap();
        let mut disabled = current.settings;
        disabled.notifications_enabled = false;
        disabled.updated_at = at(11);
        writer
            .claim_settings_coordinator()
            .unwrap()
            .replace_settings(current.revision, disabled)
            .await
            .unwrap();

        let ActionStartOutcome::Suppressed(finalization) =
            writer.start_action(claimed.clone(), at(12)).await.unwrap()
        else {
            panic!("disabled notifications must suppress the action");
        };
        let ActionStartOutcome::Suppressed(replay) =
            writer.start_action(claimed, at(12)).await.unwrap()
        else {
            panic!("identical suppressed start must replay its finalization");
        };
        assert_eq!(replay, finalization);
        assert_eq!(finalization.state, ActionLifecycleState::Suppressed);
        assert_eq!(
            finalization.suppression_reason,
            Some(ActionSuppressionReason::NotificationsDisabled)
        );
        let stored = reader.action(action_id).unwrap().unwrap();
        assert_eq!(stored.state, ActionLifecycleState::Suppressed);
        assert_eq!(stored.started_at, None);
        assert_eq!(stored.completed_at, Some(at(12)));
        assert_eq!(
            stored.suppression_reason,
            Some(ActionSuppressionReason::NotificationsDisabled)
        );
        assert!(reader.action_attempts(action_id).unwrap().is_empty());
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn processor_executes_once_and_audits_each_terminal_executor_result() {
        let cases = [
            (
                "completed",
                ActionExecutionResult::Completed,
                ActionLifecycleState::Completed,
                ActionAttemptState::Completed,
                None,
                AppLocale::ZhCn,
                ActionLocale::ZhCn,
            ),
            (
                "failed",
                ActionExecutionResult::Failed(ActionErrorCode::NotificationDispatchFailed),
                ActionLifecycleState::Failed,
                ActionAttemptState::Failed,
                Some(ActionErrorCode::NotificationDispatchFailed),
                AppLocale::EnUs,
                ActionLocale::EnUs,
            ),
            (
                "unavailable",
                ActionExecutionResult::Unavailable(ActionErrorCode::NotificationUnavailable),
                ActionLifecycleState::Unavailable,
                ActionAttemptState::Unavailable,
                Some(ActionErrorCode::NotificationUnavailable),
                AppLocale::EnUs,
                ActionLocale::EnUs,
            ),
        ];

        for (name, execution, state, attempt_state, error_code, locale, action_locale) in cases {
            let directory = tempdir().unwrap();
            let database = directory.path().join(format!("processor-{name}.sqlite3"));
            let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
            let (_, _, action_id) = create_pending_action(&writer, &database).await;
            let executor = Arc::new(FakeExecutor::valid(execution));
            let processor = ActionProcessor::new(writer.clone(), executor.clone()).unwrap();
            let claimed_at = UtcTimestamp::now();
            let window = ActionLeaseWindow::new(claimed_at, seconds_after(claimed_at, 60)).unwrap();

            let ActionProcessOutcome::Finished(finalization) =
                processor.process_next(window, locale).await.unwrap()
            else {
                panic!("{name} executor must reach a terminal result");
            };
            assert_eq!(finalization.action_id, action_id, "{name}");
            assert_eq!(finalization.state, state, "{name}");
            assert_eq!(finalization.error_code, error_code, "{name}");
            assert_eq!(executor.effect_count(), 1, "{name}");
            assert_eq!(
                executor.contexts(),
                [ActionExecutionContext::new(action_locale, false)],
                "{name}"
            );

            let next_claimed_at = UtcTimestamp::now();
            let idle = processor
                .process_next(
                    ActionLeaseWindow::new(next_claimed_at, seconds_after(next_claimed_at, 60))
                        .unwrap(),
                    AppLocale::EnUs,
                )
                .await
                .unwrap();
            assert_eq!(idle, ActionProcessOutcome::Idle, "{name}");
            assert_eq!(executor.effect_count(), 1, "{name}");

            let reader = StorageReader::new(database);
            let stored = reader.action(action_id).unwrap().unwrap();
            assert_eq!(stored.state, state, "{name}");
            assert_eq!(stored.error_code, error_code, "{name}");
            let attempts = reader.action_attempts(action_id).unwrap();
            assert_eq!(attempts.len(), 1, "{name}");
            assert_eq!(attempts[0].state, attempt_state, "{name}");
            assert_eq!(attempts[0].error_code, error_code, "{name}");
            writer.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn processor_rejects_executor_contract_mismatches_without_effects() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("processor-contract.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let (_, _, action_id) = create_pending_action(&writer, &database).await;
        let mismatches = [
            FakeExecutor {
                result: ActionExecutionResult::Completed,
                capabilities: &[],
                permissions: SYSTEM_NOTIFICATION_DEFINITION.permissions,
                effect_count: AtomicUsize::new(0),
                contexts: Mutex::new(Vec::new()),
            },
            FakeExecutor {
                result: ActionExecutionResult::Completed,
                capabilities: SYSTEM_NOTIFICATION_DEFINITION.capabilities,
                permissions: &[],
                effect_count: AtomicUsize::new(0),
                contexts: Mutex::new(Vec::new()),
            },
        ];

        for executor in mismatches {
            let executor = Arc::new(executor);
            let built = ActionProcessor::new(writer.clone(), executor.clone());
            assert!(matches!(
                built,
                Err(ActionProcessorBuildError::ExecutorContractMismatch)
            ));
            assert_eq!(executor.effect_count(), 0);
        }
        assert_eq!(
            StorageReader::new(database)
                .action(action_id)
                .unwrap()
                .unwrap()
                .state,
            ActionLifecycleState::Pending
        );
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn unknown_action_type_or_schema_becomes_unavailable_without_an_attempt() {
        let cases = [
            (
                "unknown-type",
                "webhook",
                1_u16,
                ActionErrorCode::UnknownActionType,
            ),
            (
                "unknown-schema",
                "system_notification",
                2_u16,
                ActionErrorCode::InvalidParameters,
            ),
        ];

        for (name, action_type, schema_version, expected_error) in cases {
            let directory = tempdir().unwrap();
            let database = directory.path().join(format!("poison-{name}.sqlite3"));
            let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
            let (_, alert_id, action) = create_alert(&writer, &database, 1034).await;
            assert_eq!(action, None);
            let action_id = ActionId::new();
            let marker = u8::try_from(schema_version).unwrap();
            Connection::open(&database)
                .unwrap()
                .execute(
                    "INSERT INTO action_outbox (
                        id, alert_id, action_type, parameter_schema_version,
                        idempotency_key_version, idempotency_key_bytes, state,
                        reasoning_output_tokens, pattern_n, created_at, not_before,
                        lease_generation
                     ) VALUES (?1, ?2, ?3, ?4, 1, ?5, 'pending', 1034, 2,
                               '1970-01-01T00:00:10.000000000Z',
                               '1970-01-01T00:00:10.000000000Z', 0)",
                    params![
                        action_id.to_string(),
                        alert_id.to_string(),
                        action_type,
                        schema_version,
                        vec![marker; 32],
                    ],
                )
                .unwrap();

            let ActionClaimOutcome::RejectedUnavailable(finalization) = writer
                .claim_action(
                    ActionLeaseOwner::new(),
                    ActionLeaseWindow::new(at(11), at(20)).unwrap(),
                )
                .await
                .unwrap()
            else {
                panic!("{name} request must be rejected without execution");
            };
            assert_eq!(finalization.action_id, action_id, "{name}");
            assert_eq!(
                finalization.state,
                ActionLifecycleState::Unavailable,
                "{name}"
            );
            assert_eq!(finalization.error_code, Some(expected_error), "{name}");
            assert!(
                StorageReader::new(database.clone())
                    .action_attempts(action_id)
                    .unwrap()
                    .is_empty(),
                "{name}"
            );
            let durable = StorageReader::new(database.clone())
                .action(action_id)
                .unwrap()
                .unwrap();
            assert_eq!(durable.action_type.as_str(), action_type, "{name}");
            assert_eq!(
                durable.parameter_schema_version.get(),
                schema_version,
                "{name}"
            );
            assert_eq!(durable.state, ActionLifecycleState::Unavailable, "{name}");
            assert_eq!(durable.error_code, Some(expected_error), "{name}");
            assert_eq!(durable.started_at, None, "{name}");
            assert!(durable.completed_at.is_some(), "{name}");
            assert_eq!(durable.parameters, None, "{name}");
            writer.shutdown().await.unwrap();
        }
    }
}
