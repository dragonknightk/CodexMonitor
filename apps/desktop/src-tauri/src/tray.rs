use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use codex_monitor_dashboard::{
    DashboardPlatformCapabilities, DashboardPlatformCapabilityMonitor,
    DashboardPlatformCapabilitySnapshot, SystemNotificationAvailability,
};
use codex_monitor_engine::SettingsMonitor;
use codex_monitor_engine::runtime::{ResidentWorkerMonitor, ResidentWorkerState};
use codex_monitor_host::{
    HostCoordinator, HostLocale, HostNotificationAvailability, HostPlatformSnapshot,
    HostReceiverState, HostSettingsDocument, HostSettingsToggle, HostTrayAction, HostTrayPort,
    HostTrayStatus as TrayStatus, HostTrayViewModel as TrayViewModel, HostWorkerState,
};
use codex_monitor_storage::AppLocale;
use codex_monitor_telemetry::{OtlpReceiverBindState, OtlpReceiverMonitor};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{App, AppHandle, Wry};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::host::{host_settings_document, spawn_host_command, tauri_host_coordinator};
use crate::host_catalog::{TrayStatusCatalog, catalog};
use crate::lifecycle::DesktopLifecycle;

const TRAY_ID: &str = "codex-monitor-tray";
const STATUS_MENU_ID: &str = "codex-monitor-status";
const OPEN_DASHBOARD_MENU_ID: &str = "codex-monitor-open-dashboard";
const LOGIN_AUTOSTART_MENU_ID: &str = "codex-monitor-login-autostart";
const REASONING_NOTIFICATIONS_MENU_ID: &str = "codex-monitor-reasoning-notifications";
const SETTINGS_MENU_ID: &str = "codex-monitor-settings";
const QUIT_MENU_ID: &str = "codex-monitor-quit";

pub(crate) fn host_action_for_menu_id(id: &str) -> Option<HostTrayAction> {
    match id {
        OPEN_DASHBOARD_MENU_ID => Some(HostTrayAction::OpenDashboard),
        LOGIN_AUTOSTART_MENU_ID => Some(HostTrayAction::ToggleLoginAutostart),
        REASONING_NOTIFICATIONS_MENU_ID => Some(HostTrayAction::ToggleReasoningNotifications),
        SETTINGS_MENU_ID => Some(HostTrayAction::OpenSettings),
        QUIT_MENU_ID => Some(HostTrayAction::Quit),
        _ => None,
    }
}

#[derive(Debug, Eq, PartialEq)]
struct TrayLabels {
    tooltip: String,
    status: String,
    open_dashboard: String,
    start_at_login: String,
    reasoning_notifications: String,
    settings: String,
    quit: String,
}

impl TrayLabels {
    fn new(locale: HostLocale, status: TrayStatus) -> Self {
        let catalog = catalog(locale);
        let status_text = status_text(&catalog.tray.status, status);
        Self {
            tooltip: interpolate_status(&catalog.tray.tooltip, status_text),
            status: interpolate_status(&catalog.tray.menu.status, status_text),
            open_dashboard: catalog.tray.menu.open_dashboard.clone(),
            start_at_login: catalog.tray.menu.start_at_login.clone(),
            reasoning_notifications: catalog.tray.menu.reasoning_notifications.clone(),
            settings: catalog.tray.menu.settings.clone(),
            quit: catalog.tray.menu.quit.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TrayInstallError {
    Unavailable,
}

impl fmt::Display for TrayInstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("system tray unavailable")
    }
}

impl std::error::Error for TrayInstallError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TrayShutdownIssue {
    PresentationTaskFailed,
    PresentationTaskTimedOut,
    MainThreadDispatchFailed,
    RemovalFailed,
    RemovalTimedOut,
}

pub(crate) struct TrayShutdownReport {
    issues: Vec<TrayShutdownIssue>,
    cleanup_confirmed: bool,
}

impl TrayShutdownReport {
    pub(crate) const fn released() -> Self {
        Self {
            issues: Vec::new(),
            cleanup_confirmed: true,
        }
    }

    pub(crate) const fn cleanup_confirmed(&self) -> bool {
        self.cleanup_confirmed
    }

