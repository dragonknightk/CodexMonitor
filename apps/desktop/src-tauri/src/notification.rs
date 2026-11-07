use std::fmt;
use std::sync::Arc;

use codex_monitor_core::{
    ActionCapability, ActionErrorCode, ActionExecutionContext, ActionExecutionResult,
    ActionExecutor, ActionFuture, ActionLocale, ActionParameters, ActionPermission,
    RegisteredActionType, SYSTEM_NOTIFICATION_DEFINITION, SystemNotificationParams,
};
use codex_monitor_dashboard::{DashboardPlatformCapabilities, SystemNotificationAvailability};
use codex_monitor_host::HostLocale;
use codex_monitor_storage::AppLocale;
use tauri::{App, Wry};

use crate::host_catalog::catalog;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "permission-only variants are constructed on macOS and in cross-platform tests"
)]
enum NativeNotificationPermission {
    Granted,
    Prompt,
    Denied,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "backend failure variants differ across the three supported platforms"
)]
enum NotificationBackendError {
    PermissionDenied,
    Unavailable,
    DispatchFailed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NotificationMessage {
    title: String,
    body: String,
}

trait NotificationBackend: Send + Sync + 'static {
    fn probe(&self) -> Result<NativeNotificationPermission, NotificationBackendError>;

    fn request_permission(&self) -> Result<NativeNotificationPermission, NotificationBackendError>;

    fn send(&self, message: &NotificationMessage) -> Result<(), NotificationBackendError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NotificationExecutionOutcome {
    result: ActionExecutionResult,
    availability: SystemNotificationAvailability,
}

impl NotificationExecutionOutcome {
    const fn completed() -> Self {
        Self {
            result: ActionExecutionResult::Completed,
            availability: SystemNotificationAvailability::Available,
        }
    }

    const fn permission_denied() -> Self {
        Self {
            result: ActionExecutionResult::Unavailable(ActionErrorCode::PermissionDenied),
            availability: SystemNotificationAvailability::Unavailable,
        }
    }

    const fn unavailable() -> Self {
        Self {
            result: ActionExecutionResult::Unavailable(ActionErrorCode::NotificationUnavailable),
            availability: SystemNotificationAvailability::Unavailable,
        }
    }

    const fn dispatch_failed() -> Self {
        Self {
            result: ActionExecutionResult::Failed(ActionErrorCode::NotificationDispatchFailed),
            availability: SystemNotificationAvailability::Available,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NotificationHostCreateError {
    MissingProductName,
}

impl fmt::Display for NotificationHostCreateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingProductName => formatter.write_str("Tauri product name is required"),
        }
    }
}

impl std::error::Error for NotificationHostCreateError {}

/// Native executor for the one registered v1 system-notification action.
pub(crate) struct TauriSystemNotificationHost {
    backend: Arc<dyn NotificationBackend>,
    platform: DashboardPlatformCapabilities,
}

impl TauriSystemNotificationHost {
    pub(crate) fn new(
        app: &App<Wry>,
        platform: DashboardPlatformCapabilities,
        notification_identity_ready: bool,
    ) -> Result<Self, NotificationHostCreateError> {
        let application_name = app
            .config()
            .product_name
            .clone()
            .ok_or(NotificationHostCreateError::MissingProductName)?;
        #[cfg(not(target_os = "windows"))]
        let application_id = app.config().identifier.clone();
        #[cfg(target_os = "windows")]
        let application_id = crate::platform_registration::app_user_model_id().to_owned();
        Ok(Self {
            backend: Arc::new(PlatformNotificationBackend::new(
                application_name,
                application_id,
                notification_identity_ready,
            )),
            platform,
        })
    }

    #[cfg(test)]
    fn with_backend(
        backend: Arc<dyn NotificationBackend>,
        platform: DashboardPlatformCapabilities,
    ) -> Self {
        Self { backend, platform }
    }

