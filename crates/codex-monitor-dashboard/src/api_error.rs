use axum::Json;
use axum::http::header::{CACHE_CONTROL, WWW_AUTHENTICATE};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

pub(super) const REFERRER_POLICY: HeaderName = HeaderName::from_static("referrer-policy");
const MIN_SAFE_VALIDATION_BOUND: i64 = -1_000_000_000_000_000;
const MAX_SAFE_VALIDATION_BOUND: i64 = 1_000_000_000_000_000;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ApiErrorCode {
    BootstrapInvalid,
    Unauthorized,
    ForbiddenHost,
    ForbiddenOrigin,
    CsrfInvalid,
    ValidationFailed,
    InvalidCursor,
    InvalidTimeRange,
    SourceNotFound,
    SourceRetryUnavailable,
    RevisionConflict,
    PreconditionRequired,
    UnsupportedMediaType,
    PayloadTooLarge,
    StorageUnavailable,
    InternalUnavailable,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CursorReason {
    Malformed,
    EndpointMismatch,
    FilterMismatch,
    SortMismatch,
    VersionUnsupported,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RetryReason {
    Unsupported,
    InvalidState,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationField {
    Token,
    Cursor,
    Limit,
    Conversation,
    Query,
    Model,
    SourceId,
    Client,
    From,
    To,
    Alert,
    TemporalClass,
    Sort,
    ModelCallId,
    ActionState,
    AfterEventId,
    Csrf,
    IfMatch,
    RequestBody,
    LoginAutostart,
    SystemNotifications,
    NotifyMaxN,
    RetentionDays,
    Language,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum ValidationReason {
    Required,
    InvalidFormat,
    OutOfRange,
    Unsupported,
    Inconsistent,
}

#[derive(Debug, Serialize)]
pub struct ValidationIssue {
    field: ValidationField,
    reason: ValidationReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    minimum: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    maximum: Option<i64>,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: ErrorBody,
}

#[derive(Serialize)]
struct ErrorBody {
    code: ApiErrorCode,
    params: ErrorParameters,
}

#[derive(Default, Serialize)]
struct ErrorParameters {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    issues: Vec<ValidationIssue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor_reason: Option<CursorReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_reason: Option<RetryReason>,
}

pub(super) fn bootstrap_error(
    status: StatusCode,
    code: ApiErrorCode,
    challenge: Option<&'static str>,
) -> Response {
    with_no_referrer(api_error(status, code, challenge))
}

pub(super) fn api_error(
    status: StatusCode,
    code: ApiErrorCode,
    challenge: Option<&'static str>,
) -> Response {
    api_error_with_parameters(status, code, challenge, ErrorParameters::default())
}

pub(super) fn validation_error(issue: ValidationIssue) -> Response {
    validation_errors(vec![issue])
}

pub(super) fn validation_errors(mut issues: Vec<ValidationIssue>) -> Response {
    if issues.is_empty() {
        issues.push(ValidationIssue::invalid_format(
            ValidationField::RequestBody,
        ));
    }
    issues.truncate(32);
    for issue in &mut issues {
        issue.minimum = issue
            .minimum
            .map(|value| value.clamp(MIN_SAFE_VALIDATION_BOUND, MAX_SAFE_VALIDATION_BOUND));
        issue.maximum = issue
            .maximum
            .map(|value| value.clamp(MIN_SAFE_VALIDATION_BOUND, MAX_SAFE_VALIDATION_BOUND));
    }
    api_error_with_parameters(
        StatusCode::BAD_REQUEST,
        ApiErrorCode::ValidationFailed,
        None,
        ErrorParameters {
            issues,
            cursor_reason: None,
            retry_reason: None,
        },
    )
}

pub(super) fn invalid_cursor(reason: CursorReason) -> Response {
    api_error_with_parameters(
        StatusCode::BAD_REQUEST,
        ApiErrorCode::InvalidCursor,
        None,
        ErrorParameters {
            issues: Vec::new(),
            cursor_reason: Some(reason),
            retry_reason: None,
        },
    )
}

pub(super) fn source_not_found() -> Response {
    api_error(StatusCode::NOT_FOUND, ApiErrorCode::SourceNotFound, None)
}

pub(super) fn source_retry_unavailable(reason: RetryReason) -> Response {
    api_error_with_parameters(
        StatusCode::CONFLICT,
        ApiErrorCode::SourceRetryUnavailable,
        None,
        ErrorParameters {
            issues: Vec::new(),
            cursor_reason: None,
            retry_reason: Some(reason),
        },
    )
}

pub(super) fn invalid_time_range() -> Response {
    api_error(
        StatusCode::BAD_REQUEST,
        ApiErrorCode::InvalidTimeRange,
        None,
    )
}

pub(super) fn storage_unavailable() -> Response {
    api_error(
        StatusCode::SERVICE_UNAVAILABLE,
        ApiErrorCode::StorageUnavailable,
        None,
    )
}

pub(super) fn with_no_referrer(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    response
}

fn api_error_with_parameters(
    status: StatusCode,
    code: ApiErrorCode,
    challenge: Option<&'static str>,
    params: ErrorParameters,
) -> Response {
    let mut response = (
        status,
        Json(ErrorResponse {
            error: ErrorBody { code, params },
        }),
    )
        .into_response();
    let headers = response.headers_mut();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Some(challenge) = challenge {
        headers.insert(WWW_AUTHENTICATE, HeaderValue::from_static(challenge));
    }
    response
}

impl ValidationIssue {
    #[must_use]
    pub const fn required(field: ValidationField) -> Self {
        Self {
            field,
            reason: ValidationReason::Required,
            minimum: None,
            maximum: None,
        }
    }

    #[must_use]
    pub const fn invalid_format(field: ValidationField) -> Self {
        Self {
            field,
            reason: ValidationReason::InvalidFormat,
            minimum: None,
            maximum: None,
        }
    }

    #[must_use]
    pub const fn out_of_range(
        field: ValidationField,
        minimum: Option<i64>,
        maximum: Option<i64>,
    ) -> Self {
        Self {
            field,
            reason: ValidationReason::OutOfRange,
            minimum,
            maximum,
        }
    }

    #[must_use]
    pub const fn unsupported(field: ValidationField) -> Self {
        Self {
            field,
            reason: ValidationReason::Unsupported,
            minimum: None,
            maximum: None,
        }
    }

    #[must_use]
    pub const fn inconsistent(field: ValidationField) -> Self {
        Self {
            field,
            reason: ValidationReason::Inconsistent,
            minimum: None,
            maximum: None,
        }
    }
}