    pub(crate) fn into_issues(self) -> Vec<TrayShutdownIssue> {
        self.issues
    }
}

impl TrayShutdownIssue {
    pub(crate) const fn as_code(self) -> &'static str {
        match self {
            Self::PresentationTaskFailed => "presentation_task_failed",
            Self::PresentationTaskTimedOut => "presentation_task_timed_out",
            Self::MainThreadDispatchFailed => "main_thread_dispatch_failed",
            Self::RemovalFailed => "removal_failed",
            Self::RemovalTimedOut => "removal_timed_out",
        }
    }
}

#[derive(Clone)]
struct TrayRenderTarget {
    tray: TrayIcon<Wry>,
    status: MenuItem<Wry>,
    open_dashboard: MenuItem<Wry>,
    login_autostart: CheckMenuItem<Wry>,
    reasoning_notifications: CheckMenuItem<Wry>,
    settings: MenuItem<Wry>,
    quit: MenuItem<Wry>,
}

impl TrayRenderTarget {
    fn render(&self, request: TrayRenderRequest) -> bool {
        let labels = TrayLabels::new(request.view.locale(), request.view.status());
        let mut failed = false;

        failed |= self.status.set_text(&labels.status).is_err();
        failed |= self
            .open_dashboard
            .set_text(&labels.open_dashboard)
            .is_err();
        failed |= self
            .login_autostart
            .set_text(&labels.start_at_login)
            .is_err();
        failed |= self
            .reasoning_notifications
            .set_text(&labels.reasoning_notifications)
            .is_err();
        failed |= self.settings.set_text(&labels.settings).is_err();
        failed |= self.quit.set_text(&labels.quit).is_err();
        failed |= self.quit.set_enabled(request.view.quit_enabled()).is_err();
        failed |= self
            .open_dashboard
            .set_enabled(request.view.controls_enabled())
            .is_err();
        failed |= self
            .login_autostart
            .set_enabled(
                request.view.controls_enabled()
                    && request.view.login_autostart_control_enabled()
                    && request.mutation_idle,
            )
            .is_err();
        failed |= self
            .reasoning_notifications
            .set_enabled(request.view.controls_enabled() && request.mutation_idle)
            .is_err();
        failed |= self
            .settings
            .set_enabled(request.view.controls_enabled())
            .is_err();
        failed |= self
            .login_autostart
            .set_checked(request.view.login_autostart_enabled())
            .is_err();
        failed |= self
            .reasoning_notifications
            .set_checked(request.view.reasoning_notifications_enabled())
            .is_err();
        #[cfg(not(target_os = "linux"))]
        {
            failed |= self.tray.set_tooltip(Some(&labels.tooltip)).is_err();
        }

        failed
    }
}

pub(crate) struct TrayControllerState {
    view: TrayViewModel,
    settings_mutation: bool,
    shutting_down: bool,
    render_generation: u64,
}

impl TrayControllerState {
    pub(crate) const fn new(view: TrayViewModel) -> Self {
        Self {
            view,
            settings_mutation: false,
            shutting_down: false,
            render_generation: 0,
        }
    }

    pub(crate) fn update_view(&mut self, mut view: TrayViewModel) {
        if self.shutting_down {
            view = view.shutdown_requested();
        }
        self.view = view;
    }

    fn claim_settings_mutation(&mut self) -> bool {
        if self.shutting_down || self.settings_mutation {
            return false;
        }
        self.settings_mutation = true;
        true
    }

    pub(crate) fn begin_settings_mutation(&mut self, toggle: HostSettingsToggle) -> Option<bool> {
        if !self.claim_settings_mutation() {
            return None;
        }
        Some(match toggle {
            HostSettingsToggle::LoginAutostart => !self.view.login_autostart_enabled(),
            HostSettingsToggle::ReasoningNotifications => {
                !self.view.reasoning_notifications_enabled()
            }
        })
    }

    pub(crate) fn finish_settings_mutation(&mut self, document: Option<&HostSettingsDocument>) {
        if let Some(document) = document {
            self.view = self.view.with_settings(document);
        }
        self.settings_mutation = false;
    }

    pub(crate) fn request_shutdown(&mut self) -> bool {
        if self.shutting_down {
            return false;
        }
        self.shutting_down = true;
        self.view = self.view.shutdown_requested();
        true
    }

    pub(crate) fn mark_stopped(&mut self) {
        self.shutting_down = true;
        self.view = self.view.stopped();
    }

