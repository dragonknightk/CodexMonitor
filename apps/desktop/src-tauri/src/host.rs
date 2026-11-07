use std::path::PathBuf;
use std::sync::Arc;

use codex_monitor_engine::{SettingsMutation, SettingsRuntimeError};
use codex_monitor_host::{
    HostCommand, HostCoordinator, HostFuture, HostLegacyImportOutcome, HostLocale,
    HostSettingsDocument, HostSettingsErrorCode, HostSettingsMutation, HostSettingsMutationOutcome,
    ResidentHostPort,
};
use codex_monitor_storage::{AppLocale, LegacyImportStatus, StoredSettingsDocument};
use tauri::{AppHandle, Wry};

use crate::lifecycle::{DesktopLifecycle, open_dashboard, request_shutdown};

pub(crate) fn tauri_host_coordinator(
    app: AppHandle<Wry>,
    lifecycle: DesktopLifecycle,
) -> HostCoordinator {
    HostCoordinator::new(Arc::new(TauriResidentHostPort { app, lifecycle }))
}

pub(crate) fn spawn_host_command(coordinator: HostCoordinator, command: HostCommand) {
    tauri::async_runtime::spawn(async move {
        coordinator.execute(command).await;
    });
}

struct TauriResidentHostPort {
    app: AppHandle<Wry>,
    lifecycle: DesktopLifecycle,
}

impl ResidentHostPort for TauriResidentHostPort {
    fn open_dashboard(&self) -> HostFuture<'_, ()> {
        Box::pin(open_dashboard(&self.app, &self.lifecycle))
    }

    fn mutate_settings(
        &self,
        mutation: HostSettingsMutation,
    ) -> HostFuture<'_, HostSettingsMutationOutcome> {
        Box::pin(async move {
            let Some(settings) = self.lifecycle.settings_runtime().await else {
                return HostSettingsMutationOutcome::RuntimeUnavailable;
            };
            let mutation = match mutation {
                HostSettingsMutation::LoginAutostart(enabled) => {
                    SettingsMutation::LoginAutostart(enabled)
                }
                HostSettingsMutation::NotificationsEnabled(enabled) => {
                    SettingsMutation::NotificationsEnabled(enabled)
                }
            };
            match settings.mutate(mutation).await {
                Ok(document) => {
                    HostSettingsMutationOutcome::Applied(host_settings_document(&document))
                }
                Err(error) => HostSettingsMutationOutcome::Rejected {
                    error: host_settings_error(error),
                    observed: host_settings_document(&settings.monitor().snapshot()),
                },
            }
        })
    }

    fn import_legacy_database(&self, source: PathBuf) -> HostFuture<'_, HostLegacyImportOutcome> {
        Box::pin(async move {
            let Some(importer) = self.lifecycle.legacy_database_importer().await else {
                return HostLegacyImportOutcome::RuntimeUnavailable;
            };
            match importer.import(source).await {
                Ok(outcome) => match outcome.status() {
                    LegacyImportStatus::Imported => HostLegacyImportOutcome::Imported,
                    LegacyImportStatus::AlreadyImported => HostLegacyImportOutcome::AlreadyImported,
                },
                Err(_) => HostLegacyImportOutcome::Rejected,
            }
        })
    }

    fn shutdown(&self, exit_code: i32) -> HostFuture<'_, ()> {
        Box::pin(async move {
            let deadline = request_shutdown(self.app.clone(), self.lifecycle.clone(), exit_code);
            let _ = self.lifecycle.wait_for_shutdown_before(deadline).await;
        })
    }
}

pub(crate) fn host_settings_document(value: &StoredSettingsDocument) -> HostSettingsDocument {
    HostSettingsDocument {
        revision: value.revision,
        locale: match value.settings.locale {
            AppLocale::EnUs => HostLocale::EnUs,
            AppLocale::ZhCn => HostLocale::ZhCn,
        },
        login_autostart: value.settings.login_autostart,
        reasoning_notifications: value.settings.notifications_enabled,
    }
}

const fn host_settings_error(value: SettingsRuntimeError) -> HostSettingsErrorCode {
    match value {
        SettingsRuntimeError::RevisionConflict => HostSettingsErrorCode::RevisionConflict,
        SettingsRuntimeError::InvalidSettings => HostSettingsErrorCode::InvalidSettings,
        SettingsRuntimeError::StorageUnavailable => HostSettingsErrorCode::StorageUnavailable,
        SettingsRuntimeError::PlatformUnavailable => HostSettingsErrorCode::PlatformUnavailable,
        SettingsRuntimeError::RuntimeUnavailable => HostSettingsErrorCode::RuntimeUnavailable,
    }
}
