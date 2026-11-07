use std::io::Read;
use std::time::Duration;

use codex_monitor_core::SafeErrorCode;
use flate2::read::GzDecoder;
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use prost::Message;
use thiserror::Error;

const DEFAULT_MAX_COMPRESSED_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_MAX_DECOMPRESSED_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_OTLP_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_REQUEST_TIMEOUT: Duration = MAX_OTLP_REQUEST_TIMEOUT;

/// Supported OTLP/HTTP wire encodings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OtlpEncoding {
    Protobuf,
    Json,
}

impl OtlpEncoding {
    #[must_use]
    pub const fn content_type(self) -> &'static str {
        match self {
            Self::Protobuf => "application/x-protobuf",
            Self::Json => "application/json",
        }
    }

    /// Parse and strictly validate an OTLP request media type.
    ///
    /// JSON may include the standard UTF-8 charset parameter. No other media
    /// type or parameter crosses the protocol boundary.
    ///
    /// # Errors
    ///
    /// Returns a closed decode error for a missing or unsupported value.
    pub fn parse_content_type(value: Option<&str>) -> Result<Self, OtlpDecodeError> {
        let value = value.ok_or(OtlpDecodeError::InvalidContentType)?;
        let mut segments = value.split(';');
        let media_type = segments
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or(OtlpDecodeError::InvalidContentType)?;

        let encoding = if media_type.eq_ignore_ascii_case("application/x-protobuf") {
            Self::Protobuf
        } else if media_type.eq_ignore_ascii_case("application/json") {
            Self::Json
        } else {
            return Err(OtlpDecodeError::InvalidContentType);
        };

        for parameter in segments {
            let parameter = parameter.trim();
            let valid_json_charset = encoding == Self::Json
                && parameter.split_once('=').is_some_and(|(name, charset)| {
                    name.trim().eq_ignore_ascii_case("charset")
                        && charset.trim().eq_ignore_ascii_case("utf-8")
                });
            if !valid_json_charset {
                return Err(OtlpDecodeError::InvalidContentType);
            }
        }
        Ok(encoding)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContentEncoding {
    Identity,
    Gzip,
}

impl ContentEncoding {
    fn parse(value: Option<&str>) -> Result<Self, OtlpDecodeError> {
        match value.map(str::trim) {
            None | Some("") => Ok(Self::Identity),
            Some(value) if value.eq_ignore_ascii_case("identity") => Ok(Self::Identity),
            Some(value) if value.eq_ignore_ascii_case("gzip") => Ok(Self::Gzip),
            Some(_) => Err(OtlpDecodeError::InvalidContentEncoding),
        }
    }
}

/// Independent allocation and time bounds for one OTLP request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OtlpRequestLimits {
    max_compressed_bytes: usize,
    max_decompressed_bytes: usize,
    request_timeout: Duration,
}

impl OtlpRequestLimits {
    /// Create validated limits.
    ///
    /// # Errors
    ///
    /// Returns an error when either byte bound is zero or the request timeout
    /// falls outside the receiver's bounded shutdown budget.
    pub const fn new(
        max_compressed_bytes: usize,
        max_decompressed_bytes: usize,
        request_timeout: Duration,
    ) -> Result<Self, OtlpLimitError> {
        if max_compressed_bytes == 0 {
            return Err(OtlpLimitError::ZeroCompressedLimit);
        }
        if max_decompressed_bytes == 0 {
            return Err(OtlpLimitError::ZeroDecompressedLimit);
        }
        if request_timeout.is_zero() {
            return Err(OtlpLimitError::ZeroRequestTimeout);
        }
        if request_timeout.as_nanos() > MAX_OTLP_REQUEST_TIMEOUT.as_nanos() {
            return Err(OtlpLimitError::RequestTimeoutTooLarge);
        }
        Ok(Self {
            max_compressed_bytes,
            max_decompressed_bytes,
            request_timeout,
        })
    }

    #[must_use]
    pub const fn max_compressed_bytes(self) -> usize {
        self.max_compressed_bytes
    }

    #[must_use]
    pub const fn max_decompressed_bytes(self) -> usize {
        self.max_decompressed_bytes
    }

    #[must_use]
    pub const fn request_timeout(self) -> Duration {
        self.request_timeout
    }
}

