use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use codex_monitor_core::{
    ActionCapability, ActionExecutionContext, ActionExecutionResult, ActionExecutor, ActionFuture,
    ActionParameters, ActionPermission, RegisteredActionType, SYSTEM_NOTIFICATION_DEFINITION,
};
use codex_monitor_dashboard::{
    DashboardAssetInput, DashboardAssets, DashboardPlatformCapabilities,
    SystemNotificationAvailability, TrayAvailability,
};
use codex_monitor_engine::runtime::{
    ResidentEngine, ResidentEngineCancellation, ResidentEngineConfig, ResidentWorkerState,
};
use codex_monitor_engine::{LoginAutostartError, LoginAutostartHost};
use codex_monitor_host::{
    HostCoordinator, HostFuture, HostLegacyImportOutcome, HostSettingsMutation,
    HostSettingsMutationOutcome, ResidentHostPort, StartupMode,
};
use codex_monitor_storage::{AppDataEnvironment, AppDataPaths, CurrentUserStorage, PlatformFamily};
use codex_monitor_telemetry::OtlpReceiverBindState;
use tempfile::tempdir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::Mutex;

struct FakeLoginAutostart {
    enabled: AtomicBool,
}

impl LoginAutostartHost for FakeLoginAutostart {
    fn login_autostart(&self) -> Result<bool, LoginAutostartError> {
        Ok(self.enabled.load(Ordering::Acquire))
    }

    fn set_login_autostart(&self, enabled: bool) -> Result<bool, LoginAutostartError> {
        self.enabled.store(enabled, Ordering::Release);
        Ok(enabled)
    }
}

struct NoopActionExecutor;

impl ActionExecutor for NoopActionExecutor {
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
        Box::pin(async { ActionExecutionResult::Completed })
    }
}

struct IntegratedResidentPort {
    engine: Mutex<Option<ResidentEngine>>,
    opened: AtomicUsize,
}

impl ResidentHostPort for IntegratedResidentPort {
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

    fn import_legacy_database(&self, _source: PathBuf) -> HostFuture<'_, HostLegacyImportOutcome> {
        Box::pin(async { HostLegacyImportOutcome::RuntimeUnavailable })
    }

    fn shutdown(&self, _exit_code: i32) -> HostFuture<'_, ()> {
        Box::pin(async move {
            if let Some(mut engine) = self.engine.lock().await.take() {
                engine.shutdown().await.unwrap();
            }
        })
    }
}

fn synthetic_assets() -> DashboardAssets {
    DashboardAssets::new([DashboardAssetInput::new(
        "index.html",
        b"<!doctype html><html><body><div id=\"root\"></div></body></html>".to_vec(),
        "text/html",
    )])
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_product_composition_starts_listeners_workers_and_shuts_down_through_host_port() {
    let directory = tempdir().unwrap();
    let paths = AppDataPaths::resolve(
        PlatformFamily::Unix,
        &AppDataEnvironment {
            home: Some(directory.path().to_path_buf()),
            ..AppDataEnvironment::default()
        },
    )
    .unwrap();
    let storage = CurrentUserStorage::open_paths(paths).unwrap();
    let config = ResidentEngineConfig::new(
        synthetic_assets(),
        directory.path().join("synthetic-session-index.jsonl"),
        codex_monitor_storage::AppLocale::ZhCn,
        Arc::new(FakeLoginAutostart {
            enabled: AtomicBool::new(false),
        }),
        Arc::new(NoopActionExecutor),
        DashboardPlatformCapabilities::new(
            true,
            SystemNotificationAvailability::Available,
            TrayAvailability::Available,
        ),
    );
    let engine =
        ResidentEngine::start_with_storage(storage, config, ResidentEngineCancellation::default())
            .await
            .unwrap();
    assert_eq!(
        engine.worker_monitor().snapshot(),
        ResidentWorkerState::Running
    );
    assert!(matches!(
        engine.receiver_monitor().snapshot().bind_state(),
        OtlpReceiverBindState::Bound | OtlpReceiverBindState::Conflict
    ));

    let dashboard_address = engine.dashboard_address();
    let mut connection = tokio::net::TcpStream::connect(dashboard_address)
        .await
        .unwrap();
    connection
        .write_all(
            format!(
                "GET /api/v1/health HTTP/1.1\r\nHost: {dashboard_address}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut response = Vec::new();
    connection.read_to_end(&mut response).await.unwrap();
    assert!(
        String::from_utf8(response)
            .unwrap()
            .starts_with("HTTP/1.1 401 Unauthorized")
    );

    let resident = Arc::new(IntegratedResidentPort {
        engine: Mutex::new(Some(engine)),
        opened: AtomicUsize::new(0),
    });
    let resident_port: Arc<dyn ResidentHostPort> = resident.clone();
    let coordinator = HostCoordinator::new(resident_port);
    coordinator
        .execute(HostCoordinator::command_for_startup(StartupMode::Manual).unwrap())
        .await;
    assert!(HostCoordinator::command_for_startup(StartupMode::Background).is_none());
    coordinator
        .execute(HostCoordinator::command_for_startup(StartupMode::Quit).unwrap())
        .await;
    assert_eq!(resident.opened.load(Ordering::Acquire), 1);
    assert!(resident.engine.lock().await.is_none());
    assert!(
        tokio::net::TcpListener::bind(dashboard_address)
            .await
            .is_ok()
    );
}
