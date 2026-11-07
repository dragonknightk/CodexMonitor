use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::num::{NonZeroU16, NonZeroU32, NonZeroU64};
use std::str::FromStr as _;

use codex_monitor_core::{
    ActionErrorCode, ActionLifecycleState, ActionSuppressionReason, AlertId, BuiltInRuleId,
    BuiltInRuleRegistry, ClientName, ConversationId, ConversationSourceId, DataOrigin,
    EvidenceQuality, FieldId, MAX_TOKEN_COUNT, ModelCallId, ModelName, PatternN, ReasoningEffort,
    RegisteredActionType, RuleErrorCode, RuleVersion, SafeErrorCode, SourceContractVersion,
    SourceHealthStatus, SourceId, SurfaceName, ThreadSourceId, TokenCount, TurnId, UtcTimestamp,
};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension as _, params, params_from_iter};

use super::codec::{parse_evidence_quality, parse_timestamp};
use super::timestamp::timestamp_text;
use super::types::MAX_NOTIFY_MAX_N;
use super::{StorageError, StorageReader, StorageResult, StoredSourceHealth};

pub const MAX_DASHBOARD_PAGE_SIZE: usize = 100;
pub const MAX_DASHBOARD_SOURCE_ROWS: usize = 32;
pub const MAX_MODEL_CALL_SOURCE_IDS: usize = 32;
pub const MAX_RESOLVED_CONVERSATION_IDENTIFIERS: usize = 50_000;
pub const MAX_SOURCE_CAPABILITY_FIELDS: usize = 128;
pub const MAX_DASHBOARD_ALERT_COUNT: u64 = 1_000_000;
pub const MAX_DASHBOARD_COUNT: u64 = 1_000_000_000_000_000;

/// Read-side state for one active built-in rule exposed by the Dashboard
/// capability document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DashboardRuleCapability {
    pub rule_id: BuiltInRuleId,
    pub rule_version: RuleVersion,
    pub enabled: bool,
    pub notify_max_n: Option<NonZeroU32>,
    pub last_evaluated_at: Option<UtcTimestamp>,
    pub last_error_code: Option<RuleErrorCode>,
}

const QUERY_TOTAL_TOKENS_SQL: &str = "(CASE
    WHEN projection.input_tokens IS NOT NULL
     AND projection.output_tokens IS NOT NULL
     AND projection.input_tokens <= 1000000000000000 - projection.output_tokens
    THEN projection.input_tokens + projection.output_tokens
    ELSE NULL
END)";

/// One raw exact conversation identifier plus stable resolver candidates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversationSelector {
    raw: Option<String>,
    resolved_identifiers: Vec<String>,
}

impl ConversationSelector {
    /// Construct a bounded selector. Resolver candidates are sorted and deduplicated.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty selector, invalid identifier, or candidate overflow.
    pub fn new(raw: Option<String>, mut resolved_identifiers: Vec<String>) -> StorageResult<Self> {
        if raw
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.chars().count() > 256)
        {
            return Err(StorageError::InvalidConversationSelector);
        }
        if resolved_identifiers
            .iter()
            .any(|value| value.len() > 256 || ConversationSourceId::new(value.clone()).is_err())
        {
            return Err(StorageError::InvalidConversationSelector);
        }
        resolved_identifiers.sort_unstable();
        resolved_identifiers.dedup();
        if resolved_identifiers.len() > MAX_RESOLVED_CONVERSATION_IDENTIFIERS {
            return Err(StorageError::TooManyResolvedConversationIdentifiers {
                max: MAX_RESOLVED_CONVERSATION_IDENTIFIERS,
            });
        }
        if raw.is_none() && resolved_identifiers.is_empty() {
            return Err(StorageError::InvalidConversationSelector);
        }
        Ok(Self {
            raw,
            resolved_identifiers,
        })
    }

    #[must_use]
    pub fn raw(&self) -> Option<&str> {
        self.raw.as_deref()
    }

    #[must_use]
    pub fn resolved_identifiers(&self) -> &[String] {
        &self.resolved_identifiers
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AlertMatchFilter {
    #[default]
    Any,
    Matched,
    None,
}

/// Shared AND-composed filters for model-call pages and summaries.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModelCallFilters {
    conversation: Option<ConversationSelector>,
    model: Option<String>,
    source_id: Option<SourceId>,
    client: Option<String>,
    from: Option<UtcTimestamp>,
    to: Option<UtcTimestamp>,
    alert: AlertMatchFilter,
    temporal_class: Option<DataOrigin>,
}

impl ModelCallFilters {
    /// Construct validated AND-composed model-call filters.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid text bounds or a non-increasing time range.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        conversation: Option<ConversationSelector>,
        model: Option<String>,
        source_id: Option<SourceId>,
        client: Option<String>,
        from: Option<UtcTimestamp>,
        to: Option<UtcTimestamp>,
        alert: AlertMatchFilter,
        temporal_class: Option<DataOrigin>,
    ) -> StorageResult<Self> {
        validate_time_range(from, to)?;
        validate_filter_text(model.as_deref(), "model", 128)?;
        validate_filter_text(client.as_deref(), "client", 128)?;
        Ok(Self {
            conversation,
            model,
            source_id,
            client,
            from,
            to,
            alert,
            temporal_class,
        })
    }

    #[must_use]
    pub const fn conversation(&self) -> Option<&ConversationSelector> {
        self.conversation.as_ref()
    }

    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    #[must_use]
    pub const fn source_id(&self) -> Option<&SourceId> {
        self.source_id.as_ref()
    }

    #[must_use]
    pub fn client(&self) -> Option<&str> {
        self.client.as_deref()
    }

    #[must_use]
    pub const fn from(&self) -> Option<UtcTimestamp> {
        self.from
    }

    #[must_use]
    pub const fn to(&self) -> Option<UtcTimestamp> {
        self.to
    }

    #[must_use]
    pub const fn alert(&self) -> AlertMatchFilter {
        self.alert
    }

    #[must_use]
    pub const fn temporal_class(&self) -> Option<DataOrigin> {
        self.temporal_class
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ModelCallSort {
    #[default]
    EventAtDesc,
    EventAtAsc,
    InputTokensDesc,
    OutputTokensDesc,
    ReasoningTokensDesc,
    TotalTokensDesc,
}

impl ModelCallSort {
    const fn token_expression(self) -> Option<&'static str> {
        match self {
            Self::EventAtDesc | Self::EventAtAsc => None,
            Self::InputTokensDesc => Some("projection.input_tokens"),
            Self::OutputTokensDesc => Some("projection.output_tokens"),
            Self::ReasoningTokensDesc => Some("projection.reasoning_output_tokens"),
            Self::TotalTokensDesc => Some(QUERY_TOTAL_TOKENS_SQL),
        }
    }
}

/// Exact exclusive keyset position for one model-call sort.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DashboardModelCallCursor {
    sort: ModelCallSort,
    token_value: Option<TokenCount>,
    event_at: UtcTimestamp,
    model_call_id: ModelCallId,
}

impl DashboardModelCallCursor {
    /// Construct an event-sort cursor boundary.
    ///
    /// # Errors
    ///
    /// Returns an error when `sort` is a token sort.
    pub fn event(
        sort: ModelCallSort,
        event_at: UtcTimestamp,
        model_call_id: ModelCallId,
    ) -> StorageResult<Self> {
        if sort.token_expression().is_some() {
            return Err(StorageError::DashboardCursorSortMismatch);
        }
        Ok(Self {
            sort,
            token_value: None,
            event_at,
            model_call_id,
        })
    }

    /// Construct a token-sort cursor boundary, where `None` represents an unknown token value.
    ///
    /// # Errors
    ///
    /// Returns an error when `sort` is an event sort.
    pub fn token(
        sort: ModelCallSort,
        token_value: Option<TokenCount>,
        event_at: UtcTimestamp,
        model_call_id: ModelCallId,
    ) -> StorageResult<Self> {
        if sort.token_expression().is_none() {
            return Err(StorageError::DashboardCursorSortMismatch);
        }
        Ok(Self {
            sort,
            token_value,
            event_at,
            model_call_id,
        })
    }

    #[must_use]
    pub const fn sort(self) -> ModelCallSort {
        self.sort
    }

    #[must_use]
    pub const fn token_value(self) -> Option<TokenCount> {
        self.token_value
    }

    #[must_use]
    pub const fn event_at(self) -> UtcTimestamp {
        self.event_at
    }

    #[must_use]
    pub const fn model_call_id(self) -> ModelCallId {
        self.model_call_id
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelCallPageQuery {
    limit: usize,
    filters: ModelCallFilters,
    sort: ModelCallSort,
    cursor: Option<DashboardModelCallCursor>,
}

impl ModelCallPageQuery {
    /// Construct a bounded model-call page query.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid limit or cursor/sort mismatch.
    pub fn new(
        limit: usize,
        filters: ModelCallFilters,
        sort: ModelCallSort,
        cursor: Option<DashboardModelCallCursor>,
    ) -> StorageResult<Self> {
        validate_page_limit(limit)?;
        if cursor.is_some_and(|value| value.sort != sort) {
            return Err(StorageError::DashboardCursorSortMismatch);
        }
        Ok(Self {
            limit,
            filters,
            sort,
            cursor,
        })
    }

    #[must_use]
    pub const fn limit(&self) -> usize {
        self.limit
    }

    #[must_use]
    pub const fn filters(&self) -> &ModelCallFilters {
        &self.filters
    }

    #[must_use]
    pub const fn sort(&self) -> ModelCallSort {
        self.sort
    }

    #[must_use]
    pub const fn cursor(&self) -> Option<DashboardModelCallCursor> {
        self.cursor
    }
}

/// Stable last-observed conversation keyset boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DashboardConversationCursor {
    last_observed_at: UtcTimestamp,
    conversation_id: ConversationId,
}

impl DashboardConversationCursor {
    #[must_use]
    pub const fn new(last_observed_at: UtcTimestamp, conversation_id: ConversationId) -> Self {
        Self {
            last_observed_at,
            conversation_id,
        }
    }

    #[must_use]
    pub const fn last_observed_at(self) -> UtcTimestamp {
        self.last_observed_at
    }

