use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use codex_monitor_engine::runtime::{
    LegacyDatabaseImporter, ResidentEngine, ResidentEngineCancellation, ResidentEngineConfig,
    ResidentEngineShutdownIssue, ResidentEngineStartError, ResidentWorkerMonitor,
    ResidentWorkerState,
};
use codex_monitor_engine::{SettingsMonitor, SettingsRuntime};
use codex_monitor_host::{HostLifecycleStateMachine, HostStartupState as StartupState};
use codex_monitor_telemetry::OtlpReceiverMonitor;
use tauri::{AppHandle, Manager as _, Runtime, Wry};
use tokio::sync::{Mutex, watch};
use tokio::time::{Duration, Instant};

use crate::browser;
use crate::tray::{TrayRuntimeState, TrayShutdownIssue, TrayShutdownReport};

#[cfg(target_os = "windows")]
const HOST_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(4);
#[cfg(not(target_os = "windows"))]
const HOST_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(target_os = "windows")]
const TRAY_SHUTDOWN_RESERVE: Duration = Duration::from_millis(500);
#[cfg(not(target_os = "windows"))]
const TRAY_SHUTDOWN_RESERVE: Duration = Duration::from_secs(1);
#[cfg(target_os = "windows")]
const ENGINE_ABORT_RESERVE: Duration = Duration::from_millis(250);
#[cfg(not(target_os = "windows"))]
const ENGINE_ABORT_RESERVE: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DashboardOpenAttemptError {
    EngineStopped,
    CredentialGeneration,
    PlatformDispatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DashboardOpenOutcome {
    Opened,
    Skipped,
    Failed(DashboardOpenAttemptError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HostShutdownIssue {
    StartupTimedOut,
    EngineOwnershipTimedOut,
    EngineOwnershipMismatch,
    EngineTaskFailed,
    EngineTaskTimedOut,
    EngineTaskAbortTimedOut,
}

impl HostShutdownIssue {
    const fn as_code(self) -> &'static str {
        match self {
            Self::StartupTimedOut => "startup_timed_out",
            Self::EngineOwnershipTimedOut => "engine_ownership_timed_out",
            Self::EngineOwnershipMismatch => "engine_ownership_mismatch",
            Self::EngineTaskFailed => "engine_task_failed",
            Self::EngineTaskTimedOut => "engine_task_timed_out",
            Self::EngineTaskAbortTimedOut => "engine_task_abort_timed_out",
        }
    }
}

struct DesktopLifecycleInner {
    engine: Mutex<Option<ResidentEngine>>,
    startup: watch::Sender<StartupState>,
    host_state: HostLifecycleStateMachine,
    startup_cancellation: ResidentEngineCancellation,
    shutdown_deadline: StdMutex<Option<Instant>>,
    native_exit_observed: AtomicBool,
    engine_shutdown_complete: watch::Sender<bool>,
    shutdown_complete: watch::Sender<bool>,
    exit_ready: AtomicBool,
    exit_code: AtomicI32,
}

/// Shared native lifecycle state. Only the contained `ResidentEngine` owns
/// engine resources; clones coordinate startup, failure observation, and exit.
#[derive(Clone)]
pub(crate) struct DesktopLifecycle {
    inner: Arc<DesktopLifecycleInner>,
}

pub(crate) struct ResidentPresentationMonitors {
    pub(crate) settings: SettingsMonitor,
    pub(crate) receiver: OtlpReceiverMonitor,
    pub(crate) workers: ResidentWorkerMonitor,
}

impl DesktopLifecycle {
    pub(crate) fn new() -> Self {
        let (startup, _) = watch::channel(StartupState::Starting);
        let (engine_shutdown_complete, _) = watch::channel(false);
        let (shutdown_complete, _) = watch::channel(false);
        Self {
            inner: Arc::new(DesktopLifecycleInner {
                engine: Mutex::new(None),
                startup,
                host_state: HostLifecycleStateMachine::new(),
                startup_cancellation: ResidentEngineCancellation::default(),
                shutdown_deadline: StdMutex::new(None),
                native_exit_observed: AtomicBool::new(false),
                engine_shutdown_complete,
                shutdown_complete,
                exit_ready: AtomicBool::new(false),
                exit_code: AtomicI32::new(0),
            }),
        }
    }

    async fn publish_ready(&self, engine: ResidentEngine) -> Result<(), ResidentEngine> {
        let mut slot = self.inner.engine.lock().await;
        *slot = Some(engine);
        if self.claim_ready() {
            self.inner.startup.send_replace(StartupState::Ready);
            Ok(())
        } else {
            Err(slot
                .take()
                .expect("startup completion must retain engine ownership"))
        }
    }

    fn claim_ready(&self) -> bool {
        self.inner.host_state.claim_ready()
    }

    fn publish_failed(&self) {
        self.inner.host_state.mark_failed();
        self.inner.startup.send_replace(StartupState::Failed);
        if self.inner.host_state.shutdown_started() {
            self.mark_engine_shutdown_complete();
        }
    }

    fn publish_cancelled(&self) {
        self.inner.host_state.mark_cancelled();
        self.inner.startup.send_replace(StartupState::Cancelled);
        if self.inner.host_state.shutdown_started() {
            self.mark_engine_shutdown_complete();
        }
    }

    fn startup_cancellation(&self) -> ResidentEngineCancellation {
        self.inner.startup_cancellation.clone()
    }

    fn cancel_startup(&self) {
        let _ = self.inner.host_state.request_startup_cancel();
        self.inner.startup_cancellation.cancel();
    }

    async fn wait_for_startup(&self) -> StartupState {
        let mut startup = self.inner.startup.subscribe();
        loop {
            let state = *startup.borrow();
            if matches!(
                state,
                StartupState::Ready | StartupState::Cancelled | StartupState::Failed
            ) {
                return state;
            }
            if startup.changed().await.is_err() {
                return StartupState::Failed;
            }
        }
    }

    pub(crate) async fn wait_for_presentation_monitors(
        &self,
    ) -> Option<ResidentPresentationMonitors> {
        if self.wait_for_startup().await != StartupState::Ready
            || self.inner.host_state.shutdown_started()
        {
            return None;
        }
        let engine = self.inner.engine.lock().await;
        if self.inner.host_state.shutdown_started() {
            return None;
        }
        let engine = engine.as_ref()?;
        Some(ResidentPresentationMonitors {
            settings: engine.settings().monitor(),
            receiver: engine.receiver_monitor(),
            workers: engine.worker_monitor(),
        })
    }

    pub(crate) async fn settings_runtime(&self) -> Option<SettingsRuntime> {
        if self.wait_for_startup().await != StartupState::Ready
            || self.inner.host_state.shutdown_started()
        {
            return None;
        }
        let engine = self.inner.engine.lock().await;
        if self.inner.host_state.shutdown_started() {
            return None;
        }
        engine.as_ref().map(ResidentEngine::settings)
    }

    pub(crate) async fn legacy_database_importer(&self) -> Option<LegacyDatabaseImporter> {
        if self.wait_for_startup().await != StartupState::Ready
            || self.inner.host_state.shutdown_started()
        {
            return None;
        }
        let engine = self.inner.engine.lock().await;
        if self.inner.host_state.shutdown_started() {
            return None;
        }
        engine.as_ref()?.legacy_database_importer()
    }

    fn begin_shutdown(&self, requested_deadline: Instant) -> (bool, Instant) {
        let mut deadline = self
            .inner
            .shutdown_deadline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = *deadline {
            return (false, existing);
        }
        *deadline = Some(requested_deadline);
        let _ = self.inner.host_state.begin_shutdown();
        (true, requested_deadline)
    }

    fn fixed_shutdown_deadline(&self) -> Option<Instant> {
        *self
            .inner
            .shutdown_deadline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn observe_native_exit(&self) {
        self.inner
            .native_exit_observed
            .store(true, Ordering::Release);
        self.cancel_startup();
    }

    fn native_exit_observed(&self) -> bool {
        self.inner.native_exit_observed.load(Ordering::Acquire)
    }

    fn mark_engine_shutdown_complete(&self) {
        self.inner.engine_shutdown_complete.send_replace(true);
    }

    pub(crate) async fn wait_for_engine_shutdown_before(&self, deadline: Instant) -> bool {
        let mut completion = self.inner.engine_shutdown_complete.subscribe();
        if *completion.borrow() {
            return true;
        }
        tokio::time::timeout_at(deadline, async {
            loop {
                if completion.changed().await.is_err() {
                    return false;
                }
                if *completion.borrow() {
                    return true;
                }
            }
        })
        .await
        .unwrap_or(false)
    }

    fn mark_shutdown_complete(&self) {
        self.inner.shutdown_complete.send_replace(true);
    }

    pub(crate) async fn wait_for_shutdown_before(&self, deadline: Instant) -> bool {
        let mut completion = self.inner.shutdown_complete.subscribe();
        if *completion.borrow() {
            return true;
        }
        tokio::time::timeout_at(deadline, async {
            loop {
                if completion.changed().await.is_err() {
                    return false;
                }
                if *completion.borrow() {
                    return true;
                }
            }
        })
        .await
        .unwrap_or(false)
    }

    pub(crate) fn exit_ready(&self) -> bool {
        self.inner.exit_ready.load(Ordering::Acquire)
    }

    fn allow_exit(&self) {
        self.inner.exit_ready.store(true, Ordering::Release);
    }

    fn record_exit_code(&self, exit_code: i32) {
        if exit_code != 0 {
            let _ = self.inner.exit_code.compare_exchange(
                0,
                exit_code,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }

    fn exit_code(&self) -> i32 {
        self.inner.exit_code.load(Ordering::Acquire)
    }
}

struct EngineShutdownOwner<T> {
    owner: Option<T>,
    lifecycle: DesktopLifecycle,
}

impl<T> EngineShutdownOwner<T> {
    fn new(owner: T, lifecycle: DesktopLifecycle) -> Self {
        Self {
            owner: Some(owner),
            lifecycle,
        }
    }

    fn owner_mut(&mut self) -> &mut T {
        self.owner
            .as_mut()
            .expect("engine shutdown owner must remain present until drop")
    }
}

impl<T> Drop for EngineShutdownOwner<T> {
    fn drop(&mut self) {
        drop(self.owner.take());
        self.lifecycle.mark_engine_shutdown_complete();
    }
}

async fn run_dashboard_open_request<F, Fut>(
    lifecycle: &DesktopLifecycle,
    attempt: F,
) -> DashboardOpenOutcome
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<(), DashboardOpenAttemptError>>,
{
    let startup = lifecycle.wait_for_startup().await;
    if startup != StartupState::Ready || lifecycle.inner.host_state.shutdown_started() {
        return DashboardOpenOutcome::Skipped;
    }
    match attempt().await {
        Ok(()) => DashboardOpenOutcome::Opened,
        Err(error) => DashboardOpenOutcome::Failed(error),
    }
}

pub(crate) async fn open_dashboard<R: Runtime>(app: &AppHandle<R>, lifecycle: &DesktopLifecycle) {
    let outcome = run_dashboard_open_request(lifecycle, || async {
        let url = {
            let engine = lifecycle.inner.engine.lock().await;
            let Some(engine) = engine.as_ref() else {
                return Err(DashboardOpenAttemptError::EngineStopped);
            };
            engine
                .fresh_dashboard_url()
                .map_err(|_| DashboardOpenAttemptError::CredentialGeneration)?
        };
        browser::open_dashboard(app, &url).map_err(|_| DashboardOpenAttemptError::PlatformDispatch)
    })
    .await;
    match outcome {
        DashboardOpenOutcome::Failed(DashboardOpenAttemptError::CredentialGeneration) => {
            eprintln!("codex_monitor_dashboard_url_failed");
        }
        DashboardOpenOutcome::Failed(DashboardOpenAttemptError::PlatformDispatch) => {
            eprintln!("codex_monitor_dashboard_open_failed");
        }
        DashboardOpenOutcome::Opened
        | DashboardOpenOutcome::Skipped
        | DashboardOpenOutcome::Failed(DashboardOpenAttemptError::EngineStopped) => {}
    }
}

pub(crate) fn start_resident_engine(
    app: AppHandle<Wry>,
    lifecycle: DesktopLifecycle,
    config: ResidentEngineConfig,
) {
    tauri::async_runtime::spawn(async move {
        let cancellation = lifecycle.startup_cancellation();
        match ResidentEngine::start_current_user(config, cancellation).await {
            Ok(engine) => {
                let worker_monitor = engine.worker_monitor();
                match lifecycle.publish_ready(engine).await {
                    Ok(()) => {
                        monitor_worker_failure(app, lifecycle, worker_monitor).await;
                    }
                    Err(mut engine) => {
                        let hard_deadline = lifecycle
                            .fixed_shutdown_deadline()
                            .unwrap_or_else(|| Instant::now() + HOST_SHUTDOWN_TIMEOUT);
                        let deadline = engine_shutdown_deadlines(hard_deadline).graceful;
                        let shutdown_issues = engine
                            .shutdown_before(deadline)
                            .await
                            .err()
                            .map(|error| error.issues().to_vec())
                            .unwrap_or_default();
                        drop(engine);
                        lifecycle.publish_cancelled();
                        if !shutdown_issues.is_empty() {
                            report_engine_shutdown_issues(&shutdown_issues);
                            lifecycle.record_exit_code(2);
                        }
                        request_shutdown(app, lifecycle, 0);
                    }
                }
            }
            Err(ResidentEngineStartError::Cancelled) => {
                lifecycle.publish_cancelled();
                request_shutdown(app, lifecycle, 0);
            }
            Err(error) => {
                eprintln!("codex_monitor_engine_start_failed:{}", error.as_code());
                lifecycle.publish_failed();
                request_shutdown(app, lifecycle, 2);
            }
        }
    });
}

async fn monitor_worker_failure(
    app: AppHandle<Wry>,
    lifecycle: DesktopLifecycle,
    mut monitor: ResidentWorkerMonitor,
) {
    if monitor.snapshot() == ResidentWorkerState::Failed {
        request_shutdown(app, lifecycle, 2);
        return;
    }
    while let Some(state) = monitor.changed().await {
        match state {
            ResidentWorkerState::Failed => {
                eprintln!("codex_monitor_engine_worker_failed");
                request_shutdown(app, lifecycle, 2);
                return;
            }
            ResidentWorkerState::Stopped => return,
            ResidentWorkerState::Running => {}
        }
    }
}

async fn shutdown_engine_before(
    lifecycle: &DesktopLifecycle,
    deadline: Instant,
) -> Result<Vec<ResidentEngineShutdownIssue>, HostShutdownIssue> {
    let startup = tokio::time::timeout_at(deadline, lifecycle.wait_for_startup())
        .await
        .map_err(|_| HostShutdownIssue::StartupTimedOut)?;
    let mut engine_slot = tokio::time::timeout_at(deadline, lifecycle.inner.engine.lock())
        .await
        .map_err(|_| HostShutdownIssue::EngineOwnershipTimedOut)?;
    let engine = engine_slot.take();
    drop(engine_slot);

    let Some(engine) = engine else {
        if startup == StartupState::Ready {
            return Err(HostShutdownIssue::EngineOwnershipMismatch);
        }
        lifecycle.mark_engine_shutdown_complete();
        return Ok(Vec::new());
    };
    let mut engine = EngineShutdownOwner::new(engine, lifecycle.clone());
    match engine.owner_mut().shutdown_before(deadline).await {
        Ok(()) => Ok(Vec::new()),
        Err(error) => Ok(error.issues().to_vec()),
    }
}

fn report_engine_shutdown_issues(issues: &[ResidentEngineShutdownIssue]) {
    for issue in issues {
        eprintln!(
            "codex_monitor_engine_shutdown_issue:{}:{}",
            issue.stage().as_code(),
            issue.kind().as_code()
        );
    }
}

fn report_host_shutdown_issue(issue: HostShutdownIssue) {
    eprintln!("codex_monitor_host_shutdown_issue:{}", issue.as_code());
}

fn report_tray_shutdown_issues(issues: &[TrayShutdownIssue]) {
    for issue in issues {
        eprintln!("codex_monitor_tray_shutdown_issue:{}", issue.as_code());
    }
}

fn report_shutdown_diagnostics(
    engine_issues: Vec<ResidentEngineShutdownIssue>,
    host_issues: Vec<HostShutdownIssue>,
    tray_issues: Vec<TrayShutdownIssue>,
) {
    if engine_issues.is_empty() && host_issues.is_empty() && tray_issues.is_empty() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("codex-monitor-shutdown-diagnostics".to_owned())
        .spawn(move || {
            report_engine_shutdown_issues(&engine_issues);
            for issue in host_issues {
                report_host_shutdown_issue(issue);
            }
            report_tray_shutdown_issues(&tray_issues);
        });
}

#[derive(Clone, Copy)]
struct EngineShutdownDeadlines {
    graceful: Instant,
    abort: Instant,
}

fn engine_shutdown_deadlines(hard_deadline: Instant) -> EngineShutdownDeadlines {
    let abort = hard_deadline
        .checked_sub(TRAY_SHUTDOWN_RESERVE)
        .unwrap_or(hard_deadline);
    let graceful = abort.checked_sub(ENGINE_ABORT_RESERVE).unwrap_or(abort);
    EngineShutdownDeadlines { graceful, abort }
}

#[derive(Default)]
struct EngineShutdownReport {
    engine_issues: Vec<ResidentEngineShutdownIssue>,
    host_issues: Vec<HostShutdownIssue>,
}

impl EngineShutdownReport {
    fn push_host_issue(&mut self, issue: HostShutdownIssue) {
        if !self.host_issues.contains(&issue) {
            self.host_issues.push(issue);
        }
    }
}

async fn supervise_engine_shutdown(
    lifecycle: &DesktopLifecycle,
    deadlines: EngineShutdownDeadlines,
) -> EngineShutdownReport {
    let mut report = EngineShutdownReport::default();
    let engine_lifecycle = lifecycle.clone();
    let mut shutdown_task = tauri::async_runtime::spawn(async move {
        shutdown_engine_before(&engine_lifecycle, deadlines.graceful).await
    });
    match tokio::time::timeout_at(deadlines.graceful, &mut shutdown_task).await {
        Ok(Ok(Ok(issues))) => report.engine_issues = issues,
        Ok(Ok(Err(issue))) => report.push_host_issue(issue),
        Ok(Err(_)) => report.push_host_issue(HostShutdownIssue::EngineTaskFailed),
        Err(_) => {
            report.push_host_issue(HostShutdownIssue::EngineTaskTimedOut);
            shutdown_task.abort();
            if tokio::time::timeout_at(deadlines.abort, &mut shutdown_task)
                .await
                .is_err()
            {
                report.push_host_issue(HostShutdownIssue::EngineTaskAbortTimedOut);
            }
        }
    }
    report
}

pub(crate) fn request_shutdown(
    app: AppHandle<Wry>,
    lifecycle: DesktopLifecycle,
    exit_code: i32,
) -> Instant {
    lifecycle.record_exit_code(exit_code);
    let requested_deadline = Instant::now() + HOST_SHUTDOWN_TIMEOUT;
    let (started, hard_deadline) = lifecycle.begin_shutdown(requested_deadline);
    lifecycle.cancel_startup();
    if let Some(tray) = app.try_state::<TrayRuntimeState>() {
        tray.begin_shutdown();
        if lifecycle.native_exit_observed() {
            tray.prepare_native_exit(&app);
        }
    }
    if !started {
        return hard_deadline;
    }
    tauri::async_runtime::spawn(async move {
        let mut engine_report =
            supervise_engine_shutdown(&lifecycle, engine_shutdown_deadlines(hard_deadline)).await;

        let tray = app
            .try_state::<TrayRuntimeState>()
            .and_then(|state| state.take());
        let tray_report = match tray {
            Some(tray) if lifecycle.native_exit_observed() => {
                tray.shutdown_for_tauri_exit(hard_deadline).await
            }
            Some(tray) => tray.shutdown(app.clone(), hard_deadline).await,
            None => TrayShutdownReport::released(),
        };
        let tray_cleanup_confirmed = tray_report.cleanup_confirmed();
        let tray_issues = tray_report.into_issues();
        let engine_released = lifecycle
            .wait_for_engine_shutdown_before(hard_deadline)
            .await;
        if !engine_released {
            engine_report.push_host_issue(HostShutdownIssue::EngineTaskAbortTimedOut);
        }
        if !engine_report.engine_issues.is_empty()
            || !engine_report.host_issues.is_empty()
            || !tray_issues.is_empty()
        {
            lifecycle.record_exit_code(2);
        }
        report_shutdown_diagnostics(
            engine_report.engine_issues,
            engine_report.host_issues,
            tray_issues,
        );
        if engine_released && tray_cleanup_confirmed {
            lifecycle.mark_shutdown_complete();
        }
        lifecycle.allow_exit();
        if !lifecycle.native_exit_observed() {
            app.exit(lifecycle.exit_code());
        }
    });
    hard_deadline
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    struct ShutdownDropProbe(Arc<AtomicUsize>);

    impl Drop for ShutdownDropProbe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn publish_test_startup(lifecycle: &DesktopLifecycle, state: StartupState) {
        match state {
            StartupState::Starting => {}
            StartupState::CancelRequested => {
                let _ = lifecycle.inner.host_state.request_startup_cancel();
            }
            StartupState::Ready => {
                let _ = lifecycle.inner.host_state.claim_ready();
            }
            StartupState::Cancelled => lifecycle.inner.host_state.mark_cancelled(),
            StartupState::Failed => lifecycle.inner.host_state.mark_failed(),
        }
        lifecycle.inner.startup.send_replace(state);
    }

    #[tokio::test]
    async fn startup_and_shutdown_transitions_are_monotonic_and_idempotent() {
        let lifecycle = DesktopLifecycle::new();
        assert!(!lifecycle.exit_ready());
        assert_eq!(lifecycle.exit_code(), 0);
        assert!(!lifecycle.startup_cancellation().is_cancelled());
        lifecycle.cancel_startup();
        assert!(lifecycle.startup_cancellation().is_cancelled());
        let first_deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(
            lifecycle.begin_shutdown(first_deadline),
            (true, first_deadline)
        );
        assert_eq!(
            lifecycle.begin_shutdown(first_deadline + Duration::from_secs(1)),
            (false, first_deadline)
        );

        lifecycle.record_exit_code(2);
        lifecycle.record_exit_code(3);
        assert_eq!(lifecycle.exit_code(), 2);

        lifecycle.publish_failed();
        assert_eq!(lifecycle.wait_for_startup().await, StartupState::Failed);
        lifecycle.allow_exit();
        assert!(lifecycle.exit_ready());
    }

    #[tokio::test]
    async fn startup_cancellation_is_terminal_without_publishing_ready() {
        let lifecycle = DesktopLifecycle::new();

        lifecycle.cancel_startup();
        assert!(!lifecycle.claim_ready());
        lifecycle.publish_cancelled();

        assert!(lifecycle.startup_cancellation().is_cancelled());
        assert_eq!(lifecycle.wait_for_startup().await, StartupState::Cancelled);
        assert!(lifecycle.inner.engine.lock().await.is_none());
    }

    #[tokio::test]
    async fn dashboard_open_waits_for_ready_and_platform_failure_is_nonfatal() {
        let lifecycle = DesktopLifecycle::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let request_lifecycle = lifecycle.clone();
        let request_calls = Arc::clone(&calls);
        let request = tokio::spawn(async move {
            run_dashboard_open_request(&request_lifecycle, || async move {
                request_calls.fetch_add(1, Ordering::AcqRel);
                Err(DashboardOpenAttemptError::PlatformDispatch)
            })
            .await
        });

        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::Acquire), 0);
        publish_test_startup(&lifecycle, StartupState::Ready);

        assert_eq!(
            request.await.unwrap(),
            DashboardOpenOutcome::Failed(DashboardOpenAttemptError::PlatformDispatch)
        );
        assert_eq!(calls.load(Ordering::Acquire), 1);
        assert!(!lifecycle.inner.host_state.shutdown_started());
        assert_eq!(lifecycle.exit_code(), 0);
    }

    #[tokio::test]
    async fn dashboard_open_skips_failed_cancelled_and_shutting_down_lifecycles() {
        for state in [StartupState::Failed, StartupState::Cancelled] {
            let lifecycle = DesktopLifecycle::new();
            publish_test_startup(&lifecycle, state);
            let calls = AtomicUsize::new(0);

            let outcome = run_dashboard_open_request(&lifecycle, || async {
                calls.fetch_add(1, Ordering::AcqRel);
                Ok(())
            })
            .await;

            assert_eq!(outcome, DashboardOpenOutcome::Skipped);
            assert_eq!(calls.load(Ordering::Acquire), 0);
        }

        let lifecycle = DesktopLifecycle::new();
        publish_test_startup(&lifecycle, StartupState::Ready);
        assert!(
            lifecycle
                .begin_shutdown(Instant::now() + Duration::from_secs(1))
                .0
        );
        let calls = AtomicUsize::new(0);
        let outcome = run_dashboard_open_request(&lifecycle, || async {
            calls.fetch_add(1, Ordering::AcqRel);
            Ok(())
        })
        .await;
        assert_eq!(outcome, DashboardOpenOutcome::Skipped);
        assert_eq!(calls.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn engine_shutdown_wait_is_bounded_while_startup_is_still_running() {
        let lifecycle = DesktopLifecycle::new();
        let deadline = Instant::now() + Duration::from_millis(20);

        assert_eq!(
            shutdown_engine_before(&lifecycle, deadline).await,
            Err(HostShutdownIssue::StartupTimedOut)
        );
        assert!(!*lifecycle.inner.engine_shutdown_complete.borrow());
    }

    #[tokio::test]
    async fn terminal_startup_without_an_engine_publishes_release() {
        let lifecycle = DesktopLifecycle::new();
        publish_test_startup(&lifecycle, StartupState::Cancelled);

        assert_eq!(
            shutdown_engine_before(&lifecycle, Instant::now() + Duration::from_millis(20)).await,
            Ok(Vec::new())
        );
        assert!(*lifecycle.inner.engine_shutdown_complete.borrow());
    }

    #[tokio::test]
    async fn native_exit_observation_exposes_engine_and_full_shutdown_completion() {
        let lifecycle = DesktopLifecycle::new();
        lifecycle.observe_native_exit();
        assert!(lifecycle.native_exit_observed());
        assert!(lifecycle.startup_cancellation().is_cancelled());

        let waiting = {
            let lifecycle = lifecycle.clone();
            tokio::spawn(async move {
                lifecycle
                    .wait_for_engine_shutdown_before(Instant::now() + Duration::from_secs(1))
                    .await
            })
        };
        tokio::task::yield_now().await;
        lifecycle.mark_engine_shutdown_complete();
        assert!(waiting.await.unwrap());

        let shutdown_waiting = {
            let lifecycle = lifecycle.clone();
            tokio::spawn(async move {
                lifecycle
                    .wait_for_shutdown_before(Instant::now() + Duration::from_secs(1))
                    .await
            })
        };
        tokio::task::yield_now().await;
        lifecycle.mark_shutdown_complete();
        assert!(shutdown_waiting.await.unwrap());
    }

    #[tokio::test]
    async fn cancelling_an_engine_shutdown_task_publishes_release_after_drop() {
        let lifecycle = DesktopLifecycle::new();
        let drops = Arc::new(AtomicUsize::new(0));
        let task_lifecycle = lifecycle.clone();
        let task_drops = Arc::clone(&drops);
        let task = tokio::spawn(async move {
            let _owner = EngineShutdownOwner::new(ShutdownDropProbe(task_drops), task_lifecycle);
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;

        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(drops.load(Ordering::Acquire), 1);
        assert!(
            lifecycle
                .wait_for_engine_shutdown_before(Instant::now() + Duration::from_secs(1))
                .await
        );
    }

    #[test]
    fn engine_deadlines_reserve_abort_acknowledgement_and_tray_cleanup() {
        let hard = Instant::now() + Duration::from_secs(30);
        let deadlines = engine_shutdown_deadlines(hard);

        assert_eq!(hard.duration_since(deadlines.abort), TRAY_SHUTDOWN_RESERVE);
        assert_eq!(
            deadlines.abort.duration_since(deadlines.graceful),
            ENGINE_ABORT_RESERVE
        );
    }
}
