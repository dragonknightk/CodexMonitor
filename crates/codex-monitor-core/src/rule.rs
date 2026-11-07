use std::fmt;
use std::num::{NonZeroU16, NonZeroU64};
use std::str::FromStr;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{DataOrigin, FieldProvenance, ModelCall, ModelCallId, TokenCount, UtcTimestamp};

const QUANTIZATION_STEP: u64 = 518;
const QUANTIZATION_OFFSET: u64 = 2;
const RULE_SEMANTIC_FINGERPRINT_DOMAIN: &[u8] =
    b"codex-monitor/built-in-rule-semantic-fingerprint/v1";

/// Stable identifier for a built-in rule.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BuiltInRuleId {
    SuspectedReasoningLimitPattern,
}

impl BuiltInRuleId {
    /// Return the stable persistence spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SuspectedReasoningLimitPattern => "suspected-reasoning-limit-pattern",
        }
    }
}

impl fmt::Display for BuiltInRuleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for BuiltInRuleId {
    type Err = RuleValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "suspected-reasoning-limit-pattern" => Ok(Self::SuspectedReasoningLimitPattern),
            _ => Err(RuleValidationError::UnknownRule),
        }
    }
}

/// Closed, privacy-safe failure codes exposed by built-in rule health.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleErrorCode {
    PersistenceCommitFailed,
    InternalQueueClosed,
}

impl RuleErrorCode {
    /// Return the stable persistence and API spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PersistenceCommitFailed => "persistence_commit_failed",
            Self::InternalQueueClosed => "internal_queue_closed",
        }
    }
}

impl fmt::Display for RuleErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for RuleErrorCode {
    type Err = RuleValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "persistence_commit_failed" => Ok(Self::PersistenceCommitFailed),
            "internal_queue_closed" => Ok(Self::InternalQueueClosed),
            _ => Err(RuleValidationError::UnknownFailureCode),
        }
    }
}

/// Positive semantic version of one built-in rule definition.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct RuleVersion(NonZeroU16);

impl RuleVersion {
    /// Create a positive rule version.
    #[must_use]
    pub const fn new(value: NonZeroU16) -> Self {
        Self(value)
    }

    /// Return the numeric rule version.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

impl<'de> Deserialize<'de> for RuleVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u16::deserialize(deserializer)?;
        NonZeroU16::new(value)
            .map(Self)
            .ok_or_else(|| D::Error::custom("rule version must be positive"))
    }
}

/// Closed field set available to built-in rules.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleField {
    #[serde(rename = "model_call.reasoning_output_tokens")]
    ReasoningOutputTokens,
}

impl RuleField {
    /// Return the registered catalog field ID.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReasoningOutputTokens => "model_call.reasoning_output_tokens",
        }
    }
}

impl FromStr for RuleField {
    type Err = RuleValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "model_call.reasoning_output_tokens" => Ok(Self::ReasoningOutputTokens),
            _ => Err(RuleValidationError::UnknownField),
        }
    }
}

/// Closed operators implemented by native built-in rule code.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleOperator {
    ReasoningQuantization518Minus2,
}

impl RuleOperator {
    /// Return the stable operator identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReasoningQuantization518Minus2 => "reasoning_quantization_518_minus_2",
        }
    }
}

impl FromStr for RuleOperator {
    type Err = RuleValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "reasoning_quantization_518_minus_2" => Ok(Self::ReasoningQuantization518Minus2),
            _ => Err(RuleValidationError::UnknownOperator),
        }
    }
}

/// Validation error for built-in rule identifiers and attempted references.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum RuleValidationError {
    #[error("unknown built-in rule")]
    UnknownRule,
    #[error("unknown rule failure code")]
    UnknownFailureCode,
    #[error("unknown or disallowed rule field")]
    UnknownField,
    #[error("unknown or disallowed rule operator")]
    UnknownOperator,
    #[error("dynamic rule expressions and scripts are not supported")]
    DynamicContentRejected,
    #[error("rule version does not match the registered built-in definition")]
    VersionMismatch,
}

/// External reference accepted only when it exactly names native rule code.
#[derive(Clone, Copy, Debug)]
pub struct RuleReferenceInput<'a> {
    pub id: &'a str,
    pub version: u16,
    pub field: &'a str,
    pub operator: &'a str,
    pub dynamic_content: Option<&'a str>,
}

/// Immutable definition of one native built-in rule.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct BuiltInRuleDefinition {
    pub id: BuiltInRuleId,
    pub version: RuleVersion,
    pub field: RuleField,
    pub operator: RuleOperator,
}