    /// Probe without requesting permission or displaying a notification.
    pub(crate) fn start_probe(self: &Arc<Self>) {
        let host = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            host.refresh_availability().await;
        });
    }

    async fn refresh_availability(&self) {
        let backend = Arc::clone(&self.backend);
        let availability = tokio::task::spawn_blocking(move || backend.probe())
            .await
            .ok()
            .and_then(Result::ok)
            .map_or(
                SystemNotificationAvailability::Unavailable,
                |permission| match permission {
                    NativeNotificationPermission::Granted => {
                        SystemNotificationAvailability::Available
                    }
                    NativeNotificationPermission::Prompt => {
                        SystemNotificationAvailability::PermissionRequired
                    }
                    NativeNotificationPermission::Denied => {
                        SystemNotificationAvailability::Unavailable
                    }
                },
            );
        self.platform.set_system_notifications(availability);
    }
}

impl ActionExecutor for TauriSystemNotificationHost {
    fn action_type(&self) -> RegisteredActionType {
        RegisteredActionType::SystemNotification
    }

    fn capabilities(&self) -> &'static [ActionCapability] {
        SYSTEM_NOTIFICATION_DEFINITION.capabilities
    }

    fn permissions(&self) -> &'static [ActionPermission] {
        SYSTEM_NOTIFICATION_DEFINITION.permissions
    }

    fn execute<'a>(
        &'a self,
        parameters: &'a ActionParameters,
        context: ActionExecutionContext,
    ) -> ActionFuture<'a> {
        let ActionParameters::SystemNotification(parameters) = parameters;
        let message = render_notification(parameters, context);
        let backend = Arc::clone(&self.backend);
        let platform = self.platform.clone();
        Box::pin(async move {
            let outcome = tokio::task::spawn_blocking(move || {
                execute_notification(backend.as_ref(), &message)
            })
            .await
            .unwrap_or_else(|_| NotificationExecutionOutcome::unavailable());
            platform.set_system_notifications(outcome.availability);
            outcome.result
        })
    }
}

fn execute_notification(
    backend: &dyn NotificationBackend,
    message: &NotificationMessage,
) -> NotificationExecutionOutcome {
    let permission = match backend.probe() {
        Ok(permission) => permission,
        Err(NotificationBackendError::PermissionDenied) => {
            return NotificationExecutionOutcome::permission_denied();
        }
        Err(NotificationBackendError::Unavailable | NotificationBackendError::DispatchFailed) => {
            return NotificationExecutionOutcome::unavailable();
        }
    };
    let permission = match permission {
        NativeNotificationPermission::Prompt => match backend.request_permission() {
            Ok(permission) => permission,
            Err(NotificationBackendError::PermissionDenied) => {
                return NotificationExecutionOutcome::permission_denied();
            }
            Err(
                NotificationBackendError::Unavailable | NotificationBackendError::DispatchFailed,
            ) => return NotificationExecutionOutcome::unavailable(),
        },
        permission => permission,
    };
    match permission {
        NativeNotificationPermission::Granted => {}
        NativeNotificationPermission::Prompt | NativeNotificationPermission::Denied => {
            return NotificationExecutionOutcome::permission_denied();
        }
    }

    match backend.send(message) {
        Ok(()) => NotificationExecutionOutcome::completed(),
        Err(NotificationBackendError::PermissionDenied) => {
            NotificationExecutionOutcome::permission_denied()
        }
        Err(NotificationBackendError::Unavailable) => NotificationExecutionOutcome::unavailable(),
        Err(NotificationBackendError::DispatchFailed) => {
            NotificationExecutionOutcome::dispatch_failed()
        }
    }
}

