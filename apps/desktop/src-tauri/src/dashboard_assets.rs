use std::error::Error;
use std::fmt;

use codex_monitor_dashboard::{DashboardAssetInput, DashboardAssets, DashboardAssetsError};
use tauri::{App, Runtime};

/// Convert Tauri's build-time `frontendDist` bundle into the engine-owned
/// immutable HTTP presentation bundle.
pub(crate) fn load<R: Runtime>(
    app: &App<R>,
) -> Result<DashboardAssets, EmbeddedDashboardAssetsError> {
    let resolver = app.asset_resolver();
    // The iterator exposes rooted keys and may expose compressed bytes. Keep the
    // rooted key for `get`, which performs Tauri's decompression and MIME work,
    // while validating the exact relative key before Tauri can percent-decode it.
    let paths = resolver
        .iter()
        .map(|(path, _)| path.into_owned())
        .collect::<Vec<_>>();
    let mut inputs = Vec::with_capacity(paths.len());

    for rooted_path in paths {
        let relative_path = relative_asset_path(&rooted_path)?.to_owned();
        let asset = resolver
            .get(rooted_path)
            .ok_or(EmbeddedDashboardAssetsError::Resolution)?;
        inputs.push(DashboardAssetInput::new(
            relative_path,
            asset.bytes,
            asset.mime_type,
        ));
    }

    DashboardAssets::new(inputs).map_err(EmbeddedDashboardAssetsError::Invalid)
}

fn relative_asset_path(rooted_path: &str) -> Result<&str, EmbeddedDashboardAssetsError> {
    let relative_path = rooted_path
        .strip_prefix('/')
        .ok_or(EmbeddedDashboardAssetsError::InvalidKey)?;
    if relative_path.is_empty()
        || relative_path.starts_with('/')
        || !relative_path.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'-' | b'_' | b'~')
        })
    {
        return Err(EmbeddedDashboardAssetsError::InvalidKey);
    }
    Ok(relative_path)
}

#[derive(Debug)]
pub(crate) enum EmbeddedDashboardAssetsError {
    InvalidKey,
    Resolution,
    Invalid(DashboardAssetsError),
}

impl fmt::Display for EmbeddedDashboardAssetsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidKey => {
                formatter.write_str("an embedded Dashboard asset key is not canonical")
            }
            Self::Resolution => {
                formatter.write_str("an embedded Dashboard asset could not be resolved")
            }
            Self::Invalid(error) => error.fmt(formatter),
        }
    }
}

impl Error for EmbeddedDashboardAssetsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidKey | Self::Resolution => None,
            Self::Invalid(error) => Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tauri_rooted_asset_keys_become_exact_engine_relative_paths() {
        assert_eq!(relative_asset_path("/index.html").unwrap(), "index.html");
        assert_eq!(
            relative_asset_path("/assets/index-synthetic.js").unwrap(),
            "assets/index-synthetic.js"
        );

        for invalid in [
            "index.html",
            "/",
            "//index.html",
            "/assets/%69ndex-synthetic.js",
            "/assets/index synthetic.js",
            "/assets/索引.js",
        ] {
            assert!(matches!(
                relative_asset_path(invalid),
                Err(EmbeddedDashboardAssetsError::InvalidKey)
            ));
        }
    }
}