    fn next_render(&mut self, latest_generation: &AtomicU64) -> TrayRenderRequest {
        self.render_generation = self
            .render_generation
            .checked_add(1)
            .expect("tray render generation must not overflow");
        latest_generation.store(self.render_generation, Ordering::Release);
        TrayRenderRequest {
            generation: self.render_generation,
            view: self.view,
            mutation_idle: !self.settings_mutation,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TrayRenderRequest {
    generation: u64,
    view: TrayViewModel,
    mutation_idle: bool,
}

impl TrayRenderRequest {
    fn should_render(self, latest_generation: &AtomicU64, disposed: &AtomicBool) -> bool {
        !disposed.load(Ordering::Acquire)
            && self.generation == latest_generation.load(Ordering::Acquire)
    }
}

struct TrayController {
    target: Arc<TrayRenderTarget>,
    state: Mutex<TrayControllerState>,
    render_generation: Arc<AtomicU64>,
    render_failure_reported: Arc<AtomicBool>,
    disposed: Arc<AtomicBool>,
}

impl TrayController {
    fn apply(&self, view: TrayViewModel) {
        let request = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.update_view(view);
            state.next_render(&self.render_generation)
        };
        self.schedule_render(request);
    }

    fn mark_stopped(&self) {
        let request = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.mark_stopped();
            state.next_render(&self.render_generation)
        };
        self.schedule_render(request);
    }

    fn begin_settings_mutation(&self, toggle: HostSettingsToggle) -> Option<bool> {
        let (requested, request) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let requested = state.begin_settings_mutation(toggle)?;
            let request = state.next_render(&self.render_generation);
            (requested, request)
        };
        self.schedule_render(request);
        Some(requested)
    }

    fn begin_shutdown(&self) -> bool {
        let Some(request) = ({
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state
                .request_shutdown()
                .then(|| state.next_render(&self.render_generation))
        }) else {
            return false;
        };
        self.schedule_render(request);
        true
    }

    fn dispose(&self) {
        if self.disposed.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.request_shutdown();
        let _ = state.next_render(&self.render_generation);
    }

    fn finish_settings_mutation(&self, document: Option<&HostSettingsDocument>) {
        let request = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.finish_settings_mutation(document);
            state.next_render(&self.render_generation)
        };
        self.schedule_render(request);
    }

    fn schedule_render(&self, request: TrayRenderRequest) {
        if self.disposed.load(Ordering::Acquire) {
            return;
        }
        let app = self.target.tray.app_handle().clone();
        let target = Arc::downgrade(&self.target);
        let latest_generation = Arc::clone(&self.render_generation);
        let disposed = Arc::clone(&self.disposed);
        let render_failure_reported = Arc::clone(&self.render_failure_reported);
        let task_failure_reported = Arc::clone(&render_failure_reported);
        let scheduled = app.run_on_main_thread(move || {
            if !request.should_render(&latest_generation, &disposed) {
                return;
            }
            let Some(target) = target.upgrade() else {
                return;
            };
            if target.render(request) {
                report_render_failure(&task_failure_reported);
            }
        });
        if scheduled.is_err() {
            report_render_failure(&render_failure_reported);
        }
    }
}

impl HostTrayPort for TrayController {
    fn begin_settings_mutation(&self, toggle: HostSettingsToggle) -> Option<bool> {
        TrayController::begin_settings_mutation(self, toggle)
    }

    fn finish_settings_mutation(&self, document: Option<&HostSettingsDocument>) {
        TrayController::finish_settings_mutation(self, document);
    }

    fn begin_shutdown(&self) -> bool {
        TrayController::begin_shutdown(self)
    }
}

pub(crate) trait TrayPresentationPort: Send + Sync + 'static {
    fn apply(&self, view: TrayViewModel);
    fn mark_stopped(&self);
}

impl TrayPresentationPort for TrayController {
    fn apply(&self, view: TrayViewModel) {
        TrayController::apply(self, view);
    }

    fn mark_stopped(&self) {
        TrayController::mark_stopped(self);
    }
}

pub(crate) struct TrayRuntime {
    controller: Arc<TrayController>,
    presentation: Option<tauri::async_runtime::JoinHandle<()>>,
    removal: Arc<TrayRemovalState>,
}

struct TrayRemovalState {
    outcome: watch::Sender<Option<bool>>,
}

impl TrayRemovalState {
    fn new() -> Self {
        let (outcome, _) = watch::channel(None);
        Self { outcome }
    }

    fn complete(&self, succeeded: bool) {
        self.outcome
            .send_if_modified(|outcome| match (*outcome, succeeded) {
                (None, _) => {
                    *outcome = Some(succeeded);
                    true
                }
                (Some(false), true) => {
                    *outcome = Some(true);
                    true
                }
                (Some(false), false) | (Some(true), _) => false,
            });
    }

