use codex_monitor_core::{
    ActionErrorCode, ActionLifecycleState, ActionSuppressionReason, AlertId, BuiltInRuleId,
    ClientName, ConversationId, ConversationSourceId, DataOrigin, EvidenceQuality, FieldId,
    ModelCallId, ModelName, PatternN, ReasoningEffort, RuleVersion, SafeErrorCode,
    SourceContractVersion, SourceHealthStatus, SourceId, SurfaceName, ThreadSourceId, TokenCount,
    TurnId, UtcTimestamp,
};
use serde::Serialize;

use crate::ports::{
    DashboardAlert, DashboardAlertAction, DashboardAlertEvidence, DashboardAlertState,
    DashboardCanonicalField, DashboardConversation, DashboardConversationRef, DashboardFieldSource,
    DashboardModelCall, DashboardModelCallSummary, DashboardPage, DashboardReceiverBindState,
    DashboardReceiverSnapshot, DashboardSourceHealth, DashboardTokenAggregate, DashboardTokenUsage,
};

#[derive(Serialize)]
pub(crate) struct PageResponse<T> {
    items: Vec<T>,
    page: PageMetadata,
}

#[derive(Serialize)]
struct PageMetadata {
    limit: usize,
    has_more: bool,
    next_cursor: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct ModelCallResponse {
    id: ModelCallId,
    event_at: UtcTimestamp,
    conversation: Option<ConversationRefResponse>,
    turn_id: Option<TurnId>,
    model: Option<ModelName>,
    reasoning_effort: Option<ReasoningEffort>,
    tokens: TokenUsageResponse,
    source_ids: Vec<SourceId>,
    client: Option<ClientName>,
    surface: Option<SurfaceName>,
    temporal_class: DataOrigin,
    alert_state: AlertStateResponse,
    alert_count: u64,
    field_sources: Vec<FieldSourceResponse>,
}

#[derive(Serialize)]
struct ConversationRefResponse {
    id: ConversationId,
    source_id: Option<ConversationSourceId>,
    thread_id: Option<ThreadSourceId>,
    resolved_display_name: Option<String>,
}

#[derive(Serialize)]
struct TokenUsageResponse {
    input: Option<TokenCount>,
    cached_input: Option<TokenCount>,
    output: Option<TokenCount>,
    reasoning: Option<TokenCount>,
    total: Option<TokenCount>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum AlertStateResponse {
    None,
    Matched,
}

#[derive(Serialize)]
struct FieldSourceResponse {
    field: CanonicalFieldResponse,
    source_id: SourceId,
    source_contract_version: SourceContractVersion,
    observed_at: UtcTimestamp,
    quality: EvidenceQuality,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum CanonicalFieldResponse {
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

#[derive(Serialize)]
pub(crate) struct ConversationResponse {
    id: ConversationId,
    source_id: Option<ConversationSourceId>,
    thread_id: Option<ThreadSourceId>,
    resolved_display_name: Option<String>,
    first_observed_at: UtcTimestamp,
    last_observed_at: UtcTimestamp,
    model_call_count: u64,
    alert_count: u64,
}

#[derive(Serialize)]
pub(crate) struct ModelCallSummaryResponse {
    call_count: u64,
    alert_count: u64,
    input: TokenAggregateResponse,
    cached_input: TokenAggregateResponse,
    output: TokenAggregateResponse,
    reasoning: TokenAggregateResponse,
    total: TokenAggregateResponse,
}

#[derive(Serialize)]
struct TokenAggregateResponse {
    value: TokenCount,
    known_call_count: u64,
    total_call_count: u64,
    complete: bool,
}

#[derive(Serialize)]
pub(crate) struct SourceListResponse {
    items: Vec<SourceHealthResponse>,
}

#[derive(Serialize)]
pub(crate) struct SourceHealthResponse {
    source_id: SourceId,
    source_contract_version: SourceContractVersion,
    status: SourceHealthStatus,
    receiver: Option<ReceiverStateResponse>,
    last_accepted_at: Option<UtcTimestamp>,
    last_error_code: Option<SafeErrorCode>,
    last_error_occurred_at: Option<UtcTimestamp>,
    updated_at: UtcTimestamp,
    supported_fields: Vec<FieldId>,
}

#[derive(Serialize)]
struct ReceiverStateResponse {
    host: &'static str,
    port: u16,
    bind_state: ReceiverBindStateResponse,
    retry_supported: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum ReceiverBindStateResponse {
    Starting,
    Bound,
    Retrying,
    Conflict,
    Stopped,
}

#[derive(Serialize)]
pub(crate) struct AlertResponse {
    id: AlertId,
    model_call_id: ModelCallId,
    conversation_id: Option<ConversationId>,
    rule_id: BuiltInRuleId,
    rule_version: RuleVersion,
    evidence: AlertEvidenceResponse,
    action: Option<AlertActionResponse>,
    created_at: UtcTimestamp,
}

#[derive(Serialize)]
struct AlertEvidenceResponse {
    reasoning_output_tokens: TokenCount,
    pattern_n: PatternN,
    event_at: UtcTimestamp,
    source_id: SourceId,
    source_contract_version: SourceContractVersion,
    observed_at: UtcTimestamp,
    quality: EvidenceQuality,
}

#[derive(Serialize)]
struct AlertActionResponse {
    action_type: &'static str,
    state: ActionLifecycleState,
    created_at: UtcTimestamp,
    not_before: UtcTimestamp,
    started_at: Option<UtcTimestamp>,
    completed_at: Option<UtcTimestamp>,
    error_code: Option<ActionErrorCode>,
    suppression_reason: Option<ActionSuppressionReason>,
}

pub(crate) fn model_call_page<C>(
    page: DashboardPage<DashboardModelCall, C>,
    next_cursor: Option<String>,
) -> Result<PageResponse<ModelCallResponse>, ()> {
    let metadata = page_metadata(page.limit, page.has_more, next_cursor)?;
    Ok(PageResponse {
        items: page.items.into_iter().map(model_call).collect(),
        page: metadata,
    })
}

pub(crate) fn conversation_page<C>(
    page: DashboardPage<DashboardConversation, C>,
    next_cursor: Option<String>,
) -> Result<PageResponse<ConversationResponse>, ()> {
    let metadata = page_metadata(page.limit, page.has_more, next_cursor)?;
    let items = page
        .items
        .into_iter()
        .map(|item| ConversationResponse {
            id: item.id,
            source_id: item.source_id,
            thread_id: item.thread_id,
            resolved_display_name: item.resolved_display_name,
            first_observed_at: item.first_observed_at,
            last_observed_at: item.last_observed_at,
            model_call_count: item.model_call_count,
            alert_count: item.alert_count,
        })
        .collect();
    Ok(PageResponse {
        items,
        page: metadata,
    })
}

pub(crate) fn summary(value: DashboardModelCallSummary) -> ModelCallSummaryResponse {
    ModelCallSummaryResponse {
        call_count: value.call_count,
        alert_count: value.alert_count,
        input: token_aggregate(value.input),
        cached_input: token_aggregate(value.cached_input),
        output: token_aggregate(value.output),
        reasoning: token_aggregate(value.reasoning),
        total: token_aggregate(value.total),
    }
}

pub(crate) fn sources(values: Vec<DashboardSourceHealth>) -> SourceListResponse {
    SourceListResponse {
        items: values.into_iter().map(source_health).collect(),
    }
}

pub(crate) fn source_health(value: DashboardSourceHealth) -> SourceHealthResponse {
    SourceHealthResponse {
        source_id: value.source_id,
        source_contract_version: value.source_contract_version,
        status: value.status,
        receiver: value.receiver.map(receiver_state),
        last_accepted_at: value.last_accepted_at,
        last_error_code: value.last_error_code,
        last_error_occurred_at: value.last_error_occurred_at,
        updated_at: value.updated_at,
        supported_fields: value.supported_fields,
    }
}

pub(crate) fn alert_page<C>(
    page: DashboardPage<DashboardAlert, C>,
    next_cursor: Option<String>,
) -> Result<PageResponse<AlertResponse>, ()> {
    let metadata = page_metadata(page.limit, page.has_more, next_cursor)?;
    Ok(PageResponse {
        items: page.items.into_iter().map(alert).collect(),
        page: metadata,
    })
}

fn page_metadata(
    limit: usize,
    has_more: bool,
    next_cursor: Option<String>,
) -> Result<PageMetadata, ()> {
    if has_more != next_cursor.is_some() {
        return Err(());
    }
    Ok(PageMetadata {
        limit,
        has_more,
        next_cursor,
    })
}

pub(crate) fn model_call(value: DashboardModelCall) -> ModelCallResponse {
    ModelCallResponse {
        id: value.id,
        event_at: value.event_at,
        conversation: value.conversation.map(conversation_ref),
        turn_id: value.turn_id,
        model: value.model,
        reasoning_effort: value.reasoning_effort,
        tokens: token_usage(value.tokens),
        source_ids: value.source_ids,
        client: value.client,
        surface: value.surface,
        temporal_class: value.temporal_class,
        alert_state: match value.alert_state {
            DashboardAlertState::None => AlertStateResponse::None,
            DashboardAlertState::Matched => AlertStateResponse::Matched,
        },
        alert_count: value.alert_count,
        field_sources: value.field_sources.into_iter().map(field_source).collect(),
    }
}

fn conversation_ref(value: DashboardConversationRef) -> ConversationRefResponse {
    ConversationRefResponse {
        id: value.id,
        source_id: value.source_id,
        thread_id: value.thread_id,
        resolved_display_name: value.resolved_display_name,
    }
}

const fn token_usage(value: DashboardTokenUsage) -> TokenUsageResponse {
    TokenUsageResponse {
        input: value.input,
        cached_input: value.cached_input,
        output: value.output,
        reasoning: value.reasoning,
        total: value.total,
    }
}

const fn token_aggregate(value: DashboardTokenAggregate) -> TokenAggregateResponse {
    TokenAggregateResponse {
        value: value.value,
        known_call_count: value.known_call_count,
        total_call_count: value.total_call_count,
        complete: value.complete,
    }
}

fn field_source(value: DashboardFieldSource) -> FieldSourceResponse {
    FieldSourceResponse {
        field: match value.field {
            DashboardCanonicalField::Conversation => CanonicalFieldResponse::Conversation,
            DashboardCanonicalField::Turn => CanonicalFieldResponse::Turn,
            DashboardCanonicalField::EventAt => CanonicalFieldResponse::EventAt,
            DashboardCanonicalField::Model => CanonicalFieldResponse::Model,
            DashboardCanonicalField::ReasoningEffort => CanonicalFieldResponse::ReasoningEffort,
            DashboardCanonicalField::InputTokens => CanonicalFieldResponse::InputTokens,
            DashboardCanonicalField::CachedInputTokens => CanonicalFieldResponse::CachedInputTokens,
            DashboardCanonicalField::OutputTokens => CanonicalFieldResponse::OutputTokens,
            DashboardCanonicalField::ReasoningOutputTokens => {
                CanonicalFieldResponse::ReasoningOutputTokens
            }
            DashboardCanonicalField::Client => CanonicalFieldResponse::Client,
            DashboardCanonicalField::Surface => CanonicalFieldResponse::Surface,
        },
        source_id: value.source_id,
        source_contract_version: value.source_contract_version,
        observed_at: value.observed_at,
        quality: value.quality,
    }
}

fn receiver_state(value: DashboardReceiverSnapshot) -> ReceiverStateResponse {
    ReceiverStateResponse {
        host: "127.0.0.1",
        port: value.port,
        bind_state: match value.bind_state {
            DashboardReceiverBindState::Starting => ReceiverBindStateResponse::Starting,
            DashboardReceiverBindState::Bound => ReceiverBindStateResponse::Bound,
            DashboardReceiverBindState::Retrying => ReceiverBindStateResponse::Retrying,
            DashboardReceiverBindState::Conflict => ReceiverBindStateResponse::Conflict,
            DashboardReceiverBindState::Stopped => ReceiverBindStateResponse::Stopped,
        },
        retry_supported: value.retry_supported,
    }
}

pub(crate) fn alert(value: DashboardAlert) -> AlertResponse {
    AlertResponse {
        id: value.id,
        model_call_id: value.model_call_id,
        conversation_id: value.conversation_id,
        rule_id: value.rule_id,
        rule_version: value.rule_version,
        evidence: alert_evidence(value.evidence),
        action: value.action.map(alert_action),
        created_at: value.created_at,
    }
}

fn alert_evidence(value: DashboardAlertEvidence) -> AlertEvidenceResponse {
    AlertEvidenceResponse {
        reasoning_output_tokens: value.reasoning_output_tokens,
        pattern_n: value.pattern_n,
        event_at: value.event_at,
        source_id: value.source_id,
        source_contract_version: value.source_contract_version,
        observed_at: value.observed_at,
        quality: value.quality,
    }
}

fn alert_action(value: DashboardAlertAction) -> AlertActionResponse {
    AlertActionResponse {
        action_type: "system_notification",
        state: value.state,
        created_at: value.created_at,
        not_before: value.not_before,
        started_at: value.started_at,
        completed_at: value.completed_at,
        error_code: value.error_code,
        suppression_reason: value.suppression_reason,
    }
}