fn render_notification(
    parameters: &SystemNotificationParams,
    context: ActionExecutionContext,
) -> NotificationMessage {
    let locale = match context.locale() {
        ActionLocale::EnUs => AppLocale::EnUs,
        ActionLocale::ZhCn => AppLocale::ZhCn,
    };
    let pattern = &catalog(host_locale(locale)).notification.reasoning_pattern;
    let model = parameters
        .model()
        .map(codex_monitor_core::ModelName::as_str);
    let (title, body_template) = match (context.grouped(), model.is_some()) {
        (false, true) => (&pattern.title, &pattern.body),
        (false, false) => (&pattern.title, &pattern.body_without_model),
        (true, true) => (&pattern.coalesced_title, &pattern.coalesced_body),
        (true, false) => (
            &pattern.coalesced_title,
            &pattern.coalesced_body_without_model,
        ),
    };
    let mut body = body_template
        .replace(
            "{{reasoningTokens}}",
            &parameters.reasoning_output_tokens().get().to_string(),
        )
        .replace("{{patternN}}", &parameters.pattern_n().get().to_string());
    if let Some(model) = model {
        body = body.replace("{{model}}", model);
    }
    debug_assert!(
        !body.contains("{{"),
        "host notification catalog is incomplete"
    );
    NotificationMessage {
        title: title.clone(),
        body,
    }
}

const fn host_locale(value: AppLocale) -> HostLocale {
    match value {
        AppLocale::EnUs => HostLocale::EnUs,
        AppLocale::ZhCn => HostLocale::ZhCn,
    }
}

struct PlatformNotificationBackend {
    #[cfg(target_os = "linux")]
    application_name: String,
    #[cfg(target_os = "windows")]
    application_id: String,
    #[cfg(target_os = "windows")]
    notification_identity_ready: bool,
}

#[cfg(target_os = "windows")]
impl PlatformNotificationBackend {
    fn new(
        _application_name: String,
        application_id: String,
        notification_identity_ready: bool,
    ) -> Self {
        Self {
            application_id,
            notification_identity_ready,
        }
    }
}

#[cfg(target_os = "windows")]
impl NotificationBackend for PlatformNotificationBackend {
    fn probe(&self) -> Result<NativeNotificationPermission, NotificationBackendError> {
        if self.notification_identity_ready {
            Ok(NativeNotificationPermission::Granted)
        } else {
            Err(NotificationBackendError::Unavailable)
        }
    }

    fn request_permission(&self) -> Result<NativeNotificationPermission, NotificationBackendError> {
        self.probe()
    }

    fn send(&self, message: &NotificationMessage) -> Result<(), NotificationBackendError> {
        if !self.notification_identity_ready {
            return Err(NotificationBackendError::Unavailable);
        }
        notify_rust::Notification::new()
            .summary(&message.title)
            .body(&message.body)
            .app_id(&self.application_id)
            .show()
            .map(drop)
            .map_err(|_| NotificationBackendError::DispatchFailed)
    }
}

#[cfg(target_os = "linux")]
impl PlatformNotificationBackend {
    fn new(
        application_name: String,
        _application_id: String,
        _notification_identity_ready: bool,
    ) -> Self {
        Self { application_name }
    }
}

#[cfg(target_os = "linux")]
impl NotificationBackend for PlatformNotificationBackend {
    fn probe(&self) -> Result<NativeNotificationPermission, NotificationBackendError> {
        notify_rust::get_server_information()
            .map(|_| NativeNotificationPermission::Granted)
            .map_err(|_| NotificationBackendError::Unavailable)
    }

    fn request_permission(&self) -> Result<NativeNotificationPermission, NotificationBackendError> {
        self.probe()
    }

    fn send(&self, message: &NotificationMessage) -> Result<(), NotificationBackendError> {
        if notify_rust::Notification::new()
            .appname(&self.application_name)
            .summary(&message.title)
            .body(&message.body)
            .show()
            .is_ok()
        {
            return Ok(());
        }
        match self.probe() {
            Ok(NativeNotificationPermission::Granted) => {
                Err(NotificationBackendError::DispatchFailed)
            }
            Ok(NativeNotificationPermission::Prompt | NativeNotificationPermission::Denied) => {
                Err(NotificationBackendError::PermissionDenied)
            }
            Err(_) => Err(NotificationBackendError::Unavailable),
        }
    }
}

