use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use codex_monitor_core::{
    ActionExecutionResult, ActionId, AlertId, BuiltInRuleRegistry, ConversationId, DataOrigin,
    ModelCall, ModelCallId, ModelCallObservation, ModelCallObservationId, ModelCallRelationship,
    RuleErrorCode, RuleEvaluation, SourceExtraction, SourceHealthStatus,
    SourceObservationCandidate, SourceObservationError, SourceObservationKind, UtcTimestamp,
};
use rusqlite::{Connection, OptionalExtension as _, Transaction, TransactionBehavior, params};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use super::action_outbox::{
    ActionTransition, claim_action, finish_action, recover_started_actions, release_action_claim,
    start_action,
};
use super::codec::{find_model_call_by_identity, find_observation_by_identity, insert_observation};
use super::legacy_import::{LegacyImportOutcome, LegacyImportStatus, import_legacy_database};
use super::migrations::open_and_migrate;
use super::projection::{
    insert_model_call, load_model_call, project_model_call, update_ranked_text,
};
use super::retention::apply_retention;
use super::rule_work::{
    claim_rule_work, complete_rule_work, fail_rule_work, register_builtin_rules, release_rule_work,
};
use super::timestamp::timestamp_text;
use super::{
    ActionClaimOutcome, ActionClaimToken, ActionFinalization, ActionLeaseOwner, ActionLeaseWindow,
    ActionRecoveryOutcome, ActionRecoveryRequest, ActionStartOutcome, AppSettings, ClaimedAction,
    ObservationCommitOutcome, ObservationWrite, RetentionOutcome, RetentionRequest, RuleWorkClaim,
    RuleWorkCompletion, SourceBatchCommitOutcome, StartedAction, StorageChange, StorageChangeFeed,
    StorageConfig, StorageError, StorageResult, StoredSettingsDocument, StoredSourceHealth,
};

/// Cloneable handle to the single bounded `SQLite` writer actor.
#[derive(Clone)]
pub struct StorageWriter {
    sender: mpsc::Sender<WriterCommand>,
    thread: Arc<Mutex<Option<JoinHandle<()>>>>,
    database_path: Arc<PathBuf>,
    settings_coordinator_claimed: Arc<AtomicBool>,
    change_feed: StorageChangeFeed,
}

pub struct SettingsCoordinatorWriter {
    writer: StorageWriter,
}

impl StorageWriter {
    /// Run migrations synchronously, then start the dedicated writer thread.
    ///
    /// # Errors
    ///
    /// Returns before spawning the actor when the database cannot be opened or
    /// its embedded migration audit cannot be verified.
    pub fn open(config: StorageConfig) -> StorageResult<Self> {
        let connection = open_and_migrate(&config.database_path)?;
        let change_feed = StorageChangeFeed::new().map_err(StorageError::StreamEntropy)?;
        let (sender, receiver) = mpsc::channel(config.writer_queue_capacity.get());
        let writer_change_feed = change_feed.clone();
        let thread = thread::Builder::new()
            .name("codex-monitor-sqlite-writer".to_owned())
            .spawn(move || run_writer(connection, receiver, &writer_change_feed))
            .map_err(|source| StorageError::AppDataIo {
                component: "SQLite writer thread",
                source,
            })?;
        Ok(Self {
            sender,
            thread: Arc::new(Mutex::new(Some(thread))),
            database_path: Arc::new(config.database_path),
            settings_coordinator_claimed: Arc::new(AtomicBool::new(false)),
            change_feed,
        })
    }

    #[must_use]
    pub fn database_path(&self) -> &Path {
        self.database_path.as_path()
    }

    pub fn claim_settings_coordinator(&self) -> Option<SettingsCoordinatorWriter> {
        self.settings_coordinator_claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            .then(|| SettingsCoordinatorWriter {
                writer: self.clone(),
            })
    }

    pub fn change_feed(&self) -> StorageChangeFeed {
        self.change_feed.clone()
    }

    /// Commit one typed observation, canonical projection and required durable
    /// work in a short transaction.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the actor is closed or the transaction
    /// fails. Retransmitted source identities return a duplicate outcome.
    pub async fn commit_observation(
        &self,
        write: ObservationWrite,
    ) -> StorageResult<ObservationCommitOutcome> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WriterCommand::CommitObservation {
                write: Box::new(write),
                reply,
            })
            .await
            .map_err(|_| StorageError::WriterClosed)?;
        response.await.map_err(|_| StorageError::WriterStopped)?
    }

    /// Commit every accepted candidate from one source request atomically.
    ///
    /// `source_health` is written in the same transaction, allowing callers to
    /// update their in-memory health only after this method succeeds.
    ///
    /// # Errors
    ///
    /// Returns an error without committing any candidate when correlation,
    /// projection, source health, or durable-work insertion fails.
    pub async fn commit_source_batch(
        &self,
        extraction: SourceExtraction,
        source_health: StoredSourceHealth,
    ) -> StorageResult<SourceBatchCommitOutcome> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WriterCommand::CommitSourceBatch {
                extraction,
                source_health,
                reply,
            })
            .await
            .map_err(|_| StorageError::WriterClosed)?;
        response.await.map_err(|_| StorageError::WriterStopped)?
    }

    /// Import one explicitly selected legacy Python database atomically.
    ///
    /// The source is reopened read-only and revalidated inside the writer
    /// command. Imported observations are historical and the source file is
    /// never attached to or modified by the application database connection.
    ///
    /// # Errors
    ///
    /// Returns a closed legacy-source error for an unsafe, malformed, changed,
    /// or unsupported source, or a storage error if the complete import rolls
    /// back.
    pub async fn import_legacy_database(
        &self,
        source_path: PathBuf,
        imported_at: UtcTimestamp,
    ) -> StorageResult<LegacyImportOutcome> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WriterCommand::ImportLegacy {
                source_path,
                imported_at,
                reply,
            })
            .await
            .map_err(|_| StorageError::WriterClosed)?;
        response.await.map_err(|_| StorageError::WriterStopped)?
    }

    /// Persist one privacy-safe source-health snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when the actor is unavailable or the snapshot violates
    /// the registered field catalog.
    pub async fn upsert_source_health(&self, health: StoredSourceHealth) -> StorageResult<()> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WriterCommand::UpsertSourceHealth { health, reply })
            .await
            .map_err(|_| StorageError::WriterClosed)?;
        response.await.map_err(|_| StorageError::WriterStopped)?
    }

    /// Atomically replace the complete settings document if its revision is
    /// current.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::SettingsRevisionConflict`] for a stale revision,
    /// or another storage error when validation or persistence fails.
    async fn replace_settings(
        &self,
        expected_revision: u64,
        settings: AppSettings,
    ) -> StorageResult<StoredSettingsDocument> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WriterCommand::ReplaceSettings {
                expected_revision,
                settings,
                reply,
            })
            .await
            .map_err(|_| StorageError::WriterClosed)?;
        response.await.map_err(|_| StorageError::WriterStopped)?
    }

    /// Run one bounded retention transaction.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the actor is unavailable or cleanup fails.
    pub async fn apply_retention(
        &self,
        request: RetentionRequest,
    ) -> StorageResult<RetentionOutcome> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WriterCommand::ApplyRetention { request, reply })
            .await
            .map_err(|_| StorageError::WriterClosed)?;
        response.await.map_err(|_| StorageError::WriterStopped)?
    }

    /// Register every typed built-in rule without overwriting user settings.
    ///
    /// # Errors
    ///
    /// Returns an error when durable metadata conflicts with the active typed
    /// registry or the writer actor is unavailable.
    pub async fn register_builtin_rules(&self) -> StorageResult<()> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WriterCommand::RegisterBuiltInRules { reply })
            .await
            .map_err(|_| StorageError::WriterClosed)?;
        response.await.map_err(|_| StorageError::WriterStopped)?
    }

    /// Claim the oldest pending or stale `model_call.completed` work item.
    ///
    /// `stale_before` must be earlier than `claimed_at`; the claim timestamp is
    /// persisted as the fencing token.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsafe claim window, invalid durable data, or an
    /// unavailable writer actor.
    pub async fn claim_rule_work(
        &self,
        claimed_at: UtcTimestamp,
        stale_before: UtcTimestamp,
    ) -> StorageResult<Option<RuleWorkClaim>> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WriterCommand::ClaimRuleWork {
                claimed_at,
                stale_before,
                reply,
            })
            .await
            .map_err(|_| StorageError::WriterClosed)?;
        response.await.map_err(|_| StorageError::WriterStopped)?
    }

    /// Atomically store one complete typed evaluation batch and finish its work.
    ///
    /// Existing rows with identical semantics are accepted as idempotent
    /// replays. A changed result for the same durable key is rejected.
    ///
    /// # Errors
    ///
    /// Returns an error when the claim was fenced, the evaluation batch differs
    /// from the active registry, or persistence fails.
    pub async fn complete_rule_work(
        &self,
        claim: RuleWorkClaim,
        model_call: ModelCall,
        evaluations: Vec<RuleEvaluation>,
        completed_at: UtcTimestamp,
    ) -> StorageResult<RuleWorkCompletion> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WriterCommand::CompleteRuleWork {
                claim,
                model_call: Box::new(model_call),
                evaluations,
                completed_at,
                reply,
            })
            .await
            .map_err(|_| StorageError::WriterClosed)?;
        response.await.map_err(|_| StorageError::WriterStopped)?
    }

    /// Return owned work to pending state after a recoverable processing error.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::RuleWorkClaimLost`] when a newer claim has fenced
    /// this worker, or another storage error when persistence fails.
    pub async fn release_rule_work(&self, claim: RuleWorkClaim) -> StorageResult<()> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WriterCommand::ReleaseRuleWork { claim, reply })
            .await
            .map_err(|_| StorageError::WriterClosed)?;
        response.await.map_err(|_| StorageError::WriterStopped)?
    }

    /// Record a privacy-safe rule failure and terminally quarantine its owned
    /// work item so one malformed internal event cannot stop or spin a worker.
    ///
    /// # Errors
    ///
    /// claim 已被更新的 lease 隔离、writer 已停止，或持久化失败时返回错误。
    pub async fn fail_rule_work(
        &self,
        claim: RuleWorkClaim,
        error_code: RuleErrorCode,
        failed_at: UtcTimestamp,
    ) -> StorageResult<()> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WriterCommand::FailRuleWork {
                claim,
                error_code,
                failed_at,
                reply,
            })
            .await
            .map_err(|_| StorageError::WriterClosed)?;
        response.await.map_err(|_| StorageError::WriterStopped)?
    }

    /// Lease the oldest ready pending action without starting its effect.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid durable state or an unavailable writer.
    pub async fn claim_action(
        &self,
        lease_owner: ActionLeaseOwner,
        window: ActionLeaseWindow,
    ) -> StorageResult<ActionClaimOutcome> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WriterCommand::Action(ActionWriterCommand::Claim {
                lease_owner,
                window,
                reply,
            }))
            .await
            .map_err(|_| StorageError::WriterClosed)?;
        response.await.map_err(|_| StorageError::WriterStopped)?
    }

    /// Release an owned pending action without starting an effect.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::ActionClaimLost`] when a newer lease fenced the
    /// claim or the action is no longer pending.
    pub async fn release_action_claim(&self, claim: ActionClaimToken) -> StorageResult<()> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WriterCommand::Action(ActionWriterCommand::Release {
                claim,
                reply,
            }))
            .await
            .map_err(|_| StorageError::WriterClosed)?;
        response.await.map_err(|_| StorageError::WriterStopped)?
    }

    /// Persist `started` and its attempt before an external effect may run.
    ///
    /// The current notification permission is checked in this transaction; a
    /// revoked permission produces a terminal suppressed result with no attempt.
    ///
    /// # Errors
    ///
    /// Returns an error for a lost lease, invalid timestamp, or persistence
    /// failure.
    pub async fn start_action(
        &self,
        claimed: ClaimedAction,
        started_at: UtcTimestamp,
    ) -> StorageResult<ActionStartOutcome> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WriterCommand::Action(ActionWriterCommand::Start {
                claimed,
                started_at,
                reply,
            }))
            .await
            .map_err(|_| StorageError::WriterClosed)?;
        response.await.map_err(|_| StorageError::WriterStopped)?
    }

    /// Atomically finish a started action and its matching attempt.
    ///
    /// # Errors
    ///
    /// Returns an error when the attempt is fenced, conflicts with durable
    /// state, or cannot be persisted.
    pub async fn finish_action(
        &self,
        started: StartedAction,
        result: ActionExecutionResult,
        completed_at: UtcTimestamp,
    ) -> StorageResult<ActionFinalization> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WriterCommand::Action(ActionWriterCommand::Finish {
                started,
                result,
                completed_at,
                reply,
            }))
            .await
            .map_err(|_| StorageError::WriterClosed)?;
        response.await.map_err(|_| StorageError::WriterStopped)?
    }

    /// Atomically classify every action still started at startup as indeterminate.
    ///
    /// This startup-only operation never invokes an executor and must run before
    /// current-process action workers begin claiming work.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid attempt audit, unavailable writer, or
    /// persistence failure. The complete recovery batch rolls back on error.
    pub async fn recover_started_actions(
        &self,
        request: ActionRecoveryRequest,
    ) -> StorageResult<ActionRecoveryOutcome> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WriterCommand::Action(ActionWriterCommand::Recover {
                request,
                reply,
            }))
            .await
            .map_err(|_| StorageError::WriterClosed)?;
        response.await.map_err(|_| StorageError::WriterStopped)?
    }

    /// Drain all earlier commands, close `SQLite`, and join the writer thread.
    ///
    /// # Errors
    ///
    /// Returns an error if the actor already stopped or the writer thread
    /// panicked.
    pub async fn shutdown(&self) -> StorageResult<()> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send(WriterCommand::Shutdown { reply })
            .await
            .map_err(|_| StorageError::WriterClosed)?;
        response.await.map_err(|_| StorageError::WriterStopped)?;

        let thread = self
            .thread
            .lock()
            .map_err(|_| StorageError::WriterStopped)?
            .take();
        if let Some(thread) = thread {
            tokio::task::spawn_blocking(move || thread.join())
                .await
                .map_err(|_| StorageError::WriterStopped)?
                .map_err(|_| StorageError::WriterStopped)?;
        }
        Ok(())
    }
}

