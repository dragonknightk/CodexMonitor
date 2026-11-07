use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::header::{ALLOW, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};

use super::api_error::REFERRER_POLICY;
use super::state::DashboardState;

const INDEX_PATH: &str = "index.html";
const ASSET_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
const INDEX_CACHE_CONTROL: &str = "no-store";
const OTHER_CACHE_CONTROL: &str = "no-cache";
const CONTENT_SECURITY_POLICY: &str = concat!(
    "default-src 'self'; ",
    "base-uri 'none'; ",
    "connect-src 'self'; ",
    "form-action 'none'; ",
    "frame-ancestors 'none'; ",
    "img-src 'self' data:; ",
    "object-src 'none'; ",
    "script-src 'self'; ",
    "style-src 'self'"
);
const MAX_ASSET_COUNT: usize = 512;
const MAX_ASSET_PATH_BYTES: usize = 512;
const MAX_SINGLE_ASSET_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOTAL_ASSET_BYTES: usize = 64 * 1024 * 1024;
const CONTENT_SECURITY_POLICY_HEADER: HeaderName =
    HeaderName::from_static("content-security-policy");
const X_CONTENT_TYPE_OPTIONS: HeaderName = HeaderName::from_static("x-content-type-options");
const X_FRAME_OPTIONS: HeaderName = HeaderName::from_static("x-frame-options");

/// One build-time Dashboard asset supplied by the native presentation host.
pub struct DashboardAssetInput {
    path: String,
    bytes: Vec<u8>,
    content_type: String,
}

impl DashboardAssetInput {
    #[must_use]
    pub fn new(
        path: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
        content_type: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            bytes: bytes.into(),
            content_type: content_type.into(),
        }
    }
}

/// Closed validation failures for the immutable Dashboard presentation bundle.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DashboardAssetsError {
    #[error("the Dashboard presentation bundle contains too many assets")]
    TooManyAssets,
    #[error("a Dashboard presentation asset path is invalid")]
    InvalidPath,
    #[error("the Dashboard presentation bundle contains a duplicate path")]
    DuplicatePath,
    #[error("a Dashboard presentation asset is empty or exceeds its size bound")]
    InvalidAssetSize,
    #[error("the Dashboard presentation bundle exceeds its total size bound")]
    TotalSizeExceeded,
    #[error("a Dashboard presentation asset has an invalid content type")]
    InvalidContentType,
    #[error("the Dashboard presentation bundle is missing a valid index document")]
    MissingIndex,
}

#[derive(Clone)]
struct DashboardAsset {
    bytes: Bytes,
    content_type: HeaderValue,
}

/// Validated immutable assets served by the loopback Dashboard listener.
#[derive(Clone)]
pub struct DashboardAssets {
    assets: Arc<BTreeMap<String, DashboardAsset>>,
}

impl DashboardAssets {
    /// Validate and own the complete presentation bundle before the listener starts.
    ///
    /// # Errors
    ///
    /// Returns a closed validation error when any asset path, size or MIME value
    /// is invalid, or when the bundle has no valid `index.html` document.
    pub fn new(
        inputs: impl IntoIterator<Item = DashboardAssetInput>,
    ) -> Result<Self, DashboardAssetsError> {
        let mut assets = BTreeMap::new();
        let mut total_bytes = 0_usize;

        for input in inputs {
            if assets.len() == MAX_ASSET_COUNT {
                return Err(DashboardAssetsError::TooManyAssets);
            }
            validate_asset_path(&input.path)?;
            if input.bytes.is_empty() || input.bytes.len() > MAX_SINGLE_ASSET_BYTES {
                return Err(DashboardAssetsError::InvalidAssetSize);
            }
            total_bytes = total_bytes
                .checked_add(input.bytes.len())
                .filter(|total| *total <= MAX_TOTAL_ASSET_BYTES)
                .ok_or(DashboardAssetsError::TotalSizeExceeded)?;
            let content_type = input
                .content_type
                .parse::<mime::Mime>()
                .map_err(|_| DashboardAssetsError::InvalidContentType)?
                .to_string();
            let content_type = HeaderValue::from_str(&content_type)
                .map_err(|_| DashboardAssetsError::InvalidContentType)?;
            let asset = DashboardAsset {
                bytes: Bytes::from(input.bytes),
                content_type,
            };
            if assets.insert(input.path, asset).is_some() {
                return Err(DashboardAssetsError::DuplicatePath);
            }
        }

        let index = assets
            .get(INDEX_PATH)
            .ok_or(DashboardAssetsError::MissingIndex)?;
        let index_content_type = index
            .content_type
            .to_str()
            .ok()
            .and_then(|value| value.parse::<mime::Mime>().ok());
        if index_content_type.as_ref().map(mime::Mime::essence_str)
            != Some(mime::TEXT_HTML.as_ref())
        {
            return Err(DashboardAssetsError::MissingIndex);
        }

        Ok(Self {
            assets: Arc::new(assets),
        })
    }

    fn response(&self, path: &str) -> Response {
        let Some(asset) = self.assets.get(path) else {
            return missing_response();
        };
        let cache_control = if path == INDEX_PATH {
            INDEX_CACHE_CONTROL
        } else if path.starts_with("assets/") {
            ASSET_CACHE_CONTROL
        } else {
            OTHER_CACHE_CONTROL
        };

        let mut response = Response::new(Body::from(asset.bytes.clone()));
        let headers = response.headers_mut();
        headers.insert(CONTENT_TYPE, asset.content_type.clone());
        headers.insert(
            CONTENT_LENGTH,
            HeaderValue::from_str(&asset.bytes.len().to_string())
                .expect("a byte length is always a valid header value"),
        );
        headers.insert(CACHE_CONTROL, HeaderValue::from_static(cache_control));
        headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
        if path == INDEX_PATH {
            headers.insert(
                CONTENT_SECURITY_POLICY_HEADER,
                HeaderValue::from_static(CONTENT_SECURITY_POLICY),
            );
            headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
            headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
        }
        response
    }
}

