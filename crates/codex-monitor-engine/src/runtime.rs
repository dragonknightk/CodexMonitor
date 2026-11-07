//! Unified lifecycle for the resident monitoring engine.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use codex_monitor_core::{ActionExecutor, UtcTimestamp, ValidationError};
use codex_monitor_dashboard::{
    DashboardAssets, DashboardCapabilityPort, DashboardEventPort, DashboardListenerHandle,
    DashboardListenerShutdownError, DashboardListenerStartError, DashboardPlatformCapabilities,
    DashboardQueryPort, DashboardSessionError, DashboardSessions, DashboardSettingsPort,
    DashboardState, dashboard_router, spawn_dashboard_listener,
};
use codex_monitor_storage::{
    ActionLeaseWindow, ActionRecoveryRequest, AppLocale, CurrentUserStorage,
    DEFAULT_RETENTION_BATCH_SIZE, LegacyImportOutcome, MAX_ACTION_RECOVERY_BATCH_SIZE,
    RetentionRequest, StorageError, StorageResult, StorageWriter,
};
use codex_monitor_telemetry::{
    ENGINE_LOG_FILE, ObservationSink, OtlpLogSource, OtlpObservationProcessor, OtlpReceiverHandle,
    OtlpReceiverMonitor, OtlpReceiverShutdownError, OtlpReceiverStartError, OtlpRequestLimits,
    SessionIndexResolver, StructuredDiagnosticLog, spawn_otlp_receiver,
};
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;

use crate::actions::{ActionProcessOutcome, ActionProcessor, ActionProcessorBuildError};
use crate::bridges::{DashboardAdapter, StorageObservationSink};
use crate::rules::{RuleWorkProcessOutcome, RuleWorkProcessor};
use crate::{
    LoginAutostartHost, SettingsMonitor, SettingsRuntime, SettingsRuntimeCreateError,
    SettingsRuntimeError,
};

const WORKER_IDLE_DELAY: Duration = Duration::from_millis(100);
const RETENTION_INTERVAL: Duration = Duration::from_hours(1);
const RULE_STALE_AFTER_SECONDS: i128 = 30;
const ACTION_LEASE_SECONDS: i128 = 60;
const NANOS_PER_SECOND: i128 = 1_000_000_000;
const DEFAULT_RESIDENT_ENGINE_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(14);
const MAX_FORCE_JOIN_RESERVE: Duration = Duration::from_secs(1);
const MAX_STORAGE_SHUTDOWN_RESERVE: Duration = Duration::from_secs(2);

/// Host adapters and local paths needed to start one resident engine.
pub struct ResidentEngineConfig {
    dashboard_assets: DashboardAssets,
    session_index_path: PathBuf,
    detected_locale: AppLocale,
    login_autostart: Arc<dyn LoginAutostartHost>,
    action_executor: Arc<dyn ActionExecutor>,
    platform: DashboardPlatformCapabilities,
}

/// Cloneable cancellation signal shared by native startup and the engine.
///
/// Cancellation is observed only at resource-ownership boundaries. An active
/// migration or durable transaction is allowed to finish before startup rolls
/// back the resources it already owns.
#[derive(Clone, Default)]
pub struct ResidentEngineCancellation {
    cancelled: Arc<AtomicBool>,
}

impl ResidentEngineCancellation {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// One-time authenticated Dashboard URL intended only for a presentation host.
///
/// `Debug` is deliberately redacted because the query contains a short-lived
/// bootstrap credential.
pub struct DashboardLaunchUrl(String);

impl DashboardLaunchUrl {
    #[must_use]
    pub fn expose_to_presentation_host(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DashboardLaunchUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("DashboardLaunchUrl")
            .field(&"[REDACTED]")
            .finish()
    }
}

impl ResidentEngineConfig {
    #[must_use]
    pub fn new(
        dashboard_assets: DashboardAssets,
        session_index_path: impl Into<PathBuf>,
        detected_locale: AppLocale,
        login_autostart: Arc<dyn LoginAutostartHost>,
        action_executor: Arc<dyn ActionExecutor>,
        platform: DashboardPlatformCapabilities,
    ) -> Self {
        Self {
            dashboard_assets,
            session_index_path: session_index_path.into(),
            detected_locale,
            login_autostart,
            action_executor,
            platform,
        }
    }
}

/// Failure before the complete resident engine has published readiness.
#[derive(Debug, thiserror::Error)]
pub enum ResidentEngineStartError {
    #[error("resident startup was cancelled")]
    Cancelled,
    #[error("resident storage could not be opened or prepared")]
    Storage(#[from] StorageError),
    #[error("the settings coordinator could not be created")]
    SettingsCreate(#[from] SettingsRuntimeCreateError),
    #[error("initial settings coordination failed")]
    SettingsInitialize(#[source] SettingsRuntimeError),
    #[error("the registered OTLP source could not be constructed")]
    Source(#[from] ValidationError),
    #[error("the action executor does not match the registered action contract")]
    ActionProcessor(#[from] ActionProcessorBuildError),
    #[error("the OTLP receiver could not be started")]
    Receiver(#[from] OtlpReceiverStartError),
    #[error("the Dashboard listener could not be started")]
    Dashboard(#[from] DashboardListenerStartError),
}

impl ResidentEngineStartError {
    /// Return the closed diagnostic code exposed to the native host.
    #[must_use]
    pub const fn as_code(&self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::Storage(_) => "storage",
            Self::SettingsCreate(_) => "settings_create",
            Self::SettingsInitialize(_) => "settings_initialize",
            Self::Source(_) => "source",
            Self::ActionProcessor(_) => "action_processor",
            Self::Receiver(_) => "receiver",
            Self::Dashboard(_) => "dashboard",
        }
    }
}

/// Stable stage categories reported after every shutdown stage was attempted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResidentEngineShutdownStage {
    OtlpReceiver,
    RuleWorker,
    ActionWorker,
    RetentionWorker,
    WorkerSupervisor,
    DashboardListener,
    SettingsRuntime,
    StorageWriter,
}

impl ResidentEngineShutdownStage {
    /// Return a privacy-safe diagnostic code for the native host.
    #[must_use]
    pub const fn as_code(self) -> &'static str {
        match self {
            Self::OtlpReceiver => "otlp_receiver",
            Self::RuleWorker => "rule_worker",
            Self::ActionWorker => "action_worker",
            Self::RetentionWorker => "retention_worker",
            Self::WorkerSupervisor => "worker_supervisor",
            Self::DashboardListener => "dashboard_listener",
            Self::SettingsRuntime => "settings_runtime",
            Self::StorageWriter => "storage_writer",
        }
    }
}

/// Closed reason for one failed shutdown stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResidentEngineShutdownIssueKind {
    Failed,
    TimedOut,
}

impl ResidentEngineShutdownIssueKind {
    /// Return a privacy-safe diagnostic code for the native host.
    #[must_use]
    pub const fn as_code(self) -> &'static str {
        match self {
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
        }
    }
}

/// One closed, privacy-safe engine shutdown issue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResidentEngineShutdownIssue {
    stage: ResidentEngineShutdownStage,
    kind: ResidentEngineShutdownIssueKind,
}

impl ResidentEngineShutdownIssue {
    const fn new(
        stage: ResidentEngineShutdownStage,
        kind: ResidentEngineShutdownIssueKind,
    ) -> Self {
        Self { stage, kind }
    }

    #[must_use]
    pub const fn stage(self) -> ResidentEngineShutdownStage {
        self.stage
    }