impl SettingsCoordinatorWriter {
    /// 以乐观 revision 条件完整替换设置文档。
    ///
    /// # Errors
    ///
    /// revision 冲突、设置不满足领域约束、writer 已停止或持久化失败时返回错误。
    pub async fn replace_settings(
        &self,
        expected_revision: u64,
        settings: AppSettings,
    ) -> StorageResult<StoredSettingsDocument> {
        self.writer
            .replace_settings(expected_revision, settings)
            .await
    }

    pub fn change_feed(&self) -> StorageChangeFeed {
        self.writer.change_feed()
    }
}

enum WriterCommand {
    CommitObservation {
        write: Box<ObservationWrite>,
        reply: oneshot::Sender<StorageResult<ObservationCommitOutcome>>,
    },
    CommitSourceBatch {
        extraction: SourceExtraction,
        source_health: StoredSourceHealth,
        reply: oneshot::Sender<StorageResult<SourceBatchCommitOutcome>>,
    },
    ImportLegacy {
        source_path: PathBuf,
        imported_at: UtcTimestamp,
        reply: oneshot::Sender<StorageResult<LegacyImportOutcome>>,
    },
    UpsertSourceHealth {
        health: StoredSourceHealth,
        reply: oneshot::Sender<StorageResult<()>>,
    },
    ReplaceSettings {
        expected_revision: u64,
        settings: AppSettings,
        reply: oneshot::Sender<StorageResult<StoredSettingsDocument>>,
    },
    ApplyRetention {
        request: RetentionRequest,
        reply: oneshot::Sender<StorageResult<RetentionOutcome>>,
    },
    RegisterBuiltInRules {
        reply: oneshot::Sender<StorageResult<()>>,
    },
    ClaimRuleWork {
        claimed_at: UtcTimestamp,
        stale_before: UtcTimestamp,
        reply: oneshot::Sender<StorageResult<Option<RuleWorkClaim>>>,
    },
    CompleteRuleWork {
        claim: RuleWorkClaim,
        model_call: Box<ModelCall>,
        evaluations: Vec<RuleEvaluation>,
        completed_at: UtcTimestamp,
        reply: oneshot::Sender<StorageResult<RuleWorkCompletion>>,
    },
    ReleaseRuleWork {
        claim: RuleWorkClaim,
        reply: oneshot::Sender<StorageResult<()>>,
    },
    FailRuleWork {
        claim: RuleWorkClaim,
        error_code: RuleErrorCode,
        failed_at: UtcTimestamp,
        reply: oneshot::Sender<StorageResult<()>>,
    },
    Action(ActionWriterCommand),
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

enum ActionWriterCommand {
    Claim {
        lease_owner: ActionLeaseOwner,
        window: ActionLeaseWindow,
        reply: oneshot::Sender<StorageResult<ActionClaimOutcome>>,
    },
    Release {
        claim: ActionClaimToken,
        reply: oneshot::Sender<StorageResult<()>>,
    },
    Start {
        claimed: ClaimedAction,
        started_at: UtcTimestamp,
        reply: oneshot::Sender<StorageResult<ActionStartOutcome>>,
    },
    Finish {
        started: StartedAction,
        result: ActionExecutionResult,
        completed_at: UtcTimestamp,
        reply: oneshot::Sender<StorageResult<ActionFinalization>>,
    },
    Recover {
        request: ActionRecoveryRequest,
        reply: oneshot::Sender<StorageResult<ActionRecoveryOutcome>>,
    },
}

#[allow(clippy::too_many_lines)]
fn run_writer(
    mut connection: Connection,
    mut receiver: mpsc::Receiver<WriterCommand>,
    change_feed: &StorageChangeFeed,
) {
    while let Some(command) = receiver.blocking_recv() {
        match command {
            WriterCommand::CommitObservation { write, reply } => {
                let result = commit_observation(&mut connection, *write);
                if let Ok(ObservationCommitOutcome::Inserted { model_call_id, .. }) = &result {
                    change_feed.publish(StorageChange::ModelCall(*model_call_id));
                }
                let _ = reply.send(result);
            }
            WriterCommand::CommitSourceBatch {
                extraction,
                source_health,
                reply,
            } => {
                let result = commit_source_batch(&mut connection, extraction, &source_health);
                let reply_result = result.map(|commit| {
                    for model_call_id in commit.model_call_ids {
                        change_feed.publish(StorageChange::ModelCall(model_call_id));
                    }
                    if commit.source_health_changed {
                        change_feed.publish(StorageChange::SourceHealth {
                            source_id: source_health.source_id.clone(),
                            source_contract_version: source_health.source_contract_version,
                        });
                    }
                    commit.outcome
                });
                let _ = reply.send(reply_result);
            }
            WriterCommand::ImportLegacy {
                source_path,
                imported_at,
                reply,
            } => {
                let result = import_legacy_database(&mut connection, &source_path, imported_at);
                if result
                    .as_ref()
                    .is_ok_and(|outcome| outcome.status() == LegacyImportStatus::Imported)
                {
                    change_feed.publish(StorageChange::ResyncRequired);
                }
                let _ = reply.send(result);
            }
            WriterCommand::UpsertSourceHealth { health, reply } => {
                let result = (|| {
                    let transaction =
                        connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    let changed = upsert_source_health(&transaction, &health)?;
                    transaction.commit()?;
                    Ok(changed)
                })();
                let reply_result = result.map(|changed| {
                    if changed {
                        change_feed.publish(StorageChange::SourceHealth {
                            source_id: health.source_id,
                            source_contract_version: health.source_contract_version,
                        });
                    }
                });
                let _ = reply.send(reply_result);
            }
            WriterCommand::ReplaceSettings {
                expected_revision,
                settings,
                reply,
            } => {
                let result = replace_settings(&mut connection, expected_revision, settings);
                if let Ok(document) = &result {
                    change_feed.publish(StorageChange::Settings(document.clone()));
                }
                let _ = reply.send(result);
            }
            WriterCommand::ApplyRetention { request, reply } => {
                let result = apply_retention(&mut connection, request);
                if result.as_ref().is_ok_and(|outcome| {
                    outcome.deleted_model_calls > 0
                        || outcome.deleted_orphan_turns > 0
                        || outcome.deleted_orphan_conversations > 0
                }) {
                    change_feed.publish(StorageChange::ResyncRequired);
                }
                let _ = reply.send(result);
            }
            WriterCommand::RegisterBuiltInRules { reply } => {
                let _ = reply.send(register_builtin_rules(
                    &mut connection,
                    BuiltInRuleRegistry::active_definitions(),
                ));
            }
            WriterCommand::ClaimRuleWork {
                claimed_at,
                stale_before,
                reply,
            } => {
                let _ = reply.send(claim_rule_work(&mut connection, claimed_at, stale_before));
            }
            WriterCommand::CompleteRuleWork {
                claim,
                model_call,
                evaluations,
                completed_at,
                reply,
            } => {
                let result = complete_rule_work(
                    &mut connection,
                    claim,
                    &model_call,
                    &evaluations,
                    completed_at,
                );
                if let Ok(completion) = &result
                    && !completion.inserted_alerts.is_empty()
                {
                    for alert in &completion.inserted_alerts {
                        change_feed.publish(StorageChange::Alert(alert.alert_id));
                    }
                    change_feed.publish(StorageChange::ModelCall(model_call.id()));
                }
                let _ = reply.send(result);
            }
            WriterCommand::ReleaseRuleWork { claim, reply } => {
                let _ = reply.send(release_rule_work(&mut connection, claim));
            }
            WriterCommand::FailRuleWork {
                claim,
                error_code,
                failed_at,
                reply,
            } => {
                let _ = reply.send(fail_rule_work(
                    &mut connection,
                    claim,
                    error_code,
                    failed_at,
                ));
            }
            WriterCommand::Action(command) => {
                dispatch_action_command(&mut connection, change_feed, command);
            }
            WriterCommand::Shutdown { reply } => {
                drop(connection);
                let _ = reply.send(());
                break;
            }
        }
    }
}

fn dispatch_action_command(
    connection: &mut Connection,
    change_feed: &StorageChangeFeed,
    command: ActionWriterCommand,
) {
    match command {
        ActionWriterCommand::Claim {
            lease_owner,
            window,
            reply,
        } => {
            let result = claim_action(connection, lease_owner, window);
            if let Ok(ActionClaimOutcome::RejectedUnavailable(finalization)) = &result {
                publish_action_alert(connection, change_feed, finalization.action_id);
            }
            let _ = reply.send(result);
        }
        ActionWriterCommand::Release { claim, reply } => {
            let _ = reply.send(release_action_claim(connection, claim));
        }
        ActionWriterCommand::Start {
            claimed,
            started_at,
            reply,
        } => {
            let alert_id = claimed.request().alert_id;
            let result = start_action(connection, claimed, started_at);
            if result.as_ref().is_ok_and(ActionTransition::changed) {
                change_feed.publish(StorageChange::Alert(alert_id));
            }
            let _ = reply.send(result.map(ActionTransition::into_outcome));
        }
        ActionWriterCommand::Finish {
            started,
            result,
            completed_at,
            reply,
        } => {
            let alert_id = started.request().alert_id;
            let transition = finish_action(connection, &started, result, completed_at);
            if transition.as_ref().is_ok_and(ActionTransition::changed) {
                change_feed.publish(StorageChange::Alert(alert_id));
            }
            let _ = reply.send(transition.map(ActionTransition::into_outcome));
        }
        ActionWriterCommand::Recover { request, reply } => {
            let result = recover_started_actions(connection, request);
            if result
                .as_ref()
                .is_ok_and(|outcome| outcome.recovered_actions > 0)
            {
                change_feed.publish(StorageChange::ResyncRequired);
            }
            let _ = reply.send(result);
        }
    }
}

fn publish_action_alert(
    connection: &Connection,
    change_feed: &StorageChangeFeed,
    action_id: ActionId,
) {
    match alert_id_for_action(connection, action_id) {
        Ok(alert_id) => change_feed.publish(StorageChange::Alert(alert_id)),
        Err(_) => change_feed.publish(StorageChange::ResyncRequired),
    }
}

fn alert_id_for_action(connection: &Connection, action_id: ActionId) -> StorageResult<AlertId> {
    let value = connection.query_row(
        "SELECT alert_id FROM action_outbox WHERE id = ?1",
        [action_id.to_string()],
        |row| row.get::<_, String>(0),
    )?;
    AlertId::from_str(&value).map_err(|_| StorageError::InvalidStoredValue { field: "alert.id" })
}

fn commit_observation(
    connection: &mut Connection,
    write: ObservationWrite,
) -> StorageResult<ObservationCommitOutcome> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let outcome = commit_observation_transaction(&transaction, write)?;
    transaction.commit()?;
    Ok(outcome)
}

