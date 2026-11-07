use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use codex_monitor_core::UtcTimestamp;
use thiserror::Error;
use tokio::sync::{Mutex, OwnedMutexGuard, watch};

use codex_monitor_storage::{
    AppLocale, AppSettings, SettingsCoordinatorWriter, StorageError, StorageReader, StorageWriter,
    StoredSettingsDocument,
};

/// Privacy-safe failure reported by the native login-autostart adapter.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum LoginAutostartError {
    #[error("login autostart is unavailable")]
    Unavailable,
}

/// Narrow native-host port for changing current-user login autostart.
///
/// Implementations apply the requested state and return the state observed
/// from the operating system. Native error text must not cross this boundary.
pub trait LoginAutostartHost: Send + Sync + 'static {
    /// Read the current operating-system login-autostart state.
    ///
    /// # Errors
    ///
    /// Returns a closed failure when the platform state cannot be read.
    fn login_autostart(&self) -> Result<bool, LoginAutostartError>;

    /// Apply and verify the current-user login-autostart state.
    ///
    /// # Errors
    ///
    /// Returns a closed failure when the platform operation or verification is
    /// unavailable.
    fn set_login_autostart(&self, enabled: bool) -> Result<bool, LoginAutostartError>;
}

/// Cloneable coordinator for settings persistence, native effects, and live
/// process-local observation.
#[derive(Clone)]
pub struct SettingsRuntime {
    inner: Arc<SettingsRuntimeInner>,
}

struct SettingsRuntimeInner {
    storage: StorageReader,
    writer: SettingsCoordinatorWriter,
    login_autostart: Arc<dyn LoginAutostartHost>,
    updates: watch::Sender<StoredSettingsDocument>,
    mutation_slot: Arc<Mutex<()>>,
    closing: AtomicBool,
}

/// Cloneable observer for the latest durable settings document.
#[derive(Clone)]
pub struct SettingsMonitor {
    receiver: watch::Receiver<StoredSettingsDocument>,
}

/// The settings publisher stopped while an observer was waiting.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SettingsMonitorError {
    #[error("settings monitoring stopped")]
    Closed,
}

/// Stable failure categories returned by the shared settings coordinator.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SettingsRuntimeError {
    #[error("settings revision does not match the current document")]
    RevisionConflict,
    #[error("settings are invalid")]
    InvalidSettings,
    #[error("settings storage is unavailable")]
    StorageUnavailable,
    #[error("the platform settings adapter is unavailable")]
    PlatformUnavailable,
    #[error("the settings runtime is unavailable")]
    RuntimeUnavailable,
}

/// One native presentation mutation applied against the latest durable
/// settings document while preserving every unrelated field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsMutation {
    LoginAutostart(bool),
    NotificationsEnabled(bool),
}

/// Failures while creating the process-wide settings coordinator.
#[derive(Debug, Error)]
pub enum SettingsRuntimeCreateError {
    #[error("settings storage is unavailable")]
    Storage(#[from] StorageError),
    #[error("a settings coordinator already owns this SQLite writer")]
    AlreadyInitialized,
}

impl SettingsRuntime {
    /// Load the initial durable document and assemble the shared settings
    /// runtime.
    ///
    /// # Errors
    ///
    /// Returns an error when the initial settings document is missing, invalid,
    /// or unavailable, or when this writer already has its process-wide
    /// settings coordinator.
    pub fn new(
        storage: StorageReader,
        writer: &StorageWriter,
        login_autostart: Arc<dyn LoginAutostartHost>,
    ) -> Result<Self, SettingsRuntimeCreateError> {
        let initial = storage.settings()?;
        let writer = writer
            .claim_settings_coordinator()
            .ok_or(SettingsRuntimeCreateError::AlreadyInitialized)?;
        let (updates, _) = watch::channel(initial);
        Ok(Self {
            inner: Arc::new(SettingsRuntimeInner {
                storage,
                writer,
                login_autostart,
                updates,
                mutation_slot: Arc::new(Mutex::new(())),
                closing: AtomicBool::new(false),
            }),
        })
    }

