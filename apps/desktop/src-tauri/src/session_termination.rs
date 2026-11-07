use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use signal_hook::consts::signal::SIGTERM;
use signal_hook::flag;
use signal_hook::iterator::Signals;
use tauri::{AppHandle, Wry};

use crate::lifecycle::{DesktopLifecycle, request_shutdown};

const FORCED_SIGTERM_EXIT_CODE: i32 = 128 + 15;

/// Register the Linux user-session termination path before resident startup.
///
/// Once installation succeeds, the first delivered `SIGTERM` wakes a blocking
/// observer and arms an async-signal-safe forced exit. Any later delivered
/// `SIGTERM` exits with the conventional status immediately, even if graceful
/// shutdown is stuck.
pub(crate) fn install(app: AppHandle<Wry>, lifecycle: DesktopLifecycle) -> io::Result<()> {
    let force_exit_armed = Arc::new(AtomicBool::new(false));
    flag::register_conditional_shutdown(
        SIGTERM,
        FORCED_SIGTERM_EXIT_CODE,
        Arc::clone(&force_exit_armed),
    )?;
    let mut terminate = Signals::new([SIGTERM])?;
    flag::register(SIGTERM, force_exit_armed)?;

    let _ = tauri::async_runtime::spawn_blocking(move || {
        if terminate.forever().next().is_some() {
            request_shutdown(app, lifecycle, 0);
        }
    });
    Ok(())
}
