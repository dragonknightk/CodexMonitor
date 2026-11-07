use std::collections::HashSet;
use std::net::SocketAddr;

use axum::Json;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, RawQuery, State};
use axum::http::header::CACHE_CONTROL;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::api_error::{
    ApiErrorCode, CursorReason, RetryReason, ValidationField, ValidationIssue, api_error,
    invalid_cursor, invalid_time_range, source_not_found, source_retry_unavailable,
    storage_unavailable, validation_error,
};
use crate::assets::{serve_dashboard_fallback, serve_dashboard_index};
use crate::capabilities_http::{CapabilitiesResponse, capabilities};
use crate::cursor::{
    CursorBoundary, CursorEndpoint, CursorIssueError, CursorResolveError, CursorSort,
    NormalizedFilterDigest,
};
use crate::events_http::{get_health, stream_events};
use crate::ports::{
    DashboardAlertCursor, DashboardAlertPageRequest, DashboardConversationCursor,
    DashboardConversationPageRequest, DashboardModelCallCursor, DashboardModelCallPageRequest,
    DashboardQueryErrorCode, DashboardRetryErrorCode, DashboardSummaryRequest,
};
use crate::query_input::{
    AlertQueryInput, ModelCallQueryInput, ModelCallSortInput, ModelFiltersInput, QueryInputError,
    parse_alert_query, parse_conversation_query, parse_model_call_query, parse_required_source_id,
    parse_summary_query,
};
use crate::query_response::{
    AlertResponse, ConversationResponse, ModelCallResponse, ModelCallSummaryResponse, PageResponse,
    SourceListResponse,
};
use crate::routing::{DashboardApi, DashboardRouter};
use crate::session::{AuthenticatedSession, AuthorizedStateChange};
use crate::session_http::{bootstrap_session, current_session};
use crate::settings_http::{SettingsRequestBody, get_settings, replace_settings};
use crate::state::DashboardState;

const MAX_QUERY_STRING_BYTES: usize = 16 * 1024;
const MAX_QUERY_PAIRS: usize = 32;

const MODEL_CALL_FIELDS: &[(&str, ValidationField)] = &[
    ("cursor", ValidationField::Cursor),
    ("limit", ValidationField::Limit),
    ("conversation", ValidationField::Conversation),
    ("model", ValidationField::Model),
    ("source_id", ValidationField::SourceId),
    ("client", ValidationField::Client),
    ("from", ValidationField::From),
    ("to", ValidationField::To),
    ("alert", ValidationField::Alert),
    ("temporal_class", ValidationField::TemporalClass),
    ("sort", ValidationField::Sort),
];
const SUMMARY_FIELDS: &[(&str, ValidationField)] = &[
    ("conversation", ValidationField::Conversation),
    ("model", ValidationField::Model),
    ("source_id", ValidationField::SourceId),
    ("client", ValidationField::Client),
    ("from", ValidationField::From),
    ("to", ValidationField::To),
    ("alert", ValidationField::Alert),
    ("temporal_class", ValidationField::TemporalClass),
];
const CONVERSATION_FIELDS: &[(&str, ValidationField)] = &[
    ("cursor", ValidationField::Cursor),
    ("limit", ValidationField::Limit),
    ("query", ValidationField::Query),
    ("source_id", ValidationField::SourceId),
    ("from", ValidationField::From),
    ("to", ValidationField::To),
];
const ALERT_FIELDS: &[(&str, ValidationField)] = &[
    ("cursor", ValidationField::Cursor),
    ("limit", ValidationField::Limit),
    ("conversation", ValidationField::Conversation),
    ("model_call_id", ValidationField::ModelCallId),
    ("action_state", ValidationField::ActionState),
    ("from", ValidationField::From),
    ("to", ValidationField::To),
];

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EmptyQuery {}

