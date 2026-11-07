//! adapter crate 之间的组合桥接。

use std::sync::Arc;

use codex_monitor_core::{SourceExtraction, SourceHealth};
use codex_monitor_dashboard::{
    AlertMatchInput, DashboardAlert, DashboardAlertAction, DashboardAlertCursor,
    DashboardAlertEvidence, DashboardAlertPageRequest, DashboardAlertState,
    DashboardCanonicalField, DashboardCapabilityPort, DashboardCheckpointCapability,
    DashboardConversation, DashboardConversationCursor, DashboardConversationPageRequest,
    DashboardConversationRef, DashboardEventChange, DashboardEventPort, DashboardEventReceive,
    DashboardEventRecord, DashboardEventSubscribeErrorCode, DashboardEventSubscription,
    DashboardEventSubscriptionStart, DashboardFieldSource, DashboardFuture, DashboardLocale,
    DashboardModelCall, DashboardModelCallCursor, DashboardModelCallPageRequest,
    DashboardModelCallSummary, DashboardPage, DashboardQueryErrorCode, DashboardQueryPort,
    DashboardReceiverBindState, DashboardReceiverSnapshot, DashboardRetentionPolicy,
    DashboardRetryErrorCode, DashboardRuleCapability, DashboardRuntimeSnapshot, DashboardSettings,
    DashboardSettingsDocument, DashboardSettingsErrorCode, DashboardSettingsPort,
    DashboardSourceHealth, DashboardStreamControlReason, DashboardSummaryRequest,
    DashboardTokenAggregate, DashboardTokenUsage, ModelCallSortInput, ModelFiltersInput,
};
use codex_monitor_storage as storage;
use codex_monitor_storage::{
    AlertFilters, AlertMatchFilter, AlertPageQuery, AppLocale, AppSettings, ConversationPageQuery,
    ConversationSelector, CurrentUserStorage, DashboardModelCallCursor as StorageModelCallCursor,
    ModelCallFilters, ModelCallPageQuery, ModelCallSort, StorageChange, StorageChangeFeed,
    StorageChangeSubscription, StorageError, StorageReader, StorageWriter, StoredSettingsDocument,
    StoredSourceHealth, StreamControlReason, StreamReceive, StreamSubscribeError,
    StreamSubscriptionStart,
};
use codex_monitor_telemetry::{
    CheckpointCapability, ObservationSink, ObservationSinkError, ObservationSinkErrorCode,
    OtlpReceiverBindState, OtlpReceiverControl, OtlpReceiverRetryError, OtlpReceiverSnapshot,
    OtlpSourceHealthMonitor, SessionIndexResolver,
};

use crate::{SettingsRuntime, SettingsRuntimeError};

pub(crate) struct StorageObservationSink {
    writer: StorageWriter,
}

impl StorageObservationSink {
    pub(crate) fn new(writer: StorageWriter) -> Self {
        Self { writer }
    }
}

impl ObservationSink for StorageObservationSink {
    fn commit(
        &self,
        extraction: SourceExtraction,
        accepted_health: SourceHealth,
    ) -> DashboardFuture<'_, Result<(), ObservationSinkError>> {
        Box::pin(async move {
            self.writer
                .commit_source_batch(extraction, StoredSourceHealth::from(&accepted_health))
                .await
                .map(|_| ())
                .map_err(|error| observation_sink_error(&error))
        })
    }

    fn store_health(
        &self,
        health: SourceHealth,
    ) -> DashboardFuture<'_, Result<(), ObservationSinkError>> {
        Box::pin(async move {
            self.writer
                .upsert_source_health(StoredSourceHealth::from(&health))
                .await
                .map_err(|error| observation_sink_error(&error))
        })
    }
}

fn observation_sink_error(error: &StorageError) -> ObservationSinkError {
    let code = match error {
        StorageError::CanonicalIdentityCollision | StorageError::ConversationIdentityCollision => {
            ObservationSinkErrorCode::IdentityConflict
        }
        StorageError::Domain(_)
        | StorageError::UnregisteredTelemetryField
        | StorageError::ObservationModelCallMismatch => ObservationSinkErrorCode::DomainRejected,
        _ => ObservationSinkErrorCode::Unavailable,
    };
    ObservationSinkError::new(code)
}

