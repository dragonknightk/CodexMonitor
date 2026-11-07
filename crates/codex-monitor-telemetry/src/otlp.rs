use std::str::FromStr;

use codex_monitor_core::{
    ClientName, ConversationSourceId, DataOrigin, EventName, EvidenceQuality,
    FallbackFingerprintInput, FieldId, FieldIdSet, FieldProvenance, FingerprintTokenFields,
    ModelName, OTLP_SOURCE_ID, ObservationFields, Observed, ReasoningEffort, SourceContractVersion,
    SourceEventIdentity, SourceHealth, SourceId, SourceIdentity, SourcePriority,
    StableSourceEventId, StreamEventKind, SurfaceName, ThreadSourceId, TokenCount, UtcTimestamp,
    ValidationError,
};
use opentelemetry_proto::tonic::{
    collector::logs::v1::ExportLogsServiceRequest,
    common::v1::{AnyValue, KeyValue, KeyValueList, any_value::Value as AnyValueKind},
    logs::v1::LogRecord,
};
use serde::Deserialize;
use serde_json::Value as JsonValue;

use super::{
    CheckpointCapability, SourceExtraction, SourceObservationCandidate, SourceObservationKind,
    TelemetrySource, TelemetrySourceDefinition,
};

const SOURCE_PRIORITY: u16 = 100;
const MAX_BODY_JSON_BYTES: usize = 1_048_576;

const SUPPORTED_FIELDS: &[&str] = &[
    "conversation.source_id",
    "conversation.thread_id",
    "model_call.cached_input_tokens",
    "model_call.client",
    "model_call.event_at",
    "model_call.event_name",
    "model_call.input_tokens",
    "model_call.model",
    "model_call.temporal_class",
    "model_call.output_tokens",
    "model_call.reasoning_effort",
    "model_call.reasoning_output_tokens",
    "model_call.stream_event_kind",
    "model_call.surface",
    "model_call_observation.field_provenance",
    "model_call_observation.field_quality",
    "model_call_observation.observed_at",
    "model_call_observation.source_contract_version",
    "model_call_observation.source_event_identity",
    "model_call_observation.source_event_identity_version",
    "model_call_observation.source_id",
];

const EVENT_NAME_KEYS: &[&str] = &[
    "event.name",
    "event_name",
    "otel.event.name",
    "name",
    "log.name",
];
const STREAM_KIND_KEYS: &[&str] = &[
    "stream_event_kind",
    "sse_event_kind",
    "event.kind",
    "kind",
    "type",
    "response.type",
];

/// Full v1 adapter for the local Codex OTLP log stream.
#[derive(Clone, Debug)]
pub struct OtlpLogSource {
    definition: TelemetrySourceDefinition,
    health: SourceHealth,
}

impl OtlpLogSource {
    /// Build the registered v1 source and its exact field capability set.
    ///
    /// # Errors
    ///
    /// Returns a domain validation error if the compile-time source registry
    /// and field catalog no longer agree.
    pub fn new(now: UtcTimestamp) -> Result<Self, ValidationError> {
        let identity = SourceIdentity::new(
            SourceId::new(OTLP_SOURCE_ID)?,
            SourceContractVersion::new(1)?,
        );
        let supported_fields = FieldIdSet::new(
            SUPPORTED_FIELDS
                .iter()
                .map(|field| FieldId::new(*field))
                .collect::<Result<Vec<_>, _>>()?,
        )?;
        let definition = TelemetrySourceDefinition::new(
            identity.clone(),
            supported_fields.clone(),
            CheckpointCapability::None,
        );
        let health = SourceHealth::new(identity, supported_fields.iter().cloned(), now)?;
        Ok(Self { definition, health })
    }
}

impl TelemetrySource for OtlpLogSource {
    type Input = ExportLogsServiceRequest;

    fn definition(&self) -> &TelemetrySourceDefinition {
        &self.definition
    }

    fn health(&self) -> &SourceHealth {
        &self.health
    }

    fn extract(&self, input: Self::Input, observed_at: UtcTimestamp) -> SourceExtraction {
        extract_request(self.definition.identity(), input, observed_at)
    }

