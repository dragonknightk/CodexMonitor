//! Native user-session host. Business behavior remains in the engine crate.

mod autostart;
mod browser;
mod build_channel;
mod dashboard_assets;
mod future_window;
mod host;
mod host_catalog;
mod installed_smoke;
mod lifecycle;
mod notification;
mod platform_registration;
#[cfg(target_os = "linux")]
mod session_termination;
mod tray;

use std::env;
use std::sync::Arc;

use autostart::TauriLoginAutostartHost;
use codex_monitor_core::ActionExecutor;
use codex_monitor_dashboard::{
    DashboardPlatformCapabilities, SystemNotificationAvailability, TrayAvailability,
};
use codex_monitor_engine::LoginAutostartHost;
use codex_monitor_engine::runtime::ResidentEngineConfig;
use codex_monitor_host::{HostCoordinator, StartupArgumentError, StartupMode};
use codex_monitor_storage::AppLocale;
use host::{spawn_host_command, tauri_host_coordinator};
use lifecycle::{DesktopLifecycle, request_shutdown, start_resident_engine};
use notification::TauriSystemNotificationHost;
use tauri::Manager;

/// Start the Tauri event loop.
///
/// # Panics
///
/// Panics when the native host cannot be initialized or its event loop exits
/// with an unrecoverable error.
pub fn run() {
    let (startup_mode, notification_identity_ready) =
        prepare_startup(env::args_os().collect::<Vec<_>>());
    let lifecycle = DesktopLifecycle::new();
    let setup_lifecycle = lifecycle.clone();
    let forwarded_lifecycle = lifecycle.clone();

    let app = tauri::Builder::default()
        // This must remain the first plugin: a secondary process must exit
        // before it can bind listeners or open the database.
        .plugin(tauri_plugin_single_instance::init(
            move |app, args, _working_directory| {
                handle_forwarded_launch(app, args, &forwarded_lifecycle);
            },
        ))
        .plugin(tauri_plugin_opener::init())
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let session_index = app
                .path()
                .home_dir()?
                .join(".codex")
                .join("session_index.jsonl");
            let system_locale = sys_locale::get_locales()
                .find_map(|language_tag| AppLocale::from_language_tag(&language_tag))
                .unwrap_or(AppLocale::EnUs);
            let dashboard_assets = dashboard_assets::load(app)?;
            let platform = DashboardPlatformCapabilities::new(
                false,
                SystemNotificationAvailability::Unavailable,
                TrayAvailability::Available,
            );
            let notification_host = Arc::new(TauriSystemNotificationHost::new(
                app,
                platform.clone(),
                notification_identity_ready,
            )?);
            let force_linux_degradation = installed_smoke::force_linux_degradation();
            if !force_linux_degradation {
                notification_host.start_probe();
            }
            let action_executor: Arc<dyn ActionExecutor> = notification_host;
            let login_autostart_host = Arc::new(TauriLoginAutostartHost::new(
                app.handle(),
                platform.clone(),
            ));
            let login_autostart: Arc<dyn LoginAutostartHost> = login_autostart_host;
            let tray_runtime = if force_linux_degradation {
                platform.set_tray(TrayAvailability::Unavailable);
                None
            } else if let Ok(tray) = tray::install(
                app,
                setup_lifecycle.clone(),
                system_locale,
                platform.clone(),
            ) {
                Some(tray)
            } else {
                platform.set_tray(TrayAvailability::Unavailable);
                eprintln!("codex_monitor_tray_unavailable");
                #[cfg(not(target_os = "linux"))]
                return Err(tray::TrayInstallError::Unavailable.into());
                #[cfg(target_os = "linux")]
                None
            };
            let config = ResidentEngineConfig::new(
                dashboard_assets,
                session_index,
                system_locale,
                login_autostart,
                action_executor,
                platform,
            );

            app.manage(startup_mode.clone());
            app.manage(setup_lifecycle.clone());
            app.manage(tray::TrayRuntimeState::new(tray_runtime));
            #[cfg(target_os = "linux")]
            session_termination::install(app.handle().clone(), setup_lifecycle.clone())?;
            start_resident_engine(app.handle().clone(), setup_lifecycle.clone(), config);
            let coordinator =
                tauri_host_coordinator(app.handle().clone(), setup_lifecycle.clone());
            if let Some(command) = HostCoordinator::command_for_startup(startup_mode) {
                spawn_host_command(coordinator, command);
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build CodexMonitor desktop host");

    let exit_code = app.run_return(move |app, event| match event {
        tauri::RunEvent::ExitRequested { code, api, .. } if !lifecycle.exit_ready() => {
            api.prevent_exit();
            request_shutdown(app.clone(), lifecycle.clone(), code.unwrap_or(0));
        }
        tauri::RunEvent::Exit if !lifecycle.exit_ready() => {
            lifecycle.observe_native_exit();
            let deadline = request_shutdown(app.clone(), lifecycle.clone(), 0);
            if !tauri::async_runtime::block_on(lifecycle.wait_for_shutdown_before(deadline)) {
                eprintln!("codex_monitor_native_exit_shutdown_wait_timed_out");
            }
        }
        _ => {}
    });
    std::process::exit(exit_code);
}

fn prepare_startup(arguments: Vec<std::ffi::OsString>) -> (StartupMode, bool) {
    if platform_registration::cleanup_requested(&arguments) {
        match platform_registration::cleanup_platform_registration() {
            Ok(()) => std::process::exit(0),
            Err(error) => {
                eprintln!("codex_monitor_platform_registration_cleanup_failed:{error}");
                std::process::exit(1);
            }
        }
    }
    let notification_identity_ready = platform_registration::prepare_notification_identity();
    let startup_mode =
        StartupMode::parse_process_args(arguments).unwrap_or_else(|_| std::process::exit(2));
    (startup_mode, notification_identity_ready)
}

fn handle_forwarded_launch(
    app: &tauri::AppHandle,
    args: Vec<String>,
    lifecycle: &DesktopLifecycle,
) {
    let coordinator = tauri_host_coordinator(app.clone(), lifecycle.clone());
    match HostCoordinator::command_for_forwarded_launch(args) {
        Ok(Some(command)) => spawn_host_command(coordinator, command),
        Ok(None) => {}
        Err(
            StartupArgumentError::MissingExecutable
            | StartupArgumentError::InvalidLegacySource
            | StartupArgumentError::UnknownOrRepeatedArgument,
        ) => {
            eprintln!("codex_monitor_forwarded_launch_rejected");
        }
    }
}