/// 将 Dashboard 消费方端口连接到 storage、telemetry 与 settings runtime。
pub(crate) struct DashboardAdapter {
    storage: StorageReader,
    settings: SettingsRuntime,
    session_index: Arc<SessionIndexResolver>,
    receiver: OtlpReceiverControl,
    source_health: OtlpSourceHealthMonitor,
    events: StorageChangeFeed,
}

impl DashboardAdapter {
    pub(crate) fn new(
        storage: &CurrentUserStorage,
        settings: SettingsRuntime,
        session_index: Arc<SessionIndexResolver>,
        receiver: OtlpReceiverControl,
        source_health: OtlpSourceHealthMonitor,
    ) -> Self {
        Self {
            storage: storage.reader().clone(),
            settings,
            session_index,
            receiver,
            source_health,
            events: storage.writer().change_feed(),
        }
    }

    fn runtime_snapshot(&self) -> DashboardRuntimeSnapshot {
        let source = self.source_health.snapshot();
        let receiver = receiver_snapshot(self.receiver.snapshot());
        DashboardRuntimeSnapshot {
            source: source_health(StoredSourceHealth::from(&source), Some(receiver)),
            checkpoint_capability: checkpoint_capability(self.source_health.checkpoint()),
            receiver,
        }
    }

    fn source_health_values(&self) -> Result<Vec<DashboardSourceHealth>, DashboardQueryErrorCode> {
        let live = self.source_health.snapshot();
        let live_id = live.identity().id().clone();
        let live_version = live.identity().contract_version();
        let receiver = receiver_snapshot(self.receiver.snapshot());
        let durable = self
            .storage
            .source_health_list()
            .map_err(|error| query_error(&error))?;
        let values = merge_registered_source_health(durable, StoredSourceHealth::from(&live))?;
        Ok(values
            .into_iter()
            .map(|value| {
                let live_receiver = (value.source_id == live_id
                    && value.source_contract_version == live_version)
                    .then_some(receiver);
                source_health(value, live_receiver)
            })
            .collect())
    }
}

impl DashboardQueryPort for DashboardAdapter {
    fn resolve_conversation_identifiers(
        &self,
        display_name: &str,
    ) -> Result<Vec<String>, DashboardQueryErrorCode> {
        Ok(self
            .session_index
            .find_identifiers_by_display_name(display_name))
    }

