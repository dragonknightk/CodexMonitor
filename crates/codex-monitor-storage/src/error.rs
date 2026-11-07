use thiserror::Error;

/// Failures at the typed `SQLite` boundary.
///
/// Error messages intentionally omit SQL parameter values and filesystem paths.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("database operation failed")]
    Database(#[from] rusqlite::Error),
    #[error("domain value validation failed")]
    Domain(#[from] codex_monitor_core::ValidationError),
    #[error("embedded migration versions are not contiguous at {version}")]
    MigrationOrder { version: u32 },
    #[error("database contains unknown migration version {version}")]
    UnknownMigration { version: u32 },
    #[error("migration {version} name differs from the embedded migration")]
    MigrationNameMismatch { version: u32 },
    #[error("migration {version} checksum differs from the embedded migration")]
    MigrationChecksumMismatch { version: u32 },
    #[error("migration {version} byte count differs from the embedded migration")]
    MigrationSizeMismatch { version: u32 },
    #[error("migration audit contains a version gap at {version}")]
    MigrationAuditGap { version: u32 },
    #[error("the SQLite writer is closed")]
    WriterClosed,
    #[error("the SQLite writer thread stopped unexpectedly")]
    WriterStopped,
    #[error("operating-system stream entropy is unavailable")]
    StreamEntropy(#[source] getrandom::Error),
    #[error("stored {field} is invalid")]
    InvalidStoredValue { field: &'static str },
    #[error("field is outside the telemetry catalog")]
    UnregisteredTelemetryField,
    #[error("observation is bound to a different canonical model call")]
    ObservationModelCallMismatch,
    #[error("canonical identity is already bound to a different model call")]
    CanonicalIdentityCollision,
    #[error("application data environment is unavailable for {component}")]
    AppDataUnavailable { component: &'static str },
    #[error("application data path for {component} must be absolute")]
    AppDataPathNotAbsolute { component: &'static str },
    #[error("application data path for {component} must not be a symbolic link")]
    AppDataSymlink { component: &'static str },
    #[error("retention batch size must be between 1 and {max}")]
    InvalidRetentionBatch { max: u32 },
    #[error("model-call query limit must be between 1 and {max}")]
    InvalidQueryLimit { max: usize },
    #[error("Dashboard query limit must be between 1 and {max}")]
    InvalidDashboardQueryLimit { max: usize },
    #[error("Dashboard query time bounds are invalid")]
    InvalidDashboardTimeRange,
    #[error("conversation selector is invalid")]
    InvalidConversationSelector,
    #[error("Dashboard filter {field} is invalid")]
    InvalidDashboardFilter { field: &'static str },
    #[error("conversation resolver identifier count exceeds {max}")]
    TooManyResolvedConversationIdentifiers { max: usize },
    #[error("Dashboard cursor does not match its sort")]
    DashboardCursorSortMismatch,
    #[error("model-call source count exceeds {max}")]
    TooManyModelCallSources { max: usize },
    #[error("model-call field provenance count exceeds {max}")]
    TooManyModelCallFieldSources { max: usize },
    #[error("source-health row count exceeds {max}")]
    TooManySourceHealthRows { max: usize },
    #[error("source capability field count exceeds {max}")]
    TooManySourceCapabilityFields { max: usize },
    #[error("model-call alert count exceeds {max}")]
    DashboardAlertCountOverflow { max: u64 },
    #[error("Dashboard aggregate exceeds the safe token bound")]
    DashboardAggregateOverflow,
    #[error("Dashboard count {field} exceeds the safe response bound")]
    DashboardCountOverflow { field: &'static str },
    #[error("settings revision does not match the current document")]
    SettingsRevisionConflict,
    #[error("source-local conversation keys resolve to different conversations")]
    ConversationIdentityCollision,
    #[error("legacy import source is unavailable or unsafe")]
    LegacySourceUnavailable,
    #[error("legacy import source is malformed or changed during import")]
    LegacySourceInvalid,
    #[error("legacy import source schema is unsupported")]
    LegacySchemaUnsupported,
    #[error("legacy import exceeds the supported row limit of {max}")]
    LegacyImportTooLarge { max: usize },
    #[error("legacy import audit conflicts with durable imported rows")]
    LegacyImportAuditConflict,
    #[error("legacy import conversation timestamps are inconsistent")]
    LegacyConversationTimeRangeInvalid,
    #[error("historical legacy import attempted to enqueue realtime work")]
    LegacyHistoricalIsolationViolation,
    #[error("registered rule definition conflicts with durable rule metadata")]
    RuleDefinitionConflict,
    #[error("rule work claim timestamps do not form a safe lease window")]
    InvalidRuleWorkClaimWindow,
    #[error("rule work is no longer owned by this claim")]
    RuleWorkClaimLost,
    #[error("rule work references a missing canonical model call")]
    RuleWorkModelCallMissing,
    #[error("rule evaluation batch does not match the active typed registry")]
    RuleEvaluationBatchInvalid,
    #[error("durable rule evaluation conflicts with the typed result")]
    RuleEvaluationConflict,
    #[error("durable alert evidence conflicts with the typed rule match")]
    AlertEvidenceConflict,
    #[error("action lease timestamps do not form a positive window")]
    InvalidActionLeaseWindow,
    #[error("action is no longer owned by this lease")]
    ActionClaimLost,
    #[error("durable action request conflicts with the typed alert action")]
    ActionRequestConflict,
    #[error("durable action attempt conflicts with the claimed action")]
    ActionAttemptConflict,
    #[error("action state transition is invalid")]
    InvalidActionTransition,
    #[error("action recovery batch size must be between 1 and {max}")]
    InvalidActionRecoveryBatch { max: u32 },
    #[error("started action recovery conflicts with its durable attempt audit")]
    ActionRecoveryConflict,
    #[error("notification coalescing window exceeds the supported timestamp range")]
    NotificationCoalescingWindowOverflow,
    #[error("rule worker blocking task stopped unexpectedly")]
    RuleWorkerStopped,
    #[error("action worker blocking task stopped unexpectedly")]
    ActionWorkerStopped,
    #[error("failed to prepare current-user {component}")]
    AppDataIo {
        component: &'static str,
        #[source]
        source: std::io::Error,
    },
}

pub type StorageResult<T> = Result<T, StorageError>;
