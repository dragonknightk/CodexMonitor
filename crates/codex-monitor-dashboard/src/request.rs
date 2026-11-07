use std::collections::HashSet;
use std::error::Error as _;
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::extract::{FromRequest, Request};
use axum::http::header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use http_body_util::LengthLimitError;
use mime::{APPLICATION, JSON, Mime};
use serde::Deserialize;
use serde::de::{self, DeserializeOwned, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

use super::api_error::{
    ApiErrorCode, ValidationField, ValidationIssue, api_error, validation_error, validation_errors,
};

pub const MAX_DASHBOARD_JSON_BODY_BYTES: usize = 16 * 1_024;

pub(super) mod sealed {
    pub trait Sealed {}
}

/// Explicit opt-in for closed Dashboard request DTOs.
///
/// Implementations must use typed fields and `#[serde(deny_unknown_fields)]`
/// on every object type. Semantic range and cross-field checks belong in
/// [`DashboardRequestBody::validate`].
pub trait DashboardRequestBody:
    sealed::Sealed + Clone + DeserializeOwned + Send + Sync + 'static
{
    /// Validate semantics that cannot be represented by deserialization alone.
    ///
    /// Returning more than 32 issues is safe; the API boundary truncates them to
    /// the `OpenAPI` maximum. Raw values and arbitrary property names must never be
    /// placed in an issue.
    ///
    /// # Errors
    ///
    /// Returns privacy-safe validation issues when the complete document is not
    /// semantically valid.
    fn validate(&self) -> Result<(), Vec<ValidationIssue>> {
        Ok(())
    }
}

/// A bounded, duplicate-key-rejecting JSON request body deserialized into a
/// closed API DTO.
///
/// DTOs express their `OpenAPI` object schema through typed fields, bounded value
/// types and `#[serde(deny_unknown_fields)]`. Rejections expose only stable
/// fields and reasons, never Serde text, property names or body excerpts.
pub struct ValidatedJson<T>(Arc<T>);

/// Proof that a state-changing operation declared without an `OpenAPI`
/// request body received no body bytes.
pub struct ValidatedEmptyBody;

impl<T> ValidatedJson<T> {
    #[must_use]
    pub fn into_inner(self) -> T
    where
        T: Clone,
    {
        Arc::unwrap_or_clone(self.0)
    }
}

impl<T> Deref for ValidatedJson<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Clone)]
struct ParsedJson<T>(Arc<T>);

struct ParsedJsonRequest<T> {
    parts: Parts,
    value: Arc<T>,
}

impl<T, S> FromRequest<S> for ValidatedJson<T>
where
    T: DashboardRequestBody,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(mut request: Request, _state: &S) -> Result<Self, Self::Rejection> {
        if let Some(parsed) = request.extensions_mut().remove::<ParsedJson<T>>() {
            return Ok(Self(parsed.0));
        }
        parse_json_request::<T>(request)
            .await
            .map(|parsed| Self(parsed.value))
    }
}

impl<S> FromRequest<S> for ValidatedEmptyBody
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(request: Request, _state: &S) -> Result<Self, Self::Rejection> {
        parse_empty_request(request).await.map(|_| Self)
    }
}

pub(super) async fn require_json_body<T>(request: Request, next: Next) -> Response
where
    T: DashboardRequestBody,
{
    match parse_json_request::<T>(request).await {
        Ok(ParsedJsonRequest { mut parts, value }) => {
            parts.extensions.insert(ParsedJson(value));
            next.run(Request::from_parts(parts, Body::empty())).await
        }
        Err(response) => response,
    }
}

pub(super) async fn require_empty_body(request: Request, next: Next) -> Response {
    match parse_empty_request(request).await {
        Ok(parts) => next.run(Request::from_parts(parts, Body::empty())).await,
        Err(response) => response,
    }
}

