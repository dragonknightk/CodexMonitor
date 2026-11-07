//! Dashboard 消费方拥有的窄端口与闭合传输类型。

use std::future::Future;
use std::num::NonZeroU32;
use std::pin::Pin;
use std::sync::Arc;

use codex_monitor_core::{
    ActionErrorCode, ActionLifecycleState, ActionSuppressionReason, AlertId, BuiltInRuleId,
    ClientName, ConversationId, ConversationSourceId, DataOrigin, EvidenceQuality, FieldId,
    ModelCallId, ModelName, PatternN, ReasoningEffort, RuleErrorCode, RuleVersion, SafeErrorCode,
    SourceContractVersion, SourceHealthStatus, SourceId, SurfaceName, ThreadSourceId, TokenCount,
    TurnId, UtcTimestamp,
};

use crate::query_input::{
    AlertQueryInput, ConversationQueryInput, ModelCallQueryInput, ModelCallSortInput,
    ModelFiltersInput,
};

/// 由端口返回的异步工作，不暴露具体运行时任务类型。
pub type DashboardFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 查询端口的稳定失败代码。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DashboardQueryErrorCode {
    /// 查询参数跨适配器转换后不再满足不变量。
    InvalidRequest,
    /// 查询实现或其持久化依赖不可用。
    Unavailable,
}

/// 设置端口的稳定失败代码。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DashboardSettingsErrorCode {
    RevisionConflict,
    InvalidSettings,
    StorageUnavailable,
    PlatformUnavailable,
    RuntimeUnavailable,
}

/// 来源重试端口的稳定失败代码。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DashboardRetryErrorCode {
    InvalidState,
    Unsupported,
    NotFound,
    Unavailable,
}

/// 订阅变更流时的稳定失败代码。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DashboardEventSubscribeErrorCode {
    Malformed,
    VersionUnsupported,
    Capacity,
}

/// 模型调用查询的闭合游标边界。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DashboardModelCallCursor {
    pub sort: ModelCallSortInput,
    pub token_value: Option<TokenCount>,
    pub event_at: UtcTimestamp,
    pub model_call_id: ModelCallId,
}

/// 会话查询的闭合游标边界。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DashboardConversationCursor {
    pub last_observed_at: UtcTimestamp,
    pub conversation_id: ConversationId,
}

/// 告警查询的闭合游标边界。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DashboardAlertCursor {
    pub created_at: UtcTimestamp,
    pub alert_id: AlertId,
}

/// 已验证的模型调用分页请求。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardModelCallPageRequest {
    pub input: ModelCallQueryInput,
    pub resolved_conversation_identifiers: Vec<String>,
    pub cursor: Option<DashboardModelCallCursor>,
}

/// 已验证的会话分页请求。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardConversationPageRequest {
    pub input: ConversationQueryInput,
    pub resolved_identifiers: Vec<String>,
    pub cursor: Option<DashboardConversationCursor>,
}

/// 已验证的汇总请求。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardSummaryRequest {
    pub filters: ModelFiltersInput,
    pub resolved_conversation_identifiers: Vec<String>,
}

/// 已验证的告警分页请求。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardAlertPageRequest {
    pub input: AlertQueryInput,
    pub resolved_conversation_identifiers: Vec<String>,
    pub cursor: Option<DashboardAlertCursor>,
}

