use std::fmt;
use std::num::NonZeroU16;
use std::str::FromStr;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// Largest token value admitted at the domain boundary.
///
/// The value remains well below `SQLite`'s signed 64-bit limit while retaining the
/// Python baseline's accepted range.
pub const MAX_TOKEN_COUNT: u64 = 1_000_000_000_000_000;

/// Validation failure for a typed monitoring value.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ValidationError {
    /// A required string was empty.
    #[error("{field} must not be empty")]
    Empty { field: &'static str },
    /// A string exceeded its field-specific length bound.
    #[error("{field} exceeds the maximum length of {max}")]
    TooLong {
        field: &'static str,
        max: usize,
        actual: usize,
    },
    /// A string contained a character outside the technical-label alphabet.
    #[error("{field} contains an invalid character at byte {index}")]
    InvalidCharacter {
        field: &'static str,
        index: usize,
        character: char,
    },
    /// A telemetry field ID did not use the catalog's dotted lower-snake form.
    #[error("field_id must use dotted lower_snake_case segments")]
    InvalidFieldId,
    /// A token value was negative.
    #[error("token count must be non-negative")]
    NegativeToken { value: i64 },
    /// A token value exceeded the safe numeric bound.
    #[error("token count exceeds the safe maximum of {max}")]
    TokenOutOfRange { value: u64, max: u64 },
    /// A token sum exceeded the safe numeric bound.
    #[error("token sum exceeds the safe maximum of {max}")]
    TokenOverflow { max: u64 },
    /// A reasoning-effort string was not registered.
    #[error("unknown reasoning effort")]
    UnknownReasoningEffort,
    /// A source-health error code was not registered.
    #[error("unknown safe error code")]
    UnknownSafeErrorCode,
    /// A source contract version was zero.
    #[error("source contract version must be positive")]
    ZeroSourceContractVersion,
    /// A timestamp could not be represented as a UTC instant.
    #[error("invalid UTC timestamp")]
    InvalidTimestamp,
    /// A persisted identity algorithm version is unsupported.
    #[error("unsupported identity algorithm version: {value}")]
    UnsupportedIdentityAlgorithmVersion { value: u16 },
    /// A persisted identity digest was structurally invalid.
    #[error("invalid source-event identity digest")]
    InvalidIdentityDigest,
    /// A model call referenced a turn without a conversation.
    #[error("a turn relationship requires a conversation relationship")]
    TurnWithoutConversation,
    /// An observation carried field evidence for a different source.
    #[error("observation field provenance does not match its source identity")]
    ObservationSourceMismatch,
    /// An observation was applied to a different canonical model call.
    #[error("observation model-call relationship does not match the target")]
    ObservationModelCallMismatch,
    /// A source declared more field capabilities than the bounded registry allows.
    #[error("source field capability count exceeds the maximum of {max}")]
    TooManySourceFields { max: usize, actual: usize },
    /// A rule or stored action referenced an unregistered action type.
    #[error("unknown action type")]
    UnknownActionType,
}

fn validate_label(
    field: &'static str,
    value: String,
    max_length: usize,
) -> Result<String, ValidationError> {
    if value.is_empty() {
        return Err(ValidationError::Empty { field });
    }
    if value.len() > max_length {
        return Err(ValidationError::TooLong {
            field,
            max: max_length,
            actual: value.len(),
        });
    }

    for (index, character) in value.char_indices() {
        let valid = if index == 0 {
            character.is_ascii_alphanumeric()
        } else {
            character.is_ascii_alphanumeric()
                || matches!(character, '.' | '_' | ':' | '+' | '(' | ')' | '-')
        };
        if !valid {
            return Err(ValidationError::InvalidCharacter {
                field,
                index,
                character,
            });
        }
    }
    Ok(value)
}

macro_rules! validated_label {
    ($name:ident, $field:literal, $max:expr, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            #[doc = concat!("Create a validated `", stringify!($name), "`.")]
            #[doc = ""]
            #[doc = "# Errors"]
            #[doc = ""]
            #[doc = "Returns an error when the value is empty, too long or contains a character outside the technical-label alphabet."]
            pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
                validate_label($field, value.into(), $max).map(Self)
            }

            #[doc = concat!("Borrow the validated `", stringify!($name), "` text.")]
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            #[doc = concat!("Consume this `", stringify!($name), "` and return its text.")]
            #[must_use]
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl TryFrom<String> for $name {
            type Error = ValidationError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(D::Error::custom)
            }
        }
    };
}

