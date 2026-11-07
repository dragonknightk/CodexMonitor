use std::num::{NonZeroU16, NonZeroU32, NonZeroU64, NonZeroUsize};
use std::path::PathBuf;

use codex_monitor_core::{
    ActionErrorCode, ActionId, ActionLifecycleState, ActionParameters, ActionRequest,
    ActionSuppressionReason, AlertId, BuiltInRuleId, ConversationId, EvidenceQuality, FieldId,
    ModelCall, ModelCallId, ModelCallObservation, ModelCallObservationId,
    NotificationCoalescingWindowId, NotificationEligibility, PatternN, RegisteredActionType,
    RuleEvaluationKey, RuleVersion, SafeErrorCode, SourceContractVersion, SourceEventIdentity,
    SourceHealth, SourceHealthStatus, SourceId, TokenCount, UtcTimestamp,
};
use uuid::Uuid;

use super::{StorageError, StorageResult};

pub const DEFAULT_WRITER_QUEUE_CAPACITY: usize = 256;
pub const DEFAULT_RETENTION_BATCH_SIZE: u32 = 500;
pub const MAX_RETENTION_BATCH_SIZE: u32 = 10_000;
pub const MAX_ACTION_RECOVERY_BATCH_SIZE: u32 = 500;
pub const MAX_MODEL_CALL_QUERY_LIMIT: usize = 500;
pub const NOTIFICATION_COALESCING_WINDOW_SECONDS: i64 = 30;

/// Configuration used to open local storage.
#[derive(Clone, Debug)]
pub struct StorageConfig {
    pub database_path: PathBuf,
    pub writer_queue_capacity: NonZeroUsize,
}

impl StorageConfig {
    /// Create the default bounded writer configuration.
    ///
    /// # Panics
    ///
    /// The compile-time queue constant is asserted to be non-zero.
    #[must_use]
    pub fn new(database_path: PathBuf) -> Self {
        Self {
            database_path,
            writer_queue_capacity: NonZeroUsize::new(DEFAULT_WRITER_QUEUE_CAPACITY)
                .expect("default writer capacity is non-zero"),
        }
    }
}

/// A typed observation together with the canonical identity it contributes to.
#[derive(Clone, Debug)]
pub struct ObservationWrite {
    canonical_identity: SourceEventIdentity,
    observation: ModelCallObservation,
}

impl ObservationWrite {
    #[must_use]
    pub const fn new(
        canonical_identity: SourceEventIdentity,
        observation: ModelCallObservation,
    ) -> Self {
        Self {
            canonical_identity,
            observation,
        }
    }

    #[must_use]
    pub const fn canonical_identity(&self) -> &SourceEventIdentity {
        &self.canonical_identity
    }

    #[must_use]
    pub const fn observation(&self) -> &ModelCallObservation {
        &self.observation
    }

    pub(crate) fn into_parts(self) -> (SourceEventIdentity, ModelCallObservation) {
        (self.canonical_identity, self.observation)
    }
}

/// Provable result of one idempotent observation transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationCommitOutcome {
    Inserted {
        model_call_id: ModelCallId,
        observation_id: ModelCallObservationId,
        canonical_created: bool,
        projection_changed: bool,
        downstream_work_enqueued: bool,
    },
    DuplicateSourceObservation {
        model_call_id: ModelCallId,
        observation_id: ModelCallObservationId,
        downstream_work_enqueued: bool,
    },
}

/// Aggregate result of one all-or-nothing source batch transaction.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SourceBatchCommitOutcome {
    pub inserted_observations: u64,
    pub duplicate_observations: u64,
    pub metadata_observations: u64,
    pub downstream_work_enqueued: u64,
}

/// Fenced ownership of one durable `model_call.completed` work item.
///
/// The claim timestamp is also the v1 fencing token stored in `domain_work`.
/// Only the writer actor can construct a claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuleWorkClaim {
    work_id: Uuid,
    model_call_id: ModelCallId,
    claimed_at: UtcTimestamp,
}

