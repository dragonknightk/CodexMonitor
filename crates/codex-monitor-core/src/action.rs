use std::future::Future;
use std::num::NonZeroU16;
use std::pin::Pin;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    ActionId, AlertId, ModelCallId, ModelName, PatternMatchEvidence, PatternN, TokenCount,
    UtcTimestamp, ValidationError,
};

/// The only effectful action registered by v1.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegisteredActionType {
    SystemNotification,
}

impl RegisteredActionType {
    /// Return the stable persistence spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SystemNotification => "system_notification",
        }
    }
}

impl FromStr for RegisteredActionType {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "system_notification" => Ok(Self::SystemNotification),
            _ => Err(ValidationError::UnknownActionType),
        }
    }
}

/// Declared platform capability used by an executor.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionCapability {
    SystemNotification,
}

/// Stored permission that must be satisfied before an effect is attempted.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionPermission {
    SystemNotificationsEnabled,
}

/// Closed locale selection supplied to a host at execution time.
///
/// Locale is presentation context rather than part of the durable action
/// parameter schema, so queued notifications follow the user's current setting.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ActionLocale {
    EnUs,
    ZhCn,
}

/// Privacy-safe process-local context for one action attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActionExecutionContext {
    locale: ActionLocale,
    grouped: bool,
}

impl ActionExecutionContext {
    #[must_use]
    pub const fn new(locale: ActionLocale, grouped: bool) -> Self {
        Self { locale, grouped }
    }

    #[must_use]
    pub const fn locale(self) -> ActionLocale {
        self.locale
    }

    /// Return whether the durable coalescing window contains follower alerts.
    #[must_use]
    pub const fn grouped(self) -> bool {
        self.grouped
    }
}

/// Immutable action definition in the v1 registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct RegisteredActionDefinition {
    pub action_type: RegisteredActionType,
    pub schema_version: NonZeroU16,
    pub capabilities: &'static [ActionCapability],
    pub permissions: &'static [ActionPermission],
    pub effectful: bool,
}

/// System notification is the only v1 effectful executor.
pub const SYSTEM_NOTIFICATION_DEFINITION: RegisteredActionDefinition = RegisteredActionDefinition {
    action_type: RegisteredActionType::SystemNotification,
    schema_version: NonZeroU16::MIN,
    capabilities: &[ActionCapability::SystemNotification],
    permissions: &[ActionPermission::SystemNotificationsEnabled],
    effectful: true,
};

/// Safe failure while resolving a persisted action reference.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ActionRegistryError {
    #[error("unknown action type")]
    UnknownActionType,
    #[error("action parameter schema version does not match the registered executor")]
    SchemaVersionMismatch,
}

/// Closed v1 registry for effectful action definitions.
#[derive(Clone, Copy, Debug, Default)]
pub struct RegisteredActionRegistry;

impl RegisteredActionRegistry {
    /// Resolve a stored action reference to a native typed executor definition.
    ///
    /// # Errors
    ///
    /// Rejects every unregistered action type and schema version.
    pub fn resolve(
        action_type: &str,
        schema_version: u16,
    ) -> Result<&'static RegisteredActionDefinition, ActionRegistryError> {
        let action_type = action_type
            .parse::<RegisteredActionType>()
            .map_err(|_| ActionRegistryError::UnknownActionType)?;
        let definition = match action_type {
            RegisteredActionType::SystemNotification => &SYSTEM_NOTIFICATION_DEFINITION,
        };
        if schema_version != definition.schema_version.get() {
            return Err(ActionRegistryError::SchemaVersionMismatch);
        }
        Ok(definition)
    }
}

/// Minimal typed notification parameters; no content, path or session label fits.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SystemNotificationParams {
    model: Option<ModelName>,
    reasoning_output_tokens: TokenCount,
    pattern_n: PatternN,
}

impl SystemNotificationParams {
    /// Build privacy-safe parameters from validated durable values.
    #[must_use]
    pub const fn new(
        model: Option<ModelName>,
        reasoning_output_tokens: TokenCount,
        pattern_n: PatternN,
    ) -> Self {
        Self {
            model,
            reasoning_output_tokens,
            pattern_n,
        }
    }