pub(super) fn commit_observation_transaction(
    transaction: &Transaction<'_>,
    write: ObservationWrite,
) -> StorageResult<ObservationCommitOutcome> {
    let (canonical_identity, observation) = write.into_parts();
    if let Some((observation_id, model_call_id)) =
        find_observation_by_identity(transaction, observation.source_event_identity())?
    {
        let canonical_model_call = find_model_call_by_identity(transaction, &canonical_identity)?
            .ok_or(StorageError::CanonicalIdentityCollision)?;
        if canonical_model_call != model_call_id {
            return Err(StorageError::ObservationModelCallMismatch);
        }
        return Ok(ObservationCommitOutcome::DuplicateSourceObservation {
            model_call_id,
            observation_id,
            downstream_work_enqueued: false,
        });
    }

    let existing_model_call = find_model_call_by_identity(transaction, &canonical_identity)?;
    let model_call_id = existing_model_call.unwrap_or_else(|| observation.model_call_id());
    let observation = rebind_observation(observation, model_call_id)?;
    let before = existing_model_call
        .map(|id| load_model_call(transaction, id))
        .transpose()?;
    let canonical_created = existing_model_call.is_none();
    if canonical_created {
        insert_model_call(transaction, &canonical_identity, &observation)?;
    }
    insert_observation(transaction, &observation)?;
    project_model_call(transaction, model_call_id, observation.id())?;
    let after = load_model_call(transaction, model_call_id)?;
    let projection_changed = before.as_ref() != Some(&after);
    let downstream_work_enqueued =
        if observation.origin() == DataOrigin::Realtime && after.origin() == DataOrigin::Realtime {
            transaction.execute(
                "INSERT OR IGNORE INTO domain_work (
                id, work_kind, model_call_id, state, created_at
             ) VALUES (?1, 'model_call.completed', ?2, 'pending', ?3)",
                params![
                    Uuid::now_v7().to_string(),
                    model_call_id.to_string(),
                    timestamp_text(observation.observed_at()),
                ],
            )? > 0
        } else {
            false
        };

    Ok(ObservationCommitOutcome::Inserted {
        model_call_id,
        observation_id: observation.id(),
        canonical_created,
        projection_changed,
        downstream_work_enqueued,
    })
}

fn rebind_observation(
    observation: ModelCallObservation,
    model_call_id: ModelCallId,
) -> StorageResult<ModelCallObservation> {
    if observation.model_call_id() == model_call_id {
        return Ok(observation);
    }
    Ok(ModelCallObservation::new(
        observation.id(),
        model_call_id,
        observation.source_event_identity().clone(),
        observation.event_time().clone(),
        observation.observed_at(),
        observation.origin(),
        observation.fields().clone(),
    )?)
}