/// 构建完整的 Dashboard API，并在返回前封闭安全边界。
///
/// # Panics
///
/// 当地址不是带非零端口的 IPv4 loopback 地址时 panic。
pub fn dashboard_router(local_address: SocketAddr, state: DashboardState) -> DashboardRouter {
    let state = state.with_listener_address(local_address);
    let stream_shutdown = state.stream_shutdown_sender();
    DashboardApi::<DashboardState>::new(local_address, state.sessions.clone())
        .bootstrap(bootstrap_session)
        .presentation_get("/", serve_dashboard_index)
        .presentation_fallback(serve_dashboard_fallback)
        .protected_get("/api/v1/session", current_session)
        .protected_get("/api/v1/health", get_health)
        .protected_get("/api/v1/capabilities", get_capabilities)
        .protected_get("/api/v1/model-calls", list_model_calls)
        .protected_get("/api/v1/conversations", list_conversations)
        .protected_get("/api/v1/summaries", model_call_summary)
        .protected_get("/api/v1/sources", list_sources)
        .protected_empty_post("/api/v1/sources/{source_id}/retry", retry_source)
        .protected_get("/api/v1/alerts", list_alerts)
        .protected_get("/api/v1/events", stream_events)
        .protected_get_json_put::<SettingsRequestBody, _, _, _, _>(
            "/api/v1/settings",
            get_settings,
            replace_settings,
        )
        .finish(state)
        .with_stream_shutdown(stream_shutdown)
}

async fn get_capabilities(State(state): State<DashboardState>) -> Response {
    run_query(move || execute_capabilities_query(&state)).await
}

async fn list_model_calls(
    State(state): State<DashboardState>,
    session: AuthenticatedSession,
    raw_query: RawQuery,
) -> Response {
    let raw = match decode_query(raw_query, MODEL_CALL_FIELDS) {
        Ok(raw) => raw,
        Err(error) => return error.into_response(),
    };
    let input = match parse_model_call_query(raw) {
        Ok(input) => input,
        Err(error) => return input_error(error),
    };
    run_query(move || execute_model_call_query(&state, &session, input)).await
}

async fn list_conversations(
    State(state): State<DashboardState>,
    session: AuthenticatedSession,
    raw_query: RawQuery,
) -> Response {
    let raw = match decode_query(raw_query, CONVERSATION_FIELDS) {
        Ok(raw) => raw,
        Err(error) => return error.into_response(),
    };
    let input = match parse_conversation_query(raw) {
        Ok(input) => input,
        Err(error) => return input_error(error),
    };
    run_query(move || execute_conversation_query(&state, &session, input)).await
}

async fn model_call_summary(State(state): State<DashboardState>, raw_query: RawQuery) -> Response {
    let raw = match decode_query(raw_query, SUMMARY_FIELDS) {
        Ok(raw) => raw,
        Err(error) => return error.into_response(),
    };
    let input = match parse_summary_query(raw) {
        Ok(input) => input,
        Err(error) => return input_error(error),
    };
    run_query(move || execute_summary_query(&state, input)).await
}

async fn list_sources(State(state): State<DashboardState>, raw_query: RawQuery) -> Response {
    if let Err(error) = decode_query::<EmptyQuery>(raw_query, &[]) {
        return error.into_response();
    }
    run_query(move || execute_source_query(&state)).await
}

async fn retry_source(
    State(state): State<DashboardState>,
    _: AuthorizedStateChange,
    path: Result<Path<String>, PathRejection>,
) -> Response {
    let source_id = match path {
        Ok(Path(source_id)) => match parse_required_source_id(source_id) {
            Ok(source_id) => source_id,
            Err(error) => return input_error(error),
        },
        Err(_) => {
            return validation_error(ValidationIssue::invalid_format(ValidationField::SourceId));
        }
    };
    match state.capabilities.retry_source(source_id) {
        Ok(source) => json_success_with_status(
            StatusCode::ACCEPTED,
            crate::query_response::source_health(source),
        ),
        Err(DashboardRetryErrorCode::InvalidState) => {
            source_retry_unavailable(RetryReason::InvalidState)
        }
        Err(DashboardRetryErrorCode::Unsupported) => {
            source_retry_unavailable(RetryReason::Unsupported)
        }
        Err(DashboardRetryErrorCode::NotFound) => source_not_found(),
        Err(DashboardRetryErrorCode::Unavailable) => storage_unavailable(),
    }
}

async fn list_alerts(
    State(state): State<DashboardState>,
    session: AuthenticatedSession,
    raw_query: RawQuery,
) -> Response {
    let raw = match decode_query(raw_query, ALERT_FIELDS) {
        Ok(raw) => raw,
        Err(error) => return error.into_response(),
    };
    let input = match parse_alert_query(raw) {
        Ok(input) => input,
        Err(error) => return input_error(error),
    };
    run_query(move || execute_alert_query(&state, &session, input)).await
}