impl RuleWorkClaim {
    pub(crate) const fn new(
        work_id: Uuid,
        model_call_id: ModelCallId,
        claimed_at: UtcTimestamp,
    ) -> Self {
        Self {
            work_id,
            model_call_id,
            claimed_at,
        }
    }

    /// Return the durable work identifier.
    #[must_use]
    pub const fn work_id(self) -> Uuid {
        self.work_id
    }

    /// Return the canonical model call owned by this claim.
    #[must_use]
    pub const fn model_call_id(self) -> ModelCallId {
        self.model_call_id
    }

    /// Return the fencing token written when this work was claimed.
    #[must_use]
    pub const fn claimed_at(self) -> UtcTimestamp {
        self.claimed_at
    }
}

/// Opaque identity of one action worker lease owner.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ActionLeaseOwner(Uuid);

impl ActionLeaseOwner {
    /// Generate a new lease-owner identity.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Return the underlying UUID for diagnostics and persistence.
    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for ActionLeaseOwner {
    fn default() -> Self {
        Self::new()
    }
}

/// Validated time window for claiming one pending action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActionLeaseWindow {
    claimed_at: UtcTimestamp,
    expires_at: UtcTimestamp,
}

impl ActionLeaseWindow {
    /// Validate a strictly positive lease window.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::InvalidActionLeaseWindow`] unless `expires_at`
    /// is later than `claimed_at`.
    pub fn new(claimed_at: UtcTimestamp, expires_at: UtcTimestamp) -> StorageResult<Self> {
        if claimed_at >= expires_at {
            return Err(StorageError::InvalidActionLeaseWindow);
        }
        Ok(Self {
            claimed_at,
            expires_at,
        })
    }

    #[must_use]
    pub const fn claimed_at(self) -> UtcTimestamp {
        self.claimed_at
    }

    #[must_use]
    pub const fn expires_at(self) -> UtcTimestamp {
        self.expires_at
    }
}

/// Fencing identity for one leased pending action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActionClaimToken {
    action_id: ActionId,
    lease_owner: ActionLeaseOwner,
    lease_generation: NonZeroU64,
}

impl ActionClaimToken {
    pub(crate) const fn new(
        action_id: ActionId,
        lease_owner: ActionLeaseOwner,
        lease_generation: NonZeroU64,
    ) -> Self {
        Self {
            action_id,
            lease_owner,
            lease_generation,
        }
    }

    #[must_use]
    pub const fn action_id(self) -> ActionId {
        self.action_id
    }

    #[must_use]
    pub const fn lease_owner(self) -> ActionLeaseOwner {
        self.lease_owner
    }

    #[must_use]
    pub const fn lease_generation(self) -> NonZeroU64 {
        self.lease_generation
    }
}

/// A typed pending action owned by one fenced lease.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimedAction {
    token: ActionClaimToken,
    request: ActionRequest,
    grouped: bool,
    claimed_at: UtcTimestamp,
    lease_expires_at: UtcTimestamp,
}

impl ClaimedAction {
    pub(crate) const fn new(
        token: ActionClaimToken,
        request: ActionRequest,
        grouped: bool,
        claimed_at: UtcTimestamp,
        lease_expires_at: UtcTimestamp,
    ) -> Self {
        Self {
            token,
            request,
            grouped,
            claimed_at,
            lease_expires_at,
        }
    }

    #[must_use]
    pub const fn token(&self) -> ActionClaimToken {
        self.token
    }

    #[must_use]
    pub const fn request(&self) -> &ActionRequest {
        &self.request
    }

    #[must_use]
    pub const fn grouped(&self) -> bool {
        self.grouped
    }

    #[must_use]
    pub const fn claimed_at(&self) -> UtcTimestamp {
        self.claimed_at
    }

    #[must_use]
    pub const fn lease_expires_at(&self) -> UtcTimestamp {
        self.lease_expires_at
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        ActionClaimToken,
        ActionRequest,
        bool,
        UtcTimestamp,
        UtcTimestamp,
    ) {
        (
            self.token,
            self.request,
            self.grouped,
            self.claimed_at,
            self.lease_expires_at,
        )
    }
}

