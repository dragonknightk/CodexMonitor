//! Durable typed rule evaluation over completed canonical model calls.

use codex_monitor_core::{BuiltInRuleRegistry, ModelCallId, RuleErrorCode, UtcTimestamp};

use codex_monitor_storage::{
    RuleWorkCompletion, StorageError, StorageReader, StorageResult, StorageWriter,
};

/// Result of processing at most one durable rule work item.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuleWorkProcessOutcome {
    Idle,
    Completed {
        model_call_id: ModelCallId,
        completion: RuleWorkCompletion,
    },
    Failed {
        model_call_id: ModelCallId,
        error_code: RuleErrorCode,
    },
}

/// Cloneable processor for durable `model_call.completed` work.
///
/// Claiming, evaluation persistence, and completion are serialized through the
/// dedicated writer. Canonical model calls are loaded outside write
/// transactions on independent query-only connections.
#[derive(Clone)]
pub struct RuleWorkProcessor {
    writer: StorageWriter,
    reader: StorageReader,
}

impl RuleWorkProcessor {
    /// Register active typed definitions and create a processor on the writer's
    /// database.
    ///
    /// # Errors
    ///
    /// Returns an error when rule metadata conflicts with the typed registry or
    /// the writer is unavailable.
    pub async fn new(writer: StorageWriter) -> StorageResult<Self> {
        writer.register_builtin_rules().await?;
        let reader = StorageReader::new(writer.database_path().to_path_buf());
        Ok(Self { writer, reader })
    }

    /// Claim and process at most one pending or stale work item.
    ///
    /// A malformed internal event is quarantined after a closed failure code is
    /// recorded. A newer stale-work claim still fences the old processor.
    ///
    /// # Errors
    ///
    /// Returns an error when the claim cannot be acquired or a failure cannot be
    /// recorded and quarantined through the writer.
    pub async fn process_next(
        &self,
        claimed_at: UtcTimestamp,
        stale_before: UtcTimestamp,
    ) -> StorageResult<RuleWorkProcessOutcome> {
        let Some(claim) = self
            .writer
            .claim_rule_work(claimed_at, stale_before)
            .await?
        else {
            return Ok(RuleWorkProcessOutcome::Idle);
        };

        let reader = self.reader.clone();
        let model_call_id = claim.model_call_id();
        let result = async {
            let stored = tokio::task::spawn_blocking(move || reader.model_call(model_call_id))
                .await
                .map_err(|_| StorageError::RuleWorkerStopped)??
                .ok_or(StorageError::RuleWorkModelCallMissing)?;
            let model_call = stored.0;
            let evaluations = BuiltInRuleRegistry::active_definitions()
                .iter()
                .map(|definition| definition.evaluate(&model_call))
                .collect();
            let completion = self
                .writer
                .complete_rule_work(claim, model_call, evaluations, claimed_at)
                .await?;
            Ok(RuleWorkProcessOutcome::Completed {
                model_call_id,
                completion,
            })
        }
        .await;

        match result {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                let error_code = rule_error_code(&error);
                self.writer
                    .fail_rule_work(claim, error_code, claimed_at)
                    .await?;
                Ok(RuleWorkProcessOutcome::Failed {
                    model_call_id,
                    error_code,
                })
            }
        }
    }
}