#[cfg(target_os = "macos")]
impl PlatformNotificationBackend {
    fn new(
        _application_name: String,
        _application_id: String,
        _notification_identity_ready: bool,
    ) -> Self {
        Self {}
    }
}

#[cfg(target_os = "macos")]
impl NotificationBackend for PlatformNotificationBackend {
    fn probe(&self) -> Result<NativeNotificationPermission, NotificationBackendError> {
        use mac_usernotifications::AuthorizationStatus;

        let settings = notify_rust::get_notification_settings_blocking()
            .map_err(|_| NotificationBackendError::Unavailable)?;
        match settings.authorization_status {
            AuthorizationStatus::NotDetermined => Ok(NativeNotificationPermission::Prompt),
            AuthorizationStatus::Denied => Ok(NativeNotificationPermission::Denied),
            AuthorizationStatus::Authorized
            | AuthorizationStatus::Provisional
            | AuthorizationStatus::Ephemeral => Ok(NativeNotificationPermission::Granted),
            AuthorizationStatus::Unknown => Err(NotificationBackendError::Unavailable),
        }
    }

    fn request_permission(&self) -> Result<NativeNotificationPermission, NotificationBackendError> {
        notify_rust::request_auth_blocking().map_err(|_| NotificationBackendError::Unavailable)?;
        self.probe()
    }

    fn send(&self, message: &NotificationMessage) -> Result<(), NotificationBackendError> {
        if notify_rust::Notification::new()
            .summary(&message.title)
            .body(&message.body)
            .show()
            .is_ok()
        {
            return Ok(());
        }
        match self.probe() {
            Ok(NativeNotificationPermission::Granted) => {
                Err(NotificationBackendError::DispatchFailed)
            }
            Ok(NativeNotificationPermission::Prompt | NativeNotificationPermission::Denied) => {
                Err(NotificationBackendError::PermissionDenied)
            }
            Err(_) => Err(NotificationBackendError::Unavailable),
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
compile_error!("CodexMonitor system notifications support Windows, macOS, and Linux only");

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use codex_monitor_core::{
        ModelName, PatternN, SourceExtraction, SourceHealth, TokenCount, UtcTimestamp,
    };
    use codex_monitor_dashboard::TrayAvailability;
    use codex_monitor_engine::actions::{ActionProcessOutcome, ActionProcessor};
    use codex_monitor_engine::rules::{RuleWorkProcessOutcome, RuleWorkProcessor};
    use codex_monitor_storage::{
        ActionLeaseWindow, AppLocale, StorageConfig, StorageWriter, StoredSourceHealth,
    };
    use codex_monitor_telemetry::{
        ObservationSink, ObservationSinkError, ObservationSinkErrorCode, OtlpLogSource,
        OtlpObservationProcessor, OtlpRequestProcessor as _,
    };
    use tempfile::tempdir;

    use super::*;

    const FORBIDDEN_PRIVACY_MARKERS: &[&str] = &[
        "synthetic-prompt-marker.example.invalid",
        "synthetic-response-marker.example.invalid",
        "synthetic-tool-arguments.example.invalid",
        "synthetic-tool-output.example.invalid",
        "synthetic-reasoning-summary.example.invalid",
        "synthetic-encrypted-reasoning.example.invalid",
        "synthetic-command.example.invalid",
        "synthetic-workspace-path.example.invalid",
        "synthetic-account-0001",
        "synthetic-user@example.invalid",
        "synthetic-api-key.example.invalid",
        "synthetic-cookie.example.invalid",
        "synthetic-auth-token.example.invalid",
        "synthetic-future-field.example.invalid",
    ];

    struct TestObservationSink {
        writer: StorageWriter,
    }

    impl ObservationSink for TestObservationSink {
        fn commit<'a>(
            &'a self,
            extraction: SourceExtraction,
            accepted_health: SourceHealth,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), ObservationSinkError>> + Send + 'a>,
        > {
            Box::pin(async move {
                self.writer
                    .commit_source_batch(extraction, StoredSourceHealth::from(&accepted_health))
                    .await
                    .map(|_| ())
                    .map_err(|_| ObservationSinkError::new(ObservationSinkErrorCode::Unavailable))
            })
        }

        fn store_health<'a>(
            &'a self,
            health: SourceHealth,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), ObservationSinkError>> + Send + 'a>,
        > {
            Box::pin(async move {
                self.writer
                    .upsert_source_health(StoredSourceHealth::from(&health))
                    .await
                    .map_err(|_| ObservationSinkError::new(ObservationSinkErrorCode::Unavailable))
            })
        }
    }

