use codex_monitor_engine::runtime::DashboardLaunchUrl;
use tauri::{AppHandle, Runtime};
use tauri_plugin_opener::OpenerExt as _;

use crate::installed_smoke;

/// Privacy-safe platform-browser failure. Native error values may contain the
/// credential-bearing URL and therefore never cross this adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DashboardOpenError {
    Unavailable,
}

pub(crate) fn open_dashboard<R: Runtime>(
    app: &AppHandle<R>,
    url: &DashboardLaunchUrl,
) -> Result<(), DashboardOpenError> {
    if installed_smoke::capture_dashboard_launch(url.expose_to_presentation_host())
        .map_err(|_| DashboardOpenError::Unavailable)?
    {
        return Ok(());
    }
    app.opener()
        .open_url(url.expose_to_presentation_host(), None::<&str>)
        .map_err(|_| DashboardOpenError::Unavailable)
}