/// v1 suspected reasoning-limit rule definition.
pub const SUSPECTED_REASONING_LIMIT_PATTERN: BuiltInRuleDefinition = BuiltInRuleDefinition {
    id: BuiltInRuleId::SuspectedReasoningLimitPattern,
    version: RuleVersion::new(NonZeroU16::MIN),
    field: RuleField::ReasoningOutputTokens,
    operator: RuleOperator::ReasoningQuantization518Minus2,
};

const ACTIVE_BUILT_IN_RULE_DEFINITIONS: &[BuiltInRuleDefinition] =
    &[SUSPECTED_REASONING_LIMIT_PATTERN];

impl BuiltInRuleDefinition {
    /// Return the stable fingerprint of this definition's persisted semantics.
    ///
    /// A changed fingerprint requires a new rule version before the definition
    /// can be registered under the same rule ID.
    #[must_use]
    pub fn semantic_fingerprint(&self) -> [u8; 32] {
        let mut encoder = RuleSemanticFingerprintEncoder::new();
        encoder.text(1, self.id.as_str());
        encoder.u16(2, self.version.get());
        encoder.text(3, self.field.as_str());
        encoder.text(4, self.operator.as_str());

        match (self.field, self.operator) {
            (RuleField::ReasoningOutputTokens, RuleOperator::ReasoningQuantization518Minus2) => {
                encoder.u64(16, QUANTIZATION_STEP);
                encoder.u64(17, QUANTIZATION_OFFSET);
            }
        }

        encoder.finish()
    }

    /// Evaluate this exact built-in definition against one canonical model call.
    #[must_use]
    pub fn evaluate(&self, model_call: &ModelCall) -> RuleEvaluation {
        let key = RuleEvaluationKey {
            rule_id: self.id,
            rule_version: self.version,
            model_call_id: model_call.id(),
        };
        let origin = model_call.origin();

        match (self.field, self.operator) {
            (RuleField::ReasoningOutputTokens, RuleOperator::ReasoningQuantization518Minus2) => {
                evaluate_reasoning_quantization(key, origin, model_call)
            }
        }
    }
}

struct RuleSemanticFingerprintEncoder(Sha256);

impl RuleSemanticFingerprintEncoder {
    fn new() -> Self {
        let mut encoder = Self(Sha256::new());
        encoder.bytes(0, RULE_SEMANTIC_FINGERPRINT_DOMAIN);
        encoder
    }

    fn field_header(&mut self, tag: u8, length: usize) {
        self.0.update([tag]);
        self.0.update(
            u64::try_from(length)
                .expect("fingerprint field length fits u64")
                .to_be_bytes(),
        );
    }

    fn bytes(&mut self, tag: u8, value: &[u8]) {
        self.field_header(tag, value.len());
        self.0.update(value);
    }

    fn text(&mut self, tag: u8, value: &str) {
        self.bytes(tag, value.as_bytes());
    }

    fn u16(&mut self, tag: u8, value: u16) {
        self.bytes(tag, &value.to_be_bytes());
    }

    fn u64(&mut self, tag: u8, value: u64) {
        self.bytes(tag, &value.to_be_bytes());
    }

    fn finish(self) -> [u8; 32] {
        self.0.finalize().into()
    }
}

/// Closed registry for built-in rule implementations.
#[derive(Clone, Copy, Debug, Default)]
pub struct BuiltInRuleRegistry;

impl BuiltInRuleRegistry {
    /// Return every built-in rule definition active in this binary.
    #[must_use]
    pub const fn active_definitions() -> &'static [BuiltInRuleDefinition] {
        ACTIVE_BUILT_IN_RULE_DEFINITIONS
    }

    /// Resolve an exact native rule reference.
    ///
    /// # Errors
    ///
    /// Rejects unknown IDs, fields, operators, versions and all dynamic content.
    pub fn resolve(
        input: RuleReferenceInput<'_>,
    ) -> Result<&'static BuiltInRuleDefinition, RuleValidationError> {
        if input.dynamic_content.is_some() {
            return Err(RuleValidationError::DynamicContentRejected);
        }
        let id = input.id.parse::<BuiltInRuleId>()?;
        let field = input.field.parse::<RuleField>()?;
        let operator = input.operator.parse::<RuleOperator>()?;
        let definition = Self::active_definitions()
            .iter()
            .find(|definition| definition.id == id)
            .ok_or(RuleValidationError::UnknownRule)?;
        if definition.version.get() != input.version {
            return Err(RuleValidationError::VersionMismatch);
        }
        if definition.field != field {
            return Err(RuleValidationError::UnknownField);
        }
        if definition.operator != operator {
            return Err(RuleValidationError::UnknownOperator);
        }
        Ok(definition)
    }
}