    fn at(seconds: i128) -> UtcTimestamp {
        UtcTimestamp::from_unix_timestamp_nanos(seconds * 1_000_000_000).unwrap()
    }

    struct FakeBackend {
        probe: Mutex<Result<NativeNotificationPermission, NotificationBackendError>>,
        request: Mutex<Result<NativeNotificationPermission, NotificationBackendError>>,
        send: Mutex<Result<(), NotificationBackendError>>,
        request_calls: AtomicUsize,
        send_calls: AtomicUsize,
        messages: Mutex<Vec<NotificationMessage>>,
    }

    impl FakeBackend {
        fn new(permission: NativeNotificationPermission) -> Self {
            Self {
                probe: Mutex::new(Ok(permission)),
                request: Mutex::new(Ok(permission)),
                send: Mutex::new(Ok(())),
                request_calls: AtomicUsize::new(0),
                send_calls: AtomicUsize::new(0),
                messages: Mutex::new(Vec::new()),
            }
        }

        fn set_probe(
            &self,
            result: Result<NativeNotificationPermission, NotificationBackendError>,
        ) {
            *self.probe.lock().unwrap() = result;
        }

        fn set_request(
            &self,
            result: Result<NativeNotificationPermission, NotificationBackendError>,
        ) {
            *self.request.lock().unwrap() = result;
        }

        fn set_send(&self, result: Result<(), NotificationBackendError>) {
            *self.send.lock().unwrap() = result;
        }
    }

    impl NotificationBackend for FakeBackend {
        fn probe(&self) -> Result<NativeNotificationPermission, NotificationBackendError> {
            *self.probe.lock().unwrap()
        }

        fn request_permission(
            &self,
        ) -> Result<NativeNotificationPermission, NotificationBackendError> {
            self.request_calls.fetch_add(1, Ordering::AcqRel);
            *self.request.lock().unwrap()
        }

        fn send(&self, message: &NotificationMessage) -> Result<(), NotificationBackendError> {
            self.send_calls.fetch_add(1, Ordering::AcqRel);
            self.messages.lock().unwrap().push(message.clone());
            *self.send.lock().unwrap()
        }
    }

    fn platform() -> DashboardPlatformCapabilities {
        DashboardPlatformCapabilities::new(
            false,
            SystemNotificationAvailability::Unavailable,
            TrayAvailability::Available,
        )
    }

    fn parameters(model: Option<&str>) -> ActionParameters {
        ActionParameters::SystemNotification(SystemNotificationParams::new(
            model.map(|model| ModelName::new(model).unwrap()),
            TokenCount::new(516).unwrap(),
            PatternN::new(NonZeroU64::MIN),
        ))
    }

    fn message(model: Option<&str>, locale: ActionLocale, grouped: bool) -> NotificationMessage {
        let parameters = parameters(model);
        let ActionParameters::SystemNotification(parameters) = &parameters;
        render_notification(parameters, ActionExecutionContext::new(locale, grouped))
    }