/// 端口分页结果。下一页边界仍是进程内闭合类型，由 Dashboard 签发不透明游标。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardPage<T, C> {
    pub items: Vec<T>,
    pub limit: usize,
    pub has_more: bool,
    pub next_cursor: Option<C>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DashboardCanonicalField {
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
    pub field: DashboardCanonicalField,
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
    pub resolved_display_name: Option<String>,
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
    pub resolved_display_name: Option<String>,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DashboardReceiverBindState {
    Starting,
    Bound,
    Retrying,
    Conflict,
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DashboardReceiverSnapshot {
    pub port: u16,
    pub bind_state: DashboardReceiverBindState,
    pub retry_supported: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardSourceHealth {
    pub source_id: SourceId,
    pub source_contract_version: SourceContractVersion,
    pub status: SourceHealthStatus,
    pub receiver: Option<DashboardReceiverSnapshot>,
    pub last_accepted_at: Option<UtcTimestamp>,
    pub last_error_code: Option<SafeErrorCode>,
    pub last_error_occurred_at: Option<UtcTimestamp>,
    pub updated_at: UtcTimestamp,
    pub supported_fields: Vec<FieldId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DashboardCheckpointCapability {
    None,
    MonotonicFileOffsetV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardRuntimeSnapshot {
    pub source: DashboardSourceHealth,
    pub checkpoint_capability: DashboardCheckpointCapability,
    pub receiver: DashboardReceiverSnapshot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DashboardRuleCapability {
    pub rule_id: BuiltInRuleId,
    pub rule_version: RuleVersion,
    pub enabled: bool,
    pub notify_max_n: Option<NonZeroU32>,
    pub last_evaluated_at: Option<UtcTimestamp>,
    pub last_error_code: Option<RuleErrorCode>,
}

/// Dashboard 支持的界面语言。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DashboardLocale {
    EnUs,
    ZhCn,
}

impl DashboardLocale {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EnUs => "en-US",
            Self::ZhCn => "zh-CN",
        }
    }

    #[must_use]
    pub fn from_language_tag(value: &str) -> Option<Self> {
        let primary = value.trim().split('-').next()?.to_ascii_lowercase();
        match primary.as_str() {
            "en" => Some(Self::EnUs),
            "zh" => Some(Self::ZhCn),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DashboardRetentionPolicy {
    Unlimited,
    Days30,
    Days90,
    Days365,
}

impl DashboardRetentionPolicy {
    #[must_use]
    pub const fn days(self) -> Option<i64> {
        match self {
            Self::Unlimited => None,
            Self::Days30 => Some(30),
            Self::Days90 => Some(90),
            Self::Days365 => Some(365),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardSettings {
    pub retention: DashboardRetentionPolicy,
    pub notifications_enabled: bool,
    pub notify_max_n: Option<NonZeroU32>,
    pub notify_all: bool,
    pub locale: DashboardLocale,
    pub login_autostart: bool,
    pub updated_at: UtcTimestamp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardSettingsDocument {
    pub revision: u64,
    pub settings: DashboardSettings,
}

/// Dashboard 的同步查询端口。实现不得把 SQL、路径或底层错误文本带回消费方。
pub trait DashboardQueryPort: Send + Sync + 'static {
    /// # Errors
    ///
    /// 查询依赖不可用或名称不满足查询约束时返回稳定的错误代码。
    fn resolve_conversation_identifiers(
        &self,
        display_name: &str,
    ) -> Result<Vec<String>, DashboardQueryErrorCode>;

    /// # Errors
    ///
    /// 请求无效或查询依赖不可用时返回稳定的错误代码。
    fn model_call_page(
        &self,
        request: DashboardModelCallPageRequest,
    ) -> Result<DashboardPage<DashboardModelCall, DashboardModelCallCursor>, DashboardQueryErrorCode>;

    /// # Errors
    ///
    /// 请求无效或查询依赖不可用时返回稳定的错误代码。
    fn conversation_page(
        &self,
        request: DashboardConversationPageRequest,
    ) -> Result<
        DashboardPage<DashboardConversation, DashboardConversationCursor>,
        DashboardQueryErrorCode,
    >;

    /// # Errors
    ///
    /// 请求无效或查询依赖不可用时返回稳定的错误代码。
    fn model_call_summary(
        &self,
        request: DashboardSummaryRequest,
    ) -> Result<DashboardModelCallSummary, DashboardQueryErrorCode>;

    /// # Errors
    ///
    /// 查询依赖不可用时返回稳定的错误代码。
    fn source_health_list(&self) -> Result<Vec<DashboardSourceHealth>, DashboardQueryErrorCode>;

    /// # Errors
    ///
    /// 请求无效或查询依赖不可用时返回稳定的错误代码。
    fn alert_page(
        &self,
        request: DashboardAlertPageRequest,
    ) -> Result<DashboardPage<DashboardAlert, DashboardAlertCursor>, DashboardQueryErrorCode>;

    /// # Errors
    ///
    /// 标识符无效或查询依赖不可用时返回稳定的错误代码。
    fn model_call(
        &self,
        id: ModelCallId,
    ) -> Result<Option<DashboardModelCall>, DashboardQueryErrorCode>;

    /// # Errors
    ///
    /// 标识符无效或查询依赖不可用时返回稳定的错误代码。
    fn alert(&self, id: AlertId) -> Result<Option<DashboardAlert>, DashboardQueryErrorCode>;

    /// # Errors
    ///
    /// 查询依赖不可用时返回稳定的错误代码。
    fn rule_capabilities(&self) -> Result<Vec<DashboardRuleCapability>, DashboardQueryErrorCode>;
}

/// Dashboard 设置端口。异步返回值只携带闭合设置文档和稳定错误代码。
pub trait DashboardSettingsPort: Send + Sync + 'static {
    fn initialize_locale(
        &self,
        detected_locale: DashboardLocale,
    ) -> DashboardFuture<'_, Result<DashboardSettingsDocument, DashboardSettingsErrorCode>>;

    fn replace(
        &self,
        expected_revision: u64,
        replacement: DashboardSettings,
    ) -> DashboardFuture<'_, Result<DashboardSettingsDocument, DashboardSettingsErrorCode>>;
}

/// Dashboard 运行能力端口。
pub trait DashboardCapabilityPort: Send + Sync + 'static {
    fn snapshot(&self) -> DashboardRuntimeSnapshot;

    /// # Errors
    ///
    /// source 不存在、不支持重试、当前状态不允许重试或运行时不可用时返回稳定的错误代码。
    fn retry_source(
        &self,
        source_id: SourceId,
    ) -> Result<DashboardSourceHealth, DashboardRetryErrorCode>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DashboardEventChange {
    ModelCall(ModelCallId),
    SourceHealth {
        source_id: SourceId,
        source_contract_version: SourceContractVersion,
    },
    Alert(AlertId),
    Settings(DashboardSettingsDocument),
    ResyncRequired,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardEventRecord {
    pub event_id: String,
    pub emitted_at: UtcTimestamp,
    pub change: DashboardEventChange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DashboardStreamControlReason {
    Overflow,
    MissedEvent,
    ServerRestart,
}

pub enum DashboardEventReceive {
    Change(Arc<DashboardEventRecord>),
    Overflow,
    Closed,
}

pub trait DashboardEventSubscription: Send + 'static {
    fn recv(&mut self) -> DashboardFuture<'_, DashboardEventReceive>;
}

pub enum DashboardEventSubscriptionStart {
    Active(Box<dyn DashboardEventSubscription>),
    Control(DashboardStreamControlReason),
}

/// Dashboard SSE 变更端口。
pub trait DashboardEventPort: Send + Sync + 'static {
    fn current_cursor(&self) -> String;

    /// # Errors
    ///
    /// 恢复游标无效、版本不受支持或订阅容量耗尽时返回稳定的错误代码。
    fn subscribe(
        &self,
        after_event_id: Option<&str>,
    ) -> Result<DashboardEventSubscriptionStart, DashboardEventSubscribeErrorCode>;
}