/// One action whose `started` state and attempt audit have committed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartedAction {
    token: ActionClaimToken,
    request: ActionRequest,
    grouped: bool,
    attempt_number: NonZeroU32,
    started_at: UtcTimestamp,
}

impl StartedAction {
    pub(crate) const fn new(
        token: ActionClaimToken,
        request: ActionRequest,
        grouped: bool,
        attempt_number: NonZeroU32,
        started_at: UtcTimestamp,
    ) -> Self {
        Self {
            token,
            request,
            grouped,
            attempt_number,
            started_at,
        }
    }

    #[must_use]
    pub const fn token(&self) -> ActionClaimToken {
        self.token
    }

    #[must_use]
    pub const fn request(&self) -> &ActionRequest {
        &self.request
    }

    #[must_use]
    pub const fn grouped(&self) -> bool {
        self.grouped
    }

    #[must_use]
    pub const fn attempt_number(&self) -> NonZeroU32 {
        self.attempt_number
    }

    #[must_use]
    pub const fn started_at(&self) -> UtcTimestamp {
        self.started_at
    }
}

/// Result of the permission check immediately before an effect may start.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActionStartOutcome {
    Started(StartedAction),
    Suppressed(ActionFinalization),
}

/// Result of attempting to lease one ready Outbox request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActionClaimOutcome {
    Idle,
    Claimed(ClaimedAction),
    RejectedUnavailable(ActionFinalization),
}

/// Privacy-safe result of one terminal action transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActionFinalization {
    pub action_id: ActionId,
    pub state: ActionLifecycleState,
    pub completed_at: UtcTimestamp,
    pub error_code: Option<ActionErrorCode>,
    pub suppression_reason: Option<ActionSuppressionReason>,
}

/// One bounded startup pass over actions started by a previous process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActionRecoveryRequest {
    recovered_at: UtcTimestamp,
    batch_size: NonZeroU32,
}

impl ActionRecoveryRequest {
    /// Build a bounded startup recovery pass.
    ///
    /// # Errors
    ///
    /// Returns an error when `batch_size` is zero or exceeds the hard cap.
    pub fn new(recovered_at: UtcTimestamp, batch_size: u32) -> StorageResult<Self> {
        let Some(batch_size) = NonZeroU32::new(batch_size) else {
            return Err(StorageError::InvalidActionRecoveryBatch {
                max: MAX_ACTION_RECOVERY_BATCH_SIZE,
            });
        };
        if batch_size.get() > MAX_ACTION_RECOVERY_BATCH_SIZE {
            return Err(StorageError::InvalidActionRecoveryBatch {
                max: MAX_ACTION_RECOVERY_BATCH_SIZE,
            });
        }
        Ok(Self {
            recovered_at,
            batch_size,
        })
    }

    #[must_use]
    pub const fn recovered_at(self) -> UtcTimestamp {
        self.recovered_at
    }

    #[must_use]
    pub const fn batch_size(self) -> NonZeroU32 {
        self.batch_size
    }
}

/// Result of one bounded startup action-recovery transaction.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ActionRecoveryOutcome {
    pub recovered_actions: u32,
    pub more_remaining: bool,
}

/// One newly stored alert and its side-effect-free notification decision.
///
/// The decision is diagnostic output, not a durable action handoff. The action
/// Outbox consumes the same policy inside the completion transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuleAlertDecision {
    pub alert_id: AlertId,
    pub evaluation_key: RuleEvaluationKey,
    pub notification_eligibility: NotificationEligibility,
    pub action: Option<RuleActionDecision>,
}

/// Durable Outbox state created in the same transaction as one new alert.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuleActionDecision {
    pub action_id: ActionId,
    pub state: ActionLifecycleState,
}

/// Result of atomically storing evaluations, matched alerts, and work completion.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RuleWorkCompletion {
    pub inserted_evaluations: u32,
    pub existing_evaluations: u32,
    pub inserted_alerts: Vec<RuleAlertDecision>,
    pub existing_alerts: u32,
}

/// Supported structured-history retention choices.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetentionPolicy {
    Days30,
    Days90,
    Days365,
    Unlimited,
}

