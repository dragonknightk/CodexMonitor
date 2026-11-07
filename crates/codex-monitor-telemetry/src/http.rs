use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::routing::post;
use codex_monitor_core::{SafeErrorCode, UtcTimestamp};
use opentelemetry_proto::tonic::collector::logs::v1::{
    ExportLogsPartialSuccess, ExportLogsServiceRequest, ExportLogsServiceResponse,
};
use prost::Message;

use crate::diagnostics::{
    OtlpDiagnosticEncoding, StructuredDiagnosticEvent, StructuredDiagnosticLog,
};

use super::{OtlpDecodeError, OtlpEncoding, OtlpRequestLimits, decode_otlp_request};

const PARTIAL_SUCCESS_MESSAGE: &str =
    "some log records were rejected by the registered source contract";
const GOOGLE_CODE_INVALID_ARGUMENT: i32 = 3;
const GOOGLE_CODE_UNAVAILABLE: i32 = 14;

/// Result returned only after the request processor has durably handled the
/// accepted records.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OtlpProcessOutcome {
    rejected_log_records: u64,
}

impl OtlpProcessOutcome {
    #[must_use]
    pub const fn new(rejected_log_records: u64) -> Self {
        Self {
            rejected_log_records,
        }
    }

    #[must_use]
    pub const fn rejected_log_records(self) -> u64 {
        self.rejected_log_records
    }
}

/// Closed processor failure used to select permanent versus retryable OTLP
/// status without carrying an internal error chain into HTTP.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OtlpProcessError {
    code: SafeErrorCode,
    retryable: bool,
}

impl OtlpProcessError {
    #[must_use]
    pub const fn permanent(code: SafeErrorCode) -> Self {
        Self {
            code,
            retryable: false,
        }
    }

    #[must_use]
    pub const fn retryable(code: SafeErrorCode) -> Self {
        Self {
            code,
            retryable: true,
        }
    }

    #[must_use]
    pub const fn code(self) -> SafeErrorCode {
        self.code
    }

    #[must_use]
    pub const fn is_retryable(self) -> bool {
        self.retryable
    }
}

/// Async boundary between OTLP protocol handling and commit-before-ack domain
/// processing.
pub trait OtlpRequestProcessor: Send + Sync + 'static {
    /// Process and durably commit an upstream OTLP request.
    ///
    /// A successful future is the handler's sole authority to return HTTP 200.
    fn process<'a>(
        &'a self,
        request: ExportLogsServiceRequest,
        received_at: UtcTimestamp,
    ) -> Pin<Box<dyn Future<Output = Result<OtlpProcessOutcome, OtlpProcessError>> + Send + 'a>>;

    /// Record a privacy-safe protocol failure in source health. Processors
    /// without durable health state may keep the default no-op implementation.
    fn report_failure<'a>(
        &'a self,
        _code: SafeErrorCode,
        _occurred_at: UtcTimestamp,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }
}

#[derive(Clone)]
struct OtlpHttpState {
    diagnostics: StructuredDiagnosticLog,
    limits: OtlpRequestLimits,
    processor: Arc<dyn OtlpRequestProcessor>,
}

/// Build the isolated OTLP router. It exposes only `POST /v1/logs`; listener
/// binding and health routing are composed by the receiver runtime.
pub fn otlp_http_router(
    limits: OtlpRequestLimits,
    processor: Arc<dyn OtlpRequestProcessor>,
    diagnostics: StructuredDiagnosticLog,
) -> Router {
    let state = Arc::new(OtlpHttpState {
        diagnostics,
        limits,
        processor,
    });
    Router::new()
        .route("/v1/logs", post(receive_logs))
        .with_state(state)
}

async fn receive_logs(State(state): State<Arc<OtlpHttpState>>, request: Request) -> Response {
    let timeout_encoding = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| OtlpEncoding::parse_content_type(Some(value)).ok())
        .unwrap_or(OtlpEncoding::Json);
    let timeout_state = Arc::clone(&state);
    if let Ok(response) = tokio::time::timeout(
        state.limits.request_timeout(),
        receive_logs_within_deadline(state, request),
    )
    .await
    {
        return response;
    }
    let _ = timeout_state
        .diagnostics
        .record(StructuredDiagnosticEvent::OtlpRequestTimedOut {
            occurred_at: UtcTimestamp::now(),
            error_code: SafeErrorCode::SourceUnavailable,
        });
    error_response(timeout_encoding, SafeErrorCode::SourceUnavailable, true)
}