impl Default for OtlpRequestLimits {
    fn default() -> Self {
        Self {
            max_compressed_bytes: DEFAULT_MAX_COMPRESSED_BYTES,
            max_decompressed_bytes: DEFAULT_MAX_DECOMPRESSED_BYTES,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }
}

/// Invalid receiver-limit configuration.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum OtlpLimitError {
    #[error("compressed OTLP body limit must be non-zero")]
    ZeroCompressedLimit,
    #[error("decompressed OTLP body limit must be non-zero")]
    ZeroDecompressedLimit,
    #[error("OTLP request timeout must be non-zero")]
    ZeroRequestTimeout,
    #[error("OTLP request timeout exceeds the receiver shutdown budget")]
    RequestTimeoutTooLarge,
}

/// A decoded request paired with the validated response encoding.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedOtlpRequest {
    pub encoding: OtlpEncoding,
    pub request: ExportLogsServiceRequest,
}

/// Privacy-safe OTLP protocol failure. It intentionally stores no parser
/// error, header text or payload excerpt.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum OtlpDecodeError {
    #[error("unsupported or missing OTLP content type")]
    InvalidContentType,
    #[error("unsupported OTLP content encoding")]
    InvalidContentEncoding,
    #[error("compressed OTLP request exceeds its limit")]
    CompressedBodyTooLarge,
    #[error("decompressed OTLP request exceeds its limit")]
    DecompressedBodyTooLarge,
    #[error("OTLP gzip body cannot be decoded")]
    GzipDecodeFailed,
    #[error("OTLP JSON body cannot be decoded")]
    JsonDecodeFailed,
    #[error("OTLP Protobuf body cannot be decoded")]
    ProtobufDecodeFailed,
    #[error("OTLP decoding exceeded its time limit")]
    DecodeTimedOut,
    #[error("OTLP decode worker is unavailable")]
    DecodeWorkerUnavailable,
}

impl OtlpDecodeError {
    #[must_use]
    pub const fn safe_code(self) -> SafeErrorCode {
        match self {
            Self::InvalidContentType | Self::InvalidContentEncoding => {
                SafeErrorCode::OtlpInvalidContentType
            }
            Self::CompressedBodyTooLarge => SafeErrorCode::OtlpCompressedBodyTooLarge,
            Self::DecompressedBodyTooLarge => SafeErrorCode::OtlpDecompressedBodyTooLarge,
            Self::GzipDecodeFailed => SafeErrorCode::OtlpGzipDecodeFailed,
            Self::JsonDecodeFailed => SafeErrorCode::OtlpJsonDecodeFailed,
            Self::ProtobufDecodeFailed => SafeErrorCode::OtlpProtobufDecodeFailed,
            Self::DecodeTimedOut => SafeErrorCode::OtlpDecodeTimedOut,
            Self::DecodeWorkerUnavailable => SafeErrorCode::SourceUnavailable,
        }
    }

    #[must_use]
    pub const fn retryable(self) -> bool {
        matches!(self, Self::DecodeTimedOut | Self::DecodeWorkerUnavailable)
    }
}

/// Decode one already-bounded HTTP body on a blocking worker.
///
/// The compressed limit is checked before dispatch. Gzip expansion is read
/// through a hard cap, and the entire decompress-and-parse job is observed
/// through a timeout.
///
/// # Errors
///
/// Returns only closed protocol errors; raw parser failures are discarded.
pub async fn decode_otlp_request(
    body: Vec<u8>,
    content_type: Option<&str>,
    content_encoding: Option<&str>,
    limits: OtlpRequestLimits,
) -> Result<DecodedOtlpRequest, OtlpDecodeError> {
    let encoding = OtlpEncoding::parse_content_type(content_type)?;
    let content_encoding = ContentEncoding::parse(content_encoding)?;
    if body.len() > limits.max_compressed_bytes {
        return Err(OtlpDecodeError::CompressedBodyTooLarge);
    }

    let worker = tokio::task::spawn_blocking(move || {
        decode_bounded(body, encoding, content_encoding, limits)
    });
    match tokio::time::timeout(limits.request_timeout, worker).await {
        Err(_) => Err(OtlpDecodeError::DecodeTimedOut),
        Ok(Err(_)) => Err(OtlpDecodeError::DecodeWorkerUnavailable),
        Ok(Ok(result)) => result,
    }
}