impl RetentionPolicy {
    #[must_use]
    pub const fn days(self) -> Option<i64> {
        match self {
            Self::Days30 => Some(30),
            Self::Days90 => Some(90),
            Self::Days365 => Some(365),
            Self::Unlimited => None,
        }
    }
}

/// One bounded retention pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetentionRequest {
    pub policy: RetentionPolicy,
    pub now: UtcTimestamp,
    pub batch_size: NonZeroU32,
}

impl RetentionRequest {
    /// Validate one bounded cleanup request.
    ///
    /// # Errors
    ///
    /// Returns an error when `batch_size` is zero or exceeds the hard cap.
    pub fn new(policy: RetentionPolicy, now: UtcTimestamp, batch_size: u32) -> StorageResult<Self> {
        let Some(batch_size) = NonZeroU32::new(batch_size) else {
            return Err(StorageError::InvalidRetentionBatch {
                max: MAX_RETENTION_BATCH_SIZE,
            });
        };
        if batch_size.get() > MAX_RETENTION_BATCH_SIZE {
            return Err(StorageError::InvalidRetentionBatch {
                max: MAX_RETENTION_BATCH_SIZE,
            });
        }
        Ok(Self {
            policy,
            now,
            batch_size,
        })
    }
}

/// Result of one bounded cleanup transaction.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetentionOutcome {
    pub deleted_model_calls: u32,
    pub deleted_orphan_turns: u32,
    pub deleted_orphan_conversations: u32,
    pub more_eligible: bool,
}

/// Maximum supported positive reasoning-pattern notification threshold.
pub const MAX_NOTIFY_MAX_N: u32 = 1_000;

/// Validated application settings stored in the singleton settings row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppSettings {
    pub retention: RetentionPolicy,
    pub notifications_enabled: bool,
    pub notify_max_n: Option<NonZeroU32>,
    pub notify_all: bool,
    pub locale: AppLocale,
    pub login_autostart: bool,
    pub updated_at: UtcTimestamp,
}

impl AppSettings {
    /// Validate cross-field settings invariants.
    ///
    /// # Errors
    ///
    /// Returns an error unless exactly one threshold mode is selected.
    pub fn validate(&self) -> StorageResult<()> {
        if self.notify_all == self.notify_max_n.is_some() {
            return Err(StorageError::InvalidStoredValue {
                field: "app_settings.notify_threshold",
            });
        }
        if self
            .notify_max_n
            .is_some_and(|maximum| maximum.get() > MAX_NOTIFY_MAX_N)
        {
            return Err(StorageError::InvalidStoredValue {
                field: "app_settings.notify_max_n",
            });
        }
        Ok(())
    }
}

/// Revisioned settings document used for atomic compare-and-swap updates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredSettingsDocument {
    pub revision: u64,
    pub settings: AppSettings,
    pub locale_initialized: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppLocale {
    EnUs,
    ZhCn,
}

impl AppLocale {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EnUs => "en-US",
            Self::ZhCn => "zh-CN",
        }
    }

    /// Match one system or browser language tag to a supported catalog.
    #[must_use]
    pub fn from_language_tag(language_tag: &str) -> Option<Self> {
        let normalized = language_tag.trim().replace('_', "-").to_ascii_lowercase();
        if normalized == "en" || normalized.starts_with("en-") {
            return Some(Self::EnUs);
        }
        if normalized == "zh"
            || normalized == "zh-cn"
            || normalized.starts_with("zh-cn-")
            || normalized == "zh-sg"
            || normalized.starts_with("zh-sg-")
            || normalized == "zh-hans"
            || normalized.starts_with("zh-hans-")
        {
            return Some(Self::ZhCn);
        }
        None
    }
}

/// Read-side representation of durable source health.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredSourceHealth {
    pub source_id: SourceId,
    pub source_contract_version: SourceContractVersion,
    pub status: SourceHealthStatus,
    pub supported_fields: Vec<FieldId>,
    pub last_accepted_at: Option<UtcTimestamp>,
    pub last_error_code: Option<SafeErrorCode>,
    pub last_error_occurred_at: Option<UtcTimestamp>,
    pub updated_at: UtcTimestamp,
}