    fn succeeded(&self) -> bool {
        *self.outcome.borrow() == Some(true)
    }

    async fn wait_before(&self, deadline: Instant) -> Option<bool> {
        let mut outcome = self.outcome.subscribe();
        loop {
            if *outcome.borrow() == Some(true) {
                return Some(true);
            }
            if Instant::now() >= deadline {
                return *outcome.borrow();
            }
            match tokio::time::timeout_at(deadline, outcome.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) | Err(_) => return *outcome.borrow(),
            }
        }
    }
}

/// Tauri-managed owner for the optional platform tray runtime.
pub(crate) struct TrayRuntimeState {
    runtime: Mutex<Option<TrayRuntime>>,
    native_controller: Weak<TrayController>,
    removal: Option<Arc<TrayRemovalState>>,
}

impl TrayRuntimeState {
    pub(crate) fn new(runtime: Option<TrayRuntime>) -> Self {
        let native_controller = runtime
            .as_ref()
            .map_or_else(Weak::new, |runtime| Arc::downgrade(&runtime.controller));
        let removal = runtime.as_ref().map(|runtime| Arc::clone(&runtime.removal));
        Self {
            runtime: Mutex::new(runtime),
            native_controller,
            removal,
        }
    }

    pub(crate) fn begin_shutdown(&self) {
        if let Some(runtime) = self
            .runtime
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            runtime.begin_shutdown();
        }
    }

    pub(crate) fn take(&self) -> Option<TrayRuntime> {
        self.runtime
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// Invalidate pending renders and hide the native icon synchronously from
    /// Tauri's main-thread `RunEvent::Exit` callback. The weak controller also
    /// covers a runtime already moved into the async shutdown supervisor.
    pub(crate) fn prepare_native_exit(&self, app: &AppHandle<Wry>) {
        if let Some(runtime) = self
            .runtime
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
        {
            runtime.begin_shutdown();
            runtime.controller.dispose();
            if let Some(presentation) = runtime.presentation.as_ref() {
                presentation.abort();
            }
        } else if let Some(controller) = self.native_controller.upgrade() {
            controller.dispose();
        }

        if let Some(removal) = self.removal.as_ref() {
            removal.complete(remove_tray_on_current_thread(app));
        }
    }
}

impl TrayRuntime {
    pub(crate) fn begin_shutdown(&self) {
        self.controller.begin_shutdown();
    }

    pub(crate) async fn shutdown(
        mut self,
        app: AppHandle<Wry>,
        deadline: Instant,
    ) -> TrayShutdownReport {
        let mut issues = Vec::new();
        self.begin_shutdown();
        self.controller.dispose();
        let presentation_released = self.stop_presentation_before(deadline, &mut issues).await;

        let dispatch_failed = if self.removal.succeeded() {
            false
        } else {
            let cleanup_app = app.clone();
            let removal = Arc::clone(&self.removal);
            app.run_on_main_thread(move || {
                removal.complete(remove_tray_on_current_thread(&cleanup_app));
            })
            .is_err()
        };
        let removal = self.removal.wait_before(deadline).await;
        if dispatch_failed && removal != Some(true) {
            issues.push(TrayShutdownIssue::MainThreadDispatchFailed);
        }
        match removal {
            Some(true) => {}
            Some(false) => issues.push(TrayShutdownIssue::RemovalFailed),
            None => issues.push(TrayShutdownIssue::RemovalTimedOut),
        }

        TrayShutdownReport {
            issues,
            cleanup_confirmed: presentation_released && removal == Some(true),
        }
    }

    /// Stop host-owned tray work after the main-thread exit callback has already
    /// hidden the icon and detached it from Tauri's resource registry. This
    /// path must not enqueue work back onto the destroyed event loop.
    pub(crate) async fn shutdown_for_tauri_exit(mut self, deadline: Instant) -> TrayShutdownReport {
        let mut issues = Vec::new();
        self.begin_shutdown();
        self.controller.dispose();
        let presentation_released = self.stop_presentation_before(deadline, &mut issues).await;
        let removal = self.removal.wait_before(deadline).await;
        match removal {
            Some(true) => {}
            Some(false) => issues.push(TrayShutdownIssue::RemovalFailed),
            None => issues.push(TrayShutdownIssue::RemovalTimedOut),
        }
        TrayShutdownReport {
            issues,
            cleanup_confirmed: presentation_released && removal == Some(true),
        }
    }