    #[must_use]
    pub const fn kind(self) -> ResidentEngineShutdownIssueKind {
        self.kind
    }
}

/// One or more resident resources could not complete normal shutdown.
#[derive(Debug, thiserror::Error)]
#[error("resident engine shutdown completed with closed issues {issues:?}")]
pub struct ResidentEngineShutdownError {
    issues: Vec<ResidentEngineShutdownIssue>,
}

impl ResidentEngineShutdownError {
    #[must_use]
    pub fn issues(&self) -> &[ResidentEngineShutdownIssue] {
        &self.issues
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ShutdownBudget {
    graceful: Instant,
    force: Instant,
    hard: Instant,
}

impl ShutdownBudget {
    fn ending_at(hard_deadline: Instant) -> Self {
        let now = Instant::now();
        let remaining = hard_deadline.saturating_duration_since(now);
        let storage_reserve = (remaining / 4).min(MAX_STORAGE_SHUTDOWN_RESERVE);
        let force_reserve = (remaining / 8).min(MAX_FORCE_JOIN_RESERVE);
        let force_deadline = hard_deadline
            .checked_sub(storage_reserve)
            .unwrap_or(hard_deadline);
        let graceful_deadline = force_deadline
            .checked_sub(force_reserve)
            .unwrap_or(force_deadline);
        Self {
            graceful: graceful_deadline,
            force: force_deadline,
            hard: hard_deadline,
        }
    }
}

/// Observable health of the durable background-worker set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResidentWorkerState {
    Running,
    Failed,
    Stopped,
}

/// Cloneable monitor used by the native lifecycle and tray presentation.
#[derive(Clone)]
pub struct ResidentWorkerMonitor {
    state: watch::Receiver<ResidentWorkerState>,
}

/// Narrow cloneable capability for one explicit legacy-database import.
///
/// Keeping this handle separate from `ResidentEngine` lets the native host
/// release lifecycle ownership before a bounded writer transaction runs.
#[derive(Clone)]
pub struct LegacyDatabaseImporter {
    writer: StorageWriter,
}

impl LegacyDatabaseImporter {
    /// Import one absolute, explicitly selected legacy database as historical
    /// observations. The source is reopened read-only by the writer actor.
    ///
    /// # Errors
    ///
    /// Returns a closed storage error when the source is unsafe or unsupported,
    /// or when the writer cannot commit the complete import atomically.
    pub async fn import(&self, source_path: PathBuf) -> StorageResult<LegacyImportOutcome> {
        self.writer
            .import_legacy_database(source_path, UtcTimestamp::now())
            .await
    }
}

/// Controlled publisher paired with a real worker monitor in integration tests.
#[cfg(feature = "test-support")]
pub struct ResidentWorkerTestController {
    state: watch::Sender<ResidentWorkerState>,
}

impl ResidentWorkerMonitor {
    #[must_use]
    pub fn snapshot(&self) -> ResidentWorkerState {
        *self.state.borrow()
    }

    /// Wait for the worker-set state to change.
    ///
    /// Returns `None` after the owning resident engine has been dropped.
    pub async fn changed(&mut self) -> Option<ResidentWorkerState> {
        self.state.changed().await.ok()?;
        Some(self.snapshot())
    }

    /// Create a real monitor backed by a controlled test publisher.
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn test_channel(initial: ResidentWorkerState) -> (ResidentWorkerTestController, Self) {
        let (state, receiver) = watch::channel(initial);
        (
            ResidentWorkerTestController { state },
            Self { state: receiver },
        )
    }
}

#[cfg(feature = "test-support")]
impl ResidentWorkerTestController {
    /// Publish the next worker state to the paired monitor.
    pub fn set_state(&self, state: ResidentWorkerState) {
        self.state.send_replace(state);
    }
}

/// Non-cloneable owner of every engine resource in the resident process.
pub struct ResidentEngine {
    storage: Option<CurrentUserStorage>,
    settings: SettingsRuntime,
    sessions: DashboardSessions,
    receiver: Option<OtlpReceiverHandle>,
    receiver_monitor: OtlpReceiverMonitor,
    workers: Option<WorkerSet>,
    worker_monitor: ResidentWorkerMonitor,
    dashboard: Option<DashboardListenerHandle>,
    dashboard_address: std::net::SocketAddr,
}

impl ResidentEngine {
    /// Resolve and migrate current-user storage, then start every resident
    /// service. Readiness is returned only after platform reconciliation,
    /// crash recovery, workers, and both listener owners have been composed.
    ///
    /// # Errors
    ///
    /// Returns a closed startup failure after rolling back every resource that
    /// was already started.
    pub async fn start_current_user(
        config: ResidentEngineConfig,
        cancellation: ResidentEngineCancellation,
    ) -> Result<Self, ResidentEngineStartError> {
        ensure_starting(&cancellation)?;
        let storage = CurrentUserStorage::open()?;
        Self::start_with_storage(storage, config, cancellation).await
    }

    /// 使用已经准备好的 current-user storage 启动完整常驻组合。
    ///
    /// # Errors
    ///
    /// 返回与 [`Self::start_current_user`] 相同的闭合启动错误。
    pub async fn start_with_storage(
        storage: CurrentUserStorage,
        config: ResidentEngineConfig,
        cancellation: ResidentEngineCancellation,
    ) -> Result<Self, ResidentEngineStartError> {
        match start_services(&storage, config, &cancellation).await {
            Ok(services) => {
                let mut engine = Self {
                    storage: Some(storage),
                    settings: services.settings,
                    sessions: services.sessions,
                    receiver: Some(services.receiver),
                    receiver_monitor: services.receiver_monitor,
                    workers: Some(services.workers),
                    worker_monitor: services.worker_monitor,
                    dashboard: Some(services.dashboard),
                    dashboard_address: services.dashboard_address,
                };
                if cancellation.is_cancelled() {
                    let _ = engine.shutdown().await;
                    Err(ResidentEngineStartError::Cancelled)
                } else {
                    Ok(engine)
                }
            }
            Err(error) => {
                let _ = storage.shutdown().await;
                Err(error)
            }
        }
    }

    #[must_use]
    pub const fn dashboard_address(&self) -> std::net::SocketAddr {
        self.dashboard_address
    }

    /// Create a fresh one-time bootstrap URL for a browser or future `WebView`.
    ///
    /// # Errors
    ///
    /// Returns a closed credential-generation failure without exposing secret
    /// material through `Debug` output.
    pub fn fresh_dashboard_url(&self) -> Result<DashboardLaunchUrl, DashboardSessionError> {
        let token = self.sessions.issue_bootstrap()?;
        Ok(DashboardLaunchUrl(format!(
            "http://{}/api/v1/session/bootstrap?token={}",
            self.dashboard_address,
            token.query_value()
        )))
    }

    #[must_use]
    pub fn receiver_monitor(&self) -> OtlpReceiverMonitor {
        self.receiver_monitor.clone()
    }

    #[must_use]
    pub fn worker_monitor(&self) -> ResidentWorkerMonitor {
        self.worker_monitor.clone()
    }

    #[must_use]
    pub fn settings(&self) -> SettingsRuntime {
        self.settings.clone()
    }

    #[must_use]
    pub fn legacy_database_importer(&self) -> Option<LegacyDatabaseImporter> {
        self.storage.as_ref().map(|storage| LegacyDatabaseImporter {
            writer: storage.writer().clone(),
        })
    }

    /// Stop every `SQLite` producer and listener before closing the writer.
    ///
    /// Every stage shares one monotonic deadline and is attempted even if an
    /// earlier stage reports failure. A short tail is reserved for aborting
    /// async tasks and closing the dedicated `SQLite` writer.
    ///
    /// # Errors
    ///
    /// Returns stable stage categories for resources that did not stop cleanly.
    pub async fn shutdown(&mut self) -> Result<(), ResidentEngineShutdownError> {
        self.shutdown_with_timeout(DEFAULT_RESIDENT_ENGINE_SHUTDOWN_TIMEOUT)
            .await
    }