async fn parse_json_request<T>(request: Request) -> Result<ParsedJsonRequest<T>, Response>
where
    T: DashboardRequestBody,
{
    if !has_supported_content_encoding(request.headers())
        || !has_application_json_content_type(request.headers())
    {
        return Err(api_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            ApiErrorCode::UnsupportedMediaType,
            None,
        ));
    }

    match content_length(request.headers()) {
        ContentLength::WithinLimit => {}
        ContentLength::TooLarge => return Err(payload_too_large()),
        ContentLength::Invalid => {
            return Err(validation_error(ValidationIssue::invalid_format(
                ValidationField::RequestBody,
            )));
        }
    }

    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, MAX_DASHBOARD_JSON_BODY_BYTES).await {
        Ok(body) => body,
        Err(error)
            if error
                .source()
                .is_some_and(<dyn std::error::Error>::is::<LengthLimitError>) =>
        {
            return Err(payload_too_large());
        }
        Err(_) => {
            return Err(validation_error(ValidationIssue::invalid_format(
                ValidationField::RequestBody,
            )));
        }
    };

    if body.iter().all(u8::is_ascii_whitespace) {
        return Err(validation_error(ValidationIssue::required(
            ValidationField::RequestBody,
        )));
    }

    let value = serde_json::from_slice::<StrictJsonValue>(&body).map_err(|_| {
        validation_error(ValidationIssue::invalid_format(
            ValidationField::RequestBody,
        ))
    })?;
    let value = serde_json::from_value::<T>(value.0).map_err(|_| {
        validation_error(ValidationIssue::invalid_format(
            ValidationField::RequestBody,
        ))
    })?;
    value.validate().map_err(validation_errors)?;
    Ok(ParsedJsonRequest {
        parts,
        value: Arc::new(value),
    })
}

async fn parse_empty_request(request: Request) -> Result<Parts, Response> {
    if !declares_empty_body(request.headers()) {
        return Err(validation_error(ValidationIssue::invalid_format(
            ValidationField::RequestBody,
        )));
    }

    let (parts, body) = request.into_parts();
    match to_bytes(body, 0).await {
        Ok(body) if body.is_empty() => Ok(parts),
        Ok(_) | Err(_) => Err(validation_error(ValidationIssue::invalid_format(
            ValidationField::RequestBody,
        ))),
    }
}

enum ContentLength {
    WithinLimit,
    TooLarge,
    Invalid,
}

fn content_length(headers: &HeaderMap) -> ContentLength {
    let mut values = headers.get_all(CONTENT_LENGTH).iter();
    let Some(value) = values.next() else {
        return ContentLength::WithinLimit;
    };
    if values.next().is_some() {
        return ContentLength::Invalid;
    }
    let Ok(value) = value.to_str() else {
        return ContentLength::Invalid;
    };
    let Ok(value) = value.parse::<usize>() else {
        return ContentLength::Invalid;
    };
    if value > MAX_DASHBOARD_JSON_BODY_BYTES {
        ContentLength::TooLarge
    } else {
        ContentLength::WithinLimit
    }
}

fn declares_empty_body(headers: &HeaderMap) -> bool {
    let mut values = headers.get_all(CONTENT_LENGTH).iter();
    let Some(value) = values.next() else {
        return true;
    };
    if values.next().is_some() {
        return false;
    }
    value.as_bytes() == b"0"
}

fn has_supported_content_encoding(headers: &HeaderMap) -> bool {
    let mut values = headers.get_all(CONTENT_ENCODING).iter();
    let Some(value) = values.next() else {
        return true;
    };
    values.next().is_none() && value.as_bytes().eq_ignore_ascii_case(b"identity")
}

fn has_application_json_content_type(headers: &HeaderMap) -> bool {
    let mut values = headers.get_all(CONTENT_TYPE).iter();
    let Some(value) = values.next() else {
        return false;
    };
    if values.next().is_some() {
        return false;
    }
    let Ok(value) = value.to_str() else {
        return false;
    };
    let Ok(media_type) = value.parse::<Mime>() else {
        return false;
    };
    media_type.type_() == APPLICATION
        && media_type.subtype() == JSON
        && media_type.suffix().is_none()
}

fn payload_too_large() -> Response {
    api_error(
        StatusCode::PAYLOAD_TOO_LARGE,
        ApiErrorCode::PayloadTooLarge,
        None,
    )
}