    fn model_call_page(
        &self,
        request: DashboardModelCallPageRequest,
    ) -> Result<DashboardPage<DashboardModelCall, DashboardModelCallCursor>, DashboardQueryErrorCode>
    {
        let sort = model_sort(request.input.sort);
        let filters = model_filters(
            request.input.filters,
            request.resolved_conversation_identifiers,
        )?;
        let cursor = request.cursor.map(storage_model_cursor).transpose()?;
        let query = ModelCallPageQuery::new(request.input.limit, filters, sort, cursor)
            .map_err(|error| query_error(&error))?;
        let page = self
            .storage
            .model_call_page(&query)
            .map_err(|error| query_error(&error))?;
        let names = self.session_index.resolve_batch(
            &page
                .items
                .iter()
                .map(|item| {
                    item.conversation
                        .as_ref()
                        .map(conversation_ref_candidates)
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>(),
        );
        let items = page
            .items
            .into_iter()
            .zip(names)
            .map(|(item, name)| model_call(item, name))
            .collect();
        Ok(DashboardPage {
            items,
            limit: page.limit,
            has_more: page.has_more,
            next_cursor: page.next_cursor.map(dashboard_model_cursor),
        })
    }

    fn conversation_page(
        &self,
        request: DashboardConversationPageRequest,
    ) -> Result<
        DashboardPage<DashboardConversation, DashboardConversationCursor>,
        DashboardQueryErrorCode,
    > {
        let selector =
            conversation_selector(request.input.query.clone(), request.resolved_identifiers)?;
        let cursor = request.cursor.map(|value| {
            storage::DashboardConversationCursor::new(value.last_observed_at, value.conversation_id)
        });
        let query = ConversationPageQuery::new(
            request.input.limit,
            selector,
            request.input.source_id,
            request.input.from,
            request.input.to,
            cursor,
        )
        .map_err(|error| query_error(&error))?;
        let page = self
            .storage
            .conversation_page(&query)
            .map_err(|error| query_error(&error))?;
        let names = self.session_index.resolve_batch(
            &page
                .items
                .iter()
                .map(conversation_candidates)
                .collect::<Vec<_>>(),
        );
        let items = page
            .items
            .into_iter()
            .zip(names)
            .map(|(item, name)| conversation(item, name))
            .collect();
        Ok(DashboardPage {
            items,
            limit: page.limit,
            has_more: page.has_more,
            next_cursor: page.next_cursor.map(|value| DashboardConversationCursor {
                last_observed_at: value.last_observed_at(),
                conversation_id: value.conversation_id(),
            }),
        })
    }

    fn model_call_summary(
        &self,
        request: DashboardSummaryRequest,
    ) -> Result<DashboardModelCallSummary, DashboardQueryErrorCode> {
        let filters = model_filters(request.filters, request.resolved_conversation_identifiers)?;
        self.storage
            .model_call_summary(&filters)
            .map(model_call_summary)
            .map_err(|error| query_error(&error))
    }

    fn source_health_list(&self) -> Result<Vec<DashboardSourceHealth>, DashboardQueryErrorCode> {
        self.source_health_values()
    }

    fn alert_page(
        &self,
        request: DashboardAlertPageRequest,
    ) -> Result<DashboardPage<DashboardAlert, DashboardAlertCursor>, DashboardQueryErrorCode> {
        let selector = conversation_selector(
            request.input.conversation.clone(),
            request.resolved_conversation_identifiers,
        )?;
        let filters = AlertFilters::new(
            selector,
            request.input.model_call_id,
            request.input.action_state,
            request.input.from,
            request.input.to,
        )
        .map_err(|error| query_error(&error))?;
        let cursor = request
            .cursor
            .map(|value| storage::DashboardAlertCursor::new(value.created_at, value.alert_id));
        let query = AlertPageQuery::new(request.input.limit, filters, cursor)
            .map_err(|error| query_error(&error))?;
        let page = self
            .storage
            .alert_page(&query)
            .map_err(|error| query_error(&error))?;
        Ok(DashboardPage {
            items: page.items.into_iter().map(alert).collect(),
            limit: page.limit,
            has_more: page.has_more,
            next_cursor: page.next_cursor.map(|value| DashboardAlertCursor {
                created_at: value.created_at(),
                alert_id: value.alert_id(),
            }),
        })
    }

    fn model_call(
        &self,
        id: codex_monitor_core::ModelCallId,
    ) -> Result<Option<DashboardModelCall>, DashboardQueryErrorCode> {
        let value = self
            .storage
            .dashboard_model_call(id)
            .map_err(|error| query_error(&error))?;
        Ok(value.map(|value| {
            let name = self
                .session_index
                .resolve_batch(&[value
                    .conversation
                    .as_ref()
                    .map(conversation_ref_candidates)
                    .unwrap_or_default()])
                .into_iter()
                .next()
                .flatten();
            model_call(value, name)
        }))
    }

    fn alert(
        &self,
        id: codex_monitor_core::AlertId,
    ) -> Result<Option<DashboardAlert>, DashboardQueryErrorCode> {
        self.storage
            .dashboard_alert(id)
            .map(|value| value.map(alert))
            .map_err(|error| query_error(&error))
    }

    fn rule_capabilities(&self) -> Result<Vec<DashboardRuleCapability>, DashboardQueryErrorCode> {
        self.storage
            .dashboard_rule_capabilities()
            .map(|values| {
                values
                    .into_iter()
                    .map(|value| DashboardRuleCapability {
                        rule_id: value.rule_id,
                        rule_version: value.rule_version,
                        enabled: value.enabled,
                        notify_max_n: value.notify_max_n,
                        last_evaluated_at: value.last_evaluated_at,
                        last_error_code: value.last_error_code,
                    })
                    .collect()
            })
            .map_err(|error| query_error(&error))
    }
}

impl DashboardSettingsPort for DashboardAdapter {
    fn initialize_locale(
        &self,
        detected_locale: DashboardLocale,
    ) -> DashboardFuture<'_, Result<DashboardSettingsDocument, DashboardSettingsErrorCode>> {
        Box::pin(async move {
            self.settings
                .initialize_locale(app_locale(detected_locale))
                .await
                .map(|document| settings_document(&document))
                .map_err(settings_error)
        })
    }

    fn replace(
        &self,
        expected_revision: u64,
        replacement: DashboardSettings,
    ) -> DashboardFuture<'_, Result<DashboardSettingsDocument, DashboardSettingsErrorCode>> {
        Box::pin(async move {
            self.settings
                .replace(expected_revision, app_settings(&replacement))
                .await
                .map(|document| settings_document(&document))
                .map_err(settings_error)
        })
    }
}

