use std::future::IntoFuture as _;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use codex_monitor_core::{SafeErrorCode, UtcTimestamp};
use tokio::net::TcpListener;
use tokio::runtime::Handle;
use tokio::sync::{Notify, watch};
use tokio::task::{JoinError, JoinHandle};
use tokio::time::{Instant, MissedTickBehavior};

use crate::diagnostics::StructuredDiagnosticLog;

use super::{ObservationSinkError, OtlpObservationProcessor, OtlpRequestLimits, otlp_http_router};

pub const OTLP_RECEIVER_ADDRESS: SocketAddr =
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 4318));
const DEFAULT_BIND_RETRY_INTERVAL: Duration = Duration::from_secs(5);
const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// Current bind phase of the resident OTLP receiver.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OtlpReceiverBindState {
    Starting,
    Bound,
    Retrying,
    Conflict,
    Stopped,
}

/// Privacy-safe, process-local receiver diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OtlpReceiverSnapshot {
    address: SocketAddr,
    bind_state: OtlpReceiverBindState,
}

impl OtlpReceiverSnapshot {
    #[cfg(test)]
    pub(crate) const fn new(address: SocketAddr, bind_state: OtlpReceiverBindState) -> Self {
        Self {
            address,
            bind_state,
        }
    }

    #[must_use]
    pub const fn address(self) -> SocketAddr {
        self.address
    }

    #[must_use]
    pub const fn bind_state(self) -> OtlpReceiverBindState {
        self.bind_state
    }

    #[must_use]
    pub const fn retry_supported(self) -> bool {
        true
    }
}

/// Cloneable observer for the receiver's live bind state.
#[derive(Clone)]
pub struct OtlpReceiverMonitor {
    state: watch::Receiver<OtlpReceiverSnapshot>,
}

/// Controlled publisher paired with a real receiver monitor in integration tests.
#[cfg(feature = "test-support")]
pub struct OtlpReceiverTestController {
    address: SocketAddr,
    state: watch::Sender<OtlpReceiverSnapshot>,
}

impl OtlpReceiverMonitor {
    #[must_use]
    pub fn snapshot(&self) -> OtlpReceiverSnapshot {
        *self.state.borrow()
    }

    /// Wait for the receiver bind state to change.
    ///
    /// Returns `None` after the receiver owner has been dropped.
    pub async fn changed(&mut self) -> Option<OtlpReceiverSnapshot> {
        self.state.changed().await.ok()?;
        Some(self.snapshot())
    }

    /// Create a real monitor backed by a controlled test publisher.
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn test_channel(
        address: SocketAddr,
        bind_state: OtlpReceiverBindState,
    ) -> (OtlpReceiverTestController, Self) {
        let (state, receiver) = watch::channel(OtlpReceiverSnapshot {
            address,
            bind_state,
        });
        (
            OtlpReceiverTestController { address, state },
            Self { state: receiver },
        )
    }

    #[cfg(test)]
    pub(crate) fn from_snapshot(snapshot: OtlpReceiverSnapshot) -> Self {
        let (sender, state) = watch::channel(snapshot);
        drop(sender);
        Self { state }
    }
}

#[cfg(feature = "test-support")]
impl OtlpReceiverTestController {
    /// Publish the next receiver bind state to the paired monitor.
    pub fn set_bind_state(&self, bind_state: OtlpReceiverBindState) {
        self.state.send_replace(OtlpReceiverSnapshot {
            address: self.address,
            bind_state,
        });
    }
}

/// Cloneable control surface for observing and explicitly retrying the fixed
/// OTLP receiver binding.
#[derive(Clone)]
pub struct OtlpReceiverControl {
    monitor: OtlpReceiverMonitor,
    retry_bind: Arc<Notify>,
}

/// Closed reason an explicit receiver retry cannot be accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum OtlpReceiverRetryError {
    #[error("the OTLP receiver is not waiting for a bind retry")]
    InvalidState,
}

