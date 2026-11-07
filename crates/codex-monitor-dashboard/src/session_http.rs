#[cfg(test)]
use std::net::SocketAddr;

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::header::{CACHE_CONTROL, LOCATION, SET_COOKIE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use codex_monitor_core::UtcTimestamp;
use serde::{Deserialize, Serialize};

use super::api_error::{ApiErrorCode, REFERRER_POLICY, bootstrap_error};
#[cfg(test)]
use super::routing::{DashboardApi, DashboardRouter};
use super::security::{BOOTSTRAP_CHALLENGE, CSRF_HEADER_NAME, SESSION_COOKIE_NAME};
use super::session::{
    AuthenticatedSession, BootstrapConsumeError, DashboardSessions, EstablishedSession,
};

const CLEAN_DASHBOARD_LOCATION: &str = "/";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BootstrapQuery {
    token: String,
}

#[derive(Serialize)]
struct CurrentSessionResponse<'a> {
    authenticated: bool,
    csrf: CsrfResponse<'a>,
    expires_at: UtcTimestamp,
}

#[derive(Serialize)]
struct CsrfResponse<'a> {
    header_name: &'static str,
    proof: &'a str,
}

/// Build the process-local session endpoints for a verified Dashboard address.
///
/// The exact active authority is captured before any token can be looked up or
/// consumed. The remaining cross-origin and CSRF policy is composed in the
/// complete Dashboard router.
///
/// # Panics
///
/// Panics when called with an address outside IPv4 loopback. The listener
/// factory supplies only an address it has already bound and verified.
#[cfg(test)]
pub(crate) fn dashboard_session_router(
    local_address: SocketAddr,
    sessions: DashboardSessions,
) -> DashboardRouter {
    DashboardApi::new(local_address, sessions.clone())
        .bootstrap(bootstrap_session)
        .protected_get("/api/v1/session", current_session)
        .finish(sessions)
}

pub(super) async fn bootstrap_session(
    State(sessions): State<DashboardSessions>,
    query: Result<Query<BootstrapQuery>, QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return bootstrap_rejected();
    };

    match sessions.consume_bootstrap(&query.token) {
        Ok(session) => bootstrap_success(&session),
        Err(BootstrapConsumeError::Invalid) => bootstrap_rejected(),
        Err(BootstrapConsumeError::Internal(_)) => bootstrap_error(
            StatusCode::SERVICE_UNAVAILABLE,
            ApiErrorCode::InternalUnavailable,
            None,
        ),
    }
}

pub(super) async fn current_session(session: AuthenticatedSession) -> Response {
    current_session_success(&session)
}

fn bootstrap_success(session: &EstablishedSession) -> Response {
    let cookie = format!(
        "{SESSION_COOKIE_NAME}={}; Path=/; HttpOnly; SameSite=Strict",
        session.cookie_value()
    );
    let mut cookie = HeaderValue::from_str(&cookie)
        .expect("base64url session credentials always form a valid cookie header");
    cookie.set_sensitive(true);

    let mut response = StatusCode::SEE_OTHER.into_response();
    let headers = response.headers_mut();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(LOCATION, HeaderValue::from_static(CLEAN_DASHBOARD_LOCATION));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(SET_COOKIE, cookie);
    response
}