    /// Subscribe to the latest durable settings document.
    #[must_use]
    pub fn monitor(&self) -> SettingsMonitor {
        SettingsMonitor {
            receiver: self.inner.updates.subscribe(),
        }
    }

    /// Read the current durable document and refresh process-local observers.
    ///
    /// # Errors
    ///
    /// Returns a closed runtime or storage failure category.
    pub async fn load(&self) -> Result<StoredSettingsDocument, SettingsRuntimeError> {
        let document = self.read_storage().await?;
        self.publish(document.clone());
        Ok(document)
    }

    /// Persist the first supported system or browser locale exactly once.
    ///
    /// A later full settings replacement remains authoritative. Concurrent
    /// first-run callers serialize through the same process-wide mutation slot,
    /// so only the first detected locale advances the durable revision.
    ///
    /// # Errors
    ///
    /// Returns a storage or runtime failure when the initial locale cannot be
    /// committed or observed safely.
    pub async fn initialize_locale(
        &self,
        detected_locale: AppLocale,
    ) -> Result<StoredSettingsDocument, SettingsRuntimeError> {
        let permit = self.acquire_mutation().await?;
        let runtime = self.clone();

        tokio::spawn(async move {
            let _permit = permit;
            runtime.initialize_locale_inner(detected_locale).await
        })
        .await
        .map_err(|_| SettingsRuntimeError::RuntimeUnavailable)?
    }

    /// Reconcile the durable login-autostart choice with the operating system
    /// before Dashboard and native controls are exposed.
    ///
    /// # Errors
    ///
    /// Returns a closed platform, storage, or runtime failure. The durable
    /// document remains authoritative and is not revised by reconciliation.
    pub async fn reconcile_login_autostart(
        &self,
    ) -> Result<StoredSettingsDocument, SettingsRuntimeError> {
        let permit = self.acquire_mutation().await?;
        let runtime = self.clone();

        tokio::spawn(async move {
            let _permit = permit;
            let document = runtime.read_storage().await?;
            runtime
                .apply_login_autostart(document.settings.login_autostart)
                .await?;
            runtime.publish(document.clone());
            Ok(document)
        })
        .await
        .map_err(|_| SettingsRuntimeError::RuntimeUnavailable)?
    }

    async fn initialize_locale_inner(
        &self,
        detected_locale: AppLocale,
    ) -> Result<StoredSettingsDocument, SettingsRuntimeError> {
        let current = self.read_storage().await?;
        if current.locale_initialized {
            self.publish(current.clone());
            return Ok(current);
        }

        let mut replacement = current.settings;
        replacement.locale = detected_locale;
        replacement.updated_at = next_applied_at(replacement.updated_at)?;
        match self
            .inner
            .writer
            .replace_settings(current.revision, replacement.clone())
            .await
        {
            Ok(document) => {
                self.publish(document.clone());
                Ok(document)
            }
            Err(error) => {
                if let Ok(document) = self.read_storage().await
                    && document.locale_initialized
                {
                    self.publish(document.clone());
                    return Ok(document);
                }
                if matches!(error, StorageError::SettingsRevisionConflict) {
                    Err(SettingsRuntimeError::RevisionConflict)
                } else {
                    Err(SettingsRuntimeError::StorageUnavailable)
                }
            }
        }
    }

    /// Validate and replace the complete settings document with optimistic
    /// concurrency and native autostart compensation.
    ///
    /// The operation continues to completion after it has acquired the mutation
    /// slot even if an individual HTTP or tray caller is cancelled.
    ///
    /// # Errors
    ///
    /// Returns a stable validation, revision, storage, platform, or runtime
    /// failure category. No update is published before persistence commits.
    pub async fn replace(
        &self,
        expected_revision: u64,
        replacement: AppSettings,
    ) -> Result<StoredSettingsDocument, SettingsRuntimeError> {
        let permit = self.acquire_mutation().await?;
        let runtime = self.clone();

        tokio::spawn(async move {
            let _permit = permit;
            runtime.replace_inner(expected_revision, replacement).await
        })
        .await
        .map_err(|_| SettingsRuntimeError::RuntimeUnavailable)?
    }

