use std::net::{Ipv4Addr, SocketAddr};

use axum::Router;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::header::{ALLOW, CACHE_CONTROL, COOKIE, HOST, ORIGIN};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::MethodRouter;

use super::api_error::{ApiErrorCode, REFERRER_POLICY, api_error, with_no_referrer};
use super::session::{
    AuthenticatedSession, AuthorizedStateChange, DashboardSessions, SessionAuthenticationError,
};

pub const SESSION_COOKIE_NAME: &str = "codex_monitor_session";
pub const CSRF_HEADER_NAME: &str = "X-CodexMonitor-CSRF";

pub(super) const BOOTSTRAP_CHALLENGE: &str = "CodexMonitorBootstrap realm=\"local-dashboard\"";
pub(super) const SESSION_CHALLENGE: &str = "CodexMonitorSession realm=\"local-dashboard\"";
pub(super) const MAX_COOKIE_HEADER_BYTES: usize = 4_096;

const BOOTSTRAP_PATH: &str = "/api/v1/session/bootstrap";
const CSRF_HEADER: HeaderName = HeaderName::from_static("x-codexmonitor-csrf");
#[derive(Clone)]
pub(super) struct DashboardSecurity {
    expected_authority: HeaderValue,
    expected_origin: HeaderValue,
    sessions: DashboardSessions,
}

impl DashboardSecurity {
    /// Capture the exact authority and origin of an already-bound Dashboard listener.
    ///
    /// # Panics
    ///
    /// Panics unless the address is `127.0.0.1` with a non-zero port.
    pub(super) fn new(local_address: SocketAddr, sessions: DashboardSessions) -> Self {
        let SocketAddr::V4(local_address) = local_address else {
            panic!("Dashboard security requires an IPv4 loopback address");
        };
        assert_eq!(
            *local_address.ip(),
            Ipv4Addr::LOCALHOST,
            "Dashboard security requires 127.0.0.1"
        );
        assert_ne!(
            local_address.port(),
            0,
            "Dashboard security requires an already-bound port"
        );

        let authority = local_address.to_string();
        Self {
            expected_authority: HeaderValue::from_str(&authority)
                .expect("an IPv4 socket authority is always a valid Host value"),
            expected_origin: HeaderValue::from_str(&format!("http://{authority}"))
                .expect("an IPv4 loopback HTTP origin is always a valid header value"),
            sessions,
        }
    }

    /// Put Host validation in front of inner business routing and remove every
    /// CORS permission header from inner success and error responses.
    pub(super) fn wrap(self, inner: Router) -> Router {
        Router::new()
            .fallback_service(inner)
            .layer(middleware::from_fn_with_state(
                self,
                enforce_dashboard_boundary,
            ))
    }
}

/// Apply current-session authentication only to methods already registered in
/// `route`. Add explicit rejected methods and the method fallback afterwards.
pub(super) fn protect_read<S>(
    route: MethodRouter<S>,
    security: DashboardSecurity,
) -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    route.route_layer(middleware::from_fn_with_state(
        security,
        require_current_session,
    ))
}

/// Apply session, exact-Origin and session-bound CSRF authorization only to
/// methods already registered in `route`.
pub(super) fn protect_state_change<S>(
    route: MethodRouter<S>,
    security: DashboardSecurity,
) -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    route.route_layer(middleware::from_fn_with_state(
        security,
        authorize_state_change,
    ))
}