impl From<&SourceHealth> for StoredSourceHealth {
    fn from(value: &SourceHealth) -> Self {
        Self {
            source_id: value.identity().id().clone(),
            source_contract_version: value.identity().contract_version(),
            status: value.status(),
            supported_fields: value.supported_fields().iter().cloned().collect(),
            last_accepted_at: value.last_accepted_at(),
            last_error_code: value
                .last_failure()
                .map(codex_monitor_core::SourceFailure::code),
            last_error_occurred_at: value
                .last_failure()
                .map(codex_monitor_core::SourceFailure::occurred_at),
            updated_at: value.updated_at(),
        }
    }
}

/// Privacy-safe alert query row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredAlert {
    pub id: AlertId,
    pub model_call_id: ModelCallId,
    pub rule_id: BuiltInRuleId,
    pub rule_version: RuleVersion,
    pub reasoning_output_tokens: TokenCount,
    pub pattern_n: PatternN,
    pub event_at: UtcTimestamp,
    pub source_quality: EvidenceQuality,
    pub created_at: UtcTimestamp,
}

/// Snapshot used to verify connection-level `SQLite` safety settings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlitePragmas {
    pub journal_mode: String,
    pub foreign_keys: bool,
    pub busy_timeout_millis: u64,
    pub query_only: bool,
}

/// Stable position immediately after one canonical model call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelCallCursor {
    event_at: UtcTimestamp,
    id: ModelCallId,
}

impl ModelCallCursor {
    #[must_use]
    pub const fn new(event_at: UtcTimestamp, id: ModelCallId) -> Self {
        Self { event_at, id }
    }

    #[must_use]
    pub const fn event_at(self) -> UtcTimestamp {
        self.event_at
    }

    #[must_use]
    pub const fn id(self) -> ModelCallId {
        self.id
    }
}

/// Bounded query for recent canonical model calls.
#[derive(Clone, Copy, Debug)]
pub struct ModelCallQuery {
    limit: NonZeroUsize,
    before: Option<ModelCallCursor>,
}

impl ModelCallQuery {
    /// Create a bounded recent-call query.
    ///
    /// # Errors
    ///
    /// Returns an error when `limit` is zero or exceeds the API hard cap.
    pub fn new(limit: usize, before: Option<ModelCallCursor>) -> StorageResult<Self> {
        let Some(limit) = NonZeroUsize::new(limit) else {
            return Err(StorageError::InvalidQueryLimit {
                max: MAX_MODEL_CALL_QUERY_LIMIT,
            });
        };
        if limit.get() > MAX_MODEL_CALL_QUERY_LIMIT {
            return Err(StorageError::InvalidQueryLimit {
                max: MAX_MODEL_CALL_QUERY_LIMIT,
            });
        }
        Ok(Self { limit, before })
    }

    #[must_use]
    pub const fn limit(self) -> NonZeroUsize {
        self.limit
    }

    #[must_use]
    pub const fn before(self) -> Option<ModelCallCursor> {
        self.before
    }
}

impl Default for ModelCallQuery {
    fn default() -> Self {
        Self {
            limit: NonZeroUsize::new(100).expect("default query limit is non-zero"),
            before: None,
        }
    }
}

/// A canonical call returned by the read repository.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredModelCall(pub ModelCall);

/// Registry classification retained even for rejected stored requests.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoredActionType {
    Registered(RegisteredActionType),
    Unregistered(String),
}

impl StoredActionType {
    /// Return the exact bounded persistence spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Registered(action_type) => action_type.as_str(),
            Self::Unregistered(action_type) => action_type,
        }
    }

    /// Return the typed registry member when the stored name is registered.
    #[must_use]
    pub const fn registered(&self) -> Option<RegisteredActionType> {
        match self {
            Self::Registered(action_type) => Some(*action_type),
            Self::Unregistered(_) => None,
        }
    }
}

impl PartialEq<RegisteredActionType> for StoredActionType {
    fn eq(&self, other: &RegisteredActionType) -> bool {
        self.registered() == Some(*other)
    }
}