impl OtlpReceiverControl {
    /// Return the latest live receiver state.
    #[must_use]
    pub fn snapshot(&self) -> OtlpReceiverSnapshot {
        self.monitor.snapshot()
    }

    /// Wake a receiver that is waiting after a failed bind attempt.
    ///
    /// # Errors
    ///
    /// Returns [`OtlpReceiverRetryError::InvalidState`] unless the receiver is
    /// currently waiting after a bind failure.
    pub fn request_bind_retry(&self) -> Result<OtlpReceiverSnapshot, OtlpReceiverRetryError> {
        let snapshot = self.snapshot();
        if !matches!(
            snapshot.bind_state(),
            OtlpReceiverBindState::Retrying | OtlpReceiverBindState::Conflict
        ) {
            return Err(OtlpReceiverRetryError::InvalidState);
        }
        self.retry_bind.notify_one();
        Ok(snapshot)
    }
}

/// Cancellation and immediate-retry control for the resident OTLP receiver.
pub struct OtlpReceiverHandle {
    processor: Arc<OtlpObservationProcessor>,
    shutdown: watch::Sender<bool>,
    receiver_state: watch::Sender<OtlpReceiverSnapshot>,
    retry_bind: Arc<Notify>,
    task: Option<JoinHandle<Result<(), OtlpReceiverRunError>>>,
    shutdown_started: bool,
}

