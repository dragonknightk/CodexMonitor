use std::fmt::Write as _;
use std::num::NonZeroU16;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    ConversationSourceId, ModelName, SourceIdentity, StableSourceEventId, TokenCount, UtcTimestamp,
    ValidationError,
};

const IDENTITY_DOMAIN: &[u8] = b"codex-monitor/source-event-identity";

/// Version of the canonical source-event identity encoding.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct IdentityAlgorithmVersion(NonZeroU16);

impl IdentityAlgorithmVersion {
    /// Identity/fingerprint encoding used by `CodexMonitor` v1.
    pub const V1: Self = Self(NonZeroU16::MIN);

    /// Return the numeric algorithm version.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

impl<'de> Deserialize<'de> for IdentityAlgorithmVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u16::deserialize(deserializer)?;
        if value == Self::V1.get() {
            Ok(Self::V1)
        } else {
            Err(D::Error::custom(
                ValidationError::UnsupportedIdentityAlgorithmVersion { value },
            ))
        }
    }
}

/// Whether an identity used a source's stable identifier or a fallback digest.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityKind {
    Stable,
    FallbackFingerprint,
}

/// Allowlisted token fields used by the v1 fallback fingerprint.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FingerprintTokenFields {
    pub input: Option<TokenCount>,
    pub cached_input: Option<TokenCount>,
    pub output: Option<TokenCount>,
    pub reasoning_output: Option<TokenCount>,
}

/// Allowlisted inputs to the v1 fallback fingerprint.
#[derive(Clone, Copy, Debug)]
pub struct FallbackFingerprintInput<'a> {
    pub event_time: UtcTimestamp,
    pub conversation: Option<&'a ConversationSourceId>,
    pub model: Option<&'a ModelName>,
    pub tokens: FingerprintTokenFields,
}

/// Versioned deterministic identity for an event emitted by one source.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SourceEventIdentity {
    source: SourceIdentity,
    algorithm_version: IdentityAlgorithmVersion,
    kind: IdentityKind,
    digest: [u8; 32],
}

impl SourceEventIdentity {
    /// Build the preferred identity from a stable source event ID.
    #[must_use]
    pub fn from_stable_id(source: SourceIdentity, stable_id: &StableSourceEventId) -> Self {
        let mut encoder = IdentityEncoder::new(IdentityKind::Stable, &source);
        encoder.text(1, stable_id.as_str());
        Self::finish(source, IdentityKind::Stable, encoder)
    }

    /// Build a fallback fingerprint from the privacy-reviewed allowlist.
    #[must_use]
    pub fn from_fallback(source: SourceIdentity, input: FallbackFingerprintInput<'_>) -> Self {
        let mut encoder = IdentityEncoder::new(IdentityKind::FallbackFingerprint, &source);
        encoder.i128(1, input.event_time.unix_timestamp_nanos());
        encoder.optional_text(2, input.conversation.map(ConversationSourceId::as_str));
        encoder.optional_text(3, input.model.map(ModelName::as_str));
        encoder.optional_u64(4, input.tokens.input.map(TokenCount::get));
        encoder.optional_u64(5, input.tokens.cached_input.map(TokenCount::get));
        encoder.optional_u64(6, input.tokens.output.map(TokenCount::get));
        encoder.optional_u64(7, input.tokens.reasoning_output.map(TokenCount::get));
        Self::finish(source, IdentityKind::FallbackFingerprint, encoder)
    }

    /// Rebuild an identity from validated persistence columns.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidIdentityDigest`] for the all-zero
    /// sentinel. The algorithm version type admits only versions supported by
    /// this binary.
    pub fn from_stored_digest(
        source: SourceIdentity,
        algorithm_version: IdentityAlgorithmVersion,
        kind: IdentityKind,
        digest: [u8; 32],
    ) -> Result<Self, ValidationError> {
        if digest == [0; 32] {
            return Err(ValidationError::InvalidIdentityDigest);
        }
        Ok(Self {
            source,
            algorithm_version,
            kind,
            digest,
        })
    }