struct SourceBatchCommit {
    outcome: SourceBatchCommitOutcome,
    model_call_ids: Vec<ModelCallId>,
    source_health_changed: bool,
}

fn commit_source_batch(
    connection: &mut Connection,
    extraction: SourceExtraction,
    source_health: &StoredSourceHealth,
) -> StorageResult<SourceBatchCommit> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut outcome = SourceBatchCommitOutcome::default();
    let mut model_call_ids = Vec::new();
    for candidate in extraction.observations {
        let conversation_id = resolve_conversation(&transaction, &candidate)?;
        if candidate.kind() == SourceObservationKind::ConversationMetadata {
            if conversation_id.is_some() {
                outcome.metadata_observations += 1;
            }
            continue;
        }

        let relationship = conversation_id
            .map(|conversation_id| ModelCallRelationship::Conversation { conversation_id });
        let canonical_identity = candidate.source_event_identity().clone();
        let observation = candidate
            .into_model_call_observation(
                ModelCallId::new(),
                ModelCallObservationId::new(),
                relationship,
            )
            .map_err(|error| match error {
                SourceObservationError::InvalidDomain(error) => StorageError::Domain(error),
                SourceObservationError::NotCompletedModelCall => StorageError::InvalidStoredValue {
                    field: "source_observation.kind",
                },
            })?;
        match commit_observation_transaction(
            &transaction,
            ObservationWrite::new(canonical_identity, observation),
        )? {
            ObservationCommitOutcome::Inserted {
                model_call_id,
                downstream_work_enqueued,
                ..
            } => {
                outcome.inserted_observations += 1;
                outcome.downstream_work_enqueued += u64::from(downstream_work_enqueued);
                if !model_call_ids.contains(&model_call_id) {
                    model_call_ids.push(model_call_id);
                }
            }
            ObservationCommitOutcome::DuplicateSourceObservation { .. } => {
                outcome.duplicate_observations += 1;
            }
        }
    }
    let source_health_changed = upsert_source_health(&transaction, source_health)?;
    transaction.commit()?;
    Ok(SourceBatchCommit {
        outcome,
        model_call_ids,
        source_health_changed,
    })
}

pub(super) fn resolve_conversation(
    transaction: &Transaction<'_>,
    candidate: &SourceObservationCandidate,
) -> StorageResult<Option<ConversationId>> {
    resolve_conversation_range(
        transaction,
        candidate,
        candidate.observed_at(),
        candidate.observed_at(),
    )
}

pub(super) fn resolve_legacy_conversation_range(
    transaction: &Transaction<'_>,
    candidate: &SourceObservationCandidate,
    first_observed_at: UtcTimestamp,
    last_observed_at: UtcTimestamp,
) -> StorageResult<Option<ConversationId>> {
    if first_observed_at > last_observed_at {
        return Err(StorageError::LegacyConversationTimeRangeInvalid);
    }
    resolve_conversation_range(transaction, candidate, first_observed_at, last_observed_at)
}

fn resolve_conversation_range(
    transaction: &Transaction<'_>,
    candidate: &SourceObservationCandidate,
    first_observed_at: UtcTimestamp,
    last_observed_at: UtcTimestamp,
) -> StorageResult<Option<ConversationId>> {
    let source = candidate.source_event_identity().source();
    let mut keys = Vec::with_capacity(2);
    if let Some(value) = candidate.fields().conversation_source_id.as_ref() {
        keys.push(("conversation", value.value().as_str()));
    }
    if let Some(value) = candidate.fields().thread_source_id.as_ref() {
        keys.push(("thread", value.value().as_str()));
    }
    if keys.is_empty() {
        return Ok(None);
    }

    let mapped_id = mapped_conversation_id(transaction, source, &keys)?;
    let conversation_id = mapped_id.unwrap_or_else(ConversationId::new);
    let first_observed_at = timestamp_text(first_observed_at);
    let last_observed_at = timestamp_text(last_observed_at);
    transaction.execute(
        "INSERT INTO conversations (id, first_observed_at, last_observed_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT (id) DO UPDATE SET
             first_observed_at = min(first_observed_at, excluded.first_observed_at),
             last_observed_at = max(last_observed_at, excluded.last_observed_at)",
        params![
            conversation_id.to_string(),
            first_observed_at,
            last_observed_at
        ],
    )?;
    for (kind, value) in keys {
        let changed = transaction.execute(
            "INSERT INTO conversation_source_keys (
                telemetry_source_id, source_contract_version, key_kind, key_value,
                conversation_id, first_observed_at, last_observed_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (telemetry_source_id, source_contract_version, key_kind, key_value)
             DO UPDATE SET
                first_observed_at = min(first_observed_at, excluded.first_observed_at),
                last_observed_at = max(last_observed_at, excluded.last_observed_at)
             WHERE conversation_id = excluded.conversation_id",
            params![
                source.id().as_str(),
                source.contract_version().get(),
                kind,
                value,
                conversation_id.to_string(),
                first_observed_at,
                last_observed_at,
            ],
        )?;
        if changed == 0 {
            return Err(StorageError::ConversationIdentityCollision);
        }
    }

    if let Some(value) = candidate.fields().conversation_source_id.as_ref() {
        update_ranked_text(
            transaction,
            "conversations",
            "source",
            &conversation_id.to_string(),
            value.value().as_str(),
            value.provenance(),
        )?;
    }
    if let Some(value) = candidate.fields().thread_source_id.as_ref() {
        update_ranked_text(
            transaction,
            "conversations",
            "thread",
            &conversation_id.to_string(),
            value.value().as_str(),
            value.provenance(),
        )?;
    }
    Ok(Some(conversation_id))
}

fn mapped_conversation_id(
    transaction: &Transaction<'_>,
    source: &codex_monitor_core::SourceIdentity,
    keys: &[(&str, &str)],
) -> StorageResult<Option<ConversationId>> {
    let mut mapped_id = None;
    for (kind, value) in keys {
        let stored = transaction
            .query_row(
                "SELECT conversation_id FROM conversation_source_keys
                 WHERE telemetry_source_id = ?1
                   AND source_contract_version = ?2
                   AND key_kind = ?3
                   AND key_value = ?4",
                params![
                    source.id().as_str(),
                    source.contract_version().get(),
                    kind,
                    value,
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|value| {
                ConversationId::from_str(&value).map_err(|_| StorageError::InvalidStoredValue {
                    field: "conversation.id",
                })
            })
            .transpose()?;
        if let Some(stored) = stored {
            if mapped_id.is_some_and(|existing| existing != stored) {
                return Err(StorageError::ConversationIdentityCollision);
            }
            mapped_id = Some(stored);
        }
    }
    Ok(mapped_id)
}

fn upsert_source_health(
    transaction: &Transaction<'_>,
    health: &StoredSourceHealth,
) -> StorageResult<bool> {
    let status = match health.status {
        SourceHealthStatus::Starting => "starting",
        SourceHealthStatus::Healthy => "healthy",
        SourceHealthStatus::Degraded => "degraded",
        SourceHealthStatus::Unavailable => "unavailable",
        SourceHealthStatus::Stopped => "stopped",
    };
    let changed = transaction.execute(
        "INSERT INTO source_health (
            source_id, source_contract_version, status, last_accepted_at,
            last_error_code, last_error_occurred_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT (source_id, source_contract_version) DO UPDATE SET
            status = excluded.status,
            last_accepted_at = CASE
                WHEN source_health.last_accepted_at IS NULL
                  OR (excluded.last_accepted_at IS NOT NULL
                      AND excluded.last_accepted_at > source_health.last_accepted_at)
                THEN excluded.last_accepted_at
                ELSE source_health.last_accepted_at
            END,
            last_error_code = CASE
                WHEN source_health.last_error_occurred_at IS NULL
                  OR (excluded.last_error_occurred_at IS NOT NULL
                      AND excluded.last_error_occurred_at >= source_health.last_error_occurred_at)
                THEN excluded.last_error_code
                ELSE source_health.last_error_code
            END,
            last_error_occurred_at = CASE
                WHEN source_health.last_error_occurred_at IS NULL
                  OR (excluded.last_error_occurred_at IS NOT NULL
                      AND excluded.last_error_occurred_at >= source_health.last_error_occurred_at)
                THEN excluded.last_error_occurred_at
                ELSE source_health.last_error_occurred_at
            END,
            updated_at = excluded.updated_at
         WHERE excluded.updated_at >= source_health.updated_at",
        params![
            health.source_id.as_str(),
            health.source_contract_version.get(),
            status,
            health.last_accepted_at.map(timestamp_text),
            health
                .last_error_code
                .map(codex_monitor_core::SafeErrorCode::as_str),
            health.last_error_occurred_at.map(timestamp_text),
            timestamp_text(health.updated_at),
        ],
    )?;
    if changed == 0 {
        return Ok(false);
    }
    transaction.execute(
        "DELETE FROM source_health_supported_fields
         WHERE source_id = ?1 AND source_contract_version = ?2",
        params![
            health.source_id.as_str(),
            health.source_contract_version.get()
        ],
    )?;
    for field in &health.supported_fields {
        transaction.execute(
            "INSERT INTO source_health_supported_fields (
                source_id, source_contract_version, field_id
             ) VALUES (?1, ?2, ?3)",
            params![
                health.source_id.as_str(),
                health.source_contract_version.get(),
                field.as_str(),
            ],
        )?;
    }
    Ok(true)
}

