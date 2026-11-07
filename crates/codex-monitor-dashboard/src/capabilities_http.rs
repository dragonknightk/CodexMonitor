use codex_monitor_core::{
    BuiltInRuleId, FieldId, RuleErrorCode, RuleVersion, SourceContractVersion, SourceId,
    UtcTimestamp,
};
use serde::Serialize;

use super::state::{
    DashboardPlatformCapabilities, DashboardState, SystemNotificationAvailability, TrayAvailability,
};
use crate::ports::{
    DashboardCheckpointCapability, DashboardQueryErrorCode, DashboardRuleCapability,
};

const QUERY_FILTERS: &[&str] = &[
    "conversation",
    "model",
    "source_id",
    "client",
    "from",
    "to",
    "alert",
    "temporal_class",
];
const QUERY_SORTS: &[&str] = &[
    "event_at_desc",
    "event_at_asc",
    "input_tokens_desc",
    "output_tokens_desc",
    "reasoning_tokens_desc",
    "total_tokens_desc",
];
const LOCALES: &[&str] = &["en-US", "zh-CN"];
const EVENT_TYPES: &[&str] = &[
    "model-call",
    "source-health",
    "alert",
    "settings",
    "stream-control",
];

#[derive(Serialize)]
pub(super) struct CapabilitiesResponse {
    api_version: &'static str,
    query: QueryCapabilitiesResponse,
    locales: &'static [&'static str],
    event_types: &'static [&'static str],
    sources: Vec<SourceCapabilityResponse>,
    rules: Vec<RuleCapabilityResponse>,
    platform: PlatformCapabilitiesResponse,
}

#[derive(Serialize)]
struct QueryCapabilitiesResponse {
    default_page_limit: usize,
    max_page_limit: usize,
    filters: &'static [&'static str],
    sorts: &'static [&'static str],
}

#[derive(Serialize)]
struct SourceCapabilityResponse {
    source_id: SourceId,
    contract_version: SourceContractVersion,
    supported_fields: Vec<FieldId>,
    checkpoint_capability: CheckpointCapabilityResponse,
    supports_retry: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum CheckpointCapabilityResponse {
    None,
    MonotonicFileOffsetV1,
}

#[derive(Serialize)]
struct RuleCapabilityResponse {
    rule_id: BuiltInRuleId,
    rule_version: RuleVersion,
    enabled: bool,
    notify_max_n: Option<u32>,
    last_evaluated_at: Option<UtcTimestamp>,
    last_error_code: Option<RuleErrorCode>,
}

#[derive(Serialize)]
struct PlatformCapabilitiesResponse {
    login_autostart: bool,
    system_notifications: SystemNotificationAvailabilityResponse,
    tray: TrayAvailabilityResponse,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum SystemNotificationAvailabilityResponse {
    Available,
    PermissionRequired,
    Unavailable,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum TrayAvailabilityResponse {
    Available,
    Unavailable,
}

pub(super) fn capabilities(
    state: &DashboardState,
) -> Result<CapabilitiesResponse, DashboardQueryErrorCode> {
    let runtime = state.capabilities.snapshot();
    let source = runtime.source;
    let source_capability = SourceCapabilityResponse {
        source_id: source.source_id,
        contract_version: source.source_contract_version,
        supported_fields: source.supported_fields,
        checkpoint_capability: checkpoint_capability(runtime.checkpoint_capability),
        supports_retry: runtime.receiver.retry_supported,
    };
    let rules = state
        .queries
        .rule_capabilities()?
        .into_iter()
        .map(rule_capability)
        .collect();
    Ok(CapabilitiesResponse {
        api_version: "v1",
        query: QueryCapabilitiesResponse {
            default_page_limit: super::query_input::DEFAULT_PAGE_LIMIT,
            max_page_limit: super::query_input::MAX_PAGE_LIMIT,
            filters: QUERY_FILTERS,
            sorts: QUERY_SORTS,
        },
        locales: LOCALES,
        event_types: EVENT_TYPES,
        sources: vec![source_capability],
        rules,
        platform: platform_capabilities(&state.platform),
    })
}

fn checkpoint_capability(value: DashboardCheckpointCapability) -> CheckpointCapabilityResponse {
    match value {
        DashboardCheckpointCapability::None => CheckpointCapabilityResponse::None,
        DashboardCheckpointCapability::MonotonicFileOffsetV1 => {
            CheckpointCapabilityResponse::MonotonicFileOffsetV1
        }
    }
}

fn rule_capability(value: DashboardRuleCapability) -> RuleCapabilityResponse {
    RuleCapabilityResponse {
        rule_id: value.rule_id,
        rule_version: value.rule_version,
        enabled: value.enabled,
        notify_max_n: value.notify_max_n.map(std::num::NonZeroU32::get),
        last_evaluated_at: value.last_evaluated_at,
        last_error_code: value.last_error_code,
    }
}

fn platform_capabilities(value: &DashboardPlatformCapabilities) -> PlatformCapabilitiesResponse {
    PlatformCapabilitiesResponse {
        login_autostart: value.login_autostart(),
        system_notifications: match value.system_notifications() {
            SystemNotificationAvailability::Available => {
                SystemNotificationAvailabilityResponse::Available
            }
            SystemNotificationAvailability::PermissionRequired => {
                SystemNotificationAvailabilityResponse::PermissionRequired
            }
            SystemNotificationAvailability::Unavailable => {
                SystemNotificationAvailabilityResponse::Unavailable
            }
        },
        tray: match value.tray() {
            TrayAvailability::Available => TrayAvailabilityResponse::Available,
            TrayAvailability::Unavailable => TrayAvailabilityResponse::Unavailable,
        },
    }
}
