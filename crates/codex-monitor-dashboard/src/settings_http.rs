use std::num::NonZeroU32;

use axum::Json;
use axum::extract::State;
use axum::http::header::{ACCEPT_LANGUAGE, CACHE_CONTROL, ETAG, IF_MATCH};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use codex_monitor_core::UtcTimestamp;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::api_error::{
    ApiErrorCode, ValidationField, ValidationIssue, api_error, storage_unavailable,
    validation_error,
};
use super::request::{DashboardRequestBody, ValidatedJson, sealed};
use super::session::AuthorizedStateChange;
use super::state::DashboardState;
use crate::ports::{
    DashboardLocale, DashboardRetentionPolicy, DashboardSettings, DashboardSettingsDocument,
    DashboardSettingsErrorCode,
};

const MAX_NOTIFY_MAX_N: u32 = 1_000;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SettingsRequestBody {
    #[serde(default)]
    login_autostart: BooleanInput,
    #[serde(default)]
    system_notifications: BooleanInput,
    #[serde(default)]
    notify_max_n: NullableIntegerInput,
    #[serde(default)]
    retention_days: NullableIntegerInput,
    #[serde(default)]
    language: LocaleInput,
}

#[derive(Clone, Copy, Default)]
enum BooleanInput {
    #[default]
    Missing,
    Value(bool),
    Invalid,
}

#[derive(Clone, Copy, Default)]
enum NullableIntegerInput {
    #[default]
    Missing,
    Null,
    Integer(i64),
    OutOfRange,
    Invalid,
}

#[derive(Clone, Copy, Default)]
enum LocaleInput {
    #[default]
    Missing,
    Value(DashboardLocale),
    Unsupported,
    Invalid,
}

#[derive(Serialize)]
pub(super) struct SettingsDocumentResponse {
    revision: String,
    settings: SettingsResponse,
    applied_at: UtcTimestamp,
}

#[derive(Serialize)]
struct SettingsResponse {
    login_autostart: bool,
    system_notifications: bool,
    notify_max_n: Option<u32>,
    retention_days: Option<i64>,
    language: &'static str,
}

impl sealed::Sealed for SettingsRequestBody {}

impl DashboardRequestBody for SettingsRequestBody {
    fn validate(&self) -> Result<(), Vec<ValidationIssue>> {
        let mut issues = Vec::new();
        validate_boolean(
            self.login_autostart,
            ValidationField::LoginAutostart,
            &mut issues,
        );
        validate_boolean(
            self.system_notifications,
            ValidationField::SystemNotifications,
            &mut issues,
        );
        match self.notify_max_n {
            NullableIntegerInput::Missing => {
                issues.push(ValidationIssue::required(ValidationField::NotifyMaxN));
            }
            NullableIntegerInput::Null => {}
            NullableIntegerInput::Integer(value)
                if (1..=i64::from(MAX_NOTIFY_MAX_N)).contains(&value) => {}
            NullableIntegerInput::Integer(_) | NullableIntegerInput::OutOfRange => {
                issues.push(ValidationIssue::out_of_range(
                    ValidationField::NotifyMaxN,
                    Some(1),
                    Some(i64::from(MAX_NOTIFY_MAX_N)),
                ));
            }
            NullableIntegerInput::Invalid => {
                issues.push(ValidationIssue::invalid_format(ValidationField::NotifyMaxN));
            }
        }
        match self.retention_days {
            NullableIntegerInput::Missing => {
                issues.push(ValidationIssue::required(ValidationField::RetentionDays));
            }
            NullableIntegerInput::Null | NullableIntegerInput::Integer(30 | 90 | 365) => {}
            NullableIntegerInput::Integer(_) | NullableIntegerInput::OutOfRange => {
                issues.push(ValidationIssue::unsupported(ValidationField::RetentionDays));
            }
            NullableIntegerInput::Invalid => {
                issues.push(ValidationIssue::invalid_format(
                    ValidationField::RetentionDays,
                ));
            }
        }
        match self.language {
            LocaleInput::Missing => {
                issues.push(ValidationIssue::required(ValidationField::Language));
            }
            LocaleInput::Value(_) => {}
            LocaleInput::Unsupported => {
                issues.push(ValidationIssue::unsupported(ValidationField::Language));
            }
            LocaleInput::Invalid => {
                issues.push(ValidationIssue::invalid_format(ValidationField::Language));
            }
        }

        if issues.is_empty() {
            Ok(())
        } else {
            Err(issues)
        }
    }
}