    async fn stop_presentation_before(
        &mut self,
        deadline: Instant,
        issues: &mut Vec<TrayShutdownIssue>,
    ) -> bool {
        let Some(mut presentation) = self.presentation.take() else {
            return true;
        };
        presentation.abort();
        match tokio::time::timeout_at(deadline, &mut presentation).await {
            Ok(Ok(())) => true,
            Ok(Err(tauri::Error::JoinError(error))) if error.is_cancelled() => true,
            Ok(Err(_)) => {
                issues.push(TrayShutdownIssue::PresentationTaskFailed);
                true
            }
            Err(_) => {
                issues.push(TrayShutdownIssue::PresentationTaskTimedOut);
                false
            }
        }
    }
}

fn remove_tray_on_current_thread(app: &AppHandle<Wry>) -> bool {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return true;
    };
    let hidden = tray.set_visible(false).is_ok();
    drop(tray);
    let removed = app.remove_tray_by_id(TRAY_ID);
    let removed_from_registry = removed.is_some();
    drop(removed);
    hidden && removed_from_registry
}

impl Drop for TrayRuntime {
    fn drop(&mut self) {
        self.begin_shutdown();
        self.controller.dispose();
        if let Some(presentation) = self.presentation.as_ref() {
            presentation.abort();
        }
    }
}

fn report_render_failure(reported: &AtomicBool) {
    if !reported.swap(true, Ordering::AcqRel) {
        eprintln!("codex_monitor_tray_update_failed");
    }
}

pub(crate) fn install(
    app: &App,
    lifecycle: DesktopLifecycle,
    initial_locale: AppLocale,
    platform: DashboardPlatformCapabilities,
) -> Result<TrayRuntime, TrayInstallError> {
    let initial_state = TrayViewModel::starting(host_locale(initial_locale));
    let controller = build_controller(app, initial_state)?;
    register_menu_handler(&controller, lifecycle.clone());
    let presentation = tauri::async_runtime::spawn(run_presentation_monitor(
        Arc::clone(&controller),
        lifecycle,
        platform,
    ));
    let removal = Arc::new(TrayRemovalState::new());
    Ok(TrayRuntime {
        controller,
        presentation: Some(presentation),
        removal,
    })
}

fn build_controller(
    app: &App,
    initial_state: TrayViewModel,
) -> Result<Arc<TrayController>, TrayInstallError> {
    let labels = TrayLabels::new(initial_state.locale(), initial_state.status());
    let status = MenuItem::with_id(app, STATUS_MENU_ID, &labels.status, false, None::<&str>)
        .map_err(|_| TrayInstallError::Unavailable)?;
    let open_dashboard = MenuItem::with_id(
        app,
        OPEN_DASHBOARD_MENU_ID,
        &labels.open_dashboard,
        false,
        None::<&str>,
    )
    .map_err(|_| TrayInstallError::Unavailable)?;
    let login_autostart = CheckMenuItem::with_id(
        app,
        LOGIN_AUTOSTART_MENU_ID,
        &labels.start_at_login,
        false,
        false,
        None::<&str>,
    )
    .map_err(|_| TrayInstallError::Unavailable)?;
    let reasoning_notifications = CheckMenuItem::with_id(
        app,
        REASONING_NOTIFICATIONS_MENU_ID,
        &labels.reasoning_notifications,
        false,
        false,
        None::<&str>,
    )
    .map_err(|_| TrayInstallError::Unavailable)?;
    let settings = MenuItem::with_id(app, SETTINGS_MENU_ID, &labels.settings, false, None::<&str>)
        .map_err(|_| TrayInstallError::Unavailable)?;
    let quit = MenuItem::with_id(app, QUIT_MENU_ID, &labels.quit, true, None::<&str>)
        .map_err(|_| TrayInstallError::Unavailable)?;
    let status_separator =
        PredefinedMenuItem::separator(app).map_err(|_| TrayInstallError::Unavailable)?;
    let quit_separator =
        PredefinedMenuItem::separator(app).map_err(|_| TrayInstallError::Unavailable)?;
    let menu = Menu::with_items(
        app,
        &[
            &status,
            &status_separator,
            &open_dashboard,
            &login_autostart,
            &reasoning_notifications,
            &settings,
            &quit_separator,
            &quit,
        ],
    )
    .map_err(|_| TrayInstallError::Unavailable)?;
    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or(TrayInstallError::Unavailable)?;
    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip(&labels.tooltip)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .build(app)
        .map_err(|_| TrayInstallError::Unavailable)?;

    let controller = Arc::new(TrayController {
        target: Arc::new(TrayRenderTarget {
            tray,
            status,
            open_dashboard,
            login_autostart,
            reasoning_notifications,
            settings,
            quit,
        }),
        state: Mutex::new(TrayControllerState::new(initial_state)),
        render_generation: Arc::new(AtomicU64::new(0)),
        render_failure_reported: Arc::new(AtomicBool::new(false)),
        disposed: Arc::new(AtomicBool::new(false)),
    });
    Ok(controller)
}