fn rule_error_code(error: &StorageError) -> RuleErrorCode {
    match error {
        StorageError::WriterClosed
        | StorageError::WriterStopped
        | StorageError::RuleWorkerStopped => RuleErrorCode::InternalQueueClosed,
        _ => RuleErrorCode::PersistenceCommitFailed,
    }
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU32, NonZeroUsize};
    use std::path::Path;

    use codex_monitor_core::{
        ActionExecutionResult, ActionId, ActionLifecycleState, ActionSuppressionReason, AlertId,
        CanonicalField, DataOrigin, ModelCall, NotificationEligibility, Observed, TokenCount,
        UtcTimestamp,
    };
    use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
    use rusqlite::{Connection, params};
    use tempfile::tempdir;

    use codex_monitor_storage::{
        ActionClaimOutcome, ActionLeaseOwner, ActionLeaseWindow, ActionStartOutcome,
        ModelCallQuery, NotificationCoalescingRole, RuleAlertDecision, StorageChange,
        StorageConfig, StoredAction, StoredSourceHealth, StreamReceive, StreamSubscriptionStart,
    };
    use codex_monitor_telemetry::{
        OtlpLogSource, SourceObservationCandidate, TelemetrySource as _,
    };

    use super::*;

    fn at(seconds: i128) -> UtcTimestamp {
        UtcTimestamp::from_unix_timestamp_nanos(seconds * 1_000_000_000).unwrap()
    }

    fn at_nanos(nanoseconds: i128) -> UtcTimestamp {
        UtcTimestamp::from_unix_timestamp_nanos(nanoseconds).unwrap()
    }

    fn fixture() -> ExportLogsServiceRequest {
        fixture_with_reasoning_tokens(516)
    }

    fn fixture_with_reasoning_tokens(tokens: u64) -> ExportLogsServiceRequest {
        fixture_with_identity(
            tokens,
            "synthetic-response-0001",
            Some(("synthetic-conversation-0001", "synthetic-thread-0001")),
        )
    }

    fn fixture_with_identity(
        tokens: u64,
        response_id: &str,
        conversation: Option<(&str, &str)>,
    ) -> ExportLogsServiceRequest {
        fixture_with_optional_reasoning_tokens(Some(tokens), response_id, conversation)
    }

    fn fixture_with_optional_reasoning_tokens(
        tokens: Option<u64>,
        response_id: &str,
        conversation: Option<(&str, &str)>,
    ) -> ExportLogsServiceRequest {
        let fixture = include_str!("../../../fixtures/otlp/response_completed.json");
        let mut fixture: serde_json::Value = serde_json::from_str(fixture).unwrap();
        let log_record = &mut fixture["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0];
        let attributes = log_record["attributes"].as_array_mut().unwrap();
        if let Some((conversation_id, thread_id)) = conversation {
            let mut found_conversation = false;
            let mut found_thread = false;
            for attribute in attributes {
                match attribute["key"].as_str() {
                    Some("conversation.id") => {
                        attribute["value"]["stringValue"] =
                            serde_json::Value::String(conversation_id.to_owned());
                        found_conversation = true;
                    }
                    Some("thread.id") => {
                        attribute["value"]["stringValue"] =
                            serde_json::Value::String(thread_id.to_owned());
                        found_thread = true;
                    }
                    _ => {}
                }
            }
            assert!(found_conversation && found_thread);
        } else {
            attributes.retain(|attribute| {
                !matches!(
                    attribute["key"].as_str(),
                    Some("conversation.id" | "thread.id")
                )
            });
        }

        let mut body: serde_json::Value =
            serde_json::from_str(log_record["body"]["stringValue"].as_str().unwrap()).unwrap();
        assert_eq!(
            body["response"]["id"].as_str(),
            Some("synthetic-response-0001")
        );
        body["response"]["id"] = serde_json::Value::String(response_id.to_owned());
        let output_details = body["response"]["usage"]["output_tokens_details"]
            .as_object_mut()
            .unwrap();
        assert_eq!(
            output_details
                .get("reasoning_tokens")
                .and_then(serde_json::Value::as_u64),
            Some(516)
        );
        if let Some(tokens) = tokens {
            output_details.insert(
                "reasoning_tokens".to_owned(),
                serde_json::Value::Number(tokens.into()),
            );
        } else {
            output_details.remove("reasoning_tokens");
        }
        log_record["body"]["stringValue"] =
            serde_json::Value::String(serde_json::to_string(&body).unwrap());
        serde_json::from_value(fixture).unwrap()
    }

    fn healthy_snapshot(source: &OtlpLogSource, accepted_at: UtcTimestamp) -> StoredSourceHealth {
        let mut accepted = source.clone();
        accepted.mark_committed(accepted_at);
        StoredSourceHealth::from(accepted.health())
    }

    async fn commit_fixture(writer: &StorageWriter, database: &Path) -> ModelCallId {
        commit_fixture_with_reasoning_tokens(writer, database, 516).await
    }

    async fn commit_fixture_with_reasoning_tokens(
        writer: &StorageWriter,
        database: &Path,
        tokens: u64,
    ) -> ModelCallId {
        commit_fixture_request(writer, database, fixture_with_reasoning_tokens(tokens)).await
    }

    async fn commit_distinct_fixture(
        writer: &StorageWriter,
        database: &Path,
        response_id: &str,
        conversation: Option<(&str, &str)>,
    ) -> ModelCallId {
        commit_fixture_request(
            writer,
            database,
            fixture_with_identity(516, response_id, conversation),
        )
        .await
    }

    async fn commit_fixture_request(
        writer: &StorageWriter,
        database: &Path,
        request: ExportLogsServiceRequest,
    ) -> ModelCallId {
        let reader = StorageReader::new(database.to_path_buf());
        let before = reader
            .model_calls(ModelCallQuery::default())
            .unwrap()
            .into_iter()
            .map(|stored| stored.0.id())
            .collect::<Vec<_>>();
        let source = OtlpLogSource::new(at(1)).unwrap();
        let outcome = writer
            .commit_source_batch(
                source.extract(request, at(2)),
                healthy_snapshot(&source, at(2)),
            )
            .await
            .unwrap();
        assert_eq!(outcome.inserted_observations, 1);
        assert_eq!(outcome.downstream_work_enqueued, 1);

        let inserted = reader
            .model_calls(ModelCallQuery::default())
            .unwrap()
            .into_iter()
            .map(|stored| stored.0.id())
            .filter(|id| !before.contains(id))
            .collect::<Vec<_>>();
        assert_eq!(inserted.len(), 1);
        inserted[0]
    }

    async fn process_match_at(
        processor: &RuleWorkProcessor,
        completed_at: UtcTimestamp,
    ) -> (ModelCallId, RuleAlertDecision) {
        let RuleWorkProcessOutcome::Completed {
            model_call_id,
            completion,
        } = processor.process_next(completed_at, at(0)).await.unwrap()
        else {
            panic!("one pending matching call must complete");
        };
        assert_eq!(completion.inserted_evaluations, 1);
        assert_eq!(completion.inserted_alerts.len(), 1);
        (model_call_id, completion.inserted_alerts[0])
    }

    fn stored_action(reader: &StorageReader, decision: RuleAlertDecision) -> StoredAction {
        let action_id = decision
            .action
            .expect("eligible match must create an action")
            .action_id;
        reader.action(action_id).unwrap().unwrap()
    }

    fn coalescing_counts(database: &Path) -> (i64, i64, i64, i64, i64) {
        Connection::open(database)
            .unwrap()
            .query_row(
                "SELECT
                    (SELECT count(*) FROM model_calls),
                    (SELECT count(*) FROM alerts),
                    (SELECT count(*) FROM action_outbox),
                    (SELECT count(*) FROM notification_coalescing_windows),
                    (SELECT count(*) FROM notification_coalescing_members)",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap()
    }

    fn coalescing_completion_state(connection: &Connection) -> (i64, i64, i64, i64, i64, String) {
        connection
            .query_row(
                "SELECT
                    (SELECT count(*) FROM rule_evaluations),
                    (SELECT count(*) FROM alerts),
                    (SELECT count(*) FROM action_outbox),
                    (SELECT count(*) FROM notification_coalescing_windows),
                    (SELECT count(*) FROM notification_coalescing_members),
                    (SELECT state FROM domain_work)",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap()
    }

    fn notification_window_count(database: &Path) -> i64 {
        Connection::open(database)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM notification_coalescing_windows",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn only_rule_evaluation(
        database: &Path,
    ) -> (
        String,
        i64,
        String,
        bool,
        Option<i64>,
        Option<i64>,
        Option<String>,
    ) {
        Connection::open(database)
            .unwrap()
            .query_row(
                "SELECT rule_id, rule_version, model_call_id, matched,
                        reasoning_output_tokens, pattern_n, source_quality
                 FROM rule_evaluations",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .unwrap()
    }

    fn rule_pipeline_counts(database: &Path) -> (i64, i64, i64, i64, i64, i64, i64, i64) {
        Connection::open(database)
            .unwrap()
            .query_row(
                "SELECT
                    (SELECT count(*) FROM model_call_observations),
                    (SELECT count(*) FROM model_calls),
                    (SELECT count(*) FROM domain_work),
                    (SELECT count(*) FROM domain_work WHERE state = 'completed'),
                    (SELECT count(*) FROM rule_evaluations),
                    (SELECT count(*) FROM alerts),
                    (SELECT count(*) FROM action_outbox),
                    (SELECT count(*) FROM action_attempts)",
                [],
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
    }

    fn coalescing_leader_and_follower(actions: &[StoredAction]) -> (&StoredAction, &StoredAction) {
        let mut leaders = actions.iter().filter(|action| {
            action.notification_coalescing.unwrap().role == NotificationCoalescingRole::Leader
        });
        let leader = leaders.next().expect("one coalescing leader must exist");
        assert!(leaders.next().is_none(), "only one leader may exist");
        let mut followers = actions.iter().filter(|action| {
            action.notification_coalescing.unwrap().role == NotificationCoalescingRole::Follower
        });
        let follower = followers
            .next()
            .expect("one coalescing follower must exist");
        assert!(followers.next().is_none(), "only one follower may exist");
        (leader, follower)
    }

    async fn finish_ready_action(
        writer: &StorageWriter,
        expected_action_id: ActionId,
        expected_grouped: bool,
        claimed_at: UtcTimestamp,
        lease_expires_at: UtcTimestamp,
        completed_at: UtcTimestamp,
    ) {
        let ActionClaimOutcome::Claimed(claimed) = writer
            .claim_action(
                ActionLeaseOwner::new(),
                ActionLeaseWindow::new(claimed_at, lease_expires_at).unwrap(),
            )
            .await
            .unwrap()
        else {
            panic!("expected notification action must be ready");
        };
        assert_eq!(claimed.request().id, expected_action_id);
        assert_eq!(claimed.grouped(), expected_grouped);
        let ActionStartOutcome::Started(started) =
            writer.start_action(claimed, claimed_at).await.unwrap()
        else {
            panic!("enabled notification action must start");
        };
        assert_eq!(started.grouped(), expected_grouped);
        writer
            .finish_action(started, ActionExecutionResult::Completed, completed_at)
            .await
            .unwrap();
    }

    struct NotificationPolicyCase {
        name: &'static str,
        tokens: u64,
        settings: Option<(bool, Option<u32>, bool)>,
        rule_enabled: bool,
        expected: NotificationEligibility,
    }

    struct ReasoningBoundaryCase {
        name: &'static str,
        tokens: Option<u64>,
        expected_n: Option<u64>,
        expected_eligibility: Option<NotificationEligibility>,
    }

    fn assert_notification_action(
        reader: &StorageReader,
        decision: RuleAlertDecision,
        expected: NotificationEligibility,
        case_name: &str,
    ) {
        let actions = reader.actions_for_alert(decision.alert_id).unwrap();
        match expected {
            NotificationEligibility::Eligible => {
                assert_eq!(actions.len(), 1, "{case_name}");
                assert_eq!(
                    actions[0].state,
                    ActionLifecycleState::Pending,
                    "{case_name}"
                );
                assert_eq!(
                    decision.action.map(|action| action.action_id),
                    Some(actions[0].id),
                    "{case_name}"
                );
                assert_eq!(
                    decision.action.map(|action| action.state),
                    Some(ActionLifecycleState::Pending)
                );
                assert_eq!(actions[0].suppression_reason, None, "{case_name}");
                assert_eq!(actions[0].created_at, at(10), "{case_name}");
                assert_eq!(actions[0].not_before, at(40), "{case_name}");
                assert_eq!(
                    actions[0]
                        .notification_coalescing
                        .expect("eligible known-conversation action must lead a window")
                        .role,
                    NotificationCoalescingRole::Leader,
                    "{case_name}"
                );
            }
            NotificationEligibility::NotificationsDisabled => {
                assert_eq!(actions.len(), 1, "{case_name}");
                assert_eq!(
                    actions[0].state,
                    ActionLifecycleState::Suppressed,
                    "{case_name}"
                );
                assert_eq!(
                    actions[0].suppression_reason,
                    Some(ActionSuppressionReason::NotificationsDisabled),
                    "{case_name}"
                );
                assert_eq!(
                    decision
                        .action
                        .map(|action| (action.action_id, action.state)),
                    Some((actions[0].id, ActionLifecycleState::Suppressed)),
                    "{case_name}"
                );
            }
            NotificationEligibility::AboveThreshold | NotificationEligibility::HistoricalEvent => {
                assert!(actions.is_empty(), "{case_name}");
                assert_eq!(decision.action, None, "{case_name}");
            }
        }
    }

    async fn assert_notification_policy_case(directory: &Path, case: NotificationPolicyCase) {
        let database = directory.join(format!("{}.sqlite3", case.name));
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let processor = RuleWorkProcessor::new(writer.clone()).await.unwrap();
        let reader = StorageReader::new(database.clone());
        if let Some((enabled, maximum, all)) = case.settings {
            let current = reader.settings().unwrap();
            let mut settings = current.settings;
            settings.notifications_enabled = enabled;
            settings.notify_max_n = maximum.and_then(NonZeroU32::new);
            settings.notify_all = all;
            settings.updated_at = at(3);
            writer
                .claim_settings_coordinator()
                .unwrap()
                .replace_settings(current.revision, settings)
                .await
                .unwrap();
        }
        if !case.rule_enabled {
            Connection::open(&database)
                .unwrap()
                .execute(
                    "UPDATE rules SET enabled = 0
                     WHERE rule_id = 'suspected-reasoning-limit-pattern'
                       AND rule_version = 1",
                    [],
                )
                .unwrap();
        }

        let model_call_id =
            commit_fixture_with_reasoning_tokens(&writer, &database, case.tokens).await;
        let RuleWorkProcessOutcome::Completed { completion, .. } =
            processor.process_next(at(10), at(9)).await.unwrap()
        else {
            panic!("{} must complete one rule work item", case.name);
        };
        assert_eq!(completion.inserted_evaluations, 1, "{}", case.name);
        assert_eq!(completion.inserted_alerts.len(), 1, "{}", case.name);
        let decision = completion.inserted_alerts[0];
        assert_eq!(
            decision.notification_eligibility, case.expected,
            "{}",
            case.name
        );

        let alerts = reader
            .alerts(std::num::NonZeroUsize::new(1).unwrap())
            .unwrap();
        assert_eq!(alerts.len(), 1, "{}", case.name);
        assert_eq!(alerts[0].model_call_id, model_call_id, "{}", case.name);
        assert_eq!(
            alerts[0].reasoning_output_tokens.get(),
            case.tokens,
            "{}",
            case.name
        );
        assert_eq!(
            alerts[0].pattern_n.get(),
            (case.tokens + 2) / 518,
            "{}",
            case.name
        );
        assert_notification_action(&reader, decision, case.expected, case.name);
        writer.shutdown().await.unwrap();
    }

    async fn assert_reasoning_boundary_case(directory: &Path, case: ReasoningBoundaryCase) {
        let database = directory.join(format!("boundary-{}.sqlite3", case.name));
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let processor = RuleWorkProcessor::new(writer.clone()).await.unwrap();
        let response_id = format!("synthetic-response-boundary-{}", case.name);
        let model_call_id = commit_fixture_request(
            &writer,
            &database,
            fixture_with_optional_reasoning_tokens(
                case.tokens,
                &response_id,
                Some((
                    "synthetic-conversation-boundary-matrix",
                    "synthetic-thread-boundary-matrix",
                )),
            ),
        )
        .await;

        let RuleWorkProcessOutcome::Completed {
            model_call_id: processed_model_call_id,
            completion,
        } = processor.process_next(at(10), at(9)).await.unwrap()
        else {
            panic!("{} must complete one rule work item", case.name);
        };
        assert_eq!(processed_model_call_id, model_call_id, "{}", case.name);
        assert_eq!(completion.inserted_evaluations, 1, "{}", case.name);
        assert_eq!(completion.existing_evaluations, 0, "{}", case.name);

        let stored = only_rule_evaluation(&database);
        assert_eq!(
            stored.0, "suspected-reasoning-limit-pattern",
            "{}",
            case.name
        );
        assert_eq!(stored.1, 1, "{}", case.name);
        assert_eq!(stored.2, model_call_id.to_string(), "{}", case.name);
        assert_eq!(stored.3, case.expected_n.is_some(), "{}", case.name);
        assert_eq!(
            stored.4,
            case.tokens.map(|tokens| i64::try_from(tokens).unwrap()),
            "{}",
            case.name
        );
        assert_eq!(
            stored.5,
            case.expected_n.map(|value| i64::try_from(value).unwrap()),
            "{}",
            case.name
        );
        assert_eq!(
            stored.6.as_deref(),
            case.tokens.map(|_| "direct"),
            "{}",
            case.name
        );

        let reader = StorageReader::new(database.clone());
        if let Some(expected_n) = case.expected_n {
            assert_eq!(completion.inserted_alerts.len(), 1, "{}", case.name);
            let decision = completion.inserted_alerts[0];
            let expected_eligibility = case
                .expected_eligibility
                .expect("every match must declare notification eligibility");
            assert_eq!(
                decision.notification_eligibility, expected_eligibility,
                "{}",
                case.name
            );
            let alerts = reader.alerts(NonZeroUsize::new(1).unwrap()).unwrap();
            assert_eq!(alerts.len(), 1, "{}", case.name);
            assert_eq!(alerts[0].model_call_id, model_call_id, "{}", case.name);
            assert_eq!(alerts[0].pattern_n.get(), expected_n, "{}", case.name);
            assert_eq!(
                alerts[0].reasoning_output_tokens.get(),
                case.tokens.unwrap(),
                "{}",
                case.name
            );
            assert_notification_action(&reader, decision, expected_eligibility, case.name);
        } else {
            assert!(completion.inserted_alerts.is_empty(), "{}", case.name);
            assert_eq!(case.expected_eligibility, None, "{}", case.name);
            assert!(
                reader
                    .alerts(NonZeroUsize::new(1).unwrap())
                    .unwrap()
                    .is_empty(),
                "{}",
                case.name
            );
            let action_count: i64 = Connection::open(&database)
                .unwrap()
                .query_row("SELECT count(*) FROM action_outbox", [], |row| row.get(0))
                .unwrap();
            assert_eq!(action_count, 0, "{}", case.name);
        }
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn processor_persists_one_typed_evaluation_and_completes_work() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("rule-processor.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let processor = RuleWorkProcessor::new(writer.clone()).await.unwrap();
        let model_call_id = commit_fixture(&writer, &database).await;
        let event_boundary = writer.change_feed().current_cursor();

        let processed = processor.process_next(at(10), at(9)).await.unwrap();
        let RuleWorkProcessOutcome::Completed {
            model_call_id: processed_model_call_id,
            completion,
        } = processed
        else {
            panic!("fixture work must complete");
        };
        assert_eq!(processed_model_call_id, model_call_id);
        assert_eq!(completion.inserted_evaluations, 1);
        assert_eq!(completion.existing_evaluations, 0);
        assert_eq!(completion.existing_alerts, 0);
        assert_eq!(completion.inserted_alerts.len(), 1);
        let decision = completion.inserted_alerts[0];
        assert_eq!(decision.evaluation_key.model_call_id, model_call_id);
        assert_eq!(
            decision.notification_eligibility,
            NotificationEligibility::Eligible
        );
        let StreamSubscriptionStart::Active(mut events) = writer
            .change_feed()
            .subscribe(Some(&event_boundary))
            .unwrap()
        else {
            panic!("current event boundary must be replayable");
        };
        let StreamReceive::Change(alert_event) = events.recv().await else {
            panic!("rule completion must publish its alert");
        };
        let StreamReceive::Change(model_call_event) = events.recv().await else {
            panic!("rule completion must refresh its model call");
        };
        assert_eq!(alert_event.change, StorageChange::Alert(decision.alert_id));
        assert_eq!(
            model_call_event.change,
            StorageChange::ModelCall(model_call_id)
        );
        assert_eq!(
            processor.process_next(at(11), at(10)).await.unwrap(),
            RuleWorkProcessOutcome::Idle
        );

        let connection = Connection::open(&database).unwrap();
        let evaluation = connection
            .query_row(
                "SELECT rule_id, rule_version, model_call_id, matched,
                        reasoning_output_tokens, pattern_n, source_quality
                 FROM rule_evaluations",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            evaluation,
            (
                "suspected-reasoning-limit-pattern".to_owned(),
                1,
                model_call_id.to_string(),
                1,
                Some(516),
                Some(1),
                Some("direct".to_owned()),
            )
        );
        let work_state = connection
            .query_row(
                "SELECT state, started_at IS NOT NULL, completed_at IS NOT NULL
                 FROM domain_work",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, bool>(1)?,
                        row.get::<_, bool>(2)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(work_state, ("completed".to_owned(), true, true));
        let side_effects = connection
            .query_row(
                "SELECT
                    (SELECT count(*) FROM alerts),
                    (SELECT count(*) FROM action_outbox)",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap();
        assert_eq!(side_effects, (1, 1));
        drop(connection);
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn notification_policy_retains_matches_and_returns_typed_eligibility() {
        let cases = [
            NotificationPolicyCase {
                name: "default-first-match",
                tokens: 516,
                settings: None,
                rule_enabled: true,
                expected: NotificationEligibility::Eligible,
            },
            NotificationPolicyCase {
                name: "default-fourth-match",
                tokens: 2_070,
                settings: None,
                rule_enabled: true,
                expected: NotificationEligibility::AboveThreshold,
            },
            NotificationPolicyCase {
                name: "custom-maximum",
                tokens: 2_070,
                settings: Some((true, Some(4), false)),
                rule_enabled: true,
                expected: NotificationEligibility::Eligible,
            },
            NotificationPolicyCase {
                name: "all-matches",
                tokens: 2_070,
                settings: Some((true, None, true)),
                rule_enabled: true,
                expected: NotificationEligibility::Eligible,
            },
            NotificationPolicyCase {
                name: "all-matches-large-pattern-number",
                tokens: 518 * (u64::from(u32::MAX) + 1) - 2,
                settings: Some((true, None, true)),
                rule_enabled: true,
                expected: NotificationEligibility::Eligible,
            },
            NotificationPolicyCase {
                name: "notifications-disabled",
                tokens: 516,
                settings: Some((false, Some(1), false)),
                rule_enabled: true,
                expected: NotificationEligibility::NotificationsDisabled,
            },
            NotificationPolicyCase {
                name: "rule-disabled",
                tokens: 516,
                settings: None,
                rule_enabled: false,
                expected: NotificationEligibility::NotificationsDisabled,
            },
        ];

        let directory = tempdir().unwrap();
        for case in cases {
            assert_notification_policy_case(directory.path(), case).await;
        }
    }

    #[tokio::test]
    async fn reasoning_boundary_matrix_persists_evidence_and_action_policy() {
        let cases = [
            ReasoningBoundaryCase {
                name: "below-first-pattern",
                tokens: Some(515),
                expected_n: None,
                expected_eligibility: None,
            },
            ReasoningBoundaryCase {
                name: "first-pattern",
                tokens: Some(516),
                expected_n: Some(1),
                expected_eligibility: Some(NotificationEligibility::Eligible),
            },
            ReasoningBoundaryCase {
                name: "above-first-pattern",
                tokens: Some(517),
                expected_n: None,
                expected_eligibility: None,
            },
            ReasoningBoundaryCase {
                name: "second-pattern",
                tokens: Some(1_034),
                expected_n: Some(2),
                expected_eligibility: Some(NotificationEligibility::AboveThreshold),
            },
            ReasoningBoundaryCase {
                name: "third-pattern",
                tokens: Some(1_552),
                expected_n: Some(3),
                expected_eligibility: Some(NotificationEligibility::AboveThreshold),
            },
            ReasoningBoundaryCase {
                name: "fourth-pattern",
                tokens: Some(2_070),
                expected_n: Some(4),
                expected_eligibility: Some(NotificationEligibility::AboveThreshold),
            },
            ReasoningBoundaryCase {
                name: "missing-reasoning",
                tokens: None,
                expected_n: None,
                expected_eligibility: None,
            },
        ];

        let directory = tempdir().unwrap();
        for case in cases {
            assert_reasoning_boundary_case(directory.path(), case).await;
        }
    }

    #[tokio::test]
    async fn non_match_stores_evaluation_without_alert_or_decision() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("rule-non-match.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let processor = RuleWorkProcessor::new(writer.clone()).await.unwrap();
        commit_fixture_with_reasoning_tokens(&writer, &database, 517).await;

        let RuleWorkProcessOutcome::Completed { completion, .. } =
            processor.process_next(at(10), at(9)).await.unwrap()
        else {
            panic!("non-match work must still complete");
        };
        assert_eq!(completion.inserted_evaluations, 1);
        assert!(completion.inserted_alerts.is_empty());
        assert_eq!(completion.existing_alerts, 0);
        let counts = Connection::open(&database)
            .unwrap()
            .query_row(
                "SELECT
                    (SELECT count(*) FROM rule_evaluations WHERE matched = 0),
                    (SELECT count(*) FROM alerts),
                    (SELECT count(*) FROM action_outbox)",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(counts, (1, 0, 0));
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn built_in_registration_is_idempotent_and_preserves_user_settings() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("rule-registration.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        writer.register_builtin_rules().await.unwrap();

        let connection = Connection::open(&database).unwrap();
        connection
            .execute(
                "UPDATE rules
                 SET enabled = 0, notify_max_n = 7, notify_all = 0
                 WHERE rule_id = 'suspected-reasoning-limit-pattern' AND rule_version = 1",
                [],
            )
            .unwrap();
        drop(connection);
        writer.register_builtin_rules().await.unwrap();

        let connection = Connection::open(&database).unwrap();
        let settings = connection
            .query_row(
                "SELECT enabled, notify_max_n, notify_all FROM rules
                 WHERE rule_id = 'suspected-reasoning-limit-pattern' AND rule_version = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, bool>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, bool>(2)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(settings, (false, Some(7), false));

        connection
            .pragma_update(None, "ignore_check_constraints", true)
            .unwrap();
        connection
            .execute(
                "UPDATE rules SET rule_kind = 'corrupt_kind'
                 WHERE rule_id = 'suspected-reasoning-limit-pattern' AND rule_version = 1",
                [],
            )
            .unwrap();
        drop(connection);
        assert!(matches!(
            writer.register_builtin_rules().await.unwrap_err(),
            StorageError::RuleDefinitionConflict
        ));
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn completion_replay_is_idempotent() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("rule-replay.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        writer.register_builtin_rules().await.unwrap();
        let model_call_id = commit_fixture(&writer, &database).await;
        let claim = writer
            .claim_rule_work(at(10), at(9))
            .await
            .unwrap()
            .unwrap();
        let model_call = StorageReader::new(database.clone())
            .model_call(model_call_id)
            .unwrap()
            .unwrap()
            .0;
        let evaluations = BuiltInRuleRegistry::active_definitions()
            .iter()
            .map(|definition| definition.evaluate(&model_call))
            .collect::<Vec<_>>();

        let first = writer
            .complete_rule_work(claim, model_call.clone(), evaluations.clone(), at(10))
            .await
            .unwrap();
        let ActionClaimOutcome::Claimed(action_claim) = writer
            .claim_action(
                ActionLeaseOwner::new(),
                ActionLeaseWindow::new(at(40), at(50)).unwrap(),
            )
            .await
            .unwrap()
        else {
            panic!("new alert action must be claimable");
        };
        let start_boundary = writer.change_feed().current_cursor();
        let ActionStartOutcome::Started(started) =
            writer.start_action(action_claim, at(40)).await.unwrap()
        else {
            panic!("enabled notification action must start");
        };
        let StreamSubscriptionStart::Active(mut start_events) = writer
            .change_feed()
            .subscribe(Some(&start_boundary))
            .unwrap()
        else {
            panic!("action start boundary must remain replayable");
        };
        let StreamReceive::Change(start_event) = start_events.recv().await else {
            panic!("action start must refresh its alert");
        };
        assert_eq!(
            start_event.change,
            StorageChange::Alert(first.inserted_alerts[0].alert_id)
        );

        let finish_boundary = writer.change_feed().current_cursor();
        writer
            .finish_action(started, ActionExecutionResult::Completed, at(41))
            .await
            .unwrap();
        let StreamSubscriptionStart::Active(mut finish_events) = writer
            .change_feed()
            .subscribe(Some(&finish_boundary))
            .unwrap()
        else {
            panic!("action finish boundary must remain replayable");
        };
        let StreamReceive::Change(finish_event) = finish_events.recv().await else {
            panic!("action finish must refresh its alert");
        };
        assert_eq!(
            finish_event.change,
            StorageChange::Alert(first.inserted_alerts[0].alert_id)
        );

        let replay_boundary = writer.change_feed().current_cursor();
        let replay = writer
            .complete_rule_work(claim, model_call, evaluations, at(42))
            .await
            .unwrap();
        assert_eq!(first.inserted_evaluations, 1);
        assert_eq!(first.existing_evaluations, 0);
        assert_eq!(first.inserted_alerts.len(), 1);
        assert_eq!(first.existing_alerts, 0);
        assert_eq!(replay.inserted_evaluations, 0);
        assert_eq!(replay.existing_evaluations, 1);
        assert!(replay.inserted_alerts.is_empty());
        assert_eq!(replay.existing_alerts, 1);
        let StreamSubscriptionStart::Active(mut replay_events) = writer
            .change_feed()
            .subscribe(Some(&replay_boundary))
            .unwrap()
        else {
            panic!("completion replay boundary must remain active");
        };
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), replay_events.recv())
                .await
                .is_err(),
            "idempotent completion replay must not publish another event"
        );
        let counts = Connection::open(&database)
            .unwrap()
            .query_row(
                "SELECT
                    (SELECT count(*) FROM rule_evaluations),
                    (SELECT count(*) FROM alerts),
                    (SELECT count(*) FROM action_outbox)",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(counts, (1, 1, 1));
        let action = StorageReader::new(database.clone())
            .actions_for_alert(first.inserted_alerts[0].alert_id)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(action.state, ActionLifecycleState::Completed);
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn action_insert_failure_rolls_back_the_complete_rule_transaction() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("rule-action-rollback.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        writer.register_builtin_rules().await.unwrap();
        let model_call_id = commit_fixture(&writer, &database).await;
        let claim = writer
            .claim_rule_work(at(10), at(9))
            .await
            .unwrap()
            .unwrap();
        let model_call = StorageReader::new(database.clone())
            .model_call(model_call_id)
            .unwrap()
            .unwrap()
            .0;
        let evaluations = BuiltInRuleRegistry::active_definitions()
            .iter()
            .map(|definition| definition.evaluate(&model_call))
            .collect::<Vec<_>>();

        let failure_injector = Connection::open(&database).unwrap();
        failure_injector
            .execute_batch(
                "CREATE TRIGGER test_abort_action_insert
                 BEFORE INSERT ON action_outbox
                 BEGIN
                     SELECT RAISE(ABORT, 'injected action Outbox failure');
                 END;",
            )
            .unwrap();
        let event_boundary = writer.change_feed().current_cursor();
        let error = writer
            .complete_rule_work(claim, model_call.clone(), evaluations.clone(), at(11))
            .await
            .unwrap_err();
        assert!(matches!(error, StorageError::Database(_)));
        let StreamSubscriptionStart::Active(mut events) = writer
            .change_feed()
            .subscribe(Some(&event_boundary))
            .unwrap()
        else {
            panic!("rollback boundary must remain active");
        };
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), events.recv())
                .await
                .is_err(),
            "rolled-back rule completion must not publish an alert"
        );

        let rolled_back = failure_injector
            .query_row(
                "SELECT
                    (SELECT count(*) FROM rule_evaluations),
                    (SELECT count(*) FROM alerts),
                    (SELECT count(*) FROM action_outbox),
                    (SELECT state FROM domain_work)",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(rolled_back, (0, 0, 0, "started".to_owned()));

        failure_injector
            .execute_batch("DROP TRIGGER test_abort_action_insert;")
            .unwrap();
        writer
            .complete_rule_work(claim, model_call, evaluations, at(12))
            .await
            .unwrap();
        let committed = failure_injector
            .query_row(
                "SELECT
                    (SELECT count(*) FROM rule_evaluations),
                    (SELECT count(*) FROM alerts),
                    (SELECT count(*) FROM action_outbox),
                    (SELECT state FROM domain_work)",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(committed, (1, 1, 1, "completed".to_owned()));
        drop(failure_injector);
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn notification_coalescing_survives_restart_and_preserves_every_record() {
        let directory = tempdir().unwrap();
        let database = directory
            .path()
            .join("notification-coalescing-restart.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let processor = RuleWorkProcessor::new(writer.clone()).await.unwrap();
        let first_call_id = commit_distinct_fixture(
            &writer,
            &database,
            "synthetic-response-coalescing-leader",
            Some((
                "synthetic-conversation-coalescing",
                "synthetic-thread-coalescing",
            )),
        )
        .await;
        let (processed_first_call_id, leader_decision) = process_match_at(&processor, at(10)).await;
        assert_eq!(processed_first_call_id, first_call_id);
        drop(processor);
        writer.shutdown().await.unwrap();

        let reopened = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let restarted_processor = RuleWorkProcessor::new(reopened.clone()).await.unwrap();
        let second_call_id = commit_distinct_fixture(
            &reopened,
            &database,
            "synthetic-response-coalescing-follower",
            Some((
                "synthetic-conversation-coalescing",
                "synthetic-thread-coalescing",
            )),
        )
        .await;
        let last_nanosecond_in_window = at_nanos(39_999_999_999);
        let (processed_second_call_id, follower_decision) =
            process_match_at(&restarted_processor, last_nanosecond_in_window).await;
        assert_eq!(processed_second_call_id, second_call_id);
        assert_ne!(first_call_id, second_call_id);

        let reader = StorageReader::new(database.clone());
        let leader = stored_action(&reader, leader_decision);
        let follower = stored_action(&reader, follower_decision);
        let leader_coalescing = leader
            .notification_coalescing
            .expect("known-conversation leader must retain window audit");
        let follower_coalescing = follower
            .notification_coalescing
            .expect("known-conversation follower must retain window audit");
        assert_eq!(leader.state, ActionLifecycleState::Pending);
        assert_eq!(leader.created_at, at(10));
        assert_eq!(leader.not_before, at(40));
        assert_eq!(leader.suppression_reason, None);
        assert_eq!(leader_coalescing.role, NotificationCoalescingRole::Leader);
        assert_eq!(leader_coalescing.leader_action_id, Some(leader.id));
        assert_eq!(leader_coalescing.opened_at, at(10));
        assert_eq!(leader_coalescing.closes_at, at(40));
        assert_eq!(follower.state, ActionLifecycleState::Suppressed);
        assert_eq!(follower.created_at, last_nanosecond_in_window);
        assert_eq!(follower.not_before, last_nanosecond_in_window);
        assert_eq!(follower.completed_at, Some(last_nanosecond_in_window));
        assert_eq!(
            follower.suppression_reason,
            Some(ActionSuppressionReason::Coalesced)
        );
        assert_eq!(
            follower_coalescing.role,
            NotificationCoalescingRole::Follower
        );
        assert_eq!(follower_coalescing.window_id, leader_coalescing.window_id);
        assert_eq!(follower_coalescing.leader_action_id, Some(leader.id));
        assert_eq!(
            follower_coalescing.conversation_id,
            leader_coalescing.conversation_id
        );
        assert_eq!(coalescing_counts(&database), (2, 2, 2, 1, 2));

        assert_eq!(
            reopened
                .claim_action(
                    ActionLeaseOwner::new(),
                    ActionLeaseWindow::new(last_nanosecond_in_window, at(50)).unwrap(),
                )
                .await
                .unwrap(),
            ActionClaimOutcome::Idle
        );
        finish_ready_action(&reopened, leader.id, true, at(40), at(50), at(41)).await;
        assert_eq!(
            reopened
                .claim_action(
                    ActionLeaseOwner::new(),
                    ActionLeaseWindow::new(at(41), at(51)).unwrap(),
                )
                .await
                .unwrap(),
            ActionClaimOutcome::Idle
        );
        assert_eq!(reader.action_attempts(leader.id).unwrap().len(), 1);
        assert!(reader.action_attempts(follower.id).unwrap().is_empty());
        drop(restarted_processor);
        reopened.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn notification_coalescing_exact_boundary_opens_a_new_window() {
        let directory = tempdir().unwrap();
        let database = directory
            .path()
            .join("notification-coalescing-boundary.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let processor = RuleWorkProcessor::new(writer.clone()).await.unwrap();
        commit_distinct_fixture(
            &writer,
            &database,
            "synthetic-response-boundary-first",
            Some((
                "synthetic-conversation-boundary",
                "synthetic-thread-boundary",
            )),
        )
        .await;
        let (_, first_decision) = process_match_at(&processor, at(10)).await;
        commit_distinct_fixture(
            &writer,
            &database,
            "synthetic-response-boundary-second",
            Some((
                "synthetic-conversation-boundary",
                "synthetic-thread-boundary",
            )),
        )
        .await;
        let (_, second_decision) = process_match_at(&processor, at(40)).await;

        let reader = StorageReader::new(database.clone());
        let first = stored_action(&reader, first_decision);
        let second = stored_action(&reader, second_decision);
        let first_coalescing = first.notification_coalescing.unwrap();
        let second_coalescing = second.notification_coalescing.unwrap();
        assert_eq!(first.state, ActionLifecycleState::Pending);
        assert_eq!(second.state, ActionLifecycleState::Pending);
        assert_eq!(first_coalescing.role, NotificationCoalescingRole::Leader);
        assert_eq!(second_coalescing.role, NotificationCoalescingRole::Leader);
        assert_eq!(first_coalescing.opened_at, at(10));
        assert_eq!(first_coalescing.closes_at, at(40));
        assert_eq!(second_coalescing.opened_at, at(40));
        assert_eq!(second_coalescing.closes_at, at(70));
        assert_eq!(first.not_before, at(40));
        assert_eq!(second.not_before, at(70));
        assert_ne!(first_coalescing.window_id, second_coalescing.window_id);
        assert_eq!(
            first_coalescing.conversation_id,
            second_coalescing.conversation_id
        );
        assert_eq!(notification_window_count(&database), 2);
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn notification_coalescing_is_independent_per_canonical_conversation() {
        let directory = tempdir().unwrap();
        let database = directory
            .path()
            .join("notification-coalescing-conversations.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let processor = RuleWorkProcessor::new(writer.clone()).await.unwrap();
        commit_distinct_fixture(
            &writer,
            &database,
            "synthetic-response-conversation-a",
            Some(("synthetic-conversation-a", "synthetic-thread-a")),
        )
        .await;
        let (_, first_decision) = process_match_at(&processor, at(10)).await;
        commit_distinct_fixture(
            &writer,
            &database,
            "synthetic-response-conversation-b",
            Some(("synthetic-conversation-b", "synthetic-thread-b")),
        )
        .await;
        let (_, second_decision) = process_match_at(&processor, at(11)).await;

        let reader = StorageReader::new(database.clone());
        let first = stored_action(&reader, first_decision);
        let second = stored_action(&reader, second_decision);
        let first_coalescing = first.notification_coalescing.unwrap();
        let second_coalescing = second.notification_coalescing.unwrap();
        assert_eq!(first_coalescing.role, NotificationCoalescingRole::Leader);
        assert_eq!(second_coalescing.role, NotificationCoalescingRole::Leader);
        assert_ne!(
            first_coalescing.conversation_id,
            second_coalescing.conversation_id
        );
        assert_ne!(first_coalescing.window_id, second_coalescing.window_id);
        assert_eq!(first.not_before, at(40));
        assert_eq!(second.not_before, at(41));
        assert_eq!(notification_window_count(&database), 2);
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_processors_create_one_coalescing_leader_and_one_follower() {
        let directory = tempdir().unwrap();
        let database = directory
            .path()
            .join("notification-coalescing-concurrent.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let processor = RuleWorkProcessor::new(writer.clone()).await.unwrap();
        let first_call_id = commit_distinct_fixture(
            &writer,
            &database,
            "synthetic-response-concurrent-first",
            Some((
                "synthetic-conversation-concurrent",
                "synthetic-thread-concurrent",
            )),
        )
        .await;
        let second_call_id = commit_distinct_fixture(
            &writer,
            &database,
            "synthetic-response-concurrent-second",
            Some((
                "synthetic-conversation-concurrent",
                "synthetic-thread-concurrent",
            )),
        )
        .await;

        let first_processor = processor.clone();
        let second_processor = processor.clone();
        let (first, second) = tokio::join!(
            first_processor.process_next(at(10), at(9)),
            second_processor.process_next(at(10), at(9)),
        );
        let mut processed_call_ids = Vec::new();
        let mut decisions = Vec::new();
        for outcome in [first.unwrap(), second.unwrap()] {
            let RuleWorkProcessOutcome::Completed {
                model_call_id,
                completion,
            } = outcome
            else {
                panic!("each processor must claim one of the two work items");
            };
            assert_eq!(completion.inserted_evaluations, 1);
            assert_eq!(completion.inserted_alerts.len(), 1);
            processed_call_ids.push(model_call_id);
            decisions.push(completion.inserted_alerts[0]);
        }
        assert!(processed_call_ids.contains(&first_call_id));
        assert!(processed_call_ids.contains(&second_call_id));

        let reader = StorageReader::new(database.clone());
        let actions = decisions
            .into_iter()
            .map(|decision| stored_action(&reader, decision))
            .collect::<Vec<_>>();
        let (leader, follower) = coalescing_leader_and_follower(&actions);
        let leader_coalescing = leader.notification_coalescing.unwrap();
        let follower_coalescing = follower.notification_coalescing.unwrap();
        assert_eq!(leader.state, ActionLifecycleState::Pending);
        assert_eq!(follower.state, ActionLifecycleState::Suppressed);
        assert_eq!(
            follower.suppression_reason,
            Some(ActionSuppressionReason::Coalesced)
        );
        assert_eq!(follower_coalescing.window_id, leader_coalescing.window_id);
        assert_eq!(follower_coalescing.leader_action_id, Some(leader.id));
        assert_eq!(coalescing_counts(&database), (2, 2, 2, 1, 2));
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn unknown_conversations_do_not_coalesce_or_delay_actions() {
        let directory = tempdir().unwrap();
        let database = directory
            .path()
            .join("notification-coalescing-unknown.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let processor = RuleWorkProcessor::new(writer.clone()).await.unwrap();
        let first_call_id =
            commit_distinct_fixture(&writer, &database, "synthetic-response-unknown-first", None)
                .await;
        let (processed_first_call_id, first_decision) = process_match_at(&processor, at(10)).await;
        assert_eq!(processed_first_call_id, first_call_id);
        let second_call_id = commit_distinct_fixture(
            &writer,
            &database,
            "synthetic-response-unknown-second",
            None,
        )
        .await;
        let (processed_second_call_id, second_decision) =
            process_match_at(&processor, at(11)).await;
        assert_eq!(processed_second_call_id, second_call_id);

        let reader = StorageReader::new(database.clone());
        assert!(
            reader
                .model_call(first_call_id)
                .unwrap()
                .unwrap()
                .0
                .fields()
                .relationship
                .is_none()
        );
        assert!(
            reader
                .model_call(second_call_id)
                .unwrap()
                .unwrap()
                .0
                .fields()
                .relationship
                .is_none()
        );
        let first = stored_action(&reader, first_decision);
        let second = stored_action(&reader, second_decision);
        assert_eq!(first.state, ActionLifecycleState::Pending);
        assert_eq!(second.state, ActionLifecycleState::Pending);
        assert_eq!(first.not_before, at(10));
        assert_eq!(second.not_before, at(11));
        assert!(first.notification_coalescing.is_none());
        assert!(second.notification_coalescing.is_none());

        finish_ready_action(&writer, first.id, false, at(10), at(20), at(11)).await;
        finish_ready_action(&writer, second.id, false, at(11), at(21), at(12)).await;
        assert_eq!(reader.action_attempts(first.id).unwrap().len(), 1);
        assert_eq!(reader.action_attempts(second.id).unwrap().len(), 1);
        let coalescing_counts = Connection::open(&database)
            .unwrap()
            .query_row(
                "SELECT
                    (SELECT count(*) FROM notification_coalescing_windows),
                    (SELECT count(*) FROM notification_coalescing_members)",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap();
        assert_eq!(coalescing_counts, (0, 0));
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn coalescing_member_insert_failure_rolls_back_rule_completion() {
        let directory = tempdir().unwrap();
        let database = directory
            .path()
            .join("notification-coalescing-rollback.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        writer.register_builtin_rules().await.unwrap();
        let model_call_id = commit_distinct_fixture(
            &writer,
            &database,
            "synthetic-response-coalescing-rollback",
            Some((
                "synthetic-conversation-rollback",
                "synthetic-thread-rollback",
            )),
        )
        .await;
        let claim = writer
            .claim_rule_work(at(10), at(9))
            .await
            .unwrap()
            .unwrap();
        let model_call = StorageReader::new(database.clone())
            .model_call(model_call_id)
            .unwrap()
            .unwrap()
            .0;
        let evaluations = BuiltInRuleRegistry::active_definitions()
            .iter()
            .map(|definition| definition.evaluate(&model_call))
            .collect::<Vec<_>>();

        let failure_injector = Connection::open(&database).unwrap();
        failure_injector
            .execute_batch(
                "CREATE TRIGGER test_abort_coalescing_member_insert
                 BEFORE INSERT ON notification_coalescing_members
                 BEGIN
                     SELECT RAISE(ABORT, 'injected coalescing member failure');
                 END;",
            )
            .unwrap();
        let error = writer
            .complete_rule_work(claim, model_call.clone(), evaluations.clone(), at(10))
            .await
            .unwrap_err();
        assert!(matches!(error, StorageError::Database(_)));
        assert_eq!(
            coalescing_completion_state(&failure_injector),
            (0, 0, 0, 0, 0, "started".to_owned())
        );

        failure_injector
            .execute_batch("DROP TRIGGER test_abort_coalescing_member_insert;")
            .unwrap();
        writer
            .complete_rule_work(claim, model_call, evaluations, at(11))
            .await
            .unwrap();
        assert_eq!(
            coalescing_completion_state(&failure_injector),
            (1, 1, 1, 1, 1, "completed".to_owned())
        );
        drop(failure_injector);
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_processors_evaluate_one_work_item_once() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("rule-concurrent.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let processor = RuleWorkProcessor::new(writer.clone()).await.unwrap();
        commit_fixture(&writer, &database).await;

        let first_processor = processor.clone();
        let second_processor = processor.clone();
        let (first, second) = tokio::join!(
            first_processor.process_next(at(10), at(9)),
            second_processor.process_next(at(11), at(9)),
        );
        let outcomes = [first.unwrap(), second.unwrap()];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, RuleWorkProcessOutcome::Completed { .. }))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, RuleWorkProcessOutcome::Idle))
                .count(),
            1
        );
        let count: i64 = Connection::open(&database)
            .unwrap()
            .query_row("SELECT count(*) FROM rule_evaluations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn stale_reclaim_fences_the_previous_owner() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("rule-fencing.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        writer.register_builtin_rules().await.unwrap();
        let model_call_id = commit_fixture(&writer, &database).await;
        let first = writer
            .claim_rule_work(at(10), at(9))
            .await
            .unwrap()
            .unwrap();
        assert!(
            writer
                .claim_rule_work(at(11), at(9))
                .await
                .unwrap()
                .is_none()
        );
        let second = writer
            .claim_rule_work(at(20), at(15))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.work_id(), second.work_id());
        assert_eq!(first.model_call_id(), second.model_call_id());

        let model_call = StorageReader::new(database.clone())
            .model_call(model_call_id)
            .unwrap()
            .unwrap()
            .0;
        let evaluations = BuiltInRuleRegistry::active_definitions()
            .iter()
            .map(|definition| definition.evaluate(&model_call))
            .collect::<Vec<_>>();
        assert!(matches!(
            writer
                .complete_rule_work(first, model_call.clone(), evaluations.clone(), at(21))
                .await
                .unwrap_err(),
            StorageError::RuleWorkClaimLost
        ));
        writer
            .complete_rule_work(second, model_call, evaluations, at(21))
            .await
            .unwrap();
        assert!(matches!(
            writer.release_rule_work(first).await.unwrap_err(),
            StorageError::RuleWorkClaimLost
        ));
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn release_rotates_identity_before_same_timestamp_reclaim() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("rule-release-fencing.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        writer.register_builtin_rules().await.unwrap();
        let model_call_id = commit_fixture(&writer, &database).await;
        let released = writer
            .claim_rule_work(at(10), at(9))
            .await
            .unwrap()
            .unwrap();
        writer.release_rule_work(released).await.unwrap();
        let reclaimed = writer
            .claim_rule_work(at(10), at(9))
            .await
            .unwrap()
            .unwrap();
        assert_ne!(released.work_id(), reclaimed.work_id());

        let model_call = StorageReader::new(database.clone())
            .model_call(model_call_id)
            .unwrap()
            .unwrap()
            .0;
        let evaluations = BuiltInRuleRegistry::active_definitions()
            .iter()
            .map(|definition| definition.evaluate(&model_call))
            .collect::<Vec<_>>();
        assert!(matches!(
            writer
                .complete_rule_work(released, model_call.clone(), evaluations.clone(), at(11))
                .await
                .unwrap_err(),
            StorageError::RuleWorkClaimLost
        ));
        writer
            .complete_rule_work(reclaimed, model_call, evaluations, at(11))
            .await
            .unwrap();
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn duplicate_event_across_restarts_produces_one_terminal_rule_chain() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("rule-duplicate-restart.sqlite3");
        let source = OtlpLogSource::new(at(1)).unwrap();
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let processor = RuleWorkProcessor::new(writer.clone()).await.unwrap();

        let first = writer
            .commit_source_batch(
                source.extract(fixture(), at(2)),
                healthy_snapshot(&source, at(2)),
            )
            .await
            .unwrap();
        assert_eq!(first.inserted_observations, 1);
        assert_eq!(first.duplicate_observations, 0);
        assert_eq!(first.downstream_work_enqueued, 1);
        let duplicate_before_restart = writer
            .commit_source_batch(
                source.extract(fixture(), at(3)),
                healthy_snapshot(&source, at(3)),
            )
            .await
            .unwrap();
        assert_eq!(duplicate_before_restart.inserted_observations, 0);
        assert_eq!(duplicate_before_restart.duplicate_observations, 1);
        assert_eq!(duplicate_before_restart.downstream_work_enqueued, 0);
        let model_call_id = StorageReader::new(database.clone())
            .model_calls(ModelCallQuery::default())
            .unwrap()[0]
            .0
            .id();
        drop(processor);
        writer.shutdown().await.unwrap();

        let reopened = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let restarted_processor = RuleWorkProcessor::new(reopened.clone()).await.unwrap();
        let RuleWorkProcessOutcome::Completed {
            model_call_id: processed_model_call_id,
            completion,
        } = restarted_processor
            .process_next(at(20), at(10))
            .await
            .unwrap()
        else {
            panic!("pending work must survive the first restart");
        };
        assert_eq!(processed_model_call_id, model_call_id);
        assert_eq!(completion.inserted_evaluations, 1);
        assert_eq!(completion.inserted_alerts.len(), 1);
        let action_id = completion.inserted_alerts[0]
            .action
            .expect("the 516 match must create an action")
            .action_id;
        let pending_action = StorageReader::new(database.clone())
            .action(action_id)
            .unwrap()
            .unwrap();
        assert_eq!(pending_action.state, ActionLifecycleState::Pending);
        assert_eq!(pending_action.not_before, at(50));
        finish_ready_action(&reopened, action_id, false, at(50), at(60), at(51)).await;
        drop(restarted_processor);
        reopened.shutdown().await.unwrap();

        let final_writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let duplicate_after_restart = final_writer
            .commit_source_batch(
                source.extract(fixture(), at(70)),
                healthy_snapshot(&source, at(70)),
            )
            .await
            .unwrap();
        assert_eq!(duplicate_after_restart.inserted_observations, 0);
        assert_eq!(duplicate_after_restart.duplicate_observations, 1);
        assert_eq!(duplicate_after_restart.downstream_work_enqueued, 0);
        let final_processor = RuleWorkProcessor::new(final_writer.clone()).await.unwrap();
        assert_eq!(
            final_processor.process_next(at(71), at(70)).await.unwrap(),
            RuleWorkProcessOutcome::Idle
        );

        let reader = StorageReader::new(database.clone());
        let completed_action = reader.action(action_id).unwrap().unwrap();
        assert_eq!(completed_action.state, ActionLifecycleState::Completed);
        assert_eq!(reader.action_attempts(action_id).unwrap().len(), 1);
        assert_eq!(rule_pipeline_counts(&database), (1, 1, 1, 1, 1, 1, 1, 1));
        drop(final_processor);
        final_writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn started_rule_work_reclaims_at_exact_stale_boundary_after_restart() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("rule-started-restart.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let model_call_id = commit_fixture(&writer, &database).await;
        let abandoned = writer
            .claim_rule_work(at(10), at(9))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(abandoned.model_call_id(), model_call_id);
        writer.shutdown().await.unwrap();

        let reopened = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let processor = RuleWorkProcessor::new(reopened.clone()).await.unwrap();
        assert_eq!(
            processor.process_next(at(20), at(9)).await.unwrap(),
            RuleWorkProcessOutcome::Idle
        );
        let RuleWorkProcessOutcome::Completed {
            model_call_id: processed_model_call_id,
            completion,
        } = processor.process_next(at(21), at(10)).await.unwrap()
        else {
            panic!("the abandoned claim must become reclaimable at its exact stale boundary");
        };
        assert_eq!(processed_model_call_id, model_call_id);
        assert_eq!(completion.inserted_evaluations, 1);
        assert_eq!(completion.inserted_alerts.len(), 1);
        assert!(completion.inserted_alerts[0].action.is_some());
        assert_eq!(rule_pipeline_counts(&database), (1, 1, 1, 1, 1, 1, 1, 0));
        drop(processor);
        reopened.shutdown().await.unwrap();

        let final_writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let final_processor = RuleWorkProcessor::new(final_writer.clone()).await.unwrap();
        assert_eq!(
            final_processor.process_next(at(30), at(29)).await.unwrap(),
            RuleWorkProcessOutcome::Idle
        );
        assert_eq!(rule_pipeline_counts(&database), (1, 1, 1, 1, 1, 1, 1, 0));
        drop(final_processor);
        final_writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn pending_work_survives_writer_restart() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("rule-restart.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let model_call_id = commit_fixture(&writer, &database).await;
        writer.shutdown().await.unwrap();

        let reopened = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let processor = RuleWorkProcessor::new(reopened.clone()).await.unwrap();
        let RuleWorkProcessOutcome::Completed {
            model_call_id: processed_model_call_id,
            completion,
        } = processor.process_next(at(20), at(10)).await.unwrap()
        else {
            panic!("restarted worker must complete pending work");
        };
        assert_eq!(processed_model_call_id, model_call_id);
        assert_eq!(completion.inserted_evaluations, 1);
        assert_eq!(completion.inserted_alerts.len(), 1);
        let counts = Connection::open(&database)
            .unwrap()
            .query_row(
                "SELECT
                    (SELECT count(*) FROM rule_evaluations),
                    (SELECT count(*) FROM domain_work WHERE state = 'completed')",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap();
        assert_eq!(counts, (1, 1));
        reopened.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn conflicting_evaluation_rolls_back_work_completion() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("rule-conflict.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        writer.register_builtin_rules().await.unwrap();
        let model_call_id = commit_fixture(&writer, &database).await;
        let claim = writer
            .claim_rule_work(at(10), at(9))
            .await
            .unwrap()
            .unwrap();
        let model_call = StorageReader::new(database.clone())
            .model_call(model_call_id)
            .unwrap()
            .unwrap()
            .0;
        let evaluations = BuiltInRuleRegistry::active_definitions()
            .iter()
            .map(|definition| definition.evaluate(&model_call))
            .collect::<Vec<_>>();

        let connection = Connection::open(&database).unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        let event_at: String = connection
            .query_row(
                "SELECT event_at FROM model_calls WHERE id = ?1",
                [model_call_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO rule_evaluations (
                    id, rule_id, rule_version, model_call_id, matched,
                    reasoning_output_tokens, pattern_n, event_at, source_quality, evaluated_at
                 ) VALUES (
                    'conflicting-evaluation', 'suspected-reasoning-limit-pattern', 1, ?1,
                    0, 516, NULL, ?2, 'direct', ?2
                 )",
                params![model_call_id.to_string(), event_at],
            )
            .unwrap();
        drop(connection);

        assert!(matches!(
            writer
                .complete_rule_work(claim, model_call, evaluations, at(10))
                .await
                .unwrap_err(),
            StorageError::RuleEvaluationConflict
        ));
        let connection = Connection::open(&database).unwrap();
        let state = connection
            .query_row(
                "SELECT state, completed_at, (
                    SELECT last_evaluated_at FROM rules
                    WHERE rule_id = 'suspected-reasoning-limit-pattern' AND rule_version = 1
                 ) FROM domain_work",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(state, ("started".to_owned(), None, None));
        drop(connection);
        writer.release_rule_work(claim).await.unwrap();
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn conflicting_alert_rolls_back_work_completion() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("alert-conflict.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        writer.register_builtin_rules().await.unwrap();
        let model_call_id = commit_fixture(&writer, &database).await;
        let claim = writer
            .claim_rule_work(at(10), at(9))
            .await
            .unwrap()
            .unwrap();
        let model_call = StorageReader::new(database.clone())
            .model_call(model_call_id)
            .unwrap()
            .unwrap()
            .0;
        let evaluations = BuiltInRuleRegistry::active_definitions()
            .iter()
            .map(|definition| definition.evaluate(&model_call))
            .collect::<Vec<_>>();

        let connection = Connection::open(&database).unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        let event_at: String = connection
            .query_row(
                "SELECT event_at FROM model_calls WHERE id = ?1",
                [model_call_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let evaluation_id = uuid::Uuid::now_v7().to_string();
        connection
            .execute(
                "INSERT INTO rule_evaluations (
                    id, rule_id, rule_version, model_call_id, matched,
                    reasoning_output_tokens, pattern_n, event_at, source_quality, evaluated_at
                 ) VALUES (
                    ?1, 'suspected-reasoning-limit-pattern', 1, ?2,
                    1, 516, 1, ?3, 'direct', ?3
                 )",
                params![evaluation_id, model_call_id.to_string(), event_at],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO alerts (
                    id, evaluation_id, model_call_id, rule_id, rule_version,
                    reasoning_output_tokens, pattern_n, event_at, source_quality, created_at
                 ) VALUES (
                    ?1, ?2, ?3, 'suspected-reasoning-limit-pattern', 1,
                    1034, 2, ?4, 'direct', ?4
                 )",
                params![
                    AlertId::new().to_string(),
                    evaluation_id,
                    model_call_id.to_string(),
                    event_at,
                ],
            )
            .unwrap();
        drop(connection);

        assert!(matches!(
            writer
                .complete_rule_work(claim, model_call, evaluations, at(10))
                .await
                .unwrap_err(),
            StorageError::AlertEvidenceConflict
        ));
        let state = Connection::open(&database)
            .unwrap()
            .query_row(
                "SELECT state, completed_at, (
                    SELECT last_evaluated_at FROM rules
                    WHERE rule_id = 'suspected-reasoning-limit-pattern' AND rule_version = 1
                 ) FROM domain_work",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(state, ("started".to_owned(), None, None));
        writer.release_rule_work(claim).await.unwrap();
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn completion_rejects_a_stale_canonical_snapshot() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("rule-stale-snapshot.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        writer.register_builtin_rules().await.unwrap();
        let model_call_id = commit_fixture(&writer, &database).await;
        let claim = writer
            .claim_rule_work(at(10), at(9))
            .await
            .unwrap()
            .unwrap();
        let current = StorageReader::new(database.clone())
            .model_call(model_call_id)
            .unwrap()
            .unwrap()
            .0;
        let reasoning = current.fields().reasoning_output_tokens.as_ref().unwrap();
        let mut stale_fields = current.fields().clone();
        stale_fields.reasoning_output_tokens = Some(CanonicalField::new(Observed::new(
            TokenCount::new(517).unwrap(),
            reasoning.provenance().clone(),
        )));
        let stale = ModelCall::new(
            current.id(),
            current.canonical_identity().clone(),
            current.event_time().clone(),
            current.first_observed_at(),
            current.origin(),
            stale_fields,
        );
        let evaluations = BuiltInRuleRegistry::active_definitions()
            .iter()
            .map(|definition| definition.evaluate(&stale))
            .collect::<Vec<_>>();

        assert!(matches!(
            writer
                .complete_rule_work(claim, stale, evaluations, at(10))
                .await
                .unwrap_err(),
            StorageError::RuleEvaluationBatchInvalid
        ));
        let work_state: String = Connection::open(&database)
            .unwrap()
            .query_row("SELECT state FROM domain_work", [], |row| row.get(0))
            .unwrap();
        assert_eq!(work_state, "started");
        writer.release_rule_work(claim).await.unwrap();
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn historical_calls_never_create_realtime_rule_work() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("rule-historical.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let processor = RuleWorkProcessor::new(writer.clone()).await.unwrap();
        let source = OtlpLogSource::new(at(1)).unwrap();
        let mut extraction = source.extract(fixture(), at(2));
        let candidate = extraction.observations.remove(0);
        extraction.observations.push(
            SourceObservationCandidate::new(
                candidate.kind(),
                candidate.source_event_identity().clone(),
                candidate.event_time().clone(),
                candidate.observed_at(),
                DataOrigin::Historical,
                candidate.fields().clone(),
            )
            .unwrap(),
        );
        let outcome = writer
            .commit_source_batch(extraction, healthy_snapshot(&source, at(2)))
            .await
            .unwrap();
        assert_eq!(outcome.downstream_work_enqueued, 0);
        assert_eq!(
            processor.process_next(at(10), at(9)).await.unwrap(),
            RuleWorkProcessOutcome::Idle
        );
        let counts = Connection::open(&database)
            .unwrap()
            .query_row(
                "SELECT
                    (SELECT count(*) FROM domain_work),
                    (SELECT count(*) FROM rule_evaluations),
                    (SELECT count(*) FROM alerts),
                    (SELECT count(*) FROM action_outbox)",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(counts, (0, 0, 0, 0));
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn action_claim_rechecks_realtime_origin_if_database_guards_are_bypassed() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("action-historical-poison.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let processor = RuleWorkProcessor::new(writer.clone()).await.unwrap();
        let model_call_id = commit_fixture(&writer, &database).await;
        let RuleWorkProcessOutcome::Completed { completion, .. } =
            processor.process_next(at(10), at(9)).await.unwrap()
        else {
            panic!("realtime fixture must complete rule work");
        };
        let action_id = completion.inserted_alerts[0]
            .action
            .expect("eligible alert must create an action")
            .action_id;

        let poison = Connection::open(&database).unwrap();
        poison
            .execute_batch("DROP TRIGGER model_calls_origin_immutable_guard;")
            .unwrap();
        assert_eq!(
            poison
                .execute(
                    "UPDATE model_calls SET origin = 'historical' WHERE id = ?1",
                    [model_call_id.to_string()],
                )
                .unwrap(),
            1
        );
        drop(poison);

        assert_eq!(
            writer
                .claim_action(
                    ActionLeaseOwner::new(),
                    ActionLeaseWindow::new(at(40), at(50)).unwrap(),
                )
                .await
                .unwrap(),
            ActionClaimOutcome::Idle
        );
        let action = StorageReader::new(database)
            .action(action_id)
            .unwrap()
            .unwrap();
        assert_eq!(action.state, ActionLifecycleState::Pending);
        assert_eq!(action.lease_generation, 0);
        writer.shutdown().await.unwrap();
    }
}
