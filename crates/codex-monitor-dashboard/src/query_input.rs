use codex_monitor_core::{ActionLifecycleState, DataOrigin, SourceId, UtcTimestamp};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use super::api_error::{ValidationField, ValidationIssue};
use super::cursor::{CursorSort, NormalizedFilterDigest};

pub(crate) const DEFAULT_PAGE_LIMIT: usize = 25;
pub(crate) const MAX_PAGE_LIMIT: usize = 100;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawModelCallQuery {
    cursor: Option<String>,
    limit: Option<String>,
    conversation: Option<String>,
    model: Option<String>,
    source_id: Option<String>,
    client: Option<String>,
    from: Option<String>,
    to: Option<String>,
    alert: Option<String>,
    temporal_class: Option<String>,
    sort: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawSummaryQuery {
    conversation: Option<String>,
    model: Option<String>,
    source_id: Option<String>,
    client: Option<String>,
    from: Option<String>,
    to: Option<String>,
    alert: Option<String>,
    temporal_class: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawConversationQuery {
    cursor: Option<String>,
    limit: Option<String>,
    query: Option<String>,
    source_id: Option<String>,
    from: Option<String>,
    to: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawAlertQuery {
    cursor: Option<String>,
    limit: Option<String>,
    conversation: Option<String>,
    model_call_id: Option<String>,
    action_state: Option<String>,
    from: Option<String>,
    to: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AlertMatchInput {
    #[default]
    Any,
    Matched,
    None,
}

impl AlertMatchInput {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Matched => "matched",
            Self::None => "none",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ModelCallSortInput {
    #[default]
    EventAtDesc,
    EventAtAsc,
    InputTokensDesc,
    OutputTokensDesc,
    ReasoningTokensDesc,
    TotalTokensDesc,
}

impl ModelCallSortInput {
    pub(super) const fn cursor_sort(self) -> CursorSort {
        match self {
            Self::EventAtDesc => CursorSort::EventAtDesc,
            Self::EventAtAsc => CursorSort::EventAtAsc,
            Self::InputTokensDesc => CursorSort::InputTokensDesc,
            Self::OutputTokensDesc => CursorSort::OutputTokensDesc,
            Self::ReasoningTokensDesc => CursorSort::ReasoningTokensDesc,
            Self::TotalTokensDesc => CursorSort::TotalTokensDesc,
        }
    }

    #[cfg(test)]
    const fn as_str(self) -> &'static str {
        match self {
            Self::EventAtDesc => "event_at_desc",
            Self::EventAtAsc => "event_at_asc",
            Self::InputTokensDesc => "input_tokens_desc",
            Self::OutputTokensDesc => "output_tokens_desc",
            Self::ReasoningTokensDesc => "reasoning_tokens_desc",
            Self::TotalTokensDesc => "total_tokens_desc",
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModelFiltersInput {
    pub conversation: Option<String>,
    pub model: Option<String>,
    pub source_id: Option<SourceId>,
    pub client: Option<String>,
    pub from: Option<UtcTimestamp>,
    pub to: Option<UtcTimestamp>,
    pub alert: AlertMatchInput,
    pub temporal_class: Option<DataOrigin>,
}

impl ModelFiltersInput {
    pub(super) fn digest(
        &self,
        resolved_conversation_identifiers: &[String],
    ) -> NormalizedFilterDigest {
        let mut digest = FilterDigest::new("model_call_filters_v1");
        digest.optional("conversation", self.conversation.as_deref());
        digest.sequence(
            "resolved_conversation_identifiers",
            resolved_conversation_identifiers,
        );
        digest.optional("model", self.model.as_deref());
        digest.optional("source_id", self.source_id.as_ref().map(SourceId::as_str));
        digest.optional("client", self.client.as_deref());
        digest.optional_timestamp("from", self.from);
        digest.optional_timestamp("to", self.to);
        digest.value("alert", self.alert.as_str());
        digest.optional(
            "temporal_class",
            self.temporal_class.map(temporal_class_name),
        );
        digest.finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelCallQueryInput {
    pub cursor: Option<String>,
    pub limit: usize,
    pub filters: ModelFiltersInput,
    pub sort: ModelCallSortInput,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversationQueryInput {
    pub cursor: Option<String>,
    pub limit: usize,
    pub query: Option<String>,
    pub source_id: Option<SourceId>,
    pub from: Option<UtcTimestamp>,
    pub to: Option<UtcTimestamp>,
}

impl ConversationQueryInput {
    pub(super) fn digest(&self, resolved_identifiers: &[String]) -> NormalizedFilterDigest {
        let mut digest = FilterDigest::new("conversation_filters_v1");
        digest.optional("query", self.query.as_deref());
        digest.sequence("resolved_identifiers", resolved_identifiers);
        digest.optional("source_id", self.source_id.as_ref().map(SourceId::as_str));
        digest.optional_timestamp("from", self.from);
        digest.optional_timestamp("to", self.to);
        digest.finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlertQueryInput {
    pub cursor: Option<String>,
    pub limit: usize,
    pub conversation: Option<String>,
    pub model_call_id: Option<String>,
    pub action_state: Option<ActionLifecycleState>,
    pub from: Option<UtcTimestamp>,
    pub to: Option<UtcTimestamp>,
}

impl AlertQueryInput {
    pub(super) fn digest(
        &self,
        resolved_conversation_identifiers: &[String],
    ) -> NormalizedFilterDigest {
        let mut digest = FilterDigest::new("alert_filters_v1");
        digest.optional("conversation", self.conversation.as_deref());
        digest.sequence(
            "resolved_conversation_identifiers",
            resolved_conversation_identifiers,
        );
        digest.optional("model_call_id", self.model_call_id.as_deref());
        digest.optional(
            "action_state",
            self.action_state.map(ActionLifecycleState::as_str),
        );
        digest.optional_timestamp("from", self.from);
        digest.optional_timestamp("to", self.to);
        digest.finish()
    }
}

#[derive(Debug)]
pub(super) enum QueryInputError {
    Validation(ValidationIssue),
    InvalidTimeRange,
}

pub(super) fn parse_model_call_query(
    raw: RawModelCallQuery,
) -> Result<ModelCallQueryInput, QueryInputError> {
    let filters = parse_model_filters(
        raw.conversation,
        raw.model,
        raw.source_id,
        raw.client,
        raw.from,
        raw.to,
        raw.alert.as_deref(),
        raw.temporal_class.as_deref(),
    )?;
    Ok(ModelCallQueryInput {
        cursor: raw.cursor,
        limit: parse_limit(raw.limit)?,
        filters,
        sort: parse_model_sort(raw.sort.as_deref())?,
    })
}

pub(super) fn parse_summary_query(
    raw: RawSummaryQuery,
) -> Result<ModelFiltersInput, QueryInputError> {
    parse_model_filters(
        raw.conversation,
        raw.model,
        raw.source_id,
        raw.client,
        raw.from,
        raw.to,
        raw.alert.as_deref(),
        raw.temporal_class.as_deref(),
    )
}

pub(super) fn parse_conversation_query(
    raw: RawConversationQuery,
) -> Result<ConversationQueryInput, QueryInputError> {
    let from = parse_timestamp(raw.from, ValidationField::From)?;
    let to = parse_timestamp(raw.to, ValidationField::To)?;
    validate_time_range(from, to)?;
    Ok(ConversationQueryInput {
        cursor: raw.cursor,
        limit: parse_limit(raw.limit)?,
        query: bounded_string(raw.query, ValidationField::Query, 256)?,
        source_id: parse_source_id(raw.source_id)?,
        from,
        to,
    })
}

pub(super) fn parse_alert_query(raw: RawAlertQuery) -> Result<AlertQueryInput, QueryInputError> {
    let from = parse_timestamp(raw.from, ValidationField::From)?;
    let to = parse_timestamp(raw.to, ValidationField::To)?;
    validate_time_range(from, to)?;
    Ok(AlertQueryInput {
        cursor: raw.cursor,
        limit: parse_limit(raw.limit)?,
        conversation: bounded_string(raw.conversation, ValidationField::Conversation, 256)?,
        model_call_id: parse_opaque_id(raw.model_call_id, ValidationField::ModelCallId)?,
        action_state: parse_action_state(raw.action_state.as_deref())?,
        from,
        to,
    })
}

#[allow(clippy::too_many_arguments)]
fn parse_model_filters(
    conversation: Option<String>,
    model: Option<String>,
    source_id: Option<String>,
    client: Option<String>,
    from: Option<String>,
    to: Option<String>,
    alert: Option<&str>,
    temporal_class: Option<&str>,
) -> Result<ModelFiltersInput, QueryInputError> {
    let from = parse_timestamp(from, ValidationField::From)?;
    let to = parse_timestamp(to, ValidationField::To)?;
    validate_time_range(from, to)?;
    Ok(ModelFiltersInput {
        conversation: bounded_string(conversation, ValidationField::Conversation, 256)?,
        model: bounded_string(model, ValidationField::Model, 128)?,
        source_id: parse_source_id(source_id)?,
        client: bounded_string(client, ValidationField::Client, 128)?,
        from,
        to,
        alert: parse_alert_match(alert)?,
        temporal_class: parse_temporal_class(temporal_class)?,
    })
}

fn parse_limit(raw: Option<String>) -> Result<usize, QueryInputError> {
    let Some(raw) = raw else {
        return Ok(DEFAULT_PAGE_LIMIT);
    };
    let parsed = raw.parse::<usize>().map_err(|_| {
        if !raw.is_empty() && raw.bytes().all(|byte| byte.is_ascii_digit()) {
            QueryInputError::Validation(ValidationIssue::out_of_range(
                ValidationField::Limit,
                Some(1),
                Some(i64::try_from(MAX_PAGE_LIMIT).expect("page limit fits i64")),
            ))
        } else {
            QueryInputError::Validation(ValidationIssue::invalid_format(ValidationField::Limit))
        }
    })?;
    if !(1..=MAX_PAGE_LIMIT).contains(&parsed) {
        return Err(QueryInputError::Validation(ValidationIssue::out_of_range(
            ValidationField::Limit,
            Some(1),
            Some(i64::try_from(MAX_PAGE_LIMIT).expect("page limit fits i64")),
        )));
    }
    Ok(parsed)
}

fn bounded_string(
    raw: Option<String>,
    field: ValidationField,
    max_characters: usize,
) -> Result<Option<String>, QueryInputError> {
    let Some(value) = raw else {
        return Ok(None);
    };
    if value.is_empty() {
        return Err(QueryInputError::Validation(
            ValidationIssue::invalid_format(field),
        ));
    }
    if value.chars().count() > max_characters {
        return Err(QueryInputError::Validation(ValidationIssue::out_of_range(
            field,
            Some(1),
            Some(i64::try_from(max_characters).expect("query bounds fit i64")),
        )));
    }
    Ok(Some(value))
}

fn parse_source_id(raw: Option<String>) -> Result<Option<SourceId>, QueryInputError> {
    let Some(value) = raw else {
        return Ok(None);
    };
    parse_required_source_id(value).map(Some)
}

pub(super) fn parse_required_source_id(value: String) -> Result<SourceId, QueryInputError> {
    let value = bounded_string(Some(value), ValidationField::SourceId, 64)?
        .expect("required source ID remains present");
    if !is_openapi_source_id(&value) {
        return Err(QueryInputError::Validation(
            ValidationIssue::invalid_format(ValidationField::SourceId),
        ));
    }
    SourceId::new(value).map_err(|_| {
        QueryInputError::Validation(ValidationIssue::invalid_format(ValidationField::SourceId))
    })
}

fn is_openapi_source_id(value: &str) -> bool {
    let mut segments = value.split(['.', '_', '-']);
    segments.by_ref().all(|segment| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    })
}

fn parse_opaque_id(
    raw: Option<String>,
    field: ValidationField,
) -> Result<Option<String>, QueryInputError> {
    let Some(value) = bounded_string(raw, field, 128)? else {
        return Ok(None);
    };
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(QueryInputError::Validation(
            ValidationIssue::invalid_format(field),
        ));
    }
    Ok(Some(value))
}

fn parse_timestamp(
    raw: Option<String>,
    field: ValidationField,
) -> Result<Option<UtcTimestamp>, QueryInputError> {
    raw.map(|value| {
        UtcTimestamp::parse_rfc3339(&value)
            .map_err(|_| QueryInputError::Validation(ValidationIssue::invalid_format(field)))
    })
    .transpose()
}

fn validate_time_range(
    from: Option<UtcTimestamp>,
    to: Option<UtcTimestamp>,
) -> Result<(), QueryInputError> {
    if from.zip(to).is_some_and(|(from, to)| from >= to) {
        return Err(QueryInputError::InvalidTimeRange);
    }
    Ok(())
}

fn parse_alert_match(raw: Option<&str>) -> Result<AlertMatchInput, QueryInputError> {
    match raw {
        None | Some("any") => Ok(AlertMatchInput::Any),
        Some("matched") => Ok(AlertMatchInput::Matched),
        Some("none") => Ok(AlertMatchInput::None),
        Some(_) => Err(QueryInputError::Validation(ValidationIssue::unsupported(
            ValidationField::Alert,
        ))),
    }
}

fn parse_temporal_class(raw: Option<&str>) -> Result<Option<DataOrigin>, QueryInputError> {
    match raw {
        None => Ok(None),
        Some("realtime") => Ok(Some(DataOrigin::Realtime)),
        Some("historical") => Ok(Some(DataOrigin::Historical)),
        Some(_) => Err(QueryInputError::Validation(ValidationIssue::unsupported(
            ValidationField::TemporalClass,
        ))),
    }
}

fn parse_model_sort(raw: Option<&str>) -> Result<ModelCallSortInput, QueryInputError> {
    match raw {
        None | Some("event_at_desc") => Ok(ModelCallSortInput::EventAtDesc),
        Some("event_at_asc") => Ok(ModelCallSortInput::EventAtAsc),
        Some("input_tokens_desc") => Ok(ModelCallSortInput::InputTokensDesc),
        Some("output_tokens_desc") => Ok(ModelCallSortInput::OutputTokensDesc),
        Some("reasoning_tokens_desc") => Ok(ModelCallSortInput::ReasoningTokensDesc),
        Some("total_tokens_desc") => Ok(ModelCallSortInput::TotalTokensDesc),
        Some(_) => Err(QueryInputError::Validation(ValidationIssue::unsupported(
            ValidationField::Sort,
        ))),
    }
}

fn parse_action_state(raw: Option<&str>) -> Result<Option<ActionLifecycleState>, QueryInputError> {
    match raw {
        None => Ok(None),
        Some("pending") => Ok(Some(ActionLifecycleState::Pending)),
        Some("started") => Ok(Some(ActionLifecycleState::Started)),
        Some("completed") => Ok(Some(ActionLifecycleState::Completed)),
        Some("failed") => Ok(Some(ActionLifecycleState::Failed)),
        Some("suppressed") => Ok(Some(ActionLifecycleState::Suppressed)),
        Some("unavailable") => Ok(Some(ActionLifecycleState::Unavailable)),
        Some(_) => Err(QueryInputError::Validation(ValidationIssue::unsupported(
            ValidationField::ActionState,
        ))),
    }
}

const fn temporal_class_name(value: DataOrigin) -> &'static str {
    match value {
        DataOrigin::Realtime => "realtime",
        DataOrigin::Historical => "historical",
    }
}

struct FilterDigest(Sha256);

impl FilterDigest {
    fn new(domain: &str) -> Self {
        let mut value = Self(Sha256::new());
        value.value("domain", domain);
        value
    }

    fn optional(&mut self, name: &str, value: Option<&str>) {
        self.bytes(name.as_bytes());
        match value {
            Some(value) => {
                self.bytes(&[1]);
                self.bytes(value.as_bytes());
            }
            None => self.bytes(&[0]),
        }
    }

    fn optional_timestamp(&mut self, name: &str, value: Option<UtcTimestamp>) {
        let encoded = value.map(|value| value.to_string());
        self.optional(name, encoded.as_deref());
    }

    fn value(&mut self, name: &str, value: &str) {
        self.bytes(name.as_bytes());
        self.bytes(value.as_bytes());
    }

    fn sequence(&mut self, name: &str, values: &[String]) {
        self.bytes(name.as_bytes());
        self.bytes(
            &u64::try_from(values.len())
                .expect("bounded resolved identifier count fits u64")
                .to_be_bytes(),
        );
        for value in values {
            self.bytes(value.as_bytes());
        }
    }

    fn bytes(&mut self, value: &[u8]) {
        self.0.update(
            u64::try_from(value.len())
                .expect("bounded filter component length fits u64")
                .to_be_bytes(),
        );
        self.0.update(value);
    }

    fn finish(self) -> NormalizedFilterDigest {
        self.0.finalize().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_model_query() -> RawModelCallQuery {
        RawModelCallQuery {
            cursor: None,
            limit: None,
            conversation: None,
            model: None,
            source_id: None,
            client: None,
            from: None,
            to: None,
            alert: None,
            temporal_class: None,
            sort: None,
        }
    }

    #[test]
    fn omitted_and_explicit_defaults_have_the_same_normalized_state() {
        let omitted = parse_model_call_query(empty_model_query()).unwrap();
        let mut explicit = empty_model_query();
        explicit.limit = Some("25".to_owned());
        explicit.alert = Some("any".to_owned());
        explicit.sort = Some("event_at_desc".to_owned());
        let explicit = parse_model_call_query(explicit).unwrap();

        assert_eq!(omitted.limit, explicit.limit);
        assert_eq!(omitted.sort, explicit.sort);
        assert_eq!(omitted.filters.digest(&[]), explicit.filters.digest(&[]));
    }

    #[test]
    fn page_limit_and_cursor_do_not_change_filter_binding() {
        let mut first = empty_model_query();
        first.model = Some("gpt-5.6".to_owned());
        let first = parse_model_call_query(first).unwrap();
        let mut next = empty_model_query();
        next.model = Some("gpt-5.6".to_owned());
        next.limit = Some("100".to_owned());
        next.cursor = Some("opaque".to_owned());
        let next = parse_model_call_query(next).unwrap();

        assert_eq!(first.filters.digest(&[]), next.filters.digest(&[]));
    }

    #[test]
    fn timestamps_normalize_and_equal_or_reversed_ranges_fail_closed() {
        let mut raw = empty_model_query();
        raw.from = Some("2026-07-14T08:00:00+08:00".to_owned());
        raw.to = Some("2026-07-14T00:00:01Z".to_owned());
        let parsed = parse_model_call_query(raw).unwrap();
        assert_eq!(
            parsed.filters.from.unwrap().to_string(),
            "2026-07-14T00:00:00Z"
        );

        for (from, to) in [
            ("2026-07-14T00:00:00Z", "2026-07-14T00:00:00Z"),
            ("2026-07-14T00:00:01Z", "2026-07-14T00:00:00Z"),
        ] {
            let mut raw = empty_model_query();
            raw.from = Some(from.to_owned());
            raw.to = Some(to.to_owned());
            assert!(matches!(
                parse_model_call_query(raw),
                Err(QueryInputError::InvalidTimeRange)
            ));
        }
    }

    #[test]
    fn filters_without_protocol_patterns_are_not_overvalidated() {
        let mut raw = empty_model_query();
        raw.model = Some("not/a/stored/model".to_owned());
        raw.client = Some("not a stored client".to_owned());
        let parsed = parse_model_call_query(raw).unwrap();
        assert_eq!(parsed.filters.model.as_deref(), Some("not/a/stored/model"));
        assert_eq!(
            parsed.filters.client.as_deref(),
            Some("not a stored client")
        );
    }

    #[test]
    fn source_and_opaque_id_patterns_are_enforced_exactly() {
        for source in ["OTLP-logs", "otlp--logs", "otlp+logs"] {
            let result = parse_source_id(Some(source.to_owned()));
            assert!(matches!(result, Err(QueryInputError::Validation(_))));
        }
        assert!(parse_source_id(Some("otlp-logs.v1".to_owned())).is_ok());
        assert!(
            parse_opaque_id(
                Some("synthetic.id:1-2".to_owned()),
                ValidationField::ModelCallId
            )
            .is_ok()
        );
        assert!(matches!(
            parse_opaque_id(
                Some("synthetic/id".to_owned()),
                ValidationField::ModelCallId
            ),
            Err(QueryInputError::Validation(_))
        ));
    }

    #[test]
    fn protocol_string_limits_count_unicode_characters() {
        assert!(bounded_string(Some("会".repeat(256)), ValidationField::Query, 256).is_ok());
        assert!(matches!(
            bounded_string(Some("会".repeat(257)), ValidationField::Query, 256),
            Err(QueryInputError::Validation(_))
        ));
    }

    #[test]
    fn invalid_page_limits_are_closed_validation_errors() {
        for value in ["", "0", "101", "-1", "1.5", "999999999999999999999999"] {
            assert!(matches!(
                parse_limit(Some(value.to_owned())),
                Err(QueryInputError::Validation(_))
            ));
        }
        assert_eq!(parse_limit(Some("1".to_owned())).unwrap(), 1);
        assert_eq!(parse_limit(Some("100".to_owned())).unwrap(), 100);
    }

    #[test]
    fn every_sort_maps_to_the_matching_cursor_order() {
        let cases = [
            (ModelCallSortInput::EventAtDesc, CursorSort::EventAtDesc),
            (ModelCallSortInput::EventAtAsc, CursorSort::EventAtAsc),
            (
                ModelCallSortInput::InputTokensDesc,
                CursorSort::InputTokensDesc,
            ),
            (
                ModelCallSortInput::OutputTokensDesc,
                CursorSort::OutputTokensDesc,
            ),
            (
                ModelCallSortInput::ReasoningTokensDesc,
                CursorSort::ReasoningTokensDesc,
            ),
            (
                ModelCallSortInput::TotalTokensDesc,
                CursorSort::TotalTokensDesc,
            ),
        ];
        for (input, cursor) in cases {
            assert_eq!(input.cursor_sort(), cursor);
            assert!(!input.as_str().is_empty());
        }
    }

    #[test]
    fn resolved_name_candidates_are_part_of_cursor_filter_binding() {
        let mut raw = empty_model_query();
        raw.conversation = Some("mission".to_owned());
        let parsed = parse_model_call_query(raw).unwrap();
        assert_ne!(
            parsed.filters.digest(&["conversation-a".to_owned()]),
            parsed.filters.digest(&["conversation-b".to_owned()])
        );
        assert_ne!(
            parsed.filters.digest(&[]),
            parsed.filters.digest(&["conversation-a".to_owned()])
        );
    }
}