    /// Build privacy-safe parameters directly from durable match evidence.
    #[must_use]
    pub fn from_evidence(model: Option<ModelName>, evidence: &PatternMatchEvidence) -> Self {
        Self::new(model, evidence.reasoning_output_tokens, evidence.pattern_n)
    }

    /// Borrow the optional bounded model label.
    #[must_use]
    pub const fn model(&self) -> Option<&ModelName> {
        self.model.as_ref()
    }

    /// Return the observed reasoning token count.
    #[must_use]
    pub const fn reasoning_output_tokens(&self) -> TokenCount {
        self.reasoning_output_tokens
    }

    /// Return the positive formula value.
    #[must_use]
    pub const fn pattern_n(&self) -> PatternN {
        self.pattern_n
    }
}

/// Closed typed parameter union for registered actions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "action_type", content = "parameters", rename_all = "snake_case")]
pub enum ActionParameters {
    SystemNotification(SystemNotificationParams),
}

impl ActionParameters {
    /// Return the executor type required by these parameters.
    #[must_use]
    pub const fn action_type(&self) -> RegisteredActionType {
        match self {
            Self::SystemNotification(_) => RegisteredActionType::SystemNotification,
        }
    }

    /// Return the registered parameter schema version.
    #[must_use]
    pub const fn schema_version(&self) -> NonZeroU16 {
        match self {
            Self::SystemNotification(_) => SYSTEM_NOTIFICATION_DEFINITION.schema_version,
        }
    }
}

/// Versioned deterministic Outbox idempotency key.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ActionIdempotencyKey {
    version: u16,
    digest: [u8; 32],
}

impl ActionIdempotencyKey {
    fn for_alert(
        alert_id: AlertId,
        action_type: RegisteredActionType,
        parameter_schema_version: NonZeroU16,
    ) -> Self {
        const KEY_VERSION: u16 = 1;
        let action_type = action_type.as_str().as_bytes();
        let action_type_length =
            u16::try_from(action_type.len()).expect("registered action type length fits u16");
        let mut hasher = Sha256::new();
        hasher.update(b"codex-monitor/action-idempotency\0");
        hasher.update(KEY_VERSION.to_be_bytes());
        hasher.update(alert_id.as_uuid().as_bytes());
        hasher.update(action_type_length.to_be_bytes());
        hasher.update(action_type);
        hasher.update(parameter_schema_version.get().to_be_bytes());
        Self {
            version: KEY_VERSION,
            digest: hasher.finalize().into(),
        }
    }

    /// Return the idempotency algorithm version.
    #[must_use]
    pub const fn version(self) -> u16 {
        self.version
    }

    /// Return the binary key digest.
    #[must_use]
    pub const fn digest(self) -> [u8; 32] {
        self.digest
    }
}

/// Typed durable action request committed with its alert.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ActionRequest {
    pub id: ActionId,
    pub alert_id: AlertId,
    pub model_call_id: ModelCallId,
    pub parameter_schema_version: NonZeroU16,
    pub idempotency_key: ActionIdempotencyKey,
    pub created_at: UtcTimestamp,
    pub parameters: ActionParameters,
}

impl ActionRequest {
    fn new(
        alert_id: AlertId,
        model_call_id: ModelCallId,
        created_at: UtcTimestamp,
        parameters: ActionParameters,
    ) -> Self {
        let idempotency_key = ActionIdempotencyKey::for_alert(
            alert_id,
            parameters.action_type(),
            parameters.schema_version(),
        );
        Self {
            id: ActionId::new(),
            alert_id,
            model_call_id,
            parameter_schema_version: parameters.schema_version(),
            idempotency_key,
            created_at,
            parameters,
        }
    }

    /// Create the registered v1 system-notification request directly from
    /// durable alert evidence.
    #[must_use]
    pub fn system_notification(
        alert_id: AlertId,
        model: Option<ModelName>,
        evidence: &PatternMatchEvidence,
        created_at: UtcTimestamp,
    ) -> Self {
        let parameters = ActionParameters::SystemNotification(
            SystemNotificationParams::from_evidence(model, evidence),
        );
        Self::new(alert_id, evidence.model_call_id, created_at, parameters)
    }

