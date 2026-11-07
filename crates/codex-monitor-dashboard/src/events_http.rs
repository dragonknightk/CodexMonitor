use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{RawQuery, State};
use axum::http::header::CACHE_CONTROL;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use codex_monitor_core::{SourceHealthStatus, UtcTimestamp};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tokio::time::MissedTickBehavior;
use tokio_stream::{StreamExt as _, wrappers::ReceiverStream};

use crate::api_error::{ApiErrorCode, CursorReason, api_error, invalid_cursor};
use crate::ports::{
    DashboardEventChange, DashboardEventReceive, DashboardEventRecord,
    DashboardEventSubscribeErrorCode, DashboardEventSubscriptionStart, DashboardReceiverBindState,
    DashboardStreamControlReason,
};
use crate::query_http::decode_query;
use crate::query_response::{
    AlertResponse, ModelCallResponse, SourceHealthResponse, alert, model_call, source_health,
};
use crate::session::AuthenticatedSession;
use crate::settings_http::{SettingsDocumentResponse, settings_document};
use crate::state::DashboardState;

const EVENT_FIELDS: &[(&str, crate::ValidationField)] =
    &[("after_event_id", crate::ValidationField::AfterEventId)];
const SSE_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(15);
#[cfg(not(test))]
const SESSION_REVALIDATION_INTERVAL: Duration = Duration::from_secs(30);
#[cfg(test)]
const SESSION_REVALIDATION_INTERVAL: Duration = Duration::from_millis(20);
const X_ACCEL_BUFFERING: HeaderName = HeaderName::from_static("x-accel-buffering");

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventQuery {
    after_event_id: Option<String>,
}

#[derive(Serialize)]
struct HealthSnapshot {
    api_version: &'static str,
    status: HealthStatus,
    dashboard_listener: ListenerHealth,
    stream_cursor: String,
    checked_at: UtcTimestamp,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum HealthStatus {
    Starting,
    Ready,
    Degraded,
}

#[derive(Serialize)]
struct ListenerHealth {
    host: &'static str,
    port: u16,
    state: ListenerState,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum ListenerState {
    Bound,
}

#[derive(Serialize)]
struct ModelCallEvent {
    event_id: String,
    emitted_at: UtcTimestamp,
    model_call: ModelCallResponse,
}

#[derive(Serialize)]
struct SourceHealthEvent {
    event_id: String,
    emitted_at: UtcTimestamp,
    source: SourceHealthResponse,
}

#[derive(Serialize)]
struct AlertEvent {
    event_id: String,
    emitted_at: UtcTimestamp,
    alert: AlertResponse,
}

#[derive(Serialize)]
struct SettingsEvent {
    event_id: String,
    emitted_at: UtcTimestamp,
    document: SettingsDocumentResponse,
}

#[derive(Serialize)]
struct StreamControlEvent {
    event_id: String,
    emitted_at: UtcTimestamp,
    reason: StreamControlReasonResponse,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum StreamControlReasonResponse {
    Overflow,
    MissedEvent,
    ServerRestart,
}

struct SerializedEvent {
    name: &'static str,
    data: String,
}

pub(crate) async fn get_health(State(state): State<DashboardState>) -> Response {
    let Some(listener_address) = state.listener_address else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            ApiErrorCode::InternalUnavailable,
            None,
        );
    };
    let runtime = state.capabilities.snapshot();
    let status = match (runtime.receiver.bind_state, runtime.source.status) {
        (DashboardReceiverBindState::Starting, _) | (_, SourceHealthStatus::Starting) => {
            HealthStatus::Starting
        }
        (DashboardReceiverBindState::Bound, SourceHealthStatus::Healthy) => HealthStatus::Ready,
        _ => HealthStatus::Degraded,
    };
    let mut response = Json(HealthSnapshot {
        api_version: "v1",
        status,
        dashboard_listener: ListenerHealth {
            host: "127.0.0.1",
            port: listener_address.port(),
            state: ListenerState::Bound,
        },
        stream_cursor: state.events.current_cursor(),
        checked_at: UtcTimestamp::now(),
    })
    .into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

pub(crate) async fn stream_events(
    State(state): State<DashboardState>,
    session: AuthenticatedSession,
    raw_query: RawQuery,
) -> Response {
    let query = match decode_query::<EventQuery>(raw_query, EVENT_FIELDS) {
        Ok(query) => query,
        Err(error) => return error.into_response(),
    };
    let start = match state.events.subscribe(query.after_event_id.as_deref()) {
        Ok(start) => start,
        Err(DashboardEventSubscribeErrorCode::Malformed) => {
            return invalid_cursor(CursorReason::Malformed);
        }
        Err(DashboardEventSubscribeErrorCode::VersionUnsupported) => {
            return invalid_cursor(CursorReason::VersionUnsupported);
        }
        Err(DashboardEventSubscribeErrorCode::Capacity) => {
            return api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                ApiErrorCode::InternalUnavailable,
                None,
            );
        }
    };

