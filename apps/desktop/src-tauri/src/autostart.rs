#[cfg(target_os = "windows")]
use std::ffi::{OsStr, OsString};
#[cfg(target_os = "windows")]
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use codex_monitor_dashboard::DashboardPlatformCapabilities;
use codex_monitor_engine::{LoginAutostartError, LoginAutostartHost};
use tauri::{AppHandle, Wry};

use codex_monitor_host::BACKGROUND_ARGUMENT;

const APPLICATION_NAME: &str = "CodexMonitor";
#[cfg(any(test, target_os = "linux", target_os = "macos"))]
const AUTOSTART_IDENTIFIER: &str = "dev.codexmonitor.desktop";
const AUTOSTART_ARGUMENT: &str = BACKGROUND_ARGUMENT;

trait LoginAutostartBackend: Send + Sync + 'static {
    fn is_enabled(&self) -> Result<bool, LoginAutostartError>;
    fn set_enabled(&self, enabled: bool) -> Result<(), LoginAutostartError>;
}

pub(crate) struct TauriLoginAutostartHost {
    backend: Option<Arc<dyn LoginAutostartBackend>>,
    platform: DashboardPlatformCapabilities,
    operation_failure_latched: AtomicBool,
}

impl TauriLoginAutostartHost {
    pub(crate) fn new(app: &AppHandle<Wry>, platform: DashboardPlatformCapabilities) -> Self {
        let backend = platform_backend::NativeLoginAutostartBackend::new(app)
            .ok()
            .map(|backend| Arc::new(backend) as Arc<dyn LoginAutostartBackend>);
        Self {
            backend,
            platform,
            operation_failure_latched: AtomicBool::new(false),
        }
    }

    #[cfg(test)]
    fn with_backend(
        backend: Arc<dyn LoginAutostartBackend>,
        platform: DashboardPlatformCapabilities,
    ) -> Self {
        Self {
            backend: Some(backend),
            platform,
            operation_failure_latched: AtomicBool::new(false),
        }
    }

    fn publish_available(&self, available: bool) {
        self.platform.set_login_autostart_available(available);
    }

    fn unavailable<T>(&self) -> Result<T, LoginAutostartError> {
        self.operation_failure_latched
            .store(true, Ordering::Release);
        self.publish_available(false);
        Err(LoginAutostartError::Unavailable)
    }
}

impl LoginAutostartHost for TauriLoginAutostartHost {
    fn login_autostart(&self) -> Result<bool, LoginAutostartError> {
        let Some(backend) = self.backend.as_ref() else {
            return self.unavailable();
        };
        let observed = backend.is_enabled();
        if observed.is_ok() {
            self.operation_failure_latched
                .store(false, Ordering::Release);
            self.publish_available(true);
        } else {
            self.operation_failure_latched
                .store(true, Ordering::Release);
            self.publish_available(false);
        }
        observed
    }