    /// Rebuild a typed system-notification request from the allowlisted Outbox
    /// columns after their alert relationship has been validated.
    #[must_use]
    pub fn from_stored_system_notification(
        id: ActionId,
        alert_id: AlertId,
        model_call_id: ModelCallId,
        model: Option<ModelName>,
        reasoning_output_tokens: TokenCount,
        pattern_n: PatternN,
        created_at: UtcTimestamp,
    ) -> Self {
        let parameters = ActionParameters::SystemNotification(SystemNotificationParams::new(
            model,
            reasoning_output_tokens,
            pattern_n,
        ));
        let mut request = Self::new(alert_id, model_call_id, created_at, parameters);
        request.id = id;
        request
    }
}

/// Auditable Outbox lifecycle.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionLifecycleState {
    Pending,
    Started,
    Completed,
    Failed,
    Suppressed,
    Unavailable,
}

impl ActionLifecycleState {
    /// Return the stable persistence spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Started => "started",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Suppressed => "suppressed",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Closed safe failures for action execution and crash recovery.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionErrorCode {
    InvalidParameters,
    PermissionDenied,
    NotificationUnavailable,
    NotificationDispatchFailed,
    IndeterminateAfterCrash,
    UnknownActionType,
    InternalQueueClosed,
}

impl ActionErrorCode {
    /// Return the stable persistence spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidParameters => "invalid_parameters",
            Self::PermissionDenied => "permission_denied",
            Self::NotificationUnavailable => "notification_unavailable",
            Self::NotificationDispatchFailed => "notification_dispatch_failed",
            Self::IndeterminateAfterCrash => "indeterminate_after_crash",
            Self::UnknownActionType => "unknown_action_type",
            Self::InternalQueueClosed => "internal_queue_closed",
        }
    }
}

/// Closed reasons for terminally suppressing an action before an effect starts.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionSuppressionReason {
    NotificationsDisabled,
    AboveThreshold,
    PermissionDenied,
    Coalesced,
    HistoricalEvent,
    PreOutboxUpgrade,
}

impl ActionSuppressionReason {
    /// Return the stable persistence spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotificationsDisabled => "notifications_disabled",
            Self::AboveThreshold => "above_threshold",
            Self::PermissionDenied => "permission_denied",
            Self::Coalesced => "coalesced",
            Self::HistoricalEvent => "historical_event",
            Self::PreOutboxUpgrade => "pre_outbox_upgrade",
        }
    }
}

/// Result returned by a dedicated executor without raw error text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionExecutionResult {
    Completed,
    Failed(ActionErrorCode),
    Unavailable(ActionErrorCode),
}

/// Boxed executor future used by the engine's typed action registry.
pub type ActionFuture<'a> = Pin<Box<dyn Future<Output = ActionExecutionResult> + Send + 'a>>;

/// Dedicated port implemented by a host adapter for one registered action type.
pub trait ActionExecutor: Send + Sync {
    /// Return the one action type accepted by this executor.
    fn action_type(&self) -> RegisteredActionType;

    /// Return the platform capabilities declared by this executor.
    fn capabilities(&self) -> &'static [ActionCapability];

    /// Return the stored permissions required before execution.
    fn permissions(&self) -> &'static [ActionPermission];

