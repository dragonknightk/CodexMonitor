use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::FromRef;
use tokio::sync::{Semaphore, watch};

use super::assets::DashboardAssets;
use super::cursor::CursorStore;
use super::session::DashboardSessions;
use crate::ports::{
    DashboardCapabilityPort, DashboardEventPort, DashboardQueryPort, DashboardSettingsPort,
};

pub(super) const MAX_EVENT_MATERIALIZATIONS: usize = 16;

/// Process-local native capabilities exposed by the versioned Dashboard API.
///
/// Clones observe the same live snapshot so notification permission or platform
/// availability changes can reach the Dashboard and tray without rebuilding
/// the resident engine.
#[derive(Clone)]
pub struct DashboardPlatformCapabilities {
    state: watch::Sender<DashboardPlatformCapabilitySnapshot>,
}

/// Cloneable observer for native platform capability changes.
#[derive(Clone)]
pub struct DashboardPlatformCapabilityMonitor {
    state: watch::Receiver<DashboardPlatformCapabilitySnapshot>,
}

/// Latest process-local native capability values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DashboardPlatformCapabilitySnapshot {
    login_autostart: bool,
    system_notifications: SystemNotificationAvailability,
    tray: TrayAvailability,
}

impl DashboardPlatformCapabilitySnapshot {
    #[must_use]
    pub const fn login_autostart(self) -> bool {
        self.login_autostart
    }

    #[must_use]
    pub const fn system_notifications(self) -> SystemNotificationAvailability {
        self.system_notifications
    }

    #[must_use]
    pub const fn tray(self) -> TrayAvailability {
        self.tray
    }
}

/// Current native system-notification capability state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemNotificationAvailability {
    Available,
    PermissionRequired,
    Unavailable,
}

/// Whether the active desktop environment accepted a resident tray icon.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayAvailability {
    Available,
    Unavailable,
}

impl DashboardPlatformCapabilities {
    #[must_use]
    pub fn new(
        login_autostart: bool,
        system_notifications: SystemNotificationAvailability,
        tray: TrayAvailability,
    ) -> Self {
        let (state, _) = watch::channel(DashboardPlatformCapabilitySnapshot {
            login_autostart,
            system_notifications,
            tray,
        });
        Self { state }
    }

    #[must_use]
    pub fn login_autostart(&self) -> bool {
        self.state.borrow().login_autostart
    }

    #[must_use]
    pub fn system_notifications(&self) -> SystemNotificationAvailability {
        self.state.borrow().system_notifications
    }

    #[must_use]
    pub fn tray(&self) -> TrayAvailability {
        self.state.borrow().tray
    }

    /// Subscribe to live native capability changes.
    #[must_use]
    pub fn monitor(&self) -> DashboardPlatformCapabilityMonitor {
        DashboardPlatformCapabilityMonitor {
            state: self.state.subscribe(),
        }
    }

    pub fn set_login_autostart_available(&self, available: bool) {
        self.state.send_if_modified(|state| {
            if state.login_autostart == available {
                return false;
            }
            state.login_autostart = available;
            true
        });
    }

    pub fn set_system_notifications(&self, availability: SystemNotificationAvailability) {
        self.state.send_if_modified(|state| {
            if state.system_notifications == availability {
                return false;
            }
            state.system_notifications = availability;
            true
        });
    }

    pub fn set_tray(&self, availability: TrayAvailability) {
        self.state.send_if_modified(|state| {
            if state.tray == availability {
                return false;
            }
            state.tray = availability;
            true
        });
    }
}

impl DashboardPlatformCapabilityMonitor {
    #[must_use]
    pub fn snapshot(&self) -> DashboardPlatformCapabilitySnapshot {
        *self.state.borrow()
    }

    /// Wait for the next native capability snapshot.
    ///
    /// Returns `None` after the capability owner has been dropped.
    pub async fn changed(&mut self) -> Option<DashboardPlatformCapabilitySnapshot> {
        self.state.changed().await.ok()?;
        Some(self.snapshot())
    }
}

/// Shared dependencies for the authenticated local Dashboard API.
#[derive(Clone)]
pub struct DashboardState {
    pub(super) assets: DashboardAssets,
    pub(super) sessions: DashboardSessions,
    pub(super) queries: Arc<dyn DashboardQueryPort>,
    pub(super) settings: Arc<dyn DashboardSettingsPort>,
    pub(super) capabilities: Arc<dyn DashboardCapabilityPort>,
    pub(super) platform: DashboardPlatformCapabilities,
    pub(super) cursors: CursorStore,
    pub(super) events: Arc<dyn DashboardEventPort>,
    pub(super) event_materializations: Arc<Semaphore>,
    pub(super) listener_address: Option<SocketAddr>,
    stream_shutdown: watch::Sender<bool>,
}

impl DashboardState {
    /// Assemble the process-local Dashboard query state.
    #[must_use]
    pub fn new(
        assets: DashboardAssets,
        sessions: DashboardSessions,
        queries: Arc<dyn DashboardQueryPort>,
        settings: Arc<dyn DashboardSettingsPort>,
        capabilities: Arc<dyn DashboardCapabilityPort>,
        events: Arc<dyn DashboardEventPort>,
        platform: DashboardPlatformCapabilities,
    ) -> Self {
        let (stream_shutdown, _) = watch::channel(false);
        Self {
            assets,
            sessions,
            queries,
            settings,
            capabilities,
            platform,
            cursors: CursorStore::default(),
            events,
            event_materializations: Arc::new(Semaphore::new(MAX_EVENT_MATERIALIZATIONS)),
            listener_address: None,
            stream_shutdown,
        }
    }

    pub(super) fn with_listener_address(mut self, listener_address: SocketAddr) -> Self {
        self.listener_address = Some(listener_address);
        self
    }

    pub(super) fn stream_shutdown(&self) -> watch::Receiver<bool> {
        self.stream_shutdown.subscribe()
    }

    pub(super) fn stream_shutdown_sender(&self) -> watch::Sender<bool> {
        self.stream_shutdown.clone()
    }
}

impl FromRef<DashboardState> for DashboardSessions {
    fn from_ref(state: &DashboardState) -> Self {
        state.sessions.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_capability_clones_share_live_state() {
        let capabilities = DashboardPlatformCapabilities::new(
            true,
            SystemNotificationAvailability::PermissionRequired,
            TrayAvailability::Available,
        );
        let dashboard_view = capabilities.clone();
        let monitor = capabilities.monitor();

        capabilities.set_login_autostart_available(false);
        capabilities.set_system_notifications(SystemNotificationAvailability::Available);
        capabilities.set_tray(TrayAvailability::Unavailable);

        assert!(!dashboard_view.login_autostart());
        assert_eq!(
            dashboard_view.system_notifications(),
            SystemNotificationAvailability::Available
        );
        assert!(!monitor.snapshot().login_autostart());
        assert_eq!(
            monitor.snapshot().system_notifications(),
            SystemNotificationAvailability::Available
        );
        assert_eq!(monitor.snapshot().tray(), TrayAvailability::Unavailable);
    }

    #[tokio::test]
    async fn platform_capability_monitor_observes_live_changes() {
        let capabilities = DashboardPlatformCapabilities::new(
            true,
            SystemNotificationAvailability::Unavailable,
            TrayAvailability::Available,
        );
        let mut monitor = capabilities.monitor();

        capabilities.set_system_notifications(SystemNotificationAvailability::Available);

        let snapshot = monitor.changed().await.unwrap();
        assert!(snapshot.login_autostart());
        assert_eq!(
            snapshot.system_notifications(),
            SystemNotificationAvailability::Available
        );
    }
}