    #[must_use]
    pub const fn conversation_id(self) -> ConversationId {
        self.conversation_id
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversationPageQuery {
    limit: usize,
    selector: Option<ConversationSelector>,
    source_id: Option<SourceId>,
    from: Option<UtcTimestamp>,
    to: Option<UtcTimestamp>,
    cursor: Option<DashboardConversationCursor>,
}

impl ConversationPageQuery {
    /// Construct a bounded conversation page query.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid limit or non-increasing time range.
    pub fn new(
        limit: usize,
        selector: Option<ConversationSelector>,
        source_id: Option<SourceId>,
        from: Option<UtcTimestamp>,
        to: Option<UtcTimestamp>,
        cursor: Option<DashboardConversationCursor>,
    ) -> StorageResult<Self> {
        validate_page_limit(limit)?;
        validate_time_range(from, to)?;
        Ok(Self {
            limit,
            selector,
            source_id,
            from,
            to,
            cursor,
        })
    }

    #[must_use]
    pub const fn limit(&self) -> usize {
        self.limit
    }

    #[must_use]
    pub const fn selector(&self) -> Option<&ConversationSelector> {
        self.selector.as_ref()
    }

    #[must_use]
    pub const fn source_id(&self) -> Option<&SourceId> {
        self.source_id.as_ref()
    }

    #[must_use]
    pub const fn from(&self) -> Option<UtcTimestamp> {
        self.from
    }

    #[must_use]
    pub const fn to(&self) -> Option<UtcTimestamp> {
        self.to
    }

    #[must_use]
    pub const fn cursor(&self) -> Option<DashboardConversationCursor> {
        self.cursor
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DashboardAlertCursor {
    created_at: UtcTimestamp,
    alert_id: AlertId,
}

impl DashboardAlertCursor {
    #[must_use]
    pub const fn new(created_at: UtcTimestamp, alert_id: AlertId) -> Self {
        Self {
            created_at,
            alert_id,
        }
    }

    #[must_use]
    pub const fn created_at(self) -> UtcTimestamp {
        self.created_at
    }

    #[must_use]
    pub const fn alert_id(self) -> AlertId {
        self.alert_id
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AlertFilters {
    conversation: Option<ConversationSelector>,
    model_call_id: Option<String>,
    action_state: Option<ActionLifecycleState>,
    from: Option<UtcTimestamp>,
    to: Option<UtcTimestamp>,
}

impl AlertFilters {
    /// Construct validated AND-composed alert filters.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid opaque ID or non-increasing time range.
    pub fn new(
        conversation: Option<ConversationSelector>,
        model_call_id: Option<String>,
        action_state: Option<ActionLifecycleState>,
        from: Option<UtcTimestamp>,
        to: Option<UtcTimestamp>,
    ) -> StorageResult<Self> {
        validate_time_range(from, to)?;
        if model_call_id.as_deref().is_some_and(|value| {
            value.is_empty()
                || value.len() > 128
                || !value.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-')
                })
        }) {
            return Err(StorageError::InvalidDashboardFilter {
                field: "model_call_id",
            });
        }
        Ok(Self {
            conversation,
            model_call_id,
            action_state,
            from,
            to,
        })
    }

    #[must_use]
    pub const fn conversation(&self) -> Option<&ConversationSelector> {
        self.conversation.as_ref()
    }

    #[must_use]
    pub fn model_call_id(&self) -> Option<&str> {
        self.model_call_id.as_deref()
    }

    #[must_use]
    pub const fn action_state(&self) -> Option<ActionLifecycleState> {
        self.action_state
    }

    #[must_use]
    pub const fn from(&self) -> Option<UtcTimestamp> {
        self.from
    }

    #[must_use]
    pub const fn to(&self) -> Option<UtcTimestamp> {
        self.to
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlertPageQuery {
    limit: usize,
    filters: AlertFilters,
    cursor: Option<DashboardAlertCursor>,
}

impl AlertPageQuery {
    /// Construct a bounded alert page query.
    ///
    /// # Errors
    ///
    /// Returns an error when the limit is outside the Dashboard hard cap.
    pub fn new(
        limit: usize,
        filters: AlertFilters,
        cursor: Option<DashboardAlertCursor>,
    ) -> StorageResult<Self> {
        validate_page_limit(limit)?;
        Ok(Self {
            limit,
            filters,
            cursor,
        })
    }

    #[must_use]
    pub const fn limit(&self) -> usize {
        self.limit
    }

    #[must_use]
    pub const fn filters(&self) -> &AlertFilters {
        &self.filters
    }

    #[must_use]
    pub const fn cursor(&self) -> Option<DashboardAlertCursor> {
        self.cursor
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardPage<T, C> {
    pub items: Vec<T>,
    pub limit: usize,
    pub has_more: bool,
    pub next_cursor: Option<C>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CanonicalFieldName {
    Conversation,
    Turn,
    EventAt,
    Model,
    ReasoningEffort,
    InputTokens,
    CachedInputTokens,
    OutputTokens,
    ReasoningOutputTokens,
    Client,
    Surface,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardFieldSource {
    pub field: CanonicalFieldName,
    pub source_id: SourceId,
    pub source_contract_version: SourceContractVersion,
    pub observed_at: UtcTimestamp,
    pub quality: EvidenceQuality,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardConversationRef {
    pub id: ConversationId,
    pub source_id: Option<ConversationSourceId>,
    pub thread_id: Option<ThreadSourceId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DashboardTokenUsage {
    pub input: Option<TokenCount>,
    pub cached_input: Option<TokenCount>,
    pub output: Option<TokenCount>,
    pub reasoning: Option<TokenCount>,
    pub total: Option<TokenCount>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DashboardAlertState {
    None,
    Matched,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardModelCall {
    pub id: ModelCallId,
    pub event_at: UtcTimestamp,
    pub conversation: Option<DashboardConversationRef>,
    pub turn_id: Option<TurnId>,
    pub model: Option<ModelName>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub tokens: DashboardTokenUsage,
    pub source_ids: Vec<SourceId>,
    pub client: Option<ClientName>,
    pub surface: Option<SurfaceName>,
    pub temporal_class: DataOrigin,
    pub alert_state: DashboardAlertState,
    pub alert_count: u64,
    pub field_sources: Vec<DashboardFieldSource>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DashboardTokenAggregate {
    pub value: TokenCount,
    pub known_call_count: u64,
    pub total_call_count: u64,
    pub complete: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DashboardModelCallSummary {
    pub call_count: u64,
    pub alert_count: u64,
    pub input: DashboardTokenAggregate,
    pub cached_input: DashboardTokenAggregate,
    pub output: DashboardTokenAggregate,
    pub reasoning: DashboardTokenAggregate,
    pub total: DashboardTokenAggregate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardConversation {
    pub id: ConversationId,
    pub source_id: Option<ConversationSourceId>,
    pub thread_id: Option<ThreadSourceId>,
    pub first_observed_at: UtcTimestamp,
    pub last_observed_at: UtcTimestamp,
    pub model_call_count: u64,
    pub alert_count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardAlertEvidence {
    pub reasoning_output_tokens: TokenCount,
    pub pattern_n: PatternN,
    pub event_at: UtcTimestamp,
    pub source_id: SourceId,
    pub source_contract_version: SourceContractVersion,
    pub observed_at: UtcTimestamp,
    pub quality: EvidenceQuality,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DashboardAlertAction {
    pub action_type: RegisteredActionType,
    pub parameter_schema_version: NonZeroU16,
    pub state: ActionLifecycleState,
    pub created_at: UtcTimestamp,
    pub not_before: UtcTimestamp,
    pub started_at: Option<UtcTimestamp>,
    pub completed_at: Option<UtcTimestamp>,
    pub error_code: Option<ActionErrorCode>,
    pub suppression_reason: Option<ActionSuppressionReason>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardAlert {
    pub id: AlertId,
    pub model_call_id: ModelCallId,
    pub conversation_id: Option<ConversationId>,
    pub rule_id: BuiltInRuleId,
    pub rule_version: RuleVersion,
    pub evidence: DashboardAlertEvidence,
    pub action: Option<DashboardAlertAction>,
    pub created_at: UtcTimestamp,
}

fn validate_page_limit(limit: usize) -> StorageResult<()> {
    if !(1..=MAX_DASHBOARD_PAGE_SIZE).contains(&limit) {
        return Err(StorageError::InvalidDashboardQueryLimit {
            max: MAX_DASHBOARD_PAGE_SIZE,
        });
    }
    Ok(())
}

fn validate_filter_text(value: Option<&str>, field: &'static str, max: usize) -> StorageResult<()> {
    if value.is_some_and(|value| value.is_empty() || value.chars().count() > max) {
        return Err(StorageError::InvalidDashboardFilter { field });
    }
    Ok(())
}

fn validate_time_range(from: Option<UtcTimestamp>, to: Option<UtcTimestamp>) -> StorageResult<()> {
    if from.zip(to).is_some_and(|(from, to)| from >= to) {
        return Err(StorageError::InvalidDashboardTimeRange);
    }
    Ok(())
}

fn text_parameter(value: impl Into<String>) -> Value {
    Value::Text(value.into())
}

fn origin_text(value: DataOrigin) -> &'static str {
    match value {
        DataOrigin::Realtime => "realtime",
        DataOrigin::Historical => "historical",
    }
}

fn append_projection_selector(
    sql: &mut String,
    parameters: &mut Vec<Value>,
    selector: &ConversationSelector,
) -> StorageResult<()> {
    sql.push_str(" AND (");
    let mut has_term = false;
    if let Some(raw) = selector.raw() {
        sql.push_str(
            "projection.conversation_id = ?
             OR projection.conversation_source_id = ?
             OR projection.thread_source_id = ?
             OR EXISTS (
                SELECT 1 FROM conversation_source_keys AS source_key
                WHERE source_key.conversation_id = projection.conversation_id
                  AND source_key.key_value = ?
             )",
        );
        parameters.extend((0..4).map(|_| text_parameter(raw)));
        has_term = true;
    }
    if !selector.resolved_identifiers().is_empty() {
        if has_term {
            sql.push_str(" OR ");
        }
        sql.push_str(
            "EXISTS (
                SELECT 1 FROM json_each(?) AS resolved
                WHERE resolved.type = 'text'
                  AND (
                    resolved.value = projection.conversation_source_id
                    OR resolved.value = projection.thread_source_id
                    OR EXISTS (
                        SELECT 1 FROM conversation_source_keys AS resolved_key
                        WHERE resolved_key.conversation_id = projection.conversation_id
                          AND resolved_key.key_value = resolved.value
                    )
                  )
             )",
        );
        let encoded = serde_json::to_string(selector.resolved_identifiers())
            .map_err(|_| StorageError::InvalidConversationSelector)?;
        parameters.push(text_parameter(encoded));
    }
    sql.push(')');
    Ok(())
}

fn append_conversation_selector(
    sql: &mut String,
    parameters: &mut Vec<Value>,
    selector: &ConversationSelector,
) -> StorageResult<()> {
    sql.push_str(" AND (");
    let mut has_term = false;
    if let Some(raw) = selector.raw() {
        sql.push_str(
            "conversation.id = ?
             OR conversation.source_id = ?
             OR conversation.thread_id = ?
             OR EXISTS (
                SELECT 1 FROM conversation_source_keys AS source_key
                WHERE source_key.conversation_id = conversation.id
                  AND source_key.key_value = ?
             )",
        );
        parameters.extend((0..4).map(|_| text_parameter(raw)));
        has_term = true;
    }
    if !selector.resolved_identifiers().is_empty() {
        if has_term {
            sql.push_str(" OR ");
        }
        sql.push_str(
            "EXISTS (
                SELECT 1 FROM json_each(?) AS resolved
                WHERE resolved.type = 'text'
                  AND (
                    resolved.value = conversation.source_id
                    OR resolved.value = conversation.thread_id
                    OR EXISTS (
                        SELECT 1 FROM conversation_source_keys AS resolved_key
                        WHERE resolved_key.conversation_id = conversation.id
                          AND resolved_key.key_value = resolved.value
                    )
                  )
             )",
        );
        let encoded = serde_json::to_string(selector.resolved_identifiers())
            .map_err(|_| StorageError::InvalidConversationSelector)?;
        parameters.push(text_parameter(encoded));
    }
    sql.push(')');
    Ok(())
}

fn model_call_predicate(filters: &ModelCallFilters) -> StorageResult<(String, Vec<Value>)> {
    let mut sql = String::from(" WHERE 1 = 1");
    let mut parameters = Vec::new();
    if let Some(selector) = filters.conversation() {
        append_projection_selector(&mut sql, &mut parameters, selector)?;
    }
    if let Some(model) = filters.model() {
        sql.push_str(" AND projection.model = ?");
        parameters.push(text_parameter(model));
    }
    if let Some(source_id) = filters.source_id() {
        sql.push_str(
            " AND EXISTS (
                SELECT 1 FROM model_call_observations AS observation
                WHERE observation.model_call_id = projection.model_call_id
                  AND observation.source_id = ?
             )",
        );
        parameters.push(text_parameter(source_id.as_str()));
    }
    if let Some(client) = filters.client() {
        sql.push_str(" AND projection.client = ?");
        parameters.push(text_parameter(client));
    }
    if let Some(from) = filters.from() {
        sql.push_str(" AND projection.event_at >= ?");
        parameters.push(text_parameter(timestamp_text(from)));
    }
    if let Some(to) = filters.to() {
        sql.push_str(" AND projection.event_at < ?");
        parameters.push(text_parameter(timestamp_text(to)));
    }
    match filters.alert() {
        AlertMatchFilter::Any => {}
        AlertMatchFilter::Matched => sql.push_str(
            " AND EXISTS (
                SELECT 1 FROM alerts AS matched_alert
                WHERE matched_alert.model_call_id = projection.model_call_id
             )",
        ),
        AlertMatchFilter::None => sql.push_str(
            " AND NOT EXISTS (
                SELECT 1 FROM alerts AS matched_alert
                WHERE matched_alert.model_call_id = projection.model_call_id
             )",
        ),
    }
    if let Some(temporal_class) = filters.temporal_class() {
        sql.push_str(" AND projection.temporal_class = ?");
        parameters.push(text_parameter(origin_text(temporal_class)));
    }
    Ok((sql, parameters))
}

impl StorageReader {
    /// Read the exact active built-in rule states for the Dashboard capability
    /// document inside one query-only snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when an active definition was not registered or a
    /// persisted state violates the closed rule capability contract.
    pub fn dashboard_rule_capabilities(&self) -> StorageResult<Vec<DashboardRuleCapability>> {
        self.read_snapshot(|connection| {
            let definitions = BuiltInRuleRegistry::active_definitions();
            if definitions.len() > 32 {
                return Err(StorageError::InvalidStoredValue {
                    field: "rules.active_definition_count",
                });
            }
            definitions
                .iter()
                .map(|definition| {
                    let stored = connection
                        .query_row(
                            "SELECT enabled, notify_max_n, notify_all,
                                    last_evaluated_at, last_error_code
                             FROM rules
                             WHERE rule_id = ?1 AND rule_version = ?2",
                            params![definition.id.as_str(), definition.version.get()],
                            |row| {
                                Ok((
                                    row.get::<_, bool>(0)?,
                                    row.get::<_, Option<i64>>(1)?,
                                    row.get::<_, bool>(2)?,
                                    row.get::<_, Option<String>>(3)?,
                                    row.get::<_, Option<String>>(4)?,
                                ))
                            },
                        )
                        .optional()?
                        .ok_or(StorageError::InvalidStoredValue {
                            field: "rules.active_definition",
                        })?;
                    let notify_max_n = match (stored.1, stored.2) {
                        (Some(value), false) => {
                            let maximum = NonZeroU32::new(u32::try_from(value).map_err(|_| {
                                StorageError::InvalidStoredValue {
                                    field: "rules.notify_max_n",
                                }
                            })?)
                            .ok_or(
                                StorageError::InvalidStoredValue {
                                    field: "rules.notify_max_n",
                                },
                            )?;
                            if maximum.get() > MAX_NOTIFY_MAX_N {
                                return Err(StorageError::InvalidStoredValue {
                                    field: "rules.notify_max_n",
                                });
                            }
                            Some(maximum)
                        }
                        (None, true) => None,
                        _ => {
                            return Err(StorageError::InvalidStoredValue {
                                field: "rules.notify_threshold",
                            });
                        }
                    };
                    let last_evaluated_at = stored
                        .3
                        .as_deref()
                        .map(|value| parse_timestamp(value, "rules.last_evaluated_at"))
                        .transpose()?;
                    let last_error_code = stored
                        .4
                        .as_deref()
                        .map(|value| {
                            value.parse::<RuleErrorCode>().map_err(|_| {
                                StorageError::InvalidStoredValue {
                                    field: "rules.last_error_code",
                                }
                            })
                        })
                        .transpose()?;
                    Ok(DashboardRuleCapability {
                        rule_id: definition.id,
                        rule_version: definition.version,
                        enabled: stored.0,
                        notify_max_n,
                        last_evaluated_at,
                        last_error_code,
                    })
                })
                .collect()
        })
    }
}

#[derive(Debug)]
struct RawModelCall {
    id: String,
    event_at: String,
    conversation_id: Option<String>,
    turn_id: Option<String>,
    temporal_class: String,
    conversation_source_id: Option<String>,
    thread_source_id: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<String>,
    input_tokens: Option<i64>,
    cached_input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    reasoning_output_tokens: Option<i64>,
    total_tokens: Option<i64>,
    client: Option<String>,
    surface: Option<String>,
}

impl StorageReader {
    /// Read one exact model-call Dashboard projection inside a query-only snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error for a database failure, corrupt stored value, or closed bound violation.
    pub fn dashboard_model_call(
        &self,
        id: ModelCallId,
    ) -> StorageResult<Option<DashboardModelCall>> {
        self.read_snapshot(|connection| dashboard_model_call(connection, id))
    }

    /// Read one bounded, stable model-call page inside a query-only snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error for a database failure, corrupt stored value, or closed bound violation.
    pub fn model_call_page(
        &self,
        query: &ModelCallPageQuery,
    ) -> StorageResult<DashboardPage<DashboardModelCall, DashboardModelCallCursor>> {
        self.read_snapshot(|connection| model_call_page(connection, query))
    }

    /// Aggregate the complete model-call filter set inside one query-only snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error for a database failure, corrupt stored value, or aggregate overflow.
    pub fn model_call_summary(
        &self,
        filters: &ModelCallFilters,
    ) -> StorageResult<DashboardModelCallSummary> {
        self.read_snapshot(|connection| model_call_summary(connection, filters))
    }
}

fn dashboard_model_call(
    connection: &Connection,
    id: ModelCallId,
) -> StorageResult<Option<DashboardModelCall>> {
    let sql = format!(
        "SELECT projection.model_call_id, projection.event_at,
                projection.conversation_id, projection.turn_id,
                projection.temporal_class, projection.conversation_source_id,
                projection.thread_source_id, projection.model,
                projection.reasoning_effort, projection.input_tokens,
                projection.cached_input_tokens, projection.output_tokens,
                projection.reasoning_output_tokens, {QUERY_TOTAL_TOKENS_SQL},
                projection.client, projection.surface
         FROM model_call_dashboard_projection AS projection
         WHERE projection.model_call_id = ?1"
    );
    let row = connection
        .query_row(&sql, [id.to_string()], raw_model_call)
        .optional()?;
    row.map(|row| build_dashboard_model_call(connection, row))
        .transpose()
}

fn model_call_page(
    connection: &Connection,
    query: &ModelCallPageQuery,
) -> StorageResult<DashboardPage<DashboardModelCall, DashboardModelCallCursor>> {
    let (mut predicate, mut parameters) = model_call_predicate(query.filters())?;
    append_model_call_cursor(
        &mut predicate,
        &mut parameters,
        query.sort(),
        query.cursor(),
    )?;
    let order = match query.sort() {
        ModelCallSort::EventAtDesc => {
            "projection.event_at DESC, projection.model_call_id DESC".to_owned()
        }
        ModelCallSort::EventAtAsc => {
            "projection.event_at ASC, projection.model_call_id ASC".to_owned()
        }
        ModelCallSort::InputTokensDesc => {
            "projection.input_tokens IS NULL, projection.input_tokens DESC, projection.event_at DESC, projection.model_call_id DESC".to_owned()
        }
        ModelCallSort::OutputTokensDesc => {
            "projection.output_tokens IS NULL, projection.output_tokens DESC, projection.event_at DESC, projection.model_call_id DESC".to_owned()
        }
        ModelCallSort::ReasoningTokensDesc => {
            "projection.reasoning_output_tokens IS NULL, projection.reasoning_output_tokens DESC, projection.event_at DESC, projection.model_call_id DESC".to_owned()
        }
        ModelCallSort::TotalTokensDesc => format!(
            "{QUERY_TOTAL_TOKENS_SQL} IS NULL, {QUERY_TOTAL_TOKENS_SQL} DESC, projection.event_at DESC, projection.model_call_id DESC"
        ),
    };
    let sql = format!(
        "SELECT projection.model_call_id, projection.event_at,
                projection.conversation_id, projection.turn_id,
                projection.temporal_class, projection.conversation_source_id,
                projection.thread_source_id, projection.model,
                projection.reasoning_effort, projection.input_tokens,
                projection.cached_input_tokens, projection.output_tokens,
                projection.reasoning_output_tokens, {QUERY_TOTAL_TOKENS_SQL},
                projection.client, projection.surface
         FROM model_call_dashboard_projection AS projection
         {predicate}
         ORDER BY {order}
         LIMIT ?"
    );
    parameters.push(Value::Integer(
        i64::try_from(query.limit() + 1).expect("Dashboard page limit fits SQLite INTEGER"),
    ));
    let mut rows = {
        let mut statement = connection.prepare(&sql)?;
        statement
            .query_map(params_from_iter(parameters), raw_model_call)?
            .collect::<Result<Vec<_>, _>>()?
    };
    let has_more = rows.len() > query.limit();
    if has_more {
        rows.truncate(query.limit());
    }
    let next_cursor = if has_more {
        rows.last()
            .map(|row| model_call_cursor_for_raw(query.sort(), row))
            .transpose()?
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(|row| build_dashboard_model_call(connection, row))
        .collect::<StorageResult<Vec<_>>>()?;
    Ok(DashboardPage {
        items,
        limit: query.limit(),
        has_more,
        next_cursor,
    })
}

fn raw_model_call(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawModelCall> {
    Ok(RawModelCall {
        id: row.get(0)?,
        event_at: row.get(1)?,
        conversation_id: row.get(2)?,
        turn_id: row.get(3)?,
        temporal_class: row.get(4)?,
        conversation_source_id: row.get(5)?,
        thread_source_id: row.get(6)?,
        model: row.get(7)?,
        reasoning_effort: row.get(8)?,
        input_tokens: row.get(9)?,
        cached_input_tokens: row.get(10)?,
        output_tokens: row.get(11)?,
        reasoning_output_tokens: row.get(12)?,
        total_tokens: row.get(13)?,
        client: row.get(14)?,
        surface: row.get(15)?,
    })
}

fn append_model_call_cursor(
    predicate: &mut String,
    parameters: &mut Vec<Value>,
    sort: ModelCallSort,
    cursor: Option<DashboardModelCallCursor>,
) -> StorageResult<()> {
    let Some(cursor) = cursor else {
        return Ok(());
    };
    if cursor.sort() != sort {
        return Err(StorageError::DashboardCursorSortMismatch);
    }
    let event_at = timestamp_text(cursor.event_at());
    let id = cursor.model_call_id().to_string();
    match sort {
        ModelCallSort::EventAtDesc => {
            predicate.push_str(
                " AND (projection.event_at < ?
                     OR (projection.event_at = ? AND projection.model_call_id < ?))",
            );
            parameters.extend([
                text_parameter(event_at.clone()),
                text_parameter(event_at),
                text_parameter(id),
            ]);
        }
        ModelCallSort::EventAtAsc => {
            predicate.push_str(
                " AND (projection.event_at > ?
                     OR (projection.event_at = ? AND projection.model_call_id > ?))",
            );
            parameters.extend([
                text_parameter(event_at.clone()),
                text_parameter(event_at),
                text_parameter(id),
            ]);
        }
        token_sort => {
            let expression = token_sort
                .token_expression()
                .ok_or(StorageError::DashboardCursorSortMismatch)?;
            let token = cursor.token_value();
            if let Some(token) = token {
                write!(
                    predicate,
                    " AND (({expression} IS NOT NULL AND (
                                {expression} < ?
                                OR ({expression} = ? AND (
                                    projection.event_at < ?
                                    OR (projection.event_at = ? AND projection.model_call_id < ?)
                                ))
                            )) OR {expression} IS NULL)"
                )
                .expect("writing SQL to a String cannot fail");
                let value =
                    i64::try_from(token.get()).expect("validated token count fits SQLite INTEGER");
                parameters.extend([
                    Value::Integer(value),
                    Value::Integer(value),
                    text_parameter(event_at.clone()),
                    text_parameter(event_at),
                    text_parameter(id),
                ]);
            } else {
                write!(
                    predicate,
                    " AND {expression} IS NULL
                      AND (projection.event_at < ?
                           OR (projection.event_at = ? AND projection.model_call_id < ?))"
                )
                .expect("writing SQL to a String cannot fail");
                parameters.extend([
                    text_parameter(event_at.clone()),
                    text_parameter(event_at),
                    text_parameter(id),
                ]);
            }
        }
    }
    Ok(())
}

fn model_call_cursor_for_raw(
    sort: ModelCallSort,
    row: &RawModelCall,
) -> StorageResult<DashboardModelCallCursor> {
    let event_at = parse_timestamp(&row.event_at, "model_call.event_at")?;
    let id = parse_model_call_id(&row.id)?;
    match sort {
        ModelCallSort::EventAtDesc | ModelCallSort::EventAtAsc => {
            DashboardModelCallCursor::event(sort, event_at, id)
        }
        ModelCallSort::InputTokensDesc => DashboardModelCallCursor::token(
            sort,
            parse_optional_token(row.input_tokens)?,
            event_at,
            id,
        ),
        ModelCallSort::OutputTokensDesc => DashboardModelCallCursor::token(
            sort,
            parse_optional_token(row.output_tokens)?,
            event_at,
            id,
        ),
        ModelCallSort::ReasoningTokensDesc => DashboardModelCallCursor::token(
            sort,
            parse_optional_token(row.reasoning_output_tokens)?,
            event_at,
            id,
        ),
        ModelCallSort::TotalTokensDesc => DashboardModelCallCursor::token(
            sort,
            parse_optional_token(row.total_tokens)?,
            event_at,
            id,
        ),
    }
}

fn build_dashboard_model_call(
    connection: &Connection,
    row: RawModelCall,
) -> StorageResult<DashboardModelCall> {
    let id = parse_model_call_id(&row.id)?;
    let event_at = parse_timestamp(&row.event_at, "model_call.event_at")?;
    let conversation = row
        .conversation_id
        .map(|value| -> StorageResult<DashboardConversationRef> {
            Ok(DashboardConversationRef {
                id: parse_conversation_id(&value)?,
                source_id: row
                    .conversation_source_id
                    .map(ConversationSourceId::new)
                    .transpose()?,
                thread_id: row.thread_source_id.map(ThreadSourceId::new).transpose()?,
            })
        })
        .transpose()?;
    let turn_id = row
        .turn_id
        .map(|value| {
            TurnId::from_str(&value)
                .map_err(|_| StorageError::InvalidStoredValue { field: "turn.id" })
        })
        .transpose()?;
    let tokens = DashboardTokenUsage {
        input: parse_optional_token(row.input_tokens)?,
        cached_input: parse_optional_token(row.cached_input_tokens)?,
        output: parse_optional_token(row.output_tokens)?,
        reasoning: parse_optional_token(row.reasoning_output_tokens)?,
        total: parse_optional_token(row.total_tokens)?,
    };
    let source_ids = load_model_call_source_ids(connection, id)?;
    let alert_count = load_model_call_alert_count(connection, id)?;
    let field_sources = load_model_call_field_sources(connection, id)?;
    Ok(DashboardModelCall {
        id,
        event_at,
        conversation,
        turn_id,
        model: row.model.map(ModelName::new).transpose()?,
        reasoning_effort: row
            .reasoning_effort
            .map(|value| {
                ReasoningEffort::from_str(&value).map_err(|_| StorageError::InvalidStoredValue {
                    field: "model_call.reasoning_effort",
                })
            })
            .transpose()?,
        tokens,
        source_ids,
        client: row.client.map(ClientName::new).transpose()?,
        surface: row.surface.map(SurfaceName::new).transpose()?,
        temporal_class: parse_temporal_class(&row.temporal_class)?,
        alert_state: if alert_count == 0 {
            DashboardAlertState::None
        } else {
            DashboardAlertState::Matched
        },
        alert_count,
        field_sources,
    })
}

fn load_model_call_source_ids(
    connection: &Connection,
    model_call_id: ModelCallId,
) -> StorageResult<Vec<SourceId>> {
    let mut statement = connection.prepare(
        "SELECT DISTINCT source_id
         FROM model_call_observations
         WHERE model_call_id = ?1
         ORDER BY source_id
         LIMIT 33",
    )?;
    let raw = statement
        .query_map([model_call_id.to_string()], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if raw.len() > MAX_MODEL_CALL_SOURCE_IDS {
        return Err(StorageError::TooManyModelCallSources {
            max: MAX_MODEL_CALL_SOURCE_IDS,
        });
    }
    if raw.is_empty() {
        return Err(StorageError::InvalidStoredValue {
            field: "model_call.source_ids",
        });
    }
    raw.into_iter()
        .map(|value| {
            SourceId::new(value).map_err(|_| StorageError::InvalidStoredValue {
                field: "model_call.source_ids",
            })
        })
        .collect()
}

fn load_model_call_alert_count(
    connection: &Connection,
    model_call_id: ModelCallId,
) -> StorageResult<u64> {
    let count: i64 = connection.query_row(
        "SELECT count(*) FROM alerts WHERE model_call_id = ?1",
        [model_call_id.to_string()],
        |row| row.get(0),
    )?;
    let count = u64::try_from(count).map_err(|_| StorageError::InvalidStoredValue {
        field: "model_call.alert_count",
    })?;
    if count > MAX_DASHBOARD_ALERT_COUNT {
        return Err(StorageError::DashboardAlertCountOverflow {
            max: MAX_DASHBOARD_ALERT_COUNT,
        });
    }
    Ok(count)
}

#[allow(clippy::too_many_lines)]
fn load_model_call_field_sources(
    connection: &Connection,
    model_call_id: ModelCallId,
) -> StorageResult<Vec<DashboardFieldSource>> {
    let mut statement = connection.prepare(
        "SELECT field_name, source_id, source_contract_version, observed_at, quality
         FROM (
            SELECT 'event_at' AS field_name,
                   observation.source_id,
                   observation.source_contract_version,
                   observation.event_provenance_observed_at AS observed_at,
                   observation.event_quality AS quality
            FROM model_calls AS model_call
            JOIN model_call_observations AS observation
              ON observation.id = model_call.event_time_observation_id
             AND observation.model_call_id = model_call.id
            WHERE model_call.id = ?1

            UNION ALL

            SELECT 'conversation', observation.source_id,
                   observation.source_contract_version,
                   relationship.provenance_observed_at,
                   relationship.quality
            FROM model_calls AS model_call
            JOIN model_call_observation_relationships AS relationship
              ON relationship.observation_id = model_call.relationship_observation_id
             AND relationship.model_call_id = model_call.id
            JOIN model_call_observations AS observation
              ON observation.id = relationship.observation_id
             AND observation.model_call_id = relationship.model_call_id
            WHERE model_call.id = ?1

            UNION ALL

            SELECT 'turn', observation.source_id,
                   observation.source_contract_version,
                   relationship.provenance_observed_at,
                   relationship.quality
            FROM model_calls AS model_call
            JOIN model_call_observation_relationships AS relationship
              ON relationship.observation_id = model_call.relationship_observation_id
             AND relationship.model_call_id = model_call.id
            JOIN model_call_observations AS observation
              ON observation.id = relationship.observation_id
             AND observation.model_call_id = relationship.model_call_id
            WHERE model_call.id = ?1 AND model_call.turn_id IS NOT NULL

            UNION ALL

            SELECT CASE projection.field_id
                       WHEN 'model_call.model' THEN 'model'
                       WHEN 'model_call.reasoning_effort' THEN 'reasoning_effort'
                       WHEN 'model_call.input_tokens' THEN 'input_tokens'
                       WHEN 'model_call.cached_input_tokens' THEN 'cached_input_tokens'
                       WHEN 'model_call.output_tokens' THEN 'output_tokens'
                       WHEN 'model_call.reasoning_output_tokens' THEN 'reasoning_output_tokens'
                       WHEN 'model_call.client' THEN 'client'
                       WHEN 'model_call.surface' THEN 'surface'
                   END,
                   observation.source_id,
                   observation.source_contract_version,
                   field.provenance_observed_at,
                   field.quality
            FROM model_call_projection_fields AS projection
            JOIN model_call_observations AS observation
              ON observation.id = projection.observation_id
             AND observation.model_call_id = projection.model_call_id
            JOIN model_call_observation_fields AS field
              ON field.observation_id = projection.observation_id
             AND field.field_id = projection.field_id
            WHERE projection.model_call_id = ?1
              AND projection.field_id IN (
                'model_call.model',
                'model_call.reasoning_effort',
                'model_call.input_tokens',
                'model_call.cached_input_tokens',
                'model_call.output_tokens',
                'model_call.reasoning_output_tokens',
                'model_call.client',
                'model_call.surface'
              )
         )
         ORDER BY field_name, source_id, source_contract_version
         LIMIT 33",
    )?;
    let raw = statement
        .query_map([model_call_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if raw.len() > 32 {
        return Err(StorageError::TooManyModelCallFieldSources { max: 32 });
    }
    raw.into_iter()
        .map(|(field, source_id, version, observed_at, quality)| {
            Ok(DashboardFieldSource {
                field: parse_canonical_field_name(&field)?,
                source_id: SourceId::new(source_id).map_err(|_| {
                    StorageError::InvalidStoredValue {
                        field: "model_call.field_sources.source_id",
                    }
                })?,
                source_contract_version: parse_contract_version(version)?,
                observed_at: parse_timestamp(&observed_at, "model_call.field_sources.observed_at")?,
                quality: parse_evidence_quality(&quality)?,
            })
        })
        .collect()
}

fn parse_canonical_field_name(value: &str) -> StorageResult<CanonicalFieldName> {
    match value {
        "conversation" => Ok(CanonicalFieldName::Conversation),
        "turn" => Ok(CanonicalFieldName::Turn),
        "event_at" => Ok(CanonicalFieldName::EventAt),
        "model" => Ok(CanonicalFieldName::Model),
        "reasoning_effort" => Ok(CanonicalFieldName::ReasoningEffort),
        "input_tokens" => Ok(CanonicalFieldName::InputTokens),
        "cached_input_tokens" => Ok(CanonicalFieldName::CachedInputTokens),
        "output_tokens" => Ok(CanonicalFieldName::OutputTokens),
        "reasoning_output_tokens" => Ok(CanonicalFieldName::ReasoningOutputTokens),
        "client" => Ok(CanonicalFieldName::Client),
        "surface" => Ok(CanonicalFieldName::Surface),
        _ => Err(StorageError::InvalidStoredValue {
            field: "model_call.field_sources.field",
        }),
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct TokenAccumulator {
    value: u64,
    known: u64,
}

impl TokenAccumulator {
    fn add(&mut self, value: Option<i64>) -> StorageResult<()> {
        let Some(value) = value else {
            return Ok(());
        };
        let value = TokenCount::try_from(value)?.get();
        self.value = self
            .value
            .checked_add(value)
            .filter(|sum| *sum <= MAX_TOKEN_COUNT)
            .ok_or(StorageError::DashboardAggregateOverflow)?;
        self.known = self
            .known
            .checked_add(1)
            .filter(|count| *count <= MAX_DASHBOARD_COUNT)
            .ok_or(StorageError::DashboardCountOverflow {
                field: "summary.known_call_count",
            })?;
        Ok(())
    }

    fn finish(self, total: u64) -> StorageResult<DashboardTokenAggregate> {
        Ok(DashboardTokenAggregate {
            value: TokenCount::new(self.value)?,
            known_call_count: self.known,
            total_call_count: total,
            complete: self.known == total,
        })
    }
}

fn model_call_summary(
    connection: &Connection,
    filters: &ModelCallFilters,
) -> StorageResult<DashboardModelCallSummary> {
    let (predicate, parameters) = model_call_predicate(filters)?;
    let sql = format!(
        "SELECT projection.input_tokens, projection.cached_input_tokens,
                projection.output_tokens, projection.reasoning_output_tokens,
                (SELECT count(*) FROM alerts AS alert
                 WHERE alert.model_call_id = projection.model_call_id)
         FROM model_call_dashboard_projection AS projection
         {predicate}"
    );
    let mut call_count = 0_u64;
    let mut alert_count = 0_u64;
    let mut input = TokenAccumulator::default();
    let mut cached_input = TokenAccumulator::default();
    let mut output = TokenAccumulator::default();
    let mut reasoning = TokenAccumulator::default();
    let mut total = TokenAccumulator::default();
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(parameters), |row| {
        Ok((
            row.get::<_, Option<i64>>(0)?,
            row.get::<_, Option<i64>>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, Option<i64>>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })?;
    for row in rows {
        let (row_input, row_cached, row_output, row_reasoning, row_alerts) = row?;
        let row_total = derive_total_tokens(row_input, row_output)?;
        call_count = call_count
            .checked_add(1)
            .filter(|count| *count <= MAX_DASHBOARD_COUNT)
            .ok_or(StorageError::DashboardCountOverflow {
                field: "summary.call_count",
            })?;
        alert_count = alert_count
            .checked_add(u64::try_from(row_alerts).map_err(|_| {
                StorageError::InvalidStoredValue {
                    field: "summary.alert_count",
                }
            })?)
            .filter(|count| *count <= MAX_DASHBOARD_COUNT)
            .ok_or(StorageError::DashboardCountOverflow {
                field: "summary.alert_count",
            })?;
        input.add(row_input)?;
        cached_input.add(row_cached)?;
        output.add(row_output)?;
        reasoning.add(row_reasoning)?;
        total.add(row_total)?;
    }
    Ok(DashboardModelCallSummary {
        call_count,
        alert_count,
        input: input.finish(call_count)?,
        cached_input: cached_input.finish(call_count)?,
        output: output.finish(call_count)?,
        reasoning: reasoning.finish(call_count)?,
        total: total.finish(call_count)?,
    })
}

fn derive_total_tokens(input: Option<i64>, output: Option<i64>) -> StorageResult<Option<i64>> {
    let Some((input, output)) = input.zip(output) else {
        return Ok(None);
    };
    let input = TokenCount::try_from(input)?;
    let output = TokenCount::try_from(output)?;
    Ok(input.checked_add(output).ok().map(|value| {
        i64::try_from(value.get()).expect("validated token count fits SQLite INTEGER")
    }))
}

#[derive(Debug)]
struct RawConversation {
    id: String,
    source_id: Option<String>,
    thread_id: Option<String>,
    first_observed_at: String,
    last_observed_at: String,
    model_call_count: i64,
    alert_count: i64,
}

impl StorageReader {
    /// Read one exact alert with historical evidence and current action state.
    ///
    /// # Errors
    ///
    /// Returns an error for a database failure, missing historical evidence, or corrupt value.
    pub fn dashboard_alert(&self, id: AlertId) -> StorageResult<Option<DashboardAlert>> {
        self.read_snapshot(|connection| dashboard_alert(connection, id))
    }

    /// Read one bounded conversation page inside a query-only snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error for a database failure, corrupt stored value, or count overflow.
    pub fn conversation_page(
        &self,
        query: &ConversationPageQuery,
    ) -> StorageResult<DashboardPage<DashboardConversation, DashboardConversationCursor>> {
        self.read_snapshot(|connection| conversation_page(connection, query))
    }

    /// Read the bounded source-health list with capabilities fetched in one batch.
    ///
    /// # Errors
    ///
    /// Returns an error for a database failure, corrupt stored value, or list bound violation.
    pub fn source_health_list(&self) -> StorageResult<Vec<StoredSourceHealth>> {
        self.read_snapshot(load_dashboard_sources)
    }
}

fn dashboard_alert(connection: &Connection, id: AlertId) -> StorageResult<Option<DashboardAlert>> {
    let row = connection
        .query_row(
            "SELECT alert.id, alert.model_call_id, projection.conversation_id,
                    alert.rule_id, alert.rule_version,
                    alert.reasoning_output_tokens, alert.pattern_n,
                    alert.event_at, alert.created_at,
                    provenance.source_id, provenance.source_contract_version,
                    provenance.observed_at, provenance.quality
             FROM alerts AS alert
             JOIN model_call_dashboard_projection AS projection
               ON projection.model_call_id = alert.model_call_id
             LEFT JOIN alert_evidence_provenance AS provenance
               ON provenance.alert_id = alert.id
             WHERE alert.id = ?1",
            [id.to_string()],
            raw_alert,
        )
        .optional()?;
    row.map(|row| build_dashboard_alert(connection, row))
        .transpose()
}

#[allow(clippy::too_many_lines)]
fn conversation_page(
    connection: &Connection,
    query: &ConversationPageQuery,
) -> StorageResult<DashboardPage<DashboardConversation, DashboardConversationCursor>> {
    let mut predicate = String::from(" WHERE 1 = 1");
    let mut parameters = Vec::new();
    if let Some(selector) = query.selector() {
        append_conversation_selector(&mut predicate, &mut parameters, selector)?;
    }
    if let Some(source_id) = query.source_id() {
        predicate.push_str(
            " AND EXISTS (
                SELECT 1
                FROM model_call_dashboard_projection AS associated_call
                JOIN model_call_observations AS associated_observation
                  ON associated_observation.model_call_id = associated_call.model_call_id
                WHERE associated_call.conversation_id = conversation.id
                  AND associated_observation.source_id = ?
             )",
        );
        parameters.push(text_parameter(source_id.as_str()));
    }
    if let Some(from) = query.from() {
        predicate.push_str(" AND conversation.last_observed_at >= ?");
        parameters.push(text_parameter(timestamp_text(from)));
    }
    if let Some(to) = query.to() {
        predicate.push_str(" AND conversation.last_observed_at < ?");
        parameters.push(text_parameter(timestamp_text(to)));
    }
    if let Some(cursor) = query.cursor() {
        let last_observed_at = timestamp_text(cursor.last_observed_at());
        predicate.push_str(
            " AND (conversation.last_observed_at < ?
                 OR (conversation.last_observed_at = ? AND conversation.id < ?))",
        );
        parameters.extend([
            text_parameter(last_observed_at.clone()),
            text_parameter(last_observed_at),
            text_parameter(cursor.conversation_id().to_string()),
        ]);
    }
    let sql = format!(
        "SELECT conversation.id, conversation.source_id, conversation.thread_id,
                conversation.first_observed_at, conversation.last_observed_at,
                (SELECT count(*)
                 FROM model_call_dashboard_projection AS counted_call
                 WHERE counted_call.conversation_id = conversation.id),
                (SELECT count(*)
                 FROM alerts AS counted_alert
                 JOIN model_call_dashboard_projection AS alert_call
                   ON alert_call.model_call_id = counted_alert.model_call_id
                 WHERE alert_call.conversation_id = conversation.id)
         FROM conversations AS conversation
         {predicate}
         ORDER BY conversation.last_observed_at DESC, conversation.id DESC
         LIMIT ?"
    );
    parameters.push(Value::Integer(
        i64::try_from(query.limit() + 1).expect("Dashboard page limit fits SQLite INTEGER"),
    ));
    let mut rows = {
        let mut statement = connection.prepare(&sql)?;
        statement
            .query_map(params_from_iter(parameters), |row| {
                Ok(RawConversation {
                    id: row.get(0)?,
                    source_id: row.get(1)?,
                    thread_id: row.get(2)?,
                    first_observed_at: row.get(3)?,
                    last_observed_at: row.get(4)?,
                    model_call_count: row.get(5)?,
                    alert_count: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let has_more = rows.len() > query.limit();
    if has_more {
        rows.truncate(query.limit());
    }
    let next_cursor = if has_more {
        rows.last()
            .map(|row| -> StorageResult<DashboardConversationCursor> {
                Ok(DashboardConversationCursor::new(
                    parse_timestamp(&row.last_observed_at, "conversation.last_observed_at")?,
                    parse_conversation_id(&row.id)?,
                ))
            })
            .transpose()?
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(|row| {
            Ok(DashboardConversation {
                id: parse_conversation_id(&row.id)?,
                source_id: row.source_id.map(ConversationSourceId::new).transpose()?,
                thread_id: row.thread_id.map(ThreadSourceId::new).transpose()?,
                first_observed_at: parse_timestamp(
                    &row.first_observed_at,
                    "conversation.first_observed_at",
                )?,
                last_observed_at: parse_timestamp(
                    &row.last_observed_at,
                    "conversation.last_observed_at",
                )?,
                model_call_count: parse_nonnegative_count(
                    row.model_call_count,
                    "conversation.model_call_count",
                )?,
                alert_count: parse_nonnegative_count(row.alert_count, "conversation.alert_count")?,
            })
        })
        .collect::<StorageResult<Vec<_>>>()?;
    Ok(DashboardPage {
        items,
        limit: query.limit(),
        has_more,
        next_cursor,
    })
}

type RawSourceHealth = (
    String,
    i64,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
);

#[allow(clippy::too_many_lines)]
fn load_dashboard_sources(connection: &Connection) -> StorageResult<Vec<StoredSourceHealth>> {
    let raw = {
        let mut statement = connection.prepare(
            "SELECT source_id, source_contract_version, status, last_accepted_at,
                    last_error_code, last_error_occurred_at, updated_at
             FROM source_health
             ORDER BY source_id, source_contract_version
             LIMIT 33",
        )?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            })?
            .collect::<Result<Vec<RawSourceHealth>, _>>()?
    };
    if raw.len() > MAX_DASHBOARD_SOURCE_ROWS {
        return Err(StorageError::TooManySourceHealthRows {
            max: MAX_DASHBOARD_SOURCE_ROWS,
        });
    }

    let mut fields = BTreeMap::<(String, i64), Vec<FieldId>>::new();
    {
        let mut statement = connection.prepare(
            "WITH bounded_source AS (
                SELECT source_id, source_contract_version
                FROM source_health
                ORDER BY source_id, source_contract_version
                LIMIT 33
             )
             SELECT field.source_id, field.source_contract_version, field.field_id
             FROM source_health_supported_fields AS field
             JOIN bounded_source
               ON bounded_source.source_id = field.source_id
              AND bounded_source.source_contract_version = field.source_contract_version
             ORDER BY field.source_id, field.source_contract_version, field.field_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (source_id, contract_version, field_id) = row?;
            let entry = fields.entry((source_id, contract_version)).or_default();
            if entry.len() == MAX_SOURCE_CAPABILITY_FIELDS {
                return Err(StorageError::TooManySourceCapabilityFields {
                    max: MAX_SOURCE_CAPABILITY_FIELDS,
                });
            }
            entry.push(
                FieldId::new(field_id).map_err(|_| StorageError::InvalidStoredValue {
                    field: "source_health.supported_fields",
                })?,
            );
        }
    }

    raw.into_iter()
        .map(
            |(
                source_id,
                contract_version,
                status,
                last_accepted_at,
                last_error_code,
                last_error_occurred_at,
                updated_at,
            )| {
                let supported_fields = fields
                    .remove(&(source_id.clone(), contract_version))
                    .unwrap_or_default();
                Ok(StoredSourceHealth {
                    source_id: SourceId::new(source_id).map_err(|_| {
                        StorageError::InvalidStoredValue {
                            field: "source_health.source_id",
                        }
                    })?,
                    source_contract_version: parse_contract_version(contract_version)?,
                    status: parse_source_status(&status)?,
                    supported_fields,
                    last_accepted_at: last_accepted_at
                        .map(|value| parse_timestamp(&value, "source_health.last_accepted_at"))
                        .transpose()?,
                    last_error_code: last_error_code
                        .map(|value| {
                            SafeErrorCode::from_str(&value).map_err(|_| {
                                StorageError::InvalidStoredValue {
                                    field: "source_health.last_error_code",
                                }
                            })
                        })
                        .transpose()?,
                    last_error_occurred_at: last_error_occurred_at
                        .map(|value| {
                            parse_timestamp(&value, "source_health.last_error_occurred_at")
                        })
                        .transpose()?,
                    updated_at: parse_timestamp(&updated_at, "source_health.updated_at")?,
                })
            },
        )
        .collect()
}

#[derive(Debug)]
struct RawAlert {
    id: String,
    model_call_id: String,
    conversation_id: Option<String>,
    rule_id: String,
    rule_version: i64,
    reasoning_output_tokens: i64,
    pattern_n: i64,
    event_at: String,
    created_at: String,
    provenance_source_id: Option<String>,
    provenance_contract_version: Option<i64>,
    provenance_observed_at: Option<String>,
    provenance_quality: Option<String>,
}

impl StorageReader {
    /// Read one bounded alert page with historical evidence and current action state.
    ///
    /// # Errors
    ///
    /// Returns an error for a database failure, missing historical evidence, or corrupt value.
    pub fn alert_page(
        &self,
        query: &AlertPageQuery,
    ) -> StorageResult<DashboardPage<DashboardAlert, DashboardAlertCursor>> {
        self.read_snapshot(|connection| alert_page(connection, query))
    }
}

#[allow(clippy::too_many_lines)]
fn alert_page(
    connection: &Connection,
    query: &AlertPageQuery,
) -> StorageResult<DashboardPage<DashboardAlert, DashboardAlertCursor>> {
    let mut predicate = String::from(" WHERE 1 = 1");
    let mut parameters = Vec::new();
    if let Some(selector) = query.filters().conversation() {
        append_projection_selector(&mut predicate, &mut parameters, selector)?;
    }
    if let Some(model_call_id) = query.filters().model_call_id() {
        predicate.push_str(" AND alert.model_call_id = ?");
        parameters.push(text_parameter(model_call_id));
    }
    if let Some(action_state) = query.filters().action_state() {
        predicate.push_str(
            " AND (
                SELECT current_action.state
                FROM action_outbox AS current_action
                WHERE current_action.alert_id = alert.id
                  AND current_action.action_type = 'system_notification'
                ORDER BY current_action.parameter_schema_version DESC,
                         current_action.id DESC
                LIMIT 1
             ) = ?",
        );
        parameters.push(text_parameter(action_state.as_str()));
    }
    if let Some(from) = query.filters().from() {
        predicate.push_str(" AND alert.created_at >= ?");
        parameters.push(text_parameter(timestamp_text(from)));
    }
    if let Some(to) = query.filters().to() {
        predicate.push_str(" AND alert.created_at < ?");
        parameters.push(text_parameter(timestamp_text(to)));
    }
    if let Some(cursor) = query.cursor() {
        let created_at = timestamp_text(cursor.created_at());
        predicate.push_str(
            " AND (alert.created_at < ?
                 OR (alert.created_at = ? AND alert.id < ?))",
        );
        parameters.extend([
            text_parameter(created_at.clone()),
            text_parameter(created_at),
            text_parameter(cursor.alert_id().to_string()),
        ]);
    }
    let sql = format!(
        "SELECT alert.id, alert.model_call_id, projection.conversation_id,
                alert.rule_id, alert.rule_version,
                alert.reasoning_output_tokens, alert.pattern_n,
                alert.event_at, alert.created_at,
                provenance.source_id, provenance.source_contract_version,
                provenance.observed_at, provenance.quality
         FROM alerts AS alert
         JOIN model_call_dashboard_projection AS projection
           ON projection.model_call_id = alert.model_call_id
         LEFT JOIN alert_evidence_provenance AS provenance
           ON provenance.alert_id = alert.id
         {predicate}
         ORDER BY alert.created_at DESC, alert.id DESC
         LIMIT ?"
    );
    parameters.push(Value::Integer(
        i64::try_from(query.limit() + 1).expect("Dashboard page limit fits SQLite INTEGER"),
    ));
    let mut rows = {
        let mut statement = connection.prepare(&sql)?;
        statement
            .query_map(params_from_iter(parameters), raw_alert)?
            .collect::<Result<Vec<_>, _>>()?
    };
    let has_more = rows.len() > query.limit();
    if has_more {
        rows.truncate(query.limit());
    }
    let next_cursor = if has_more {
        rows.last()
            .map(|row| -> StorageResult<DashboardAlertCursor> {
                Ok(DashboardAlertCursor::new(
                    parse_timestamp(&row.created_at, "alert.created_at")?,
                    parse_alert_id(&row.id)?,
                ))
            })
            .transpose()?
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(|row| build_dashboard_alert(connection, row))
        .collect::<StorageResult<Vec<_>>>()?;
    Ok(DashboardPage {
        items,
        limit: query.limit(),
        has_more,
        next_cursor,
    })
}

fn raw_alert(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawAlert> {
    Ok(RawAlert {
        id: row.get(0)?,
        model_call_id: row.get(1)?,
        conversation_id: row.get(2)?,
        rule_id: row.get(3)?,
        rule_version: row.get(4)?,
        reasoning_output_tokens: row.get(5)?,
        pattern_n: row.get(6)?,
        event_at: row.get(7)?,
        created_at: row.get(8)?,
        provenance_source_id: row.get(9)?,
        provenance_contract_version: row.get(10)?,
        provenance_observed_at: row.get(11)?,
        provenance_quality: row.get(12)?,
    })
}

fn build_dashboard_alert(connection: &Connection, row: RawAlert) -> StorageResult<DashboardAlert> {
    let alert_id = parse_alert_id(&row.id)?;
    let provenance_source_id =
        row.provenance_source_id
            .ok_or(StorageError::InvalidStoredValue {
                field: "alert.evidence.source_id",
            })?;
    let provenance_contract_version =
        row.provenance_contract_version
            .ok_or(StorageError::InvalidStoredValue {
                field: "alert.evidence.source_contract_version",
            })?;
    let provenance_observed_at =
        row.provenance_observed_at
            .ok_or(StorageError::InvalidStoredValue {
                field: "alert.evidence.observed_at",
            })?;
    let provenance_quality = row
        .provenance_quality
        .ok_or(StorageError::InvalidStoredValue {
            field: "alert.evidence.quality",
        })?;
    Ok(DashboardAlert {
        id: alert_id,
        model_call_id: parse_model_call_id(&row.model_call_id)?,
        conversation_id: row
            .conversation_id
            .map(|value| parse_conversation_id(&value))
            .transpose()?,
        rule_id: BuiltInRuleId::from_str(&row.rule_id)
            .map_err(|_| StorageError::InvalidStoredValue { field: "rule.id" })?,
        rule_version: parse_rule_version(row.rule_version)?,
        evidence: DashboardAlertEvidence {
            reasoning_output_tokens: TokenCount::try_from(row.reasoning_output_tokens)?,
            pattern_n: parse_pattern_n(row.pattern_n)?,
            event_at: parse_timestamp(&row.event_at, "model_call.event_at")?,
            source_id: SourceId::new(provenance_source_id).map_err(|_| {
                StorageError::InvalidStoredValue {
                    field: "alert.evidence.source_id",
                }
            })?,
            source_contract_version: parse_contract_version(provenance_contract_version)?,
            observed_at: parse_timestamp(&provenance_observed_at, "alert.evidence.observed_at")?,
            quality: parse_evidence_quality(&provenance_quality)?,
        },
        action: load_current_notification_action(connection, alert_id)?,
        created_at: parse_timestamp(&row.created_at, "alert.created_at")?,
    })
}

fn load_current_notification_action(
    connection: &Connection,
    alert_id: AlertId,
) -> StorageResult<Option<DashboardAlertAction>> {
    let raw = connection
        .query_row(
            "SELECT action_type, parameter_schema_version, state,
                    created_at, not_before, started_at, completed_at,
                    last_error_code, suppression_reason
             FROM action_outbox
             WHERE alert_id = ?1 AND action_type = 'system_notification'
             ORDER BY parameter_schema_version DESC, id DESC
             LIMIT 1",
            [alert_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                ))
            },
        )
        .optional()?;
    raw.map(
        |(
            action_type,
            schema_version,
            state,
            created_at,
            not_before,
            started_at,
            completed_at,
            error_code,
            suppression_reason,
        )| {
            Ok(DashboardAlertAction {
                action_type: RegisteredActionType::from_str(&action_type).map_err(|_| {
                    StorageError::InvalidStoredValue {
                        field: "action_outbox.action_type",
                    }
                })?,
                parameter_schema_version: NonZeroU16::new(u16::try_from(schema_version).map_err(
                    |_| StorageError::InvalidStoredValue {
                        field: "action_outbox.parameter_schema_version",
                    },
                )?)
                .ok_or(StorageError::InvalidStoredValue {
                    field: "action_outbox.parameter_schema_version",
                })?,
                state: parse_action_state(&state)?,
                created_at: parse_timestamp(&created_at, "action_outbox.created_at")?,
                not_before: parse_timestamp(&not_before, "action_outbox.not_before")?,
                started_at: started_at
                    .map(|value| parse_timestamp(&value, "action_outbox.started_at"))
                    .transpose()?,
                completed_at: completed_at
                    .map(|value| parse_timestamp(&value, "action_outbox.completed_at"))
                    .transpose()?,
                error_code: error_code
                    .map(|value| parse_action_error_code(&value))
                    .transpose()?,
                suppression_reason: suppression_reason
                    .map(|value| parse_action_suppression_reason(&value))
                    .transpose()?,
            })
        },
    )
    .transpose()
}

fn parse_action_state(value: &str) -> StorageResult<ActionLifecycleState> {
    match value {
        "pending" => Ok(ActionLifecycleState::Pending),
        "started" => Ok(ActionLifecycleState::Started),
        "completed" => Ok(ActionLifecycleState::Completed),
        "failed" => Ok(ActionLifecycleState::Failed),
        "suppressed" => Ok(ActionLifecycleState::Suppressed),
        "unavailable" => Ok(ActionLifecycleState::Unavailable),
        _ => Err(StorageError::InvalidStoredValue {
            field: "action_outbox.state",
        }),
    }
}

fn parse_action_error_code(value: &str) -> StorageResult<ActionErrorCode> {
    match value {
        "invalid_parameters" => Ok(ActionErrorCode::InvalidParameters),
        "permission_denied" => Ok(ActionErrorCode::PermissionDenied),
        "notification_unavailable" => Ok(ActionErrorCode::NotificationUnavailable),
        "notification_dispatch_failed" => Ok(ActionErrorCode::NotificationDispatchFailed),
        "indeterminate_after_crash" => Ok(ActionErrorCode::IndeterminateAfterCrash),
        "unknown_action_type" => Ok(ActionErrorCode::UnknownActionType),
        "internal_queue_closed" => Ok(ActionErrorCode::InternalQueueClosed),
        _ => Err(StorageError::InvalidStoredValue {
            field: "action_outbox.last_error_code",
        }),
    }
}

fn parse_action_suppression_reason(value: &str) -> StorageResult<ActionSuppressionReason> {
    match value {
        "notifications_disabled" => Ok(ActionSuppressionReason::NotificationsDisabled),
        "above_threshold" => Ok(ActionSuppressionReason::AboveThreshold),
        "permission_denied" => Ok(ActionSuppressionReason::PermissionDenied),
        "coalesced" => Ok(ActionSuppressionReason::Coalesced),
        "historical_event" => Ok(ActionSuppressionReason::HistoricalEvent),
        "pre_outbox_upgrade" => Ok(ActionSuppressionReason::PreOutboxUpgrade),
        _ => Err(StorageError::InvalidStoredValue {
            field: "action_outbox.suppression_reason",
        }),
    }
}

fn parse_model_call_id(value: &str) -> StorageResult<ModelCallId> {
    ModelCallId::from_str(value).map_err(|_| StorageError::InvalidStoredValue {
        field: "model_call.id",
    })
}

fn parse_conversation_id(value: &str) -> StorageResult<ConversationId> {
    ConversationId::from_str(value).map_err(|_| StorageError::InvalidStoredValue {
        field: "conversation.id",
    })
}

fn parse_alert_id(value: &str) -> StorageResult<AlertId> {
    AlertId::from_str(value).map_err(|_| StorageError::InvalidStoredValue { field: "alert.id" })
}

fn parse_contract_version(value: i64) -> StorageResult<SourceContractVersion> {
    u16::try_from(value)
        .ok()
        .and_then(|value| SourceContractVersion::new(value).ok())
        .ok_or(StorageError::InvalidStoredValue {
            field: "source_contract_version",
        })
}

fn parse_rule_version(value: i64) -> StorageResult<RuleVersion> {
    NonZeroU16::new(
        u16::try_from(value).map_err(|_| StorageError::InvalidStoredValue {
            field: "rule.version",
        })?,
    )
    .map(RuleVersion::new)
    .ok_or(StorageError::InvalidStoredValue {
        field: "rule.version",
    })
}

fn parse_pattern_n(value: i64) -> StorageResult<PatternN> {
    NonZeroU64::new(
        u64::try_from(value).map_err(|_| StorageError::InvalidStoredValue {
            field: "alert.pattern_n",
        })?,
    )
    .map(PatternN::new)
    .ok_or(StorageError::InvalidStoredValue {
        field: "alert.pattern_n",
    })
}

fn parse_optional_token(value: Option<i64>) -> StorageResult<Option<TokenCount>> {
    value
        .map(TokenCount::try_from)
        .transpose()
        .map_err(Into::into)
}

fn parse_temporal_class(value: &str) -> StorageResult<DataOrigin> {
    match value {
        "realtime" => Ok(DataOrigin::Realtime),
        "historical" => Ok(DataOrigin::Historical),
        _ => Err(StorageError::InvalidStoredValue {
            field: "model_call.temporal_class",
        }),
    }
}

fn parse_source_status(value: &str) -> StorageResult<SourceHealthStatus> {
    match value {
        "starting" => Ok(SourceHealthStatus::Starting),
        "healthy" => Ok(SourceHealthStatus::Healthy),
        "degraded" => Ok(SourceHealthStatus::Degraded),
        "unavailable" => Ok(SourceHealthStatus::Unavailable),
        "stopped" => Ok(SourceHealthStatus::Stopped),
        _ => Err(StorageError::InvalidStoredValue {
            field: "source_health.status",
        }),
    }
}

fn parse_nonnegative_count(value: i64, field: &'static str) -> StorageResult<u64> {
    let value = u64::try_from(value).map_err(|_| StorageError::InvalidStoredValue { field })?;
    if value > MAX_DASHBOARD_COUNT {
        return Err(StorageError::DashboardCountOverflow { field });
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use codex_monitor_core::{
        ActionId, FieldProvenance, ModelCallObservation, ModelCallObservationId,
        ModelCallRelationship, ObservationFields, Observed, SourceEventIdentity, SourceIdentity,
        SourcePriority, StableSourceEventId,
    };
    use rusqlite::params;
    use tempfile::{TempDir, tempdir};
    use uuid::Uuid;

    use super::*;
    use crate::migrations::open_and_migrate;
    use crate::{ObservationWrite, StorageConfig, StorageWriter};

    fn at(seconds: i128) -> UtcTimestamp {
        UtcTimestamp::from_unix_timestamp_nanos(seconds * 1_000_000_000).unwrap()
    }

    fn observed<T>(value: T, provenance: &FieldProvenance) -> Observed<T> {
        Observed::new(value, provenance.clone())
    }

    fn insert_conversation(
        connection: &Connection,
        id: ConversationId,
        external_id: &str,
        last_observed_at: UtcTimestamp,
    ) {
        connection
            .execute(
                "INSERT INTO conversations (id, first_observed_at, last_observed_at)
                 VALUES (?1, ?2, ?3)",
                params![
                    id.to_string(),
                    timestamp_text(at(1)),
                    timestamp_text(last_observed_at)
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO conversation_source_keys (
                    telemetry_source_id, source_contract_version, key_kind,
                    key_value, conversation_id, first_observed_at, last_observed_at
                 ) VALUES ('source-a', 1, 'conversation', ?1, ?2, ?3, ?4)",
                params![
                    external_id,
                    id.to_string(),
                    timestamp_text(at(1)),
                    timestamp_text(last_observed_at)
                ],
            )
            .unwrap();
    }

    struct CallSpec<'a> {
        stable_id: &'a str,
        event_at: i128,
        conversation: Option<(ConversationId, &'a str, &'a str)>,
        model: &'a str,
        input: Option<u64>,
        cached_input: Option<u64>,
        output: Option<u64>,
        reasoning: Option<u64>,
        client: &'a str,
        source_id: &'a str,
        origin: DataOrigin,
    }

    async fn seed_call(writer: &StorageWriter, spec: CallSpec<'_>) -> ModelCallId {
        let model_call_id = ModelCallId::new();
        let source = SourceIdentity::new(
            SourceId::new(spec.source_id).unwrap(),
            SourceContractVersion::new(1).unwrap(),
        );
        let provenance = FieldProvenance::new(
            source.clone(),
            at(spec.event_at + 100),
            EvidenceQuality::Direct,
            SourcePriority::new(100),
        );
        let mut fields = ObservationFields {
            model: Some(observed(ModelName::new(spec.model).unwrap(), &provenance)),
            reasoning_effort: Some(observed(ReasoningEffort::Medium, &provenance)),
            input_tokens: spec
                .input
                .map(|value| observed(TokenCount::new(value).unwrap(), &provenance)),
            cached_input_tokens: spec
                .cached_input
                .map(|value| observed(TokenCount::new(value).unwrap(), &provenance)),
            output_tokens: spec
                .output
                .map(|value| observed(TokenCount::new(value).unwrap(), &provenance)),
            reasoning_output_tokens: spec
                .reasoning
                .map(|value| observed(TokenCount::new(value).unwrap(), &provenance)),
            client: Some(observed(ClientName::new(spec.client).unwrap(), &provenance)),
            surface: Some(observed(SurfaceName::new("cli").unwrap(), &provenance)),
            ..ObservationFields::default()
        };
        if let Some((conversation_id, source_id, thread_id)) = spec.conversation {
            fields.relationship = Some(observed(
                ModelCallRelationship::Conversation { conversation_id },
                &provenance,
            ));
            fields.conversation_source_id = Some(observed(
                ConversationSourceId::new(source_id).unwrap(),
                &provenance,
            ));
            fields.thread_source_id = Some(observed(
                ThreadSourceId::new(thread_id).unwrap(),
                &provenance,
            ));
        }
        let stable_id = StableSourceEventId::new(spec.stable_id).unwrap();
        let identity = SourceEventIdentity::from_stable_id(source, &stable_id);
        let observation = ModelCallObservation::new(
            ModelCallObservationId::new(),
            model_call_id,
            identity.clone(),
            observed(at(spec.event_at), &provenance),
            at(spec.event_at + 100),
            spec.origin,
            fields,
        )
        .unwrap();
        writer
            .commit_observation(ObservationWrite::new(identity, observation))
            .await
            .unwrap();
        model_call_id
    }

    async fn seed_query_database() -> (
        TempDir,
        PathBuf,
        ConversationId,
        ConversationId,
        Vec<ModelCallId>,
    ) {
        let directory = tempdir().unwrap();
        let database = directory.path().join("dashboard-query.sqlite3");
        let conversation_a = ConversationId::new();
        let conversation_b = ConversationId::new();
        let connection = open_and_migrate(&database).unwrap();
        insert_conversation(&connection, conversation_a, "conversation-alpha", at(50));
        insert_conversation(&connection, conversation_b, "conversation-beta", at(40));
        drop(connection);

        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        writer.register_builtin_rules().await.unwrap();
        let calls = vec![
            seed_call(
                &writer,
                CallSpec {
                    stable_id: "call-one",
                    event_at: 10,
                    conversation: Some((conversation_a, "conversation-alpha", "thread-alpha")),
                    model: "gpt-a",
                    input: Some(100),
                    cached_input: Some(20),
                    output: Some(40),
                    reasoning: Some(10),
                    client: "cli",
                    source_id: "source-a",
                    origin: DataOrigin::Realtime,
                },
            )
            .await,
            seed_call(
                &writer,
                CallSpec {
                    stable_id: "call-two",
                    event_at: 20,
                    conversation: Some((conversation_b, "conversation-beta", "thread-beta")),
                    model: "GPT-a",
                    input: Some(300),
                    cached_input: Some(30),
                    output: Some(20),
                    reasoning: Some(5),
                    client: "desktop",
                    source_id: "source-b",
                    origin: DataOrigin::Realtime,
                },
            )
            .await,
            seed_call(
                &writer,
                CallSpec {
                    stable_id: "call-three",
                    event_at: 30,
                    conversation: Some((conversation_a, "conversation-alpha", "thread-alpha")),
                    model: "gpt-a",
                    input: None,
                    cached_input: None,
                    output: Some(100),
                    reasoning: Some(50),
                    client: "cli",
                    source_id: "source-a",
                    origin: DataOrigin::Historical,
                },
            )
            .await,
            seed_call(
                &writer,
                CallSpec {
                    stable_id: "call-four",
                    event_at: 40,
                    conversation: None,
                    model: "gpt-c",
                    input: Some(200),
                    cached_input: None,
                    output: None,
                    reasoning: None,
                    client: "cli",
                    source_id: "source-c",
                    origin: DataOrigin::Realtime,
                },
            )
            .await,
        ];
        writer.shutdown().await.unwrap();
        (directory, database, conversation_a, conversation_b, calls)
    }

    fn insert_alert(
        database: &Path,
        model_call_id: ModelCallId,
        event_at: UtcTimestamp,
        created_at: UtcTimestamp,
        source_id: &str,
        with_actions: bool,
    ) -> AlertId {
        let connection = Connection::open(database).unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        let evaluation_id = Uuid::now_v7().to_string();
        let alert_id = AlertId::new();
        connection
            .execute(
                "INSERT INTO rule_evaluations (
                    id, rule_id, rule_version, model_call_id, matched,
                    reasoning_output_tokens, pattern_n, event_at,
                    source_quality, evaluated_at
                 ) VALUES (
                    ?1, 'suspected-reasoning-limit-pattern', 1, ?2, 1,
                    516, 1, ?3, 'direct', ?4
                 )",
                params![
                    evaluation_id,
                    model_call_id.to_string(),
                    timestamp_text(event_at),
                    timestamp_text(created_at)
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO alerts (
                    id, evaluation_id, model_call_id, rule_id, rule_version,
                    reasoning_output_tokens, pattern_n, event_at,
                    source_quality, created_at
                 ) VALUES (
                    ?1, ?2, ?3, 'suspected-reasoning-limit-pattern', 1,
                    516, 1, ?4, 'direct', ?5
                 )",
                params![
                    alert_id.to_string(),
                    evaluation_id,
                    model_call_id.to_string(),
                    timestamp_text(event_at),
                    timestamp_text(created_at)
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO alert_evidence_provenance (
                    alert_id, source_id, source_contract_version, observed_at, quality
                 ) VALUES (?1, ?2, 1, ?3, 'direct')",
                params![alert_id.to_string(), source_id, timestamp_text(created_at)],
            )
            .unwrap();
        if with_actions {
            for (schema_version, state) in [(1_i64, "pending"), (2_i64, "suppressed")] {
                let action_id = ActionId::new();
                let (completed_at, suppression_reason) = if state == "suppressed" {
                    (Some(timestamp_text(created_at)), Some("above_threshold"))
                } else {
                    (None, None)
                };
                connection
                    .execute(
                        "INSERT INTO action_outbox (
                            id, alert_id, action_type, parameter_schema_version,
                            idempotency_key_version, idempotency_key_bytes, state,
                            reasoning_output_tokens, pattern_n, created_at, not_before,
                            completed_at, suppression_reason
                         ) VALUES (
                            ?1, ?2, 'system_notification', ?3, 1, ?4, ?5,
                            516, 1, ?6, ?6, ?7, ?8
                         )",
                        params![
                            action_id.to_string(),
                            alert_id.to_string(),
                            schema_version,
                            vec![u8::try_from(schema_version).unwrap(); 32],
                            state,
                            timestamp_text(created_at),
                            completed_at,
                            suppression_reason,
                        ],
                    )
                    .unwrap();
            }
        }
        alert_id
    }

    fn ids(page: &DashboardPage<DashboardModelCall, DashboardModelCallCursor>) -> Vec<ModelCallId> {
        page.items.iter().map(|item| item.id).collect()
    }

    fn model_query(
        filters: ModelCallFilters,
        sort: ModelCallSort,
        limit: usize,
        cursor: Option<DashboardModelCallCursor>,
    ) -> ModelCallPageQuery {
        ModelCallPageQuery::new(limit, filters, sort, cursor).unwrap()
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn model_call_pages_cover_sorts_filters_cursors_and_large_resolver_sets() {
        let (_directory, database, conversation_a, _conversation_b, calls) =
            seed_query_database().await;
        let alert_id = insert_alert(&database, calls[0], at(10), at(60), "source-a", false);
        assert_ne!(alert_id.to_string(), "");
        let reader = StorageReader::new(database.clone());

        for (sort, expected) in [
            (
                ModelCallSort::EventAtDesc,
                vec![calls[3], calls[2], calls[1], calls[0]],
            ),
            (
                ModelCallSort::EventAtAsc,
                vec![calls[0], calls[1], calls[2], calls[3]],
            ),
            (
                ModelCallSort::InputTokensDesc,
                vec![calls[1], calls[3], calls[0], calls[2]],
            ),
            (
                ModelCallSort::OutputTokensDesc,
                vec![calls[2], calls[0], calls[1], calls[3]],
            ),
            (
                ModelCallSort::ReasoningTokensDesc,
                vec![calls[2], calls[0], calls[1], calls[3]],
            ),
            (
                ModelCallSort::TotalTokensDesc,
                vec![calls[1], calls[0], calls[3], calls[2]],
            ),
        ] {
            let page = reader
                .model_call_page(&model_query(ModelCallFilters::default(), sort, 100, None))
                .unwrap();
            assert_eq!(ids(&page), expected, "unexpected order for {sort:?}");
            assert!(!page.has_more);
            assert!(page.next_cursor.is_none());
        }

        let first = reader
            .model_call_page(&model_query(
                ModelCallFilters::default(),
                ModelCallSort::EventAtDesc,
                2,
                None,
            ))
            .unwrap();
        assert!(first.has_more);
        let second = reader
            .model_call_page(&model_query(
                ModelCallFilters::default(),
                ModelCallSort::EventAtDesc,
                2,
                first.next_cursor,
            ))
            .unwrap();
        assert_eq!(ids(&first), vec![calls[3], calls[2]]);
        assert_eq!(ids(&second), vec![calls[1], calls[0]]);
        assert!(!second.has_more);

        let token_first = reader
            .model_call_page(&model_query(
                ModelCallFilters::default(),
                ModelCallSort::InputTokensDesc,
                2,
                None,
            ))
            .unwrap();
        let token_second = reader
            .model_call_page(&model_query(
                ModelCallFilters::default(),
                ModelCallSort::InputTokensDesc,
                2,
                token_first.next_cursor,
            ))
            .unwrap();
        assert_eq!(ids(&token_first), vec![calls[1], calls[3]]);
        assert_eq!(ids(&token_second), vec![calls[0], calls[2]]);

        let exact_model = ModelCallFilters::new(
            None,
            Some("gpt-a".into()),
            None,
            None,
            None,
            None,
            AlertMatchFilter::Any,
            None,
        )
        .unwrap();
        assert_eq!(
            ids(&reader
                .model_call_page(&model_query(
                    exact_model,
                    ModelCallSort::EventAtDesc,
                    100,
                    None
                ))
                .unwrap()),
            vec![calls[2], calls[0]]
        );
        let uppercase_model = ModelCallFilters::new(
            None,
            Some("GPT-a".into()),
            None,
            None,
            None,
            None,
            AlertMatchFilter::Any,
            None,
        )
        .unwrap();
        assert_eq!(
            ids(&reader
                .model_call_page(&model_query(
                    uppercase_model,
                    ModelCallSort::EventAtDesc,
                    100,
                    None
                ))
                .unwrap()),
            vec![calls[1]]
        );

        let mut resolved = (0..129)
            .map(|index| format!("resolved-{index}"))
            .collect::<Vec<_>>();
        resolved.push("conversation-alpha".into());
        let selector = ConversationSelector::new(None, resolved).unwrap();
        let filtered = ModelCallFilters::new(
            Some(selector),
            Some("gpt-a".into()),
            Some(SourceId::new("source-a").unwrap()),
            Some("cli".into()),
            Some(at(10)),
            Some(at(31)),
            AlertMatchFilter::Matched,
            Some(DataOrigin::Realtime),
        )
        .unwrap();
        assert_eq!(
            ids(&reader
                .model_call_page(&model_query(
                    filtered,
                    ModelCallSort::EventAtDesc,
                    100,
                    None
                ))
                .unwrap()),
            vec![calls[0]]
        );

        let historical = ModelCallFilters::new(
            Some(ConversationSelector::new(Some(conversation_a.to_string()), vec![]).unwrap()),
            None,
            None,
            None,
            None,
            None,
            AlertMatchFilter::None,
            Some(DataOrigin::Historical),
        )
        .unwrap();
        let page = reader
            .model_call_page(&model_query(
                historical,
                ModelCallSort::EventAtDesc,
                100,
                None,
            ))
            .unwrap();
        assert_eq!(ids(&page), vec![calls[2]]);
        assert_eq!(
            page.items[0].source_ids,
            [SourceId::new("source-a").unwrap()]
        );
        assert!(
            page.items[0]
                .field_sources
                .iter()
                .any(|source| source.field == CanonicalFieldName::EventAt)
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn summary_conversation_and_alert_queries_share_complete_filter_semantics() {
        let (_directory, database, conversation_a, conversation_b, calls) =
            seed_query_database().await;
        let first_alert = insert_alert(&database, calls[0], at(10), at(60), "source-a", true);
        let second_alert = insert_alert(&database, calls[1], at(20), at(70), "source-b", false);
        let reader = StorageReader::new(database.clone());

        let summary = reader
            .model_call_summary(&ModelCallFilters::default())
            .unwrap();
        assert_eq!(summary.call_count, 4);
        assert_eq!(summary.alert_count, 2);
        assert_eq!(summary.input.known_call_count, 3);
        assert!(!summary.input.complete);
        assert_eq!(summary.output.known_call_count, 3);
        assert_eq!(summary.total.known_call_count, 2);
        let empty = ModelCallFilters::new(
            None,
            Some("does/not/exist".into()),
            None,
            None,
            None,
            None,
            AlertMatchFilter::Any,
            None,
        )
        .unwrap();
        let empty = reader.model_call_summary(&empty).unwrap();
        assert_eq!(empty.call_count, 0);
        assert!(empty.input.complete);
        assert_eq!(empty.input.value, TokenCount::new(0).unwrap());

        let conversation_page = reader
            .conversation_page(
                &ConversationPageQuery::new(1, None, None, None, None, None).unwrap(),
            )
            .unwrap();
        assert!(conversation_page.has_more);
        assert_eq!(conversation_page.items[0].id, conversation_a);
        assert_eq!(conversation_page.items[0].model_call_count, 2);
        assert_eq!(conversation_page.items[0].alert_count, 1);
        let next = reader
            .conversation_page(
                &ConversationPageQuery::new(
                    1,
                    None,
                    None,
                    None,
                    None,
                    conversation_page.next_cursor,
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(next.items[0].id, conversation_b);

        let source_filtered = reader
            .conversation_page(
                &ConversationPageQuery::new(
                    100,
                    Some(
                        ConversationSelector::new(None, vec!["conversation-beta".into()]).unwrap(),
                    ),
                    Some(SourceId::new("source-b").unwrap()),
                    Some(at(120)),
                    Some(at(121)),
                    None,
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(source_filtered.items.len(), 1);
        assert_eq!(source_filtered.items[0].id, conversation_b);

        let alert_page = reader
            .alert_page(&AlertPageQuery::new(1, AlertFilters::default(), None).unwrap())
            .unwrap();
        assert!(alert_page.has_more);
        assert_eq!(alert_page.items[0].id, second_alert);
        let next_alert = reader
            .alert_page(
                &AlertPageQuery::new(1, AlertFilters::default(), alert_page.next_cursor).unwrap(),
            )
            .unwrap();
        assert_eq!(next_alert.items[0].id, first_alert);

        let stale_action_filtered = AlertFilters::new(
            Some(ConversationSelector::new(Some("conversation-alpha".into()), vec![]).unwrap()),
            Some(calls[0].to_string()),
            Some(ActionLifecycleState::Pending),
            Some(at(60)),
            Some(at(61)),
        )
        .unwrap();
        let stale_action_page = reader
            .alert_page(&AlertPageQuery::new(100, stale_action_filtered, None).unwrap())
            .unwrap();
        assert!(stale_action_page.items.is_empty());

        let action_filtered = AlertFilters::new(
            Some(ConversationSelector::new(Some("conversation-alpha".into()), vec![]).unwrap()),
            Some(calls[0].to_string()),
            Some(ActionLifecycleState::Suppressed),
            Some(at(60)),
            Some(at(61)),
        )
        .unwrap();
        let action_page = reader
            .alert_page(&AlertPageQuery::new(100, action_filtered, None).unwrap())
            .unwrap();
        assert_eq!(action_page.items.len(), 1);
        let action = action_page.items[0].action.as_ref().unwrap();
        assert_eq!(action.parameter_schema_version.get(), 2);
        assert_eq!(action.state, ActionLifecycleState::Suppressed);
        assert_eq!(
            action_page.items[0].evidence.source_id,
            SourceId::new("source-a").unwrap()
        );

        Connection::open(&database)
            .unwrap()
            .execute(
                "UPDATE model_call_dashboard_projection
                 SET reasoning_output_tokens = 517
                 WHERE model_call_id = ?1",
                [calls[0].to_string()],
            )
            .unwrap();
        let after_projection_change = reader
            .alert_page(
                &AlertPageQuery::new(
                    100,
                    AlertFilters::new(None, Some(calls[0].to_string()), None, None, None).unwrap(),
                    None,
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            after_projection_change.items[0]
                .evidence
                .reasoning_output_tokens,
            TokenCount::new(516).unwrap()
        );

        let connection = Connection::open(&database).unwrap();
        for model_call_id in [calls[0], calls[1]] {
            connection
                .execute(
                    "UPDATE model_call_dashboard_projection
                     SET input_tokens = 1000000000000000,
                         output_tokens = NULL
                     WHERE model_call_id = ?1",
                    [model_call_id.to_string()],
                )
                .unwrap();
        }
        assert!(matches!(
            reader.model_call_summary(&ModelCallFilters::default()),
            Err(StorageError::DashboardAggregateOverflow)
        ));
    }

    #[test]
    fn source_health_list_is_batched_stable_and_closed_at_thirty_two() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("sources.sqlite3");
        let connection = open_and_migrate(&database).unwrap();
        for index in (0..32).rev() {
            let source_id = format!("source-{index:02}");
            connection
                .execute(
                    "INSERT INTO source_health (
                        source_id, source_contract_version, status, updated_at
                     ) VALUES (?1, 1, 'healthy', ?2)",
                    params![source_id, timestamp_text(at(i128::from(index)))],
                )
                .unwrap();
            for field_id in ["model_call.event_at", "model_call.model"] {
                connection
                    .execute(
                        "INSERT INTO source_health_supported_fields (
                            source_id, source_contract_version, field_id
                         ) VALUES (?1, 1, ?2)",
                        params![source_id, field_id],
                    )
                    .unwrap();
            }
        }
        drop(connection);
        let reader = StorageReader::new(database.clone());
        let sources = reader.source_health_list().unwrap();
        assert_eq!(sources.len(), 32);
        assert_eq!(sources[0].source_id.as_str(), "source-00");
        assert_eq!(sources[31].source_id.as_str(), "source-31");
        assert!(
            sources
                .iter()
                .all(|source| source.supported_fields.len() == 2)
        );

        let connection = Connection::open(&database).unwrap();
        connection
            .execute(
                "INSERT INTO source_health (
                    source_id, source_contract_version, status, updated_at
                 ) VALUES ('source-32', 1, 'healthy', ?1)",
                [timestamp_text(at(32))],
            )
            .unwrap();
        assert!(matches!(
            reader.source_health_list(),
            Err(StorageError::TooManySourceHealthRows { max: 32 })
        ));
    }

    #[test]
    fn query_constructors_enforce_bounds_without_echoing_values() {
        assert!(matches!(
            ModelCallPageQuery::new(
                0,
                ModelCallFilters::default(),
                ModelCallSort::EventAtDesc,
                None
            ),
            Err(StorageError::InvalidDashboardQueryLimit { max: 100 })
        ));
        assert!(matches!(
            ModelCallPageQuery::new(
                101,
                ModelCallFilters::default(),
                ModelCallSort::EventAtDesc,
                None
            ),
            Err(StorageError::InvalidDashboardQueryLimit { max: 100 })
        ));
        assert!(matches!(
            ModelCallFilters::new(
                None,
                None,
                None,
                None,
                Some(at(2)),
                Some(at(2)),
                AlertMatchFilter::Any,
                None
            ),
            Err(StorageError::InvalidDashboardTimeRange)
        ));
        assert!(ConversationSelector::new(None, vec!["x".into(); 129]).is_ok());
        assert!(matches!(
            ConversationSelector::new(
                None,
                (0..=MAX_RESOLVED_CONVERSATION_IDENTIFIERS)
                    .map(|index| format!("candidate-{index}"))
                    .collect()
            ),
            Err(StorageError::TooManyResolvedConversationIdentifiers { max: 50_000 })
        ));
        assert!(
            ModelCallFilters::new(
                None,
                Some("模".repeat(128)),
                None,
                None,
                None,
                None,
                AlertMatchFilter::Any,
                None
            )
            .is_ok()
        );
        assert!(
            AlertFilters::new(None, Some("valid-but-not-a-uuid".into()), None, None, None).is_ok()
        );
        assert!(matches!(
            parse_nonnegative_count(
                i64::try_from(MAX_DASHBOARD_COUNT + 1).unwrap(),
                "synthetic.count"
            ),
            Err(StorageError::DashboardCountOverflow {
                field: "synthetic.count"
            })
        ));
    }
}