pub(super) async fn serve_dashboard_index(State(state): State<DashboardState>) -> Response {
    state.assets.response(INDEX_PATH)
}

pub(super) async fn serve_dashboard_fallback(
    State(state): State<DashboardState>,
    request: Request,
) -> Response {
    let Some(path) = request.uri().path().strip_prefix('/') else {
        return missing_response();
    };
    if !state.assets.assets.contains_key(path) {
        return missing_response();
    }

    match *request.method() {
        Method::GET => state.assets.response(path),
        Method::HEAD => {
            let mut response = state.assets.response(path);
            *response.body_mut() = Body::empty();
            response
        }
        _ => method_not_allowed_response(),
    }
}

fn missing_response() -> Response {
    let mut response = StatusCode::NOT_FOUND.into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static(INDEX_CACHE_CONTROL));
    response
        .headers_mut()
        .insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response
}

fn method_not_allowed_response() -> Response {
    let mut response = StatusCode::METHOD_NOT_ALLOWED.into_response();
    let headers = response.headers_mut();
    headers.insert(ALLOW, HeaderValue::from_static("GET, HEAD"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static(INDEX_CACHE_CONTROL));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response
}

fn validate_asset_path(path: &str) -> Result<(), DashboardAssetsError> {
    if path.is_empty()
        || path.len() > MAX_ASSET_PATH_BYTES
        || path.starts_with('/')
        || path.ends_with('/')
        || !path.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'-' | b'_' | b'~')
        })
        || path
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(DashboardAssetsError::InvalidPath);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn test_dashboard_assets() -> DashboardAssets {
    DashboardAssets::new([
        DashboardAssetInput::new(
            "index.html",
            concat!(
                "<!doctype html><html><head>",
                "<script type=\"module\" src=\"/assets/index-synthetic.js\"></script>",
                "<link rel=\"stylesheet\" href=\"/assets/index-synthetic.css\">",
                "</head><body><div id=\"root\"></div></body></html>"
            )
            .as_bytes()
            .to_vec(),
            "text/html",
        ),
        DashboardAssetInput::new(
            "assets/index-synthetic.js",
            b"document.body.dataset.ready = 'true';".to_vec(),
            "text/javascript",
        ),
        DashboardAssetInput::new(
            "assets/index-synthetic.css",
            b":root { color-scheme: light dark; }".to_vec(),
            "text/css",
        ),
    ])
    .expect("the fixed synthetic Dashboard bundle is valid")
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};

    use super::*;

    #[tokio::test]
    async fn index_and_hashed_assets_have_distinct_closed_cache_policies() {
        let index = test_dashboard_assets().response("index.html");
        assert_eq!(index.status(), StatusCode::OK);
        assert_eq!(index.headers()[CONTENT_TYPE], "text/html");
        assert_eq!(index.headers()[CACHE_CONTROL], INDEX_CACHE_CONTROL);
        assert_eq!(
            index.headers()[CONTENT_SECURITY_POLICY_HEADER],
            CONTENT_SECURITY_POLICY
        );

        let script = test_dashboard_assets().response("assets/index-synthetic.js");
        assert_eq!(script.status(), StatusCode::OK);
        assert_eq!(script.headers()[CONTENT_TYPE], "text/javascript");
        assert_eq!(script.headers()[CACHE_CONTROL], ASSET_CACHE_CONTROL);
        let body = to_bytes(script.into_body(), 1024).await.unwrap();
        assert_eq!(body, "document.body.dataset.ready = 'true';");
    }

    #[test]
    fn invalid_or_incomplete_bundles_fail_closed() {
        for path in [
            "../index.html",
            "assets/index%20test.js",
            "assets/index test.js",
            "assets/索引.js",
        ] {
            let invalid_path = DashboardAssets::new([DashboardAssetInput::new(
                path,
                b"index".to_vec(),
                "text/html",
            )]);
            assert!(matches!(
                invalid_path,
                Err(DashboardAssetsError::InvalidPath)
            ));
        }

        let missing_index = DashboardAssets::new([DashboardAssetInput::new(
            "assets/index.js",
            b"script".to_vec(),
            "text/javascript",
        )]);
        assert!(matches!(
            missing_index,
            Err(DashboardAssetsError::MissingIndex)
        ));

        let invalid_index_mime = DashboardAssets::new([DashboardAssetInput::new(
            "index.html",
            b"index".to_vec(),
            "text/plain",
        )]);
        assert!(matches!(
            invalid_index_mime,
            Err(DashboardAssetsError::MissingIndex)
        ));
    }

    #[tokio::test]
    async fn unknown_assets_never_fall_back_to_the_index_document() {
        let response = test_dashboard_assets().response("assets/missing.js");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.headers()[CACHE_CONTROL], INDEX_CACHE_CONTROL);
        assert_eq!(response.headers()[X_CONTENT_TYPE_OPTIONS], "nosniff");
        let body = to_bytes(response.into_body(), 1).await.unwrap();
        assert!(body.is_empty());
    }
}