    /// Shut the engine down within one relative timeout.
    ///
    /// # Errors
    ///
    /// Returns only closed stage and reason codes after all stages were
    /// attempted.
    pub async fn shutdown_with_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<(), ResidentEngineShutdownError> {
        self.shutdown_before(Instant::now() + timeout).await
    }

    /// Shut the engine down before an absolute monotonic deadline supplied by
    /// the native host.
    ///
    /// # Errors
    ///
    /// Returns only closed stage and reason codes after all stages were
    /// attempted.
    pub async fn shutdown_before(
        &mut self,
        hard_deadline: Instant,
    ) -> Result<(), ResidentEngineShutdownError> {
        let budget = ShutdownBudget::ending_at(hard_deadline);
        let mut issues = Vec::new();
        self.seal_shutdown_producers();
        self.shutdown_receiver(&budget, &mut issues).await;
        self.shutdown_workers(&budget, &mut issues).await;
        self.shutdown_dashboard(&budget, &mut issues).await;
        self.shutdown_settings(&budget, &mut issues).await;
        self.shutdown_storage(&budget, &mut issues).await;

        if issues.is_empty() {
            Ok(())
        } else {
            Err(ResidentEngineShutdownError { issues })
        }
    }

    fn seal_shutdown_producers(&mut self) {
        if let Some(receiver) = self.receiver.as_mut() {
            receiver.begin_shutdown();
        }
        if let Some(workers) = self.workers.as_mut() {
            workers.begin_shutdown();
        }
        if let Some(dashboard) = self.dashboard.as_mut() {
            dashboard.begin_shutdown();
        }
        self.settings.begin_shutdown();
    }

    async fn shutdown_receiver(
        &mut self,
        budget: &ShutdownBudget,
        issues: &mut Vec<ResidentEngineShutdownIssue>,
    ) {
        if let Some(receiver) = self.receiver.as_mut() {
            let kind = match tokio::time::timeout_at(budget.graceful, receiver.shutdown()).await {
                Ok(Ok(())) => None,
                Ok(Err(OtlpReceiverShutdownError::DrainTimedOut)) | Err(_) => {
                    Some(ResidentEngineShutdownIssueKind::TimedOut)
                }
                Ok(Err(_)) => Some(ResidentEngineShutdownIssueKind::Failed),
            };
            if let Some(kind) = kind {
                record_shutdown_issue(issues, ResidentEngineShutdownStage::OtlpReceiver, kind);
                receiver.abort_and_join_before(budget.force).await;
            }
        }
        self.receiver.take();
    }

    async fn shutdown_workers(
        &mut self,
        budget: &ShutdownBudget,
        issues: &mut Vec<ResidentEngineShutdownIssue>,
    ) {
        if let Some(workers) = self.workers.as_mut() {
            if let Ok(stages) = tokio::time::timeout_at(budget.graceful, workers.shutdown()).await {
                for stage in stages {
                    record_shutdown_issue(issues, stage, ResidentEngineShutdownIssueKind::Failed);
                }
            } else {
                record_shutdown_issue(
                    issues,
                    ResidentEngineShutdownStage::WorkerSupervisor,
                    ResidentEngineShutdownIssueKind::TimedOut,
                );
                workers.abort_and_join_before(budget.force).await;
            }
        }
        self.workers.take();
    }

    async fn shutdown_dashboard(
        &mut self,
        budget: &ShutdownBudget,
        issues: &mut Vec<ResidentEngineShutdownIssue>,
    ) {
        if let Some(dashboard) = self.dashboard.as_mut() {
            let kind = match tokio::time::timeout_at(budget.graceful, dashboard.shutdown()).await {
                Ok(Ok(())) => None,
                Ok(Err(DashboardListenerShutdownError::DrainTimedOut)) | Err(_) => {
                    Some(ResidentEngineShutdownIssueKind::TimedOut)
                }
                Ok(Err(_)) => Some(ResidentEngineShutdownIssueKind::Failed),
            };
            if let Some(kind) = kind {
                record_shutdown_issue(issues, ResidentEngineShutdownStage::DashboardListener, kind);
                dashboard.abort_and_join_before(budget.force).await;
            }
        }
        self.dashboard.take();
    }

    async fn shutdown_settings(
        &self,
        budget: &ShutdownBudget,
        issues: &mut Vec<ResidentEngineShutdownIssue>,
    ) {
        if tokio::time::timeout_at(budget.force, self.settings.shutdown())
            .await
            .is_err()
        {
            record_shutdown_issue(
                issues,
                ResidentEngineShutdownStage::SettingsRuntime,
                ResidentEngineShutdownIssueKind::TimedOut,
            );
        }
    }