    fn mark_committed(&mut self, committed_at: UtcTimestamp) {
        self.health.mark_accepted(committed_at);
    }

    fn health_mut(&mut self) -> &mut SourceHealth {
        &mut self.health
    }
}

#[derive(Clone, Debug)]
enum RawScalar {
    String(String),
    Signed(i64),
    Unsigned(u64),
}

impl RawScalar {
    fn from_json(value: JsonValue) -> Option<Self> {
        match value {
            JsonValue::String(value) => Some(Self::String(value)),
            JsonValue::Number(value) => value
                .as_u64()
                .map(Self::Unsigned)
                .or_else(|| value.as_i64().map(Self::Signed)),
            _ => None,
        }
    }

    fn from_any(value: &AnyValue) -> Option<Self> {
        match value.value.as_ref()? {
            AnyValueKind::StringValue(value) => Some(Self::String(value.clone())),
            AnyValueKind::IntValue(value) => Some(Self::Signed(*value)),
            _ => None,
        }
    }

    fn as_string(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            Self::Signed(_) | Self::Unsigned(_) => None,
        }
    }

    fn token_count(&self) -> Option<TokenCount> {
        let value = match self {
            Self::Unsigned(value) => *value,
            Self::Signed(value) => u64::try_from(*value).ok()?,
            Self::String(value) => value.trim().parse::<u64>().ok()?,
        };
        TokenCount::new(value).ok()
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct JsonBody {
    #[serde(rename = "type")]
    event_kind: Option<JsonValue>,
    response: Option<JsonResponse>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct JsonResponse {
    id: Option<JsonValue>,
    model: Option<JsonValue>,
    conversation: Option<JsonValue>,
    usage: Option<JsonUsage>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct JsonUsage {
    input_tokens: Option<JsonValue>,
    cached_input_tokens: Option<JsonValue>,
    output_tokens: Option<JsonValue>,
    reasoning_output_tokens: Option<JsonValue>,
    input_tokens_details: Option<JsonInputDetails>,
    output_tokens_details: Option<JsonOutputDetails>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct JsonInputDetails {
    cached_tokens: Option<JsonValue>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct JsonOutputDetails {
    reasoning_tokens: Option<JsonValue>,
}

#[derive(Clone, Debug, Default)]
struct BodyDocument {
    event_kind: Option<RawScalar>,
    response_id: Option<RawScalar>,
    model: Option<RawScalar>,
    conversation: Option<RawScalar>,
    input_tokens: Option<RawScalar>,
    cached_input_tokens: Option<RawScalar>,
    output_tokens: Option<RawScalar>,
    reasoning_output_tokens: Option<RawScalar>,
}

impl BodyDocument {
    fn parse(body: Option<&AnyValue>) -> Self {
        match body.and_then(|body| body.value.as_ref()) {
            Some(AnyValueKind::StringValue(value)) if value.len() <= MAX_BODY_JSON_BYTES => {
                serde_json::from_str::<JsonBody>(value).map_or_else(
                    |_| Self::default(),
                    |body| {
                        let response = body.response.unwrap_or_default();
                        let usage = response.usage.unwrap_or_default();
                        let input_details = usage.input_tokens_details.unwrap_or_default();
                        let output_details = usage.output_tokens_details.unwrap_or_default();
                        Self {
                            event_kind: body.event_kind.and_then(RawScalar::from_json),
                            response_id: response.id.and_then(RawScalar::from_json),
                            model: response.model.and_then(RawScalar::from_json),
                            conversation: response.conversation.and_then(RawScalar::from_json),
                            input_tokens: usage.input_tokens.and_then(RawScalar::from_json),
                            cached_input_tokens: usage
                                .cached_input_tokens
                                .or(input_details.cached_tokens)
                                .and_then(RawScalar::from_json),
                            output_tokens: usage.output_tokens.and_then(RawScalar::from_json),
                            reasoning_output_tokens: usage
                                .reasoning_output_tokens
                                .or(output_details.reasoning_tokens)
                                .and_then(RawScalar::from_json),
                        }
                    },
                )
            }
            Some(AnyValueKind::KvlistValue(values)) => Self::from_key_values(values),
            _ => Self::default(),
        }
    }

    fn from_key_values(values: &KeyValueList) -> Self {
        let response = nested_key_values(&values.values, "response");
        let usage = response.and_then(|values| nested_key_values(values, "usage"));
        let input_details =
            usage.and_then(|values| nested_key_values(values, "input_tokens_details"));
        let output_details =
            usage.and_then(|values| nested_key_values(values, "output_tokens_details"));
        Self {
            event_kind: key_value(&values.values, "type").and_then(RawScalar::from_any),
            response_id: response
                .and_then(|values| key_value(values, "id"))
                .and_then(RawScalar::from_any),
            model: response
                .and_then(|values| key_value(values, "model"))
                .and_then(RawScalar::from_any),
            conversation: response
                .and_then(|values| key_value(values, "conversation"))
                .and_then(RawScalar::from_any),
            input_tokens: usage
                .and_then(|values| key_value(values, "input_tokens"))
                .and_then(RawScalar::from_any),
            cached_input_tokens: usage
                .and_then(|values| key_value(values, "cached_input_tokens"))
                .or_else(|| input_details.and_then(|values| key_value(values, "cached_tokens")))
                .and_then(RawScalar::from_any),
            output_tokens: usage
                .and_then(|values| key_value(values, "output_tokens"))
                .and_then(RawScalar::from_any),
            reasoning_output_tokens: usage
                .and_then(|values| key_value(values, "reasoning_output_tokens"))
                .or_else(|| output_details.and_then(|values| key_value(values, "reasoning_tokens")))
                .and_then(RawScalar::from_any),
        }
    }
}

fn key_value<'a>(values: &'a [KeyValue], key: &str) -> Option<&'a AnyValue> {
    values
        .iter()
        .rev()
        .find(|candidate| candidate.key == key)
        .and_then(|candidate| candidate.value.as_ref())
}

fn nested_key_values<'a>(values: &'a [KeyValue], key: &str) -> Option<&'a [KeyValue]> {
    match key_value(values, key)?.value.as_ref()? {
        AnyValueKind::KvlistValue(values) => Some(&values.values),
        _ => None,
    }
}

#[derive(Clone, Copy)]
struct AttributeLayers<'a> {
    record: &'a [KeyValue],
    scope: &'a [KeyValue],
    resource: &'a [KeyValue],
}

