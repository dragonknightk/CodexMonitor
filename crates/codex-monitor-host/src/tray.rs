use crate::{HostLocale, HostSettingsDocument};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostTrayStatus {
    Starting,
    Healthy,
    Degraded,
    ReceiverUnavailable,
    ReceiverPortConflict,
    NotificationPermissionRequired,
    NotificationUnavailable,
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostReceiverState {
    Starting,
    Bound,
    Retrying,
    Conflict,
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostWorkerState {
    Running,
    Failed,
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostNotificationAvailability {
    Available,
    PermissionRequired,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostPlatformSnapshot {
    pub login_autostart: bool,
    pub system_notifications: HostNotificationAvailability,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ControlAvailability {
    Disabled,
    Enabled,
}

impl ControlAvailability {
    const fn from_enabled(enabled: bool) -> Self {
        if enabled {
            Self::Enabled
        } else {
            Self::Disabled
        }
    }

    const fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ToggleViewState {
    availability: ControlAvailability,
    checked: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostTrayViewModel {
    locale: HostLocale,
    status: HostTrayStatus,
    controls: ControlAvailability,
    login_autostart: ToggleViewState,
    reasoning_notifications_enabled: bool,
    quit: ControlAvailability,
}

impl HostTrayViewModel {
    #[must_use]
    pub const fn starting(locale: HostLocale) -> Self {
        Self {
            locale,
            status: HostTrayStatus::Starting,
            controls: ControlAvailability::Disabled,
            login_autostart: ToggleViewState {
                availability: ControlAvailability::Disabled,
                checked: false,
            },
            reasoning_notifications_enabled: false,
            quit: ControlAvailability::Enabled,
        }
    }

    #[must_use]
    pub const fn running(
        document: &HostSettingsDocument,
        receiver: HostReceiverState,
        workers: HostWorkerState,
        platform: HostPlatformSnapshot,
    ) -> Self {
        let controls_enabled = matches!(workers, HostWorkerState::Running)
            && !matches!(receiver, HostReceiverState::Stopped);
        Self {
            locale: document.locale,
            status: status_for(
                receiver,
                workers,
                platform.system_notifications,
                document.reasoning_notifications,
            ),
            controls: ControlAvailability::from_enabled(controls_enabled),
            login_autostart: ToggleViewState {
                availability: ControlAvailability::from_enabled(platform.login_autostart),
                checked: document.login_autostart,
            },
            reasoning_notifications_enabled: document.reasoning_notifications,
            quit: ControlAvailability::Enabled,
        }
    }

    #[must_use]
    pub const fn stopped(mut self) -> Self {
        self.status = HostTrayStatus::Stopped;
        self.controls = ControlAvailability::Disabled;
        self.quit = ControlAvailability::Disabled;
        self
    }

    #[must_use]
    pub const fn shutdown_requested(mut self) -> Self {
        self.controls = ControlAvailability::Disabled;
        self.quit = ControlAvailability::Disabled;
        self
    }

    #[must_use]
    pub const fn with_settings(mut self, document: &HostSettingsDocument) -> Self {
        self.locale = document.locale;
        self.login_autostart.checked = document.login_autostart;
        self.reasoning_notifications_enabled = document.reasoning_notifications;
        self
    }

    #[must_use]
    pub const fn locale(self) -> HostLocale {
        self.locale
    }

    #[must_use]
    pub const fn status(self) -> HostTrayStatus {
        self.status
    }

    #[must_use]
    pub const fn controls_enabled(self) -> bool {
        self.controls.is_enabled()
    }

    #[must_use]
    pub const fn login_autostart_control_enabled(self) -> bool {
        self.login_autostart.availability.is_enabled()
    }

    #[must_use]
    pub const fn login_autostart_enabled(self) -> bool {
        self.login_autostart.checked
    }

    #[must_use]
    pub const fn reasoning_notifications_enabled(self) -> bool {
        self.reasoning_notifications_enabled
    }

    #[must_use]
    pub const fn quit_enabled(self) -> bool {
        self.quit.is_enabled()
    }
}

const fn status_for(
    receiver: HostReceiverState,
    workers: HostWorkerState,
    notification_availability: HostNotificationAvailability,
    notifications_enabled: bool,
) -> HostTrayStatus {
    match workers {
        HostWorkerState::Failed => return HostTrayStatus::Degraded,
        HostWorkerState::Stopped => return HostTrayStatus::Stopped,
        HostWorkerState::Running => {}
    }
    match receiver {
        HostReceiverState::Starting => HostTrayStatus::Starting,
        HostReceiverState::Retrying => HostTrayStatus::ReceiverUnavailable,
        HostReceiverState::Conflict => HostTrayStatus::ReceiverPortConflict,
        HostReceiverState::Stopped => HostTrayStatus::Stopped,
        HostReceiverState::Bound if !notifications_enabled => HostTrayStatus::Healthy,
        HostReceiverState::Bound => match notification_availability {
            HostNotificationAvailability::Available => HostTrayStatus::Healthy,
            HostNotificationAvailability::PermissionRequired => {
                HostTrayStatus::NotificationPermissionRequired
            }
            HostNotificationAvailability::Unavailable => HostTrayStatus::NotificationUnavailable,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> HostSettingsDocument {
        HostSettingsDocument {
            revision: 1,
            locale: HostLocale::ZhCn,
            login_autostart: true,
            reasoning_notifications: true,
        }
    }

    #[test]
    fn port_conflict_and_worker_failure_have_stable_presentation() {
        let platform = HostPlatformSnapshot {
            login_autostart: true,
            system_notifications: HostNotificationAvailability::Available,
        };
        let conflict = HostTrayViewModel::running(
            &settings(),
            HostReceiverState::Conflict,
            HostWorkerState::Running,
            platform,
        );
        assert_eq!(conflict.status(), HostTrayStatus::ReceiverPortConflict);
        assert!(conflict.controls_enabled());

        let failed = HostTrayViewModel::running(
            &settings(),
            HostReceiverState::Bound,
            HostWorkerState::Failed,
            platform,
        );
        assert_eq!(failed.status(), HostTrayStatus::Degraded);
        assert!(!failed.controls_enabled());
    }
}