    fn finish(source: SourceIdentity, kind: IdentityKind, encoder: IdentityEncoder) -> Self {
        Self {
            source,
            algorithm_version: IdentityAlgorithmVersion::V1,
            kind,
            digest: encoder.finish(),
        }
    }

    /// Return the source namespace for this identity.
    #[must_use]
    pub const fn source(&self) -> &SourceIdentity {
        &self.source
    }

    /// Return the encoding version persisted with the identity.
    #[must_use]
    pub const fn algorithm_version(&self) -> IdentityAlgorithmVersion {
        self.algorithm_version
    }

    /// Return whether the identity is stable or fallback-derived.
    #[must_use]
    pub const fn kind(&self) -> IdentityKind {
        self.kind
    }

    /// Return the binary SHA-256 digest.
    #[must_use]
    pub const fn digest(&self) -> &[u8; 32] {
        &self.digest
    }

    /// Return the lowercase hexadecimal digest used in diagnostics and fixtures.
    #[must_use]
    pub fn digest_hex(&self) -> String {
        let mut value = String::with_capacity(64);
        for byte in self.digest {
            write!(&mut value, "{byte:02x}").expect("writing to String cannot fail");
        }
        value
    }
}

struct IdentityEncoder(Sha256);

impl IdentityEncoder {
    fn new(kind: IdentityKind, source: &SourceIdentity) -> Self {
        let mut encoder = Self(Sha256::new());
        encoder.bytes(0, IDENTITY_DOMAIN);
        encoder.u16(1, IdentityAlgorithmVersion::V1.get());
        encoder.u16(
            2,
            match kind {
                IdentityKind::Stable => 1,
                IdentityKind::FallbackFingerprint => 2,
            },
        );
        encoder.text(3, source.id().as_str());
        encoder.u16(4, source.contract_version().get());
        encoder
    }