impl AttributeLayers<'_> {
    fn first(&self, keys: &[&str]) -> Option<&AnyValue> {
        for key in keys {
            for layer in [self.record, self.scope, self.resource] {
                if let Some(value) = key_value(layer, key) {
                    return Some(value);
                }
            }
        }
        None
    }

    fn validated_label<T>(
        &self,
        keys: &[&str],
        constructor: impl Fn(String) -> Result<T, ValidationError>,
    ) -> Option<T> {
        let value = any_string(self.first(keys)?)?;
        constructor(value.trim().to_owned()).ok()
    }

    fn token(&self, keys: &[&str]) -> Option<TokenCount> {
        any_token(self.first(keys)?)
    }
}

#[derive(Clone, Debug, Default)]
struct TypedFields {
    conversation_source_id: Option<ConversationSourceId>,
    thread_source_id: Option<ThreadSourceId>,
    model: Option<ModelName>,
    reasoning_effort: Option<ReasoningEffort>,
    input_tokens: Option<TokenCount>,
    cached_input_tokens: Option<TokenCount>,
    output_tokens: Option<TokenCount>,
    reasoning_output_tokens: Option<TokenCount>,
    surface: Option<SurfaceName>,
    client: Option<ClientName>,
    event_name: Option<EventName>,
    stream_event_kind: Option<StreamEventKind>,
}

impl TypedFields {
    fn has_tokens(&self) -> bool {
        self.input_tokens.is_some()
            || self.cached_input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.reasoning_output_tokens.is_some()
    }

    fn fingerprint_tokens(&self) -> FingerprintTokenFields {
        FingerprintTokenFields {
            input: self.input_tokens,
            cached_input: self.cached_input_tokens,
            output: self.output_tokens,
            reasoning_output: self.reasoning_output_tokens,
        }
    }