    fn set_login_autostart(&self, enabled: bool) -> Result<bool, LoginAutostartError> {
        let Some(backend) = self.backend.as_ref() else {
            return self.unavailable();
        };
        if let Err(error) = backend.set_enabled(enabled) {
            self.operation_failure_latched
                .store(true, Ordering::Release);
            self.publish_available(false);
            return Err(error);
        }

        let observed = backend.is_enabled();
        let verified = observed.as_ref().is_ok_and(|observed| *observed == enabled);
        if !verified {
            self.operation_failure_latched
                .store(true, Ordering::Release);
        }
        self.publish_available(verified && !self.operation_failure_latched.load(Ordering::Acquire));
        observed
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn executable_text(path: &Path) -> Result<&str, LoginAutostartError> {
    if !path.is_absolute() || !path.is_file() {
        return Err(LoginAutostartError::Unavailable);
    }
    path.to_str().ok_or(LoginAutostartError::Unavailable)
}

#[cfg(target_os = "windows")]
pub(crate) fn windows_run_command(executable: &OsStr) -> Option<OsString> {
    let executable: Vec<u16> = executable.encode_wide().collect();
    if executable
        .iter()
        .any(|character| *character == 0 || *character == u16::from(b'"') || *character < 0x20)
    {
        return None;
    }

    let mut command = Vec::with_capacity(executable.len() + AUTOSTART_ARGUMENT.len() + 3);
    command.push(u16::from(b'"'));
    command.extend(executable);
    command.push(u16::from(b'"'));
    command.push(u16::from(b' '));
    command.extend(AUTOSTART_ARGUMENT.encode_utf16());
    (command.len() <= 260).then(|| OsString::from_wide(&command))
}

#[cfg(any(test, target_os = "linux"))]
fn linux_desktop_exec_argument(executable: &str) -> Option<String> {
    if executable.contains('=') || executable.chars().any(char::is_control) {
        return None;
    }

    let mut escaped = String::with_capacity(executable.len() + 2);
    escaped.push('"');
    for character in executable.chars() {
        match character {
            '\\' => escaped.push_str(r"\\\\"),
            '"' | '`' | '$' => {
                escaped.push_str(r"\\");
                escaped.push(character);
            }
            '%' => escaped.push_str("%%"),
            _ => escaped.push(character),
        }
    }
    escaped.push('"');
    Some(escaped)
}

#[cfg(any(test, target_os = "linux"))]
fn linux_desktop_entry(executable: &str) -> Option<String> {
    let executable = linux_desktop_exec_argument(executable)?;
    Some(format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Version=1.5\n\
         Name={APPLICATION_NAME}\n\
         Exec={executable} {AUTOSTART_ARGUMENT}\n\
         Terminal=false\n\
         StartupNotify=false\n"
    ))
}

#[cfg(any(test, target_os = "macos"))]
fn xml_text(value: &str) -> Option<String> {
    if value.chars().any(char::is_control) {
        return None;
    }
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(character),
        }
    }
    Some(escaped)
}

#[cfg(any(test, target_os = "macos"))]
fn macos_launch_agent_plist(executable: &str) -> Option<String> {
    let executable = xml_text(executable)?;
    Some(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
           <key>Label</key>\n\
           <string>{AUTOSTART_IDENTIFIER}</string>\n\
           <key>AssociatedBundleIdentifiers</key>\n\
           <array>\n\
             <string>{AUTOSTART_IDENTIFIER}</string>\n\
           </array>\n\
           <key>ProgramArguments</key>\n\
           <array>\n\
             <string>{executable}</string>\n\
             <string>{AUTOSTART_ARGUMENT}</string>\n\
           </array>\n\
           <key>RunAtLoad</key>\n\
           <true/>\n\
         </dict>\n\
         </plist>\n"
    ))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct FileLoginAutostartRegistration {
    path: std::path::PathBuf,
    content: Vec<u8>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl FileLoginAutostartRegistration {
    const MAX_FILE_BYTES: u64 = 64 * 1024;

    fn new(path: std::path::PathBuf, content: String) -> Self {
        Self {
            path,
            content: content.into_bytes(),
        }
    }

    fn is_enabled(&self) -> std::io::Result<bool> {
        use std::io::Read as _;
        use std::os::unix::fs::PermissionsExt as _;

        let metadata = match std::fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        if !metadata.file_type().is_file()
            || metadata.len() > Self::MAX_FILE_BYTES
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Ok(false);
        }

        let file = std::fs::File::open(&self.path)?;
        let mut content = Vec::with_capacity(self.content.len());
        file.take(Self::MAX_FILE_BYTES + 1)
            .read_to_end(&mut content)?;
        Ok(content == self.content)
    }

    fn set_enabled(&self, enabled: bool) -> std::io::Result<()> {
        if !enabled {
            return match std::fs::remove_file(&self.path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            };
        }

        let parent = self.path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "autostart registration has no parent directory",
            )
        })?;
        std::fs::create_dir_all(parent)?;
        let mut pending = tempfile::NamedTempFile::new_in(parent)?;
        std::io::Write::write_all(&mut pending, &self.content)?;
        use std::os::unix::fs::PermissionsExt as _;
        pending
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
        pending.as_file().sync_all()?;
        pending.persist(&self.path).map_err(|error| error.error)?;
        Ok(())
    }
}