impl DashboardCapabilityPort for DashboardAdapter {
    fn snapshot(&self) -> DashboardRuntimeSnapshot {
        self.runtime_snapshot()
    }

    fn retry_source(
        &self,
        source_id: codex_monitor_core::SourceId,
    ) -> Result<DashboardSourceHealth, DashboardRetryErrorCode> {
        let live = self.source_health.snapshot();
        if source_id == *live.identity().id() {
            let receiver = self
                .receiver
                .request_bind_retry()
                .map_err(|error| match error {
                    OtlpReceiverRetryError::InvalidState => DashboardRetryErrorCode::InvalidState,
                })?;
            return Ok(source_health(
                StoredSourceHealth::from(&live),
                Some(receiver_snapshot(receiver)),
            ));
        }
        let durable = self
            .storage
            .source_health_list()
            .map_err(|_| DashboardRetryErrorCode::Unavailable)?;
        if durable.iter().any(|value| value.source_id == source_id) {
            Err(DashboardRetryErrorCode::Unsupported)
        } else {
            Err(DashboardRetryErrorCode::NotFound)
        }
    }
}

impl DashboardEventPort for DashboardAdapter {
    fn current_cursor(&self) -> String {
        self.events.current_cursor()
    }

    fn subscribe(
        &self,
        after_event_id: Option<&str>,
    ) -> Result<DashboardEventSubscriptionStart, DashboardEventSubscribeErrorCode> {
        self.events
            .subscribe(after_event_id)
            .map(|start| match start {
                StreamSubscriptionStart::Active(subscription) => {
                    DashboardEventSubscriptionStart::Active(Box::new(StorageEventSubscription {
                        inner: subscription,
                    }))
                }
                StreamSubscriptionStart::Control(reason) => {
                    DashboardEventSubscriptionStart::Control(stream_control_reason(reason))
                }
            })
            .map_err(|error| match error {
                StreamSubscribeError::Malformed => DashboardEventSubscribeErrorCode::Malformed,
                StreamSubscribeError::VersionUnsupported => {
                    DashboardEventSubscribeErrorCode::VersionUnsupported
                }
                StreamSubscribeError::Capacity => DashboardEventSubscribeErrorCode::Capacity,
            })
    }
}

struct StorageEventSubscription {
    inner: StorageChangeSubscription,
}

impl DashboardEventSubscription for StorageEventSubscription {
    fn recv(&mut self) -> DashboardFuture<'_, DashboardEventReceive> {
        Box::pin(async move {
            match self.inner.recv().await {
                StreamReceive::Change(record) => {
                    DashboardEventReceive::Change(Arc::new(DashboardEventRecord {
                        event_id: record.event_id.clone(),
                        emitted_at: record.emitted_at,
                        change: event_change(&record.change),
                    }))
                }
                StreamReceive::Overflow => DashboardEventReceive::Overflow,
                StreamReceive::Closed => DashboardEventReceive::Closed,
            }
        })
    }
}

fn event_change(value: &StorageChange) -> DashboardEventChange {
    match value {
        StorageChange::ModelCall(id) => DashboardEventChange::ModelCall(*id),
        StorageChange::SourceHealth {
            source_id,
            source_contract_version,
        } => DashboardEventChange::SourceHealth {
            source_id: source_id.clone(),
            source_contract_version: *source_contract_version,
        },
        StorageChange::Alert(id) => DashboardEventChange::Alert(*id),
        StorageChange::Settings(document) => {
            DashboardEventChange::Settings(settings_document(document))
        }
        StorageChange::ResyncRequired => DashboardEventChange::ResyncRequired,
    }
}

