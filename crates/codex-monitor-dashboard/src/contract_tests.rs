use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};

use axum::body::{Body, to_bytes};
use axum::http::header::{ACCEPT_LANGUAGE, CONTENT_TYPE, COOKIE, ETAG, HOST, IF_MATCH, ORIGIN};
use axum::http::{Method, Request, StatusCode};
use codex_monitor_core::{
    AlertId, FieldId, ModelCallId, SourceContractVersion, SourceHealthStatus, SourceId,
    UtcTimestamp,
};
use serde_json::Value;
use tower::ServiceExt as _;

use crate::*;

const ADDRESS: &str = "127.0.0.1:4319";

#[derive(Default)]
struct FakeQueryPort {
    model_call_requests: Mutex<Vec<DashboardModelCallPageRequest>>,
}

impl DashboardQueryPort for FakeQueryPort {
    fn resolve_conversation_identifiers(
        &self,
        display_name: &str,
    ) -> Result<Vec<String>, DashboardQueryErrorCode> {
        Ok(vec![display_name.to_owned()])
    }

    fn model_call_page(
        &self,
        request: DashboardModelCallPageRequest,
    ) -> Result<DashboardPage<DashboardModelCall, DashboardModelCallCursor>, DashboardQueryErrorCode>
    {
        let limit = request.input.limit;
        self.model_call_requests.lock().unwrap().push(request);
        Ok(DashboardPage {
            items: Vec::new(),
            limit,
            has_more: false,
            next_cursor: None,
        })
    }

    fn conversation_page(
        &self,
        _request: DashboardConversationPageRequest,
    ) -> Result<
        DashboardPage<DashboardConversation, DashboardConversationCursor>,
        DashboardQueryErrorCode,
    > {
        panic!("unexpected conversation query")
    }

    fn model_call_summary(
        &self,
        _request: DashboardSummaryRequest,
    ) -> Result<DashboardModelCallSummary, DashboardQueryErrorCode> {
        panic!("unexpected summary query")
    }

    fn source_health_list(&self) -> Result<Vec<DashboardSourceHealth>, DashboardQueryErrorCode> {
        panic!("unexpected source query")
    }

    fn alert_page(
        &self,
        _request: DashboardAlertPageRequest,
    ) -> Result<DashboardPage<DashboardAlert, DashboardAlertCursor>, DashboardQueryErrorCode> {
        panic!("unexpected alert query")
    }

    fn model_call(
        &self,
        _id: ModelCallId,
    ) -> Result<Option<DashboardModelCall>, DashboardQueryErrorCode> {
        panic!("unexpected model-call materialization")
    }

    fn alert(&self, _id: AlertId) -> Result<Option<DashboardAlert>, DashboardQueryErrorCode> {
        panic!("unexpected alert materialization")
    }

    fn rule_capabilities(&self) -> Result<Vec<DashboardRuleCapability>, DashboardQueryErrorCode> {
        Ok(Vec::new())
    }
}

struct FakeSettingsPort {
    document: DashboardSettingsDocument,
    detected_locales: Mutex<Vec<DashboardLocale>>,
    replacements: Mutex<Vec<(u64, DashboardSettings)>>,
}

impl DashboardSettingsPort for FakeSettingsPort {
    fn initialize_locale(
        &self,
        detected_locale: DashboardLocale,
    ) -> DashboardFuture<'_, Result<DashboardSettingsDocument, DashboardSettingsErrorCode>> {
        self.detected_locales.lock().unwrap().push(detected_locale);
        let document = self.document.clone();
        Box::pin(async move { Ok(document) })
    }

    fn replace(
        &self,
        expected_revision: u64,
        replacement: DashboardSettings,
    ) -> DashboardFuture<'_, Result<DashboardSettingsDocument, DashboardSettingsErrorCode>> {
        self.replacements
            .lock()
            .unwrap()
            .push((expected_revision, replacement.clone()));
        Box::pin(async move {
            Ok(DashboardSettingsDocument {
                revision: expected_revision + 1,
                settings: replacement,
            })
        })
    }
}

#[derive(Clone)]
struct FakeCapabilityPort {
    snapshot: DashboardRuntimeSnapshot,
}