    let shutdown = state.stream_shutdown();
    let response_shutdown = shutdown.clone();
    let response_revocation = session.revocation();
    let response_deadline = session.monotonic_deadline();
    let (sender, receiver) = mpsc::channel::<Result<Event, Infallible>>(1);
    tokio::spawn(run_stream(state, session, start, shutdown, sender));
    let authenticated_receiver = ReceiverStream::new(receiver).filter_map(move |event| {
        (!stream_cancelled(&response_shutdown, &response_revocation, response_deadline))
            .then_some(event)
    });
    let mut response = Sse::new(authenticated_receiver)
        .keep_alive(
            KeepAlive::new()
                .interval(SSE_KEEP_ALIVE_INTERVAL)
                .text("keep-alive"),
        )
        .into_response();
    let headers = response.headers_mut();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(X_ACCEL_BUFFERING, HeaderValue::from_static("no"));
    response
}

async fn run_stream(
    state: DashboardState,
    session: AuthenticatedSession,
    start: DashboardEventSubscriptionStart,
    mut shutdown: watch::Receiver<bool>,
    sender: mpsc::Sender<Result<Event, Infallible>>,
) {
    let expiration =
        tokio::time::sleep_until(tokio::time::Instant::from_std(session.monotonic_deadline()));
    tokio::pin!(expiration);
    let mut revocation = session.revocation();
    if stream_cancelled(&shutdown, &revocation, session.monotonic_deadline()) {
        return;
    }
    let mut subscription = match start {
        DashboardEventSubscriptionStart::Active(subscription) => subscription,
        DashboardEventSubscriptionStart::Control(reason) => {
            let _ = send_control_event(
                &state,
                reason,
                &sender,
                &mut shutdown,
                &mut revocation,
                expiration.as_mut(),
            )
            .await;
            return;
        }
    };

    let mut session_revalidation = tokio::time::interval(SESSION_REVALIDATION_INTERVAL);
    session_revalidation.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            biased;
            () = sender.closed() => return,
            () = &mut expiration => return,
            changed = revocation.changed() => {
                if changed.is_err() || *revocation.borrow() {
                    return;
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
            _ = session_revalidation.tick() => {
                if !state.sessions.is_current_session(&session).unwrap_or(false) {
                    return;
                }
            }
            received = subscription.recv() => {
                if !forward_stream_receive(
                    &state,
                    received,
                    &sender,
                    &mut shutdown,
                    &mut revocation,
                    expiration.as_mut(),
                )
                .await
                {
                    return;
                }
            }
        }
    }
}

async fn forward_stream_receive(
    state: &DashboardState,
    received: DashboardEventReceive,
    sender: &mpsc::Sender<Result<Event, Infallible>>,
    shutdown: &mut watch::Receiver<bool>,
    revocation: &mut watch::Receiver<bool>,
    mut expiration: Pin<&mut tokio::time::Sleep>,
) -> bool {
    match received {
        DashboardEventReceive::Change(record) => {
            if matches!(&record.change, DashboardEventChange::ResyncRequired) {
                let _ = send_control_event(
                    state,
                    DashboardStreamControlReason::MissedEvent,
                    sender,
                    shutdown,
                    revocation,
                    expiration,
                )
                .await;
                return false;
            }
            if *shutdown.borrow() || *revocation.borrow() {
                return false;
            }
            let materialization = materialize_record(state.clone(), record);
            tokio::pin!(materialization);
            let materialized = tokio::select! {
                biased;
                () = &mut expiration => return false,
                _ = revocation.changed() => return false,
                _ = shutdown.changed() => return false,
                event = &mut materialization => event,
            };
            let Ok(event) = materialized else {
                let _ = send_control_event(
                    state,
                    DashboardStreamControlReason::MissedEvent,
                    sender,
                    shutdown,
                    revocation,
                    expiration.as_mut(),
                )
                .await;
                return false;
            };
            let event = Event::default().event(event.name).data(event.data);
            send_event(event, sender, shutdown, revocation, expiration.as_mut()).await
        }
        DashboardEventReceive::Overflow => {
            let _ = send_control_event(
                state,
                DashboardStreamControlReason::Overflow,
                sender,
                shutdown,
                revocation,
                expiration,
            )
            .await;
            false
        }
        DashboardEventReceive::Closed => false,
    }
}

async fn materialize_record(
    state: DashboardState,
    record: Arc<DashboardEventRecord>,
) -> Result<SerializedEvent, ()> {
    let permit = Arc::clone(&state.event_materializations)
        .acquire_owned()
        .await
        .map_err(|_| ())?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        materialize_record_blocking(&state, &record)
    })
    .await
    .map_err(|_| ())?
}