impl SettingsRequestBody {
    fn into_settings(self) -> Option<DashboardSettings> {
        let BooleanInput::Value(login_autostart) = self.login_autostart else {
            return None;
        };
        let BooleanInput::Value(notifications_enabled) = self.system_notifications else {
            return None;
        };
        let (notify_max_n, notify_all) = match self.notify_max_n {
            NullableIntegerInput::Null => (None, true),
            NullableIntegerInput::Integer(value) => {
                let value = u32::try_from(value).ok().and_then(NonZeroU32::new)?;
                (Some(value), false)
            }
            NullableIntegerInput::Missing
            | NullableIntegerInput::OutOfRange
            | NullableIntegerInput::Invalid => return None,
        };
        let retention = match self.retention_days {
            NullableIntegerInput::Null => DashboardRetentionPolicy::Unlimited,
            NullableIntegerInput::Integer(30) => DashboardRetentionPolicy::Days30,
            NullableIntegerInput::Integer(90) => DashboardRetentionPolicy::Days90,
            NullableIntegerInput::Integer(365) => DashboardRetentionPolicy::Days365,
            NullableIntegerInput::Integer(_)
            | NullableIntegerInput::Missing
            | NullableIntegerInput::OutOfRange
            | NullableIntegerInput::Invalid => return None,
        };
        let LocaleInput::Value(locale) = self.language else {
            return None;
        };
        Some(DashboardSettings {
            retention,
            notifications_enabled,
            notify_max_n,
            notify_all,
            locale,
            login_autostart,
            updated_at: UtcTimestamp::now(),
        })
    }
}

impl<'de> Deserialize<'de> for BooleanInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match Value::deserialize(deserializer)? {
            Value::Bool(value) => Self::Value(value),
            _ => Self::Invalid,
        })
    }
}

impl<'de> Deserialize<'de> for NullableIntegerInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match Value::deserialize(deserializer)? {
            Value::Null => Self::Null,
            Value::Number(value) => value.as_i64().map_or_else(
                || {
                    if value.as_u64().is_some() {
                        Self::OutOfRange
                    } else {
                        Self::Invalid
                    }
                },
                Self::Integer,
            ),
            _ => Self::Invalid,
        })
    }
}

impl<'de> Deserialize<'de> for LocaleInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match Value::deserialize(deserializer)? {
            Value::String(value) if value == DashboardLocale::EnUs.as_str() => {
                Self::Value(DashboardLocale::EnUs)
            }
            Value::String(value) if value == DashboardLocale::ZhCn.as_str() => {
                Self::Value(DashboardLocale::ZhCn)
            }
            Value::String(_) => Self::Unsupported,
            _ => Self::Invalid,
        })
    }
}

pub(super) async fn get_settings(
    State(state): State<DashboardState>,
    headers: HeaderMap,
) -> Response {
    match state
        .settings
        .initialize_locale(preferred_locale(&headers))
        .await
    {
        Ok(document) => settings_response(document),
        Err(error) => runtime_error(error),
    }
}

fn preferred_locale(headers: &HeaderMap) -> DashboardLocale {
    let mut best: Option<(u16, DashboardLocale)> = None;
    for header in headers.get_all(ACCEPT_LANGUAGE) {
        let Ok(header) = header.to_str() else {
            continue;
        };
        for range in header.split(',').take(32) {
            let mut segments = range.split(';');
            let language_tag = segments.next().unwrap_or_default().trim();
            let Some(locale) = supported_locale(language_tag) else {
                continue;
            };
            let Some(quality) = language_quality(segments) else {
                continue;
            };
            if quality == 0 {
                continue;
            }
            if best.is_none_or(|(best_quality, _)| quality > best_quality) {
                best = Some((quality, locale));
            }
        }
    }
    best.map_or(DashboardLocale::EnUs, |(_, locale)| locale)
}

fn supported_locale(language_tag: &str) -> Option<DashboardLocale> {
    if language_tag.trim() == "*" {
        return Some(DashboardLocale::EnUs);
    }
    DashboardLocale::from_language_tag(language_tag)
}

fn language_quality<'a>(segments: impl Iterator<Item = &'a str>) -> Option<u16> {
    for segment in segments {
        let Some((name, value)) = segment.trim().split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("q") {
            return parse_quality(value.trim());
        }
    }
    Some(1_000)
}

fn parse_quality(value: &str) -> Option<u16> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if fraction.len() > 3 || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    match whole {
        "1" if fraction.bytes().all(|byte| byte == b'0') => Some(1_000),
        "0" => {
            let mut quality = fraction.parse::<u16>().ok().unwrap_or(0);
            for _ in fraction.len()..3 {
                quality *= 10;
            }
            Some(quality)
        }
        _ => None,
    }
}