pub(super) fn method_not_allowed(allow: &'static str) -> Response {
    let mut response = StatusCode::METHOD_NOT_ALLOWED.into_response();
    let headers = response.headers_mut();
    headers.insert(ALLOW, HeaderValue::from_static(allow));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

pub(super) fn bootstrap_method_not_allowed(allow: &'static str) -> Response {
    with_no_referrer(method_not_allowed(allow))
}

async fn enforce_dashboard_boundary(
    State(security): State<DashboardSecurity>,
    mut request: Request,
    next: Next,
) -> Response {
    mark_credentials_sensitive(request.headers_mut());
    let is_bootstrap = request.uri().path() == BOOTSTRAP_PATH;

    if !valid_request_authority(&request, &security.expected_authority) {
        let response = api_error(StatusCode::FORBIDDEN, ApiErrorCode::ForbiddenHost, None);
        return finalize_response(response, is_bootstrap);
    }

    finalize_response(next.run(request).await, is_bootstrap)
}

async fn require_current_session(
    State(security): State<DashboardSecurity>,
    mut request: Request,
    next: Next,
) -> Response {
    let authenticated = match authenticate_request(&security.sessions, request.headers()) {
        Ok(authenticated) => authenticated,
        Err(failure) => return authentication_failure(failure),
    };
    request.extensions_mut().insert(authenticated);
    next.run(request).await
}

async fn authorize_state_change(
    State(security): State<DashboardSecurity>,
    mut request: Request,
    next: Next,
) -> Response {
    let authenticated = match authenticate_request(&security.sessions, request.headers()) {
        Ok(authenticated) => authenticated,
        Err(failure) => return authentication_failure(failure),
    };

    if !single_header_equals(request.headers(), &ORIGIN, &security.expected_origin) {
        return api_error(StatusCode::FORBIDDEN, ApiErrorCode::ForbiddenOrigin, None);
    }

    let Some(proof) =
        single_header(request.headers(), &CSRF_HEADER).and_then(|value| value.to_str().ok())
    else {
        return api_error(StatusCode::FORBIDDEN, ApiErrorCode::CsrfInvalid, None);
    };
    let Some(authorization) = authenticated.authorize_state_change(proof) else {
        return api_error(StatusCode::FORBIDDEN, ApiErrorCode::CsrfInvalid, None);
    };

    request.extensions_mut().insert(authenticated);
    request.extensions_mut().insert(authorization);
    next.run(request).await
}

fn authenticate_request(
    sessions: &DashboardSessions,
    headers: &HeaderMap,
) -> Result<AuthenticatedSession, AuthenticationFailure> {
    let Some(cookie) = session_cookie(headers) else {
        return Err(AuthenticationFailure::Unauthorized);
    };
    match sessions.authenticate_session(cookie) {
        Ok(authenticated) => Ok(authenticated),
        Err(SessionAuthenticationError::Unauthorized) => Err(AuthenticationFailure::Unauthorized),
        Err(SessionAuthenticationError::Internal(_)) => Err(AuthenticationFailure::Internal),
    }
}

#[derive(Clone, Copy)]
enum AuthenticationFailure {
    Unauthorized,
    Internal,
}

fn authentication_failure(failure: AuthenticationFailure) -> Response {
    match failure {
        AuthenticationFailure::Unauthorized => unauthorized(),
        AuthenticationFailure::Internal => api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            ApiErrorCode::InternalUnavailable,
            None,
        ),
    }
}

fn valid_request_authority(request: &Request, expected: &HeaderValue) -> bool {
    single_header_equals(request.headers(), &HOST, expected)
        && request
            .uri()
            .authority()
            .is_none_or(|authority| authority.as_str().as_bytes() == expected.as_bytes())
}

fn mark_credentials_sensitive(headers: &mut HeaderMap) {
    for (name, value) in headers.iter_mut() {
        if name == COOKIE || name == CSRF_HEADER {
            value.set_sensitive(true);
        }
    }
}

fn single_header_equals(headers: &HeaderMap, name: &HeaderName, expected: &HeaderValue) -> bool {
    single_header(headers, name).is_some_and(|actual| actual == expected)
}

fn single_header<'a>(headers: &'a HeaderMap, name: &HeaderName) -> Option<&'a HeaderValue> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

fn session_cookie(headers: &HeaderMap) -> Option<&str> {
    let mut total_bytes = 0_usize;
    let mut session_value = None;

    for header in headers.get_all(COOKIE) {
        total_bytes = total_bytes.checked_add(header.as_bytes().len())?;
        if total_bytes > MAX_COOKIE_HEADER_BYTES {
            return None;
        }
        let header = header.to_str().ok()?;
        for pair in header.split(';') {
            let Some((name, value)) = pair.trim().split_once('=') else {
                continue;
            };
            if name == SESSION_COOKIE_NAME && session_value.replace(value).is_some() {
                return None;
            }
        }
    }

    session_value
}