    #[test]
    fn renderer_uses_exact_bilingual_model_and_group_variants() {
        assert_eq!(
            message(Some("gpt-5.6-terra"), ActionLocale::EnUs, false),
            NotificationMessage {
                title: "Suspected reasoning-limit pattern".to_owned(),
                body: "gpt-5.6-terra produced 516 reasoning tokens (n = 1). Review or regenerate the response.".to_owned(),
            }
        );
        assert_eq!(
            message(None, ActionLocale::EnUs, false).body,
            "A model call produced 516 reasoning tokens (n = 1). Review or regenerate the response."
        );
        assert_eq!(
            message(Some("gpt-5.6-terra"), ActionLocale::ZhCn, true),
            NotificationMessage {
                title: "多个疑似推理上限模式".to_owned(),
                body: "检测到多个疑似模式。其中一次 gpt-5.6-terra 调用产生了 516 个推理 token（n = 1）。建议检查或重新生成这些回答。".to_owned(),
            }
        );
        assert_eq!(
            message(None, ActionLocale::ZhCn, true).body,
            "检测到多个疑似模式。其中一次模型调用产生了 516 个推理 token（n = 1）。建议检查或重新生成这些回答。"
        );
    }

    #[test]
    fn rendered_message_contains_only_allowlisted_notification_fields() {
        let message = message(Some("safe-model"), ActionLocale::EnUs, true);
        let rendered = format!("{}\n{}", message.title, message.body);
        assert!(rendered.contains("safe-model"));
        assert!(rendered.contains("516"));
        assert!(rendered.contains("n = 1"));
        for forbidden in [
            "private prompt",
            "private response",
            "session title",
            "C:\\private\\workspace",
            "account@example.com",
            "conversation-id",
        ] {
            assert!(!rendered.contains(forbidden));
        }
        assert!(!rendered.contains("{{"));
    }