/// Positive `n` in `reasoning_output_tokens = 518 * n - 2`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct PatternN(NonZeroU64);

impl PatternN {
    /// Create a positive pattern value.
    #[must_use]
    pub const fn new(value: NonZeroU64) -> Self {
        Self(value)
    }

    /// Return the positive computed value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Durable evidence produced for every formula match.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PatternMatchEvidence {
    pub model_call_id: ModelCallId,
    pub rule_id: BuiltInRuleId,
    pub rule_version: RuleVersion,
    pub reasoning_output_tokens: TokenCount,
    pub pattern_n: PatternN,
    pub event_time: UtcTimestamp,
    pub token_provenance: FieldProvenance,
}

/// Durable idempotency identity for one rule/version/model-call evaluation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct RuleEvaluationKey {
    pub rule_id: BuiltInRuleId,
    pub rule_version: RuleVersion,
    pub model_call_id: ModelCallId,
}

/// Result category for a completed native rule evaluation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RuleEvaluationOutcome {
    MissingValue,
    NonPositiveValue,
    NoMatch,
    Matched { evidence: PatternMatchEvidence },
}

/// Complete idempotent result for one built-in rule evaluation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RuleEvaluation {
    pub key: RuleEvaluationKey,
    pub origin: DataOrigin,
    pub outcome: RuleEvaluationOutcome,
}

impl RuleEvaluation {
    /// Borrow match evidence when the rule matched.
    #[must_use]
    pub const fn match_evidence(&self) -> Option<&PatternMatchEvidence> {
        match &self.outcome {
            RuleEvaluationOutcome::Matched { evidence } => Some(evidence),
            _ => None,
        }
    }
}

fn evaluate_reasoning_quantization(
    key: RuleEvaluationKey,
    origin: DataOrigin,
    model_call: &ModelCall,
) -> RuleEvaluation {
    let Some(reasoning) = model_call.fields().reasoning_output_tokens.as_ref() else {
        return RuleEvaluation {
            key,
            origin,
            outcome: RuleEvaluationOutcome::MissingValue,
        };
    };
    let value = reasoning.value().get();
    if value == 0 {
        return RuleEvaluation {
            key,
            origin,
            outcome: RuleEvaluationOutcome::NonPositiveValue,
        };
    }
    let adjusted = value + QUANTIZATION_OFFSET;
    if !adjusted.is_multiple_of(QUANTIZATION_STEP) {
        return RuleEvaluation {
            key,
            origin,
            outcome: RuleEvaluationOutcome::NoMatch,
        };
    }
    let Some(pattern_n) = NonZeroU64::new(adjusted / QUANTIZATION_STEP).map(PatternN) else {
        return RuleEvaluation {
            key,
            origin,
            outcome: RuleEvaluationOutcome::NonPositiveValue,
        };
    };
    RuleEvaluation {
        key,
        origin,
        outcome: RuleEvaluationOutcome::Matched {
            evidence: PatternMatchEvidence {
                model_call_id: key.model_call_id,
                rule_id: key.rule_id,
                rule_version: key.rule_version,
                reasoning_output_tokens: *reasoning.value(),
                pattern_n,
                event_time: *model_call.event_time().value(),
                token_provenance: reasoning.provenance().clone(),
            },
        },
    }
}

/// User-selected maximum `n`, or all matches.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "mode", content = "maximum_n", rename_all = "snake_case")]
pub enum NotificationThreshold {
    AtMost(NonZeroU64),
    All,
}

impl Default for NotificationThreshold {
    fn default() -> Self {
        Self::AtMost(NonZeroU64::MIN)
    }
}

impl NotificationThreshold {
    /// Return whether a formula match is inside this threshold.
    #[must_use]
    pub const fn includes(self, pattern_n: PatternN) -> bool {
        match self {
            Self::AtMost(maximum) => pattern_n.get() <= maximum.get(),
            Self::All => true,
        }
    }
}

/// Notification settings applied after evidence is durably retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct RuleNotificationPolicy {
    pub enabled: bool,
    pub threshold: NotificationThreshold,
}

impl Default for RuleNotificationPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold: NotificationThreshold::default(),
        }
    }
}

/// Privacy-safe action eligibility derived after a match.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationEligibility {
    Eligible,
    NotificationsDisabled,
    AboveThreshold,
    HistoricalEvent,
}