impl DashboardCapabilityPort for FakeCapabilityPort {
    fn snapshot(&self) -> DashboardRuntimeSnapshot {
        self.snapshot.clone()
    }

    fn retry_source(
        &self,
        _source_id: SourceId,
    ) -> Result<DashboardSourceHealth, DashboardRetryErrorCode> {
        Err(DashboardRetryErrorCode::Unsupported)
    }
}

#[derive(Default)]
struct FakeEventPort {
    after_event_ids: Mutex<Vec<Option<String>>>,
}

impl DashboardEventPort for FakeEventPort {
    fn current_cursor(&self) -> String {
        "event-current".to_owned()
    }

    fn subscribe(
        &self,
        after_event_id: Option<&str>,
    ) -> Result<DashboardEventSubscriptionStart, DashboardEventSubscribeErrorCode> {
        self.after_event_ids
            .lock()
            .unwrap()
            .push(after_event_id.map(str::to_owned));
        Ok(DashboardEventSubscriptionStart::Control(
            DashboardStreamControlReason::MissedEvent,
        ))
    }
}

struct Harness {
    app: axum::Router,
    cookie: String,
    csrf: String,
    queries: Arc<FakeQueryPort>,
    settings: Arc<FakeSettingsPort>,
    events: Arc<FakeEventPort>,
}

fn at(seconds: i128) -> UtcTimestamp {
    UtcTimestamp::from_unix_timestamp_nanos(seconds * 1_000_000_000).unwrap()
}

fn source_health() -> DashboardSourceHealth {
    DashboardSourceHealth {
        source_id: SourceId::new("otlp-logs").unwrap(),
        source_contract_version: SourceContractVersion::new(1).unwrap(),
        status: SourceHealthStatus::Healthy,
        receiver: Some(DashboardReceiverSnapshot {
            port: 4318,
            bind_state: DashboardReceiverBindState::Bound,
            retry_supported: true,
        }),
        last_accepted_at: Some(at(4)),
        last_error_code: None,
        last_error_occurred_at: None,
        updated_at: at(5),
        supported_fields: vec![FieldId::new("model_call.input_tokens").unwrap()],
    }
}

fn settings_document() -> DashboardSettingsDocument {
    DashboardSettingsDocument {
        revision: 7,
        settings: DashboardSettings {
            retention: DashboardRetentionPolicy::Days90,
            notifications_enabled: true,
            notify_max_n: NonZeroU32::new(1),
            notify_all: false,
            locale: DashboardLocale::EnUs,
            login_autostart: false,
            updated_at: at(6),
        },
    }
}

fn harness() -> Harness {
    let sessions = DashboardSessions::default();
    let token = sessions.issue_bootstrap().unwrap();
    let established = sessions.consume_bootstrap(token.query_value()).unwrap();
    let authenticated = sessions
        .authenticate_session(established.cookie_value())
        .unwrap();
    let cookie = format!("{SESSION_COOKIE_NAME}={}", established.cookie_value());
    let csrf = authenticated.csrf_proof().to_owned();

    let queries = Arc::new(FakeQueryPort::default());
    let settings = Arc::new(FakeSettingsPort {
        document: settings_document(),
        detected_locales: Mutex::new(Vec::new()),
        replacements: Mutex::new(Vec::new()),
    });
    let source = source_health();
    let receiver = source.receiver.unwrap();
    let capabilities = Arc::new(FakeCapabilityPort {
        snapshot: DashboardRuntimeSnapshot {
            source,
            checkpoint_capability: DashboardCheckpointCapability::None,
            receiver,
        },
    });
    let events = Arc::new(FakeEventPort::default());
    let assets = DashboardAssets::new([DashboardAssetInput::new(
        "index.html",
        b"<!doctype html><html><body><div id=\"root\"></div></body></html>".to_vec(),
        "text/html",
    )])
    .unwrap();
    let state = DashboardState::new(
        assets,
        sessions,
        Arc::clone(&queries) as Arc<dyn DashboardQueryPort>,
        Arc::clone(&settings) as Arc<dyn DashboardSettingsPort>,
        capabilities as Arc<dyn DashboardCapabilityPort>,
        Arc::clone(&events) as Arc<dyn DashboardEventPort>,
        DashboardPlatformCapabilities::new(
            true,
            SystemNotificationAvailability::Available,
            TrayAvailability::Available,
        ),
    );
    let app = dashboard_router(ADDRESS.parse().unwrap(), state).into_inner();

    Harness {
        app,
        cookie,
        csrf,
        queries,
        settings,
        events,
    }
}