    #[tokio::test]
    async fn forbidden_otlp_fixture_markers_never_reach_native_notification() {
        let fixture = include_str!("../../../../fixtures/otlp/unknown_fields.json");
        for marker in FORBIDDEN_PRIVACY_MARKERS {
            assert!(fixture.contains(marker), "fixture lost marker {marker}");
        }
        let fixture = fixture
            .replace("\"intValue\": \"517\"", "\"intValue\": \"516\"")
            .replace("\"conversation.id\"", "\"synthetic.unknown_conversation\"")
            .replace("\"thread.id\"", "\"synthetic.unknown_thread\"");
        let request = serde_json::from_str(&fixture).unwrap();
        let directory = tempdir().unwrap();
        let writer = StorageWriter::open(StorageConfig::new(
            directory.path().join("notification-privacy.sqlite3"),
        ))
        .unwrap();
        let sink: Arc<dyn ObservationSink> = Arc::new(TestObservationSink {
            writer: writer.clone(),
        });
        let ingestion = OtlpObservationProcessor::new(OtlpLogSource::new(at(1)).unwrap(), sink);
        ingestion.process(request, at(2)).await.unwrap();
        let rules = RuleWorkProcessor::new(writer.clone()).await.unwrap();
        assert!(matches!(
            rules.process_next(at(10), at(9)).await.unwrap(),
            RuleWorkProcessOutcome::Completed { .. }
        ));

        let backend = Arc::new(FakeBackend::new(NativeNotificationPermission::Granted));
        let host = Arc::new(TauriSystemNotificationHost::with_backend(
            backend.clone(),
            platform(),
        ));
        let actions = ActionProcessor::new(writer.clone(), host).unwrap();
        let claimed_at = UtcTimestamp::now();
        let lease_expires_at = UtcTimestamp::from_unix_timestamp_nanos(
            claimed_at.unix_timestamp_nanos() + 60_000_000_000,
        )
        .unwrap();
        assert!(matches!(
            actions
                .process_next(
                    ActionLeaseWindow::new(claimed_at, lease_expires_at).unwrap(),
                    AppLocale::EnUs
                )
                .await
                .unwrap(),
            ActionProcessOutcome::Finished(_)
        ));

        assert_eq!(backend.send_calls.load(Ordering::Acquire), 1);
        let rendered = {
            let messages = backend.messages.lock().unwrap();
            assert_eq!(messages.len(), 1);
            format!("{}\n{}", messages[0].title, messages[0].body)
        };
        assert!(rendered.contains("synthetic-model-unknown-v1"));
        assert!(rendered.contains("516"));
        for marker in FORBIDDEN_PRIVACY_MARKERS {
            assert!(!rendered.contains(marker), "notification contains {marker}");
        }
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn permission_state_is_probed_without_prompt_and_recovers_dynamically() {
        let backend = Arc::new(FakeBackend::new(NativeNotificationPermission::Prompt));
        backend.set_request(Ok(NativeNotificationPermission::Denied));
        let platform = platform();
        let host = TauriSystemNotificationHost::with_backend(backend.clone(), platform.clone());

        host.refresh_availability().await;
        assert_eq!(
            platform.system_notifications(),
            SystemNotificationAvailability::PermissionRequired
        );
        assert_eq!(backend.request_calls.load(Ordering::Acquire), 0);

        let denied = host
            .execute(
                &parameters(Some("safe-model")),
                ActionExecutionContext::new(ActionLocale::EnUs, false),
            )
            .await;
        assert_eq!(
            denied,
            ActionExecutionResult::Unavailable(ActionErrorCode::PermissionDenied)
        );
        assert_eq!(backend.request_calls.load(Ordering::Acquire), 1);
        assert_eq!(backend.send_calls.load(Ordering::Acquire), 0);
        assert_eq!(
            platform.system_notifications(),
            SystemNotificationAvailability::Unavailable
        );

        backend.set_probe(Ok(NativeNotificationPermission::Granted));
        let completed = host
            .execute(
                &parameters(Some("safe-model")),
                ActionExecutionContext::new(ActionLocale::ZhCn, false),
            )
            .await;
        assert_eq!(completed, ActionExecutionResult::Completed);
        assert_eq!(backend.send_calls.load(Ordering::Acquire), 1);
        assert_eq!(
            platform.system_notifications(),
            SystemNotificationAvailability::Available
        );
    }

    #[tokio::test]
    async fn unavailable_and_dispatch_failures_have_closed_results() {
        let backend = Arc::new(FakeBackend::new(NativeNotificationPermission::Granted));
        let platform = platform();
        let host = TauriSystemNotificationHost::with_backend(backend.clone(), platform.clone());
        let parameters = parameters(None);
        let context = ActionExecutionContext::new(ActionLocale::EnUs, false);

        backend.set_send(Err(NotificationBackendError::Unavailable));
        assert_eq!(
            host.execute(&parameters, context).await,
            ActionExecutionResult::Unavailable(ActionErrorCode::NotificationUnavailable)
        );
        assert_eq!(
            platform.system_notifications(),
            SystemNotificationAvailability::Unavailable
        );

        backend.set_send(Err(NotificationBackendError::DispatchFailed));
        assert_eq!(
            host.execute(&parameters, context).await,
            ActionExecutionResult::Failed(ActionErrorCode::NotificationDispatchFailed)
        );
        assert_eq!(
            platform.system_notifications(),
            SystemNotificationAvailability::Available
        );
    }

    #[test]
    fn adapter_preserves_the_registered_contract() {
        let host = TauriSystemNotificationHost::with_backend(
            Arc::new(FakeBackend::new(NativeNotificationPermission::Granted)),
            platform(),
        );
        assert_eq!(
            host.action_type(),
            SYSTEM_NOTIFICATION_DEFINITION.action_type
        );
        assert_eq!(
            host.capabilities(),
            SYSTEM_NOTIFICATION_DEFINITION.capabilities
        );
        assert_eq!(
            host.permissions(),
            SYSTEM_NOTIFICATION_DEFINITION.permissions
        );
    }
}
