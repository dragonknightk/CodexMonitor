//! 受隐私边界约束的 OTLP ingestion 与 telemetry source adapter。

mod decoder;
mod diagnostics;
mod http;
#[cfg(test)]
mod legacy_contract;
mod otlp;
mod ports;
mod processor;
mod receiver;
mod session_index;
mod source;

pub use codex_monitor_core::{
    SourceExtraction, SourceObservationCandidate, SourceObservationError, SourceObservationKind,
};
pub use decoder::{
    DecodedOtlpRequest, MAX_OTLP_REQUEST_TIMEOUT, OtlpDecodeError, OtlpEncoding, OtlpLimitError,
    OtlpRequestLimits, decode_otlp_request,
};
pub use diagnostics::{
    ENGINE_LOG_FILE, OtlpDiagnosticEncoding, StructuredDiagnosticEvent, StructuredDiagnosticLog,
};
pub use http::{OtlpProcessError, OtlpProcessOutcome, OtlpRequestProcessor, otlp_http_router};
pub use otlp::OtlpLogSource;
pub use ports::{ObservationSink, ObservationSinkError, ObservationSinkErrorCode};
pub use processor::{OtlpObservationProcessor, OtlpSourceHealthMonitor};
#[cfg(feature = "test-support")]
pub use receiver::OtlpReceiverTestController;
pub use receiver::{
    OTLP_RECEIVER_ADDRESS, OtlpReceiverBindState, OtlpReceiverControl, OtlpReceiverHandle,
    OtlpReceiverMonitor, OtlpReceiverRetryError, OtlpReceiverShutdownError, OtlpReceiverSnapshot,
    OtlpReceiverStartError, spawn_otlp_receiver,
};
pub use session_index::{
    MAX_SESSION_DISPLAY_NAME_BYTES, MAX_SESSION_DISPLAY_NAME_CHARACTERS, MAX_SESSION_INDEX_ENTRIES,
    MAX_SESSION_INDEX_FILE_BYTES, MAX_SESSION_INDEX_ID_BYTES, MAX_SESSION_INDEX_LINE_BYTES,
    SessionIndexLimits, SessionIndexRefresh, SessionIndexResolver, SessionIndexState,
};
pub use source::{CheckpointCapability, TelemetrySource, TelemetrySourceDefinition};