/// A stable registered telemetry-source identifier.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceId(String);

impl SourceId {
    /// Create a lower-segment source identifier.
    ///
    /// # Errors
    ///
    /// Returns an error unless the value is 1..=64 ASCII bytes matching
    /// `^[a-z0-9]+(?:[._-][a-z0-9]+)*$`.
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ValidationError::Empty { field: "source_id" });
        }
        if value.len() > 64 {
            return Err(ValidationError::TooLong {
                field: "source_id",
                max: 64,
                actual: value.len(),
            });
        }
        let mut previous_was_separator = false;
        for (index, character) in value.char_indices() {
            let is_alphanumeric = character.is_ascii_lowercase() || character.is_ascii_digit();
            let is_separator = matches!(character, '.' | '_' | '-');
            if (!is_alphanumeric && !is_separator)
                || (is_separator
                    && (index == 0
                        || index + character.len_utf8() == value.len()
                        || previous_was_separator))
            {
                return Err(ValidationError::InvalidCharacter {
                    field: "source_id",
                    index,
                    character,
                });
            }
            previous_was_separator = is_separator;
        }
        Ok(Self(value))
    }

    /// Borrow the stable protocol spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume this identifier and return its protocol spelling.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for SourceId {
    type Error = ValidationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<SourceId> for String {
    fn from(value: SourceId) -> Self {
        value.0
    }
}

impl Serialize for SourceId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SourceId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}
/// A stable field identifier from the privacy-reviewed catalog.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FieldId(String);

impl FieldId {
    /// Create a dotted lower-snake field ID.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, oversized or non-catalog-shaped values.
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ValidationError::Empty { field: "field_id" });
        }
        if value.len() > 128 {
            return Err(ValidationError::TooLong {
                field: "field_id",
                max: 128,
                actual: value.len(),
            });
        }
        let mut segment_count = 0;
        let segments_valid = value.split('.').all(|segment| {
            segment_count += 1;
            let mut characters = segment.chars();
            characters
                .next()
                .is_some_and(|first| first.is_ascii_lowercase())
                && characters.all(|character| {
                    character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
                })
        });
        if segment_count < 2 || !segments_valid {
            return Err(ValidationError::InvalidFieldId);
        }
        Ok(Self(value))
    }

    /// Borrow the stable catalog ID.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the ID and return its text.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for FieldId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for FieldId {
    type Error = ValidationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<FieldId> for String {
    fn from(value: FieldId) -> Self {
        value.0
    }
}

impl Serialize for FieldId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for FieldId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}
validated_label!(
    ConversationSourceId,
    "conversation_source_id",
    256,
    "A validated pseudonymous conversation identifier supplied by a source."
);
validated_label!(
    ThreadSourceId,
    "thread_source_id",
    256,
    "A validated pseudonymous Codex thread identifier supplied by a source."
);
validated_label!(
    TurnSourceId,
    "turn_source_id",
    256,
    "A validated pseudonymous turn identifier supplied by a source."
);
validated_label!(
    StableSourceEventId,
    "stable_source_event_id",
    256,
    "A validated stable response or event identifier supplied by a source."
);
validated_label!(ModelName, "model", 128, "A validated model identifier.");
validated_label!(
    SurfaceName,
    "surface",
    64,
    "A validated Codex surface identifier."
);
validated_label!(
    ClientName,
    "client",
    128,
    "A validated Codex client identifier."
);
validated_label!(
    EventName,
    "event_name",
    128,
    "A validated telemetry event identifier."
);
validated_label!(
    StreamEventKind,
    "stream_event_kind",
    128,
    "A validated stream event kind."
);

/// Closed, privacy-reviewed diagnostics codes allowed in source health.
///
/// The enum intentionally has no free-form variant: raw errors, paths, URLs,
/// identifiers and payload excerpts cannot be represented.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SafeErrorCode {
    OtlpBindFailed,
    OtlpInvalidContentType,
    OtlpCompressedBodyTooLarge,
    OtlpDecompressedBodyTooLarge,
    OtlpGzipDecodeFailed,
    OtlpJsonDecodeFailed,
    OtlpProtobufDecodeFailed,
    OtlpDecodeTimedOut,
    PersistenceCommitFailed,
    SourceCheckpointInvalid,
    SourceUnavailable,
    InternalQueueClosed,
}