impl RuleNotificationPolicy {
    /// Decide notification eligibility without affecting alert retention.
    #[must_use]
    pub fn eligibility(
        self,
        origin: DataOrigin,
        evidence: &PatternMatchEvidence,
    ) -> NotificationEligibility {
        if origin == DataOrigin::Historical {
            NotificationEligibility::HistoricalEvent
        } else if !self.enabled {
            NotificationEligibility::NotificationsDisabled
        } else if !self.threshold.includes(evidence.pattern_n) {
            NotificationEligibility::AboveThreshold
        } else {
            NotificationEligibility::Eligible
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::{
        CanonicalField, CanonicalModelCallFields, EvidenceQuality, FieldProvenance,
        SourceContractVersion, SourceEventIdentity, SourceId, SourceIdentity, SourcePriority,
        StableSourceEventId,
    };

    fn at(seconds: i128) -> UtcTimestamp {
        UtcTimestamp::from_unix_timestamp_nanos(seconds * 1_000_000_000).unwrap()
    }

    fn provenance() -> FieldProvenance {
        FieldProvenance::new(
            SourceIdentity::new(
                SourceId::new("otlp-logs").unwrap(),
                SourceContractVersion::new(1).unwrap(),
            ),
            at(10),
            EvidenceQuality::Direct,
            SourcePriority::new(100),
        )
    }

    fn field<T: Ord>(value: T) -> CanonicalField<T> {
        CanonicalField::new(crate::Observed::new(value, provenance()))
    }

    fn model_call(value: Option<u64>, origin: DataOrigin) -> ModelCall {
        let identity = SourceEventIdentity::from_stable_id(
            provenance().source().clone(),
            &StableSourceEventId::new("rule-test-model-call").unwrap(),
        );
        ModelCall::new(
            ModelCallId::new(),
            identity,
            field(at(9)),
            at(10),
            origin,
            CanonicalModelCallFields {
                reasoning_output_tokens: value.map(|value| field(TokenCount::new(value).unwrap())),
                ..CanonicalModelCallFields::default()
            },
        )
    }

    fn evaluate(value: Option<u64>, origin: DataOrigin) -> RuleEvaluation {
        SUSPECTED_REASONING_LIMIT_PATTERN.evaluate(&model_call(value, origin))
    }

    #[test]
    fn formula_boundaries_match_only_registered_values() {
        for value in [515, 517] {
            assert!(matches!(
                evaluate(Some(value), DataOrigin::Realtime).outcome,
                RuleEvaluationOutcome::NoMatch
            ));
        }
        for (value, expected_n) in [(516, 1), (1_034, 2), (1_552, 3), (2_070, 4)] {
            assert_eq!(
                evaluate(Some(value), DataOrigin::Realtime)
                    .match_evidence()
                    .unwrap()
                    .pattern_n
                    .get(),
                expected_n
            );
        }
        assert!(matches!(
            evaluate(None, DataOrigin::Realtime).outcome,
            RuleEvaluationOutcome::MissingValue
        ));
        assert!(matches!(
            evaluate(Some(0), DataOrigin::Realtime).outcome,
            RuleEvaluationOutcome::NonPositiveValue
        ));
    }

    #[test]
    fn default_threshold_and_historical_isolation_are_explicit() {
        let default_policy = RuleNotificationPolicy::default();
        let first = evaluate(Some(516), DataOrigin::Realtime);
        let fourth = evaluate(Some(2_070), DataOrigin::Realtime);
        let historical = evaluate(Some(516), DataOrigin::Historical);

        assert_eq!(
            default_policy.eligibility(DataOrigin::Realtime, first.match_evidence().unwrap()),
            NotificationEligibility::Eligible
        );
        assert_eq!(
            default_policy.eligibility(DataOrigin::Realtime, fourth.match_evidence().unwrap()),
            NotificationEligibility::AboveThreshold
        );
        assert_eq!(
            default_policy
                .eligibility(DataOrigin::Historical, historical.match_evidence().unwrap()),
            NotificationEligibility::HistoricalEvent
        );
    }

    #[test]
    fn registry_rejects_unknown_and_dynamic_rule_content() {
        let valid = RuleReferenceInput {
            id: "suspected-reasoning-limit-pattern",
            version: 1,
            field: "model_call.reasoning_output_tokens",
            operator: "reasoning_quantization_518_minus_2",
            dynamic_content: None,
        };
        assert_eq!(
            BuiltInRuleRegistry::resolve(valid).unwrap(),
            &SUSPECTED_REASONING_LIMIT_PATTERN
        );
        assert_eq!(
            BuiltInRuleRegistry::resolve(RuleReferenceInput {
                dynamic_content: Some("reasoning_output_tokens > 0; run('cmd')"),
                ..valid
            }),
            Err(RuleValidationError::DynamicContentRejected)
        );
        assert_eq!(
            BuiltInRuleRegistry::resolve(RuleReferenceInput {
                field: "content.prompt",
                ..valid
            }),
            Err(RuleValidationError::UnknownField)
        );
    }

    #[test]
    fn registry_enumerates_every_active_definition() {
        assert_eq!(
            BuiltInRuleRegistry::active_definitions(),
            &[SUSPECTED_REASONING_LIMIT_PATTERN]
        );
    }

    #[test]
    fn rule_error_codes_are_closed_and_stable() {
        for code in [
            RuleErrorCode::PersistenceCommitFailed,
            RuleErrorCode::InternalQueueClosed,
        ] {
            assert_eq!(code.as_str().parse::<RuleErrorCode>(), Ok(code));
            assert_eq!(serde_json::to_string(&code).unwrap(), format!("\"{code}\""));
        }
        assert_eq!(
            "raw-rule-error".parse::<RuleErrorCode>(),
            Err(RuleValidationError::UnknownFailureCode)
        );
    }

    #[test]
    fn definition_evaluation_uses_its_own_identity() {
        let definition = BuiltInRuleDefinition {
            version: RuleVersion::new(NonZeroU16::new(7).unwrap()),
            ..SUSPECTED_REASONING_LIMIT_PATTERN
        };
        let model_call = model_call(Some(516), DataOrigin::Realtime);

        let evaluation = definition.evaluate(&model_call);

        assert_eq!(
            evaluation.key,
            RuleEvaluationKey {
                rule_id: definition.id,
                rule_version: definition.version,
                model_call_id: model_call.id(),
            }
        );
        let evidence = evaluation.match_evidence().unwrap();
        assert_eq!(evidence.rule_id, definition.id);
        assert_eq!(evidence.rule_version, definition.version);
        assert_eq!(evidence.model_call_id, model_call.id());
    }

    #[test]
    fn rule_version_change_keeps_evaluation_histories_distinct() {
        let model_call = model_call(Some(516), DataOrigin::Realtime);
        let version_one = SUSPECTED_REASONING_LIMIT_PATTERN;
        let version_two = BuiltInRuleDefinition {
            version: RuleVersion::new(NonZeroU16::new(2).unwrap()),
            ..version_one
        };

        let first = version_one.evaluate(&model_call);
        let second = version_two.evaluate(&model_call);

        assert_eq!(first.key.model_call_id, model_call.id());
        assert_eq!(second.key.model_call_id, model_call.id());
        assert_eq!(first.key.rule_version, version_one.version);
        assert_eq!(second.key.rule_version, version_two.version);
        assert_ne!(first.key, second.key);
        assert_eq!(
            first.match_evidence().unwrap().rule_version,
            version_one.version
        );
        assert_eq!(
            second.match_evidence().unwrap().rule_version,
            version_two.version
        );
        assert_ne!(
            version_one.semantic_fingerprint(),
            version_two.semantic_fingerprint()
        );
    }

    #[test]
    fn semantic_fingerprint_is_stable_and_version_sensitive() {
        let definition = SUSPECTED_REASONING_LIMIT_PATTERN;
        let next_version = BuiltInRuleDefinition {
            version: RuleVersion::new(NonZeroU16::new(2).unwrap()),
            ..definition
        };

        assert_eq!(
            definition.semantic_fingerprint(),
            definition.semantic_fingerprint()
        );
        assert_ne!(
            definition.semantic_fingerprint(),
            next_version.semantic_fingerprint()
        );
    }

    #[test]
    fn suspected_reasoning_limit_v1_fingerprint_matches_test_vector() {
        assert_eq!(
            SUSPECTED_REASONING_LIMIT_PATTERN.semantic_fingerprint(),
            [
                78, 4, 250, 249, 40, 177, 105, 150, 80, 92, 243, 96, 119, 184, 78, 121, 77, 205,
                91, 98, 210, 146, 74, 119, 209, 188, 150, 115, 54, 252, 106, 159,
            ]
        );
    }

    proptest! {
        #[test]
        fn quantization_formula_matches_exactly(value in 1_u64..1_000_000) {
            let evaluation = evaluate(Some(value), DataOrigin::Realtime);
            let expected = (value + QUANTIZATION_OFFSET).is_multiple_of(QUANTIZATION_STEP);
            prop_assert_eq!(evaluation.match_evidence().is_some(), expected);
        }
    }
}
