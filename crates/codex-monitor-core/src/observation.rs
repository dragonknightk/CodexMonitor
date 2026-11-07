use serde::Serialize;
use thiserror::Error;

use crate::{
    DataOrigin, ModelCallId, ModelCallObservation, ModelCallObservationId, ModelCallRelationship,
    ObservationFields, Observed, SourceEventIdentity, UtcTimestamp, ValidationError,
};

/// 经过 allowlist 抽取后产生的 observation 语义类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceObservationKind {
    CompletedModelCall,
    ConversationMetadata,
}

/// 相关性处理前的 observation，只包含字段目录允许的领域字段。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SourceObservationCandidate {
    kind: SourceObservationKind,
    source_event_identity: SourceEventIdentity,
    event_time: Observed<UtcTimestamp>,
    observed_at: UtcTimestamp,
    origin: DataOrigin,
    fields: ObservationFields,
}

impl SourceObservationCandidate {
    /// 创建来源边界一致的 observation candidate。
    ///
    /// # Errors
    ///
    /// 当 event 或字段 provenance 指向不同来源时返回领域错误。
    pub fn new(
        kind: SourceObservationKind,
        source_event_identity: SourceEventIdentity,
        event_time: Observed<UtcTimestamp>,
        observed_at: UtcTimestamp,
        origin: DataOrigin,
        fields: ObservationFields,
    ) -> Result<Self, SourceObservationError> {
        if event_time.provenance().source() != source_event_identity.source()
            || !fields.all_from(source_event_identity.source())
        {
            return Err(SourceObservationError::InvalidDomain(
                ValidationError::ObservationSourceMismatch,
            ));
        }
        Ok(Self {
            kind,
            source_event_identity,
            event_time,
            observed_at,
            origin,
            fields,
        })
    }

    #[must_use]
    pub const fn kind(&self) -> SourceObservationKind {
        self.kind
    }

    #[must_use]
    pub const fn source_event_identity(&self) -> &SourceEventIdentity {
        &self.source_event_identity
    }

    #[must_use]
    pub const fn event_time(&self) -> &Observed<UtcTimestamp> {
        &self.event_time
    }

    #[must_use]
    pub const fn observed_at(&self) -> UtcTimestamp {
        self.observed_at
    }

    #[must_use]
    pub const fn origin(&self) -> DataOrigin {
        self.origin
    }

    #[must_use]
    pub const fn fields(&self) -> &ObservationFields {
        &self.fields
    }

    /// 在 storage transaction 内用 canonical ID 关联 completed-call candidate。
    ///
    /// # Errors
    ///
    /// metadata-only candidate 或领域证据不一致时返回错误。
    pub fn into_model_call_observation(
        mut self,
        model_call_id: ModelCallId,
        observation_id: ModelCallObservationId,
        relationship: Option<ModelCallRelationship>,
    ) -> Result<ModelCallObservation, SourceObservationError> {
        if self.kind != SourceObservationKind::CompletedModelCall {
            return Err(SourceObservationError::NotCompletedModelCall);
        }
        if let Some(relationship) = relationship {
            self.fields.relationship = Some(Observed::new(
                relationship,
                self.event_time.provenance().clone(),
            ));
        }
        ModelCallObservation::new(
            observation_id,
            model_call_id,
            self.source_event_identity,
            self.event_time,
            self.observed_at,
            self.origin,
            self.fields,
        )
        .map_err(SourceObservationError::InvalidDomain)
    }
}

/// 一个 decoded source batch 的隐私安全抽取摘要。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct SourceExtraction {
    pub records_seen: u64,
    pub rejected_records: u64,
    pub ignored_records: u64,
    pub observations: Vec<SourceObservationCandidate>,
}

/// 不携带原始来源值的 candidate 转换失败。
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SourceObservationError {
    #[error("source observation is not a completed model call")]
    NotCompletedModelCall,
    #[error("source observation violates the domain contract")]
    InvalidDomain(ValidationError),
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::{
        EvidenceQuality, FallbackFingerprintInput, FieldProvenance, FingerprintTokenFields,
        ModelName, SourceContractVersion, SourceId, SourceIdentity, SourcePriority,
    };

    fn at(seconds: i128) -> UtcTimestamp {
        UtcTimestamp::from_unix_timestamp_nanos(seconds * 1_000_000_000).unwrap()
    }

    fn source(id: &str) -> SourceIdentity {
        SourceIdentity::new(
            SourceId::new(id).unwrap(),
            SourceContractVersion::new(1).unwrap(),
        )
    }

    fn candidate(source: SourceIdentity) -> SourceObservationCandidate {
        let provenance = FieldProvenance::new(
            source.clone(),
            at(2),
            EvidenceQuality::Direct,
            SourcePriority::new(10),
        );
        SourceObservationCandidate::new(
            SourceObservationKind::CompletedModelCall,
            SourceEventIdentity::from_fallback(
                source,
                FallbackFingerprintInput {
                    event_time: at(1),
                    conversation: None,
                    model: None,
                    tokens: FingerprintTokenFields::default(),
                },
            ),
            Observed::new(at(1), provenance.clone()),
            at(2),
            DataOrigin::Realtime,
            ObservationFields {
                model: Some(Observed::new(
                    ModelName::new("synthetic-model").unwrap(),
                    provenance,
                )),
                ..ObservationFields::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn constructor_rejects_mixed_source_provenance() {
        let identity_source = source("source-a");
        let other_source = source("source-b");
        let event_time = Observed::new(
            at(1),
            FieldProvenance::new(
                other_source,
                at(2),
                EvidenceQuality::Direct,
                SourcePriority::new(10),
            ),
        );

        assert_eq!(
            SourceObservationCandidate::new(
                SourceObservationKind::CompletedModelCall,
                SourceEventIdentity::from_fallback(
                    identity_source,
                    FallbackFingerprintInput {
                        event_time: at(1),
                        conversation: None,
                        model: None,
                        tokens: FingerprintTokenFields::default(),
                    },
                ),
                event_time,
                at(2),
                DataOrigin::Realtime,
                ObservationFields::default(),
            ),
            Err(SourceObservationError::InvalidDomain(
                ValidationError::ObservationSourceMismatch,
            ))
        );
    }

    #[test]
    fn metadata_candidate_cannot_become_model_call_observation() {
        let mut value = candidate(source("source-a"));
        value.kind = SourceObservationKind::ConversationMetadata;

        assert_eq!(
            value.into_model_call_observation(
                ModelCallId::new(),
                ModelCallObservationId::new(),
                None,
            ),
            Err(SourceObservationError::NotCompletedModelCall)
        );
    }

    #[test]
    fn serialization_exposes_only_closed_allowlisted_fields() {
        let json = serde_json::to_value(SourceExtraction {
            records_seen: 1,
            observations: vec![candidate(source("source-a"))],
            ..SourceExtraction::default()
        })
        .unwrap();
        let object = json.as_object().unwrap();
        assert_eq!(
            object.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "ignored_records",
                "observations",
                "records_seen",
                "rejected_records",
            ]
        );

        let encoded = serde_json::to_string(&json).unwrap();
        for forbidden in [
            "attribute_map",
            "authentication",
            "generic_json",
            "prompt",
            "raw_otlp",
            "reasoning_content",
            "response",
            "tool_data",
        ] {
            assert!(!encoded.contains(forbidden), "found {forbidden}");
        }
        assert!(matches!(json, Value::Object(_)));
    }
}