async fn receive_logs_within_deadline(state: Arc<OtlpHttpState>, request: Request) -> Response {
    let received_at = UtcTimestamp::now();
    let (parts, body) = request.into_parts();
    let content_type = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    let encoding = match OtlpEncoding::parse_content_type(content_type) {
        Ok(encoding) => encoding,
        Err(error) => {
            record_decode_failure(&state, error, None, None, received_at);
            state
                .processor
                .report_failure(error.safe_code(), received_at)
                .await;
            return decode_error_response(OtlpEncoding::Json, error);
        }
    };
    let content_encoding = parts
        .headers
        .get(header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok());

    let Ok(body) = to_bytes(body, state.limits.max_compressed_bytes()).await else {
        let error = OtlpDecodeError::CompressedBodyTooLarge;
        record_decode_failure(&state, error, Some(encoding), None, received_at);
        state
            .processor
            .report_failure(error.safe_code(), received_at)
            .await;
        return decode_error_response(encoding, error);
    };
    let body = body.to_vec();
    let wire_bytes = u64::try_from(body.len()).unwrap_or(u64::MAX);
    let decoded =
        match decode_otlp_request(body, content_type, content_encoding, state.limits).await {
            Ok(decoded) => decoded,
            Err(error) => {
                record_decode_failure(&state, error, Some(encoding), Some(wire_bytes), received_at);
                state
                    .processor
                    .report_failure(error.safe_code(), received_at)
                    .await;
                return decode_error_response(encoding, error);
            }
        };
    let outcome = match state.processor.process(decoded.request, received_at).await {
        Ok(outcome) => outcome,
        Err(error) => {
            let _ = state
                .diagnostics
                .record(StructuredDiagnosticEvent::OtlpProcessingFailed {
                    occurred_at: received_at,
                    error_code: error.code(),
                    encoding: diagnostic_encoding(decoded.encoding),
                    retryable: error.is_retryable(),
                });
            return process_error_response(decoded.encoding, error);
        }
    };
    success_response(decoded.encoding, outcome)
}

fn record_decode_failure(
    state: &OtlpHttpState,
    error: OtlpDecodeError,
    encoding: Option<OtlpEncoding>,
    wire_bytes: Option<u64>,
    occurred_at: UtcTimestamp,
) {
    let _ = state
        .diagnostics
        .record(StructuredDiagnosticEvent::OtlpDecodeFailed {
            occurred_at,
            error_code: error.safe_code(),
            encoding: encoding.map(diagnostic_encoding),
            wire_bytes,
        });
}

const fn diagnostic_encoding(encoding: OtlpEncoding) -> OtlpDiagnosticEncoding {
    match encoding {
        OtlpEncoding::Json => OtlpDiagnosticEncoding::Json,
        OtlpEncoding::Protobuf => OtlpDiagnosticEncoding::Protobuf,
    }
}

fn success_response(encoding: OtlpEncoding, outcome: OtlpProcessOutcome) -> Response {
    let rejected_log_records = i64::try_from(outcome.rejected_log_records).unwrap_or(i64::MAX);
    let partial_success = (rejected_log_records > 0).then(|| ExportLogsPartialSuccess {
        rejected_log_records,
        error_message: PARTIAL_SUCCESS_MESSAGE.to_owned(),
    });
    let response = ExportLogsServiceResponse { partial_success };
    let body = match encoding {
        OtlpEncoding::Protobuf => response.encode_to_vec(),
        OtlpEncoding::Json => encode_json_success(&response),
    };
    encoded_response(StatusCode::OK, encoding, body)
}

fn encode_json_success(response: &ExportLogsServiceResponse) -> Vec<u8> {
    let Some(partial) = response.partial_success.as_ref() else {
        return b"{}".to_vec();
    };
    serde_json::to_vec(&serde_json::json!({
        "partialSuccess": {
            "rejectedLogRecords": partial.rejected_log_records.to_string(),
            "errorMessage": partial.error_message,
        }
    }))
    .expect("the fixed OTLP success response is serializable")
}

