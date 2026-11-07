use std::net::SocketAddr;

use axum::Router;
use axum::handler::Handler;
use axum::middleware;
use axum::routing::{MethodRouter, delete, get, patch, post, put};
use tokio::sync::watch;

use super::request::{DashboardRequestBody, require_empty_body, require_json_body};
use super::security::{
    DashboardSecurity, bootstrap_method_not_allowed, method_not_allowed, protect_read,
    protect_state_change,
};
use super::session::DashboardSessions;

/// Builder for the complete Dashboard business router and its final security
/// boundary.
///
/// Route categories apply their required method-level authorization before
/// unsupported methods are installed. [`DashboardApi::finish`] supplies the
/// application state and puts exact Host validation around all business routing.
#[must_use = "Dashboard routes have no effect until finish is called"]
pub struct DashboardApi<S = ()> {
    inner: Router<S>,
    security: DashboardSecurity,
}

/// Fully composed Dashboard service. Its inner `Router` is intentionally
/// inaccessible outside the Dashboard listener, so routes and layers cannot be
/// appended after the security boundary has been installed.
pub struct DashboardRouter {
    inner: Router,
    stream_shutdown: Option<watch::Sender<bool>>,
}

impl DashboardRouter {
    #[cfg(test)]
    pub(super) fn into_inner(self) -> Router {
        self.inner
    }

    pub(super) fn with_stream_shutdown(mut self, shutdown: watch::Sender<bool>) -> Self {
        self.stream_shutdown = Some(shutdown);
        self
    }

    pub(super) fn into_parts(self) -> (Router, Option<watch::Sender<bool>>) {
        (self.inner, self.stream_shutdown)
    }
}