    async fn shutdown_storage(
        &mut self,
        budget: &ShutdownBudget,
        issues: &mut Vec<ResidentEngineShutdownIssue>,
    ) {
        if let Some(storage) = self.storage.as_ref() {
            let kind = match tokio::time::timeout_at(budget.hard, storage.shutdown()).await {
                Ok(Ok(())) => None,
                Ok(Err(_)) => Some(ResidentEngineShutdownIssueKind::Failed),
                Err(_) => Some(ResidentEngineShutdownIssueKind::TimedOut),
            };
            if let Some(kind) = kind {
                record_shutdown_issue(issues, ResidentEngineShutdownStage::StorageWriter, kind);
            }
        }
        self.storage.take();
    }
}

struct StartedServices {
    settings: SettingsRuntime,
    sessions: DashboardSessions,
    receiver: OtlpReceiverHandle,
    receiver_monitor: OtlpReceiverMonitor,
    workers: WorkerSet,
    worker_monitor: ResidentWorkerMonitor,
    dashboard: DashboardListenerHandle,
    dashboard_address: std::net::SocketAddr,
}

async fn start_services(
    storage: &CurrentUserStorage,
    config: ResidentEngineConfig,
    cancellation: &ResidentEngineCancellation,
) -> Result<StartedServices, ResidentEngineStartError> {
    ensure_starting(cancellation)?;
    let writer = storage.writer().clone();
    let settings = SettingsRuntime::new(storage.reader().clone(), &writer, config.login_autostart)?;
    settings
        .initialize_locale(config.detected_locale)
        .await
        .map_err(ResidentEngineStartError::SettingsInitialize)?;
    ensure_starting(cancellation)?;
    if let Err(error) = settings.reconcile_login_autostart().await {
        if error == SettingsRuntimeError::PlatformUnavailable {
            config.platform.set_login_autostart_available(false);
        } else {
            return Err(ResidentEngineStartError::SettingsInitialize(error));
        }
    }
    ensure_starting(cancellation)?;

    let rule_processor = RuleWorkProcessor::new(writer.clone()).await?;
    ensure_starting(cancellation)?;
    recover_started_actions(&writer, cancellation).await?;
    ensure_starting(cancellation)?;
    let action_processor = ActionProcessor::new(writer.clone(), config.action_executor)?;
    ensure_starting(cancellation)?;

    let source = OtlpLogSource::new(UtcTimestamp::now())?;
    let sink: Arc<dyn ObservationSink> = Arc::new(StorageObservationSink::new(writer.clone()));
    let processor = Arc::new(OtlpObservationProcessor::new(source, sink));
    let source_health = processor.monitor();
    let diagnostics_path = storage.paths().secure_log_file(ENGINE_LOG_FILE)?;
    let diagnostics = StructuredDiagnosticLog::open(&diagnostics_path).map_err(|source| {
        StorageError::AppDataIo {
            component: "engine diagnostics",
            source,
        }
    })?;
    let mut receiver = spawn_otlp_receiver(
        Arc::clone(&processor),
        OtlpRequestLimits::default(),
        diagnostics,
    )?;
    let receiver_monitor = receiver.monitor();
    if cancellation.is_cancelled() {
        let _ = receiver.shutdown().await;
        return Err(ResidentEngineStartError::Cancelled);
    }

    let sessions = DashboardSessions::default();
    let adapter = Arc::new(DashboardAdapter::new(
        storage,
        settings.clone(),
        Arc::new(SessionIndexResolver::new(config.session_index_path)),
        receiver.control(),
        source_health,
    ));
    let queries: Arc<dyn DashboardQueryPort> = adapter.clone();
    let dashboard_settings: Arc<dyn DashboardSettingsPort> = adapter.clone();
    let capabilities: Arc<dyn DashboardCapabilityPort> = adapter.clone();
    let events: Arc<dyn DashboardEventPort> = adapter;
    let state = DashboardState::new(
        config.dashboard_assets,
        sessions.clone(),
        queries,
        dashboard_settings,
        capabilities,
        events,
        config.platform,
    );
    let mut workers =
        WorkerSet::spawn(rule_processor, action_processor, writer, settings.monitor());
    let worker_monitor = workers.monitor();
    ensure_starting_with_worker_rollback(cancellation, &mut receiver, &mut workers).await?;
    let dashboard =
        match spawn_dashboard_listener(move |address| dashboard_router(address, state)).await {
            Ok(dashboard) => dashboard,
            Err(error) => {
                let _ = receiver.shutdown().await;
                let _ = workers.shutdown().await;
                return Err(ResidentEngineStartError::Dashboard(error));
            }
        };
    let dashboard_address = dashboard.local_address();
    if cancellation.is_cancelled() {
        let mut dashboard = dashboard;
        let _ = receiver.shutdown().await;
        let _ = workers.shutdown().await;
        let _ = dashboard.shutdown().await;
        return Err(ResidentEngineStartError::Cancelled);
    }

    Ok(StartedServices {
        settings,
        sessions,
        receiver,
        receiver_monitor,
        workers,
        worker_monitor,
        dashboard,
        dashboard_address,
    })
}

async fn ensure_starting_with_worker_rollback(
    cancellation: &ResidentEngineCancellation,
    receiver: &mut OtlpReceiverHandle,
    workers: &mut WorkerSet,
) -> Result<(), ResidentEngineStartError> {
    if cancellation.is_cancelled() {
        let _ = receiver.shutdown().await;
        let _ = workers.shutdown().await;
        Err(ResidentEngineStartError::Cancelled)
    } else {
        Ok(())
    }
}

async fn recover_started_actions(
    writer: &StorageWriter,
    cancellation: &ResidentEngineCancellation,
) -> Result<(), ResidentEngineStartError> {
    let recovered_at = UtcTimestamp::now();
    loop {
        ensure_starting(cancellation)?;
        let request = ActionRecoveryRequest::new(recovered_at, MAX_ACTION_RECOVERY_BATCH_SIZE)?;
        let outcome = writer.recover_started_actions(request).await?;
        if !outcome.more_remaining {
            return Ok(());
        }
    }
}

fn ensure_starting(
    cancellation: &ResidentEngineCancellation,
) -> Result<(), ResidentEngineStartError> {
    if cancellation.is_cancelled() {
        Err(ResidentEngineStartError::Cancelled)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkerKind {
    Rule,
    Action,
    Retention,
    Supervisor,
}

#[derive(Debug, thiserror::Error)]
enum WorkerRunError {
    #[error("a worker storage operation failed")]
    Storage(#[from] StorageError),
    #[error("a worker timestamp exceeded the supported range")]
    ClockOutOfRange,
    #[error("the settings monitor closed before worker cancellation")]
    SettingsMonitorClosed,
}

struct WorkerSet {
    cancellation: watch::Sender<bool>,
    supervisor: Option<JoinHandle<Result<(), Vec<WorkerKind>>>>,
    state: watch::Sender<ResidentWorkerState>,
    monitor: ResidentWorkerMonitor,
}

impl WorkerSet {
    fn spawn(
        rule: RuleWorkProcessor,
        action: ActionProcessor,
        writer: StorageWriter,
        settings: SettingsMonitor,
    ) -> Self {
        let (cancellation, cancellation_receiver) = watch::channel(false);
        let (state, state_receiver) = watch::channel(ResidentWorkerState::Running);
        let supervisor_cancellation = cancellation.clone();
        let supervisor_state = state.clone();
        let supervisor = tokio::spawn(run_worker_supervisor(
            rule,
            action,
            writer,
            settings,
            supervisor_cancellation,
            cancellation_receiver,
            supervisor_state,
        ));
        Self {
            cancellation,
            supervisor: Some(supervisor),
            state,
            monitor: ResidentWorkerMonitor {
                state: state_receiver,
            },
        }
    }

    fn monitor(&self) -> ResidentWorkerMonitor {
        self.monitor.clone()
    }

    fn begin_shutdown(&self) {
        self.cancellation.send_replace(true);
    }

    async fn shutdown(&mut self) -> Vec<ResidentEngineShutdownStage> {
        self.begin_shutdown();
        let Some(supervisor) = self.supervisor.as_mut() else {
            return Vec::new();
        };
        let completion = supervisor.await;
        self.supervisor.take();
        match completion {
            Ok(Ok(())) => Vec::new(),
            Ok(Err(failures)) => failures.into_iter().map(shutdown_stage).collect(),
            Err(_error) => vec![ResidentEngineShutdownStage::WorkerSupervisor],
        }
    }

    async fn abort_and_join_before(&mut self, deadline: Instant) {
        self.begin_shutdown();
        let Some(supervisor) = self.supervisor.as_mut() else {
            return;
        };
        self.state.send_replace(ResidentWorkerState::Failed);
        supervisor.abort();
        if tokio::time::timeout_at(deadline, supervisor).await.is_ok() {
            self.supervisor.take();
        }
    }
}

impl Drop for WorkerSet {
    fn drop(&mut self) {
        self.begin_shutdown();
        if let Some(supervisor) = self.supervisor.as_ref() {
            self.state.send_replace(ResidentWorkerState::Failed);
            supervisor.abort();
        }
    }
}

async fn run_worker_supervisor(
    rule: RuleWorkProcessor,
    action: ActionProcessor,
    writer: StorageWriter,
    settings: SettingsMonitor,
    cancellation_sender: watch::Sender<bool>,
    cancellation: watch::Receiver<bool>,
    state: watch::Sender<ResidentWorkerState>,
) -> Result<(), Vec<WorkerKind>> {
    let mut workers = JoinSet::new();
    let rule_cancellation = cancellation.clone();
    workers.spawn(async move {
        (
            WorkerKind::Rule,
            run_rule_worker(rule, rule_cancellation).await,
        )
    });
    let action_cancellation = cancellation.clone();
    let action_settings = settings.clone();
    workers.spawn(async move {
        (
            WorkerKind::Action,
            run_action_worker(action, action_settings, action_cancellation).await,
        )
    });
    workers.spawn(async move {
        (
            WorkerKind::Retention,
            run_retention_worker(writer, settings, cancellation).await,
        )
    });

    let mut failures = Vec::new();
    while let Some(completion) = workers.join_next().await {
        let expected_stop = *cancellation_sender.borrow();
        match completion {
            Ok((_kind, Ok(()))) if expected_stop => {}
            Ok((kind, Ok(()) | Err(_))) => failures.push(kind),
            Err(_error) => failures.push(WorkerKind::Supervisor),
        }
        if !failures.is_empty() {
            cancellation_sender.send_replace(true);
        }
    }

    if failures.is_empty() {
        state.send_replace(ResidentWorkerState::Stopped);
        Ok(())
    } else {
        state.send_replace(ResidentWorkerState::Failed);
        Err(failures)
    }
}

async fn run_rule_worker(
    processor: RuleWorkProcessor,
    mut cancellation: watch::Receiver<bool>,
) -> Result<(), WorkerRunError> {
    while !cancellation_requested(&cancellation) {
        let claimed_at = UtcTimestamp::now();
        let stale_before = shift_seconds(claimed_at, -RULE_STALE_AFTER_SECONDS)?;
        let outcome = processor.process_next(claimed_at, stale_before).await?;
        if cancellation_requested(&cancellation) {
            return Ok(());
        }
        if matches!(outcome, RuleWorkProcessOutcome::Idle)
            && wait_for_cancellation_or_delay(&mut cancellation).await
        {
            return Ok(());
        }
    }
    Ok(())
}

async fn run_action_worker(
    processor: ActionProcessor,
    settings: SettingsMonitor,
    mut cancellation: watch::Receiver<bool>,
) -> Result<(), WorkerRunError> {
    while !cancellation_requested(&cancellation) {
        let claimed_at = UtcTimestamp::now();
        let expires_at = shift_seconds(claimed_at, ACTION_LEASE_SECONDS)?;
        let locale = settings.snapshot().settings.locale;
        let outcome = processor
            .process_next(ActionLeaseWindow::new(claimed_at, expires_at)?, locale)
            .await?;
        if cancellation_requested(&cancellation) {
            return Ok(());
        }
        if matches!(outcome, ActionProcessOutcome::Idle)
            && wait_for_cancellation_or_delay(&mut cancellation).await
        {
            return Ok(());
        }
    }
    Ok(())
}

async fn run_retention_worker(
    writer: StorageWriter,
    mut settings: SettingsMonitor,
    mut cancellation: watch::Receiver<bool>,
) -> Result<(), WorkerRunError> {
    loop {
        drain_retention(&writer, &settings, &cancellation).await?;
        if cancellation_requested(&cancellation) {
            return Ok(());
        }

        tokio::select! {
            () = tokio::time::sleep(RETENTION_INTERVAL) => {}
            changed = settings.changed() => {
                if changed.is_err() && !cancellation_requested(&cancellation) {
                    return Err(WorkerRunError::SettingsMonitorClosed);
                }
            }
            changed = cancellation.changed() => {
                if changed.is_err() || cancellation_requested(&cancellation) {
                    return Ok(());
                }
            }
        }
    }
}

async fn drain_retention(
    writer: &StorageWriter,
    settings: &SettingsMonitor,
    cancellation: &watch::Receiver<bool>,
) -> Result<(), WorkerRunError> {
    loop {
        if cancellation_requested(cancellation) {
            return Ok(());
        }
        let policy = settings.snapshot().settings.retention;
        let request =
            RetentionRequest::new(policy, UtcTimestamp::now(), DEFAULT_RETENTION_BATCH_SIZE)?;
        let outcome = writer.apply_retention(request).await?;
        if !outcome.more_eligible {
            return Ok(());
        }
        tokio::task::yield_now().await;
    }
}

fn cancellation_requested(cancellation: &watch::Receiver<bool>) -> bool {
    *cancellation.borrow() || cancellation.has_changed().is_err()
}

async fn wait_for_cancellation_or_delay(cancellation: &mut watch::Receiver<bool>) -> bool {
    if cancellation_requested(cancellation) {
        return true;
    }
    tokio::select! {
        () = tokio::time::sleep(WORKER_IDLE_DELAY) => false,
        changed = cancellation.changed() => changed.is_err() || cancellation_requested(cancellation),
    }
}

fn shift_seconds(timestamp: UtcTimestamp, seconds: i128) -> Result<UtcTimestamp, WorkerRunError> {
    let nanoseconds = seconds
        .checked_mul(NANOS_PER_SECOND)
        .and_then(|offset| timestamp.unix_timestamp_nanos().checked_add(offset))
        .ok_or(WorkerRunError::ClockOutOfRange)?;
    UtcTimestamp::from_unix_timestamp_nanos(nanoseconds)
        .map_err(|_| WorkerRunError::ClockOutOfRange)
}

fn shutdown_stage(kind: WorkerKind) -> ResidentEngineShutdownStage {
    match kind {
        WorkerKind::Rule => ResidentEngineShutdownStage::RuleWorker,
        WorkerKind::Action => ResidentEngineShutdownStage::ActionWorker,
        WorkerKind::Retention => ResidentEngineShutdownStage::RetentionWorker,
        WorkerKind::Supervisor => ResidentEngineShutdownStage::WorkerSupervisor,
    }
}

fn record_shutdown_issue(
    issues: &mut Vec<ResidentEngineShutdownIssue>,
    stage: ResidentEngineShutdownStage,
    kind: ResidentEngineShutdownIssueKind,
) {
    let issue = ResidentEngineShutdownIssue::new(stage, kind);
    if !issues.contains(&issue) {
        issues.push(issue);
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use codex_monitor_core::{
        ActionCapability, ActionErrorCode, ActionExecutionContext, ActionExecutionResult,
        ActionFuture, ActionLifecycleState, ActionParameters, ActionPermission,
        RegisteredActionType, RuleErrorCode, SYSTEM_NOTIFICATION_DEFINITION,
    };
    use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
    use rusqlite::Connection;
    use tempfile::tempdir;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use crate::LoginAutostartError;
    use codex_monitor_dashboard::{
        DashboardAssetInput, SystemNotificationAvailability, TrayAvailability,
    };
    use codex_monitor_storage::{
        ActionClaimOutcome, ActionLeaseOwner, ActionStartOutcome, AppDataPaths, RetentionPolicy,
        StorageReader, StoredSourceHealth,
    };
    use codex_monitor_telemetry::TelemetrySource as _;

    use super::*;

    fn test_dashboard_assets() -> DashboardAssets {
        DashboardAssets::new([DashboardAssetInput::new(
            "index.html",
            b"<!doctype html><html><body><div id=\"root\"></div></body></html>".to_vec(),
            "text/html",
        )])
        .unwrap()
    }

    struct TestLoginAutostartHost {
        enabled: AtomicBool,
    }

    impl TestLoginAutostartHost {
        fn new(enabled: bool) -> Self {
            Self {
                enabled: AtomicBool::new(enabled),
            }
        }

        fn enabled(&self) -> bool {
            self.enabled.load(Ordering::Acquire)
        }
    }

    impl LoginAutostartHost for TestLoginAutostartHost {
        fn login_autostart(&self) -> Result<bool, LoginAutostartError> {
            Ok(self.enabled())
        }

        fn set_login_autostart(&self, enabled: bool) -> Result<bool, LoginAutostartError> {
            self.enabled.store(enabled, Ordering::Release);
            Ok(enabled)
        }
    }

    struct BlockingStartupLoginAutostartHost {
        enabled: AtomicBool,
        entered: std::sync::mpsc::SyncSender<()>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl LoginAutostartHost for BlockingStartupLoginAutostartHost {
        fn login_autostart(&self) -> Result<bool, LoginAutostartError> {
            Ok(self.enabled.load(Ordering::Acquire))
        }

        fn set_login_autostart(&self, enabled: bool) -> Result<bool, LoginAutostartError> {
            self.entered
                .send(())
                .map_err(|_| LoginAutostartError::Unavailable)?;
            self.release
                .lock()
                .map_err(|_| LoginAutostartError::Unavailable)?
                .recv()
                .map_err(|_| LoginAutostartError::Unavailable)?;
            self.enabled.store(enabled, Ordering::Release);
            Ok(enabled)
        }
    }

    struct UnavailableLoginAutostartHost {
        calls: AtomicUsize,
    }

    impl LoginAutostartHost for UnavailableLoginAutostartHost {
        fn login_autostart(&self) -> Result<bool, LoginAutostartError> {
            Err(LoginAutostartError::Unavailable)
        }

        fn set_login_autostart(&self, _enabled: bool) -> Result<bool, LoginAutostartError> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            Err(LoginAutostartError::Unavailable)
        }
    }

    struct CountingActionExecutor {
        calls: AtomicUsize,
    }

    impl CountingActionExecutor {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::Acquire)
        }
    }

    impl ActionExecutor for CountingActionExecutor {
        fn action_type(&self) -> RegisteredActionType {
            RegisteredActionType::SystemNotification
        }

        fn capabilities(&self) -> &'static [ActionCapability] {
            SYSTEM_NOTIFICATION_DEFINITION.capabilities
        }

        fn permissions(&self) -> &'static [ActionPermission] {
            SYSTEM_NOTIFICATION_DEFINITION.permissions
        }

        fn execute<'a>(
            &'a self,
            _parameters: &'a ActionParameters,
            _context: ActionExecutionContext,
        ) -> ActionFuture<'a> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            Box::pin(async { ActionExecutionResult::Completed })
        }
    }