    fn into_observation_fields(self, provenance: &FieldProvenance) -> ObservationFields {
        fn direct<T>(value: Option<T>, provenance: &FieldProvenance) -> Option<Observed<T>> {
            value.map(|value| Observed::new(value, provenance.clone()))
        }

        ObservationFields {
            relationship: None,
            conversation_source_id: direct(self.conversation_source_id, provenance),
            thread_source_id: direct(self.thread_source_id, provenance),
            turn_source_id: None,
            model: direct(self.model, provenance),
            reasoning_effort: direct(self.reasoning_effort, provenance),
            input_tokens: direct(self.input_tokens, provenance),
            cached_input_tokens: direct(self.cached_input_tokens, provenance),
            output_tokens: direct(self.output_tokens, provenance),
            reasoning_output_tokens: direct(self.reasoning_output_tokens, provenance),
            surface: direct(self.surface, provenance),
            client: direct(self.client, provenance),
            event_name: direct(self.event_name, provenance),
            stream_event_kind: direct(self.stream_event_kind, provenance),
        }
    }
}

fn extract_request(
    source: &SourceIdentity,
    request: ExportLogsServiceRequest,
    observed_at: UtcTimestamp,
) -> SourceExtraction {
    let mut extraction = SourceExtraction::default();
    for resource_logs in request.resource_logs {
        let resource_attributes = resource_logs
            .resource
            .as_ref()
            .map_or(&[][..], |resource| resource.attributes.as_slice());
        for scope_logs in resource_logs.scope_logs {
            let scope_attributes = scope_logs
                .scope
                .as_ref()
                .map_or(&[][..], |scope| scope.attributes.as_slice());
            for record in scope_logs.log_records {
                extraction.records_seen += 1;
                let layers = AttributeLayers {
                    record: &record.attributes,
                    scope: scope_attributes,
                    resource: resource_attributes,
                };
                match extract_record(source, &record, layers, observed_at) {
                    RecordOutcome::Observation(observation) => {
                        extraction.observations.push(*observation);
                    }
                    RecordOutcome::Ignored => extraction.ignored_records += 1,
                    RecordOutcome::Rejected => extraction.rejected_records += 1,
                }
            }
        }
    }
    extraction
}

enum RecordOutcome {
    Observation(Box<SourceObservationCandidate>),
    Ignored,
    Rejected,
}

fn extract_record(
    source: &SourceIdentity,
    record: &LogRecord,
    attributes: AttributeLayers<'_>,
    observed_at: UtcTimestamp,
) -> RecordOutcome {
    let body = BodyDocument::parse(record.body.as_ref());
    let Some(event_name) = attributes.validated_label(EVENT_NAME_KEYS, EventName::new) else {
        return RecordOutcome::Ignored;
    };
    let registered_event = match event_name.as_str() {
        "codex.sse_event" => RegisteredEvent::Sse,
        "codex.conversation_starts" => RegisteredEvent::ConversationStarts,
        _ => return RecordOutcome::Ignored,
    };
    let event_time_nanos = if record.time_unix_nano > 0 {
        record.time_unix_nano
    } else {
        record.observed_time_unix_nano
    };
    if event_time_nanos == 0 {
        return RecordOutcome::Rejected;
    }
    let Ok(event_time) = UtcTimestamp::from_unix_timestamp_nanos(i128::from(event_time_nanos))
    else {
        return RecordOutcome::Rejected;
    };

    let stream_event_kind = attributes
        .validated_label(STREAM_KIND_KEYS, StreamEventKind::new)
        .or_else(|| {
            body.event_kind
                .as_ref()
                .and_then(RawScalar::as_string)
                .and_then(|value| StreamEventKind::new(value.trim()).ok())
        });

    let mut fields = extract_typed_fields(&attributes, &body, event_name, stream_event_kind);

    let kind = match registered_event {
        RegisteredEvent::Sse
            if fields
                .stream_event_kind
                .as_ref()
                .is_some_and(|kind| kind.as_str() == "response.completed")
                && fields.has_tokens() =>
        {
            SourceObservationKind::CompletedModelCall
        }
        RegisteredEvent::Sse
            if fields
                .stream_event_kind
                .as_ref()
                .is_some_and(|kind| kind.as_str() == "response.completed")
                && fields.conversation_source_id.is_some() =>
        {
            clear_token_fields(&mut fields);
            SourceObservationKind::ConversationMetadata
        }
        RegisteredEvent::ConversationStarts if fields.conversation_source_id.is_some() => {
            clear_token_fields(&mut fields);
            SourceObservationKind::ConversationMetadata
        }
        RegisteredEvent::Sse | RegisteredEvent::ConversationStarts => {
            return RecordOutcome::Ignored;
        }
    };

    let identity = source_event_identity(source, record, &attributes, &body, event_time, &fields);
    let provenance = FieldProvenance::new(
        source.clone(),
        observed_at,
        EvidenceQuality::Direct,
        SourcePriority::new(SOURCE_PRIORITY),
    );
    let event_time = Observed::new(event_time, provenance.clone());
    SourceObservationCandidate::new(
        kind,
        identity,
        event_time,
        observed_at,
        DataOrigin::Realtime,
        fields.into_observation_fields(&provenance),
    )
    .map_or(RecordOutcome::Rejected, |candidate| {
        RecordOutcome::Observation(Box::new(candidate))
    })
}

