use std::sync::LazyLock;

use codex_monitor_host::HostLocale;
use serde::Deserialize;

const EN_US_HOST_CATALOG: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../web/src/locales/en-US/host.json"
));
const ZH_CN_HOST_CATALOG: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../web/src/locales/zh-CN/host.json"
));

static EN_US_CATALOG: LazyLock<HostCatalog> = LazyLock::new(|| {
    serde_json::from_str(EN_US_HOST_CATALOG).expect("en-US host catalog must be valid")
});
static ZH_CN_CATALOG: LazyLock<HostCatalog> = LazyLock::new(|| {
    serde_json::from_str(ZH_CN_HOST_CATALOG).expect("zh-CN host catalog must be valid")
});

#[derive(Deserialize)]
pub(crate) struct HostCatalog {
    pub(crate) tray: TrayCatalog,
    pub(crate) notification: NotificationCatalog,
}

#[derive(Deserialize)]
pub(crate) struct TrayCatalog {
    pub(crate) tooltip: String,
    pub(crate) status: TrayStatusCatalog,
    pub(crate) menu: TrayMenuCatalog,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TrayStatusCatalog {
    pub(crate) starting: String,
    pub(crate) healthy: String,
    pub(crate) degraded: String,
    pub(crate) receiver_unavailable: String,
    pub(crate) receiver_port_conflict: String,
    pub(crate) notification_permission_required: String,
    pub(crate) notification_unavailable: String,
    pub(crate) stopped: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TrayMenuCatalog {
    pub(crate) status: String,
    pub(crate) open_dashboard: String,
    pub(crate) start_at_login: String,
    pub(crate) reasoning_notifications: String,
    pub(crate) settings: String,
    pub(crate) quit: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NotificationCatalog {
    pub(crate) reasoning_pattern: ReasoningPatternCatalog,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReasoningPatternCatalog {
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) body_without_model: String,
    pub(crate) coalesced_title: String,
    pub(crate) coalesced_body: String,
    pub(crate) coalesced_body_without_model: String,
}

#[must_use]
pub(crate) fn catalog(locale: HostLocale) -> &'static HostCatalog {
    match locale {
        HostLocale::EnUs => &EN_US_CATALOG,
        HostLocale::ZhCn => &ZH_CN_CATALOG,
    }
}
