use codex_monitor_engine::runtime::DashboardLaunchUrl;
use tauri::{AppHandle, Runtime, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

const FUTURE_DASHBOARD_WINDOW_LABEL: &str = "codex-monitor-dashboard";

/// Privacy-safe future-window failure. Tauri and URL-parser errors can contain
/// the credential-bearing bootstrap URL, so native error values stay inside
/// this adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DashboardWindowError {
    InvalidDashboardUrl,
    Unavailable,
}

struct DashboardWindowPlan {
    external_url: Url,
    visible: bool,
}

/// Load the authenticated Dashboard in a hidden `WebView` window.
///
/// This adapter is intentionally dormant in v1: no startup, tray, forwarded
/// launch, or Tauri command calls it. A future window feature can call it with
/// the same one-time URL used by the browser and explicitly show the returned
/// window after installing its native lifecycle behavior.
#[allow(
    dead_code,
    reason = "task 9.9 requires a compiled adapter with no v1 runtime call site"
)]
pub(crate) fn load_dashboard<R: Runtime>(
    app: &AppHandle<R>,
    url: &DashboardLaunchUrl,
) -> Result<WebviewWindow<R>, DashboardWindowError> {
    load_external_dashboard(app, url.expose_to_presentation_host())
}

fn load_external_dashboard<R: Runtime>(
    app: &AppHandle<R>,
    value: &str,
) -> Result<WebviewWindow<R>, DashboardWindowError> {
    let plan = dashboard_window_plan(value)?;

    WebviewWindowBuilder::new(
        app,
        FUTURE_DASHBOARD_WINDOW_LABEL,
        WebviewUrl::External(plan.external_url),
    )
    .visible(plan.visible)
    .build()
    .map_err(|_| DashboardWindowError::Unavailable)
}

fn dashboard_window_plan(value: &str) -> Result<DashboardWindowPlan, DashboardWindowError> {
    let external_url = Url::parse(value).map_err(|_| DashboardWindowError::InvalidDashboardUrl)?;
    if external_url.scheme() != "http"
        || external_url.host_str() != Some("127.0.0.1")
        || external_url.port().is_none()
    {
        return Err(DashboardWindowError::InvalidDashboardUrl);
    }

    Ok(DashboardWindowPlan {
        external_url,
        visible: false,
    })
}

#[cfg(test)]
mod tests {
    use super::{DashboardWindowError, dashboard_window_plan};

    const SYNTHETIC_DASHBOARD_URL: &str =
        "http://127.0.0.1:4319/api/v1/session/bootstrap?token=synthetic-bootstrap-token";

    #[test]
    fn adapter_preserves_the_dashboard_url_and_starts_hidden() {
        let plan = dashboard_window_plan(SYNTHETIC_DASHBOARD_URL).unwrap();

        assert_eq!(plan.external_url.as_str(), SYNTHETIC_DASHBOARD_URL);
        assert!(!plan.visible);
    }

    #[test]
    fn adapter_rejects_non_loopback_urls_with_closed_errors() {
        for value in [
            "https://127.0.0.1:4319/api/v1/session/bootstrap?token=synthetic-secret",
            "http://localhost:4319/api/v1/session/bootstrap?token=synthetic-secret",
            "http://127.0.0.1/api/v1/session/bootstrap?token=synthetic-secret",
        ] {
            let Err(error) = dashboard_window_plan(value) else {
                panic!("an invalid Dashboard presentation URL must be rejected");
            };

            assert_eq!(error, DashboardWindowError::InvalidDashboardUrl);
            assert_eq!(format!("{error:?}"), "InvalidDashboardUrl");
        }
    }
}