fn extract_typed_fields(
    attributes: &AttributeLayers<'_>,
    body: &BodyDocument,
    event_name: EventName,
    stream_event_kind: Option<StreamEventKind>,
) -> TypedFields {
    let conversation_source_id = attributes
        .validated_label(
            &[
                "conversation_id",
                "conversation.id",
                "codex.conversation_id",
            ],
            ConversationSourceId::new,
        )
        .or_else(|| {
            body.conversation
                .as_ref()
                .and_then(RawScalar::as_string)
                .and_then(|value| ConversationSourceId::new(value.trim()).ok())
        });
    let model = attributes
        .validated_label(
            &[
                "model",
                "model_name",
                "gen_ai.request.model",
                "gen_ai.response.model",
                "openai.model",
            ],
            ModelName::new,
        )
        .or_else(|| {
            body.model
                .as_ref()
                .and_then(RawScalar::as_string)
                .and_then(|value| ModelName::new(value.trim()).ok())
        });
    TypedFields {
        conversation_source_id,
        thread_source_id: attributes.validated_label(
            &[
                "thread_id",
                "thread.id",
                "codex.thread_id",
                "session_id",
                "session.id",
            ],
            ThreadSourceId::new,
        ),
        model,
        reasoning_effort: attributes
            .first(&[
                "reasoning_effort",
                "reasoning.effort",
                "openai.reasoning_effort",
                "model_reasoning_effort",
            ])
            .and_then(any_string)
            .and_then(|value| ReasoningEffort::from_str(&value.trim().to_ascii_lowercase()).ok()),
        input_tokens: attributes
            .token(&["input_token_count", "input_tokens"])
            .or_else(|| body.input_tokens.as_ref().and_then(RawScalar::token_count)),
        cached_input_tokens: attributes
            .token(&["cached_token_count", "cached_input_tokens"])
            .or_else(|| {
                body.cached_input_tokens
                    .as_ref()
                    .and_then(RawScalar::token_count)
            }),
        output_tokens: attributes
            .token(&["output_token_count", "output_tokens"])
            .or_else(|| body.output_tokens.as_ref().and_then(RawScalar::token_count)),
        reasoning_output_tokens: attributes
            .token(&["reasoning_token_count", "reasoning_output_tokens"])
            .or_else(|| {
                body.reasoning_output_tokens
                    .as_ref()
                    .and_then(RawScalar::token_count)
            }),
        surface: attributes.validated_label(
            &["surface", "codex.surface", "app.surface", "originator"],
            SurfaceName::new,
        ),
        client: attributes.validated_label(
            &["client", "client.name", "app", "app.name", "service.name"],
            ClientName::new,
        ),
        event_name: Some(event_name),
        stream_event_kind,
    }
}

#[derive(Clone, Copy)]
enum RegisteredEvent {
    Sse,
    ConversationStarts,
}

fn clear_token_fields(fields: &mut TypedFields) {
    fields.input_tokens = None;
    fields.cached_input_tokens = None;
    fields.output_tokens = None;
    fields.reasoning_output_tokens = None;
}

