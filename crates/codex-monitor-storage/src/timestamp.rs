use codex_monitor_core::UtcTimestamp;
use time::macros::format_description;

const STORAGE_TIMESTAMP_FORMAT: &[time::format_description::FormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:9]Z");

/// Render a UTC timestamp with fixed-width nanoseconds so `SQLite` text ordering
/// is chronological.
pub(crate) fn timestamp_text(value: UtcTimestamp) -> String {
    value
        .as_offset_datetime()
        .format(STORAGE_TIMESTAMP_FORMAT)
        .expect("validated UTC timestamps fit the fixed storage format")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_width_text_sorts_chronologically_inside_one_second() {
        let first = UtcTimestamp::parse_rfc3339("2026-01-01T00:00:00Z").unwrap();
        let second = UtcTimestamp::parse_rfc3339("2026-01-01T00:00:00.5Z").unwrap();

        assert_eq!(timestamp_text(first), "2026-01-01T00:00:00.000000000Z");
        assert_eq!(timestamp_text(second), "2026-01-01T00:00:00.500000000Z");
        assert!(timestamp_text(first) < timestamp_text(second));
    }
}
