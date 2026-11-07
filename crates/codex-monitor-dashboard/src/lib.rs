//! 经过身份验证的本地 Dashboard 传输层。

mod api_error;
mod assets;
mod capabilities_http;
#[cfg(test)]
mod contract_tests;
mod cursor;
mod events_http;
mod listener;
mod ports;
mod query_http;
mod query_input;
mod query_response;
mod request;
mod routing;
mod security;
mod session;
mod session_http;
mod settings_http;
mod state;

pub use api_error::{ValidationField, ValidationIssue};
pub use assets::{DashboardAssetInput, DashboardAssets, DashboardAssetsError};
pub use listener::{
    DASHBOARD_DEFAULT_ADDRESS, DashboardListenerHandle, DashboardListenerShutdownError,
    DashboardListenerStartError, spawn_dashboard_listener,
};
pub use ports::*;
pub use query_http::dashboard_router;
pub use query_input::{
    AlertMatchInput, AlertQueryInput, ConversationQueryInput, ModelCallQueryInput,
    ModelCallSortInput, ModelFiltersInput,
};
pub use request::{
    DashboardRequestBody, MAX_DASHBOARD_JSON_BODY_BYTES, ValidatedEmptyBody, ValidatedJson,
};
pub use routing::{DashboardApi, DashboardRouter};
pub use security::{CSRF_HEADER_NAME, SESSION_COOKIE_NAME};
pub use session::{
    AuthenticatedSession, AuthorizedStateChange, BOOTSTRAP_TOKEN_TTL, BootstrapConsumeError,
    BootstrapToken, DashboardSessionError, DashboardSessions, MAX_ACTIVE_SESSIONS,
    MAX_PENDING_BOOTSTRAP_TOKENS, SESSION_TTL, SessionAuthenticationError,
};
#[cfg(test)]
pub(crate) use session_http::dashboard_session_router;
pub use state::{
    DashboardPlatformCapabilities, DashboardPlatformCapabilityMonitor,
    DashboardPlatformCapabilitySnapshot, DashboardState, SystemNotificationAvailability,
    TrayAvailability,
};