struct StrictJsonValue(Value);

impl<'de> Deserialize<'de> for StrictJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictJsonValueVisitor)
    }
}

struct StrictJsonValueVisitor;

impl<'de> Visitor<'de> for StrictJsonValueVisitor {
    type Value = StrictJsonValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Number(Number::from(value))))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Number(Number::from(value))))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .map(StrictJsonValue)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Null))
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        StrictJsonValue::deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or_default());
        while let Some(value) = sequence.next_element::<StrictJsonValue>()? {
            values.push(value.0);
        }
        Ok(StrictJsonValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = HashSet::with_capacity(object.size_hint().unwrap_or_default());
        let mut values = Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(de::Error::custom("duplicate JSON object key"));
            }
            let value = object.next_value::<StrictJsonValue>()?;
            values.insert(key, value.0);
        }
        Ok(StrictJsonValue(Value::Object(values)))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use axum::http::header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE};
    use axum::response::Response;
    use axum::routing::post;
    use serde::Deserialize;
    use serde_json::Value;
    use tower::ServiceExt as _;

    use super::*;

    #[derive(Clone, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct TestBody {
        enabled: bool,
        count: u16,
        nested: TestNested,
    }

    #[derive(Clone, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct TestNested {
        label: String,
    }

    impl sealed::Sealed for TestBody {}

    impl DashboardRequestBody for TestBody {
        fn validate(&self) -> Result<(), Vec<ValidationIssue>> {
            let _ = (self.enabled, self.nested.label.as_str());
            if self.count > 10 {
                Err(vec![ValidationIssue::out_of_range(
                    ValidationField::NotifyMaxN,
                    Some(0),
                    Some(10),
                )])
            } else {
                Ok(())
            }
        }
    }

    fn router(calls: Arc<AtomicUsize>) -> Router {
        Router::new().route(
            "/",
            post(move |body: ValidatedJson<TestBody>| {
                let calls = Arc::clone(&calls);
                async move {
                    let _ = body.into_inner();
                    calls.fetch_add(1, Ordering::SeqCst);
                    StatusCode::NO_CONTENT
                }
            }),
        )
    }

    fn empty_router(calls: Arc<AtomicUsize>) -> Router {
        Router::new().route(
            "/",
            post(move |_: ValidatedEmptyBody| {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    StatusCode::NO_CONTENT
                }
            }),
        )
    }

    fn request_builder() -> axum::http::request::Builder {
        Request::builder().method("POST").uri("/")
    }

    fn valid_json() -> &'static str {
        r#"{"enabled":true,"count":3,"nested":{"label":"ok"}}"#
    }

    async fn response_json(response: Response) -> Value {
        let body = to_bytes(response.into_body(), 8_192).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn send(
        calls: &Arc<AtomicUsize>,
        builder: axum::http::request::Builder,
        body: impl Into<Body>,
    ) -> Response {
        router(Arc::clone(calls))
            .oneshot(builder.body(body.into()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn exact_json_media_type_with_parameters_and_identity_encoding_is_accepted() {
        let calls = Arc::new(AtomicUsize::new(0));
        let response = send(
            &calls,
            request_builder()
                .header(CONTENT_TYPE, "application/json; charset=utf-8")
                .header(CONTENT_ENCODING, "identity"),
            valid_json(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn missing_duplicate_or_non_json_media_metadata_is_rejected_without_calling_handler() {
        let calls = Arc::new(AtomicUsize::new(0));
        let builders = [
            request_builder(),
            request_builder().header(CONTENT_TYPE, "application/problem+json"),
            request_builder().header(CONTENT_TYPE, "text/json"),
            request_builder()
                .header(CONTENT_TYPE, "application/json")
                .header(CONTENT_TYPE, "application/json"),
            request_builder()
                .header(CONTENT_TYPE, "application/json")
                .header(CONTENT_ENCODING, "gzip"),
            request_builder()
                .header(CONTENT_TYPE, "application/json")
                .header(CONTENT_ENCODING, "identity")
                .header(CONTENT_ENCODING, "identity"),
        ];

        for builder in builders {
            let response = send(&calls, builder, valid_json()).await;
            assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
            assert_eq!(
                response_json(response).await["error"]["code"],
                "unsupported_media_type"
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn malformed_duplicate_unknown_and_incomplete_objects_are_closed_schema_errors() {
        let calls = Arc::new(AtomicUsize::new(0));
        let invalid_bodies = [
            String::new(),
            "   \r\n".to_owned(),
            "{".to_owned(),
            format!("{} trailing", valid_json()),
            r#"{"enabled":true,"enabled":false,"count":3,"nested":{"label":"ok"}}"#.to_owned(),
            r#"{"enabled":true,"count":3,"nested":{"label":"a","label":"b"}}"#.to_owned(),
            r#"{"enabled":true,"count":3,"nested":{"label":"ok"},"secret_key":"secret-value"}"#
                .to_owned(),
            r#"{"enabled":true,"count":3,"nested":{"label":"ok","extra":"secret-value"}}"#
                .to_owned(),
            r#"{"enabled":true,"nested":{"label":"ok"}}"#.to_owned(),
            r#"{"enabled":"yes","count":3,"nested":{"label":"ok"}}"#.to_owned(),
        ];

        for body in invalid_bodies {
            let response = send(
                &calls,
                request_builder().header(CONTENT_TYPE, "application/json"),
                body,
            )
            .await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let response = response_json(response).await;
            assert_eq!(response["error"]["code"], "validation_failed");
            let encoded = serde_json::to_string(&response).unwrap();
            assert!(!encoded.contains("secret_key"));
            assert!(!encoded.contains("secret-value"));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn semantic_validation_returns_only_bounded_safe_issues() {
        let calls = Arc::new(AtomicUsize::new(0));
        let response = send(
            &calls,
            request_builder().header(CONTENT_TYPE, "application/json"),
            r#"{"enabled":true,"count":11,"nested":{"label":"ok"}}"#,
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let response = response_json(response).await;
        assert_eq!(response["error"]["code"], "validation_failed");
        assert_eq!(
            response["error"]["params"]["issues"][0],
            serde_json::json!({
                "field": "notify_max_n",
                "reason": "out_of_range",
                "minimum": 0,
                "maximum": 10
            })
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn exact_body_limit_is_accepted_and_every_over_limit_form_is_413() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut exact = valid_json().as_bytes().to_vec();
        exact.resize(MAX_DASHBOARD_JSON_BODY_BYTES, b' ');
        let response = send(
            &calls,
            request_builder().header(CONTENT_TYPE, "application/json"),
            exact,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let mut streamed_over_limit = valid_json().as_bytes().to_vec();
        streamed_over_limit.resize(MAX_DASHBOARD_JSON_BODY_BYTES + 1, b' ');
        let responses = [
            send(
                &calls,
                request_builder().header(CONTENT_TYPE, "application/json"),
                streamed_over_limit,
            )
            .await,
            send(
                &calls,
                request_builder()
                    .header(CONTENT_TYPE, "application/json")
                    .header(
                        CONTENT_LENGTH,
                        (MAX_DASHBOARD_JSON_BODY_BYTES + 1).to_string(),
                    ),
                valid_json(),
            )
            .await,
        ];
        for response in responses {
            assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
            assert_eq!(
                response_json(response).await["error"]["code"],
                "payload_too_large"
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn operations_without_a_body_reject_declared_and_streamed_bytes() {
        let calls = Arc::new(AtomicUsize::new(0));
        let accepted = empty_router(Arc::clone(&calls))
            .oneshot(
                request_builder()
                    .header(CONTENT_LENGTH, "0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::NO_CONTENT);

        let rejected = [
            request_builder()
                .header(CONTENT_LENGTH, "1")
                .body(Body::empty())
                .unwrap(),
            request_builder().body(Body::from("x")).unwrap(),
        ];
        for request in rejected {
            let response = empty_router(Arc::clone(&calls))
                .oneshot(request)
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(
                response_json(response).await["error"]["code"],
                "validation_failed"
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
