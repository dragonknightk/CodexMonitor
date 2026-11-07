//! Pure domain model and policy for `CodexMonitor`.
//!
//! This crate deliberately has no dependency on `OpenTelemetry`, `SQLite`, HTTP
//! or `Tauri` so the privacy and precedence rules can be tested in isolation.

mod action;
mod identity;
mod ids;
mod model;
mod observation;
mod provenance;
mod rule;
mod source;
mod timestamp;
mod value;

pub use identity::{
    FallbackFingerprintInput, FingerprintTokenFields, IdentityAlgorithmVersion, IdentityKind,
    SourceEventIdentity,
};
pub use ids::{
    ActionId, AlertId, ConversationId, ModelCallId, ModelCallObservationId,
    NotificationCoalescingWindowId, TurnId,
};
pub use model::{
    CanonicalModelCallFields, Conversation, DataOrigin, ModelCall, ModelCallObservation,
    ModelCallRelationship, ObservationFields, Turn,
};
pub use observation::{
    SourceExtraction, SourceObservationCandidate, SourceObservationError, SourceObservationKind,
};
pub use provenance::{CanonicalField, EvidenceQuality, FieldProvenance, MergeOutcome, Observed};
pub use rule::{
    BuiltInRuleDefinition, BuiltInRuleId, BuiltInRuleRegistry, NotificationEligibility,
    NotificationThreshold, PatternMatchEvidence, PatternN, RuleErrorCode, RuleEvaluation,
    RuleEvaluationKey, RuleEvaluationOutcome, RuleField, RuleNotificationPolicy, RuleOperator,
    RuleReferenceInput, RuleValidationError, RuleVersion, SUSPECTED_REASONING_LIMIT_PATTERN,
};
pub use source::{
    FieldIdSet, MAX_SOURCE_FIELDS, SourceFailure, SourceHealth, SourceHealthStatus, SourceIdentity,
};
pub use timestamp::UtcTimestamp;
pub use value::{
    ClientName, ConversationSourceId, EventName, FieldId, MAX_TOKEN_COUNT, ModelName,
    ReasoningEffort, SafeErrorCode, SourceContractVersion, SourceId, SourcePriority,
    StableSourceEventId, StreamEventKind, SurfaceName, ThreadSourceId, TokenCount, TurnSourceId,
    ValidationError,
};

/// A stable identifier for the built-in OTLP source.
pub const OTLP_SOURCE_ID: &str = "otlp-logs";
pub use action::{
    ActionCapability, ActionErrorCode, ActionExecutionContext, ActionExecutionResult,
    ActionExecutor, ActionFuture, ActionIdempotencyKey, ActionLifecycleState, ActionLocale,
    ActionParameters, ActionPermission, ActionRegistryError, ActionRequest,
    ActionSuppressionReason, RegisteredActionDefinition, RegisteredActionRegistry,
    RegisteredActionType, SYSTEM_NOTIFICATION_DEFINITION, SystemNotificationParams,
};
