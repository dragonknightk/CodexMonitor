use std::future::Future;
use std::pin::Pin;

use codex_monitor_core::{SourceExtraction, SourceHealth};

/// telemetry 消费方拥有的窄 observation sink。
///
/// 实现方只能接收 allowlisted observation 与闭合的 source health，无法访问
/// raw OTLP、任意 attribute map 或 generic JSON。
pub trait ObservationSink: Send + Sync + 'static {
    fn commit<'a>(
        &'a self,
        extraction: SourceExtraction,
        accepted_health: SourceHealth,
    ) -> Pin<Box<dyn Future<Output = Result<(), ObservationSinkError>> + Send + 'a>>;

    fn store_health<'a>(
        &'a self,
        health: SourceHealth,
    ) -> Pin<Box<dyn Future<Output = Result<(), ObservationSinkError>> + Send + 'a>>;
}

/// 跨组合边界稳定的 sink 错误代码。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationSinkErrorCode {
    DomainRejected,
    IdentityConflict,
    Unavailable,
}

impl ObservationSinkErrorCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DomainRejected => "domain_rejected",
            Self::IdentityConflict => "identity_conflict",
            Self::Unavailable => "unavailable",
        }
    }
}

/// 不携带 SQL、路径、payload 或底层错误链的闭合 sink 失败。
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("observation sink failed with code {code}", code = .code.as_str())]
pub struct ObservationSinkError {
    code: ObservationSinkErrorCode,
}

impl ObservationSinkError {
    #[must_use]
    pub const fn new(code: ObservationSinkErrorCode) -> Self {
        Self { code }
    }

    #[must_use]
    pub const fn code(self) -> ObservationSinkErrorCode {
        self.code
    }

    #[must_use]
    pub const fn is_retryable(self) -> bool {
        matches!(self.code, ObservationSinkErrorCode::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_are_closed_and_have_stable_retry_semantics() {
        assert_eq!(
            ObservationSinkError::new(ObservationSinkErrorCode::DomainRejected).code(),
            ObservationSinkErrorCode::DomainRejected,
        );
        assert!(
            !ObservationSinkError::new(ObservationSinkErrorCode::IdentityConflict).is_retryable()
        );
        assert!(ObservationSinkError::new(ObservationSinkErrorCode::Unavailable).is_retryable());
    }
}