fn replace_settings(
    connection: &mut Connection,
    expected_revision: u64,
    settings: AppSettings,
) -> StorageResult<StoredSettingsDocument> {
    settings.validate()?;
    let expected_revision =
        i64::try_from(expected_revision).map_err(|_| StorageError::InvalidStoredValue {
            field: "app_settings.revision",
        })?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = transaction.execute(
        "UPDATE app_settings SET
            revision = revision + 1,
            retention_days = ?2,
            notifications_enabled = ?3,
            notify_max_n = ?4,
            notify_all = ?5,
            locale = ?6,
            login_autostart = ?7,
            updated_at = ?8
         WHERE singleton = 1 AND revision = ?1",
        params![
            expected_revision,
            settings.retention.days(),
            settings.notifications_enabled,
            settings.notify_max_n.map(std::num::NonZeroU32::get),
            settings.notify_all,
            settings.locale.as_str(),
            settings.login_autostart,
            timestamp_text(settings.updated_at),
        ],
    )?;
    if changed == 0 {
        return Err(StorageError::SettingsRevisionConflict);
    }
    let locale_state_changed = transaction.execute(
        "UPDATE app_locale_initialization SET initialized = 1 WHERE singleton = 1",
        [],
    )?;
    if locale_state_changed != 1 {
        return Err(StorageError::InvalidStoredValue {
            field: "app_locale_initialization.singleton",
        });
    }
    for definition in BuiltInRuleRegistry::active_definitions() {
        transaction.execute(
            "UPDATE rules
             SET notify_max_n = ?3, notify_all = ?4
             WHERE rule_id = ?1 AND rule_version = ?2",
            params![
                definition.id.as_str(),
                definition.version.get(),
                settings.notify_max_n.map(std::num::NonZeroU32::get),
                settings.notify_all,
            ],
        )?;
    }
    let revision = transaction.query_row(
        "SELECT revision FROM app_settings WHERE singleton = 1",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    let revision = u64::try_from(revision).map_err(|_| StorageError::InvalidStoredValue {
        field: "app_settings.revision",
    })?;
    transaction.commit()?;
    Ok(StoredSettingsDocument {
        revision,
        settings,
        locale_initialized: true,
    })
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU32, NonZeroUsize};

    use codex_monitor_core::{
        ConversationSourceId, DataOrigin, EvidenceQuality, FieldProvenance, ModelName,
        ObservationFields, Observed, SafeErrorCode, SourceContractVersion, SourceEventIdentity,
        SourceFailure, SourceHealth, SourceHealthStatus, SourceId, SourceIdentity, SourcePriority,
        StableSourceEventId, ThreadSourceId, TokenCount, UtcTimestamp,
    };
    use tempfile::tempdir;

    use crate::{
        ModelCallCursor, ModelCallQuery, RetentionPolicy, RetentionRequest, StorageReader,
        StreamControlReason, StreamReceive, StreamSubscriptionStart,
    };

    use super::*;

    fn at(seconds: i128) -> UtcTimestamp {
        UtcTimestamp::from_unix_timestamp_nanos(seconds * 1_000_000_000).unwrap()
    }

    #[derive(Clone)]
    struct SyntheticSource {
        identity: SourceIdentity,
        health: SourceHealth,
    }

    impl SyntheticSource {
        fn new(now: UtcTimestamp) -> Result<Self, codex_monitor_core::ValidationError> {
            let identity = SourceIdentity::new(
                SourceId::new("synthetic-storage").unwrap(),
                SourceContractVersion::new(1).unwrap(),
            );
            let health = SourceHealth::new(identity.clone(), [], now)?;
            Ok(Self { identity, health })
        }

        fn extract(&self, input: SyntheticInput, observed_at: UtcTimestamp) -> SourceExtraction {
            let provenance = FieldProvenance::new(
                self.identity.clone(),
                observed_at,
                EvidenceQuality::Direct,
                SourcePriority::new(100),
            );
            let candidate = SourceObservationCandidate::new(
                SourceObservationKind::CompletedModelCall,
                SourceEventIdentity::from_stable_id(
                    self.identity.clone(),
                    &StableSourceEventId::new(input.response).unwrap(),
                ),
                Observed::new(at(10), provenance.clone()),
                observed_at,
                DataOrigin::Realtime,
                ObservationFields {
                    conversation_source_id: Some(Observed::new(
                        ConversationSourceId::new(input.conversation).unwrap(),
                        provenance.clone(),
                    )),
                    thread_source_id: Some(Observed::new(
                        ThreadSourceId::new(input.thread).unwrap(),
                        provenance.clone(),
                    )),
                    model: Some(Observed::new(
                        ModelName::new("synthetic-model").unwrap(),
                        provenance.clone(),
                    )),
                    input_tokens: Some(Observed::new(
                        TokenCount::new(1_024).unwrap(),
                        provenance.clone(),
                    )),
                    cached_input_tokens: Some(Observed::new(
                        TokenCount::new(512).unwrap(),
                        provenance.clone(),
                    )),
                    output_tokens: Some(Observed::new(
                        TokenCount::new(258).unwrap(),
                        provenance.clone(),
                    )),
                    reasoning_output_tokens: Some(Observed::new(
                        TokenCount::new(516).unwrap(),
                        provenance,
                    )),
                    ..ObservationFields::default()
                },
            )
            .unwrap();
            SourceExtraction {
                records_seen: 1,
                rejected_records: 0,
                ignored_records: 0,
                observations: vec![candidate],
            }
        }

        fn mark_committed(&mut self, committed_at: UtcTimestamp) {
            self.health.mark_accepted(committed_at);
        }

        fn mark_failure(&mut self, code: SafeErrorCode, occurred_at: UtcTimestamp) {
            self.health
                .mark_degraded(SourceFailure::new(code, occurred_at));
        }

        fn health(&self) -> &SourceHealth {
            &self.health
        }

        fn health_mut(&mut self) -> &mut SourceHealth {
            &mut self.health
        }
    }

    struct SyntheticInput {
        conversation: String,
        thread: String,
        response: String,
    }

    fn fixture() -> SyntheticInput {
        named_fixture(
            "synthetic-conversation-0001",
            "synthetic-thread-0001",
            "synthetic-response-0001",
        )
    }

    fn named_fixture(conversation: &str, thread: &str, response: &str) -> SyntheticInput {
        SyntheticInput {
            conversation: conversation.to_owned(),
            thread: thread.to_owned(),
            response: response.to_owned(),
        }
    }

    fn healthy_snapshot(source: &SyntheticSource, accepted_at: UtcTimestamp) -> StoredSourceHealth {
        let mut accepted = source.clone();
        accepted.mark_committed(accepted_at);
        StoredSourceHealth::from(accepted.health())
    }

    fn synthetic_candidate(
        source: &SyntheticSource,
        response: &str,
        observed_at: UtcTimestamp,
    ) -> SourceObservationCandidate {
        let mut extraction = source.extract(
            named_fixture(
                "synthetic-conversation-origin",
                "synthetic-thread-origin",
                response,
            ),
            observed_at,
        );
        assert_eq!(extraction.observations.len(), 1);
        extraction.observations.remove(0)
    }

    fn typed_observation_write(
        candidate: &SourceObservationCandidate,
        canonical_identity: SourceEventIdentity,
        origin: DataOrigin,
    ) -> ObservationWrite {
        let candidate = SourceObservationCandidate::new(
            candidate.kind(),
            candidate.source_event_identity().clone(),
            candidate.event_time().clone(),
            candidate.observed_at(),
            origin,
            candidate.fields().clone(),
        )
        .unwrap();
        let observation = candidate
            .into_model_call_observation(ModelCallId::new(), ModelCallObservationId::new(), None)
            .unwrap();
        ObservationWrite::new(canonical_identity, observation)
    }

    fn inserted_ids(
        outcome: ObservationCommitOutcome,
        expected_canonical_created: bool,
        expected_work_enqueued: bool,
    ) -> (ModelCallId, ModelCallObservationId) {
        match outcome {
            ObservationCommitOutcome::Inserted {
                model_call_id,
                observation_id,
                canonical_created,
                downstream_work_enqueued,
                ..
            } => {
                assert_eq!(canonical_created, expected_canonical_created);
                assert_eq!(downstream_work_enqueued, expected_work_enqueued);
                (model_call_id, observation_id)
            }
            duplicate @ ObservationCommitOutcome::DuplicateSourceObservation { .. } => {
                panic!("unexpected duplicate observation outcome: {duplicate:?}")
            }
        }
    }

    fn assert_historical_projection(
        database: &Path,
        model_call_id: ModelCallId,
        historical_observation_id: ModelCallObservationId,
        realtime_observation_id: ModelCallObservationId,
    ) {
        let reader = StorageReader::new(database.to_path_buf());
        let canonical = reader.model_call(model_call_id).unwrap().unwrap();
        assert_eq!(canonical.0.origin(), DataOrigin::Historical);

        let connection = Connection::open(database).unwrap();
        let stored_origin = |observation_id: ModelCallObservationId| {
            connection
                .query_row(
                    "SELECT origin FROM model_call_observations WHERE id = ?1",
                    [observation_id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap()
        };
        assert_eq!(stored_origin(historical_observation_id), "historical");
        assert_eq!(stored_origin(realtime_observation_id), "realtime");

        let observation_counts = connection
            .query_row(
                "SELECT count(*), count(DISTINCT hex(identity_digest)),
                        count(DISTINCT model_call_id)
                 FROM model_call_observations",
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
        assert_eq!(observation_counts, (2, 2, 1));
        let work_count: i64 = connection
            .query_row("SELECT count(*) FROM domain_work", [], |row| row.get(0))
            .unwrap();
        assert_eq!(work_count, 0);
    }

    fn seed_retention_audit(database: &Path, model_call_id: &str) {
        let connection = Connection::open(database).unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        let event_at = "2026-01-01T00:00:00.000000000Z";
        assert_eq!(
            connection
                .execute(
                    "UPDATE domain_work
                     SET state = 'completed', started_at = ?1, completed_at = ?1
                     WHERE model_call_id = ?2",
                    params![event_at, model_call_id],
                )
                .unwrap(),
            1
        );
        connection
            .execute(
                "INSERT INTO rules (
                    rule_id, rule_version, rule_kind, enabled, notify_max_n, notify_all
                 ) VALUES ('reasoning-pattern', 1, 'suspected_reasoning_limit_pattern', 1, 1, 0)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO rule_evaluations (
                    id, rule_id, rule_version, model_call_id, matched,
                    reasoning_output_tokens, pattern_n, event_at, source_quality, evaluated_at
                 ) VALUES (
                    'synthetic-evaluation', 'reasoning-pattern', 1, ?1, 1,
                    516, 1, ?2, 'direct', ?2
                 )",
                params![model_call_id, event_at],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO alerts (
                    id, evaluation_id, model_call_id, rule_id, rule_version,
                    reasoning_output_tokens, pattern_n, event_at, source_quality, created_at
                 ) VALUES (
                    'synthetic-alert', 'synthetic-evaluation', ?1, 'reasoning-pattern', 1,
                    516, 1, ?2, 'direct', ?2
                 )",
                params![model_call_id, event_at],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO action_outbox (
                    id, alert_id, action_type, parameter_schema_version,
                    idempotency_key_version, idempotency_key_bytes, state,
                    reasoning_output_tokens, pattern_n, created_at, not_before
                 ) VALUES (
                    'synthetic-action', 'synthetic-alert', 'system_notification',
                    1, 1, zeroblob(32), 'pending', 516, 1, ?1, ?1
                 )",
                [event_at],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO legacy_import_audit (
                    id, source_fingerprint, detected_schema, imported_model_calls,
                    imported_conversations, completed_at
                 ) VALUES ('synthetic-import', zeroblob(32), 'structured_v1', 1, 1, ?1)",
                [event_at],
            )
            .unwrap();
    }

    fn mark_pending_action_unavailable(database: &Path) {
        let connection = Connection::open(database).unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        connection
            .execute(
                "UPDATE action_outbox
                 SET state = 'unavailable', completed_at = ?1,
                     last_error_code = 'invalid_parameters'
                 WHERE id = 'synthetic-action'",
                ["2026-01-01T00:00:01.000000000Z"],
            )
            .unwrap();
    }

    fn retention_counts(database: &Path) -> (i64, i64, i64) {
        Connection::open(database)
            .unwrap()
            .query_row(
                "SELECT
                    (SELECT count(*) FROM model_calls),
                    (SELECT count(*) FROM legacy_import_audit),
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
            .unwrap()
    }

    fn retention_recovery_state(database: &Path) -> (Vec<String>, i64, i64, i64, String) {
        let connection = Connection::open(database).unwrap();
        let model_call_ids = {
            let mut statement = connection
                .prepare("SELECT id FROM model_calls ORDER BY id")
                .unwrap();
            statement
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        let (import_audits, migration_audits, actions, action_state) = connection
            .query_row(
                "SELECT
                    (SELECT count(*) FROM legacy_import_audit),
                    (SELECT count(*) FROM schema_migrations),
                    (SELECT count(*) FROM action_outbox),
                    (SELECT state FROM action_outbox WHERE id = 'synthetic-action')",
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
        (
            model_call_ids,
            import_audits,
            migration_audits,
            actions,
            action_state,
        )
    }

    struct RetentionRecoveryFixture {
        protected_model_call_id: String,
        deletable_model_call_ids: Vec<String>,
        all_model_call_ids: Vec<String>,
        migration_audits: i64,
    }

    impl RetentionRecoveryFixture {
        fn expected_state(
            &self,
            model_call_ids: Vec<String>,
        ) -> (Vec<String>, i64, i64, i64, String) {
            (
                model_call_ids,
                1,
                self.migration_audits,
                1,
                "pending".to_owned(),
            )
        }
    }

    fn retention_request(batch_size: u32) -> RetentionRequest {
        RetentionRequest::new(
            RetentionPolicy::Days30,
            UtcTimestamp::parse_rfc3339("2027-01-01T00:00:00Z").unwrap(),
            batch_size,
        )
        .unwrap()
    }

    async fn arrange_retention_recovery(
        database: &Path,
    ) -> (StorageWriter, RetentionRecoveryFixture) {
        let writer = StorageWriter::open(StorageConfig::new(database.to_path_buf())).unwrap();
        let source = SyntheticSource::new(at(1)).unwrap();
        for suffix in ["delete-a", "delete-b", "protected"] {
            writer
                .commit_source_batch(
                    source.extract(
                        named_fixture(
                            &format!("synthetic-conversation-{suffix}"),
                            &format!("synthetic-thread-{suffix}"),
                            &format!("synthetic-response-{suffix}"),
                        ),
                        at(2),
                    ),
                    healthy_snapshot(&source, at(2)),
                )
                .await
                .unwrap();
        }

        let reader = StorageReader::new(database.to_path_buf());
        let mut all_model_call_ids = reader
            .model_calls(ModelCallQuery::default())
            .unwrap()
            .into_iter()
            .map(|stored| stored.0.id().to_string())
            .collect::<Vec<_>>();
        assert_eq!(all_model_call_ids.len(), 3);
        let protected_model_call_id = all_model_call_ids[0].clone();
        seed_retention_audit(database, &protected_model_call_id);

        let failure_injector = Connection::open(database).unwrap();
        let completed_at = "2026-01-01T00:00:00.000000000Z";
        assert_eq!(
            failure_injector
                .execute(
                    "UPDATE domain_work
                     SET state = 'completed', started_at = ?1, completed_at = ?1
                     WHERE model_call_id <> ?2",
                    params![completed_at, protected_model_call_id],
                )
                .unwrap(),
            2
        );
        let deletable_model_call_ids = {
            let mut statement = failure_injector
                .prepare(
                    "SELECT id FROM model_calls
                     WHERE id <> ?1
                     ORDER BY event_at, id",
                )
                .unwrap();
            statement
                .query_map([&protected_model_call_id], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(deletable_model_call_ids.len(), 2);
        let abort_model_call_id = ModelCallId::from_str(&deletable_model_call_ids[1]).unwrap();
        failure_injector
            .execute_batch(&format!(
                "CREATE TRIGGER test_abort_retention_batch
                 BEFORE DELETE ON model_calls
                 WHEN OLD.id = '{abort_model_call_id}'
                 BEGIN
                     SELECT RAISE(ABORT, 'injected retention batch failure');
                 END;"
            ))
            .unwrap();
        drop(failure_injector);

        all_model_call_ids.sort();
        let initial_state = retention_recovery_state(database);
        assert_eq!(initial_state.0, all_model_call_ids);
        assert_eq!(initial_state.1, 1);
        assert_eq!(initial_state.3, 1);
        assert_eq!(initial_state.4, "pending");
        assert!(initial_state.2 > 0);

        let fixture = RetentionRecoveryFixture {
            protected_model_call_id,
            deletable_model_call_ids,
            all_model_call_ids,
            migration_audits: initial_state.2,
        };
        (writer, fixture)
    }

    async fn assert_failed_retention_is_atomic(
        writer: &StorageWriter,
        database: &Path,
        fixture: &RetentionRecoveryFixture,
    ) {
        assert!(matches!(
            writer.apply_retention(retention_request(2)).await,
            Err(StorageError::Database(_))
        ));
        assert_eq!(
            retention_recovery_state(database),
            fixture.expected_state(fixture.all_model_call_ids.clone())
        );
        writer.shutdown().await.unwrap();
    }

    async fn assert_bounded_retention_resumes_after_restarts(
        database: &Path,
        fixture: &RetentionRecoveryFixture,
    ) {
        let reopened = StorageWriter::open(StorageConfig::new(database.to_path_buf())).unwrap();
        assert_eq!(
            retention_recovery_state(database),
            fixture.expected_state(fixture.all_model_call_ids.clone())
        );
        Connection::open(database)
            .unwrap()
            .execute_batch("DROP TRIGGER test_abort_retention_batch")
            .unwrap();

        let first_batch = reopened
            .apply_retention(retention_request(1))
            .await
            .unwrap();
        assert_eq!(first_batch.deleted_model_calls, 1);
        assert!(first_batch.more_eligible);
        let mut after_first_batch = vec![
            fixture.protected_model_call_id.clone(),
            fixture.deletable_model_call_ids[1].clone(),
        ];
        after_first_batch.sort();
        assert_eq!(
            retention_recovery_state(database),
            fixture.expected_state(after_first_batch)
        );
        reopened.shutdown().await.unwrap();

        let final_writer = StorageWriter::open(StorageConfig::new(database.to_path_buf())).unwrap();
        let final_batch = final_writer
            .apply_retention(retention_request(1))
            .await
            .unwrap();
        assert_eq!(final_batch.deleted_model_calls, 1);
        assert!(!final_batch.more_eligible);
        assert_eq!(
            retention_recovery_state(database),
            fixture.expected_state(vec![fixture.protected_model_call_id.clone()])
        );
        final_writer.shutdown().await.unwrap();
    }

    fn active_rule_settings(database: &Path) -> (bool, Option<i64>, bool) {
        Connection::open(database)
            .unwrap()
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
            .unwrap()
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn source_batch_is_atomic_deduplicated_and_queryable() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("writer.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let source = SyntheticSource::new(at(1)).unwrap();
        let first_boundary = writer.change_feed().current_cursor();

        let first = writer
            .commit_source_batch(
                source.extract(fixture(), at(2)),
                healthy_snapshot(&source, at(2)),
            )
            .await
            .unwrap();
        assert_eq!(first.inserted_observations, 1);
        assert_eq!(first.downstream_work_enqueued, 1);
        let StreamSubscriptionStart::Active(mut first_events) = writer
            .change_feed()
            .subscribe(Some(&first_boundary))
            .unwrap()
        else {
            panic!("source batch boundary must remain replayable");
        };
        let StreamReceive::Change(model_call_event) = first_events.recv().await else {
            panic!("inserted source observation must publish a model call");
        };
        let StreamReceive::Change(source_event) = first_events.recv().await else {
            panic!("committed source health must publish after its model call");
        };
        assert!(matches!(
            model_call_event.change,
            StorageChange::ModelCall(_)
        ));
        assert!(matches!(
            source_event.change,
            StorageChange::SourceHealth { .. }
        ));

        let duplicate_boundary = writer.change_feed().current_cursor();
        let duplicate = writer
            .commit_source_batch(
                source.extract(fixture(), at(3)),
                healthy_snapshot(&source, at(3)),
            )
            .await
            .unwrap();
        assert_eq!(duplicate.duplicate_observations, 1);
        assert_eq!(duplicate.downstream_work_enqueued, 0);
        let StreamSubscriptionStart::Active(mut duplicate_events) = writer
            .change_feed()
            .subscribe(Some(&duplicate_boundary))
            .unwrap()
        else {
            panic!("duplicate source boundary must remain replayable");
        };
        let StreamReceive::Change(duplicate_source_event) = duplicate_events.recv().await else {
            panic!("newer source health must still publish");
        };
        assert!(matches!(
            duplicate_source_event.change,
            StorageChange::SourceHealth { .. }
        ));
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                duplicate_events.recv()
            )
            .await
            .is_err(),
            "duplicate source observations must not republish model calls"
        );

        let reader = StorageReader::new(database.clone());
        let calls = reader.model_calls(ModelCallQuery::default()).unwrap();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].0.fields().relationship.is_some());
        assert_eq!(
            reader
                .conversations(NonZeroUsize::new(10).unwrap())
                .unwrap()
                .len(),
            1
        );
        let health = reader.source_health().unwrap();
        assert_eq!(health.len(), 1);
        assert_eq!(health[0].status, SourceHealthStatus::Healthy);
        let pragmas = reader.pragmas().unwrap();
        assert_eq!(pragmas.journal_mode.to_ascii_lowercase(), "wal");
        assert!(pragmas.foreign_keys);
        assert!(pragmas.query_only);
        assert_eq!(pragmas.busy_timeout_millis, 5_000);

        let connection = Connection::open(&database).unwrap();
        let work_count: i64 = connection
            .query_row("SELECT count(*) FROM domain_work", [], |row| row.get(0))
            .unwrap();
        assert_eq!(work_count, 1);

        writer.shutdown().await.unwrap();
        let error = writer
            .commit_source_batch(
                source.extract(fixture(), at(4)),
                healthy_snapshot(&source, at(4)),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, StorageError::WriterClosed));
    }

    #[tokio::test]
    async fn historical_canonical_call_cannot_be_promoted_into_realtime_work() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("historical-origin.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let source = SyntheticSource::new(at(1)).unwrap();

        let historical_candidate =
            synthetic_candidate(&source, "synthetic-response-history", at(2));
        let historical_source_identity = historical_candidate.source_event_identity().clone();
        let canonical_identity = historical_source_identity.clone();
        let historical_write = typed_observation_write(
            &historical_candidate,
            canonical_identity.clone(),
            DataOrigin::Historical,
        );

        let realtime_candidate =
            synthetic_candidate(&source, "synthetic-response-realtime-evidence", at(3));
        let realtime_source_identity = realtime_candidate.source_event_identity().clone();
        assert_ne!(historical_source_identity, realtime_source_identity);
        let realtime_write = typed_observation_write(
            &realtime_candidate,
            canonical_identity,
            DataOrigin::Realtime,
        );

        let historical_outcome = writer.commit_observation(historical_write).await.unwrap();
        let (canonical_model_call_id, historical_observation_id) =
            inserted_ids(historical_outcome, true, false);
        let realtime_outcome = writer.commit_observation(realtime_write).await.unwrap();
        let (realtime_model_call_id, realtime_observation_id) =
            inserted_ids(realtime_outcome, false, false);
        assert_eq!(realtime_model_call_id, canonical_model_call_id);
        assert_historical_projection(
            &database,
            canonical_model_call_id,
            historical_observation_id,
            realtime_observation_id,
        );

        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn new_realtime_canonical_call_still_enqueues_work() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("realtime-origin.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let source = SyntheticSource::new(at(1)).unwrap();
        let candidate = synthetic_candidate(&source, "synthetic-response-realtime-call", at(2));
        let canonical_identity = candidate.source_event_identity().clone();
        let write = typed_observation_write(&candidate, canonical_identity, DataOrigin::Realtime);

        let outcome = writer.commit_observation(write).await.unwrap();
        let (model_call_id, _) = inserted_ids(outcome, true, true);
        let canonical = StorageReader::new(database.clone())
            .model_call(model_call_id)
            .unwrap()
            .unwrap();
        assert_eq!(canonical.0.origin(), DataOrigin::Realtime);
        let work_count: i64 = Connection::open(&database)
            .unwrap()
            .query_row("SELECT count(*) FROM domain_work", [], |row| row.get(0))
            .unwrap();
        assert_eq!(work_count, 1);

        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn settings_replace_is_full_document_compare_and_swap() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("settings.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let reader = StorageReader::new(database);
        let initial = reader.settings().unwrap();
        assert_eq!(initial.revision, 1);

        let replacement = AppSettings {
            retention: super::super::RetentionPolicy::Days30,
            notifications_enabled: false,
            notify_max_n: None,
            notify_all: true,
            locale: super::super::AppLocale::ZhCn,
            login_autostart: true,
            updated_at: at(10),
        };
        let saved = writer
            .replace_settings(initial.revision, replacement.clone())
            .await
            .unwrap();
        assert_eq!(saved.revision, 2);
        assert_eq!(reader.settings().unwrap(), saved);

        let stale = writer
            .replace_settings(initial.revision, replacement)
            .await
            .unwrap_err();
        assert!(matches!(stale, StorageError::SettingsRevisionConflict));
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn settings_saved_before_rule_registration_seed_the_first_rule_threshold() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("settings-before-rules.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let reader = StorageReader::new(database.clone());
        let initial = reader.settings().unwrap();
        let replacement = AppSettings {
            retention: RetentionPolicy::Days90,
            notifications_enabled: true,
            notify_max_n: Some(NonZeroU32::new(7).unwrap()),
            notify_all: false,
            locale: super::super::AppLocale::EnUs,
            login_autostart: false,
            updated_at: at(10),
        };

        writer
            .replace_settings(initial.revision, replacement)
            .await
            .unwrap();
        let rule_count: i64 = Connection::open(&database)
            .unwrap()
            .query_row("SELECT count(*) FROM rules", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rule_count, 0);

        writer.register_builtin_rules().await.unwrap();

        assert_eq!(active_rule_settings(&database), (true, Some(7), false));
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn settings_update_projects_to_active_rules_without_changing_enablement() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("settings-rule-projection.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        writer.register_builtin_rules().await.unwrap();
        Connection::open(&database)
            .unwrap()
            .execute(
                "UPDATE rules SET enabled = 0
                 WHERE rule_id = 'suspected-reasoning-limit-pattern' AND rule_version = 1",
                [],
            )
            .unwrap();
        let reader = StorageReader::new(database.clone());
        let initial = reader.settings().unwrap();
        let all_matches = AppSettings {
            retention: RetentionPolicy::Days30,
            notifications_enabled: false,
            notify_max_n: None,
            notify_all: true,
            locale: super::super::AppLocale::ZhCn,
            login_autostart: true,
            updated_at: at(10),
        };

        let saved = writer
            .replace_settings(initial.revision, all_matches)
            .await
            .unwrap();
        assert_eq!(active_rule_settings(&database), (false, None, true));

        let stale_replacement = AppSettings {
            retention: RetentionPolicy::Days365,
            notifications_enabled: true,
            notify_max_n: Some(NonZeroU32::new(9).unwrap()),
            notify_all: false,
            locale: super::super::AppLocale::EnUs,
            login_autostart: false,
            updated_at: at(20),
        };
        let stale = writer
            .replace_settings(initial.revision, stale_replacement)
            .await
            .unwrap_err();
        assert!(matches!(stale, StorageError::SettingsRevisionConflict));
        assert_eq!(reader.settings().unwrap(), saved);
        assert_eq!(active_rule_settings(&database), (false, None, true));
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn older_source_health_cannot_overwrite_a_newer_snapshot() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("source-health-order.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let older_source = SyntheticSource::new(at(1)).unwrap();
        let mut newer_source = SyntheticSource::new(at(10)).unwrap();
        newer_source.mark_committed(at(20));
        let newer = StoredSourceHealth::from(newer_source.health());

        writer.upsert_source_health(newer.clone()).await.unwrap();
        writer
            .upsert_source_health(StoredSourceHealth::from(older_source.health()))
            .await
            .unwrap();

        assert_eq!(
            StorageReader::new(database).source_health().unwrap(),
            [newer]
        );
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn fresh_runtime_health_preserves_monotonic_diagnostic_history() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("source-health-restart.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let mut previous = SyntheticSource::new(at(1)).unwrap();
        previous.mark_committed(at(2));
        previous.mark_failure(SafeErrorCode::OtlpJsonDecodeFailed, at(3));
        let mut current = SyntheticSource::new(at(10)).unwrap();
        current.health_mut().mark_available(at(11));

        writer
            .upsert_source_health(StoredSourceHealth::from(previous.health()))
            .await
            .unwrap();
        writer
            .upsert_source_health(StoredSourceHealth::from(current.health()))
            .await
            .unwrap();

        let stored = StorageReader::new(database).source_health().unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].status, SourceHealthStatus::Healthy);
        assert_eq!(stored[0].updated_at, at(11));
        assert_eq!(stored[0].last_accepted_at, Some(at(2)));
        assert_eq!(
            stored[0].last_error_code,
            Some(SafeErrorCode::OtlpJsonDecodeFailed)
        );
        assert_eq!(stored[0].last_error_occurred_at, Some(at(3)));
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_retransmissions_create_one_observation_and_one_work_item() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("concurrent.sqlite3");
        let mut config = StorageConfig::new(database.clone());
        config.writer_queue_capacity = NonZeroUsize::new(4).unwrap();
        let writer = StorageWriter::open(config).unwrap();
        let source = SyntheticSource::new(at(1)).unwrap();

        let mut tasks = Vec::new();
        for index in 0..24 {
            let writer = writer.clone();
            let source = source.clone();
            tasks.push(tokio::spawn(async move {
                let observed_at = at(100 + index);
                writer
                    .commit_source_batch(
                        source.extract(fixture(), observed_at),
                        healthy_snapshot(&source, observed_at),
                    )
                    .await
                    .unwrap()
            }));
        }
        let mut inserted = 0;
        let mut duplicates = 0;
        for task in tasks {
            let outcome = task.await.unwrap();
            inserted += outcome.inserted_observations;
            duplicates += outcome.duplicate_observations;
        }
        assert_eq!(inserted, 1);
        assert_eq!(duplicates, 23);

        let connection = Connection::open(database).unwrap();
        let counts = connection
            .query_row(
                "SELECT
                    (SELECT count(*) FROM model_calls),
                    (SELECT count(*) FROM model_call_observations),
                    (SELECT count(*) FROM domain_work)",
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
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn model_call_cursor_is_stable_when_event_times_match() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("stable-cursor.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let source = SyntheticSource::new(at(1)).unwrap();
        let mut observations = Vec::new();
        for suffix in ["a", "b", "c"] {
            observations.extend(
                source
                    .extract(
                        named_fixture(
                            &format!("synthetic-conversation-{suffix}"),
                            &format!("synthetic-thread-{suffix}"),
                            &format!("synthetic-response-{suffix}"),
                        ),
                        at(2),
                    )
                    .observations,
            );
        }
        writer
            .commit_source_batch(
                SourceExtraction {
                    records_seen: 3,
                    rejected_records: 0,
                    ignored_records: 0,
                    observations,
                },
                healthy_snapshot(&source, at(2)),
            )
            .await
            .unwrap();

        let reader = StorageReader::new(database);
        let first_page = reader
            .model_calls(ModelCallQuery::new(2, None).unwrap())
            .unwrap();
        assert_eq!(first_page.len(), 2);
        let last = &first_page[1].0;
        let cursor = ModelCallCursor::new(*last.event_time().value(), last.id());
        let second_page = reader
            .model_calls(ModelCallQuery::new(2, Some(cursor)).unwrap())
            .unwrap();
        assert_eq!(second_page.len(), 1);
        assert!(
            first_page
                .iter()
                .all(|call| call.0.id() != second_page[0].0.id())
        );
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn conversation_key_collision_rolls_back_the_entire_source_batch() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("atomic-rollback.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let source = SyntheticSource::new(at(1)).unwrap();
        let mut observations = Vec::new();
        for request in [
            named_fixture(
                "synthetic-conversation-a",
                "synthetic-thread-a",
                "synthetic-response-a",
            ),
            named_fixture(
                "synthetic-conversation-b",
                "synthetic-thread-b",
                "synthetic-response-b",
            ),
            named_fixture(
                "synthetic-conversation-a",
                "synthetic-thread-b",
                "synthetic-response-c",
            ),
        ] {
            observations.extend(source.extract(request, at(2)).observations);
        }
        let extraction = SourceExtraction {
            records_seen: 3,
            rejected_records: 0,
            ignored_records: 0,
            observations,
        };
        let event_boundary = writer.change_feed().current_cursor();
        let error = writer
            .commit_source_batch(extraction, healthy_snapshot(&source, at(2)))
            .await
            .unwrap_err();
        assert!(matches!(error, StorageError::ConversationIdentityCollision));
        let StreamSubscriptionStart::Active(mut events) = writer
            .change_feed()
            .subscribe(Some(&event_boundary))
            .unwrap()
        else {
            panic!("rolled-back source boundary must remain active");
        };
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), events.recv())
                .await
                .is_err(),
            "rolled-back source batches must not publish events"
        );

        let connection = Connection::open(database).unwrap();
        for table in [
            "conversations",
            "conversation_source_keys",
            "model_calls",
            "model_call_observations",
            "domain_work",
            "source_health",
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0, "{table} must roll back");
        }
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn retention_is_bounded_and_preserves_action_and_import_audit_rows() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("retention.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let source = SyntheticSource::new(at(1)).unwrap();
        writer
            .commit_source_batch(
                source.extract(fixture(), at(2)),
                healthy_snapshot(&source, at(2)),
            )
            .await
            .unwrap();
        let reader = StorageReader::new(database.clone());
        let model_call_id = reader.model_calls(ModelCallQuery::default()).unwrap()[0]
            .0
            .id()
            .to_string();

        seed_retention_audit(&database, &model_call_id);

        let request = RetentionRequest::new(
            RetentionPolicy::Days30,
            UtcTimestamp::parse_rfc3339("2027-01-01T00:00:00Z").unwrap(),
            10,
        )
        .unwrap();
        let preserved_boundary = writer.change_feed().current_cursor();
        let preserved = writer.apply_retention(request).await.unwrap();
        assert_eq!(preserved.deleted_model_calls, 0);
        assert_eq!(retention_counts(&database), (1, 1, 1));
        assert!(
            reader
                .model_call(ModelCallId::from_str(&model_call_id).unwrap())
                .unwrap()
                .is_some()
        );
        let StreamSubscriptionStart::Active(mut preserved_events) = writer
            .change_feed()
            .subscribe(Some(&preserved_boundary))
            .unwrap()
        else {
            panic!("no-op retention boundary must remain active");
        };
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                preserved_events.recv()
            )
            .await
            .is_err(),
            "no-op retention must not force a resynchronization"
        );

        mark_pending_action_unavailable(&database);
        let cleaned_boundary = writer.change_feed().current_cursor();
        let cleaned = writer.apply_retention(request).await.unwrap();
        assert_eq!(cleaned.deleted_model_calls, 1);
        assert!(matches!(
            writer
                .change_feed()
                .subscribe(Some(&cleaned_boundary))
                .unwrap(),
            StreamSubscriptionStart::Control(StreamControlReason::MissedEvent)
        ));

        assert_eq!(retention_counts(&database), (0, 1, 0));
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn retention_rollback_and_bounded_progress_survive_writer_restarts() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("retention-recovery.sqlite3");
        let (writer, fixture) = arrange_retention_recovery(&database).await;

        assert_failed_retention_is_atomic(&writer, &database, &fixture).await;
        assert_bounded_retention_resumes_after_restarts(&database, &fixture).await;
    }
}