#[cfg(target_os = "windows")]
mod platform_backend {
    use std::borrow::Cow;
    use std::ffi::OsString;
    use std::io::ErrorKind;

    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_BINARY, REG_SZ};
    use winreg::types::FromRegValue as _;
    use winreg::{RegKey, RegValue};

    use super::{
        APPLICATION_NAME, AppHandle, LoginAutostartBackend, LoginAutostartError, Wry,
        windows_run_command,
    };

    const RUN_KEY: &str = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run";
    const STARTUP_APPROVED_KEY: &str =
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run";
    // Explorer uses state 2 plus a zero FILETIME for an enabled Startup Apps override.
    // Any unknown shape is unavailable rather than an invented successful state.
    const STARTUP_APPROVED_ENABLED: [u8; 12] = [
        0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    pub(super) struct NativeLoginAutostartBackend {
        command: OsString,
    }

    impl NativeLoginAutostartBackend {
        pub(super) fn new(_app: &AppHandle<Wry>) -> Result<Self, LoginAutostartError> {
            let executable =
                std::env::current_exe().map_err(|_| LoginAutostartError::Unavailable)?;
            if !executable.is_absolute() || !executable.is_file() {
                return Err(LoginAutostartError::Unavailable);
            }
            let command = windows_run_command(executable.as_os_str())
                .ok_or(LoginAutostartError::Unavailable)?;
            Ok(Self { command })
        }

        fn task_manager_enabled(current_user: &RegKey) -> Result<bool, LoginAutostartError> {
            let key = match current_user.open_subkey_with_flags(STARTUP_APPROVED_KEY, KEY_READ) {
                Ok(key) => key,
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(true),
                Err(_) => return Err(LoginAutostartError::Unavailable),
            };
            let value = match key.get_raw_value(APPLICATION_NAME) {
                Ok(value) => value,
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(true),
                Err(_) => return Err(LoginAutostartError::Unavailable),
            };
            if value.vtype != REG_BINARY {
                return Err(LoginAutostartError::Unavailable);
            }
            startup_approved_enabled(&value.bytes).ok_or(LoginAutostartError::Unavailable)
        }

        fn enable(&self, current_user: &RegKey) -> Result<(), LoginAutostartError> {
            let (run, _) = current_user
                .create_subkey(RUN_KEY)
                .map_err(|_| LoginAutostartError::Unavailable)?;
            run.set_value(APPLICATION_NAME, &self.command)
                .map_err(|_| LoginAutostartError::Unavailable)?;
            if crate::platform_registration::record_login_autostart_ownership(&self.command)
                .is_err()
            {
                let _ = run.delete_value(APPLICATION_NAME);
                return Err(LoginAutostartError::Unavailable);
            }

            match current_user.open_subkey_with_flags(STARTUP_APPROVED_KEY, KEY_SET_VALUE) {
                Ok(key) => key
                    .set_raw_value(
                        APPLICATION_NAME,
                        &RegValue {
                            bytes: Cow::Borrowed(&STARTUP_APPROVED_ENABLED),
                            vtype: REG_BINARY,
                        },
                    )
                    .map_err(|_| LoginAutostartError::Unavailable),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
                Err(_) => Err(LoginAutostartError::Unavailable),
            }
        }

        fn disable(&self, current_user: &RegKey) -> Result<(), LoginAutostartError> {
            let run = match current_user.open_subkey_with_flags(RUN_KEY, KEY_READ | KEY_SET_VALUE) {
                Ok(run) => run,
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    return crate::platform_registration::clear_login_autostart_ownership(
                        &self.command,
                    )
                    .map_err(|_| LoginAutostartError::Unavailable);
                }
                Err(_) => return Err(LoginAutostartError::Unavailable),
            };
            let raw = match run.get_raw_value(APPLICATION_NAME) {
                Ok(raw) => raw,
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    return crate::platform_registration::clear_login_autostart_ownership(
                        &self.command,
                    )
                    .map_err(|_| LoginAutostartError::Unavailable);
                }
                Err(_) => return Err(LoginAutostartError::Unavailable),
            };
            if raw.vtype != REG_SZ {
                return Err(LoginAutostartError::Unavailable);
            }
            let command =
                OsString::from_reg_value(&raw).map_err(|_| LoginAutostartError::Unavailable)?;
            if command != self.command {
                return Err(LoginAutostartError::Unavailable);
            }
            crate::platform_registration::clear_login_autostart_ownership(&self.command)
                .map_err(|_| LoginAutostartError::Unavailable)?;
            match run.delete_value(APPLICATION_NAME) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
                Err(_) => Err(LoginAutostartError::Unavailable),
            }
        }
    }

    impl LoginAutostartBackend for NativeLoginAutostartBackend {
        fn is_enabled(&self) -> Result<bool, LoginAutostartError> {
            let current_user = RegKey::predef(HKEY_CURRENT_USER);
            let run = match current_user.open_subkey_with_flags(RUN_KEY, KEY_READ) {
                Ok(run) => run,
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
                Err(_) => return Err(LoginAutostartError::Unavailable),
            };
            let value = match run.get_raw_value(APPLICATION_NAME) {
                Ok(value) => value,
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
                Err(_) => return Err(LoginAutostartError::Unavailable),
            };
            if value.vtype != REG_SZ {
                return Ok(false);
            }
            let command =
                OsString::from_reg_value(&value).map_err(|_| LoginAutostartError::Unavailable)?;
            if command != self.command {
                return Ok(false);
            }
            Self::task_manager_enabled(&current_user)
        }

        fn set_enabled(&self, enabled: bool) -> Result<(), LoginAutostartError> {
            let current_user = RegKey::predef(HKEY_CURRENT_USER);
            if enabled {
                self.enable(&current_user)
            } else {
                self.disable(&current_user)
            }
        }
    }

    fn startup_approved_enabled(bytes: &[u8]) -> Option<bool> {
        if bytes.len() != 12 {
            return None;
        }
        let (state, timestamp) = bytes.split_at(4);
        match u32::from_le_bytes(state.try_into().ok()?) {
            2 => Some(timestamp.iter().all(|byte| *byte == 0)),
            3 => Some(false),
            _ => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::startup_approved_enabled;

        #[test]
        fn startup_approval_requires_a_complete_zero_timestamp() {
            assert_eq!(
                startup_approved_enabled(&[0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,]),
                Some(true)
            );
            assert_eq!(
                startup_approved_enabled(&[0x03, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0,]),
                Some(false)
            );
            assert_eq!(startup_approved_enabled(&[0; 8]), None);
            assert_eq!(startup_approved_enabled(&[0; 12]), None);
        }
    }
}