fn current_session_success(session: &AuthenticatedSession) -> Response {
    let mut response = Json(CurrentSessionResponse {
        authenticated: true,
        csrf: CsrfResponse {
            header_name: CSRF_HEADER_NAME,
            proof: session.csrf_proof(),
        },
        expires_at: session.expires_at(),
    })
    .into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn bootstrap_rejected() -> Response {
    bootstrap_error(
        StatusCode::UNAUTHORIZED,
        ApiErrorCode::BootstrapInvalid,
        Some(BOOTSTRAP_CHALLENGE),
    )
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use axum::http::header::{ALLOW, COOKIE, HOST, WWW_AUTHENTICATE};
    use serde_json::Value;
    use tower::ServiceExt as _;

    use super::*;
    use crate::security::{MAX_COOKIE_HEADER_BYTES, SESSION_CHALLENGE};

    const ADDRESS: &str = "127.0.0.1:4319";

    fn router(sessions: DashboardSessions) -> Router {
        dashboard_session_router(ADDRESS.parse().unwrap(), sessions).into_inner()
    }

    fn request(uri: &str) -> axum::http::request::Builder {
        Request::builder().uri(uri).header(HOST, ADDRESS)
    }

    async fn body_bytes(response: Response) -> Vec<u8> {
        to_bytes(response.into_body(), 4_096)
            .await
            .unwrap()
            .to_vec()
    }

    #[tokio::test]
    async fn bootstrap_sets_exact_session_cookie_and_redirects_to_a_clean_url() {
        let sessions = DashboardSessions::default();
        let token = sessions.issue_bootstrap().unwrap();
        let response = router(sessions.clone())
            .oneshot(
                request(&format!(
                    "/api/v1/session/bootstrap?token={}",
                    token.query_value()
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()[LOCATION], "/");
        assert_eq!(response.headers()[REFERRER_POLICY], "no-referrer");
        assert!(response.headers()[SET_COOKIE].is_sensitive());
        let set_cookie = response.headers()[SET_COOKIE].to_str().unwrap().to_owned();
        assert!(set_cookie.starts_with("codex_monitor_session="));
        assert!(set_cookie.ends_with("; Path=/; HttpOnly; SameSite=Strict"));
        for forbidden in ["Domain=", "Expires=", "Max-Age=", "Secure"] {
            assert!(!set_cookie.contains(forbidden));
        }
        assert!(!set_cookie.contains(token.query_value()));
        assert!(body_bytes(response).await.is_empty());

        let cookie = set_cookie.split_once(';').unwrap().0;
        let response = router(sessions)
            .oneshot(
                request("/api/v1/session")
                    .header(COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        let body: Value = serde_json::from_slice(&body_bytes(response).await).unwrap();
        assert_eq!(body["authenticated"], true);
        assert_eq!(body["csrf"]["header_name"], CSRF_HEADER_NAME);
        assert_eq!(body["csrf"]["proof"].as_str().unwrap().len(), 43);
        assert!(body["expires_at"].is_string());
    }

    #[tokio::test]
    async fn invalid_missing_duplicate_and_reused_tokens_are_indistinguishable() {
        let sessions = DashboardSessions::default();
        let token = sessions.issue_bootstrap().unwrap();
        let valid_uri = format!("/api/v1/session/bootstrap?token={}", token.query_value());
        let success = router(sessions.clone())
            .oneshot(request(&valid_uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(success.status(), StatusCode::SEE_OTHER);

        let mut snapshots = Vec::new();
        for uri in [
            valid_uri,
            "/api/v1/session/bootstrap".to_owned(),
            "/api/v1/session/bootstrap?token=short".to_owned(),
            "/api/v1/session/bootstrap?token=one&token=two".to_owned(),
            "/api/v1/session/bootstrap?token=invalid&extra=value".to_owned(),
        ] {
            let response = router(sessions.clone())
                .oneshot(request(&uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            let status = response.status();
            let headers = response.headers().clone();
            let body = body_bytes(response).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            assert_eq!(headers[CACHE_CONTROL], "no-store");
            assert_eq!(headers[REFERRER_POLICY], "no-referrer");
            assert_eq!(headers[WWW_AUTHENTICATE], BOOTSTRAP_CHALLENGE);
            assert!(!headers.contains_key(SET_COOKIE));
            assert!(!headers.contains_key(LOCATION));
            snapshots.push((status, headers, body));
        }
        for snapshot in &snapshots[1..] {
            assert_eq!(snapshot.0, snapshots[0].0);
            assert_eq!(snapshot.1, snapshots[0].1);
            assert_eq!(snapshot.2, snapshots[0].2);
        }
        let payload: Value = serde_json::from_slice(&snapshots[0].2).unwrap();
        assert_eq!(payload["error"]["code"], "bootstrap_invalid");
        assert_eq!(payload["error"]["params"], serde_json::json!({}));
    }

    #[tokio::test]
    async fn unsupported_methods_are_rejected_without_consuming_or_authenticating() {
        let sessions = DashboardSessions::default();
        let token = sessions.issue_bootstrap().unwrap();
        let uri = format!("/api/v1/session/bootstrap?token={}", token.query_value());
        for method in [
            axum::http::Method::HEAD,
            axum::http::Method::OPTIONS,
            axum::http::Method::POST,
        ] {
            let rejected = router(sessions.clone())
                .oneshot(
                    request(&uri)
                        .method(method)
                        .body(Body::from("must-not-be-read"))
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(rejected.status(), StatusCode::METHOD_NOT_ALLOWED);
            assert_eq!(rejected.headers()[ALLOW], "GET");
            assert_eq!(rejected.headers()[CACHE_CONTROL], "no-store");
            assert_eq!(rejected.headers()[REFERRER_POLICY], "no-referrer");
            assert!(!rejected.headers().contains_key(SET_COOKIE));
            assert!(!rejected.headers().contains_key(LOCATION));
            assert!(body_bytes(rejected).await.is_empty());
        }

        let accepted = router(sessions.clone())
            .oneshot(request(&uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::SEE_OTHER);

        for method in [
            axum::http::Method::HEAD,
            axum::http::Method::OPTIONS,
            axum::http::Method::POST,
        ] {
            let rejected = router(sessions.clone())
                .oneshot(
                    request("/api/v1/session")
                        .method(method)
                        .body(Body::from("must-not-be-read"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(rejected.status(), StatusCode::METHOD_NOT_ALLOWED);
            assert_eq!(rejected.headers()[ALLOW], "GET");
            assert_eq!(rejected.headers()[CACHE_CONTROL], "no-store");
            assert!(!rejected.headers().contains_key(WWW_AUTHENTICATE));
            assert!(body_bytes(rejected).await.is_empty());
        }
    }

    #[tokio::test]
    async fn host_rejection_happens_before_the_one_time_token_is_consumed() {
        let sessions = DashboardSessions::default();
        let token = sessions.issue_bootstrap().unwrap();
        let uri = format!("/api/v1/session/bootstrap?token={}", token.query_value());
        let rejected = router(sessions.clone())
            .oneshot(
                Request::builder()
                    .uri(&uri)
                    .header(HOST, "127.0.0.1:9999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
        assert_eq!(rejected.headers()[CACHE_CONTROL], "no-store");
        assert_eq!(rejected.headers()[REFERRER_POLICY], "no-referrer");
        assert!(!rejected.headers().contains_key(SET_COOKIE));
        assert!(!rejected.headers().contains_key(LOCATION));
        let payload: Value = serde_json::from_slice(&body_bytes(rejected).await).unwrap();
        assert_eq!(payload["error"]["code"], "forbidden_host");
        assert_eq!(payload["error"]["params"], serde_json::json!({}));

        let accepted = router(sessions)
            .oneshot(request(&uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::SEE_OTHER);
    }

    #[tokio::test]
    async fn bootstrap_service_failure_has_the_closed_no_referrer_response() {
        let response = bootstrap_error(
            StatusCode::SERVICE_UNAVAILABLE,
            ApiErrorCode::InternalUnavailable,
            None,
        );

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()[REFERRER_POLICY], "no-referrer");
        assert!(!response.headers().contains_key(SET_COOKIE));
        assert!(!response.headers().contains_key(LOCATION));
        assert!(!response.headers().contains_key(WWW_AUTHENTICATE));
        let payload: Value = serde_json::from_slice(&body_bytes(response).await).unwrap();
        assert_eq!(payload["error"]["code"], "internal_unavailable");
        assert_eq!(payload["error"]["params"], serde_json::json!({}));
    }

    #[tokio::test]
    async fn malformed_duplicate_and_previous_process_cookies_are_unauthorized() {
        let old_process = DashboardSessions::default();
        let token = old_process.issue_bootstrap().unwrap();
        let established = old_process.consume_bootstrap(token.query_value()).unwrap();
        let old_cookie = format!("{SESSION_COOKIE_NAME}={}", established.cookie_value());
        let new_process = DashboardSessions::default();

        let mut requests = Vec::new();

        for cookie in [
            None,
            Some(old_cookie.as_str()),
            Some("codex_monitor_session=short"),
            Some("codex_monitor_session=one; codex_monitor_session=two"),
        ] {
            let mut request = request("/api/v1/session");
            if let Some(cookie) = cookie {
                request = request.header(COOKIE, cookie);
            }
            requests.push(request.body(Body::empty()).unwrap());
        }
        requests.push(
            request("/api/v1/session")
                .header(COOKIE, "codex_monitor_session=one")
                .header(COOKIE, "codex_monitor_session=two")
                .body(Body::empty())
                .unwrap(),
        );
        requests.push(
            request("/api/v1/session")
                .header(
                    COOKIE,
                    format!("other={}", "x".repeat(MAX_COOKIE_HEADER_BYTES)),
                )
                .header(COOKIE, old_cookie)
                .body(Body::empty())
                .unwrap(),
        );

        for request in requests {
            let response = router(new_process.clone()).oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
            assert_eq!(response.headers()[WWW_AUTHENTICATE], SESSION_CHALLENGE);
            let payload: Value = serde_json::from_slice(&body_bytes(response).await).unwrap();
            assert_eq!(payload["error"]["code"], "unauthorized");
        }
    }
}