fn source_event_identity(
    source: &SourceIdentity,
    record: &LogRecord,
    attributes: &AttributeLayers<'_>,
    body: &BodyDocument,
    event_time: UtcTimestamp,
    fields: &TypedFields,
) -> SourceEventIdentity {
    let stable = attributes
        .validated_label(&["response.id"], StableSourceEventId::new)
        .or_else(|| trace_or_span_identity(record))
        .or_else(|| {
            body.response_id
                .as_ref()
                .and_then(RawScalar::as_string)
                .and_then(|value| StableSourceEventId::new(value.trim()).ok())
        });
    if let Some(stable) = stable {
        return SourceEventIdentity::from_stable_id(source.clone(), &stable);
    }
    SourceEventIdentity::from_fallback(
        source.clone(),
        FallbackFingerprintInput {
            event_time,
            conversation: fields.conversation_source_id.as_ref(),
            model: fields.model.as_ref(),
            tokens: fields.fingerprint_tokens(),
        },
    )
}

fn trace_or_span_identity(record: &LogRecord) -> Option<StableSourceEventId> {
    if record.trace_id.is_empty() && record.span_id.is_empty() {
        return None;
    }
    let mut value = String::new();
    if !record.trace_id.is_empty() {
        value.push_str("trace:");
        push_hex(&mut value, &record.trace_id);
    }
    if !record.span_id.is_empty() {
        if !value.is_empty() {
            value.push(':');
        }
        value.push_str("span:");
        push_hex(&mut value, &record.span_id);
    }
    StableSourceEventId::new(value).ok()
}