fn execute_model_call_query(
    state: &DashboardState,
    session: &AuthenticatedSession,
    input: ModelCallQueryInput,
) -> Result<PageResponse<ModelCallResponse>, QueryExecutionError> {
    let resolved = resolve_name_filter(state, input.filters.conversation.as_deref())?;
    let filter_digest = input.filters.digest(&resolved);
    let cursor = resolve_model_cursor(
        state,
        session,
        input.cursor.as_deref(),
        input.sort,
        filter_digest,
    )?;
    let sort = input.sort;
    let page = state
        .queries
        .model_call_page(DashboardModelCallPageRequest {
            input,
            resolved_conversation_identifiers: resolved,
            cursor,
        })?;
    let next_cursor = issue_model_cursor(state, session, sort, filter_digest, page.next_cursor)?;
    crate::query_response::model_call_page(page, next_cursor)
        .map_err(|()| QueryExecutionError::Internal)
}

fn execute_capabilities_query(
    state: &DashboardState,
) -> Result<CapabilitiesResponse, QueryExecutionError> {
    capabilities(state).map_err(Into::into)
}

fn execute_conversation_query(
    state: &DashboardState,
    session: &AuthenticatedSession,
    input: crate::query_input::ConversationQueryInput,
) -> Result<PageResponse<ConversationResponse>, QueryExecutionError> {
    let resolved = resolve_name_filter(state, input.query.as_deref())?;
    let filter_digest = input.digest(&resolved);
    let cursor =
        resolve_conversation_cursor(state, session, input.cursor.as_deref(), filter_digest)?;
    let page = state
        .queries
        .conversation_page(DashboardConversationPageRequest {
            input,
            resolved_identifiers: resolved,
            cursor,
        })?;
    let next_cursor = issue_conversation_cursor(state, session, filter_digest, page.next_cursor)?;
    crate::query_response::conversation_page(page, next_cursor)
        .map_err(|()| QueryExecutionError::Internal)
}

fn execute_summary_query(
    state: &DashboardState,
    filters: ModelFiltersInput,
) -> Result<ModelCallSummaryResponse, QueryExecutionError> {
    let resolved = resolve_name_filter(state, filters.conversation.as_deref())?;
    let value = state.queries.model_call_summary(DashboardSummaryRequest {
        filters,
        resolved_conversation_identifiers: resolved,
    })?;
    Ok(crate::query_response::summary(value))
}

fn execute_source_query(state: &DashboardState) -> Result<SourceListResponse, QueryExecutionError> {
    Ok(crate::query_response::sources(
        state.queries.source_health_list()?,
    ))
}

fn execute_alert_query(
    state: &DashboardState,
    session: &AuthenticatedSession,
    input: AlertQueryInput,
) -> Result<PageResponse<AlertResponse>, QueryExecutionError> {
    let resolved = resolve_name_filter(state, input.conversation.as_deref())?;
    let filter_digest = input.digest(&resolved);
    let cursor = resolve_alert_cursor(state, session, input.cursor.as_deref(), filter_digest)?;
    let page = state.queries.alert_page(DashboardAlertPageRequest {
        input,
        resolved_conversation_identifiers: resolved,
        cursor,
    })?;
    let next_cursor = issue_alert_cursor(state, session, filter_digest, page.next_cursor)?;
    crate::query_response::alert_page(page, next_cursor).map_err(|()| QueryExecutionError::Internal)
}

fn resolve_name_filter(
    state: &DashboardState,
    raw: Option<&str>,
) -> Result<Vec<String>, QueryExecutionError> {
    raw.map_or_else(
        || Ok(Vec::new()),
        |value| {
            state
                .queries
                .resolve_conversation_identifiers(value)
                .map_err(Into::into)
        },
    )
}

fn resolve_model_cursor(
    state: &DashboardState,
    session: &AuthenticatedSession,
    encoded: Option<&str>,
    sort: ModelCallSortInput,
    filter_digest: NormalizedFilterDigest,
) -> Result<Option<DashboardModelCallCursor>, QueryExecutionError> {
    let Some(encoded) = encoded else {
        return Ok(None);
    };
    let boundary = state.cursors.resolve(
        encoded,
        session,
        CursorEndpoint::ModelCalls,
        sort.cursor_sort(),
        filter_digest,
    )?;
    match boundary {
        CursorBoundary::ModelCallEvent { event_at, id } => Ok(Some(DashboardModelCallCursor {
            sort,
            token_value: None,
            event_at,
            model_call_id: id,
        })),
        CursorBoundary::ModelCallTokens {
            tokens,
            event_at,
            id,
        } => Ok(Some(DashboardModelCallCursor {
            sort,
            token_value: tokens,
            event_at,
            model_call_id: id,
        })),
        CursorBoundary::Conversation { .. } | CursorBoundary::Alert { .. } => {
            Err(QueryExecutionError::Internal)
        }
    }
}