    struct PendingActionExecutor {
        calls: AtomicUsize,
    }

    impl PendingActionExecutor {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::Acquire)
        }
    }

    impl ActionExecutor for PendingActionExecutor {
        fn action_type(&self) -> RegisteredActionType {
            RegisteredActionType::SystemNotification
        }

        fn capabilities(&self) -> &'static [ActionCapability] {
            SYSTEM_NOTIFICATION_DEFINITION.capabilities
        }

        fn permissions(&self) -> &'static [ActionPermission] {
            SYSTEM_NOTIFICATION_DEFINITION.permissions
        }

        fn execute<'a>(
            &'a self,
            _parameters: &'a ActionParameters,
            _context: ActionExecutionContext,
        ) -> ActionFuture<'a> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            Box::pin(std::future::pending())
        }
    }

    struct InvalidActionExecutor;

    impl ActionExecutor for InvalidActionExecutor {
        fn action_type(&self) -> RegisteredActionType {
            RegisteredActionType::SystemNotification
        }

        fn capabilities(&self) -> &'static [ActionCapability] {
            &[]
        }

        fn permissions(&self) -> &'static [ActionPermission] {
            SYSTEM_NOTIFICATION_DEFINITION.permissions
        }

        fn execute<'a>(
            &'a self,
            _parameters: &'a ActionParameters,
            _context: ActionExecutionContext,
        ) -> ActionFuture<'a> {
            unreachable!("an invalid executor must be rejected before execution")
        }
    }

    fn test_storage(root: &Path) -> CurrentUserStorage {
        let data_directory = root.join("resident-data");
        CurrentUserStorage::open_paths(AppDataPaths {
            database_file: data_directory.join("codex-monitor.sqlite3"),
            settings_file: data_directory.join("settings.toml"),
            logs_directory: data_directory.join("logs"),
            data_directory,
        })
        .unwrap()
    }

    fn test_config(
        root: &Path,
        login: Arc<TestLoginAutostartHost>,
        executor: Arc<CountingActionExecutor>,
    ) -> ResidentEngineConfig {
        let login_port: Arc<dyn LoginAutostartHost> = login;
        let action_port: Arc<dyn ActionExecutor> = executor;
        ResidentEngineConfig::new(
            test_dashboard_assets(),
            root.join("session_index.jsonl"),
            AppLocale::ZhCn,
            login_port,
            action_port,
            DashboardPlatformCapabilities::new(
                true,
                SystemNotificationAvailability::Available,
                TrayAvailability::Available,
            ),
        )
    }

    fn fixture_without_conversation(event_at: UtcTimestamp) -> ExportLogsServiceRequest {
        let fixture = include_str!("../../../fixtures/otlp/response_completed.json");
        let mut fixture: serde_json::Value = serde_json::from_str(fixture).unwrap();
        fixture["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0]["timeUnixNano"] =
            serde_json::Value::String(event_at.unix_timestamp_nanos().to_string());
        let body =
            fixture["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0]["body"]["stringValue"]
                .as_str()
                .unwrap();
        let mut body: serde_json::Value = serde_json::from_str(body).unwrap();
        body["response"]["id"] = serde_json::Value::String(format!(
            "synthetic-response-{}",
            event_at.unix_timestamp_nanos()
        ));
        fixture["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0]["body"]["stringValue"] =
            serde_json::Value::String(serde_json::to_string(&body).unwrap());
        let attributes = fixture["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0]["attributes"]
            .as_array_mut()
            .unwrap();
        attributes.retain(|attribute| {
            !matches!(
                attribute["key"].as_str(),
                Some(
                    "conversation.id"
                        | "conversation_id"
                        | "codex.conversation_id"
                        | "thread.id"
                        | "thread_id"
                        | "codex.thread_id"
                )
            )
        });
        serde_json::from_value(fixture).unwrap()
    }

    async fn commit_fixture(writer: &StorageWriter, observed_at: UtcTimestamp) {
        let source = OtlpLogSource::new(observed_at).unwrap();
        let extraction = source.extract(fixture_without_conversation(observed_at), observed_at);
        let mut accepted = source.clone();
        accepted.mark_committed(observed_at);
        writer
            .commit_source_batch(extraction, StoredSourceHealth::from(accepted.health()))
            .await
            .unwrap();
    }

    async fn assert_runtime_ready(engine: &ResidentEngine, login: &TestLoginAutostartHost) {
        assert!(
            !login.enabled(),
            "durable autostart=false must win before readiness"
        );
        assert_eq!(
            engine.settings().monitor().snapshot().settings.locale,
            AppLocale::ZhCn
        );
        assert_eq!(
            engine.worker_monitor().snapshot(),
            ResidentWorkerState::Running
        );
        let receiver_snapshot = engine.receiver_monitor().snapshot();
        assert_eq!(
            receiver_snapshot.address(),
            codex_monitor_telemetry::OTLP_RECEIVER_ADDRESS
        );
        assert!(receiver_snapshot.address().ip().is_loopback());
        assert!(engine.dashboard_address().ip().is_loopback());
        assert_ne!(engine.dashboard_address().port(), 0);
        tokio::net::TcpStream::connect(engine.dashboard_address())
            .await
            .unwrap();
        let dashboard_url = engine.fresh_dashboard_url().unwrap();
        let dashboard_url_value = dashboard_url.expose_to_presentation_host().to_owned();
        assert!(dashboard_url_value.starts_with(&format!(
            "http://{}/api/v1/session/bootstrap?token=",
            engine.dashboard_address()
        )));
        let second_dashboard_url = engine.fresh_dashboard_url().unwrap();
        assert_ne!(
            dashboard_url_value,
            second_dashboard_url.expose_to_presentation_host()
        );
        assert_eq!(
            format!("{dashboard_url:?}"),
            "DashboardLaunchUrl(\"[REDACTED]\")"
        );
    }

    async fn assert_unauthorized_dashboard_is_private(dashboard_address: SocketAddr) {
        let mut unauthorized = tokio::net::TcpStream::connect(dashboard_address)
            .await
            .unwrap();
        unauthorized
            .write_all(
                format!(
                    "GET /api/v1/model-calls HTTP/1.1\r\nHost: {dashboard_address}\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut unauthorized_response = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(2),
            unauthorized.read_to_end(&mut unauthorized_response),
        )
        .await
        .expect("unauthorized Dashboard response must close promptly")
        .unwrap();
        let unauthorized_response = String::from_utf8(unauthorized_response).unwrap();
        assert!(unauthorized_response.starts_with("HTTP/1.1 401 Unauthorized"));
        assert!(unauthorized_response.contains("\"code\":\"unauthorized\""));
        assert!(!unauthorized_response.contains("synthetic-model-v1"));
        assert!(!unauthorized_response.contains("synthetic-response-"));
        assert!(!unauthorized_response.contains("\"items\""));
    }

    async fn wait_for_action(executor: &CountingActionExecutor) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while executor.calls() == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("rule and action workers must process committed realtime work");
        assert_eq!(executor.calls(), 1);
    }

    async fn assert_ordered_runtime_shutdown(mut engine: ResidentEngine, writer: StorageWriter) {
        let mut worker_monitor = engine.worker_monitor();
        let receiver_monitor = engine.receiver_monitor();
        let dashboard_address = engine.dashboard_address();
        engine.shutdown().await.unwrap();
        assert_eq!(
            worker_monitor.changed().await,
            Some(ResidentWorkerState::Stopped)
        );
        assert_eq!(
            receiver_monitor.snapshot().bind_state(),
            codex_monitor_telemetry::OtlpReceiverBindState::Stopped
        );
        let released_dashboard = tokio::net::TcpListener::bind(dashboard_address)
            .await
            .expect("Dashboard listener address must be released after shutdown");
        drop(released_dashboard);
        let request = RetentionRequest::new(
            RetentionPolicy::Unlimited,
            UtcTimestamp::now(),
            DEFAULT_RETENTION_BATCH_SIZE,
        )
        .unwrap();
        assert!(matches!(
            writer.apply_retention(request).await,
            Err(StorageError::WriterClosed)
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn resident_runtime_composes_workers_listeners_reconciliation_and_shutdown() {
        let directory = tempdir().unwrap();
        let storage = test_storage(directory.path());
        let login = Arc::new(TestLoginAutostartHost::new(true));
        let executor = Arc::new(CountingActionExecutor::new());
        let engine = ResidentEngine::start_with_storage(
            storage,
            test_config(directory.path(), Arc::clone(&login), Arc::clone(&executor)),
            ResidentEngineCancellation::default(),
        )
        .await
        .unwrap();

        assert_runtime_ready(&engine, &login).await;
        let dashboard_address = engine.dashboard_address();
        let writer = engine.storage.as_ref().unwrap().writer().clone();
        commit_fixture(&writer, UtcTimestamp::now()).await;
        assert_unauthorized_dashboard_is_private(dashboard_address).await;
        wait_for_action(&executor).await;
        assert_ordered_runtime_shutdown(engine, writer).await;
    }

    #[test]
    fn shutdown_budget_reserves_force_and_storage_tails_inside_the_hard_deadline() {
        let started_at = Instant::now();
        let budget = ShutdownBudget::ending_at(started_at + Duration::from_secs(8));

        assert!(budget.graceful >= started_at);
        assert!(budget.graceful <= budget.force);
        assert!(budget.force <= budget.hard);
        assert!(budget.force.duration_since(budget.graceful) <= MAX_FORCE_JOIN_RESERVE);
        assert!(budget.hard.duration_since(budget.force) <= MAX_STORAGE_SHUTDOWN_RESERVE);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shutdown_deadline_aborts_a_stuck_worker_and_still_closes_sqlite() {
        let directory = tempdir().unwrap();
        let storage = test_storage(directory.path());
        let login: Arc<dyn LoginAutostartHost> = Arc::new(TestLoginAutostartHost::new(false));
        let executor = Arc::new(PendingActionExecutor::new());
        let action_port: Arc<dyn ActionExecutor> = executor.clone();
        let config = ResidentEngineConfig::new(
            test_dashboard_assets(),
            directory.path().join("session_index.jsonl"),
            AppLocale::EnUs,
            login,
            action_port,
            DashboardPlatformCapabilities::new(
                true,
                SystemNotificationAvailability::Available,
                TrayAvailability::Available,
            ),
        );
        let mut engine = ResidentEngine::start_with_storage(
            storage,
            config,
            ResidentEngineCancellation::default(),
        )
        .await
        .unwrap();
        let writer = engine.storage.as_ref().unwrap().writer().clone();
        let worker_monitor = engine.worker_monitor();

        commit_fixture(&writer, UtcTimestamp::now()).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while executor.calls() == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the synthetic action must enter its non-returning executor");

        let error = tokio::time::timeout(
            Duration::from_secs(3),
            engine.shutdown_with_timeout(Duration::from_secs(2)),
        )
        .await
        .expect("the engine must honor its total shutdown timeout")
        .unwrap_err();
        assert!(error.issues().contains(&ResidentEngineShutdownIssue::new(
            ResidentEngineShutdownStage::WorkerSupervisor,
            ResidentEngineShutdownIssueKind::TimedOut,
        )));
        assert_eq!(worker_monitor.snapshot(), ResidentWorkerState::Failed);

        let request = RetentionRequest::new(
            RetentionPolicy::Unlimited,
            UtcTimestamp::now(),
            DEFAULT_RETENTION_BATCH_SIZE,
        )
        .unwrap();
        assert!(matches!(
            writer.apply_retention(request).await,
            Err(StorageError::WriterClosed)
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn malformed_internal_rule_event_is_quarantined_without_stopping_ingestion() {
        let directory = tempdir().unwrap();
        let storage = test_storage(directory.path());
        let database = storage.writer().database_path().to_path_buf();
        let writer = storage.writer().clone();
        let first_event_at = UtcTimestamp::now();
        commit_fixture(&writer, first_event_at).await;

        let poison = Connection::open(&database).unwrap();
        poison
            .pragma_update(None, "ignore_check_constraints", true)
            .unwrap();
        assert_eq!(
            poison
                .execute(
                    "UPDATE model_calls SET canonical_identity_kind = 'malformed-internal-event'",
                    [],
                )
                .unwrap(),
            1
        );
        drop(poison);

        let login: Arc<dyn LoginAutostartHost> = Arc::new(TestLoginAutostartHost::new(false));
        let settings = SettingsRuntime::new(storage.reader().clone(), &writer, login).unwrap();
        settings.initialize_locale(AppLocale::EnUs).await.unwrap();
        let rule = RuleWorkProcessor::new(writer.clone()).await.unwrap();
        let executor: Arc<dyn ActionExecutor> = Arc::new(CountingActionExecutor::new());
        let action = ActionProcessor::new(writer.clone(), executor).unwrap();
        let mut workers = WorkerSet::spawn(rule, action, writer.clone(), settings.monitor());
        let worker_monitor = workers.monitor();

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let capability = storage
                    .reader()
                    .dashboard_rule_capabilities()
                    .unwrap()
                    .into_iter()
                    .next()
                    .unwrap();
                let completed_work: i64 = Connection::open(&database)
                    .unwrap()
                    .query_row(
                        "SELECT count(*) FROM domain_work WHERE state = 'completed'",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                if capability.last_error_code == Some(RuleErrorCode::PersistenceCommitFailed)
                    && completed_work == 1
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the malformed event must become observable and terminal");
        assert_eq!(worker_monitor.snapshot(), ResidentWorkerState::Running);

        let second_event_at = shift_seconds(first_event_at, 1).unwrap();
        commit_fixture(&writer, second_event_at).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let capability = storage
                    .reader()
                    .dashboard_rule_capabilities()
                    .unwrap()
                    .into_iter()
                    .next()
                    .unwrap();
                let counts = Connection::open(&database)
                    .unwrap()
                    .query_row(
                        "SELECT
                            (SELECT count(*) FROM domain_work WHERE state = 'completed'),
                            (SELECT count(*) FROM rule_evaluations)",
                        [],
                        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                    )
                    .unwrap();
                if capability.last_error_code == Some(RuleErrorCode::PersistenceCommitFailed)
                    && capability.last_evaluated_at.is_some()
                    && counts == (2, 1)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the worker and ingestion path must process the next valid event");
        assert_eq!(worker_monitor.snapshot(), ResidentWorkerState::Running);

        assert!(workers.shutdown().await.is_empty());
        settings.shutdown().await;
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn startup_recovers_started_actions_before_the_executor_can_run() {
        let directory = tempdir().unwrap();
        let storage = test_storage(directory.path());
        let writer = storage.writer().clone();
        let event_at = UtcTimestamp::now();
        commit_fixture(&writer, event_at).await;
        let rule = RuleWorkProcessor::new(writer.clone()).await.unwrap();
        assert!(matches!(
            rule.process_next(
                shift_seconds(event_at, 2).unwrap(),
                shift_seconds(event_at, 1).unwrap(),
            )
            .await
            .unwrap(),
            RuleWorkProcessOutcome::Completed { .. }
        ));
        let ActionClaimOutcome::Claimed(claimed) = writer
            .claim_action(
                ActionLeaseOwner::new(),
                ActionLeaseWindow::new(
                    shift_seconds(event_at, 3).unwrap(),
                    shift_seconds(event_at, 12).unwrap(),
                )
                .unwrap(),
            )
            .await
            .unwrap()
        else {
            panic!("the synthetic notification action must be ready");
        };
        let action_id = claimed.request().id;
        let ActionStartOutcome::Started(_started) = writer
            .start_action(claimed, shift_seconds(event_at, 3).unwrap())
            .await
            .unwrap()
        else {
            panic!("the synthetic notification action must enter started");
        };

        let login = Arc::new(TestLoginAutostartHost::new(false));
        let executor = Arc::new(CountingActionExecutor::new());
        let database = writer.database_path().to_path_buf();
        let mut engine = ResidentEngine::start_with_storage(
            storage,
            test_config(directory.path(), login, Arc::clone(&executor)),
            ResidentEngineCancellation::default(),
        )
        .await
        .unwrap();

        let recovered = StorageReader::new(database)
            .action(action_id)
            .unwrap()
            .unwrap();
        assert_eq!(recovered.state, ActionLifecycleState::Failed);
        assert_eq!(
            recovered.error_code,
            Some(ActionErrorCode::IndeterminateAfterCrash)
        );
        assert_eq!(executor.calls(), 0);

        engine.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn unavailable_login_autostart_is_reported_without_blocking_monitoring_startup() {
        let directory = tempdir().unwrap();
        let storage = test_storage(directory.path());
        let login = Arc::new(UnavailableLoginAutostartHost {
            calls: AtomicUsize::new(0),
        });
        let login_port: Arc<dyn LoginAutostartHost> = login.clone();
        let executor: Arc<dyn ActionExecutor> = Arc::new(CountingActionExecutor::new());
        let platform = DashboardPlatformCapabilities::new(
            true,
            SystemNotificationAvailability::Available,
            TrayAvailability::Available,
        );
        let config = ResidentEngineConfig::new(
            test_dashboard_assets(),
            directory.path().join("session_index.jsonl"),
            AppLocale::EnUs,
            login_port,
            executor,
            platform.clone(),
        );

        let mut engine = ResidentEngine::start_with_storage(
            storage,
            config,
            ResidentEngineCancellation::default(),
        )
        .await
        .unwrap();

        assert_eq!(login.calls.load(Ordering::Acquire), 1);
        assert!(!platform.login_autostart());
        assert_eq!(
            engine.worker_monitor().snapshot(),
            ResidentWorkerState::Running
        );

        engine.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn startup_failure_closes_the_storage_writer() {
        let directory = tempdir().unwrap();
        let storage = test_storage(directory.path());
        let writer = storage.writer().clone();
        let login: Arc<dyn LoginAutostartHost> = Arc::new(TestLoginAutostartHost::new(false));
        let executor: Arc<dyn ActionExecutor> = Arc::new(InvalidActionExecutor);
        let config = ResidentEngineConfig::new(
            test_dashboard_assets(),
            directory.path().join("session_index.jsonl"),
            AppLocale::EnUs,
            login,
            executor,
            DashboardPlatformCapabilities::new(
                true,
                SystemNotificationAvailability::Available,
                TrayAvailability::Available,
            ),
        );

        let result = ResidentEngine::start_with_storage(
            storage,
            config,
            ResidentEngineCancellation::default(),
        )
        .await;
        assert!(matches!(
            result,
            Err(ResidentEngineStartError::ActionProcessor(_))
        ));
        let request = RetentionRequest::new(
            RetentionPolicy::Unlimited,
            UtcTimestamp::now(),
            DEFAULT_RETENTION_BATCH_SIZE,
        )
        .unwrap();
        assert!(matches!(
            writer.apply_retention(request).await,
            Err(StorageError::WriterClosed)
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn startup_cancellation_stops_at_the_next_safe_boundary_and_rolls_back() {
        let directory = tempdir().unwrap();
        let storage = test_storage(directory.path());
        let writer = storage.writer().clone();
        let (entered_sender, entered_receiver) = std::sync::mpsc::sync_channel(1);
        let (release_sender, release_receiver) = std::sync::mpsc::sync_channel(1);
        let login: Arc<dyn LoginAutostartHost> = Arc::new(BlockingStartupLoginAutostartHost {
            enabled: AtomicBool::new(true),
            entered: entered_sender,
            release: std::sync::Mutex::new(release_receiver),
        });
        let executor: Arc<dyn ActionExecutor> = Arc::new(InvalidActionExecutor);
        let config = ResidentEngineConfig::new(
            test_dashboard_assets(),
            directory.path().join("session_index.jsonl"),
            AppLocale::EnUs,
            login,
            executor,
            DashboardPlatformCapabilities::new(
                true,
                SystemNotificationAvailability::Available,
                TrayAvailability::Available,
            ),
        );
        let cancellation = ResidentEngineCancellation::default();
        let startup_cancellation = cancellation.clone();
        let startup = tokio::spawn(async move {
            ResidentEngine::start_with_storage(storage, config, startup_cancellation).await
        });
        tokio::time::timeout(
            Duration::from_secs(1),
            tokio::task::spawn_blocking(move || entered_receiver.recv()),
        )
        .await
        .expect("platform reconciliation must reach the test latch")
        .unwrap()
        .unwrap();

        cancellation.cancel();
        release_sender.send(()).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(1), startup)
            .await
            .expect("cancelled startup must roll back promptly")
            .unwrap();
        assert!(matches!(result, Err(ResidentEngineStartError::Cancelled)));

        let request = RetentionRequest::new(
            RetentionPolicy::Unlimited,
            UtcTimestamp::now(),
            DEFAULT_RETENTION_BATCH_SIZE,
        )
        .unwrap();
        assert!(matches!(
            writer.apply_retention(request).await,
            Err(StorageError::WriterClosed)
        ));
    }
}