    /// Execute typed parameters. Implementations must not evaluate strings as
    /// code, paths, URLs or commands.
    fn execute<'a>(
        &'a self,
        parameters: &'a ActionParameters,
        context: ActionExecutionContext,
    ) -> ActionFuture<'a>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BuiltInRuleId, EvidenceQuality, FieldProvenance, PatternN, RuleVersion,
        SourceContractVersion, SourceId, SourceIdentity, SourcePriority,
    };
    use std::num::{NonZeroU16, NonZeroU64};

    fn at(seconds: i128) -> UtcTimestamp {
        UtcTimestamp::from_unix_timestamp_nanos(seconds * 1_000_000_000).unwrap()
    }

    fn evidence(model_call_id: ModelCallId) -> PatternMatchEvidence {
        PatternMatchEvidence {
            model_call_id,
            rule_id: BuiltInRuleId::SuspectedReasoningLimitPattern,
            rule_version: RuleVersion::new(NonZeroU16::MIN),
            reasoning_output_tokens: TokenCount::new(516).unwrap(),
            pattern_n: PatternN::new(NonZeroU64::MIN),
            event_time: at(1),
            token_provenance: FieldProvenance::new(
                SourceIdentity::new(
                    SourceId::new("otlp-logs").unwrap(),
                    SourceContractVersion::new(1).unwrap(),
                ),
                at(2),
                EvidenceQuality::Direct,
                SourcePriority::new(100),
            ),
        }
    }

    #[test]
    fn action_type_is_closed_and_has_no_command_fallback() {
        assert_eq!(
            "system_notification".parse(),
            Ok(RegisteredActionType::SystemNotification)
        );
        for unknown in ["shell", "webhook", "open_url", "codex_control"] {
            assert_eq!(
                unknown.parse::<RegisteredActionType>(),
                Err(ValidationError::UnknownActionType)
            );
        }
    }

    #[test]
    fn registry_requires_the_exact_typed_schema_and_permission() {
        assert_eq!(
            RegisteredActionRegistry::resolve("system_notification", 1),
            Ok(&SYSTEM_NOTIFICATION_DEFINITION)
        );
        assert_eq!(
            SYSTEM_NOTIFICATION_DEFINITION.permissions,
            &[ActionPermission::SystemNotificationsEnabled]
        );
        assert_eq!(
            RegisteredActionRegistry::resolve("system_notification", 2),
            Err(ActionRegistryError::SchemaVersionMismatch)
        );
        assert_eq!(
            RegisteredActionRegistry::resolve("shell", 1),
            Err(ActionRegistryError::UnknownActionType)
        );
    }

    #[test]
    fn notification_parameters_contain_only_approved_metadata() {
        let model_call_id = ModelCallId::new();
        let parameters = SystemNotificationParams::from_evidence(
            Some(ModelName::new("gpt-5.6-terra").unwrap()),
            &evidence(model_call_id),
        );
        let json = serde_json::to_string(&parameters).unwrap();

        assert!(json.contains("gpt-5.6-terra"));
        assert!(json.contains("516"));
        for forbidden in ["prompt", "response", "session", "path", "tool", "account"] {
            assert!(!json.contains(forbidden));
        }
    }

    #[test]
    fn idempotency_key_is_stable_for_one_alert_and_action_type() {
        let alert_id = AlertId::new();
        let model_call_id = ModelCallId::new();
        let parameters = || {
            ActionParameters::SystemNotification(SystemNotificationParams::from_evidence(
                None,
                &evidence(model_call_id),
            ))
        };
        let evidence = evidence(model_call_id);
        let first = ActionRequest::system_notification(alert_id, None, &evidence, at(3));
        let second = ActionRequest::system_notification(alert_id, None, &evidence, at(4));

        assert_ne!(first.id, second.id);
        assert_eq!(first.idempotency_key, second.idempotency_key);
        assert_eq!(first.parameters, parameters());
    }

    #[test]
    fn idempotency_key_v1_matches_the_pinned_binary_vector() {
        let alert_id = "018f4f7a-4b2c-7d10-8a11-223344556677"
            .parse::<AlertId>()
            .unwrap();
        let model_call_id = ModelCallId::new();
        let request =
            ActionRequest::system_notification(alert_id, None, &evidence(model_call_id), at(3));

        assert_eq!(request.idempotency_key.version(), 1);
        assert_eq!(
            request.idempotency_key.digest(),
            [
                0x52, 0x3d, 0x7a, 0xc8, 0x5b, 0xc3, 0xa7, 0x99, 0x08, 0x33, 0x77, 0xcc, 0x23, 0x56,
                0x9d, 0x73, 0x1c, 0x5a, 0xa2, 0x3a, 0xd7, 0x7c, 0xaf, 0x34, 0x0b, 0x97, 0x34, 0xe0,
                0x89, 0xde, 0x51, 0x01,
            ]
        );
    }
}