fn push_hex(target: &mut String, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    target.reserve(bytes.len() * 2);
    for byte in bytes {
        target.push(char::from(HEX[usize::from(byte >> 4)]));
        target.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
}

fn any_string(value: &AnyValue) -> Option<&str> {
    match value.value.as_ref()? {
        AnyValueKind::StringValue(value) => Some(value),
        _ => None,
    }
}

fn any_token(value: &AnyValue) -> Option<TokenCount> {
    RawScalar::from_any(value)?.token_count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_monitor_core::{IdentityKind, SafeErrorCode, SourceHealthStatus};
    use proptest::prelude::*;
    use prost::Message;

    fn at(seconds: i128) -> UtcTimestamp {
        UtcTimestamp::from_unix_timestamp_nanos(seconds * 1_000_000_000).unwrap()
    }

    fn request(fixture: &str) -> ExportLogsServiceRequest {
        serde_json::from_str(fixture).unwrap()
    }

    fn source() -> OtlpLogSource {
        OtlpLogSource::new(at(1)).unwrap()
    }

    #[test]
    fn source_contract_is_versioned_and_has_no_checkpoint() {
        let source = source();
        assert_eq!(source.definition().identity().id().as_str(), OTLP_SOURCE_ID);
        assert_eq!(source.definition().identity().contract_version().get(), 1);
        assert_eq!(source.definition().checkpoint(), CheckpointCapability::None);
        assert!(
            source
                .definition()
                .supported_fields()
                .contains(&FieldId::new("model_call.reasoning_output_tokens").unwrap())
        );
    }

    #[test]
    fn response_completed_extracts_only_typed_allowlisted_values() {
        let extraction = source().extract(
            request(include_str!(
                "../../../fixtures/otlp/response_completed.json"
            )),
            at(2),
        );
        assert_eq!(extraction.records_seen, 1);
        assert_eq!(extraction.rejected_records, 0);
        assert_eq!(extraction.observations.len(), 1);
        let observation = &extraction.observations[0];
        assert_eq!(
            observation.kind(),
            SourceObservationKind::CompletedModelCall
        );
        assert_eq!(
            observation
                .fields()
                .reasoning_output_tokens
                .as_ref()
                .unwrap()
                .value()
                .get(),
            516
        );
        assert_eq!(
            observation
                .fields()
                .model
                .as_ref()
                .unwrap()
                .value()
                .as_str(),
            "synthetic-model-v1"
        );
        assert_eq!(
            observation.source_event_identity().kind(),
            IdentityKind::Stable
        );
    }

    #[test]
    fn metadata_event_does_not_create_a_completed_call() {
        let extraction = source().extract(
            request(include_str!("../../../fixtures/otlp/metadata_only.json")),
            at(2),
        );
        assert_eq!(extraction.observations.len(), 1);
        assert_eq!(
            extraction.observations[0].kind(),
            SourceObservationKind::ConversationMetadata
        );
        assert!(extraction.observations[0].fields().input_tokens.is_none());
    }

    #[test]
    fn unknown_and_forbidden_fields_cannot_cross_the_typed_boundary() {
        let fixture = include_str!("../../../fixtures/otlp/unknown_fields.json");
        let extraction = source().extract(request(fixture), at(2));
        assert_eq!(extraction.observations.len(), 1);
        let debug = format!("{extraction:?}");
        for forbidden in [
            "synthetic-prompt-marker.example.invalid",
            "synthetic-response-marker.example.invalid",
            "synthetic-tool-arguments.example.invalid",
            "synthetic-tool-output.example.invalid",
            "synthetic-reasoning-summary.example.invalid",
            "synthetic-encrypted-reasoning.example.invalid",
            "synthetic-command.example.invalid",
            "synthetic-workspace-path.example.invalid",
            "synthetic-account-0001",
            "synthetic-user@example.invalid",
            "synthetic-api-key.example.invalid",
            "synthetic-cookie.example.invalid",
            "synthetic-auth-token.example.invalid",
            "synthetic-future-field.example.invalid",
        ] {
            assert!(fixture.contains(forbidden));
            assert!(!debug.contains(forbidden));
        }
    }

    #[test]
    fn protobuf_round_trip_preserves_the_same_source_identity() {
        let request = request(include_str!(
            "../../../fixtures/otlp/response_completed.json"
        ));
        let binary = request.encode_to_vec();
        let decoded = ExportLogsServiceRequest::decode(binary.as_slice()).unwrap();
        let first = source().extract(request, at(2));
        let second = source().extract(decoded, at(3));
        assert_eq!(
            first.observations[0].source_event_identity(),
            second.observations[0].source_event_identity()
        );
    }

    #[test]
    fn missing_required_event_time_is_rejected_without_invention() {
        let mut request = request(include_str!(
            "../../../fixtures/otlp/response_completed.json"
        ));
        let record = &mut request.resource_logs[0].scope_logs[0].log_records[0];
        record.time_unix_nano = 0;
        record.observed_time_unix_nano = 0;
        let extraction = source().extract(request, at(2));
        assert_eq!(extraction.rejected_records, 1);
        assert!(extraction.observations.is_empty());
    }

    #[test]
    fn source_health_changes_only_after_commit_or_safe_failure() {
        let mut source = source();
        let _ = source.extract(
            request(include_str!(
                "../../../fixtures/otlp/response_completed.json"
            )),
            at(2),
        );
        assert_eq!(source.health().status(), SourceHealthStatus::Starting);
        source.mark_committed(at(3));
        assert_eq!(source.health().status(), SourceHealthStatus::Healthy);
        source.mark_failure(SafeErrorCode::OtlpJsonDecodeFailed, at(4));
        assert_eq!(source.health().status(), SourceHealthStatus::Degraded);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn extractor_fuzz_discards_arbitrary_unknown_attributes(
            key_suffix in "[a-z0-9._-]{1,48}",
            marker_suffix in "[A-Za-z0-9._-]{1,128}",
        ) {
            let mut request = request(include_str!(
                "../../../fixtures/otlp/response_completed.json"
            ));
            let marker = format!("synthetic-fuzz-{marker_suffix}");
            request.resource_logs[0].scope_logs[0].log_records[0]
                .attributes
                .push(KeyValue {
                    key: format!("future.{key_suffix}"),
                    value: Some(AnyValue {
                        value: Some(AnyValueKind::StringValue(marker.clone())),
                    }),
                    ..KeyValue::default()
                });

            let extraction = source().extract(request, at(2));
            let classified = u64::try_from(extraction.observations.len()).unwrap()
                + extraction.rejected_records
                + extraction.ignored_records;
            prop_assert_eq!(classified, extraction.records_seen);
            let extraction_debug = format!("{extraction:?}");
            prop_assert!(!extraction_debug.contains(&marker));
        }
    }
}