fn register_menu_handler(controller: &Arc<TrayController>, lifecycle: DesktopLifecycle) {
    let tray: Arc<dyn HostTrayPort> = controller.clone();
    let weak_tray = Arc::downgrade(&tray);
    controller.target.tray.on_menu_event(move |app, event| {
        let Some(action) = host_action_for_menu_id(event.id().as_ref()) else {
            return;
        };
        let Some(tray) = weak_tray.upgrade() else {
            return;
        };
        let coordinator = tauri_host_coordinator(app.clone(), lifecycle.clone());
        if let Some(command) = HostCoordinator::command_for_tray_action(action, &tray) {
            spawn_host_command(coordinator, command);
        }
    });
}

async fn run_presentation_monitor(
    controller: Arc<TrayController>,
    lifecycle: DesktopLifecycle,
    platform: DashboardPlatformCapabilities,
) {
    let Some(monitors) = lifecycle.wait_for_presentation_monitors().await else {
        controller.mark_stopped();
        return;
    };
    let presentation: Arc<dyn TrayPresentationPort> = controller;
    run_presentation_updates(
        presentation,
        monitors.settings,
        monitors.receiver,
        monitors.workers,
        platform.monitor(),
    )
    .await;
}

pub(crate) async fn run_presentation_updates(
    presentation: Arc<dyn TrayPresentationPort>,
    mut settings: SettingsMonitor,
    mut receiver: OtlpReceiverMonitor,
    mut workers: ResidentWorkerMonitor,
    mut platform: DashboardPlatformCapabilityMonitor,
) {
    loop {
        let state = TrayViewModel::running(
            &host_settings_document(&settings.snapshot()),
            host_receiver_state(receiver.snapshot().bind_state()),
            host_worker_state(workers.snapshot()),
            host_platform_snapshot(platform.snapshot()),
        );
        presentation.apply(state);
        if state.status() == TrayStatus::Stopped {
            return;
        }

        tokio::select! {
            changed = settings.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            changed = receiver.changed() => {
                if changed.is_none() {
                    break;
                }
            }
            changed = workers.changed() => {
                if changed.is_none() {
                    break;
                }
            }
            changed = platform.changed() => {
                if changed.is_none() {
                    break;
                }
            }
        }
    }
    presentation.mark_stopped();
}

const fn host_locale(value: AppLocale) -> HostLocale {
    match value {
        AppLocale::EnUs => HostLocale::EnUs,
        AppLocale::ZhCn => HostLocale::ZhCn,
    }
}

const fn host_receiver_state(value: OtlpReceiverBindState) -> HostReceiverState {
    match value {
        OtlpReceiverBindState::Starting => HostReceiverState::Starting,
        OtlpReceiverBindState::Bound => HostReceiverState::Bound,
        OtlpReceiverBindState::Retrying => HostReceiverState::Retrying,
        OtlpReceiverBindState::Conflict => HostReceiverState::Conflict,
        OtlpReceiverBindState::Stopped => HostReceiverState::Stopped,
    }
}

const fn host_worker_state(value: ResidentWorkerState) -> HostWorkerState {
    match value {
        ResidentWorkerState::Running => HostWorkerState::Running,
        ResidentWorkerState::Failed => HostWorkerState::Failed,
        ResidentWorkerState::Stopped => HostWorkerState::Stopped,
    }
}

const fn host_platform_snapshot(
    value: DashboardPlatformCapabilitySnapshot,
) -> HostPlatformSnapshot {
    HostPlatformSnapshot {
        login_autostart: value.login_autostart(),
        system_notifications: match value.system_notifications() {
            SystemNotificationAvailability::Available => HostNotificationAvailability::Available,
            SystemNotificationAvailability::PermissionRequired => {
                HostNotificationAvailability::PermissionRequired
            }
            SystemNotificationAvailability::Unavailable => {
                HostNotificationAvailability::Unavailable
            }
        },
    }
}