fn decode_bounded(
    body: Vec<u8>,
    encoding: OtlpEncoding,
    content_encoding: ContentEncoding,
    limits: OtlpRequestLimits,
) -> Result<DecodedOtlpRequest, OtlpDecodeError> {
    let decoded = match content_encoding {
        ContentEncoding::Identity => body,
        ContentEncoding::Gzip => decompress_gzip(&body, limits.max_decompressed_bytes)?,
    };
    if decoded.len() > limits.max_decompressed_bytes {
        return Err(OtlpDecodeError::DecompressedBodyTooLarge);
    }

    let request = match encoding {
        OtlpEncoding::Protobuf => ExportLogsServiceRequest::decode(decoded.as_slice())
            .map_err(|_| OtlpDecodeError::ProtobufDecodeFailed)?,
        OtlpEncoding::Json => {
            serde_json::from_slice(&decoded).map_err(|_| OtlpDecodeError::JsonDecodeFailed)?
        }
    };
    Ok(DecodedOtlpRequest { encoding, request })
}

fn decompress_gzip(body: &[u8], limit: usize) -> Result<Vec<u8>, OtlpDecodeError> {
    let read_limit = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    let initial_capacity = limit.min(8 * 1024);
    let mut decoded = Vec::with_capacity(initial_capacity);
    let mut reader = GzDecoder::new(body).take(read_limit);
    reader
        .read_to_end(&mut decoded)
        .map_err(|_| OtlpDecodeError::GzipDecodeFailed)?;
    if decoded.len() > limit {
        return Err(OtlpDecodeError::DecompressedBodyTooLarge);
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::{Compression, write::GzEncoder};
    use proptest::prelude::*;
    use prost::Message;

    use super::*;

    const FIXTURE: &str = include_str!("../../../fixtures/otlp/response_completed.json");

    fn limits(compressed: usize, decompressed: usize) -> OtlpRequestLimits {
        OtlpRequestLimits::new(compressed, decompressed, Duration::from_secs(2)).unwrap()
    }

    fn gzip(value: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(value).unwrap();
        encoder.finish().unwrap()
    }

    #[tokio::test]
    async fn decodes_upstream_otlp_json_and_binary_protobuf() {
        let json = decode_otlp_request(
            FIXTURE.as_bytes().to_vec(),
            Some("application/json; charset=UTF-8"),
            None,
            OtlpRequestLimits::default(),
        )
        .await
        .unwrap();
        assert_eq!(json.encoding, OtlpEncoding::Json);

        let binary = json.request.encode_to_vec();
        let protobuf = decode_otlp_request(
            binary,
            Some("application/x-protobuf"),
            Some("identity"),
            OtlpRequestLimits::default(),
        )
        .await
        .unwrap();
        assert_eq!(protobuf.encoding, OtlpEncoding::Protobuf);
        assert_eq!(protobuf.request, json.request);
    }

    #[tokio::test]
    async fn gzip_has_independent_wire_and_expansion_limits() {
        let compressed = gzip(FIXTURE.as_bytes());
        let decoded = decode_otlp_request(
            compressed.clone(),
            Some("application/json"),
            Some("gzip"),
            limits(compressed.len(), FIXTURE.len()),
        )
        .await
        .unwrap();
        assert_eq!(decoded.encoding, OtlpEncoding::Json);

        let wire_error = decode_otlp_request(
            compressed.clone(),
            Some("application/json"),
            Some("gzip"),
            limits(compressed.len() - 1, FIXTURE.len()),
        )
        .await
        .unwrap_err();
        assert_eq!(wire_error, OtlpDecodeError::CompressedBodyTooLarge);

        let expansion_error = decode_otlp_request(
            compressed,
            Some("application/json"),
            Some("gzip"),
            limits(FIXTURE.len(), FIXTURE.len() - 1),
        )
        .await
        .unwrap_err();
        assert_eq!(expansion_error, OtlpDecodeError::DecompressedBodyTooLarge);
    }

    #[tokio::test]
    async fn malformed_inputs_return_only_closed_safe_codes() {
        let cases = [
            (
                include_bytes!("../../../fixtures/otlp/malformed.json").to_vec(),
                Some("application/json"),
                None,
                OtlpDecodeError::JsonDecodeFailed,
            ),
            (
                decode_hex(include_str!(
                    "../../../fixtures/otlp/malformed.protobuf.hex"
                )),
                Some("application/x-protobuf"),
                None,
                OtlpDecodeError::ProtobufDecodeFailed,
            ),
            (
                include_bytes!("../../../fixtures/otlp/malformed.gzip.txt").to_vec(),
                Some("application/json"),
                Some("gzip"),
                OtlpDecodeError::GzipDecodeFailed,
            ),
        ];
        for (body, content_type, content_encoding, expected) in cases {
            let error = decode_otlp_request(
                body,
                content_type,
                content_encoding,
                OtlpRequestLimits::default(),
            )
            .await
            .unwrap_err();
            assert_eq!(error, expected);
            assert!(!format!("{error:?}").contains("synthetic-"));
        }
    }

    #[tokio::test]
    async fn unknown_protobuf_wire_fields_are_discarded_before_extraction() {
        let request: ExportLogsServiceRequest = serde_json::from_str(FIXTURE).unwrap();
        let mut wire = request.encode_to_vec();
        let marker = b"synthetic-forbidden-unknown-wire.example.invalid";
        push_varint((50_000_u64 << 3) | 2, &mut wire);
        push_varint(u64::try_from(marker.len()).unwrap(), &mut wire);
        wire.extend_from_slice(marker);

        let decoded = decode_otlp_request(
            wire,
            Some("application/x-protobuf"),
            None,
            OtlpRequestLimits::default(),
        )
        .await
        .unwrap();
        assert_eq!(decoded.request, request);
        assert!(!format!("{:?}", decoded.request).contains("synthetic-forbidden"));
    }

    #[tokio::test]
    async fn unsupported_protocol_headers_are_permanent_bad_data() {
        for (content_type, content_encoding) in [
            (None, None),
            (Some("text/plain"), None),
            (Some("application/x-protobuf; charset=utf-8"), None),
            (Some("application/json"), Some("br")),
            (Some("application/json"), Some("gzip, identity")),
        ] {
            let error = decode_otlp_request(
                Vec::new(),
                content_type,
                content_encoding,
                OtlpRequestLimits::default(),
            )
            .await
            .unwrap_err();
            assert!(!error.retryable());
            assert_eq!(error.safe_code(), SafeErrorCode::OtlpInvalidContentType);
        }
    }

    #[test]
    fn rejects_invalid_limits() {
        assert_eq!(
            OtlpRequestLimits::new(0, 1, Duration::from_secs(1)),
            Err(OtlpLimitError::ZeroCompressedLimit)
        );
        assert_eq!(
            OtlpRequestLimits::new(1, 0, Duration::from_secs(1)),
            Err(OtlpLimitError::ZeroDecompressedLimit)
        );
        assert_eq!(
            OtlpRequestLimits::new(1, 1, Duration::ZERO),
            Err(OtlpLimitError::ZeroRequestTimeout)
        );
        assert_eq!(
            OtlpRequestLimits::new(
                1,
                1,
                MAX_OTLP_REQUEST_TIMEOUT.saturating_add(Duration::from_nanos(1)),
            ),
            Err(OtlpLimitError::RequestTimeoutTooLarge)
        );
    }

    fn decode_hex(value: &str) -> Vec<u8> {
        let value = value.trim();
        assert_eq!(value.len() % 2, 0);
        value
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let pair = std::str::from_utf8(pair).unwrap();
                u8::from_str_radix(pair, 16).unwrap()
            })
            .collect()
    }

    fn push_varint(mut value: u64, target: &mut Vec<u8>) {
        while value >= 0x80 {
            target.push(u8::try_from(value & 0x7f).unwrap() | 0x80);
            value >>= 7;
        }
        target.push(u8::try_from(value).unwrap());
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn bounded_decoder_fuzzes_arbitrary_wire_bytes_without_panicking(
            body in prop::collection::vec(any::<u8>(), 0..4096),
            protobuf in any::<bool>(),
            compressed in any::<bool>(),
        ) {
            let encoding = if protobuf {
                OtlpEncoding::Protobuf
            } else {
                OtlpEncoding::Json
            };
            let content_encoding = if compressed {
                ContentEncoding::Gzip
            } else {
                ContentEncoding::Identity
            };
            let result = decode_bounded(body, encoding, content_encoding, limits(4096, 4096));
            if let Err(error) = result {
                prop_assert!(matches!(
                    error,
                    OtlpDecodeError::DecompressedBodyTooLarge
                        | OtlpDecodeError::GzipDecodeFailed
                        | OtlpDecodeError::JsonDecodeFailed
                        | OtlpDecodeError::ProtobufDecodeFailed
                ));
            }
        }
    }
}