/// Failure while joining the receiver or persisting its final stopped state.
#[derive(Debug, thiserror::Error)]
pub enum OtlpReceiverShutdownError {
    #[error("OTLP receiver task stopped unexpectedly")]
    Task(#[source] JoinError),
    #[error("OTLP receiver stopped but source health could not be persisted")]
    SourceHealth(#[source] ObservationSinkError),
    #[error("an OTLP processing task stopped unexpectedly")]
    ProcessingTask(#[source] JoinError),
    #[error("OTLP receiver exceeded its shutdown drain deadline")]
    DrainTimedOut,
}

#[derive(Debug, thiserror::Error)]
enum OtlpReceiverRunError {
    #[error("OTLP receiver source health could not be persisted")]
    SourceHealth(#[from] ObservationSinkError),
    #[error("an OTLP processing task stopped unexpectedly")]
    ProcessingTask(#[source] JoinError),
}

/// Failure to start the receiver task without an active Tokio runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum OtlpReceiverStartError {
    #[error("OTLP receiver requires an active Tokio runtime")]
    RuntimeUnavailable,
}

impl OtlpReceiverHandle {
    /// Return a cloneable observer for live Dashboard and tray diagnostics.
    #[must_use]
    pub fn monitor(&self) -> OtlpReceiverMonitor {
        OtlpReceiverMonitor {
            state: self.receiver_state.subscribe(),
        }
    }

    /// Return a cloneable Dashboard control surface for receiver diagnostics
    /// and explicit bind retries.
    #[must_use]
    pub fn control(&self) -> OtlpReceiverControl {
        OtlpReceiverControl {
            monitor: self.monitor(),
            retry_bind: Arc::clone(&self.retry_bind),
        }
    }

    /// Stop accepting requests, persist stopped source health, and join the
    /// receiver task.
    ///
    /// # Errors
    ///
    /// Returns an error if a supervised task failed, final source health could
    /// not be persisted, or the drain deadline elapsed. A deadline error keeps
    /// the receiver task owned by this handle, so callers may invoke shutdown
    /// again while a higher-level lifecycle applies its overall deadline.
    pub async fn shutdown(&mut self) -> Result<(), OtlpReceiverShutdownError> {
        self.shutdown_with_timeout(SHUTDOWN_DRAIN_TIMEOUT).await
    }

    pub fn begin_shutdown(&mut self) {
        if self.shutdown_started {
            return;
        }
        self.shutdown_started = true;
        let _ = self.shutdown.send(true);
        self.retry_bind.notify_one();
    }

    async fn shutdown_with_timeout(
        &mut self,
        drain_timeout: Duration,
    ) -> Result<(), OtlpReceiverShutdownError> {
        let deadline = Instant::now() + drain_timeout;
        self.begin_shutdown();
        if tokio::time::timeout_at(deadline, self.processor.close_intake())
            .await
            .is_err()
        {
            return Err(OtlpReceiverShutdownError::DrainTimedOut);
        }
        let Some(task) = self.task.as_mut() else {
            return Ok(());
        };
        let Ok(result) = tokio::time::timeout_at(deadline, task).await else {
            return Err(OtlpReceiverShutdownError::DrainTimedOut);
        };
        self.task.take();
        match result.map_err(OtlpReceiverShutdownError::Task)? {
            Ok(()) => Ok(()),
            Err(OtlpReceiverRunError::SourceHealth(error)) => {
                Err(OtlpReceiverShutdownError::SourceHealth(error))
            }
            Err(OtlpReceiverRunError::ProcessingTask(error)) => {
                Err(OtlpReceiverShutdownError::ProcessingTask(error))
            }
        }
    }

    pub async fn abort_and_join_before(&mut self, deadline: Instant) {
        self.begin_shutdown();
        let Some(task) = self.task.as_mut() else {
            return;
        };
        task.abort();
        if tokio::time::timeout_at(deadline, task).await.is_ok() {
            self.task.take();
        }
    }
}

impl Drop for OtlpReceiverHandle {
    fn drop(&mut self) {
        self.begin_shutdown();
        if let Some(task) = self.task.as_ref() {
            task.abort();
        }
    }
}

/// Spawn the fixed loopback OTLP receiver.
///
/// Bind failures are recorded as unavailable source health and retried in the
/// background; they never escape this boundary or terminate the resident host.
///
/// # Errors
///
/// Returns [`OtlpReceiverStartError::RuntimeUnavailable`] when called outside
/// an active Tokio runtime.
#[must_use = "the handle controls retry and ordered receiver shutdown"]
pub fn spawn_otlp_receiver(
    processor: Arc<OtlpObservationProcessor>,
    limits: OtlpRequestLimits,
    diagnostics: StructuredDiagnosticLog,
) -> Result<OtlpReceiverHandle, OtlpReceiverStartError> {
    spawn_otlp_receiver_at(
        processor,
        limits,
        diagnostics,
        OTLP_RECEIVER_ADDRESS,
        DEFAULT_BIND_RETRY_INTERVAL,
    )
}

fn spawn_otlp_receiver_at(
    processor: Arc<OtlpObservationProcessor>,
    limits: OtlpRequestLimits,
    diagnostics: StructuredDiagnosticLog,
    address: SocketAddr,
    retry_interval: Duration,
) -> Result<OtlpReceiverHandle, OtlpReceiverStartError> {
    debug_assert!(address.ip().is_loopback());
    let runtime = Handle::try_current().map_err(|_| OtlpReceiverStartError::RuntimeUnavailable)?;
    let (shutdown, shutdown_receiver) = watch::channel(false);
    let initial_state = OtlpReceiverSnapshot {
        address,
        bind_state: OtlpReceiverBindState::Starting,
    };
    let (receiver_state, receiver_state_observer) = watch::channel(initial_state);
    drop(receiver_state_observer);
    let retry_bind = Arc::new(Notify::new());
    let task = runtime.spawn(run_receiver(
        ReceiverTaskContext {
            processor: Arc::clone(&processor),
            limits,
            diagnostics,
            address,
            retry_interval,
            retry_bind: Arc::clone(&retry_bind),
            receiver_state: receiver_state.clone(),
        },
        shutdown_receiver,
    ));
    Ok(OtlpReceiverHandle {
        processor,
        shutdown,
        receiver_state,
        retry_bind,
        task: Some(task),
        shutdown_started: false,
    })
}

struct ReceiverTaskContext {
    processor: Arc<OtlpObservationProcessor>,
    limits: OtlpRequestLimits,
    diagnostics: StructuredDiagnosticLog,
    address: SocketAddr,
    retry_interval: Duration,
    retry_bind: Arc<Notify>,
    receiver_state: watch::Sender<OtlpReceiverSnapshot>,
}

async fn run_receiver(
    context: ReceiverTaskContext,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), OtlpReceiverRunError> {
    let ReceiverTaskContext {
        processor,
        limits,
        diagnostics,
        address,
        retry_interval,
        receiver_state,
        retry_bind,
    } = context;
    while !shutdown_requested(&shutdown) {
        match TcpListener::bind(address).await {
            Ok(listener) => {
                update_receiver_state(&receiver_state, address, OtlpReceiverBindState::Bound);
                let _ = processor.mark_receiver_available(UtcTimestamp::now()).await;
                let server = serve_receiver(
                    listener,
                    limits,
                    Arc::clone(&processor),
                    diagnostics.clone(),
                    shutdown.clone(),
                    retry_interval,
                )
                .await;
                if server.is_ok() {
                    break;
                }
                update_receiver_state(&receiver_state, address, OtlpReceiverBindState::Retrying);
                let _ = processor
                    .mark_receiver_unavailable(SafeErrorCode::OtlpBindFailed, UtcTimestamp::now())
                    .await;
            }
            Err(error) => {
                let bind_state = if error.kind() == std::io::ErrorKind::AddrInUse {
                    OtlpReceiverBindState::Conflict
                } else {
                    OtlpReceiverBindState::Retrying
                };
                update_receiver_state(&receiver_state, address, bind_state);
                let _ = processor
                    .mark_receiver_unavailable(SafeErrorCode::OtlpBindFailed, UtcTimestamp::now())
                    .await;
            }
        }

        if wait_before_rebind(&mut shutdown, &retry_bind, retry_interval).await {
            break;
        }
    }

    processor.close_intake().await;
    let processing_error = processor.drain_processing().await.err();
    update_receiver_state(&receiver_state, address, OtlpReceiverBindState::Stopped);
    let stopped_result = processor.mark_receiver_stopped(UtcTimestamp::now()).await;
    if let Some(error) = processing_error {
        return Err(OtlpReceiverRunError::ProcessingTask(error));
    }
    stopped_result.map_err(OtlpReceiverRunError::SourceHealth)
}

fn update_receiver_state(
    state: &watch::Sender<OtlpReceiverSnapshot>,
    address: SocketAddr,
    bind_state: OtlpReceiverBindState,
) {
    state.send_replace(OtlpReceiverSnapshot {
        address,
        bind_state,
    });
}

async fn serve_receiver(
    listener: TcpListener,
    limits: OtlpRequestLimits,
    processor: Arc<OtlpObservationProcessor>,
    diagnostics: StructuredDiagnosticLog,
    shutdown: watch::Receiver<bool>,
    health_retry_interval: Duration,
) -> std::io::Result<()> {
    let server = axum::serve(
        listener,
        receiver_router(limits, Arc::clone(&processor), diagnostics),
    )
    .with_graceful_shutdown(wait_for_shutdown(shutdown))
    .into_future();
    tokio::pin!(server);
    let mut health_retry = tokio::time::interval_at(
        Instant::now() + health_retry_interval,
        health_retry_interval,
    );
    health_retry.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            result = &mut server => return result,
            _ = health_retry.tick() => {
                let _ = processor.persist_source_health_if_dirty().await;
            }
        }
    }
}

fn receiver_router(
    limits: OtlpRequestLimits,
    processor: Arc<OtlpObservationProcessor>,
    diagnostics: StructuredDiagnosticLog,
) -> Router {
    otlp_http_router(limits, processor, diagnostics)
        .route("/healthz", get(|| async { StatusCode::NO_CONTENT }))
}

fn shutdown_requested(shutdown: &watch::Receiver<bool>) -> bool {
    *shutdown.borrow() || shutdown.has_changed().is_err()
}

async fn wait_for_shutdown(mut shutdown: watch::Receiver<bool>) {
    while !shutdown_requested(&shutdown) {
        if shutdown.changed().await.is_err() {
            break;
        }
    }
}

async fn wait_before_rebind(
    shutdown: &mut watch::Receiver<bool>,
    retry_bind: &Notify,
    retry_interval: Duration,
) -> bool {
    if shutdown_requested(shutdown) {
        return true;
    }
    tokio::select! {
        () = retry_bind.notified() => false,
        result = shutdown.changed() => result.is_err() || *shutdown.borrow(),
        () = tokio::time::sleep(retry_interval) => false,
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::net::TcpListener as BlockingTcpListener;
    use std::pin::Pin;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use axum::body::Body;
    use axum::http::Request;
    use codex_monitor_core::{SourceExtraction, SourceHealth, SourceHealthStatus, SourceId};
    use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
    use tokio::net::TcpStream;
    use tokio::sync::Notify;
    use tokio::time::{sleep, timeout};
    use tower::ServiceExt as _;

    use crate::{
        MAX_OTLP_REQUEST_TIMEOUT, ObservationSink, ObservationSinkError, ObservationSinkErrorCode,
        OtlpLogSource, OtlpRequestProcessor,
    };

    use super::*;

    const FIXTURE: &str = include_str!("../../../fixtures/otlp/response_completed.json");

    #[derive(Default)]
    struct TestSink {
        commits: AtomicUsize,
        health: StdMutex<Vec<SourceHealth>>,
        fail_health: AtomicBool,
        commit_gate: StdMutex<Option<Arc<Notify>>>,
    }

    impl ObservationSink for TestSink {
        fn commit<'a>(
            &'a self,
            _extraction: SourceExtraction,
            _accepted_health: SourceHealth,
        ) -> Pin<Box<dyn Future<Output = Result<(), ObservationSinkError>> + Send + 'a>> {
            let gate = self.commit_gate.lock().unwrap().clone();
            Box::pin(async move {
                if let Some(gate) = gate {
                    gate.notified().await;
                }
                self.commits.fetch_add(1, Ordering::Relaxed);
                Ok(())
            })
        }

        fn store_health<'a>(
            &'a self,
            health: SourceHealth,
        ) -> Pin<Box<dyn Future<Output = Result<(), ObservationSinkError>> + Send + 'a>> {
            Box::pin(async move {
                if self.fail_health.load(Ordering::Relaxed) {
                    return Err(ObservationSinkError::new(
                        ObservationSinkErrorCode::Unavailable,
                    ));
                }
                self.health.lock().unwrap().push(health);
                Ok(())
            })
        }
    }

    fn test_processor(sink: &Arc<TestSink>) -> Arc<OtlpObservationProcessor> {
        Arc::new(OtlpObservationProcessor::new(
            OtlpLogSource::new(UtcTimestamp::now()).unwrap(),
            Arc::clone(sink) as Arc<dyn ObservationSink>,
        ))
    }

    async fn wait_for_status(processor: &OtlpObservationProcessor, expected: SourceHealthStatus) {
        timeout(Duration::from_secs(2), async {
            loop {
                if processor.source_health().await.status() == expected {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    async fn wait_for_listener(address: SocketAddr) {
        timeout(Duration::from_secs(2), async {
            loop {
                if TcpStream::connect(address).await.is_ok() {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    async fn wait_for_bind_state(monitor: &OtlpReceiverMonitor, expected: OtlpReceiverBindState) {
        timeout(Duration::from_secs(2), async {
            loop {
                if monitor.snapshot().bind_state() == expected {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    #[test]
    fn every_legal_request_deadline_is_shorter_than_receiver_drain() {
        assert!(MAX_OTLP_REQUEST_TIMEOUT < SHUTDOWN_DRAIN_TIMEOUT);
        assert!(
            OtlpRequestLimits::new(1, 1, MAX_OTLP_REQUEST_TIMEOUT).is_ok(),
            "the documented maximum must remain constructible"
        );
    }

    #[test]
    fn fixed_test_monitor_preserves_its_closed_snapshot() {
        let snapshot =
            OtlpReceiverSnapshot::new(OTLP_RECEIVER_ADDRESS, OtlpReceiverBindState::Conflict);
        let monitor = OtlpReceiverMonitor::from_snapshot(snapshot);

        assert_eq!(monitor.snapshot(), snapshot);
    }

    #[tokio::test]
    async fn port_conflict_is_reported_and_recovers_without_restarting_sink() {
        let blocker = BlockingTcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = blocker.local_addr().unwrap();
        let sink = Arc::new(TestSink::default());
        let processor = test_processor(&sink);
        let mut receiver = spawn_otlp_receiver_at(
            Arc::clone(&processor),
            OtlpRequestLimits::default(),
            StructuredDiagnosticLog::memory(),
            address,
            Duration::from_secs(30),
        )
        .unwrap();
        let monitor = receiver.monitor();
        let control = receiver.control();
        assert_eq!(monitor.snapshot().address(), address);
        assert!(monitor.snapshot().retry_supported());

        wait_for_status(&processor, SourceHealthStatus::Unavailable).await;
        wait_for_bind_state(&monitor, OtlpReceiverBindState::Conflict).await;
        drop(blocker);
        assert_eq!(
            control.request_bind_retry().unwrap().bind_state(),
            OtlpReceiverBindState::Conflict
        );
        wait_for_status(&processor, SourceHealthStatus::Healthy).await;
        wait_for_bind_state(&monitor, OtlpReceiverBindState::Bound).await;
        assert_eq!(
            control.request_bind_retry(),
            Err(OtlpReceiverRetryError::InvalidState)
        );
        wait_for_listener(address).await;

        receiver.shutdown().await.unwrap();
        assert_eq!(
            monitor.snapshot().bind_state(),
            OtlpReceiverBindState::Stopped
        );
        assert_eq!(
            processor.source_health().await.status(),
            SourceHealthStatus::Stopped
        );
        let health = sink.health.lock().unwrap();
        assert_eq!(
            health.last().unwrap().identity().id(),
            &SourceId::new("otlp-logs").unwrap()
        );
        assert_eq!(health.last().unwrap().status(), SourceHealthStatus::Stopped);
        assert_eq!(
            health
                .last()
                .unwrap()
                .last_failure()
                .map(codex_monitor_core::SourceFailure::code),
            Some(SafeErrorCode::OtlpBindFailed)
        );
    }

    #[tokio::test]
    async fn receiver_exposes_only_logs_and_minimal_health_routes() {
        let sink = Arc::new(TestSink::default());
        let processor = test_processor(&sink);
        let router = receiver_router(
            OtlpRequestLimits::default(),
            processor,
            StructuredDiagnosticLog::memory(),
        );

        let health = router
            .clone()
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::NO_CONTENT);

        let unknown = router
            .oneshot(
                Request::get("/api/v1/model-calls")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn shutdown_reports_when_the_final_health_state_cannot_be_persisted() {
        let blocker = BlockingTcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = blocker.local_addr().unwrap();
        let sink = Arc::new(TestSink::default());
        let processor = test_processor(&sink);
        let mut receiver = spawn_otlp_receiver_at(
            Arc::clone(&processor),
            OtlpRequestLimits::default(),
            StructuredDiagnosticLog::memory(),
            address,
            Duration::from_secs(30),
        )
        .unwrap();
        wait_for_status(&processor, SourceHealthStatus::Unavailable).await;

        sink.fail_health.store(true, Ordering::Relaxed);
        let error = receiver.shutdown().await.unwrap_err();
        assert!(matches!(
            error,
            OtlpReceiverShutdownError::SourceHealth(error)
                if error.code() == ObservationSinkErrorCode::Unavailable
        ));
        assert_eq!(
            processor.source_health().await.status(),
            SourceHealthStatus::Stopped
        );
        drop(blocker);
    }

    #[test]
    fn public_start_outside_tokio_returns_a_closed_error() {
        let sink = Arc::new(TestSink::default());
        let processor = test_processor(&sink);

        let result = spawn_otlp_receiver(
            processor,
            OtlpRequestLimits::default(),
            StructuredDiagnosticLog::memory(),
        );
        assert!(matches!(
            result,
            Err(OtlpReceiverStartError::RuntimeUnavailable)
        ));
    }

    #[tokio::test]
    async fn partial_http_body_cannot_block_receiver_shutdown() {
        let reserved = BlockingTcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = reserved.local_addr().unwrap();
        drop(reserved);
        let sink = Arc::new(TestSink::default());
        let processor = test_processor(&sink);
        let limits = OtlpRequestLimits::new(1024, 1024, Duration::from_millis(200)).unwrap();
        let mut receiver = spawn_otlp_receiver_at(
            Arc::clone(&processor),
            limits,
            StructuredDiagnosticLog::memory(),
            address,
            Duration::from_secs(30),
        )
        .unwrap();
        wait_for_status(&processor, SourceHealthStatus::Healthy).await;

        let stream = TcpStream::connect(address).await.unwrap();
        let partial_request = b"POST /v1/logs HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{";
        stream.writable().await.unwrap();
        assert_eq!(
            stream.try_write(partial_request).unwrap(),
            partial_request.len()
        );
        sleep(Duration::from_millis(25)).await;

        timeout(Duration::from_secs(1), receiver.shutdown())
            .await
            .expect("partial request must not hold graceful shutdown open")
            .unwrap();
        drop(stream);
    }

    #[tokio::test]
    async fn drain_timeout_retains_supervised_commit_for_a_later_join() {
        let reserved = BlockingTcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = reserved.local_addr().unwrap();
        drop(reserved);
        let sink = Arc::new(TestSink::default());
        let gate = Arc::new(Notify::new());
        *sink.commit_gate.lock().unwrap() = Some(Arc::clone(&gate));
        let processor = test_processor(&sink);
        let mut receiver = spawn_otlp_receiver_at(
            Arc::clone(&processor),
            OtlpRequestLimits::default(),
            StructuredDiagnosticLog::memory(),
            address,
            Duration::from_secs(30),
        )
        .unwrap();
        wait_for_status(&processor, SourceHealthStatus::Healthy).await;

        let request: ExportLogsServiceRequest = serde_json::from_str(FIXTURE).unwrap();
        let processing = tokio::spawn({
            let processor = Arc::clone(&processor);
            async move { processor.process(request, UtcTimestamp::now()).await }
        });
        timeout(Duration::from_secs(1), async {
            loop {
                if processor.processing_count().await == 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the commit must register with its supervisor");

        let error = receiver
            .shutdown_with_timeout(Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(matches!(error, OtlpReceiverShutdownError::DrainTimedOut));
        assert!(!processing.is_finished());
        let repeated_shutdown_started = Instant::now();
        let repeated_error = receiver
            .shutdown_with_timeout(Duration::from_millis(25))
            .await
            .unwrap_err();
        assert!(
            repeated_shutdown_started.elapsed() < Duration::from_secs(1),
            "the repeated shutdown must include intake sealing in its deadline"
        );
        assert!(matches!(
            repeated_error,
            OtlpReceiverShutdownError::DrainTimedOut
        ));

        gate.notify_waiters();
        receiver
            .shutdown_with_timeout(Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(processing.await.unwrap().unwrap().rejected_log_records(), 0);
        let health = processor.source_health().await;
        assert_eq!(health.status(), SourceHealthStatus::Stopped);
        assert!(health.last_accepted_at().is_some());

        assert_eq!(sink.commits.load(Ordering::Relaxed), 1);
        let stored = sink.health.lock().unwrap();
        assert_eq!(stored.last().unwrap().status(), SourceHealthStatus::Stopped);
        assert!(
            stored
                .iter()
                .any(|health| health.last_accepted_at().is_some())
        );
    }
}
