use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::future::Future;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use codex_monitor_core::{ObservationFields, SourceHealth, UtcTimestamp};
use flate2::read::GzDecoder;
use opentelemetry_proto::tonic::collector::logs::v1::{
    ExportLogsServiceRequest, ExportLogsServiceResponse,
};
use prost::Message;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use tower::ServiceExt as _;

use crate::diagnostics::StructuredDiagnosticLog;

use super::{
    ObservationSink, ObservationSinkError, OtlpLogSource, OtlpObservationProcessor,
    OtlpRequestLimits, SourceExtraction, SourceObservationKind, TelemetrySource,
    decode_otlp_request, otlp_http_router,
};

const CONTRACT_BYTES: &[u8] = include_bytes!("../../../contracts/legacy-otlp-v1.json");
const DISPOSITION_TEXT: &str = include_str!("../../../contracts/legacy-otlp-v1-disposition.json");
const AUTHORITY_TEST: &str = "legacy_contract::frozen_authorities_are_complete_and_self_consistent";
const EXECUTION_TEST: &str = "legacy_contract::all_frozen_cases_execute_through_native_ingestion";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DispositionAuthority {
    schema_version: u64,
    authority_id: String,
    classification_scope: ClassificationScope,
    baseline: BaselineBinding,
    runtime_evidence: RuntimeEvidence,
    difference_catalog: BTreeMap<String, DifferenceDefinition>,
    cases: Vec<CaseDisposition>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClassificationScope {
    preserved: String,
    intentional_change: String,
    shared_cutover_changes: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BaselineBinding {
    path: String,
    contract_id: String,
    case_count: usize,
    sha256: String,
    fragment_sha256: FragmentHashes,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FragmentHashes {
    case_ids: String,
    fixture_paths: String,
    privacy_forbidden_storage_markers: String,
    sqlite_allowlist: String,
    projections: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeEvidence {
    authority_test: String,
    execution_test: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DifferenceDefinition {
    legacy: String,
    current: String,
    rust_evidence: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Disposition {
    Preserved,
    IntentionalChange,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum DecodeExpectation {
    Accepted,
    Rejected,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ResponseBodyKind {
    OtlpJsonExportResponse,
    OtlpProtobufExportResponse,
    GoogleRpcStatusJson,
    GoogleRpcStatusProtobuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaseDisposition {
    id: String,
    disposition: Disposition,
    frozen_request_sha256: String,
    legacy_projection: String,
    difference_codes: Vec<String>,
    current_expectation: CurrentExpectation,
    rust_test_evidence: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CurrentExpectation {
    decode: DecodeExpectation,
    safe_error_code: Option<String>,
    source_observations: usize,
    http: HttpExpectation,
    native_projection: NativeProjection,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpExpectation {
    status: u16,
    content_type: String,
    content_encoding: Option<String>,
    body_kind: ResponseBodyKind,
    google_code: Option<i32>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeProjection {
    model_calls: usize,
    conversations: usize,
    domain_work: usize,
    conversation_source_id: Option<String>,
    thread_source_id: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
struct GoogleRpcStatus {
    #[prost(int32, tag = "1")]
    code: i32,
    #[prost(string, tag = "2")]
    message: String,
}
#[derive(Default)]
struct FrozenSink {
    commits: Mutex<Vec<(SourceExtraction, SourceHealth)>>,
    health: Mutex<Vec<SourceHealth>>,
}

impl ObservationSink for FrozenSink {
    fn commit<'a>(
        &'a self,
        extraction: SourceExtraction,
        accepted_health: SourceHealth,
    ) -> Pin<Box<dyn Future<Output = Result<(), ObservationSinkError>> + Send + 'a>> {
        Box::pin(async move {
            self.commits
                .lock()
                .unwrap()
                .push((extraction, accepted_health));
            Ok(())
        })
    }

    fn store_health<'a>(
        &'a self,
        health: SourceHealth,
    ) -> Pin<Box<dyn Future<Output = Result<(), ObservationSinkError>> + Send + 'a>> {
        Box::pin(async move {
            self.health.lock().unwrap().push(health);
            Ok(())
        })
    }
}

#[test]
fn frozen_authorities_are_complete_and_self_consistent() {
    let (contract, authority) = authorities();
    let contract_cases = cases(&contract);
    assert_authority_identity(&contract, &authority, contract_cases);
    assert_frozen_fragment_bindings(&contract, &authority, contract_cases);
    assert_difference_catalog(&authority);
    assert_case_coverage(contract_cases, &authority);

    let dispositions = authority
        .cases
        .iter()
        .map(|case| (case.id.as_str(), case))
        .collect::<BTreeMap<_, _>>();
    let mut preserved = 0;
    let mut intentional_changes = 0;
    let mut embedded_markers = BTreeSet::new();
    for frozen in contract_cases {
        let id = frozen["id"].as_str().unwrap();
        let disposition = dispositions[id];
        match assert_case_disposition_binding(&contract, &authority, frozen, disposition) {
            Disposition::Preserved => preserved += 1,
            Disposition::IntentionalChange => intentional_changes += 1,
        }
        embedded_markers.extend(assert_frozen_request_materialization(frozen, disposition));
    }
    assert_eq!((preserved, intentional_changes), (3, 8));
    assert_forbidden_marker_coverage(&contract, &embedded_markers);
}

fn assert_authority_identity(
    contract: &Value,
    authority: &DispositionAuthority,
    contract_cases: &[Value],
) {
    assert_eq!(authority.schema_version, 1);
    assert_eq!(
        authority.authority_id,
        "codex-monitor-rust-legacy-otlp-cutover-v1"
    );
    assert!(
        [
            &authority.classification_scope.preserved,
            &authority.classification_scope.intentional_change,
            &authority.classification_scope.shared_cutover_changes,
        ]
        .into_iter()
        .all(|value| !value.trim().is_empty())
    );
    assert_eq!(authority.baseline.path, "contracts/legacy-otlp-v1.json");
    assert_eq!(
        authority.baseline.contract_id,
        contract["contract_id"].as_str().unwrap()
    );
    assert_eq!(sha256_hex(CONTRACT_BYTES), authority.baseline.sha256);
    assert_eq!(authority.runtime_evidence.authority_test, AUTHORITY_TEST);
    assert_eq!(authority.runtime_evidence.execution_test, EXECUTION_TEST);
    assert_eq!(contract_cases.len(), 11);
    assert_eq!(contract_cases.len(), authority.baseline.case_count);
    assert_eq!(authority.cases.len(), authority.baseline.case_count);
}

fn assert_frozen_fragment_bindings(
    contract: &Value,
    authority: &DispositionAuthority,
    contract_cases: &[Value],
) {
    let case_ids = Value::Array(
        contract_cases
            .iter()
            .map(|case| case["id"].clone())
            .collect(),
    );
    let fixture_paths = Value::Array(
        contract_cases
            .iter()
            .map(|case| case["request"]["source_fixture"].clone())
            .collect(),
    );
    assert_eq!(
        json_sha256(&case_ids),
        authority.baseline.fragment_sha256.case_ids
    );
    assert_eq!(
        json_sha256(&fixture_paths),
        authority.baseline.fragment_sha256.fixture_paths
    );
    assert_eq!(
        json_sha256(&contract["privacy"]["forbidden_storage_markers"]),
        authority
            .baseline
            .fragment_sha256
            .privacy_forbidden_storage_markers
    );
    assert_eq!(
        json_sha256(&contract["sqlite_allowlist"]),
        authority.baseline.fragment_sha256.sqlite_allowlist
    );
    assert_eq!(
        json_sha256(&contract["projections"]),
        authority.baseline.fragment_sha256.projections
    );
}

fn assert_difference_catalog(authority: &DispositionAuthority) {
    let expected_difference_codes = BTreeSet::from([
        "encoding_matched_google_rpc_status",
        "normalized_conversation_projection",
        "standard_otlp_json_success_response",
    ]);
    assert_eq!(
        authority
            .difference_catalog
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        expected_difference_codes
    );
    for definition in authority.difference_catalog.values() {
        assert!(!definition.legacy.trim().is_empty());
        assert!(!definition.current.trim().is_empty());
        assert_eq!(definition.rust_evidence, EXECUTION_TEST);
    }
}

fn assert_case_coverage(contract_cases: &[Value], authority: &DispositionAuthority) {
    let frozen_ids = contract_cases
        .iter()
        .map(|case| case["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    let disposition_ids = authority
        .cases
        .iter()
        .map(|case| case.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        disposition_ids, frozen_ids,
        "case order and coverage drifted"
    );
    assert_eq!(
        disposition_ids.iter().collect::<BTreeSet<_>>().len(),
        authority.baseline.case_count,
        "duplicate disposition case ID"
    );
}

fn assert_case_disposition_binding(
    contract: &Value,
    authority: &DispositionAuthority,
    frozen: &Value,
    disposition: &CaseDisposition,
) -> Disposition {
    let id = frozen["id"].as_str().unwrap();
    let expected_evidence = [AUTHORITY_TEST, EXECUTION_TEST];
    assert_eq!(
        disposition.frozen_request_sha256,
        frozen["request"]["body_sha256"].as_str().unwrap(),
        "{id}: frozen request SHA binding drifted"
    );
    assert_eq!(
        disposition.legacy_projection,
        frozen["expected"]["projection"].as_str().unwrap(),
        "{id}: legacy projection binding drifted"
    );
    assert!(
        contract["projections"]
            .get(&disposition.legacy_projection)
            .is_some(),
        "{id}: missing legacy projection"
    );
    assert_eq!(
        disposition
            .rust_test_evidence
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        expected_evidence,
        "{id}: runtime evidence is not bound to both native tests"
    );
    for difference_code in &disposition.difference_codes {
        assert!(
            authority.difference_catalog.contains_key(difference_code),
            "{id}: unknown difference code {difference_code}"
        );
    }
    match disposition.disposition {
        Disposition::Preserved => {
            assert!(
                disposition.difference_codes.is_empty(),
                "{id}: preserved case declares a behavior difference"
            );
        }
        Disposition::IntentionalChange => {
            assert!(
                !disposition.difference_codes.is_empty(),
                "{id}: intentional change lacks a difference code"
            );
        }
    }
    disposition.disposition
}

fn assert_frozen_request_materialization(
    frozen: &Value,
    disposition: &CaseDisposition,
) -> BTreeSet<String> {
    let id = frozen["id"].as_str().unwrap();
    let fixture = frozen["request"]["source_fixture"].as_str().unwrap();
    assert!(fixture.starts_with("fixtures/otlp/"));
    assert!(!fixture.split('/').any(|component| component == ".."));
    assert!(
        repository_root().join(fixture).is_file(),
        "missing {fixture}"
    );

    let body = build_case_body(frozen);
    assert_eq!(
        sha256_hex(&body),
        disposition.frozen_request_sha256,
        "{id}: native materializer changed the frozen request"
    );
    if frozen["request"]["body_transform"] == "gzip" {
        assert_eq!(&body[4..8], &[0, 0, 0, 0], "{id}: gzip mtime drifted");
    }
    let marker_body = uncompressed_body(frozen, &body);
    let mut embedded_markers = BTreeSet::new();
    for marker in frozen["request"]["embedded_forbidden_markers"]
        .as_array()
        .unwrap()
    {
        let marker = marker.as_str().unwrap();
        assert!(
            marker_body
                .windows(marker.len())
                .any(|window| window == marker.as_bytes()),
            "{id}: request no longer embeds {marker}"
        );
        embedded_markers.insert(marker.to_owned());
    }
    embedded_markers
}

fn assert_forbidden_marker_coverage(contract: &Value, embedded_markers: &BTreeSet<String>) {
    let forbidden_markers = contract["privacy"]["forbidden_storage_markers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|marker| marker.as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(embedded_markers, &forbidden_markers);
    assert!(
        forbidden_markers
            .iter()
            .all(|marker| marker.starts_with("synthetic-"))
    );
}

#[tokio::test]
async fn all_frozen_cases_execute_through_native_ingestion() {
    let (contract, authority) = authorities();
    let dispositions = authority
        .cases
        .iter()
        .map(|case| (case.id.as_str(), case))
        .collect::<BTreeMap<_, _>>();
    let forbidden_markers = contract["privacy"]["forbidden_storage_markers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|marker| marker.as_str().unwrap())
        .collect::<Vec<_>>();

    for frozen in cases(&contract) {
        let id = frozen["id"].as_str().unwrap();
        let disposition = dispositions[id];
        let request_spec = &frozen["request"];
        let body = build_case_body(frozen);
        let content_type = request_spec["content_type"].as_str().unwrap();
        let content_encoding = request_spec["content_encoding"].as_str();

        let decoded = decode_otlp_request(
            body.clone(),
            Some(content_type),
            content_encoding,
            OtlpRequestLimits::default(),
        )
        .await;
        match disposition.current_expectation.decode {
            DecodeExpectation::Accepted => {
                let decoded = decoded.unwrap_or_else(|error| {
                    panic!("{id}: frozen valid request failed native decode: {error}")
                });
                let source = OtlpLogSource::new(at(1)).unwrap();
                let extraction = source.extract(decoded.request, at(2));
                assert_eq!(
                    extraction.observations.len(),
                    disposition.current_expectation.source_observations,
                    "{id}: typed observation count drifted"
                );
                assert_eq!(extraction.records_seen, 1, "{id}: record count drifted");
                assert_eq!(extraction.rejected_records, 0, "{id}: record rejected");
                assert_eq!(extraction.ignored_records, 0, "{id}: record ignored");
                assert_extraction_matches_legacy_projection(
                    id,
                    &extraction,
                    &contract["projections"][&disposition.legacy_projection],
                );
                let debug = format!("{extraction:?}");
                for marker in &forbidden_markers {
                    assert!(
                        !debug.contains(marker),
                        "{id}: typed extraction retained forbidden marker {marker}"
                    );
                }
                assert!(disposition.current_expectation.safe_error_code.is_none());
            }
            DecodeExpectation::Rejected => {
                let error = decoded.expect_err("frozen malformed request decoded successfully");
                assert_eq!(
                    error.safe_code().as_str(),
                    disposition
                        .current_expectation
                        .safe_error_code
                        .as_deref()
                        .unwrap(),
                    "{id}: closed decoder error drifted"
                );
                assert_eq!(disposition.current_expectation.source_observations, 0);
            }
        }

        execute_case_through_http_and_sink(
            id,
            frozen,
            disposition,
            &contract["projections"][&disposition.legacy_projection],
            &forbidden_markers,
            body,
        )
        .await;
    }
}

async fn execute_case_through_http_and_sink(
    id: &str,
    frozen: &Value,
    disposition: &CaseDisposition,
    legacy_projection: &Value,
    forbidden_markers: &[&str],
    body: Vec<u8>,
) {
    let diagnostics = StructuredDiagnosticLog::memory();
    let sink = Arc::new(FrozenSink::default());
    let processor = Arc::new(OtlpObservationProcessor::new(
        OtlpLogSource::new(at(1)).unwrap(),
        Arc::clone(&sink) as Arc<dyn ObservationSink>,
    ));
    let router = otlp_http_router(OtlpRequestLimits::default(), processor, diagnostics.clone());
    let request_spec = &frozen["request"];
    let mut request = Request::post("/v1/logs").header(
        header::CONTENT_TYPE,
        request_spec["content_type"].as_str().unwrap(),
    );
    if let Some(content_encoding) = request_spec["content_encoding"].as_str() {
        request = request.header(header::CONTENT_ENCODING, content_encoding);
    }
    let response = router
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    assert_http_response(
        id,
        response,
        &disposition.current_expectation.http,
        forbidden_markers,
    )
    .await;
    assert_sink_projection(
        id,
        &sink,
        matches!(
            disposition.current_expectation.decode,
            DecodeExpectation::Accepted
        ),
        &disposition.current_expectation.native_projection,
        legacy_projection,
    );
    assert_closed_privacy_surfaces(id, &diagnostics, &sink, forbidden_markers);
}

async fn assert_http_response(
    id: &str,
    response: axum::response::Response,
    expected_http: &HttpExpectation,
    forbidden_markers: &[&str],
) {
    assert_eq!(
        response.status(),
        StatusCode::from_u16(expected_http.status).unwrap(),
        "{id}: HTTP status drifted"
    );
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        expected_http.content_type,
        "{id}: response media type drifted"
    );
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_ENCODING)
            .and_then(|value| value.to_str().ok()),
        expected_http.content_encoding.as_deref(),
        "{id}: response content encoding drifted"
    );
    let response_body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap()
        .to_vec();
    assert_response_body(id, expected_http, &response_body);
    for marker in forbidden_markers {
        assert!(
            !response_body
                .windows(marker.len())
                .any(|window| window == marker.as_bytes()),
            "{id}: response leaked forbidden marker {marker}"
        );
    }
}

fn assert_sink_projection(
    id: &str,
    sink: &FrozenSink,
    accepted: bool,
    expected_projection: &NativeProjection,
    legacy_projection: &Value,
) {
    let commits = sink.commits.lock().unwrap();
    if !accepted {
        assert!(
            commits.is_empty(),
            "{id}: rejected request reached the typed sink"
        );
        return;
    }

    assert_eq!(
        commits.len(),
        1,
        "{id}: accepted request commit count drifted"
    );
    let extraction = &commits[0].0;
    assert_extraction_matches_legacy_projection(id, extraction, legacy_projection);

    let model_calls = extraction
        .observations
        .iter()
        .filter(|candidate| candidate.kind() == SourceObservationKind::CompletedModelCall)
        .count();
    let conversation_source_id = extraction.observations.iter().find_map(|candidate| {
        candidate
            .fields()
            .conversation_source_id
            .as_ref()
            .map(|field| field.value().as_str())
    });
    let thread_source_id = extraction.observations.iter().find_map(|candidate| {
        candidate
            .fields()
            .thread_source_id
            .as_ref()
            .map(|field| field.value().as_str())
    });
    let conversations = usize::from(conversation_source_id.is_some());

    assert_eq!(
        model_calls, expected_projection.model_calls,
        "{id}: typed model-call projection drifted"
    );
    assert_eq!(
        conversations, expected_projection.conversations,
        "{id}: typed conversation projection drifted"
    );
    assert_eq!(
        model_calls, expected_projection.domain_work,
        "{id}: typed downstream-work projection drifted"
    );
    assert_eq!(
        conversation_source_id,
        expected_projection.conversation_source_id.as_deref(),
        "{id}: typed conversation source ID drifted"
    );
    assert_eq!(
        thread_source_id,
        expected_projection.thread_source_id.as_deref(),
        "{id}: typed thread source ID drifted"
    );
}

fn assert_closed_privacy_surfaces(
    id: &str,
    diagnostics: &StructuredDiagnosticLog,
    sink: &FrozenSink,
    forbidden_markers: &[&str],
) {
    let diagnostic_text = String::from_utf8(diagnostics.memory_contents()).unwrap();
    let sink_debug = format!("{:?}", sink.commits.lock().unwrap());
    for marker in forbidden_markers {
        assert!(
            !diagnostic_text.contains(marker),
            "{id}: structured diagnostics retained forbidden marker {marker}"
        );
        assert!(
            !sink_debug.contains(marker),
            "{id}: typed sink retained forbidden marker {marker}"
        );
    }
}

fn assert_extraction_matches_legacy_projection(
    id: &str,
    extraction: &SourceExtraction,
    projection: &Value,
) {
    let token_events = projection["token_events"].as_array().unwrap();
    let conversations = projection["conversations"].as_array().unwrap();
    assert_eq!(
        extraction.observations.len(),
        token_events.len() + conversations.len(),
        "{id}: frozen semantic projection no longer maps one-to-one"
    );
    if let Some(row) = token_events.first() {
        let candidate = &extraction.observations[0];
        assert_eq!(candidate.kind(), SourceObservationKind::CompletedModelCall);
        assert_eq!(
            candidate.event_time().value(),
            &UtcTimestamp::parse_rfc3339(row["created_at"].as_str().unwrap()).unwrap()
        );
        assert_fields_match_legacy_row(id, candidate.fields(), row);
    }
    if let Some(row) = conversations.first() {
        let candidate = &extraction.observations[0];
        assert_eq!(
            candidate.kind(),
            SourceObservationKind::ConversationMetadata
        );
        assert_eq!(
            candidate.event_time().value(),
            &UtcTimestamp::parse_rfc3339(row["first_seen_at"].as_str().unwrap()).unwrap()
        );
        assert_fields_match_legacy_row(id, candidate.fields(), row);
        assert!(candidate.fields().input_tokens.is_none());
        assert!(candidate.fields().cached_input_tokens.is_none());
        assert!(candidate.fields().output_tokens.is_none());
        assert!(candidate.fields().reasoning_output_tokens.is_none());
    }
}

fn assert_fields_match_legacy_row(id: &str, fields: &ObservationFields, row: &Value) {
    assert_eq!(
        fields
            .conversation_source_id
            .as_ref()
            .map(|field| field.value().as_str()),
        row["conversation_id"].as_str(),
        "{id}: conversation ID drifted"
    );
    assert_eq!(
        fields
            .thread_source_id
            .as_ref()
            .map(|field| field.value().as_str()),
        row.get("thread_id").and_then(Value::as_str),
        "{id}: thread ID drifted"
    );
    assert_eq!(
        fields.model.as_ref().map(|field| field.value().as_str()),
        row.get("model").and_then(Value::as_str),
        "{id}: model drifted"
    );
    assert_eq!(
        fields
            .reasoning_effort
            .as_ref()
            .map(|field| field.value().as_str()),
        row.get("reasoning_effort").and_then(Value::as_str),
        "{id}: reasoning effort drifted"
    );
    assert_eq!(
        fields.surface.as_ref().map(|field| field.value().as_str()),
        row.get("surface").and_then(Value::as_str),
        "{id}: surface drifted"
    );
    assert_eq!(
        fields.client.as_ref().map(|field| field.value().as_str()),
        row.get("client").and_then(Value::as_str),
        "{id}: client drifted"
    );
    assert_eq!(
        fields
            .event_name
            .as_ref()
            .map(|field| field.value().as_str()),
        row.get("raw_event_name").and_then(Value::as_str),
        "{id}: event name drifted"
    );
    assert_eq!(
        fields
            .stream_event_kind
            .as_ref()
            .map(|field| field.value().as_str()),
        row.get("stream_event_kind").and_then(Value::as_str),
        "{id}: stream event kind drifted"
    );
    assert_token_field(id, "input_tokens", fields.input_tokens.as_ref(), row);
    assert_token_field(
        id,
        "cached_input_tokens",
        fields.cached_input_tokens.as_ref(),
        row,
    );
    assert_token_field(id, "output_tokens", fields.output_tokens.as_ref(), row);
    assert_token_field(
        id,
        "reasoning_output_tokens",
        fields.reasoning_output_tokens.as_ref(),
        row,
    );
}

fn assert_token_field(
    id: &str,
    key: &str,
    field: Option<&codex_monitor_core::Observed<codex_monitor_core::TokenCount>>,
    row: &Value,
) {
    assert_eq!(
        field.map(|field| field.value().get()),
        row.get(key).and_then(Value::as_u64),
        "{id}: {key} drifted"
    );
}

fn assert_response_body(id: &str, expected: &HttpExpectation, body: &[u8]) {
    match expected.body_kind {
        ResponseBodyKind::OtlpJsonExportResponse => {
            let value: Value = serde_json::from_slice(body).unwrap();
            assert_eq!(value, serde_json::json!({}), "{id}: JSON success drifted");
            assert!(expected.google_code.is_none());
            assert!(expected.message.is_none());
        }
        ResponseBodyKind::OtlpProtobufExportResponse => {
            let response = ExportLogsServiceResponse::decode(body).unwrap();
            assert!(response.partial_success.is_none(), "{id}: partial success");
            assert!(expected.google_code.is_none());
            assert!(expected.message.is_none());
        }
        ResponseBodyKind::GoogleRpcStatusJson => {
            let value: Value = serde_json::from_slice(body).unwrap();
            assert_eq!(value["code"].as_i64(), expected.google_code.map(i64::from));
            assert_eq!(value["message"].as_str(), expected.message.as_deref());
        }
        ResponseBodyKind::GoogleRpcStatusProtobuf => {
            let status = GoogleRpcStatus::decode(body).unwrap();
            assert_eq!(Some(status.code), expected.google_code);
            assert_eq!(Some(status.message.as_str()), expected.message.as_deref());
        }
    }
}

fn authorities() -> (Value, DispositionAuthority) {
    (
        serde_json::from_slice(CONTRACT_BYTES).unwrap(),
        serde_json::from_str(DISPOSITION_TEXT).unwrap(),
    )
}

fn cases(contract: &Value) -> &[Value] {
    contract["cases"].as_array().unwrap()
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn build_case_body(case: &Value) -> Vec<u8> {
    let request = &case["request"];
    let path = repository_root().join(request["source_fixture"].as_str().unwrap());
    let payload = match request["body_builder"].as_str().unwrap() {
        "otlp_json" => canonical_json_bytes(&path),
        "otlp_protobuf" => protobuf_bytes(&path),
        "otlp_protobuf_unknown_wire" => {
            let mut payload = protobuf_bytes(&path);
            let field_number = request["unknown_wire_field_number"].as_u64().unwrap();
            let value = request["unknown_wire_value"].as_str().unwrap().as_bytes();
            encode_varint((field_number << 3) | 2, &mut payload);
            encode_varint(u64::try_from(value.len()).unwrap(), &mut payload);
            payload.extend_from_slice(value);
            payload
        }
        "raw_text" => normalized_text(&path).into_bytes(),
        "hex" => decode_hex(&normalized_text(&path)),
        builder => panic!("unsupported frozen body builder {builder}"),
    };
    match request["body_transform"].as_str().unwrap() {
        "identity" => payload,
        "gzip" => deterministic_gzip(&payload),
        transform => panic!("unsupported frozen body transform {transform}"),
    }
}

fn canonical_json_bytes(path: &Path) -> Vec<u8> {
    let value: Value = serde_json::from_str(&normalized_text(path)).unwrap();
    assert!(value.is_object());
    serde_json::to_vec(&value).unwrap()
}

fn protobuf_bytes(path: &Path) -> Vec<u8> {
    let request: ExportLogsServiceRequest = serde_json::from_str(&normalized_text(path)).unwrap();
    request.encode_to_vec()
}

fn normalized_text(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap()
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}

fn decode_hex(value: &str) -> Vec<u8> {
    let compact = value.split_whitespace().collect::<String>();
    assert_eq!(compact.len() % 2, 0);
    compact
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn encode_varint(mut value: u64, target: &mut Vec<u8>) {
    while value > 0x7f {
        target.push(u8::try_from(value & 0x7f).unwrap() | 0x80);
        value >>= 7;
    }
    target.push(u8::try_from(value).unwrap());
}

fn deterministic_gzip(payload: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(payload.len() + 32);
    output.extend_from_slice(&[0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff]);
    let chunks = payload.chunks(0xffff).collect::<Vec<_>>();
    if chunks.is_empty() {
        output.push(1);
        output.extend_from_slice(&0_u16.to_le_bytes());
        output.extend_from_slice(&u16::MAX.to_le_bytes());
    } else {
        for (index, chunk) in chunks.iter().enumerate() {
            output.push(u8::from(index + 1 == chunks.len()));
            let length = u16::try_from(chunk.len()).unwrap();
            output.extend_from_slice(&length.to_le_bytes());
            output.extend_from_slice(&(u16::MAX - length).to_le_bytes());
            output.extend_from_slice(chunk);
        }
    }
    output.extend_from_slice(&crc32(payload).to_le_bytes());
    output.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_le_bytes());
    output
}

fn crc32(payload: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in payload {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (0_u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

fn uncompressed_body(case: &Value, body: &[u8]) -> Vec<u8> {
    if case["request"]["body_transform"] != "gzip" {
        return body.to_vec();
    }
    let mut decoded = Vec::new();
    GzDecoder::new(body).read_to_end(&mut decoded).unwrap();
    decoded
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn json_sha256(value: &Value) -> String {
    sha256_hex(&serde_json::to_vec(value).unwrap())
}

fn at(seconds: i128) -> UtcTimestamp {
    UtcTimestamp::from_unix_timestamp_nanos(seconds * 1_000_000_000).unwrap()
}