#[cfg(target_os = "linux")]
mod platform_backend {
    use tauri::Manager as _;

    use super::{
        AUTOSTART_IDENTIFIER, AppHandle, FileLoginAutostartRegistration, LoginAutostartBackend,
        LoginAutostartError, Wry, executable_text, linux_desktop_entry,
    };

    pub(super) struct NativeLoginAutostartBackend {
        registration: FileLoginAutostartRegistration,
    }

    impl NativeLoginAutostartBackend {
        pub(super) fn new(app: &AppHandle<Wry>) -> Result<Self, LoginAutostartError> {
            let executable = app
                .env()
                .appimage
                .map_or_else(std::env::current_exe, Ok)
                .map_err(|_| LoginAutostartError::Unavailable)?;
            let executable = executable_text(&executable)?;
            let content =
                linux_desktop_entry(executable).ok_or(LoginAutostartError::Unavailable)?;
            let registration = app
                .path()
                .config_dir()
                .map_err(|_| LoginAutostartError::Unavailable)?
                .join("autostart")
                .join(format!("{AUTOSTART_IDENTIFIER}.desktop"));
            Ok(Self {
                registration: FileLoginAutostartRegistration::new(registration, content),
            })
        }
    }

    impl LoginAutostartBackend for NativeLoginAutostartBackend {
        fn is_enabled(&self) -> Result<bool, LoginAutostartError> {
            self.registration
                .is_enabled()
                .map_err(|_| LoginAutostartError::Unavailable)
        }