    /// Apply one native-menu setting against the latest durable revision.
    ///
    /// This operation shares the same mutation slot as full Dashboard
    /// replacements. It therefore cannot overwrite an unrelated field from a
    /// concurrent Dashboard commit. Notification-only changes do not require
    /// the login-autostart adapter to be available.
    ///
    /// # Errors
    ///
    /// Returns a stable storage, platform, validation, or runtime category.
    pub async fn mutate(
        &self,
        mutation: SettingsMutation,
    ) -> Result<StoredSettingsDocument, SettingsRuntimeError> {
        let permit = self.acquire_mutation().await?;
        let runtime = self.clone();

        tokio::spawn(async move {
            let _permit = permit;
            let current = runtime.read_storage().await?;
            let revision = current.revision;
            let mut replacement = current.settings;
            match mutation {
                SettingsMutation::LoginAutostart(enabled) => {
                    replacement.login_autostart = enabled;
                }
                SettingsMutation::NotificationsEnabled(enabled) => {
                    replacement.notifications_enabled = enabled;
                }
            }
            runtime.replace_inner(revision, replacement).await
        })
        .await
        .map_err(|_| SettingsRuntimeError::RuntimeUnavailable)?
    }

    /// Reject future setting mutations and wait for the acquired mutation to
    /// finish before storage shutdown begins.
    pub async fn shutdown(&self) {
        self.begin_shutdown();
        let guard = Arc::clone(&self.inner.mutation_slot).lock_owned().await;
        drop(guard);
    }

    pub(crate) fn begin_shutdown(&self) {
        self.inner.closing.store(true, Ordering::Release);
    }

    async fn acquire_mutation(&self) -> Result<OwnedMutexGuard<()>, SettingsRuntimeError> {
        if self.inner.closing.load(Ordering::Acquire) {
            return Err(SettingsRuntimeError::RuntimeUnavailable);
        }
        let permit = Arc::clone(&self.inner.mutation_slot).lock_owned().await;
        if self.inner.closing.load(Ordering::Acquire) {
            return Err(SettingsRuntimeError::RuntimeUnavailable);
        }
        Ok(permit)
    }

    async fn replace_inner(
        &self,
        expected_revision: u64,
        mut replacement: AppSettings,
    ) -> Result<StoredSettingsDocument, SettingsRuntimeError> {
        replacement
            .validate()
            .map_err(|_| SettingsRuntimeError::InvalidSettings)?;
        let current = self.read_storage().await?;
        if current.revision != expected_revision {
            return Err(SettingsRuntimeError::RevisionConflict);
        }

        replacement.updated_at = next_applied_at(current.settings.updated_at)?;
        let autostart_baseline = if replacement.login_autostart == current.settings.login_autostart
        {
            None
        } else {
            Some(self.read_login_autostart().await?)
        };
        if let Some(autostart_baseline) = autostart_baseline
            && self
                .apply_login_autostart(replacement.login_autostart)
                .await
                .is_err()
        {
            let _ = self.apply_login_autostart(autostart_baseline).await;
            return Err(SettingsRuntimeError::PlatformUnavailable);
        }

        match self
            .inner
            .writer
            .replace_settings(expected_revision, replacement.clone())
            .await
        {
            Ok(document) => {
                self.publish(document.clone());
                Ok(document)
            }
            Err(error) => {
                let durable = self.read_storage().await.ok();
                if !matches!(error, StorageError::SettingsRevisionConflict)
                    && let Some(document) = durable.as_ref()
                    && document.revision == expected_revision.saturating_add(1)
                    && document.settings == replacement
                {
                    let document = document.clone();
                    self.publish(document.clone());
                    return Ok(document);
                }

                if let Some(autostart_baseline) = autostart_baseline
                    && self
                        .apply_login_autostart(autostart_baseline)
                        .await
                        .is_err()
                {
                    return Err(SettingsRuntimeError::PlatformUnavailable);
                }

                if matches!(error, StorageError::SettingsRevisionConflict) {
                    Err(SettingsRuntimeError::RevisionConflict)
                } else {
                    Err(SettingsRuntimeError::StorageUnavailable)
                }
            }
        }
    }