const fn stream_control_reason(value: StreamControlReason) -> DashboardStreamControlReason {
    match value {
        StreamControlReason::Overflow => DashboardStreamControlReason::Overflow,
        StreamControlReason::MissedEvent => DashboardStreamControlReason::MissedEvent,
        StreamControlReason::ServerRestart => DashboardStreamControlReason::ServerRestart,
    }
}

fn query_error(error: &StorageError) -> DashboardQueryErrorCode {
    match error {
        StorageError::InvalidDashboardQueryLimit { .. }
        | StorageError::InvalidDashboardTimeRange
        | StorageError::InvalidConversationSelector
        | StorageError::InvalidDashboardFilter { .. }
        | StorageError::TooManyResolvedConversationIdentifiers { .. }
        | StorageError::DashboardCursorSortMismatch => DashboardQueryErrorCode::InvalidRequest,
        _ => DashboardQueryErrorCode::Unavailable,
    }
}

fn model_filters(
    input: ModelFiltersInput,
    resolved: Vec<String>,
) -> Result<ModelCallFilters, DashboardQueryErrorCode> {
    let selector = conversation_selector(input.conversation, resolved)?;
    ModelCallFilters::new(
        selector,
        input.model,
        input.source_id,
        input.client,
        input.from,
        input.to,
        match input.alert {
            AlertMatchInput::Any => AlertMatchFilter::Any,
            AlertMatchInput::Matched => AlertMatchFilter::Matched,
            AlertMatchInput::None => AlertMatchFilter::None,
        },
        input.temporal_class,
    )
    .map_err(|error| query_error(&error))
}

fn conversation_selector(
    raw: Option<String>,
    resolved: Vec<String>,
) -> Result<Option<ConversationSelector>, DashboardQueryErrorCode> {
    match raw {
        Some(raw) => ConversationSelector::new(Some(raw), resolved)
            .map(Some)
            .map_err(|error| query_error(&error)),
        None if resolved.is_empty() => Ok(None),
        None => Err(DashboardQueryErrorCode::InvalidRequest),
    }
}

const fn model_sort(value: ModelCallSortInput) -> ModelCallSort {
    match value {
        ModelCallSortInput::EventAtDesc => ModelCallSort::EventAtDesc,
        ModelCallSortInput::EventAtAsc => ModelCallSort::EventAtAsc,
        ModelCallSortInput::InputTokensDesc => ModelCallSort::InputTokensDesc,
        ModelCallSortInput::OutputTokensDesc => ModelCallSort::OutputTokensDesc,
        ModelCallSortInput::ReasoningTokensDesc => ModelCallSort::ReasoningTokensDesc,
        ModelCallSortInput::TotalTokensDesc => ModelCallSort::TotalTokensDesc,
    }
}

fn storage_model_cursor(
    value: DashboardModelCallCursor,
) -> Result<StorageModelCallCursor, DashboardQueryErrorCode> {
    let sort = model_sort(value.sort);
    match value.sort {
        ModelCallSortInput::EventAtDesc | ModelCallSortInput::EventAtAsc => {
            StorageModelCallCursor::event(sort, value.event_at, value.model_call_id)
        }
        ModelCallSortInput::InputTokensDesc
        | ModelCallSortInput::OutputTokensDesc
        | ModelCallSortInput::ReasoningTokensDesc
        | ModelCallSortInput::TotalTokensDesc => StorageModelCallCursor::token(
            sort,
            value.token_value,
            value.event_at,
            value.model_call_id,
        ),
    }
    .map_err(|error| query_error(&error))
}

