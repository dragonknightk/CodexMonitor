use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

use crate::ValidationError;

/// A domain timestamp normalized to the UTC offset.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UtcTimestamp(OffsetDateTime);

impl UtcTimestamp {
    /// Normalize an instant to UTC.
    #[must_use]
    pub fn new(value: OffsetDateTime) -> Self {
        Self(value.to_offset(UtcOffset::UTC))
    }

    /// Read the current UTC instant.
    #[must_use]
    pub fn now() -> Self {
        Self(OffsetDateTime::now_utc())
    }

    /// Construct a UTC timestamp from Unix nanoseconds.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidTimestamp`] when the instant is outside
    /// the range supported by the pinned `time` crate.
    pub fn from_unix_timestamp_nanos(value: i128) -> Result<Self, ValidationError> {
        OffsetDateTime::from_unix_timestamp_nanos(value)
            .map(Self::new)
            .map_err(|_| ValidationError::InvalidTimestamp)
    }

    /// Parse an RFC 3339 timestamp and normalize it to UTC.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidTimestamp`] for malformed or
    /// out-of-range input.
    pub fn parse_rfc3339(value: &str) -> Result<Self, ValidationError> {
        OffsetDateTime::parse(value, &Rfc3339)
            .map(Self::new)
            .map_err(|_| ValidationError::InvalidTimestamp)
    }

    /// Return Unix nanoseconds for stable ordering and fingerprints.
    #[must_use]
    pub fn unix_timestamp_nanos(self) -> i128 {
        self.0.unix_timestamp_nanos()
    }

    /// Return the normalized `time` value.
    #[must_use]
    pub const fn as_offset_datetime(self) -> OffsetDateTime {
        self.0
    }

    fn format_rfc3339(self) -> Result<String, time::error::Format> {
        self.0.format(&Rfc3339)
    }
}

impl fmt::Display for UtcTimestamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.format_rfc3339().map_err(|_| fmt::Error)?)
    }
}

impl Serialize for UtcTimestamp {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.format_rfc3339().map_err(serde::ser::Error::custom)?)
    }
}

impl<'de> Deserialize<'de> for UtcTimestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse_rfc3339(&value).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    #[test]
    fn normalizes_non_utc_offsets() {
        let value = UtcTimestamp::new(datetime!(2026-07-14 20:00 +08:00));

        assert_eq!(value.to_string(), "2026-07-14T12:00:00Z");
        assert_eq!(value.as_offset_datetime().offset(), UtcOffset::UTC);
    }

    #[test]
    fn serde_uses_rfc3339_strings() {
        let value = UtcTimestamp::parse_rfc3339("2026-01-01T00:00:00.125Z").unwrap();
        let serialized = serde_json::to_string(&value).unwrap();

        assert_eq!(serialized, "\"2026-01-01T00:00:00.125Z\"");
        assert_eq!(
            serde_json::from_str::<UtcTimestamp>(&serialized).unwrap(),
            value
        );
    }
}