pub(super) async fn replace_settings(
    State(state): State<DashboardState>,
    _authorization: AuthorizedStateChange,
    headers: HeaderMap,
    body: ValidatedJson<SettingsRequestBody>,
) -> Response {
    let expected_revision = match parse_if_match(&headers) {
        Ok(Some(revision)) => revision,
        Ok(None) => {
            return api_error(
                StatusCode::PRECONDITION_FAILED,
                ApiErrorCode::RevisionConflict,
                None,
            );
        }
        Err(IfMatchError::Missing) => {
            return api_error(
                StatusCode::PRECONDITION_REQUIRED,
                ApiErrorCode::PreconditionRequired,
                None,
            );
        }
        Err(IfMatchError::Invalid) => return invalid_if_match(),
    };
    let Some(settings) = body.into_inner().into_settings() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            ApiErrorCode::InternalUnavailable,
            None,
        );
    };

    match state.settings.replace(expected_revision, settings).await {
        Ok(document) => settings_response(document),
        Err(error) => runtime_error(error),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IfMatchError {
    Missing,
    Invalid,
}

fn parse_if_match(headers: &HeaderMap) -> Result<Option<u64>, IfMatchError> {
    let mut values = headers.get_all(IF_MATCH).iter();
    let Some(value) = values.next() else {
        return Err(IfMatchError::Missing);
    };
    if values.next().is_some() {
        return Err(IfMatchError::Invalid);
    }
    let bytes = value.as_bytes();
    if !(3..=130).contains(&bytes.len())
        || bytes.first() != Some(&b'"')
        || bytes.last() != Some(&b'"')
    {
        return Err(IfMatchError::Invalid);
    }
    let opaque = &bytes[1..bytes.len() - 1];
    if opaque.is_empty()
        || opaque.len() > 128
        || !opaque
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(IfMatchError::Invalid);
    }
    let Ok(opaque) = std::str::from_utf8(opaque) else {
        return Err(IfMatchError::Invalid);
    };
    let Ok(revision) = opaque.parse::<u64>() else {
        return Ok(None);
    };
    if revision.to_string() == opaque {
        Ok(Some(revision))
    } else {
        Ok(None)
    }
}

fn invalid_if_match() -> Response {
    validation_error(ValidationIssue::invalid_format(ValidationField::IfMatch))
}

fn settings_response(document: DashboardSettingsDocument) -> Response {
    let revision = document.revision.to_string();
    let etag = HeaderValue::from_str(&format!("\"{revision}\""))
        .expect("a decimal settings revision is always a valid entity tag");
    let mut response = (StatusCode::OK, Json(settings_document(document))).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(ETAG, etag);
    response
}

pub(super) fn settings_document(document: DashboardSettingsDocument) -> SettingsDocumentResponse {
    let settings = document.settings;
    SettingsDocumentResponse {
        revision: document.revision.to_string(),
        settings: SettingsResponse {
            login_autostart: settings.login_autostart,
            system_notifications: settings.notifications_enabled,
            notify_max_n: settings.notify_max_n.map(NonZeroU32::get),
            retention_days: settings.retention.days(),
            language: settings.locale.as_str(),
        },
        applied_at: settings.updated_at,
    }
}

fn runtime_error(error: DashboardSettingsErrorCode) -> Response {
    match error {
        DashboardSettingsErrorCode::RevisionConflict => api_error(
            StatusCode::PRECONDITION_FAILED,
            ApiErrorCode::RevisionConflict,
            None,
        ),
        DashboardSettingsErrorCode::StorageUnavailable => storage_unavailable(),
        DashboardSettingsErrorCode::InvalidSettings
        | DashboardSettingsErrorCode::PlatformUnavailable
        | DashboardSettingsErrorCode::RuntimeUnavailable => api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            ApiErrorCode::InternalUnavailable,
            None,
        ),
    }
}

fn validate_boolean(
    value: BooleanInput,
    field: ValidationField,
    issues: &mut Vec<ValidationIssue>,
) {
    match value {
        BooleanInput::Missing => issues.push(ValidationIssue::required(field)),
        BooleanInput::Value(_) => {}
        BooleanInput::Invalid => issues.push(ValidationIssue::invalid_format(field)),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::header::ACCEPT_LANGUAGE;

    use super::*;

    #[test]
    fn quality_parser_accepts_only_http_language_weight_shape() {
        assert_eq!(parse_quality("1"), Some(1_000));
        assert_eq!(parse_quality("0.75"), Some(750));
        assert_eq!(parse_quality("0.001"), Some(1));
        assert_eq!(parse_quality("1.1"), None);
        assert_eq!(parse_quality("0.0001"), None);
        assert_eq!(parse_quality("-1"), None);
    }

    #[test]
    fn preferred_locale_uses_highest_supported_weight() {
        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT_LANGUAGE,
            HeaderValue::from_static("en-US;q=0.4, zh-CN;q=0.9"),
        );
        assert_eq!(preferred_locale(&headers), DashboardLocale::ZhCn);
    }

    #[test]
    fn validated_payload_maps_to_closed_settings_dto() {
        let body: SettingsRequestBody = serde_json::from_str(
            r#"{
                "login_autostart": true,
                "system_notifications": false,
                "notify_max_n": 3,
                "retention_days": 90,
                "language": "zh-CN"
            }"#,
        )
        .unwrap();
        assert!(body.validate().is_ok());
        let settings = body.into_settings().unwrap();
        assert_eq!(settings.retention, DashboardRetentionPolicy::Days90);
        assert_eq!(settings.notify_max_n.map(NonZeroU32::get), Some(3));
        assert_eq!(settings.locale, DashboardLocale::ZhCn);
        assert!(settings.login_autostart);
        assert!(!settings.notifications_enabled);
    }
}