        fn set_enabled(&self, enabled: bool) -> Result<(), LoginAutostartError> {
            self.registration
                .set_enabled(enabled)
                .map_err(|_| LoginAutostartError::Unavailable)
        }
    }
}

#[cfg(target_os = "macos")]
mod platform_backend {
    use tauri::Manager as _;

    use super::{
        AUTOSTART_IDENTIFIER, AppHandle, FileLoginAutostartRegistration, LoginAutostartBackend,
        LoginAutostartError, Wry, executable_text, macos_launch_agent_plist,
    };

    pub(super) struct NativeLoginAutostartBackend {
        registration: FileLoginAutostartRegistration,
    }

    impl NativeLoginAutostartBackend {
        pub(super) fn new(app: &AppHandle<Wry>) -> Result<Self, LoginAutostartError> {
            let executable =
                std::env::current_exe().map_err(|_| LoginAutostartError::Unavailable)?;
            let executable = executable_text(&executable)?;
            let content =
                macos_launch_agent_plist(executable).ok_or(LoginAutostartError::Unavailable)?;
            let registration = app
                .path()
                .home_dir()
                .map_err(|_| LoginAutostartError::Unavailable)?
                .join("Library")
                .join("LaunchAgents")
                .join(format!("{AUTOSTART_IDENTIFIER}.plist"));
            Ok(Self {
                registration: FileLoginAutostartRegistration::new(registration, content),
            })
        }
    }

    impl LoginAutostartBackend for NativeLoginAutostartBackend {
        fn is_enabled(&self) -> Result<bool, LoginAutostartError> {
            self.registration
                .is_enabled()
                .map_err(|_| LoginAutostartError::Unavailable)
        }

        fn set_enabled(&self, enabled: bool) -> Result<(), LoginAutostartError> {
            self.registration
                .set_enabled(enabled)
                .map_err(|_| LoginAutostartError::Unavailable)
        }
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
mod platform_backend {
    use super::{AppHandle, LoginAutostartBackend, LoginAutostartError, Wry};

    pub(super) struct NativeLoginAutostartBackend;

    impl NativeLoginAutostartBackend {
        pub(super) fn new(_app: &AppHandle<Wry>) -> Result<Self, LoginAutostartError> {
            Err(LoginAutostartError::Unavailable)
        }
    }

    impl LoginAutostartBackend for NativeLoginAutostartBackend {
        fn is_enabled(&self) -> Result<bool, LoginAutostartError> {
            Err(LoginAutostartError::Unavailable)
        }

        fn set_enabled(&self, _enabled: bool) -> Result<(), LoginAutostartError> {
            Err(LoginAutostartError::Unavailable)
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "windows")]
    use std::ffi::{OsStr, OsString};
    use std::sync::{Arc, Mutex};

    use codex_monitor_dashboard::{
        DashboardPlatformCapabilities, SystemNotificationAvailability, TrayAvailability,
    };
    use codex_monitor_engine::{LoginAutostartError, LoginAutostartHost};

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    use super::FileLoginAutostartRegistration;
    #[cfg(target_os = "windows")]
    use super::windows_run_command;
    use super::{
        AUTOSTART_ARGUMENT, LoginAutostartBackend, TauriLoginAutostartHost, linux_desktop_entry,
        macos_launch_agent_plist,
    };

    struct FakeBackend {
        state: Mutex<FakeBackendState>,
    }

