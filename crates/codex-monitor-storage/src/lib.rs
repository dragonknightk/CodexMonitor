//! Strongly typed local `SQLite` persistence.
//!
//! Raw telemetry messages and generic JSON are intentionally absent from this
//! API. Only validated `codex-monitor-core` aggregates cross the boundary.

mod action_outbox;
mod change_feed;
mod codec;
mod dashboard_query;
mod error;
mod legacy_import;
mod migrations;
mod paths;
mod projection;
mod repository;
mod retention;
mod rule_work;
mod runtime;
#[cfg(feature = "test-support")]
mod test_support;
mod timestamp;
mod types;
mod writer;

pub use change_feed::{
    StorageChange, StorageChangeFeed, StorageChangeRecord, StorageChangeSubscription,
    StreamControlReason, StreamReceive, StreamSubscribeError, StreamSubscriptionStart,
};
pub use dashboard_query::{
    AlertFilters, AlertMatchFilter, AlertPageQuery, CanonicalFieldName, ConversationPageQuery,
    ConversationSelector, DashboardAlert, DashboardAlertAction, DashboardAlertCursor,
    DashboardAlertEvidence, DashboardAlertState, DashboardConversation,
    DashboardConversationCursor, DashboardConversationRef, DashboardFieldSource,
    DashboardModelCall, DashboardModelCallCursor, DashboardModelCallSummary, DashboardPage,
    DashboardRuleCapability, DashboardTokenAggregate, DashboardTokenUsage,
    MAX_DASHBOARD_ALERT_COUNT, MAX_DASHBOARD_COUNT, MAX_DASHBOARD_PAGE_SIZE,
    MAX_DASHBOARD_SOURCE_ROWS, MAX_MODEL_CALL_SOURCE_IDS, MAX_RESOLVED_CONVERSATION_IDENTIFIERS,
    MAX_SOURCE_CAPABILITY_FIELDS, ModelCallFilters, ModelCallPageQuery, ModelCallSort,
};
pub use error::{StorageError, StorageResult};
pub use legacy_import::{
    LegacyImportOutcome, LegacyImportStatus, LegacySchemaDetection, LegacySchemaDetectionError,
    LegacySchemaKind, detect_legacy_schema,
};
pub use paths::{AppDataEnvironment, AppDataPaths, PlatformFamily};
pub use repository::StorageReader;
pub use runtime::CurrentUserStorage;
#[cfg(feature = "test-support")]
pub use test_support::open_test_database;
pub use types::{
    ActionAttemptState, ActionClaimOutcome, ActionClaimToken, ActionFinalization, ActionLeaseOwner,
    ActionLeaseWindow, ActionRecoveryOutcome, ActionRecoveryRequest, ActionStartOutcome, AppLocale,
    AppSettings, ClaimedAction, DEFAULT_RETENTION_BATCH_SIZE, DEFAULT_WRITER_QUEUE_CAPACITY,
    MAX_ACTION_RECOVERY_BATCH_SIZE, MAX_MODEL_CALL_QUERY_LIMIT, MAX_NOTIFY_MAX_N,
    MAX_RETENTION_BATCH_SIZE, ModelCallCursor, ModelCallQuery,
    NOTIFICATION_COALESCING_WINDOW_SECONDS, NotificationCoalescingRole, ObservationCommitOutcome,
    ObservationWrite, RetentionOutcome, RetentionPolicy, RetentionRequest, RuleActionDecision,
    RuleAlertDecision, RuleWorkClaim, RuleWorkCompletion, SourceBatchCommitOutcome,
    SourceCheckpoint, SourceCheckpointKind, SqlitePragmas, StartedAction, StorageConfig,
    StoredAction, StoredActionAttempt, StoredActionType, StoredAlert, StoredModelCall,
    StoredNotificationCoalescing, StoredSettingsDocument, StoredSourceHealth,
};
pub use writer::SettingsCoordinatorWriter;
pub use writer::StorageWriter;