    async fn read_storage(&self) -> Result<StoredSettingsDocument, SettingsRuntimeError> {
        let storage = self.inner.storage.clone();
        tokio::task::spawn_blocking(move || storage.settings())
            .await
            .map_err(|_| SettingsRuntimeError::RuntimeUnavailable)?
            .map_err(|_| SettingsRuntimeError::StorageUnavailable)
    }

    async fn apply_login_autostart(&self, enabled: bool) -> Result<(), SettingsRuntimeError> {
        let host = Arc::clone(&self.inner.login_autostart);
        let observed = tokio::task::spawn_blocking(move || host.set_login_autostart(enabled))
            .await
            .map_err(|_| SettingsRuntimeError::RuntimeUnavailable)?
            .map_err(|_| SettingsRuntimeError::PlatformUnavailable)?;
        if observed == enabled {
            Ok(())
        } else {
            Err(SettingsRuntimeError::PlatformUnavailable)
        }
    }

    async fn read_login_autostart(&self) -> Result<bool, SettingsRuntimeError> {
        let host = Arc::clone(&self.inner.login_autostart);
        tokio::task::spawn_blocking(move || host.login_autostart())
            .await
            .map_err(|_| SettingsRuntimeError::RuntimeUnavailable)?
            .map_err(|_| SettingsRuntimeError::PlatformUnavailable)
    }

    fn publish(&self, document: StoredSettingsDocument) {
        self.inner.updates.send_if_modified(|current| {
            if document.revision > current.revision {
                *current = document;
                true
            } else {
                false
            }
        });
    }
}

impl SettingsMonitor {
    /// Return the latest process-local durable settings snapshot.
    #[must_use]
    pub fn snapshot(&self) -> StoredSettingsDocument {
        self.receiver.borrow().clone()
    }

    /// Wait for and return the next durable settings snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsMonitorError::Closed`] when the owning runtime has
    /// stopped.
    pub async fn changed(&mut self) -> Result<StoredSettingsDocument, SettingsMonitorError> {
        self.receiver
            .changed()
            .await
            .map_err(|_| SettingsMonitorError::Closed)?;
        Ok(self.snapshot())
    }
}

fn next_applied_at(previous: UtcTimestamp) -> Result<UtcTimestamp, SettingsRuntimeError> {
    let now = UtcTimestamp::now();
    if now > previous {
        return Ok(now);
    }
    previous
        .unix_timestamp_nanos()
        .checked_add(1)
        .ok_or(SettingsRuntimeError::RuntimeUnavailable)
        .and_then(|value| {
            UtcTimestamp::from_unix_timestamp_nanos(value)
                .map_err(|_| SettingsRuntimeError::RuntimeUnavailable)
        })
}

#[cfg(test)]
pub(crate) struct EchoLoginAutostartHost;

#[cfg(test)]
impl LoginAutostartHost for EchoLoginAutostartHost {
    fn login_autostart(&self) -> Result<bool, LoginAutostartError> {
        Ok(false)
    }