    struct FakeBackendState {
        enabled: bool,
        read: ReadBehavior,
        write: WriteBehavior,
        writes: Vec<bool>,
    }

    #[derive(Clone, Copy)]
    enum ReadBehavior {
        ReturnState,
        Unavailable,
    }

    #[derive(Clone, Copy)]
    enum WriteBehavior {
        Apply,
        Ignore,
        Unavailable,
    }

    impl FakeBackend {
        fn new(enabled: bool) -> Self {
            Self {
                state: Mutex::new(FakeBackendState {
                    enabled,
                    read: ReadBehavior::ReturnState,
                    write: WriteBehavior::Apply,
                    writes: Vec::new(),
                }),
            }
        }

        fn set_read_available(&self, available: bool) {
            self.state.lock().unwrap().read = if available {
                ReadBehavior::ReturnState
            } else {
                ReadBehavior::Unavailable
            };
        }

        fn set_write_available(&self, available: bool) {
            self.state.lock().unwrap().write = if available {
                WriteBehavior::Apply
            } else {
                WriteBehavior::Unavailable
            };
        }

        fn ignore_writes(&self) {
            self.state.lock().unwrap().write = WriteBehavior::Ignore;
        }

        fn writes(&self) -> Vec<bool> {
            self.state.lock().unwrap().writes.clone()
        }
    }

    impl LoginAutostartBackend for FakeBackend {
        fn is_enabled(&self) -> Result<bool, LoginAutostartError> {
            let state = self.state.lock().unwrap();
            match state.read {
                ReadBehavior::ReturnState => Ok(state.enabled),
                ReadBehavior::Unavailable => Err(LoginAutostartError::Unavailable),
            }
        }

        fn set_enabled(&self, enabled: bool) -> Result<(), LoginAutostartError> {
            let mut state = self.state.lock().unwrap();
            match state.write {
                WriteBehavior::Unavailable => Err(LoginAutostartError::Unavailable),
                WriteBehavior::Apply => {
                    state.writes.push(enabled);
                    state.enabled = enabled;
                    Ok(())
                }
                WriteBehavior::Ignore => {
                    state.writes.push(enabled);
                    Ok(())
                }
            }
        }
    }

    fn platform() -> DashboardPlatformCapabilities {
        DashboardPlatformCapabilities::new(
            false,
            SystemNotificationAvailability::Unavailable,
            TrayAvailability::Available,
        )
    }

    fn host(
        backend: Arc<FakeBackend>,
        platform: DashboardPlatformCapabilities,
    ) -> TauriLoginAutostartHost {
        let backend: Arc<dyn LoginAutostartBackend> = backend;
        TauriLoginAutostartHost::with_backend(backend, platform)
    }