fn authorized_request(method: Method, path: &str, harness: &Harness) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header(HOST, ADDRESS)
        .header(COOKIE, &harness.cookie)
        .body(Body::empty())
        .unwrap()
}

async fn json_body(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn rest_query_is_validated_and_dispatched_through_the_query_port() {
    let harness = harness();
    let response = harness
        .app
        .clone()
        .oneshot(authorized_request(
            Method::GET,
            "/api/v1/model-calls?limit=2&model=gpt-5.5&sort=event_at_asc",
            &harness,
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["items"], serde_json::json!([]));
    assert_eq!(body["page"]["limit"], 2);
    assert_eq!(body["page"]["has_more"], false);
    let requests = harness.queries.model_call_requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].input.limit, 2);
    assert_eq!(
        requests[0].input.filters.model.as_ref().unwrap().as_str(),
        "gpt-5.5"
    );
    assert_eq!(requests[0].input.sort, ModelCallSortInput::EventAtAsc);
}

#[tokio::test]
async fn settings_and_capabilities_use_consumer_owned_ports() {
    let harness = harness();
    let mut get = authorized_request(Method::GET, "/api/v1/settings", &harness);
    get.headers_mut()
        .insert(ACCEPT_LANGUAGE, "zh-CN, en-US;q=0.5".parse().unwrap());
    let response = harness.app.clone().oneshot(get).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[ETAG], "\"7\"");
    let body = json_body(response).await;
    assert_eq!(body["revision"], "7");
    assert_eq!(body["settings"]["language"], "en-US");
    assert_eq!(
        harness.settings.detected_locales.lock().unwrap().as_slice(),
        &[DashboardLocale::ZhCn]
    );

    let response = harness
        .app
        .clone()
        .oneshot(authorized_request(
            Method::GET,
            "/api/v1/capabilities",
            &harness,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["api_version"], "v1");
    assert_eq!(body["sources"][0]["source_id"], "otlp-logs");
    assert_eq!(body["sources"][0]["supports_retry"], true);
    assert_eq!(body["platform"]["system_notifications"], "available");

    let mut put = authorized_request(Method::PUT, "/api/v1/settings", &harness);
    put.headers_mut()
        .insert(ORIGIN, format!("http://{ADDRESS}").parse().unwrap());
    put.headers_mut()
        .insert(CSRF_HEADER_NAME, harness.csrf.parse().unwrap());
    put.headers_mut()
        .insert(CONTENT_TYPE, "application/json".parse().unwrap());
    put.headers_mut().insert(IF_MATCH, "\"7\"".parse().unwrap());
    *put.body_mut() = Body::from(
        r#"{"login_autostart":true,"system_notifications":false,"notify_max_n":null,"retention_days":30,"language":"zh-CN"}"#,
    );
    let response = harness.app.clone().oneshot(put).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[ETAG], "\"8\"");
    let replacements = harness.settings.replacements.lock().unwrap();
    assert_eq!(replacements.len(), 1);
    assert_eq!(replacements[0].0, 7);
    assert!(replacements[0].1.login_autostart);
    assert!(!replacements[0].1.notifications_enabled);
    assert!(replacements[0].1.notify_all);
    assert_eq!(replacements[0].1.locale, DashboardLocale::ZhCn);
}

#[tokio::test]
async fn sse_control_event_preserves_the_resume_cursor_and_requires_resync() {
    let harness = harness();
    let response = harness
        .app
        .clone()
        .oneshot(authorized_request(
            Method::GET,
            "/api/v1/events?after_event_id=event-stale",
            &harness,
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()[CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains("event: stream-control"));
    assert!(body.contains("\"event_id\":\"event-current\""));
    assert!(body.contains("\"reason\":\"missed_event\""));
    assert_eq!(
        harness.events.after_event_ids.lock().unwrap().as_slice(),
        &[Some("event-stale".to_owned())]
    );
}