fn dashboard_model_cursor(value: StorageModelCallCursor) -> DashboardModelCallCursor {
    DashboardModelCallCursor {
        sort: match value.sort() {
            ModelCallSort::EventAtDesc => ModelCallSortInput::EventAtDesc,
            ModelCallSort::EventAtAsc => ModelCallSortInput::EventAtAsc,
            ModelCallSort::InputTokensDesc => ModelCallSortInput::InputTokensDesc,
            ModelCallSort::OutputTokensDesc => ModelCallSortInput::OutputTokensDesc,
            ModelCallSort::ReasoningTokensDesc => ModelCallSortInput::ReasoningTokensDesc,
            ModelCallSort::TotalTokensDesc => ModelCallSortInput::TotalTokensDesc,
        },
        token_value: value.token_value(),
        event_at: value.event_at(),
        model_call_id: value.model_call_id(),
    }
}

fn model_call(
    value: storage::DashboardModelCall,
    resolved_display_name: Option<String>,
) -> DashboardModelCall {
    DashboardModelCall {
        id: value.id,
        event_at: value.event_at,
        conversation: value
            .conversation
            .map(|conversation| DashboardConversationRef {
                id: conversation.id,
                source_id: conversation.source_id,
                thread_id: conversation.thread_id,
                resolved_display_name,
            }),
        turn_id: value.turn_id,
        model: value.model,
        reasoning_effort: value.reasoning_effort,
        tokens: DashboardTokenUsage {
            input: value.tokens.input,
            cached_input: value.tokens.cached_input,
            output: value.tokens.output,
            reasoning: value.tokens.reasoning,
            total: value.tokens.total,
        },
        source_ids: value.source_ids,
        client: value.client,
        surface: value.surface,
        temporal_class: value.temporal_class,
        alert_state: match value.alert_state {
            storage::DashboardAlertState::None => DashboardAlertState::None,
            storage::DashboardAlertState::Matched => DashboardAlertState::Matched,
        },
        alert_count: value.alert_count,
        field_sources: value.field_sources.into_iter().map(field_source).collect(),
    }
}

fn field_source(value: storage::DashboardFieldSource) -> DashboardFieldSource {
    DashboardFieldSource {
        field: match value.field {
            storage::CanonicalFieldName::Conversation => DashboardCanonicalField::Conversation,
            storage::CanonicalFieldName::Turn => DashboardCanonicalField::Turn,
            storage::CanonicalFieldName::EventAt => DashboardCanonicalField::EventAt,
            storage::CanonicalFieldName::Model => DashboardCanonicalField::Model,
            storage::CanonicalFieldName::ReasoningEffort => {
                DashboardCanonicalField::ReasoningEffort
            }
            storage::CanonicalFieldName::InputTokens => DashboardCanonicalField::InputTokens,
            storage::CanonicalFieldName::CachedInputTokens => {
                DashboardCanonicalField::CachedInputTokens
            }
            storage::CanonicalFieldName::OutputTokens => DashboardCanonicalField::OutputTokens,
            storage::CanonicalFieldName::ReasoningOutputTokens => {
                DashboardCanonicalField::ReasoningOutputTokens
            }
            storage::CanonicalFieldName::Client => DashboardCanonicalField::Client,
            storage::CanonicalFieldName::Surface => DashboardCanonicalField::Surface,
        },
        source_id: value.source_id,
        source_contract_version: value.source_contract_version,
        observed_at: value.observed_at,
        quality: value.quality,
    }
}

fn model_call_summary(value: storage::DashboardModelCallSummary) -> DashboardModelCallSummary {
    DashboardModelCallSummary {
        call_count: value.call_count,
        alert_count: value.alert_count,
        input: token_aggregate(value.input),
        cached_input: token_aggregate(value.cached_input),
        output: token_aggregate(value.output),
        reasoning: token_aggregate(value.reasoning),
        total: token_aggregate(value.total),
    }
}

const fn token_aggregate(value: storage::DashboardTokenAggregate) -> DashboardTokenAggregate {
    DashboardTokenAggregate {
        value: value.value,
        known_call_count: value.known_call_count,
        total_call_count: value.total_call_count,
        complete: value.complete,
    }
}

fn conversation(
    value: storage::DashboardConversation,
    resolved_display_name: Option<String>,
) -> DashboardConversation {
    DashboardConversation {
        id: value.id,
        source_id: value.source_id,
        thread_id: value.thread_id,
        resolved_display_name,
        first_observed_at: value.first_observed_at,
        last_observed_at: value.last_observed_at,
        model_call_count: value.model_call_count,
        alert_count: value.alert_count,
    }
}