fn decode_error_response(encoding: OtlpEncoding, error: OtlpDecodeError) -> Response {
    error_response(encoding, error.safe_code(), error.retryable())
}

fn process_error_response(encoding: OtlpEncoding, error: OtlpProcessError) -> Response {
    error_response(encoding, error.code(), error.is_retryable())
}

fn error_response(encoding: OtlpEncoding, code: SafeErrorCode, retryable: bool) -> Response {
    let status = if retryable {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::BAD_REQUEST
    };
    let google_code = if retryable {
        GOOGLE_CODE_UNAVAILABLE
    } else {
        GOOGLE_CODE_INVALID_ARGUMENT
    };
    let message = code.as_str();
    let body = match encoding {
        OtlpEncoding::Protobuf => GoogleRpcStatus {
            code: google_code,
            message: message.to_owned(),
        }
        .encode_to_vec(),
        OtlpEncoding::Json => serde_json::to_vec(&serde_json::json!({
            "code": google_code,
            "message": message,
        }))
        .expect("the fixed google.rpc.Status response is serializable"),
    };
    encoded_response(status, encoding, body)
}

fn encoded_response(status: StatusCode, encoding: OtlpEncoding, body: Vec<u8>) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(encoding.content_type()),
    );
    response
}

/// Minimal wire-compatible `google.rpc.Status`. OTLP/HTTP does not require
/// `details`, so tag 3 is intentionally absent.
#[derive(Clone, PartialEq, Message)]
struct GoogleRpcStatus {
    #[prost(int32, tag = "1")]
    code: i32,
    #[prost(string, tag = "2")]
    message: String,
}

#[cfg(test)]
mod tests {
    use std::future::pending;
    use std::sync::Weak;
    use std::time::Duration;

    use axum::body::to_bytes;
    use axum::http::{Request, header};
    use serde_json::Value;
    use tower::ServiceExt;

    use super::*;

    const FIXTURE: &str = include_str!("../../../fixtures/otlp/response_completed.json");

    #[derive(Clone, Copy)]
    struct FixedProcessor(Result<OtlpProcessOutcome, OtlpProcessError>);

