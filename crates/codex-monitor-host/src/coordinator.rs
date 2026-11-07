use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Weak};

use crate::{ForwardedLaunchCommand, StartupArgumentError, StartupMode};

pub type HostFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostLocale {
    EnUs,
    ZhCn,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostSettingsDocument {
    pub revision: u64,
    pub locale: HostLocale,
    pub login_autostart: bool,
    pub reasoning_notifications: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostSettingsMutation {
    LoginAutostart(bool),
    NotificationsEnabled(bool),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostSettingsErrorCode {
    RevisionConflict,
    InvalidSettings,
    StorageUnavailable,
    PlatformUnavailable,
    RuntimeUnavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostTrayAction {
    OpenDashboard,
    ToggleLoginAutostart,
    ToggleReasoningNotifications,
    OpenSettings,
    Quit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostSettingsToggle {
    LoginAutostart,
    ReasoningNotifications,
}

pub trait HostTrayPort: Send + Sync + 'static {
    fn begin_settings_mutation(&self, toggle: HostSettingsToggle) -> Option<bool>;
    fn finish_settings_mutation(&self, document: Option<&HostSettingsDocument>);
    fn begin_shutdown(&self) -> bool;
}

pub enum HostSettingsMutationOutcome {
    Applied(HostSettingsDocument),
    Rejected {
        error: HostSettingsErrorCode,
        observed: HostSettingsDocument,
    },
    RuntimeUnavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostLegacyImportOutcome {
    Imported,
    AlreadyImported,
    Rejected,
    RuntimeUnavailable,
}

/// 由 host 消费方拥有的常驻引擎控制端口。
pub trait ResidentHostPort: Send + Sync + 'static {
    fn open_dashboard(&self) -> HostFuture<'_, ()>;
    fn mutate_settings(
        &self,
        mutation: HostSettingsMutation,
    ) -> HostFuture<'_, HostSettingsMutationOutcome>;
    fn import_legacy_database(&self, source: PathBuf) -> HostFuture<'_, HostLegacyImportOutcome>;
    fn shutdown(&self, exit_code: i32) -> HostFuture<'_, ()>;
}

pub enum HostCommand {
    OpenDashboard,
    MutateSettings {
        mutation: HostSettingsMutation,
        tray: Weak<dyn HostTrayPort>,
    },
    ImportLegacy {
        source: PathBuf,
    },
    Shutdown {
        exit_code: i32,
    },
}

#[derive(Clone)]
pub struct HostCoordinator {
    resident: Arc<dyn ResidentHostPort>,
}

impl HostCoordinator {
    #[must_use]
    pub fn new(resident: Arc<dyn ResidentHostPort>) -> Self {
        Self { resident }
    }

    pub fn command_for_startup(mode: StartupMode) -> Option<HostCommand> {
        match mode {
            StartupMode::Manual => Some(HostCommand::OpenDashboard),
            StartupMode::Background => None,
            StartupMode::ImportLegacy(source) => Some(HostCommand::ImportLegacy { source }),
            StartupMode::Quit => Some(HostCommand::Shutdown { exit_code: 0 }),
        }
    }

    /// 将第二实例转发的闭合参数集转换为 host 命令。
    ///
    /// # Errors
    ///
    /// 参数缺少可执行文件、包含未知或重复选项，或 legacy source 不是绝对路径时，
    /// 返回稳定的 [`StartupArgumentError`]，且不保留参数原文。
    pub fn command_for_forwarded_launch(
        args: Vec<String>,
    ) -> Result<Option<HostCommand>, StartupArgumentError> {
        Ok(match ForwardedLaunchCommand::parse(args)? {
            ForwardedLaunchCommand::OpenDashboard => Some(HostCommand::OpenDashboard),
            ForwardedLaunchCommand::IgnoreBackgroundLaunch => None,
            ForwardedLaunchCommand::ImportLegacy(source) => {
                Some(HostCommand::ImportLegacy { source })
            }
            ForwardedLaunchCommand::Quit => Some(HostCommand::Shutdown { exit_code: 0 }),
        })
    }

    pub fn command_for_tray_action(
        action: HostTrayAction,
        tray: &Arc<dyn HostTrayPort>,
    ) -> Option<HostCommand> {
        match action {
            HostTrayAction::OpenDashboard | HostTrayAction::OpenSettings => {
                Some(HostCommand::OpenDashboard)
            }
            HostTrayAction::ToggleLoginAutostart => {
                Self::settings_command(tray, HostSettingsToggle::LoginAutostart)
            }
            HostTrayAction::ToggleReasoningNotifications => {
                Self::settings_command(tray, HostSettingsToggle::ReasoningNotifications)
            }
            HostTrayAction::Quit => tray
                .begin_shutdown()
                .then_some(HostCommand::Shutdown { exit_code: 0 }),
        }
    }

    fn settings_command(
        tray: &Arc<dyn HostTrayPort>,
        toggle: HostSettingsToggle,
    ) -> Option<HostCommand> {
        let enabled = tray.begin_settings_mutation(toggle)?;
        let mutation = match toggle {
            HostSettingsToggle::LoginAutostart => HostSettingsMutation::LoginAutostart(enabled),
            HostSettingsToggle::ReasoningNotifications => {
                HostSettingsMutation::NotificationsEnabled(enabled)
            }
        };
        Some(HostCommand::MutateSettings {
            mutation,
            tray: Arc::downgrade(tray),
        })
    }

    pub async fn execute(&self, command: HostCommand) {
        match command {
            HostCommand::OpenDashboard => self.resident.open_dashboard().await,
            HostCommand::MutateSettings { mutation, tray } => {
                let outcome = self.resident.mutate_settings(mutation).await;
                let (document, error) = match outcome {
                    HostSettingsMutationOutcome::Applied(document) => (Some(document), None),
                    HostSettingsMutationOutcome::Rejected { error, observed } => {
                        (Some(observed), Some(error))
                    }
                    HostSettingsMutationOutcome::RuntimeUnavailable => (None, None),
                };
                if let Some(error) = error {
                    eprintln!(
                        "codex_monitor_tray_settings_update_failed:{}",
                        settings_error_code(error)
                    );
                }
                if let Some(tray) = tray.upgrade() {
                    tray.finish_settings_mutation(document.as_ref());
                }
            }
            HostCommand::ImportLegacy { source } => {
                match self.resident.import_legacy_database(source).await {
                    HostLegacyImportOutcome::Imported => {
                        eprintln!("codex_monitor_legacy_import_completed");
                        self.resident.open_dashboard().await;
                    }
                    HostLegacyImportOutcome::AlreadyImported => {
                        eprintln!("codex_monitor_legacy_import_already_completed");
                        self.resident.open_dashboard().await;
                    }
                    HostLegacyImportOutcome::Rejected => {
                        eprintln!("codex_monitor_legacy_import_rejected");
                    }
                    HostLegacyImportOutcome::RuntimeUnavailable => {
                        eprintln!("codex_monitor_legacy_import_unavailable");
                    }
                }
            }
            HostCommand::Shutdown { exit_code } => self.resident.shutdown(exit_code).await,
        }
    }
}

const fn settings_error_code(error: HostSettingsErrorCode) -> &'static str {
    match error {
        HostSettingsErrorCode::RevisionConflict => "revision_conflict",
        HostSettingsErrorCode::InvalidSettings => "invalid_settings",
        HostSettingsErrorCode::StorageUnavailable => "storage_unavailable",
        HostSettingsErrorCode::PlatformUnavailable => "platform_unavailable",
        HostSettingsErrorCode::RuntimeUnavailable => "runtime_unavailable",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::QUIT_ARGUMENT;

    struct FakeResident {
        opened: AtomicUsize,
        shutdowns: AtomicUsize,
    }

    impl ResidentHostPort for FakeResident {
        fn open_dashboard(&self) -> HostFuture<'_, ()> {
            Box::pin(async move {
                self.opened.fetch_add(1, Ordering::AcqRel);
            })
        }

        fn mutate_settings(
            &self,
            _mutation: HostSettingsMutation,
        ) -> HostFuture<'_, HostSettingsMutationOutcome> {
            Box::pin(async { HostSettingsMutationOutcome::RuntimeUnavailable })
        }

        fn import_legacy_database(
            &self,
            _source: PathBuf,
        ) -> HostFuture<'_, HostLegacyImportOutcome> {
            Box::pin(async { HostLegacyImportOutcome::AlreadyImported })
        }

        fn shutdown(&self, _exit_code: i32) -> HostFuture<'_, ()> {
            Box::pin(async move {
                self.shutdowns.fetch_add(1, Ordering::AcqRel);
            })
        }
    }

    #[tokio::test]
    async fn manual_background_and_forwarded_launches_drive_fake_port() {
        let resident = Arc::new(FakeResident {
            opened: AtomicUsize::new(0),
            shutdowns: AtomicUsize::new(0),
        });
        let resident_port: Arc<dyn ResidentHostPort> = resident.clone();
        let coordinator = HostCoordinator::new(resident_port);

        coordinator
            .execute(HostCoordinator::command_for_startup(StartupMode::Manual).unwrap())
            .await;
        assert!(HostCoordinator::command_for_startup(StartupMode::Background).is_none());
        let forwarded = HostCoordinator::command_for_forwarded_launch(vec![
            "codex-monitor".to_owned(),
            QUIT_ARGUMENT.to_owned(),
        ])
        .unwrap()
        .unwrap();
        coordinator.execute(forwarded).await;

        assert_eq!(resident.opened.load(Ordering::Acquire), 1);
        assert_eq!(resident.shutdowns.load(Ordering::Acquire), 1);
    }
}