    fn field_header(&mut self, tag: u8, length: usize) {
        self.0.update([tag]);
        self.0.update(
            u64::try_from(length)
                .expect("field length fits u64")
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

    fn i128(&mut self, tag: u8, value: i128) {
        self.bytes(tag, &value.to_be_bytes());
    }

    fn optional_text(&mut self, tag: u8, value: Option<&str>) {
        match value {
            Some(value) => {
                self.bytes(tag, &[1]);
                self.text(tag, value);
            }
            None => self.bytes(tag, &[0]),
        }
    }

    fn optional_u64(&mut self, tag: u8, value: Option<u64>) {
        match value {
            Some(value) => {
                self.bytes(tag, &[1]);
                self.bytes(tag, &value.to_be_bytes());
            }
            None => self.bytes(tag, &[0]),
        }
    }

    fn finish(self) -> [u8; 32] {
        self.0.finalize().into()
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::{SourceContractVersion, SourceId};

    fn source() -> SourceIdentity {
        SourceIdentity::new(
            SourceId::new("otlp-logs").unwrap(),
            SourceContractVersion::new(1).unwrap(),
        )
    }

    #[test]
    fn stable_identity_matches_v1_test_vector() {
        let identity = SourceEventIdentity::from_stable_id(
            source(),
            &StableSourceEventId::new("response_01JTEST").unwrap(),
        );

        assert_eq!(identity.kind(), IdentityKind::Stable);
        assert_eq!(identity.algorithm_version(), IdentityAlgorithmVersion::V1);
        assert_eq!(
            identity.digest_hex(),
            "896ce679924c573d2f037f815929e671ff641452499e40e9fd14fc322758e4f1"
        );
    }

    #[test]
    fn persistence_rebuild_rejects_unknown_versions_and_zero_digest() {
        assert!(serde_json::from_str::<IdentityAlgorithmVersion>("2").is_err());
        assert_eq!(
            SourceEventIdentity::from_stored_digest(
                source(),
                IdentityAlgorithmVersion::V1,
                IdentityKind::Stable,
                [0; 32],
            ),
            Err(ValidationError::InvalidIdentityDigest)
        );
    }

    #[test]
    fn fallback_identity_matches_v1_test_vector() {
        let conversation = ConversationSourceId::new("0190-test-conversation").unwrap();
        let model = ModelName::new("gpt-5.6-terra").unwrap();
        let identity = SourceEventIdentity::from_fallback(
            source(),
            FallbackFingerprintInput {
                event_time: UtcTimestamp::parse_rfc3339("2026-07-14T12:34:56.789Z").unwrap(),
                conversation: Some(&conversation),
                model: Some(&model),
                tokens: FingerprintTokenFields {
                    input: Some(TokenCount::new(1_024).unwrap()),
                    cached_input: Some(TokenCount::new(512).unwrap()),
                    output: Some(TokenCount::new(258).unwrap()),
                    reasoning_output: Some(TokenCount::new(256).unwrap()),
                },
            },
        );

        assert_eq!(identity.kind(), IdentityKind::FallbackFingerprint);
        assert_eq!(
            identity.digest_hex(),
            "073fdd3920b577a24f4834e80793edccabcf2e5395c71e91528586c951cfcc3d"
        );
    }

    #[test]
    fn length_prefixes_prevent_cross_field_ambiguity() {
        let first_conversation = ConversationSourceId::new("ab").unwrap();
        let first_model = ModelName::new("c").unwrap();
        let second_conversation = ConversationSourceId::new("a").unwrap();
        let second_model = ModelName::new("bc").unwrap();
        let tokens = FingerprintTokenFields::default();
        let first = SourceEventIdentity::from_fallback(
            source(),
            FallbackFingerprintInput {
                event_time: UtcTimestamp::from_unix_timestamp_nanos(1).unwrap(),
                conversation: Some(&first_conversation),
                model: Some(&first_model),
                tokens,
            },
        );
        let second = SourceEventIdentity::from_fallback(
            source(),
            FallbackFingerprintInput {
                event_time: UtcTimestamp::from_unix_timestamp_nanos(1).unwrap(),
                conversation: Some(&second_conversation),
                model: Some(&second_model),
                tokens,
            },
        );

        assert_ne!(first, second);
    }

    proptest! {
        #[test]
        fn stable_identity_is_repeatable(id in "[A-Za-z0-9][A-Za-z0-9_.:-]{0,63}") {
            let id = StableSourceEventId::new(id).unwrap();
            let first = SourceEventIdentity::from_stable_id(source(), &id);
            let second = SourceEventIdentity::from_stable_id(source(), &id);
            prop_assert_eq!(first, second);
        }

        #[test]
        fn fallback_identity_is_repeatable_for_arbitrary_allowlisted_inputs(
            seconds in 0_i128..4_102_444_800,
            conversation in "[A-Za-z0-9][A-Za-z0-9_.:-]{0,63}",
            model in "[A-Za-z0-9][A-Za-z0-9._-]{0,31}",
            input in 0_u64..1_000_000,
            output in 0_u64..1_000_000,
            reasoning in 0_u64..1_000_000,
        ) {
            let conversation = ConversationSourceId::new(conversation).unwrap();
            let model = ModelName::new(model).unwrap();
            let event_time = UtcTimestamp::from_unix_timestamp_nanos(
                seconds * 1_000_000_000,
            ).unwrap();
            let input = FallbackFingerprintInput {
                event_time,
                conversation: Some(&conversation),
                model: Some(&model),
                tokens: FingerprintTokenFields {
                    input: Some(TokenCount::new(input).unwrap()),
                    cached_input: None,
                    output: Some(TokenCount::new(output).unwrap()),
                    reasoning_output: Some(TokenCount::new(reasoning).unwrap()),
                },
            };

            let first = SourceEventIdentity::from_fallback(source(), input);
            let second = SourceEventIdentity::from_fallback(source(), input);
            prop_assert_eq!(first, second);
        }
    }
}