    #[test]
    fn startup_registration_has_only_the_background_argument() {
        assert_eq!(AUTOSTART_ARGUMENT, "--background");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_registration_quotes_paths_with_spaces_and_has_a_closed_length() {
        assert_eq!(
            windows_run_command(OsStr::new(
                r"C:\Program Files\CodexMonitor\CodexMonitor.exe"
            ))
            .unwrap(),
            OsString::from(r#""C:\Program Files\CodexMonitor\CodexMonitor.exe" --background"#)
        );
        assert_eq!(
            windows_run_command(OsStr::new(
                r"C:\用户\Codex & Monitor (100%)\CodexMonitor.exe"
            ))
            .unwrap(),
            OsString::from(r#""C:\用户\Codex & Monitor (100%)\CodexMonitor.exe" --background"#)
        );
        assert!(windows_run_command(OsStr::new(&"x".repeat(300))).is_none());
    }

    #[test]
    fn linux_registration_escapes_reserved_exec_characters() {
        let entry = linux_desktop_entry("/opt/Codex Monitor/$`\\%/app").unwrap();
        assert!(entry.contains(r#"Exec="/opt/Codex Monitor/\\$\\`\\\\%%/app" --background"#));
        let quoted = linux_desktop_entry("/opt/a\"b/app").unwrap();
        assert!(quoted.contains(r#"Exec="/opt/a\\"b/app" --background"#));
    }

    #[test]
    fn macos_registration_uses_separate_escaped_program_arguments() {
        let plist = macos_launch_agent_plist(
            "/Applications/Codex & Monitor <beta>.app/Contents/MacOS/CodexMonitor",
        )
        .unwrap();
        assert!(plist.contains("<key>ProgramArguments</key>"));
        assert!(plist.contains("<key>AssociatedBundleIdentifiers</key>"));
        assert!(plist.contains(
            "<string>/Applications/Codex &amp; Monitor &lt;beta&gt;.app/Contents/MacOS/CodexMonitor</string>"
        ));
        assert!(plist.contains("<string>--background</string>"));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn file_registration_is_atomic_bounded_owner_only_and_symlink_safe() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("registration");
        let registration =
            FileLoginAutostartRegistration::new(path.clone(), "canonical".to_owned());

        assert!(!registration.is_enabled().unwrap());
        registration.set_enabled(true).unwrap();
        assert!(registration.is_enabled().unwrap());
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        std::fs::write(&path, "stale").unwrap();
        assert!(!registration.is_enabled().unwrap());
        registration.set_enabled(false).unwrap();
        assert!(!path.exists());

        let target = directory.path().join("target");
        std::fs::write(&target, "canonical").unwrap();
        symlink(&target, &path).unwrap();
        assert!(!registration.is_enabled().unwrap());
        registration.set_enabled(true).unwrap();
        assert!(registration.is_enabled().unwrap());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "canonical");
    }

    #[test]
    fn native_read_publishes_availability_and_can_recover() {
        let platform = platform();
        let backend = Arc::new(FakeBackend::new(false));
        let host = host(Arc::clone(&backend), platform.clone());

        assert!(!host.login_autostart().unwrap());
        assert!(platform.login_autostart());

        backend.set_read_available(false);
        assert_eq!(
            host.login_autostart(),
            Err(LoginAutostartError::Unavailable)
        );
        assert!(!platform.login_autostart());

        backend.set_read_available(true);
        assert!(!host.login_autostart().unwrap());
        assert!(platform.login_autostart());
    }

    #[test]
    fn native_write_is_followed_by_observation_before_state_is_reported() {
        let platform = platform();
        let backend = Arc::new(FakeBackend::new(false));
        let host = host(Arc::clone(&backend), platform.clone());

        assert!(host.set_login_autostart(true).unwrap());
        assert_eq!(backend.writes(), [true]);
        assert!(platform.login_autostart());

        assert!(!host.set_login_autostart(false).unwrap());
        assert_eq!(backend.writes(), [true, false]);
        assert!(platform.login_autostart());
    }

    #[test]
    fn failed_or_mismatched_native_write_clears_the_live_capability() {
        let platform = platform();
        let backend = Arc::new(FakeBackend::new(false));
        let host = host(Arc::clone(&backend), platform.clone());

        backend.set_write_available(false);
        assert_eq!(
            host.set_login_autostart(true),
            Err(LoginAutostartError::Unavailable)
        );
        assert!(!platform.login_autostart());

        backend.set_write_available(true);
        backend.ignore_writes();
        assert!(!host.set_login_autostart(true).unwrap());
        assert!(!platform.login_autostart());
    }

    #[test]
    fn compensation_cannot_hide_a_failed_requested_write() {
        let platform = platform();
        let backend = Arc::new(FakeBackend::new(false));
        let host = host(Arc::clone(&backend), platform.clone());

        backend.set_write_available(false);
        assert_eq!(
            host.set_login_autostart(true),
            Err(LoginAutostartError::Unavailable)
        );
        assert!(!platform.login_autostart());

        backend.set_write_available(true);
        assert!(!host.set_login_autostart(false).unwrap());
        assert!(!platform.login_autostart());

        assert!(!host.login_autostart().unwrap());
        assert!(platform.login_autostart());
    }
}