fn resolve_conversation_cursor(
    state: &DashboardState,
    session: &AuthenticatedSession,
    encoded: Option<&str>,
    filter_digest: NormalizedFilterDigest,
) -> Result<Option<DashboardConversationCursor>, QueryExecutionError> {
    let Some(encoded) = encoded else {
        return Ok(None);
    };
    match state.cursors.resolve(
        encoded,
        session,
        CursorEndpoint::Conversations,
        CursorSort::ConversationLastObservedAtDesc,
        filter_digest,
    )? {
        CursorBoundary::Conversation {
            last_observed_at,
            id,
        } => Ok(Some(DashboardConversationCursor {
            last_observed_at,
            conversation_id: id,
        })),
        CursorBoundary::ModelCallEvent { .. }
        | CursorBoundary::ModelCallTokens { .. }
        | CursorBoundary::Alert { .. } => Err(QueryExecutionError::Internal),
    }
}

fn resolve_alert_cursor(
    state: &DashboardState,
    session: &AuthenticatedSession,
    encoded: Option<&str>,
    filter_digest: NormalizedFilterDigest,
) -> Result<Option<DashboardAlertCursor>, QueryExecutionError> {
    let Some(encoded) = encoded else {
        return Ok(None);
    };
    match state.cursors.resolve(
        encoded,
        session,
        CursorEndpoint::Alerts,
        CursorSort::AlertCreatedAtDesc,
        filter_digest,
    )? {
        CursorBoundary::Alert { created_at, id } => Ok(Some(DashboardAlertCursor {
            created_at,
            alert_id: id,
        })),
        CursorBoundary::ModelCallEvent { .. }
        | CursorBoundary::ModelCallTokens { .. }
        | CursorBoundary::Conversation { .. } => Err(QueryExecutionError::Internal),
    }
}

fn issue_model_cursor(
    state: &DashboardState,
    session: &AuthenticatedSession,
    sort: ModelCallSortInput,
    filter_digest: NormalizedFilterDigest,
    cursor: Option<DashboardModelCallCursor>,
) -> Result<Option<String>, QueryExecutionError> {
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    let boundary = match sort {
        ModelCallSortInput::EventAtDesc | ModelCallSortInput::EventAtAsc => {
            CursorBoundary::ModelCallEvent {
                event_at: cursor.event_at,
                id: cursor.model_call_id,
            }
        }
        ModelCallSortInput::InputTokensDesc
        | ModelCallSortInput::OutputTokensDesc
        | ModelCallSortInput::ReasoningTokensDesc
        | ModelCallSortInput::TotalTokensDesc => CursorBoundary::ModelCallTokens {
            tokens: cursor.token_value,
            event_at: cursor.event_at,
            id: cursor.model_call_id,
        },
    };
    Ok(Some(state.cursors.issue(
        session,
        CursorEndpoint::ModelCalls,
        sort.cursor_sort(),
        filter_digest,
        boundary,
    )?))
}

fn issue_conversation_cursor(
    state: &DashboardState,
    session: &AuthenticatedSession,
    filter_digest: NormalizedFilterDigest,
    cursor: Option<DashboardConversationCursor>,
) -> Result<Option<String>, QueryExecutionError> {
    cursor
        .map(|cursor| {
            state.cursors.issue(
                session,
                CursorEndpoint::Conversations,
                CursorSort::ConversationLastObservedAtDesc,
                filter_digest,
                CursorBoundary::Conversation {
                    last_observed_at: cursor.last_observed_at,
                    id: cursor.conversation_id,
                },
            )
        })
        .transpose()
        .map_err(Into::into)
}

fn issue_alert_cursor(
    state: &DashboardState,
    session: &AuthenticatedSession,
    filter_digest: NormalizedFilterDigest,
    cursor: Option<DashboardAlertCursor>,
) -> Result<Option<String>, QueryExecutionError> {
    cursor
        .map(|cursor| {
            state.cursors.issue(
                session,
                CursorEndpoint::Alerts,
                CursorSort::AlertCreatedAtDesc,
                filter_digest,
                CursorBoundary::Alert {
                    created_at: cursor.created_at,
                    id: cursor.alert_id,
                },
            )
        })
        .transpose()
        .map_err(Into::into)
}