fn status_text(catalog: &TrayStatusCatalog, status: TrayStatus) -> &str {
    match status {
        TrayStatus::Starting => &catalog.starting,
        TrayStatus::Healthy => &catalog.healthy,
        TrayStatus::Degraded => &catalog.degraded,
        TrayStatus::ReceiverUnavailable => &catalog.receiver_unavailable,
        TrayStatus::ReceiverPortConflict => &catalog.receiver_port_conflict,
        TrayStatus::NotificationPermissionRequired => &catalog.notification_permission_required,
        TrayStatus::NotificationUnavailable => &catalog.notification_unavailable,
        TrayStatus::Stopped => &catalog.stopped,
    }
}

fn interpolate_status(template: &str, status: &str) -> String {
    template.replace("{{status}}", status)
}

#[cfg(test)]
const fn status_for(
    receiver: OtlpReceiverBindState,
    workers: ResidentWorkerState,
    notification_availability: SystemNotificationAvailability,
    notifications_enabled: bool,
) -> TrayStatus {
    let document = HostSettingsDocument {
        revision: 1,
        locale: HostLocale::EnUs,
        login_autostart: false,
        reasoning_notifications: notifications_enabled,
    };
    TrayViewModel::running(
        &document,
        host_receiver_state(receiver),
        host_worker_state(workers),
        HostPlatformSnapshot {
            login_autostart: true,
            system_notifications: match notification_availability {
                SystemNotificationAvailability::Available => {
                    HostNotificationAvailability::Available
                }
                SystemNotificationAvailability::PermissionRequired => {
                    HostNotificationAvailability::PermissionRequired
                }
                SystemNotificationAvailability::Unavailable => {
                    HostNotificationAvailability::Unavailable
                }
            },
        },
    )
    .status()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_catalogs_drive_complete_localized_tray_labels() {
        let english = TrayLabels::new(HostLocale::EnUs, TrayStatus::ReceiverPortConflict);
        assert_eq!(english.tooltip, "CodexMonitor — OTLP receiver port in use");
        assert_eq!(english.status, "Status: OTLP receiver port in use");
        assert_eq!(english.open_dashboard, "Open Dashboard");
        assert_eq!(english.start_at_login, "Start at login");
        assert_eq!(english.reasoning_notifications, "Reasoning notifications");
        assert_eq!(english.settings, "Settings");
        assert_eq!(english.quit, "Quit CodexMonitor");

        let chinese = TrayLabels::new(HostLocale::ZhCn, TrayStatus::ReceiverPortConflict);
        assert_eq!(chinese.tooltip, "CodexMonitor — OTLP 接收端口被占用");
        assert_eq!(chinese.status, "状态：OTLP 接收端口被占用");
        assert_eq!(chinese.open_dashboard, "打开 Dashboard");
        assert_eq!(chinese.start_at_login, "登录时启动");
        assert_eq!(chinese.reasoning_notifications, "推理通知");
        assert_eq!(chinese.settings, "设置");
        assert_eq!(chinese.quit, "退出 CodexMonitor");
    }

    #[test]
    fn tray_status_prioritizes_worker_and_receiver_failures() {
        assert_eq!(
            status_for(
                OtlpReceiverBindState::Bound,
                ResidentWorkerState::Failed,
                SystemNotificationAvailability::Available,
                true,
            ),
            TrayStatus::Degraded
        );
        assert_eq!(
            status_for(
                OtlpReceiverBindState::Conflict,
                ResidentWorkerState::Running,
                SystemNotificationAvailability::Available,
                true,
            ),
            TrayStatus::ReceiverPortConflict
        );
        assert_eq!(
            status_for(
                OtlpReceiverBindState::Retrying,
                ResidentWorkerState::Running,
                SystemNotificationAvailability::Available,
                true,
            ),
            TrayStatus::ReceiverUnavailable
        );
        assert_eq!(
            status_for(
                OtlpReceiverBindState::Stopped,
                ResidentWorkerState::Running,
                SystemNotificationAvailability::Available,
                true,
            ),
            TrayStatus::Stopped
        );
    }

    #[test]
    fn notification_capability_only_degrades_an_enabled_notification_feature() {
        assert_eq!(
            status_for(
                OtlpReceiverBindState::Bound,
                ResidentWorkerState::Running,
                SystemNotificationAvailability::PermissionRequired,
                true,
            ),
            TrayStatus::NotificationPermissionRequired
        );
        assert_eq!(
            status_for(
                OtlpReceiverBindState::Bound,
                ResidentWorkerState::Running,
                SystemNotificationAvailability::Unavailable,
                true,
            ),
            TrayStatus::NotificationUnavailable
        );
        assert_eq!(
            status_for(
                OtlpReceiverBindState::Bound,
                ResidentWorkerState::Running,
                SystemNotificationAvailability::Unavailable,
                false,
            ),
            TrayStatus::Healthy
        );
    }

    #[test]
    fn every_actionable_menu_id_has_one_closed_dispatch() {
        assert_eq!(
            host_action_for_menu_id(OPEN_DASHBOARD_MENU_ID),
            Some(HostTrayAction::OpenDashboard)
        );
        assert_eq!(
            host_action_for_menu_id(LOGIN_AUTOSTART_MENU_ID),
            Some(HostTrayAction::ToggleLoginAutostart)
        );
        assert_eq!(
            host_action_for_menu_id(REASONING_NOTIFICATIONS_MENU_ID),
            Some(HostTrayAction::ToggleReasoningNotifications)
        );
        assert_eq!(
            host_action_for_menu_id(SETTINGS_MENU_ID),
            Some(HostTrayAction::OpenSettings)
        );
        assert_eq!(
            host_action_for_menu_id(QUIT_MENU_ID),
            Some(HostTrayAction::Quit)
        );
        assert_eq!(host_action_for_menu_id(STATUS_MENU_ID), None);
        assert_eq!(host_action_for_menu_id("unknown"), None);
    }

    #[test]
    fn newer_render_generation_invalidates_stale_main_thread_tasks() {
        let latest_generation = AtomicU64::new(0);
        let disposed = AtomicBool::new(false);
        let mut state = TrayControllerState::new(TrayViewModel::starting(HostLocale::EnUs));
        let first = state.next_render(&latest_generation);

        state.view = state.view.with_settings(&HostSettingsDocument {
            revision: 2,
            locale: HostLocale::ZhCn,
            login_autostart: false,
            reasoning_notifications: false,
        });
        state.settings_mutation = true;
        let second = state.next_render(&latest_generation);

        assert!(!first.should_render(&latest_generation, &disposed));
        assert!(second.should_render(&latest_generation, &disposed));
        assert_eq!(second.view.locale(), HostLocale::ZhCn);
        assert!(!second.mutation_idle);

        disposed.store(true, Ordering::Release);
        assert!(!second.should_render(&latest_generation, &disposed));
    }

    #[test]
    fn shutdown_state_cannot_be_reenabled_by_late_monitor_updates() {
        let mut state = TrayControllerState::new(TrayViewModel::starting(HostLocale::EnUs));
        assert!(state.request_shutdown());
        assert!(!state.request_shutdown());
        assert!(!state.claim_settings_mutation());

        let late_running = TrayViewModel::running(
            &HostSettingsDocument {
                revision: 2,
                locale: HostLocale::ZhCn,
                login_autostart: true,
                reasoning_notifications: true,
            },
            HostReceiverState::Bound,
            HostWorkerState::Running,
            HostPlatformSnapshot {
                login_autostart: true,
                system_notifications: HostNotificationAvailability::Available,
            },
        );
        state.update_view(late_running);

        assert_eq!(state.view.locale(), HostLocale::ZhCn);
        assert!(!state.view.controls_enabled());
        assert!(!state.view.quit_enabled());
    }

    #[tokio::test]
    async fn native_removal_success_releases_a_waiting_normal_shutdown() {
        let removal = Arc::new(TrayRemovalState::new());
        removal.complete(false);
        let waiting = {
            let removal = Arc::clone(&removal);
            tokio::spawn(async move {
                removal
                    .wait_before(Instant::now() + tokio::time::Duration::from_secs(1))
                    .await
            })
        };
        tokio::task::yield_now().await;

        removal.complete(true);
        assert_eq!(waiting.await.unwrap(), Some(true));
    }

    #[tokio::test]
    async fn failed_removal_is_final_only_at_the_shared_deadline() {
        let removal = TrayRemovalState::new();
        removal.complete(false);

        assert_eq!(
            removal
                .wait_before(Instant::now() + tokio::time::Duration::from_millis(5))
                .await,
            Some(false)
        );
    }
}