fn alert(value: storage::DashboardAlert) -> DashboardAlert {
    DashboardAlert {
        id: value.id,
        model_call_id: value.model_call_id,
        conversation_id: value.conversation_id,
        rule_id: value.rule_id,
        rule_version: value.rule_version,
        evidence: DashboardAlertEvidence {
            reasoning_output_tokens: value.evidence.reasoning_output_tokens,
            pattern_n: value.evidence.pattern_n,
            event_at: value.evidence.event_at,
            source_id: value.evidence.source_id,
            source_contract_version: value.evidence.source_contract_version,
            observed_at: value.evidence.observed_at,
            quality: value.evidence.quality,
        },
        action: value.action.map(|action| DashboardAlertAction {
            state: action.state,
            created_at: action.created_at,
            not_before: action.not_before,
            started_at: action.started_at,
            completed_at: action.completed_at,
            error_code: action.error_code,
            suppression_reason: action.suppression_reason,
        }),
        created_at: value.created_at,
    }
}

fn source_health(
    value: StoredSourceHealth,
    receiver: Option<DashboardReceiverSnapshot>,
) -> DashboardSourceHealth {
    DashboardSourceHealth {
        source_id: value.source_id,
        source_contract_version: value.source_contract_version,
        status: value.status,
        receiver,
        last_accepted_at: value.last_accepted_at,
        last_error_code: value.last_error_code,
        last_error_occurred_at: value.last_error_occurred_at,
        updated_at: value.updated_at,
        supported_fields: value.supported_fields,
    }
}

fn merge_registered_source_health(
    mut durable: Vec<StoredSourceHealth>,
    registered: StoredSourceHealth,
) -> Result<Vec<StoredSourceHealth>, DashboardQueryErrorCode> {
    if let Some(stored) = durable.iter_mut().find(|stored| {
        stored.source_id == registered.source_id
            && stored.source_contract_version == registered.source_contract_version
    }) {
        *stored = merge_source_health_snapshots(stored.clone(), registered);
    } else {
        if durable.len() >= storage::MAX_DASHBOARD_SOURCE_ROWS {
            return Err(DashboardQueryErrorCode::Unavailable);
        }
        durable.push(registered);
    }
    durable.sort_unstable_by(|left, right| {
        left.source_id.cmp(&right.source_id).then_with(|| {
            left.source_contract_version
                .get()
                .cmp(&right.source_contract_version.get())
        })
    });
    Ok(durable)
}

fn merge_source_health_snapshots(
    durable: StoredSourceHealth,
    registered: StoredSourceHealth,
) -> StoredSourceHealth {
    let registered_is_current =
        (registered.updated_at, registered.status) >= (durable.updated_at, durable.status);
    let (mut current, historical) = if registered_is_current {
        (registered, durable)
    } else {
        (durable, registered)
    };
    current.last_accepted_at = current.last_accepted_at.max(historical.last_accepted_at);
    let current_failure = current.last_error_occurred_at.zip(current.last_error_code);
    let historical_failure = historical
        .last_error_occurred_at
        .zip(historical.last_error_code);
    if historical_failure > current_failure {
        current.last_error_occurred_at = historical.last_error_occurred_at;
        current.last_error_code = historical.last_error_code;
    }
    current
}

fn conversation_ref_candidates(value: &storage::DashboardConversationRef) -> Vec<String> {
    let mut candidates = Vec::with_capacity(2);
    if let Some(source_id) = value.source_id.as_ref() {
        candidates.push(source_id.as_str().to_owned());
    }
    if let Some(thread_id) = value.thread_id.as_ref() {
        candidates.push(thread_id.as_str().to_owned());
    }
    candidates
}

fn conversation_candidates(value: &storage::DashboardConversation) -> Vec<String> {
    let mut candidates = Vec::with_capacity(2);
    if let Some(source_id) = value.source_id.as_ref() {
        candidates.push(source_id.as_str().to_owned());
    }
    if let Some(thread_id) = value.thread_id.as_ref() {
        candidates.push(thread_id.as_str().to_owned());
    }
    candidates
}