pub(crate) fn decode_query<T>(
    RawQuery(raw): RawQuery,
    allowed: &[(&str, ValidationField)],
) -> Result<T, QueryDecodeError>
where
    T: DeserializeOwned,
{
    let raw = raw.unwrap_or_default();
    if raw.len() > MAX_QUERY_STRING_BYTES {
        return Err(QueryDecodeError(ValidationIssue::out_of_range(
            ValidationField::Query,
            None,
            Some(i64::try_from(MAX_QUERY_STRING_BYTES).expect("query bound fits i64")),
        )));
    }
    let pairs = serde_urlencoded::from_str::<Vec<(String, String)>>(&raw)
        .map_err(|_| QueryDecodeError(ValidationIssue::invalid_format(ValidationField::Query)))?;
    if pairs.len() > MAX_QUERY_PAIRS {
        return Err(QueryDecodeError(ValidationIssue::out_of_range(
            ValidationField::Query,
            None,
            Some(i64::try_from(MAX_QUERY_PAIRS).expect("query pair bound fits i64")),
        )));
    }
    let mut seen = HashSet::with_capacity(pairs.len());
    for (name, _) in &pairs {
        let Some((_, field)) = allowed.iter().find(|(allowed, _)| name == allowed) else {
            return Err(QueryDecodeError(ValidationIssue::unsupported(
                ValidationField::Query,
            )));
        };
        if !seen.insert(name) {
            return Err(QueryDecodeError(ValidationIssue::inconsistent(*field)));
        }
    }
    serde_urlencoded::from_str(&raw)
        .map_err(|_| QueryDecodeError(ValidationIssue::invalid_format(ValidationField::Query)))
}

#[derive(Debug)]
pub(crate) struct QueryDecodeError(ValidationIssue);

impl QueryDecodeError {
    pub(crate) fn into_response(self) -> Response {
        validation_error(self.0)
    }
}

fn input_error(error: QueryInputError) -> Response {
    match error {
        QueryInputError::Validation(issue) => validation_error(issue),
        QueryInputError::InvalidTimeRange => invalid_time_range(),
    }
}

async fn run_query<T, F>(job: F) -> Response
where
    T: Serialize + Send + 'static,
    F: FnOnce() -> Result<T, QueryExecutionError> + Send + 'static,
{
    match tokio::task::spawn_blocking(job).await {
        Ok(Ok(value)) => json_success(value),
        Ok(Err(error)) => execution_error(error),
        Err(_) => api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            ApiErrorCode::InternalUnavailable,
            None,
        ),
    }
}

fn json_success<T: Serialize>(value: T) -> Response {
    json_success_with_status(StatusCode::OK, value)
}

fn json_success_with_status<T: Serialize>(status: StatusCode, value: T) -> Response {
    let mut response = (status, Json(value)).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QueryExecutionError {
    Query(DashboardQueryErrorCode),
    CursorResolve(CursorResolveError),
    CursorIssue,
    Internal,
}

impl From<DashboardQueryErrorCode> for QueryExecutionError {
    fn from(value: DashboardQueryErrorCode) -> Self {
        Self::Query(value)
    }
}

impl From<CursorResolveError> for QueryExecutionError {
    fn from(value: CursorResolveError) -> Self {
        Self::CursorResolve(value)
    }
}

impl From<CursorIssueError> for QueryExecutionError {
    fn from(_value: CursorIssueError) -> Self {
        Self::CursorIssue
    }
}

fn execution_error(error: QueryExecutionError) -> Response {
    match error {
        QueryExecutionError::Query(DashboardQueryErrorCode::Unavailable) => storage_unavailable(),
        QueryExecutionError::CursorResolve(CursorResolveError::Malformed) => {
            invalid_cursor(CursorReason::Malformed)
        }
        QueryExecutionError::CursorResolve(CursorResolveError::VersionUnsupported) => {
            invalid_cursor(CursorReason::VersionUnsupported)
        }
        QueryExecutionError::CursorResolve(CursorResolveError::EndpointMismatch) => {
            invalid_cursor(CursorReason::EndpointMismatch)
        }
        QueryExecutionError::CursorResolve(CursorResolveError::FilterMismatch) => {
            invalid_cursor(CursorReason::FilterMismatch)
        }
        QueryExecutionError::CursorResolve(CursorResolveError::SortMismatch) => {
            invalid_cursor(CursorReason::SortMismatch)
        }
        QueryExecutionError::Query(DashboardQueryErrorCode::InvalidRequest)
        | QueryExecutionError::CursorResolve(CursorResolveError::StateUnavailable)
        | QueryExecutionError::CursorIssue
        | QueryExecutionError::Internal => api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            ApiErrorCode::InternalUnavailable,
            None,
        ),
    }
}