fn materialize_record_blocking(
    state: &DashboardState,
    record: &DashboardEventRecord,
) -> Result<SerializedEvent, ()> {
    match &record.change {
        DashboardEventChange::ModelCall(id) => {
            let value = state.queries.model_call(*id).map_err(|_| ())?.ok_or(())?;
            serialize_event(
                "model-call",
                &ModelCallEvent {
                    event_id: record.event_id.clone(),
                    emitted_at: record.emitted_at,
                    model_call: model_call(value),
                },
            )
        }
        DashboardEventChange::SourceHealth {
            source_id,
            source_contract_version,
        } => {
            let value = state
                .queries
                .source_health_list()
                .map_err(|_| ())?
                .into_iter()
                .find(|value| {
                    value.source_id == *source_id
                        && value.source_contract_version == *source_contract_version
                })
                .ok_or(())?;
            serialize_event(
                "source-health",
                &SourceHealthEvent {
                    event_id: record.event_id.clone(),
                    emitted_at: record.emitted_at,
                    source: source_health(value),
                },
            )
        }
        DashboardEventChange::Alert(id) => {
            let value = state.queries.alert(*id).map_err(|_| ())?.ok_or(())?;
            serialize_event(
                "alert",
                &AlertEvent {
                    event_id: record.event_id.clone(),
                    emitted_at: record.emitted_at,
                    alert: alert(value),
                },
            )
        }
        DashboardEventChange::Settings(document) => serialize_event(
            "settings",
            &SettingsEvent {
                event_id: record.event_id.clone(),
                emitted_at: record.emitted_at,
                document: settings_document(document.clone()),
            },
        ),
        DashboardEventChange::ResyncRequired => Err(()),
    }
}

fn serialize_event(name: &'static str, value: &impl Serialize) -> Result<SerializedEvent, ()> {
    Ok(SerializedEvent {
        name,
        data: serde_json::to_string(value).map_err(|_| ())?,
    })
}

fn control_event(state: &DashboardState, reason: DashboardStreamControlReason) -> Option<Event> {
    let reason = match reason {
        DashboardStreamControlReason::Overflow => StreamControlReasonResponse::Overflow,
        DashboardStreamControlReason::MissedEvent => StreamControlReasonResponse::MissedEvent,
        DashboardStreamControlReason::ServerRestart => StreamControlReasonResponse::ServerRestart,
    };
    let data = serde_json::to_string(&StreamControlEvent {
        event_id: state.events.current_cursor(),
        emitted_at: UtcTimestamp::now(),
        reason,
    })
    .ok()?;
    Some(Event::default().event("stream-control").data(data))
}

async fn send_control_event(
    state: &DashboardState,
    reason: DashboardStreamControlReason,
    sender: &mpsc::Sender<Result<Event, Infallible>>,
    shutdown: &mut watch::Receiver<bool>,
    revocation: &mut watch::Receiver<bool>,
    expiration: Pin<&mut tokio::time::Sleep>,
) -> bool {
    let Some(event) = control_event(state, reason) else {
        return false;
    };
    send_event(event, sender, shutdown, revocation, expiration).await
}

async fn send_event(
    event: Event,
    sender: &mpsc::Sender<Result<Event, Infallible>>,
    shutdown: &mut watch::Receiver<bool>,
    revocation: &mut watch::Receiver<bool>,
    mut expiration: Pin<&mut tokio::time::Sleep>,
) -> bool {
    if *shutdown.borrow() || *revocation.borrow() {
        return false;
    }
    tokio::select! {
        biased;
        () = &mut expiration => false,
        _ = revocation.changed() => false,
        _ = shutdown.changed() => false,
        result = sender.send(Ok(event)) => result.is_ok(),
    }
}

fn stream_cancelled(
    shutdown: &watch::Receiver<bool>,
    revocation: &watch::Receiver<bool>,
    deadline: Instant,
) -> bool {
    *shutdown.borrow() || *revocation.borrow() || Instant::now() >= deadline
}