fn unauthorized() -> Response {
    api_error(
        StatusCode::UNAUTHORIZED,
        ApiErrorCode::Unauthorized,
        Some(SESSION_CHALLENGE),
    )
}

fn finalize_response(mut response: Response, is_bootstrap: bool) -> Response {
    let cors_headers = response
        .headers()
        .keys()
        .filter(|name| name.as_str().starts_with("access-control-"))
        .cloned()
        .collect::<Vec<_>>();
    for header in cors_headers {
        response.headers_mut().remove(header);
    }
    if is_bootstrap {
        response
            .headers_mut()
            .insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    }
    response
}

impl<S> FromRequestParts<S> for AuthenticatedSession
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts.extensions.get::<Self>().cloned().ok_or_else(|| {
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                ApiErrorCode::InternalUnavailable,
                None,
            )
        })
    }
}

impl<S> FromRequestParts<S> for AuthorizedStateChange
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts.extensions.get::<Self>().copied().ok_or_else(|| {
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                ApiErrorCode::InternalUnavailable,
                None,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::{Body, to_bytes};
    use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE, WWW_AUTHENTICATE};
    use axum::http::{Method, Request};
    use axum::routing::get;
    use serde::Deserialize;
    use serde_json::Value;
    use tower::ServiceExt as _;

    use super::*;
    use crate::request::{
        DashboardRequestBody, MAX_DASHBOARD_JSON_BODY_BYTES, ValidatedJson, sealed,
    };
    use crate::routing::DashboardApi;

    const ADDRESS: &str = "127.0.0.1:4319";
    const ORIGIN_VALUE: &str = "http://127.0.0.1:4319";

    #[derive(Clone)]
    struct Calls {
        read: Arc<AtomicUsize>,
        mutation: Arc<AtomicUsize>,
    }

    struct Credentials {
        cookie: String,
        csrf: String,
    }

    #[derive(Clone, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct MutationBody {
        enabled: bool,
    }

    impl sealed::Sealed for MutationBody {}

    impl DashboardRequestBody for MutationBody {}

    fn test_router(sessions: DashboardSessions) -> (Router, Calls) {
        let calls = Calls {
            read: Arc::new(AtomicUsize::new(0)),
            mutation: Arc::new(AtomicUsize::new(0)),
        };

        let read_calls = Arc::clone(&calls.read);
        let read = move |_: AuthenticatedSession| {
            let read_calls = Arc::clone(&read_calls);
            async move {
                read_calls.fetch_add(1, Ordering::SeqCst);
                StatusCode::NO_CONTENT
            }
        };

        let mutation_calls = Arc::clone(&calls.mutation);
        let mutation = move |_: AuthorizedStateChange, body: ValidatedJson<MutationBody>| {
            let mutation_calls = Arc::clone(&mutation_calls);
            async move {
                assert!(body.into_inner().enabled);
                mutation_calls.fetch_add(1, Ordering::SeqCst);
                StatusCode::NO_CONTENT
            }
        };

        let inspect_headers = |_: AuthorizedStateChange, headers: HeaderMap| async move {
            let cookie_sensitive = headers.get(COOKIE).is_some_and(HeaderValue::is_sensitive);
            let csrf_sensitive = headers
                .get(&CSRF_HEADER)
                .is_some_and(HeaderValue::is_sensitive);
            if cookie_sensitive && csrf_sensitive {
                StatusCode::NO_CONTENT
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };

        let router = DashboardApi::new(ADDRESS.parse().unwrap(), sessions)
            .protected_get("/read", read)
            .protected_json_post::<MutationBody, _, _>("/mutate", mutation)
            .protected_empty_post("/inspect", inspect_headers)
            .finish(())
            .into_inner();
        (router, calls)
    }

    async fn malicious_cors_response() -> Response {
        let mut response = StatusCode::NO_CONTENT.into_response();
        for name in [
            "access-control-allow-origin",
            "access-control-allow-private-network",
            "access-control-expose-headers",
            "access-control-unregistered-test-header",
        ] {
            response.headers_mut().insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_static("malicious"),
            );
        }
        response
    }

    fn credentials(sessions: &DashboardSessions) -> Credentials {
        let token = sessions.issue_bootstrap().unwrap();
        let established = sessions.consume_bootstrap(token.query_value()).unwrap();
        let authenticated = sessions
            .authenticate_session(established.cookie_value())
            .unwrap();
        Credentials {
            cookie: format!("{SESSION_COOKIE_NAME}={}", established.cookie_value()),
            csrf: authenticated.csrf_proof().to_owned(),
        }
    }

    fn request(method: Method, uri: &str) -> axum::http::request::Builder {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(HOST, ADDRESS)
    }

    fn authenticated(
        builder: axum::http::request::Builder,
        credentials: &Credentials,
    ) -> axum::http::request::Builder {
        builder.header(COOKIE, &credentials.cookie)
    }

    fn authorized(
        builder: axum::http::request::Builder,
        credentials: &Credentials,
    ) -> axum::http::request::Builder {
        authenticated(builder, credentials)
            .header(ORIGIN, ORIGIN_VALUE)
            .header(CSRF_HEADER_NAME, &credentials.csrf)
    }

    async fn body_bytes(response: Response) -> Vec<u8> {
        to_bytes(response.into_body(), 8_192)
            .await
            .unwrap()
            .to_vec()
    }

    async fn error_code(response: Response) -> String {
        let body: Value = serde_json::from_slice(&body_bytes(response).await).unwrap();
        body["error"]["code"].as_str().unwrap().to_owned()
    }

    fn assert_no_cors(response: &Response) {
        assert!(
            response
                .headers()
                .keys()
                .all(|name| !name.as_str().starts_with("access-control-"))
        );
    }

    #[tokio::test]
    async fn exact_host_precedes_business_routing_for_every_path() {
        let (app, _) = test_router(DashboardSessions::default());
        let rejected = [
            Request::builder()
                .uri("/missing")
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .uri("/missing")
                .header(HOST, "localhost:4319")
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .uri("/missing")
                .header(HOST, ADDRESS)
                .header(HOST, ADDRESS)
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .uri("http://127.0.0.1:9999/missing")
                .header(HOST, ADDRESS)
                .body(Body::empty())
                .unwrap(),
        ];
        for request in rejected {
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
            assert_no_cors(&response);
            assert_eq!(error_code(response).await, "forbidden_host");
        }

        let response = app
            .oneshot(
                request(Method::GET, "http://127.0.0.1:4319/missing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_no_cors(&response);
    }

    #[tokio::test]
    async fn undeclared_methods_are_405_before_authentication_and_never_call_handlers() {
        let (app, calls) = test_router(DashboardSessions::default());
        for method in [Method::HEAD, Method::OPTIONS, Method::POST] {
            let response = app
                .clone()
                .oneshot(request(method, "/read").body(Body::from("unread")).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
            assert_eq!(response.headers()[ALLOW], "GET");
            assert!(!response.headers().contains_key(WWW_AUTHENTICATE));
            assert_no_cors(&response);
        }
        for method in [Method::HEAD, Method::OPTIONS, Method::GET, Method::PUT] {
            let response = app
                .clone()
                .oneshot(
                    request(method, "/mutate")
                        .body(Body::from("unread"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
            assert_eq!(response.headers()[ALLOW], "POST");
            assert!(!response.headers().contains_key(WWW_AUTHENTICATE));
            assert_no_cors(&response);
        }
        assert_eq!(calls.read.load(Ordering::SeqCst), 0);
        assert_eq!(calls.mutation.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn protected_read_requires_session_but_does_not_reject_external_origin() {
        let sessions = DashboardSessions::default();
        let credentials = credentials(&sessions);
        let (app, calls) = test_router(sessions);

        let response = app
            .clone()
            .oneshot(request(Method::GET, "/read").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers()[WWW_AUTHENTICATE], SESSION_CHALLENGE);
        assert_eq!(error_code(response).await, "unauthorized");

        let response = app
            .oneshot(
                authenticated(request(Method::GET, "/read"), &credentials)
                    .header(ORIGIN, "https://external.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_no_cors(&response);
        assert_eq!(calls.read.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn state_change_validation_order_includes_body_size_before_schema_and_handler() {
        let sessions = DashboardSessions::default();
        let credentials = credentials(&sessions);
        let (app, calls) = test_router(sessions);

        let response = app
            .clone()
            .oneshot(
                request(Method::POST, "/mutate")
                    .body(Body::from("not-json"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(error_code(response).await, "unauthorized");

        let response = app
            .clone()
            .oneshot(
                authenticated(request(Method::POST, "/mutate"), &credentials)
                    .body(Body::from("not-json"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(error_code(response).await, "forbidden_origin");

        let response = app
            .clone()
            .oneshot(
                authenticated(request(Method::POST, "/mutate"), &credentials)
                    .header(ORIGIN, ORIGIN_VALUE)
                    .body(Body::from("not-json"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(error_code(response).await, "csrf_invalid");

        let response = app
            .clone()
            .oneshot(
                authorized(request(Method::POST, "/mutate"), &credentials)
                    .body(Body::from("not-json"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(error_code(response).await, "unsupported_media_type");

        let response = app
            .clone()
            .oneshot(
                authorized(request(Method::POST, "/mutate"), &credentials)
                    .header(CONTENT_TYPE, "application/json")
                    .header(
                        CONTENT_LENGTH,
                        (MAX_DASHBOARD_JSON_BODY_BYTES + 1).to_string(),
                    )
                    .body(Body::from("not-json"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(error_code(response).await, "payload_too_large");

        let response = app
            .clone()
            .oneshot(
                authorized(request(Method::POST, "/mutate"), &credentials)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from("not-json"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(error_code(response).await, "validation_failed");
        assert_eq!(calls.mutation.load(Ordering::SeqCst), 0);

        let response = app
            .oneshot(
                authorized(request(Method::POST, "/mutate"), &credentials)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"enabled":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(calls.mutation.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn csrf_is_bound_to_the_cookie_session_and_credentials_are_sensitive() {
        let sessions = DashboardSessions::default();
        let first = credentials(&sessions);
        let second = credentials(&sessions);
        let (app, calls) = test_router(sessions);

        let response = app
            .clone()
            .oneshot(
                authenticated(request(Method::POST, "/mutate"), &first)
                    .header(ORIGIN, ORIGIN_VALUE)
                    .header(CSRF_HEADER_NAME, &second.csrf)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"enabled":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(error_code(response).await, "csrf_invalid");
        assert_eq!(calls.mutation.load(Ordering::SeqCst), 0);

        let response = app
            .oneshot(
                authorized(request(Method::POST, "/inspect"), &first)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn outer_boundary_removes_registered_and_future_cors_permission_headers() {
        let security =
            DashboardSecurity::new(ADDRESS.parse().unwrap(), DashboardSessions::default());
        let app = security.wrap(Router::new().route("/cors", get(malicious_cors_response)));
        let response = app
            .oneshot(request(Method::GET, "/cors").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_no_cors(&response);
    }

    #[tokio::test]
    async fn missing_internal_request_capabilities_fail_with_stable_json() {
        let security =
            DashboardSecurity::new(ADDRESS.parse().unwrap(), DashboardSessions::default());
        let app = security.wrap(
            Router::new()
                .route(
                    "/session-capability",
                    get(|_: AuthenticatedSession| async { StatusCode::NO_CONTENT }),
                )
                .route(
                    "/mutation-capability",
                    get(|_: AuthorizedStateChange| async { StatusCode::NO_CONTENT }),
                ),
        );

        for path in ["/session-capability", "/mutation-capability"] {
            let response = app
                .clone()
                .oneshot(request(Method::GET, path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
            assert_eq!(error_code(response).await, "internal_unavailable");
        }
    }

    #[test]
    #[should_panic(expected = "Dashboard security requires 127.0.0.1")]
    fn security_rejects_alternate_loopback_addresses() {
        let _ = DashboardSecurity::new(
            "127.0.0.2:4319".parse().unwrap(),
            DashboardSessions::default(),
        );
    }
}