/// One action's role inside a durable notification-coalescing window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotificationCoalescingRole {
    Leader,
    Follower,
}

/// Privacy-safe audit link from an action to its canonical conversation window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoredNotificationCoalescing {
    pub window_id: NotificationCoalescingWindowId,
    pub conversation_id: ConversationId,
    pub leader_action_id: Option<ActionId>,
    pub role: NotificationCoalescingRole,
    pub opened_at: UtcTimestamp,
    pub closes_at: UtcTimestamp,
}

/// Privacy-safe read model for one durable Outbox request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredAction {
    pub id: ActionId,
    pub alert_id: AlertId,
    pub action_type: StoredActionType,
    pub parameter_schema_version: NonZeroU16,
    pub state: ActionLifecycleState,
    pub parameters: Option<ActionParameters>,
    pub created_at: UtcTimestamp,
    pub not_before: UtcTimestamp,
    pub lease_generation: u64,
    pub lease_expires_at: Option<UtcTimestamp>,
    pub started_at: Option<UtcTimestamp>,
    pub completed_at: Option<UtcTimestamp>,
    pub error_code: Option<ActionErrorCode>,
    pub suppression_reason: Option<ActionSuppressionReason>,
    pub notification_coalescing: Option<StoredNotificationCoalescing>,
}

/// Auditable state of one started action attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionAttemptState {
    Started,
    Completed,
    Failed,
    Unavailable,
    Indeterminate,
}

/// Privacy-safe read model for one action attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredActionAttempt {
    pub action_id: ActionId,
    pub attempt_number: NonZeroU32,
    pub lease_generation: u64,
    pub state: ActionAttemptState,
    pub started_at: UtcTimestamp,
    pub completed_at: Option<UtcTimestamp>,
    pub error_code: Option<ActionErrorCode>,
}

/// Typed source-checkpoint infrastructure record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceCheckpoint {
    pub source_id: SourceId,
    pub source_contract_version: SourceContractVersion,
    pub kind: SourceCheckpointKind,
    pub generation: u64,
    pub offset_bytes: Option<u64>,
    pub fingerprint_digest: Option<[u8; 32]>,
    pub updated_at: UtcTimestamp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceCheckpointKind {
    OtlpReceiver,
    SessionsFile,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(notify_max_n: Option<NonZeroU32>, notify_all: bool) -> AppSettings {
        AppSettings {
            retention: RetentionPolicy::Days90,
            notifications_enabled: true,
            notify_max_n,
            notify_all,
            locale: AppLocale::EnUs,
            login_autostart: false,
            updated_at: UtcTimestamp::from_unix_timestamp_nanos(0).unwrap(),
        }
    }

    #[test]
    fn application_notification_threshold_requires_exactly_one_mode() {
        assert!(settings(NonZeroU32::new(1), false).validate().is_ok());
        assert!(
            settings(NonZeroU32::new(MAX_NOTIFY_MAX_N), false)
                .validate()
                .is_ok()
        );
        assert!(settings(None, true).validate().is_ok());
        assert!(matches!(
            settings(NonZeroU32::new(1), true).validate(),
            Err(StorageError::InvalidStoredValue {
                field: "app_settings.notify_threshold"
            })
        ));
        assert!(matches!(
            settings(None, false).validate(),
            Err(StorageError::InvalidStoredValue {
                field: "app_settings.notify_threshold"
            })
        ));
        assert!(matches!(
            settings(NonZeroU32::new(MAX_NOTIFY_MAX_N + 1), false).validate(),
            Err(StorageError::InvalidStoredValue {
                field: "app_settings.notify_max_n"
            })
        ));
    }

    #[test]
    fn locale_matching_accepts_supported_families_without_mislabeling_traditional_chinese() {
        assert_eq!(AppLocale::from_language_tag("en-GB"), Some(AppLocale::EnUs));
        assert_eq!(
            AppLocale::from_language_tag("zh_Hans_CN"),
            Some(AppLocale::ZhCn)
        );
        assert_eq!(AppLocale::from_language_tag("zh-TW"), None);
        assert_eq!(AppLocale::from_language_tag("ja-JP"), None);
    }
}