impl<S> DashboardApi<S>
where
    S: Clone + Send + Sync + 'static,
{
    /// Start a Dashboard router for an already-bound `127.0.0.1` listener.
    ///
    /// # Panics
    ///
    /// Panics unless `local_address` is `127.0.0.1` with a non-zero port.
    pub fn new(local_address: SocketAddr, sessions: DashboardSessions) -> Self {
        Self {
            inner: Router::new(),
            security: DashboardSecurity::new(local_address, sessions),
        }
    }

    /// Register the only unauthenticated `/api/v1` operation.
    pub fn bootstrap<H, T>(mut self, handler: H) -> Self
    where
        H: Handler<T, S>,
        T: 'static,
    {
        let route = get(handler)
            .head(reject_bootstrap_method)
            .fallback(reject_bootstrap_method);
        self.inner = self.inner.route("/api/v1/session/bootstrap", route);
        self
    }

    /// Register one presentation asset `GET` route inside the shared Host and
    /// no-CORS boundary. Presentation assets contain no monitoring data and do
    /// not receive API session middleware.
    pub(super) fn presentation_get<H, T>(mut self, path: &'static str, handler: H) -> Self
    where
        H: Handler<T, S>,
        T: 'static,
    {
        self.inner = self.inner.route(path, get(handler));
        self
    }

    /// Register the presentation fallback inside the shared Host and no-CORS
    /// boundary. The handler distinguishes exact embedded asset paths from
    /// genuinely unknown routes for every HTTP method.
    pub(super) fn presentation_fallback<H, T>(mut self, handler: H) -> Self
    where
        H: Handler<T, S>,
        T: 'static,
    {
        self.inner = self.inner.fallback(handler);
        self
    }

    /// Register one authenticated read-only `GET` operation.
    pub fn protected_get<H, T>(mut self, path: &'static str, handler: H) -> Self
    where
        H: Handler<T, S>,
        T: 'static,
    {
        let route = protect_read(get(handler), self.security.clone())
            .head(reject_get)
            .fallback(reject_get);
        self.inner = self.inner.route(path, route);
        self
    }

    /// Register an authenticated and CSRF-protected `POST` operation whose
    /// `OpenAPI` operation declares no request body.
    pub fn protected_empty_post<H, T>(mut self, path: &'static str, handler: H) -> Self
    where
        H: Handler<T, S>,
        T: 'static,
    {
        let route = protect_empty_state_change(post(handler), self.security.clone())
            .head(reject_post)
            .fallback(reject_post);
        self.inner = self.inner.route(path, route);
        self
    }

    /// Register an authenticated and CSRF-protected `POST` operation with a
    /// closed JSON request schema. Validation runs even when the handler does
    /// not extract [`super::request::ValidatedJson`].
    pub fn protected_json_post<B, H, T>(mut self, path: &'static str, handler: H) -> Self
    where
        B: DashboardRequestBody,
        H: Handler<T, S>,
        T: 'static,
    {
        let route = protect_json_state_change::<B, S>(post(handler), self.security.clone())
            .head(reject_post)
            .fallback(reject_post);
        self.inner = self.inner.route(path, route);
        self
    }

    /// Register an authenticated and CSRF-protected `PUT` operation whose
    /// `OpenAPI` operation declares no request body.
    pub fn protected_empty_put<H, T>(mut self, path: &'static str, handler: H) -> Self
    where
        H: Handler<T, S>,
        T: 'static,
    {
        let route = protect_empty_state_change(put(handler), self.security.clone())
            .head(reject_put)
            .fallback(reject_put);
        self.inner = self.inner.route(path, route);
        self
    }

    /// Register an authenticated and CSRF-protected `PUT` operation with a
    /// closed JSON request schema.
    pub fn protected_json_put<B, H, T>(mut self, path: &'static str, handler: H) -> Self
    where
        B: DashboardRequestBody,
        H: Handler<T, S>,
        T: 'static,
    {
        let route = protect_json_state_change::<B, S>(put(handler), self.security.clone())
            .head(reject_put)
            .fallback(reject_put);
        self.inner = self.inner.route(path, route);
        self
    }

    /// Register an authenticated and CSRF-protected `PATCH` operation whose
    /// `OpenAPI` operation declares no request body.
    pub fn protected_empty_patch<H, T>(mut self, path: &'static str, handler: H) -> Self
    where
        H: Handler<T, S>,
        T: 'static,
    {
        let route = protect_empty_state_change(patch(handler), self.security.clone())
            .head(reject_patch)
            .fallback(reject_patch);
        self.inner = self.inner.route(path, route);
        self
    }

    /// Register an authenticated and CSRF-protected `PATCH` operation with a
    /// closed JSON request schema.
    pub fn protected_json_patch<B, H, T>(mut self, path: &'static str, handler: H) -> Self
    where
        B: DashboardRequestBody,
        H: Handler<T, S>,
        T: 'static,
    {
        let route = protect_json_state_change::<B, S>(patch(handler), self.security.clone())
            .head(reject_patch)
            .fallback(reject_patch);
        self.inner = self.inner.route(path, route);
        self
    }

    /// Register an authenticated and CSRF-protected `DELETE` operation whose
    /// `OpenAPI` operation declares no request body.
    pub fn protected_empty_delete<H, T>(mut self, path: &'static str, handler: H) -> Self
    where
        H: Handler<T, S>,
        T: 'static,
    {
        let route = protect_empty_state_change(delete(handler), self.security.clone())
            .head(reject_delete)
            .fallback(reject_delete);
        self.inner = self.inner.route(path, route);
        self
    }

    /// Register an authenticated and CSRF-protected `DELETE` operation with a
    /// closed JSON request schema.
    pub fn protected_json_delete<B, H, T>(mut self, path: &'static str, handler: H) -> Self
    where
        B: DashboardRequestBody,
        H: Handler<T, S>,
        T: 'static,
    {
        let route = protect_json_state_change::<B, S>(delete(handler), self.security.clone())
            .head(reject_delete)
            .fallback(reject_delete);
        self.inner = self.inner.route(path, route);
        self
    }

    /// Register an authenticated `GET` and a CSRF-protected bodyless `PUT` on
    /// one path.
    pub fn protected_get_empty_put<HG, TG, HP, TP>(
        mut self,
        path: &'static str,
        get_handler: HG,
        put_handler: HP,
    ) -> Self
    where
        HG: Handler<TG, S>,
        TG: 'static,
        HP: Handler<TP, S>,
        TP: 'static,
    {
        let read = protect_read(get(get_handler), self.security.clone());
        let mutation = protect_empty_state_change(put(put_handler), self.security.clone());
        let route = read
            .merge(mutation)
            .head(reject_get_put)
            .fallback(reject_get_put);
        self.inner = self.inner.route(path, route);
        self
    }

    /// Register an authenticated `GET` and a CSRF-protected `PUT` with a closed
    /// JSON request schema on one path.
    pub fn protected_get_json_put<B, HG, TG, HP, TP>(
        mut self,
        path: &'static str,
        get_handler: HG,
        put_handler: HP,
    ) -> Self
    where
        B: DashboardRequestBody,
        HG: Handler<TG, S>,
        TG: 'static,
        HP: Handler<TP, S>,
        TP: 'static,
    {
        let read = protect_read(get(get_handler), self.security.clone());
        let mutation = protect_json_state_change::<B, S>(put(put_handler), self.security.clone());
        let route = read
            .merge(mutation)
            .head(reject_get_put)
            .fallback(reject_get_put);
        self.inner = self.inner.route(path, route);
        self
    }

    /// Supply the complete application state and install the final Host/no-CORS
    /// boundary. No route may be merged into the returned router.
    pub fn finish(self, state: S) -> DashboardRouter {
        DashboardRouter {
            inner: self.security.wrap(self.inner.with_state(state)),
            stream_shutdown: None,
        }
    }
}

fn protect_empty_state_change<S>(
    route: MethodRouter<S>,
    security: DashboardSecurity,
) -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    let route = route.route_layer(middleware::from_fn(require_empty_body));
    protect_state_change(route, security)
}

fn protect_json_state_change<B, S>(
    route: MethodRouter<S>,
    security: DashboardSecurity,
) -> MethodRouter<S>
where
    B: DashboardRequestBody,
    S: Clone + Send + Sync + 'static,
{
    let route = route.route_layer(middleware::from_fn(require_json_body::<B>));
    protect_state_change(route, security)
}

async fn reject_bootstrap_method() -> axum::response::Response {
    bootstrap_method_not_allowed("GET")
}

async fn reject_get() -> axum::response::Response {
    method_not_allowed("GET")
}

async fn reject_post() -> axum::response::Response {
    method_not_allowed("POST")
}

async fn reject_put() -> axum::response::Response {
    method_not_allowed("PUT")
}

async fn reject_patch() -> axum::response::Response {
    method_not_allowed("PATCH")
}

async fn reject_delete() -> axum::response::Response {
    method_not_allowed("DELETE")
}

async fn reject_get_put() -> axum::response::Response {
    method_not_allowed("GET, PUT")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::Body;
    use axum::http::header::{ALLOW, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, HOST, ORIGIN};
    use axum::http::{Method, Request, StatusCode};
    use serde::Deserialize;
    use tower::ServiceExt as _;

    use super::*;
    use crate::request::{MAX_DASHBOARD_JSON_BODY_BYTES, sealed};
    use crate::{AuthorizedStateChange, CSRF_HEADER_NAME, SESSION_COOKIE_NAME};

    const ADDRESS: &str = "127.0.0.1:4319";

    fn request(method: Method, path: &str) -> axum::http::Request<Body> {
        Request::builder()
            .method(method)
            .uri(path)
            .header(HOST, ADDRESS)
            .body(Body::empty())
            .unwrap()
    }

    #[derive(Clone, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct TestBody {
        enabled: bool,
    }

    impl sealed::Sealed for TestBody {}

    impl DashboardRequestBody for TestBody {
        fn validate(&self) -> Result<(), Vec<crate::ValidationIssue>> {
            let _ = self.enabled;
            Ok(())
        }
    }

    fn credentials(sessions: &DashboardSessions) -> (String, String) {
        let token = sessions.issue_bootstrap().unwrap();
        let established = sessions.consume_bootstrap(token.query_value()).unwrap();
        let authenticated = sessions
            .authenticate_session(established.cookie_value())
            .unwrap();
        (
            format!("{SESSION_COOKIE_NAME}={}", established.cookie_value()),
            authenticated.csrf_proof().to_owned(),
        )
    }

    fn authorized_request(
        method: Method,
        path: &str,
        cookie: &str,
        csrf: &str,
        body: impl Into<Body>,
    ) -> axum::http::Request<Body> {
        Request::builder()
            .method(method)
            .uri(path)
            .header(HOST, ADDRESS)
            .header(COOKIE, cookie)
            .header(ORIGIN, format!("http://{ADDRESS}"))
            .header(CSRF_HEADER_NAME, csrf)
            .body(body.into())
            .unwrap()
    }

    #[tokio::test]
    async fn every_declared_mutation_category_is_protected_and_has_an_exact_method_fallback() {
        let app = DashboardApi::new(ADDRESS.parse().unwrap(), DashboardSessions::default())
            .protected_empty_post("/post", || async { StatusCode::NO_CONTENT })
            .protected_empty_put("/put", || async { StatusCode::NO_CONTENT })
            .protected_empty_patch("/patch", || async { StatusCode::NO_CONTENT })
            .protected_empty_delete("/delete", || async { StatusCode::NO_CONTENT })
            .protected_get_json_put::<TestBody, _, _, _, _>(
                "/settings",
                || async { StatusCode::NO_CONTENT },
                || async { StatusCode::NO_CONTENT },
            )
            .finish(())
            .into_inner();

        for (method, path, allow) in [
            (Method::POST, "/post", "POST"),
            (Method::PUT, "/put", "PUT"),
            (Method::PATCH, "/patch", "PATCH"),
            (Method::DELETE, "/delete", "DELETE"),
            (Method::PUT, "/settings", "GET, PUT"),
        ] {
            let response = app.clone().oneshot(request(method, path)).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

            let response = app
                .clone()
                .oneshot(request(Method::OPTIONS, path))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
            assert_eq!(response.headers()[ALLOW], allow);

            let response = app
                .clone()
                .oneshot(request(Method::HEAD, path))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
            assert_eq!(response.headers()[ALLOW], allow);
        }
    }

    #[tokio::test]
    async fn mutation_builders_validate_declared_bodies_before_handlers_that_ignore_them() {
        let sessions = DashboardSessions::default();
        let (cookie, csrf) = credentials(&sessions);
        let calls = Arc::new(AtomicUsize::new(0));
        let json_calls = Arc::clone(&calls);
        let empty_calls = Arc::clone(&calls);
        let app = DashboardApi::new(ADDRESS.parse().unwrap(), sessions)
            .protected_json_post::<TestBody, _, _>("/json", move |_: AuthorizedStateChange| {
                let calls = Arc::clone(&json_calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    StatusCode::NO_CONTENT
                }
            })
            .protected_empty_post("/empty", move |_: AuthorizedStateChange| {
                let calls = Arc::clone(&empty_calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    StatusCode::NO_CONTENT
                }
            })
            .finish(())
            .into_inner();

        let missing_media_type =
            authorized_request(Method::POST, "/json", &cookie, &csrf, r#"{"enabled":true}"#);
        let response = app.clone().oneshot(missing_media_type).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

        let mut oversized =
            authorized_request(Method::POST, "/json", &cookie, &csrf, r#"{"enabled":true}"#);
        oversized
            .headers_mut()
            .insert(CONTENT_TYPE, "application/json".parse().unwrap());
        oversized.headers_mut().insert(
            CONTENT_LENGTH,
            (MAX_DASHBOARD_JSON_BODY_BYTES + 1)
                .to_string()
                .parse()
                .unwrap(),
        );
        let response = app.clone().oneshot(oversized).await.unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

        let mut unknown_field = authorized_request(
            Method::POST,
            "/json",
            &cookie,
            &csrf,
            r#"{"enabled":true,"secret":"must-not-reach-handler"}"#,
        );
        unknown_field
            .headers_mut()
            .insert(CONTENT_TYPE, "application/json".parse().unwrap());
        let response = app.clone().oneshot(unknown_field).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let response = app
            .clone()
            .oneshot(authorized_request(
                Method::POST,
                "/empty",
                &cookie,
                &csrf,
                "must-not-reach-handler",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        let mut valid_json =
            authorized_request(Method::POST, "/json", &cookie, &csrf, r#"{"enabled":true}"#);
        valid_json
            .headers_mut()
            .insert(CONTENT_TYPE, "application/json".parse().unwrap());
        let response = app.clone().oneshot(valid_json).await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = app
            .oneshot(authorized_request(
                Method::POST,
                "/empty",
                &cookie,
                &csrf,
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