const fn receiver_snapshot(value: OtlpReceiverSnapshot) -> DashboardReceiverSnapshot {
    DashboardReceiverSnapshot {
        port: value.address().port(),
        bind_state: match value.bind_state() {
            OtlpReceiverBindState::Starting => DashboardReceiverBindState::Starting,
            OtlpReceiverBindState::Bound => DashboardReceiverBindState::Bound,
            OtlpReceiverBindState::Retrying => DashboardReceiverBindState::Retrying,
            OtlpReceiverBindState::Conflict => DashboardReceiverBindState::Conflict,
            OtlpReceiverBindState::Stopped => DashboardReceiverBindState::Stopped,
        },
        retry_supported: value.retry_supported(),
    }
}

const fn checkpoint_capability(value: CheckpointCapability) -> DashboardCheckpointCapability {
    match value {
        CheckpointCapability::None => DashboardCheckpointCapability::None,
        CheckpointCapability::MonotonicFileOffsetV1 => {
            DashboardCheckpointCapability::MonotonicFileOffsetV1
        }
    }
}

const fn app_locale(value: DashboardLocale) -> AppLocale {
    match value {
        DashboardLocale::EnUs => AppLocale::EnUs,
        DashboardLocale::ZhCn => AppLocale::ZhCn,
    }
}

const fn dashboard_locale(value: AppLocale) -> DashboardLocale {
    match value {
        AppLocale::EnUs => DashboardLocale::EnUs,
        AppLocale::ZhCn => DashboardLocale::ZhCn,
    }
}

const fn app_retention(value: DashboardRetentionPolicy) -> storage::RetentionPolicy {
    match value {
        DashboardRetentionPolicy::Unlimited => storage::RetentionPolicy::Unlimited,
        DashboardRetentionPolicy::Days30 => storage::RetentionPolicy::Days30,
        DashboardRetentionPolicy::Days90 => storage::RetentionPolicy::Days90,
        DashboardRetentionPolicy::Days365 => storage::RetentionPolicy::Days365,
    }
}

const fn dashboard_retention(value: storage::RetentionPolicy) -> DashboardRetentionPolicy {
    match value {
        storage::RetentionPolicy::Unlimited => DashboardRetentionPolicy::Unlimited,
        storage::RetentionPolicy::Days30 => DashboardRetentionPolicy::Days30,
        storage::RetentionPolicy::Days90 => DashboardRetentionPolicy::Days90,
        storage::RetentionPolicy::Days365 => DashboardRetentionPolicy::Days365,
    }
}

fn app_settings(value: &DashboardSettings) -> AppSettings {
    AppSettings {
        retention: app_retention(value.retention),
        notifications_enabled: value.notifications_enabled,
        notify_max_n: value.notify_max_n,
        notify_all: value.notify_all,
        locale: app_locale(value.locale),
        login_autostart: value.login_autostart,
        updated_at: value.updated_at,
    }
}

fn settings_document(value: &StoredSettingsDocument) -> DashboardSettingsDocument {
    DashboardSettingsDocument {
        revision: value.revision,
        settings: DashboardSettings {
            retention: dashboard_retention(value.settings.retention),
            notifications_enabled: value.settings.notifications_enabled,
            notify_max_n: value.settings.notify_max_n,
            notify_all: value.settings.notify_all,
            locale: dashboard_locale(value.settings.locale),
            login_autostart: value.settings.login_autostart,
            updated_at: value.settings.updated_at,
        },
    }
}

const fn settings_error(value: SettingsRuntimeError) -> DashboardSettingsErrorCode {
    match value {
        SettingsRuntimeError::RevisionConflict => DashboardSettingsErrorCode::RevisionConflict,
        SettingsRuntimeError::InvalidSettings => DashboardSettingsErrorCode::InvalidSettings,
        SettingsRuntimeError::StorageUnavailable => DashboardSettingsErrorCode::StorageUnavailable,
        SettingsRuntimeError::PlatformUnavailable => {
            DashboardSettingsErrorCode::PlatformUnavailable
        }
        SettingsRuntimeError::RuntimeUnavailable => DashboardSettingsErrorCode::RuntimeUnavailable,
    }
}
