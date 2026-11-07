//! Privacy-safe structured diagnostics for the resident engine.
//!
//! The writer accepts only closed event variants and bounded scalar metadata.
//! Raw payloads, protocol headers, parser errors, paths, and source identifiers
//! have no representation in this API.

use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::path::Path;
use std::sync::{Arc, Mutex};

use codex_monitor_core::{SafeErrorCode, UtcTimestamp};
use serde::Serialize;

/// File name reserved for the engine's current-user JSONL diagnostics.
pub const ENGINE_LOG_FILE: &str = "engine.jsonl";

/// One privacy-reviewed diagnostic event.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "event_code", rename_all = "snake_case")]
pub enum StructuredDiagnosticEvent {
    /// The bounded OTLP decoder rejected a request.
    OtlpDecodeFailed {
        occurred_at: UtcTimestamp,
        error_code: SafeErrorCode,
        #[serde(skip_serializing_if = "Option::is_none")]
        encoding: Option<OtlpDiagnosticEncoding>,
        #[serde(skip_serializing_if = "Option::is_none")]
        wire_bytes: Option<u64>,
    },
    /// Typed OTLP processing failed after decoding.
    OtlpProcessingFailed {
        occurred_at: UtcTimestamp,
        error_code: SafeErrorCode,
        encoding: OtlpDiagnosticEncoding,
        retryable: bool,
    },
    /// The outer request deadline elapsed before a response was produced.
    OtlpRequestTimedOut {
        occurred_at: UtcTimestamp,
        error_code: SafeErrorCode,
    },
}

/// Closed wire-encoding labels allowed in structured diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OtlpDiagnosticEncoding {
    Json,
    Protobuf,
}

/// Cloneable append-only JSONL writer shared by receiver request tasks.
#[derive(Clone)]
pub struct StructuredDiagnosticLog {
    destination: Arc<DiagnosticDestination>,
}

enum DiagnosticDestination {
    File(Mutex<File>),
    #[cfg(test)]
    Memory(Mutex<Vec<u8>>),
}

impl StructuredDiagnosticLog {
    /// Open the already-secured current-user diagnostic file for appends.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the existing file cannot be opened for append.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().append(true).open(path)?;
        Ok(Self {
            destination: Arc::new(DiagnosticDestination::File(Mutex::new(file))),
        })
    }

    #[cfg(test)]
    pub(crate) fn memory() -> Self {
        Self {
            destination: Arc::new(DiagnosticDestination::Memory(Mutex::new(Vec::new()))),
        }
    }

    #[cfg(test)]
    pub(crate) fn memory_contents(&self) -> Vec<u8> {
        let DiagnosticDestination::Memory(bytes) = self.destination.as_ref() else {
            panic!("memory_contents requires an in-memory diagnostic log");
        };
        bytes.lock().unwrap().clone()
    }

    /// Append and flush one complete JSONL event.
    ///
    /// The event is serialized before the file lock is acquired so a
    /// serialization failure cannot leave a partial line in the log.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when serialization, locking, writing, or flushing
    /// the complete event fails.
    pub fn record(&self, event: StructuredDiagnosticEvent) -> io::Result<()> {
        let mut line = serde_json::to_vec(&event).map_err(io::Error::other)?;
        line.push(b'\n');
        match self.destination.as_ref() {
            DiagnosticDestination::File(file) => {
                let mut file = file
                    .lock()
                    .map_err(|_| io::Error::other("structured diagnostic log lock poisoned"))?;
                file.write_all(&line)?;
                file.flush()
            }
            #[cfg(test)]
            DiagnosticDestination::Memory(bytes) => bytes
                .lock()
                .map_err(|_| io::Error::other("structured diagnostic log lock poisoned"))?
                .write_all(&line),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn at(seconds: i128) -> UtcTimestamp {
        UtcTimestamp::from_unix_timestamp_nanos(seconds * 1_000_000_000).unwrap()
    }

    #[test]
    fn jsonl_schema_can_represent_only_closed_safe_fields() {
        let directory = tempdir().unwrap();
        let path = directory.path().join(ENGINE_LOG_FILE);
        File::create(&path).unwrap();
        let log = StructuredDiagnosticLog::open(&path).unwrap();
        log.record(StructuredDiagnosticEvent::OtlpDecodeFailed {
            occurred_at: at(1),
            error_code: SafeErrorCode::OtlpJsonDecodeFailed,
            encoding: Some(OtlpDiagnosticEncoding::Json),
            wire_bytes: Some(91),
        })
        .unwrap();

        let line = fs::read_to_string(path).unwrap();
        let value: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(value["event_code"], "otlp_decode_failed");
        assert_eq!(value["error_code"], "otlp_json_decode_failed");
        assert_eq!(value["encoding"], "json");
        assert_eq!(value["wire_bytes"], 91);
        assert_eq!(value.as_object().unwrap().len(), 5);
    }
}