impl SafeErrorCode {
    /// Return the stable persistence spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OtlpBindFailed => "otlp_bind_failed",
            Self::OtlpInvalidContentType => "otlp_invalid_content_type",
            Self::OtlpCompressedBodyTooLarge => "otlp_compressed_body_too_large",
            Self::OtlpDecompressedBodyTooLarge => "otlp_decompressed_body_too_large",
            Self::OtlpGzipDecodeFailed => "otlp_gzip_decode_failed",
            Self::OtlpJsonDecodeFailed => "otlp_json_decode_failed",
            Self::OtlpProtobufDecodeFailed => "otlp_protobuf_decode_failed",
            Self::OtlpDecodeTimedOut => "otlp_decode_timed_out",
            Self::PersistenceCommitFailed => "persistence_commit_failed",
            Self::SourceCheckpointInvalid => "source_checkpoint_invalid",
            Self::SourceUnavailable => "source_unavailable",
            Self::InternalQueueClosed => "internal_queue_closed",
        }
    }
}

impl fmt::Display for SafeErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for SafeErrorCode {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "otlp_bind_failed" => Ok(Self::OtlpBindFailed),
            "otlp_invalid_content_type" => Ok(Self::OtlpInvalidContentType),
            "otlp_compressed_body_too_large" => Ok(Self::OtlpCompressedBodyTooLarge),
            "otlp_decompressed_body_too_large" => Ok(Self::OtlpDecompressedBodyTooLarge),
            "otlp_gzip_decode_failed" => Ok(Self::OtlpGzipDecodeFailed),
            "otlp_json_decode_failed" => Ok(Self::OtlpJsonDecodeFailed),
            "otlp_protobuf_decode_failed" => Ok(Self::OtlpProtobufDecodeFailed),
            "otlp_decode_timed_out" => Ok(Self::OtlpDecodeTimedOut),
            "persistence_commit_failed" => Ok(Self::PersistenceCommitFailed),
            "source_checkpoint_invalid" => Ok(Self::SourceCheckpointInvalid),
            "source_unavailable" => Ok(Self::SourceUnavailable),
            "internal_queue_closed" => Ok(Self::InternalQueueClosed),
            _ => Err(ValidationError::UnknownSafeErrorCode),
        }
    }
}

/// A non-negative token count inside the safe persistence range.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct TokenCount(u64);

impl TokenCount {
    /// Create a validated token count.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::TokenOutOfRange`] when `value` exceeds
    /// [`MAX_TOKEN_COUNT`].
    pub const fn new(value: u64) -> Result<Self, ValidationError> {
        if value > MAX_TOKEN_COUNT {
            return Err(ValidationError::TokenOutOfRange {
                value,
                max: MAX_TOKEN_COUNT,
            });
        }
        Ok(Self(value))
    }

    /// Return the raw token count.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Add two token counts without crossing the safe bound.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::TokenOverflow`] if the sum overflows or exceeds
    /// [`MAX_TOKEN_COUNT`].
    pub fn checked_add(self, other: Self) -> Result<Self, ValidationError> {
        self.0
            .checked_add(other.0)
            .filter(|sum| *sum <= MAX_TOKEN_COUNT)
            .map(Self)
            .ok_or(ValidationError::TokenOverflow {
                max: MAX_TOKEN_COUNT,
            })
    }
}

impl TryFrom<i64> for TokenCount {
    type Error = ValidationError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        let value = u64::try_from(value).map_err(|_| ValidationError::NegativeToken { value })?;
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for TokenCount {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u64::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

/// Registered reasoning-effort values.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
    Ultra,
}

impl ReasoningEffort {
    /// Return the stable protocol spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
            Self::Ultra => "ultra",
        }
    }
}

impl fmt::Display for ReasoningEffort {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ReasoningEffort {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" => Ok(Self::None),
            "minimal" => Ok(Self::Minimal),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::Xhigh),
            "max" => Ok(Self::Max),
            "ultra" => Ok(Self::Ultra),
            _ => Err(ValidationError::UnknownReasoningEffort),
        }
    }
}