    impl OtlpRequestProcessor for FixedProcessor {
        fn process<'a>(
            &'a self,
            _request: ExportLogsServiceRequest,
            _received_at: UtcTimestamp,
        ) -> Pin<Box<dyn Future<Output = Result<OtlpProcessOutcome, OtlpProcessError>> + Send + 'a>>
        {
            Box::pin(async move { self.0 })
        }
    }

    struct PendingProcessor;

    impl OtlpRequestProcessor for PendingProcessor {
        fn process<'a>(
            &'a self,
            _request: ExportLogsServiceRequest,
            _received_at: UtcTimestamp,
        ) -> Pin<Box<dyn Future<Output = Result<OtlpProcessOutcome, OtlpProcessError>> + Send + 'a>>
        {
            Box::pin(pending())
        }
    }

    fn router(result: Result<OtlpProcessOutcome, OtlpProcessError>) -> Router {
        otlp_http_router(
            OtlpRequestLimits::default(),
            Arc::new(FixedProcessor(result)),
            StructuredDiagnosticLog::memory(),
        )
    }

    async fn response_bytes(response: Response) -> Vec<u8> {
        to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .to_vec()
    }

    fn diagnostic_lines(log: &StructuredDiagnosticLog) -> Vec<Value> {
        String::from_utf8(log.memory_contents())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn json_success_and_partial_success_use_otlp_json_mapping() {
        let request = Request::post("/v1/logs")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(FIXTURE))
            .unwrap();
        let response = router(Ok(OtlpProcessOutcome::new(0)))
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        assert_eq!(response_bytes(response).await, b"{}");

        let request = Request::post("/v1/logs")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(FIXTURE))
            .unwrap();
        let response = router(Ok(OtlpProcessOutcome::new(2)))
            .oneshot(request)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&response_bytes(response).await).unwrap();
        assert_eq!(body["partialSuccess"]["rejectedLogRecords"], "2");
        assert_eq!(
            body["partialSuccess"]["errorMessage"],
            PARTIAL_SUCCESS_MESSAGE
        );
    }

    #[tokio::test]
    async fn protobuf_success_is_an_upstream_export_response() {
        let decoded: ExportLogsServiceRequest = serde_json::from_str(FIXTURE).unwrap();
        let request = Request::post("/v1/logs")
            .header(header::CONTENT_TYPE, "application/x-protobuf")
            .body(Body::from(decoded.encode_to_vec()))
            .unwrap();
        let response = router(Ok(OtlpProcessOutcome::new(0)))
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/x-protobuf"
        );
        let response =
            ExportLogsServiceResponse::decode(response_bytes(response).await.as_slice()).unwrap();
        assert!(response.partial_success.is_none());
    }

    #[tokio::test]
    async fn malformed_body_returns_encoding_matched_google_status() {
        let request = Request::post("/v1/logs")
            .header(header::CONTENT_TYPE, "application/x-protobuf")
            .body(Body::from(vec![0xff, 0xff]))
            .unwrap();
        let response = router(Ok(OtlpProcessOutcome::default()))
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/x-protobuf"
        );
        let status = GoogleRpcStatus::decode(response_bytes(response).await.as_slice()).unwrap();
        assert_eq!(status.code, GOOGLE_CODE_INVALID_ARGUMENT);
        assert_eq!(status.message, "otlp_protobuf_decode_failed");
    }

    #[tokio::test]
    async fn decoder_failure_jsonl_contains_only_safe_bounded_metadata() {
        const PAYLOAD_MARKER: &str = "synthetic-malformed-payload-secret.example.invalid";
        let body = format!(r#"{{"payload":"{PAYLOAD_MARKER}""#);
        let diagnostics = StructuredDiagnosticLog::memory();
        let response = otlp_http_router(
            OtlpRequestLimits::default(),
            Arc::new(FixedProcessor(Ok(OtlpProcessOutcome::default()))),
            diagnostics.clone(),
        )
        .oneshot(
            Request::post("/v1/logs")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let lines = diagnostic_lines(&diagnostics);
        assert_eq!(lines.len(), 1);
        let event = lines[0].as_object().unwrap();
        assert_eq!(event["event_code"], "otlp_decode_failed");
        assert_eq!(event["error_code"], "otlp_json_decode_failed");
        assert_eq!(event["encoding"], "json");
        assert_eq!(event["wire_bytes"], body.len());
        assert_eq!(
            event
                .keys()
                .map(String::as_str)
                .collect::<std::collections::HashSet<_>>(),
            std::collections::HashSet::from([
                "event_code",
                "occurred_at",
                "error_code",
                "encoding",
                "wire_bytes",
            ])
        );
        let log = String::from_utf8(diagnostics.memory_contents()).unwrap();
        assert!(!log.contains(PAYLOAD_MARKER));
        assert!(!log.contains(&body));
    }

    #[tokio::test]
    async fn processing_failure_jsonl_omits_payload_and_source_identifiers() {
        let diagnostics = StructuredDiagnosticLog::memory();
        let response = otlp_http_router(
            OtlpRequestLimits::default(),
            Arc::new(FixedProcessor(Err(OtlpProcessError::retryable(
                SafeErrorCode::PersistenceCommitFailed,
            )))),
            diagnostics.clone(),
        )
        .oneshot(
            Request::post("/v1/logs")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(FIXTURE))
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let lines = diagnostic_lines(&diagnostics);
        assert_eq!(lines.len(), 1);
        let event = lines[0].as_object().unwrap();
        assert_eq!(event["event_code"], "otlp_processing_failed");
        assert_eq!(event["error_code"], "persistence_commit_failed");
        assert_eq!(event["encoding"], "json");
        assert_eq!(event["retryable"], true);
        assert_eq!(
            event
                .keys()
                .map(String::as_str)
                .collect::<std::collections::HashSet<_>>(),
            std::collections::HashSet::from([
                "event_code",
                "occurred_at",
                "error_code",
                "encoding",
                "retryable",
            ])
        );
        let log = String::from_utf8(diagnostics.memory_contents()).unwrap();
        for raw_identifier in [
            "synthetic-response-0001",
            "synthetic-conversation-0001",
            "synthetic-thread-0001",
        ] {
            assert!(!log.contains(raw_identifier));
        }
        assert!(!log.contains("resourceLogs"));
    }

    #[tokio::test]
    async fn retryable_processor_failure_is_not_acknowledged() {
        let request = Request::post("/v1/logs")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(FIXTURE))
            .unwrap();
        let response = router(Err(OtlpProcessError::retryable(
            SafeErrorCode::PersistenceCommitFailed,
        )))
        .oneshot(request)
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body: Value = serde_json::from_slice(&response_bytes(response).await).unwrap();
        assert_eq!(body["code"], GOOGLE_CODE_UNAVAILABLE);
        assert_eq!(body["message"], "persistence_commit_failed");
    }

    #[tokio::test]
    async fn missing_or_unsupported_content_type_is_permanent_json_status() {
        for content_type in [None, Some("text/plain")] {
            let mut builder = Request::post("/v1/logs");
            if let Some(content_type) = content_type {
                builder = builder.header(header::CONTENT_TYPE, content_type);
            }
            let response = router(Ok(OtlpProcessOutcome::default()))
                .oneshot(builder.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
            let body: Value = serde_json::from_slice(&response_bytes(response).await).unwrap();
            assert_eq!(body["message"], "otlp_invalid_content_type");
        }
    }

    #[tokio::test]
    async fn receiver_rejects_the_wire_body_before_decode_when_limit_is_exceeded() {
        let limits = OtlpRequestLimits::new(16, 32, std::time::Duration::from_secs(1)).unwrap();
        let router = otlp_http_router(
            limits,
            Arc::new(FixedProcessor(Ok(OtlpProcessOutcome::default()))),
            StructuredDiagnosticLog::memory(),
        );
        let response = router
            .oneshot(
                Request::post("/v1/logs")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(vec![b'x'; 17]))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: Value = serde_json::from_slice(&response_bytes(response).await).unwrap();
        assert_eq!(body["message"], "otlp_compressed_body_too_large");
    }

    #[tokio::test]
    async fn full_request_deadline_cancels_a_pending_processor_and_releases_it() {
        let limits =
            OtlpRequestLimits::new(1024 * 1024, 1024 * 1024, Duration::from_millis(50)).unwrap();
        let processor = Arc::new(PendingProcessor);
        let weak: Weak<PendingProcessor> = Arc::downgrade(&processor);
        let router = otlp_http_router(limits, processor.clone(), StructuredDiagnosticLog::memory());
        drop(processor);

        let response = router
            .oneshot(
                Request::post("/v1/logs")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(FIXTURE))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body: Value = serde_json::from_slice(&response_bytes(response).await).unwrap();
        assert_eq!(body["message"], "source_unavailable");
        assert!(weak.upgrade().is_none());
    }

    #[tokio::test]
    async fn request_deadline_error_matches_json_and_protobuf_request_encoding() {
        let protobuf: ExportLogsServiceRequest = serde_json::from_str(FIXTURE).unwrap();
        let cases = [
            (
                OtlpEncoding::Json,
                FIXTURE.as_bytes().to_vec(),
                "application/json",
            ),
            (
                OtlpEncoding::Protobuf,
                protobuf.encode_to_vec(),
                "application/x-protobuf",
            ),
        ];

        for (encoding, body, content_type) in cases {
            let limits =
                OtlpRequestLimits::new(1024 * 1024, 1024 * 1024, Duration::from_millis(25))
                    .unwrap();
            let response = otlp_http_router(
                limits,
                Arc::new(PendingProcessor),
                StructuredDiagnosticLog::memory(),
            )
            .oneshot(
                Request::post("/v1/logs")
                    .header(header::CONTENT_TYPE, content_type)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(response.headers()[header::CONTENT_TYPE], content_type);
            let body = response_bytes(response).await;
            match encoding {
                OtlpEncoding::Json => {
                    let status: Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(status["code"], GOOGLE_CODE_UNAVAILABLE);
                    assert_eq!(status["message"], "source_unavailable");
                }
                OtlpEncoding::Protobuf => {
                    let status = GoogleRpcStatus::decode(body.as_slice()).unwrap();
                    assert_eq!(status.code, GOOGLE_CODE_UNAVAILABLE);
                    assert_eq!(status.message, "source_unavailable");
                }
            }
        }
    }
}