    fn set_login_autostart(&self, enabled: bool) -> Result<bool, LoginAutostartError> {
        Ok(enabled)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use tempfile::tempdir;

    use super::*;
    use codex_monitor_storage::StorageConfig;

    struct StatefulLoginAutostartHost {
        enabled: AtomicBool,
        writes: AtomicUsize,
    }

    impl StatefulLoginAutostartHost {
        fn new(enabled: bool) -> Self {
            Self {
                enabled: AtomicBool::new(enabled),
                writes: AtomicUsize::new(0),
            }
        }

        fn enabled(&self) -> bool {
            self.enabled.load(Ordering::Acquire)
        }

        fn writes(&self) -> usize {
            self.writes.load(Ordering::Acquire)
        }

        fn reset_writes(&self) {
            self.writes.store(0, Ordering::Release);
        }
    }

    impl LoginAutostartHost for StatefulLoginAutostartHost {
        fn login_autostart(&self) -> Result<bool, LoginAutostartError> {
            Ok(self.enabled())
        }

        fn set_login_autostart(&self, enabled: bool) -> Result<bool, LoginAutostartError> {
            self.writes.fetch_add(1, Ordering::AcqRel);
            self.enabled.store(enabled, Ordering::Release);
            Ok(enabled)
        }
    }

    struct BlockingLoginAutostartHost {
        enabled: AtomicBool,
        entered: std::sync::mpsc::SyncSender<()>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl LoginAutostartHost for BlockingLoginAutostartHost {
        fn login_autostart(&self) -> Result<bool, LoginAutostartError> {
            Ok(self.enabled.load(Ordering::Acquire))
        }

        fn set_login_autostart(&self, enabled: bool) -> Result<bool, LoginAutostartError> {
            self.entered
                .send(())
                .map_err(|_| LoginAutostartError::Unavailable)?;
            self.release
                .lock()
                .map_err(|_| LoginAutostartError::Unavailable)?
                .recv()
                .map_err(|_| LoginAutostartError::Unavailable)?;
            self.enabled.store(enabled, Ordering::Release);
            Ok(enabled)
        }
    }

    struct UnavailableLoginAutostartHost;

    impl LoginAutostartHost for UnavailableLoginAutostartHost {
        fn login_autostart(&self) -> Result<bool, LoginAutostartError> {
            Err(LoginAutostartError::Unavailable)
        }

        fn set_login_autostart(&self, _enabled: bool) -> Result<bool, LoginAutostartError> {
            Err(LoginAutostartError::Unavailable)
        }
    }

    #[tokio::test]
    async fn one_writer_can_have_only_one_settings_coordinator() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("settings-runtime.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let reader = StorageReader::new(database);

        let runtime =
            SettingsRuntime::new(reader.clone(), &writer, Arc::new(EchoLoginAutostartHost))
                .unwrap();
        let duplicate = SettingsRuntime::new(reader, &writer, Arc::new(EchoLoginAutostartHost));

        assert!(matches!(
            duplicate,
            Err(SettingsRuntimeCreateError::AlreadyInitialized)
        ));
        drop(runtime);
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn first_detected_locale_is_persisted_and_published_once() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("locale-initialization.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let reader = StorageReader::new(database);
        let runtime =
            SettingsRuntime::new(reader.clone(), &writer, Arc::new(EchoLoginAutostartHost))
                .unwrap();
        let mut monitor = runtime.monitor();

        assert!(!monitor.snapshot().locale_initialized);
        let initialized = runtime.initialize_locale(AppLocale::ZhCn).await.unwrap();
        assert_eq!(initialized.revision, 2);
        assert_eq!(initialized.settings.locale, AppLocale::ZhCn);
        assert!(initialized.locale_initialized);
        assert_eq!(monitor.changed().await.unwrap(), initialized);

        let replay = runtime.initialize_locale(AppLocale::EnUs).await.unwrap();
        assert_eq!(replay, initialized);
        assert_eq!(reader.settings().unwrap(), initialized);

        drop(runtime);
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn reconciliation_applies_durable_autostart_without_revising_settings() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("autostart-reconciliation.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let reader = StorageReader::new(database);
        let host = Arc::new(StatefulLoginAutostartHost::new(true));
        let host_port: Arc<dyn LoginAutostartHost> = host.clone();
        let runtime = SettingsRuntime::new(reader.clone(), &writer, host_port).unwrap();
        let durable = reader.settings().unwrap();

        assert!(!durable.settings.login_autostart);
        let reconciled = runtime.reconcile_login_autostart().await.unwrap();

        assert_eq!(reconciled, durable);
        assert_eq!(reader.settings().unwrap(), durable);
        assert!(!host.enabled());
        assert_eq!(host.writes(), 1);
        assert_eq!(runtime.monitor().snapshot().revision, durable.revision);

        runtime.shutdown().await;
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn reconciliation_refreshes_an_enabled_registration_without_revising_settings() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("enabled-autostart-refresh.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let reader = StorageReader::new(database);
        let host = Arc::new(StatefulLoginAutostartHost::new(false));
        let host_port: Arc<dyn LoginAutostartHost> = host.clone();
        let runtime = SettingsRuntime::new(reader.clone(), &writer, host_port).unwrap();

        let current = reader.settings().unwrap();
        let mut replacement = current.settings;
        replacement.login_autostart = true;
        let durable = runtime
            .replace(current.revision, replacement)
            .await
            .unwrap();
        assert_eq!(host.writes(), 1);
        host.reset_writes();

        let reconciled = runtime.reconcile_login_autostart().await.unwrap();

        assert_eq!(reconciled, durable);
        assert_eq!(reader.settings().unwrap(), durable);
        assert!(host.enabled());
        assert_eq!(host.writes(), 1);

        runtime.shutdown().await;
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn unrelated_full_replacement_does_not_depend_on_autostart_availability() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("unrelated-full-settings.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let reader = StorageReader::new(database);
        let runtime = SettingsRuntime::new(
            reader.clone(),
            &writer,
            Arc::new(UnavailableLoginAutostartHost),
        )
        .unwrap();

        let current = reader.settings().unwrap();
        let mut replacement = current.settings;
        replacement.locale = AppLocale::ZhCn;
        let document = runtime
            .replace(current.revision, replacement)
            .await
            .unwrap();

        assert_eq!(document.settings.locale, AppLocale::ZhCn);
        assert!(!document.settings.login_autostart);
        assert_eq!(reader.settings().unwrap(), document);

        runtime.shutdown().await;
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn explicit_enable_rewrites_an_already_enabled_native_registration() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("autostart-explicit-rewrite.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let reader = StorageReader::new(database);
        let host = Arc::new(StatefulLoginAutostartHost::new(true));
        let host_port: Arc<dyn LoginAutostartHost> = host.clone();
        let runtime = SettingsRuntime::new(reader.clone(), &writer, host_port).unwrap();

        let current = reader.settings().unwrap();
        let mut replacement = current.settings;
        replacement.login_autostart = true;
        let document = runtime
            .replace(current.revision, replacement)
            .await
            .unwrap();

        assert!(document.settings.login_autostart);
        assert!(host.enabled());
        assert_eq!(host.writes(), 1);

        runtime.shutdown().await;
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn targeted_mutations_preserve_every_unrelated_latest_field() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("targeted-settings.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let reader = StorageReader::new(database);
        let host = Arc::new(StatefulLoginAutostartHost::new(false));
        let host_port: Arc<dyn LoginAutostartHost> = host.clone();
        let runtime = SettingsRuntime::new(reader.clone(), &writer, host_port).unwrap();

        let current = reader.settings().unwrap();
        let mut dashboard_replacement = current.settings;
        dashboard_replacement.locale = AppLocale::ZhCn;
        dashboard_replacement.retention = codex_monitor_storage::RetentionPolicy::Days365;
        let dashboard_document = runtime
            .replace(current.revision, dashboard_replacement)
            .await
            .unwrap();

        let notification_document = runtime
            .mutate(SettingsMutation::NotificationsEnabled(false))
            .await
            .unwrap();
        assert!(!notification_document.settings.notifications_enabled);
        assert_eq!(notification_document.settings.locale, AppLocale::ZhCn);
        assert_eq!(
            notification_document.settings.retention,
            codex_monitor_storage::RetentionPolicy::Days365
        );
        assert_eq!(
            notification_document.revision,
            dashboard_document.revision + 1
        );

        let autostart_document = runtime
            .mutate(SettingsMutation::LoginAutostart(true))
            .await
            .unwrap();
        assert!(autostart_document.settings.login_autostart);
        assert!(!autostart_document.settings.notifications_enabled);
        assert_eq!(autostart_document.settings.locale, AppLocale::ZhCn);
        assert!(host.enabled());

        runtime.shutdown().await;
        writer.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn targeted_mutation_waits_for_and_preserves_a_concurrent_full_commit() {
        let directory = tempdir().unwrap();
        let database = directory
            .path()
            .join("concurrent-targeted-settings.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let reader = StorageReader::new(database);
        let (entered_sender, entered_receiver) = std::sync::mpsc::sync_channel(1);
        let (release_sender, release_receiver) = std::sync::mpsc::sync_channel(1);
        let host: Arc<dyn LoginAutostartHost> = Arc::new(BlockingLoginAutostartHost {
            enabled: AtomicBool::new(false),
            entered: entered_sender,
            release: std::sync::Mutex::new(release_receiver),
        });
        let runtime = SettingsRuntime::new(reader.clone(), &writer, host).unwrap();
        let current = reader.settings().unwrap();
        let revision = current.revision;
        let mut replacement = current.settings;
        replacement.login_autostart = true;
        replacement.locale = AppLocale::ZhCn;

        let full_runtime = runtime.clone();
        let full = tokio::spawn(async move { full_runtime.replace(revision, replacement).await });
        tokio::time::timeout(
            Duration::from_secs(1),
            tokio::task::spawn_blocking(move || entered_receiver.recv()),
        )
        .await
        .expect("the full replacement must hold the shared mutation slot")
        .unwrap()
        .unwrap();

        let targeted_runtime = runtime.clone();
        let mut targeted = tokio::spawn(async move {
            targeted_runtime
                .mutate(SettingsMutation::NotificationsEnabled(false))
                .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut targeted)
                .await
                .is_err(),
            "the targeted mutation must wait behind the full replacement"
        );

        release_sender.send(()).unwrap();
        let full = full.await.unwrap().unwrap();
        let targeted = targeted.await.unwrap().unwrap();
        assert_eq!(targeted.revision, full.revision + 1);
        assert!(targeted.settings.login_autostart);
        assert_eq!(targeted.settings.locale, AppLocale::ZhCn);
        assert!(!targeted.settings.notifications_enabled);
        assert_eq!(reader.settings().unwrap(), targeted);

        runtime.shutdown().await;
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn notification_mutation_does_not_depend_on_autostart_availability() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("notification-only-settings.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let reader = StorageReader::new(database);
        let runtime = SettingsRuntime::new(
            reader.clone(),
            &writer,
            Arc::new(UnavailableLoginAutostartHost),
        )
        .unwrap();

        let document = runtime
            .mutate(SettingsMutation::NotificationsEnabled(false))
            .await
            .unwrap();
        assert!(!document.settings.notifications_enabled);
        assert!(!document.settings.login_autostart);
        assert_eq!(reader.settings().unwrap(), document);

        runtime.shutdown().await;
        writer.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_drains_an_acquired_mutation_and_rejects_future_mutations() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("settings-shutdown-drain.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();
        let reader = StorageReader::new(database);
        let (entered_sender, entered_receiver) = std::sync::mpsc::sync_channel(1);
        let (release_sender, release_receiver) = std::sync::mpsc::sync_channel(1);
        let host: Arc<dyn LoginAutostartHost> = Arc::new(BlockingLoginAutostartHost {
            enabled: AtomicBool::new(false),
            entered: entered_sender,
            release: std::sync::Mutex::new(release_receiver),
        });
        let runtime = SettingsRuntime::new(reader.clone(), &writer, host).unwrap();
        let current = reader.settings().unwrap();
        let mut replacement = current.settings;
        replacement.login_autostart = true;

        let replacement_runtime = runtime.clone();
        let replacement_task = tokio::spawn(async move {
            replacement_runtime
                .replace(current.revision, replacement)
                .await
        });
        tokio::time::timeout(
            Duration::from_secs(1),
            tokio::task::spawn_blocking(move || entered_receiver.recv()),
        )
        .await
        .expect("the native mutation must start")
        .unwrap()
        .unwrap();

        let shutdown_runtime = runtime.clone();
        let mut shutdown_task = tokio::spawn(async move { shutdown_runtime.shutdown().await });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut shutdown_task)
                .await
                .is_err(),
            "shutdown must wait for the acquired mutation"
        );

        release_sender.send(()).unwrap();
        let replaced = tokio::time::timeout(Duration::from_secs(1), replacement_task)
            .await
            .expect("the released mutation must finish")
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), shutdown_task)
            .await
            .expect("shutdown must finish after the mutation")
            .unwrap();
        assert_eq!(
            runtime
                .replace(replaced.revision, replaced.settings)
                .await
                .unwrap_err(),
            SettingsRuntimeError::RuntimeUnavailable
        );

        writer.shutdown().await.unwrap();
    }
}