/// Positive version for a registered telemetry-source contract.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct SourceContractVersion(NonZeroU16);

impl SourceContractVersion {
    /// Create a positive source contract version.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::ZeroSourceContractVersion`] for zero.
    pub const fn new(value: u16) -> Result<Self, ValidationError> {
        match NonZeroU16::new(value) {
            Some(value) => Ok(Self(value)),
            None => Err(ValidationError::ZeroSourceContractVersion),
        }
    }

    /// Return the raw positive version.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

impl<'de> Deserialize<'de> for SourceContractVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u16::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

/// Deterministic priority within one evidence-quality tier.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(transparent)]
pub struct SourcePriority(u16);

impl SourcePriority {
    /// Create a source priority. Larger values win inside the same quality tier.
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Return the raw priority.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn labels_enforce_bounds_and_alphabet() {
        assert!(ModelName::new("gpt-5.6-terra").is_ok());
        assert!(ModelName::new("").is_err());
        assert!(ModelName::new("model with spaces").is_err());
        assert!(ModelName::new("https://example.com/model").is_err());
        assert!(ClientName::new("account@example.com").is_err());
        assert!(SurfaceName::new("C:/Users/example").is_err());
        assert!(SurfaceName::new("x".repeat(65)).is_err());
        assert!(FieldId::new("model_call.reasoning_output_tokens").is_ok());
        assert!(FieldId::new("model").is_err());
        assert!(FieldId::new("Model.Call").is_err());
    }

    #[test]
    fn source_ids_match_the_public_lower_segment_contract() {
        for value in ["otlp-logs", "legacy_import.v1", "0"] {
            assert_eq!(SourceId::new(value).unwrap().as_str(), value);
        }
        for value in [
            "OTLP-logs",
            "otlp+logs",
            "otlp--logs",
            "-otlp",
            "otlp-",
            "otlp..logs",
            "otlp/logs",
        ] {
            assert!(SourceId::new(value).is_err(), "{value}");
        }
    }

    #[test]
    fn reasoning_effort_is_closed_and_normalized() {
        assert_eq!(" HIGH ".parse(), Ok(ReasoningEffort::High));
        assert!("custom".parse::<ReasoningEffort>().is_err());
    }

    #[test]
    fn safe_error_codes_are_closed_and_cannot_carry_sensitive_text() {
        assert_eq!(
            "otlp_json_decode_failed".parse(),
            Ok(SafeErrorCode::OtlpJsonDecodeFailed)
        );
        for forbidden in [
            "https://user:secret@example.com",
            "C:/Users/example/private.log",
            "account@example.com",
            "token_sk-secret",
            "arbitrary_runtime_error",
        ] {
            assert!(forbidden.parse::<SafeErrorCode>().is_err());
        }
    }

    #[test]
    fn token_count_rejects_negative_and_excessive_values() {
        assert_eq!(
            TokenCount::try_from(-1),
            Err(ValidationError::NegativeToken { value: -1 })
        );
        assert!(TokenCount::new(MAX_TOKEN_COUNT).is_ok());
        assert!(TokenCount::new(MAX_TOKEN_COUNT + 1).is_err());
    }

    #[test]
    fn token_deserialization_rejects_negative_fractional_and_excessive_values() {
        for invalid in ["-1", "1.5", "1000000000000001"] {
            assert!(serde_json::from_str::<TokenCount>(invalid).is_err());
        }
        assert_eq!(
            serde_json::from_str::<TokenCount>("1000000000000000").unwrap(),
            TokenCount::new(MAX_TOKEN_COUNT).unwrap()
        );
    }

    proptest! {
        #[test]
        fn token_validation_matches_the_documented_bound(value in any::<u64>()) {
            prop_assert_eq!(TokenCount::new(value).is_ok(), value <= MAX_TOKEN_COUNT);
        }

        #[test]
        fn whitespace_never_becomes_a_valid_model(prefix in "[A-Za-z0-9]{1,16}", suffix in "[A-Za-z0-9]{1,16}") {
            let value = format!("{prefix} {suffix}");
            prop_assert!(ModelName::new(value).is_err());
        }
    }
}
