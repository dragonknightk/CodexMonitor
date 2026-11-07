use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::build_channel::BuildChannel;

pub(crate) const CLEANUP_ARGUMENT: &str = "--cleanup-platform-registration";

const APPLICATION_NAME: &str = "CodexMonitor";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlatformRegistrationError {
    #[allow(dead_code, reason = "constructed by the non-Windows cleanup boundary")]
    Unsupported,
    InvalidExecutable,
    UnsafePath,
    ShellUnavailable,
    OwnershipConflict,
    AtomicReplaceFailed,
    CleanupFailed,
}

impl PlatformRegistrationError {
    pub(crate) const fn stable_code(self) -> &'static str {
        match self {
            Self::Unsupported => "platform_registration_unsupported",
            Self::InvalidExecutable => "platform_registration_invalid_executable",
            Self::UnsafePath => "platform_registration_unsafe_path",
            Self::ShellUnavailable => "platform_registration_shell_unavailable",
            Self::OwnershipConflict => "platform_registration_ownership_conflict",
            Self::AtomicReplaceFailed => "platform_registration_atomic_replace_failed",
            Self::CleanupFailed => "platform_registration_cleanup_failed",
        }
    }
}

impl fmt::Display for PlatformRegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.stable_code())
    }
}

impl std::error::Error for PlatformRegistrationError {}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ShortcutRecord {
    target: PathBuf,
    arguments: String,
    icon: PathBuf,
    description: String,
    app_user_model_id: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReconciliationAction {
    Keep,
    Replace,
    Refuse,
}

pub(crate) fn cleanup_requested(arguments: &[OsString]) -> bool {
    arguments.len() == 2 && arguments[1] == OsStr::new(CLEANUP_ARGUMENT)
}

pub(crate) fn app_user_model_id() -> &'static str {
    BuildChannel::current().app_user_model_id()
}

pub(crate) fn prepare_notification_identity() -> bool {
    match platform::prepare(BuildChannel::current()) {
        Ok(()) => true,
        Err(error) => {
            eprintln!(
                "codex_monitor_notification_identity_unavailable:{}",
                error.stable_code()
            );
            false
        }
    }
}

pub(crate) fn cleanup_platform_registration() -> Result<(), PlatformRegistrationError> {
    platform::cleanup(BuildChannel::current())
}

#[cfg(target_os = "windows")]
pub(crate) fn record_login_autostart_ownership(
    command: &OsStr,
) -> Result<(), PlatformRegistrationError> {
    platform::record_login_autostart_ownership(command)
}

#[cfg(target_os = "windows")]
pub(crate) fn clear_login_autostart_ownership(
    command: &OsStr,
) -> Result<(), PlatformRegistrationError> {
    platform::clear_login_autostart_ownership(command)
}

fn reconciliation_action(
    current: Option<&ShortcutRecord>,
    target: &Path,
    channel: BuildChannel,
) -> ReconciliationAction {
    let Some(current) = current else {
        return ReconciliationAction::Replace;
    };
    if current.app_user_model_id != channel.app_user_model_id() {
        return ReconciliationAction::Refuse;
    }
    if current.arguments.is_empty()
        && windows_paths_equal(&current.target, target)
        && windows_paths_equal(&current.icon, target)
        && current.description == APPLICATION_NAME
    {
        ReconciliationAction::Keep
    } else {
        ReconciliationAction::Replace
    }
}

fn owned_for_cleanup(record: &ShortcutRecord, channel: BuildChannel) -> bool {
    record.app_user_model_id == channel.app_user_model_id()
        && record.arguments.is_empty()
        && record.description == APPLICATION_NAME
        && record.target.is_absolute()
        && record
            .target
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        && windows_paths_equal(&record.target, &record.icon)
}

fn windows_paths_equal(first: &Path, second: &Path) -> bool {
    windows_path_comparison_text(first).eq_ignore_ascii_case(&windows_path_comparison_text(second))
}

fn windows_path_comparison_text(path: &Path) -> String {
    let text = path.as_os_str().to_string_lossy();
    if let Some(path) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{path}")
    } else if let Some(path) = text.strip_prefix(r"\\?\") {
        path.to_owned()
    } else {
        text.into_owned()
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::io::ErrorKind;
    use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
    use std::os::windows::fs::MetadataExt as _;
    use std::path::{Path, PathBuf};

    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_SZ};
    use winreg::types::FromRegValue as _;
    use winsafe::{self as w, co, prelude::*};

    use super::{
        APPLICATION_NAME, BuildChannel, PlatformRegistrationError, ReconciliationAction,
        ShortcutRecord, owned_for_cleanup, reconciliation_action,
    };

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const MAX_SHELL_PATH_UNITS: usize = 259;
    const RUN_KEY: &str = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run";
    const STARTUP_APPROVED_KEY: &str =
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run";
    const AUTOSTART_OWNERSHIP_KEY: &str = "SOFTWARE\\CodexMonitor\\PlatformRegistration";
    const AUTOSTART_OWNERSHIP_VALUE: &str = "LoginAutostartCommand";

    pub(super) fn prepare(channel: BuildChannel) -> Result<(), PlatformRegistrationError> {
        w::SetCurrentProcessExplicitAppUserModelID(channel.app_user_model_id())
            .map_err(|_| PlatformRegistrationError::ShellUnavailable)?;
        let executable = current_executable()?;
        let shortcut = shortcut_path(channel)?;
        reconcile_shortcut(&shortcut, &executable, channel)?;
        if let Err(error) = reconcile_login_autostart_ownership(&executable) {
            eprintln!(
                "codex_monitor_autostart_ownership_unavailable:{}",
                error.stable_code()
            );
        }
        Ok(())
    }

    pub(super) fn cleanup(channel: BuildChannel) -> Result<(), PlatformRegistrationError> {
        let _com = initialize_com()?;
        let shortcut = shortcut_path(channel)?;
        cleanup_shortcut(&shortcut, channel)?;
        cleanup_login_autostart()?;
        if let Some(parent) = shortcut.parent() {
            let _ = fs::remove_dir(parent);
        }
        Ok(())
    }

    fn initialize_com() -> Result<w::guard::CoUninitializeGuard, PlatformRegistrationError> {
        w::CoInitializeEx(co::COINIT::APARTMENTTHREADED | co::COINIT::DISABLE_OLE1DDE)
            .map_err(|_| PlatformRegistrationError::ShellUnavailable)
    }

    fn current_executable() -> Result<PathBuf, PlatformRegistrationError> {
        let executable =
            std::env::current_exe().map_err(|_| PlatformRegistrationError::InvalidExecutable)?;
        validate_regular_file(&executable)?;
        let executable = fs::canonicalize(executable)
            .map_err(|_| PlatformRegistrationError::InvalidExecutable)?;
        let executable = shell_compatible_path(&executable)?;
        validate_regular_file(&executable)?;
        path_text(&executable)?;
        Ok(executable)
    }

    fn shortcut_path(channel: BuildChannel) -> Result<PathBuf, PlatformRegistrationError> {
        let roaming =
            w::SHGetKnownFolderPath(&co::KNOWNFOLDERID::RoamingAppData, co::KF::DEFAULT, None)
                .map_err(|_| PlatformRegistrationError::ShellUnavailable)?;
        let directory = PathBuf::from(roaming)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join(APPLICATION_NAME);
        fs::create_dir_all(&directory).map_err(|_| PlatformRegistrationError::ShellUnavailable)?;
        validate_directory(&directory)?;
        let shortcut = directory.join(channel.shortcut_name());
        path_text(&shortcut)?;
        Ok(shortcut)
    }

    fn validate_directory(path: &Path) -> Result<(), PlatformRegistrationError> {
        let metadata =
            fs::symlink_metadata(path).map_err(|_| PlatformRegistrationError::UnsafePath)?;
        if !metadata.is_dir() {
            return Err(PlatformRegistrationError::UnsafePath);
        }
        reject_reparse_points(path)
    }

    fn validate_regular_file(path: &Path) -> Result<(), PlatformRegistrationError> {
        if !path.is_absolute() {
            return Err(PlatformRegistrationError::InvalidExecutable);
        }
        let metadata =
            fs::symlink_metadata(path).map_err(|_| PlatformRegistrationError::InvalidExecutable)?;
        if !metadata.is_file() {
            return Err(PlatformRegistrationError::InvalidExecutable);
        }
        reject_reparse_points(path)
    }

    fn validate_shortcut_file(path: &Path) -> Result<(), PlatformRegistrationError> {
        let metadata =
            fs::symlink_metadata(path).map_err(|_| PlatformRegistrationError::UnsafePath)?;
        if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(PlatformRegistrationError::UnsafePath);
        }
        Ok(())
    }

    fn reject_reparse_points(path: &Path) -> Result<(), PlatformRegistrationError> {
        for ancestor in path.ancestors() {
            let metadata = fs::symlink_metadata(ancestor)
                .map_err(|_| PlatformRegistrationError::UnsafePath)?;
            if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(PlatformRegistrationError::UnsafePath);
            }
        }
        Ok(())
    }

    fn path_text(path: &Path) -> Result<String, PlatformRegistrationError> {
        let text = path.to_str().ok_or(PlatformRegistrationError::UnsafePath)?;
        let units = text.encode_utf16().count();
        if units == 0 || units > MAX_SHELL_PATH_UNITS || text.contains('\0') {
            return Err(PlatformRegistrationError::UnsafePath);
        }
        Ok(text.to_owned())
    }

    fn shell_compatible_path(path: &Path) -> Result<PathBuf, PlatformRegistrationError> {
        let text = path.to_str().ok_or(PlatformRegistrationError::UnsafePath)?;
        if let Some(path) = text.strip_prefix(r"\\?\UNC\") {
            Ok(PathBuf::from(format!(r"\\{path}")))
        } else if let Some(path) = text.strip_prefix(r"\\?\") {
            Ok(PathBuf::from(path))
        } else {
            Ok(path.to_owned())
        }
    }

    fn reconcile_shortcut(
        shortcut: &Path,
        executable: &Path,
        channel: BuildChannel,
    ) -> Result<(), PlatformRegistrationError> {
        let _com = initialize_com()?;
        let executable = shell_compatible_path(executable)?;
        validate_regular_file(&executable)?;
        let current = match fs::symlink_metadata(shortcut) {
            Ok(_) => {
                validate_shortcut_file(shortcut)?;
                Some(read_shortcut(shortcut)?)
            }
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(_) => return Err(PlatformRegistrationError::UnsafePath),
        };
        match reconciliation_action(current.as_ref(), &executable, channel) {
            ReconciliationAction::Keep => return Ok(()),
            ReconciliationAction::Refuse => {
                return Err(PlatformRegistrationError::OwnershipConflict);
            }
            ReconciliationAction::Replace => {}
        }

        let (pending, reservation) = reserve_pending_path(shortcut, channel)?;
        let result = (|| {
            write_shortcut(&pending, &executable, channel)?;
            validate_shortcut_file(&pending)?;
            let staged = read_shortcut(&pending)?;
            if reconciliation_action(Some(&staged), &executable, channel)
                != ReconciliationAction::Keep
            {
                return Err(PlatformRegistrationError::ShellUnavailable);
            }
            drop(reservation);

            match fs::symlink_metadata(shortcut) {
                Ok(_) => {
                    validate_shortcut_file(shortcut)?;
                    let latest = read_shortcut(shortcut)?;
                    if latest.app_user_model_id != channel.app_user_model_id() {
                        return Err(PlatformRegistrationError::OwnershipConflict);
                    }
                    w::ReplaceFile(
                        &path_text(shortcut)?,
                        &path_text(&pending)?,
                        None,
                        co::REPLACEFILE::WRITE_THROUGH,
                    )
                    .map_err(|_| PlatformRegistrationError::AtomicReplaceFailed)?;
                }
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    fs::rename(&pending, shortcut)
                        .map_err(|_| PlatformRegistrationError::AtomicReplaceFailed)?;
                }
                Err(_) => return Err(PlatformRegistrationError::UnsafePath),
            }

            validate_shortcut_file(shortcut)?;
            let installed = read_shortcut(shortcut)?;
            if reconciliation_action(Some(&installed), &executable, channel)
                == ReconciliationAction::Keep
            {
                Ok(())
            } else {
                Err(PlatformRegistrationError::ShellUnavailable)
            }
        })();
        if pending.exists() {
            let _ = fs::remove_file(&pending);
        }
        result
    }

    fn reserve_pending_path(
        shortcut: &Path,
        channel: BuildChannel,
    ) -> Result<(PathBuf, w::guard::CloseHandleGuard<w::HFILE>), PlatformRegistrationError> {
        let directory = shortcut
            .parent()
            .ok_or(PlatformRegistrationError::UnsafePath)?;
        for attempt in 0..32_u8 {
            let pending = directory.join(format!(
                ".codex-monitor-{}-{}-{attempt}.pending.lnk",
                channel.as_str(),
                std::process::id()
            ));
            path_text(&pending)?;
            match w::HFILE::CreateFile(
                &path_text(&pending)?,
                co::GENERIC::READ,
                Some(co::FILE_SHARE::READ | co::FILE_SHARE::WRITE),
                None,
                co::DISPOSITION::CREATE_NEW,
                co::FILE_ATTRIBUTE::NORMAL,
                None,
                None,
                None,
            ) {
                Ok((reservation, _)) => return Ok((pending, reservation)),
                Err(error)
                    if error == co::ERROR::FILE_EXISTS || error == co::ERROR::ALREADY_EXISTS => {}
                Err(_) => return Err(PlatformRegistrationError::ShellUnavailable),
            }
        }
        Err(PlatformRegistrationError::ShellUnavailable)
    }

    fn write_shortcut(
        path: &Path,
        executable: &Path,
        channel: BuildChannel,
    ) -> Result<(), PlatformRegistrationError> {
        let executable = path_text(executable)?;
        let link = w::CoCreateInstance::<w::IShellLink>(
            &co::CLSID::ShellLink,
            None::<&w::IUnknown>,
            co::CLSCTX::INPROC_SERVER,
        )
        .map_err(|_| PlatformRegistrationError::ShellUnavailable)?;
        link.SetPath(&executable)
            .and_then(|()| link.SetArguments(""))
            .and_then(|()| link.SetIconLocation(&executable, 0))
            .and_then(|()| link.SetDescription(APPLICATION_NAME))
            .map_err(|_| PlatformRegistrationError::ShellUnavailable)?;
        let properties = link
            .QueryInterface::<w::IPropertyStore>()
            .map_err(|_| PlatformRegistrationError::ShellUnavailable)?;
        properties
            .SetValue(
                &co::PKEY::AppUserModel_ID,
                &w::PropVariant::from_str(channel.app_user_model_id()),
            )
            .and_then(|()| properties.Commit())
            .map_err(|_| PlatformRegistrationError::ShellUnavailable)?;
        let persistent = link
            .QueryInterface::<w::IPersistFile>()
            .map_err(|_| PlatformRegistrationError::ShellUnavailable)?;
        persistent
            .Save(Some(&path_text(path)?), true)
            .map_err(|_| PlatformRegistrationError::ShellUnavailable)
    }

    fn read_shortcut(path: &Path) -> Result<ShortcutRecord, PlatformRegistrationError> {
        let link = w::CoCreateInstance::<w::IShellLink>(
            &co::CLSID::ShellLink,
            None::<&w::IUnknown>,
            co::CLSCTX::INPROC_SERVER,
        )
        .map_err(|_| PlatformRegistrationError::ShellUnavailable)?;
        let persistent = link
            .QueryInterface::<w::IPersistFile>()
            .map_err(|_| PlatformRegistrationError::ShellUnavailable)?;
        persistent
            .Load(
                &path_text(path)?,
                co::STGM::READ | co::STGM::SHARE_DENY_WRITE,
            )
            .map_err(|_| PlatformRegistrationError::ShellUnavailable)?;
        let properties = link
            .QueryInterface::<w::IPropertyStore>()
            .map_err(|_| PlatformRegistrationError::ShellUnavailable)?;
        let w::PropVariant::Bstr(app_user_model_id) = properties
            .GetValue(&co::PKEY::AppUserModel_ID)
            .map_err(|_| PlatformRegistrationError::ShellUnavailable)?
        else {
            return Err(PlatformRegistrationError::OwnershipConflict);
        };
        let target = link
            .GetPath(None, co::SLGP::RAWPATH)
            .map_err(|_| PlatformRegistrationError::ShellUnavailable)?;
        let (icon, icon_index) = link
            .GetIconLocation()
            .map_err(|_| PlatformRegistrationError::ShellUnavailable)?;
        if icon_index != 0 {
            return Err(PlatformRegistrationError::OwnershipConflict);
        }
        Ok(ShortcutRecord {
            target: PathBuf::from(target),
            arguments: link
                .GetArguments()
                .map_err(|_| PlatformRegistrationError::ShellUnavailable)?,
            icon: PathBuf::from(icon),
            description: link
                .GetDescription()
                .map_err(|_| PlatformRegistrationError::ShellUnavailable)?,
            app_user_model_id,
        })
    }

    fn cleanup_shortcut(
        shortcut: &Path,
        channel: BuildChannel,
    ) -> Result<(), PlatformRegistrationError> {
        match fs::symlink_metadata(shortcut) {
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(PlatformRegistrationError::CleanupFailed),
        }
        validate_shortcut_file(shortcut)?;
        let record = read_shortcut(shortcut)?;
        if !owned_for_cleanup(&record, channel) {
            return Err(PlatformRegistrationError::OwnershipConflict);
        }
        fs::remove_file(shortcut).map_err(|_| PlatformRegistrationError::CleanupFailed)
    }

    pub(super) fn record_login_autostart_ownership(
        command: &OsStr,
    ) -> Result<(), PlatformRegistrationError> {
        if !owned_autostart_command(command) {
            return Err(PlatformRegistrationError::OwnershipConflict);
        }
        let current_user = RegKey::predef(HKEY_CURRENT_USER);
        let (ownership, _) = current_user
            .create_subkey(AUTOSTART_OWNERSHIP_KEY)
            .map_err(|_| PlatformRegistrationError::CleanupFailed)?;
        ownership
            .set_value(AUTOSTART_OWNERSHIP_VALUE, &command)
            .map_err(|_| PlatformRegistrationError::CleanupFailed)
    }

    pub(super) fn clear_login_autostart_ownership(
        command: &OsStr,
    ) -> Result<(), PlatformRegistrationError> {
        let current_user = RegKey::predef(HKEY_CURRENT_USER);
        remove_login_autostart_ownership(&current_user, command)
    }

    fn reconcile_login_autostart_ownership(
        executable: &Path,
    ) -> Result<(), PlatformRegistrationError> {
        let expected = crate::autostart::windows_run_command(executable.as_os_str())
            .ok_or(PlatformRegistrationError::InvalidExecutable)?;
        let current_user = RegKey::predef(HKEY_CURRENT_USER);
        let run = match current_user.open_subkey_with_flags(RUN_KEY, KEY_READ) {
            Ok(run) => run,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(PlatformRegistrationError::CleanupFailed),
        };
        let raw = match run.get_raw_value(APPLICATION_NAME) {
            Ok(raw) => raw,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(PlatformRegistrationError::CleanupFailed),
        };
        if raw.vtype != REG_SZ {
            return Ok(());
        }
        let command = OsString::from_reg_value(&raw)
            .map_err(|_| PlatformRegistrationError::OwnershipConflict)?;
        if command == expected {
            record_login_autostart_ownership(&command)?;
        }
        Ok(())
    }

    fn login_autostart_ownership(
        current_user: &RegKey,
    ) -> Result<Option<OsString>, PlatformRegistrationError> {
        let ownership = match current_user.open_subkey_with_flags(AUTOSTART_OWNERSHIP_KEY, KEY_READ)
        {
            Ok(ownership) => ownership,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(PlatformRegistrationError::CleanupFailed),
        };
        let raw = match ownership.get_raw_value(AUTOSTART_OWNERSHIP_VALUE) {
            Ok(raw) => raw,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(PlatformRegistrationError::CleanupFailed),
        };
        if raw.vtype != REG_SZ {
            return Err(PlatformRegistrationError::OwnershipConflict);
        }
        OsString::from_reg_value(&raw)
            .map(Some)
            .map_err(|_| PlatformRegistrationError::OwnershipConflict)
    }

    fn remove_login_autostart_ownership(
        current_user: &RegKey,
        command: &OsStr,
    ) -> Result<(), PlatformRegistrationError> {
        let Some(owned) = login_autostart_ownership(current_user)? else {
            return Ok(());
        };
        if owned != command {
            return Err(PlatformRegistrationError::OwnershipConflict);
        }
        let ownership = current_user
            .open_subkey_with_flags(AUTOSTART_OWNERSHIP_KEY, KEY_SET_VALUE)
            .map_err(|_| PlatformRegistrationError::CleanupFailed)?;
        ownership
            .delete_value(AUTOSTART_OWNERSHIP_VALUE)
            .map_err(|_| PlatformRegistrationError::CleanupFailed)
    }

    fn cleanup_login_autostart() -> Result<(), PlatformRegistrationError> {
        let current_user = RegKey::predef(HKEY_CURRENT_USER);
        let run = match current_user.open_subkey_with_flags(RUN_KEY, KEY_READ | KEY_SET_VALUE) {
            Ok(run) => run,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return cleanup_orphaned_login_autostart_ownership(&current_user);
            }
            Err(_) => return Err(PlatformRegistrationError::CleanupFailed),
        };
        let raw = match run.get_raw_value(APPLICATION_NAME) {
            Ok(raw) => raw,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return cleanup_orphaned_login_autostart_ownership(&current_user);
            }
            Err(_) => return Err(PlatformRegistrationError::CleanupFailed),
        };
        if raw.vtype != REG_SZ {
            return Err(PlatformRegistrationError::OwnershipConflict);
        }
        let command = OsString::from_reg_value(&raw)
            .map_err(|_| PlatformRegistrationError::OwnershipConflict)?;
        let marker = login_autostart_ownership(&current_user)?
            .ok_or(PlatformRegistrationError::OwnershipConflict)?;
        if !owned_autostart_registration(&command, Some(&marker)) {
            return Err(PlatformRegistrationError::OwnershipConflict);
        }
        cleanup_startup_approved(&current_user)?;
        run.delete_value(APPLICATION_NAME)
            .map_err(|_| PlatformRegistrationError::CleanupFailed)?;
        remove_login_autostart_ownership(&current_user, &command)
    }

    fn cleanup_orphaned_login_autostart_ownership(
        current_user: &RegKey,
    ) -> Result<(), PlatformRegistrationError> {
        let Some(command) = login_autostart_ownership(current_user)? else {
            return Ok(());
        };
        if !owned_autostart_command(&command) {
            return Err(PlatformRegistrationError::OwnershipConflict);
        }
        cleanup_startup_approved(current_user)?;
        remove_login_autostart_ownership(current_user, &command)
    }

    fn cleanup_startup_approved(current_user: &RegKey) -> Result<(), PlatformRegistrationError> {
        match current_user.open_subkey_with_flags(STARTUP_APPROVED_KEY, KEY_SET_VALUE) {
            Ok(startup_approved) => match startup_approved.delete_value(APPLICATION_NAME) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
                Err(_) => Err(PlatformRegistrationError::CleanupFailed),
            },
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(_) => Err(PlatformRegistrationError::CleanupFailed),
        }
    }

    fn owned_autostart_command(command: &OsStr) -> bool {
        let units = command.encode_wide().collect::<Vec<_>>();
        let Some((&first, remainder)) = units.split_first() else {
            return false;
        };
        if first != u16::from(b'\"') {
            return false;
        }
        let Some(closing) = remainder.iter().position(|unit| *unit == u16::from(b'\"')) else {
            return false;
        };
        let executable = &remainder[..closing];
        let suffix = &remainder[closing + 1..];
        if suffix != " --background".encode_utf16().collect::<Vec<_>>() {
            return false;
        }
        let executable = OsString::from_wide(executable);
        let executable = Path::new(&executable);
        executable.is_absolute()
            && executable
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    }

    fn owned_autostart_registration(command: &OsStr, marker: Option<&OsStr>) -> bool {
        owned_autostart_command(command) && marker.is_some_and(|marker| marker == command)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::Write as _;

        fn synthetic_executable(path: &Path) {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut file = fs::File::create(path).unwrap();
            file.write_all(b"synthetic executable fixture").unwrap();
            file.sync_all().unwrap();
        }

        #[test]
        fn shell_shortcut_reconciles_first_start_idempotence_and_path_move() {
            let directory = tempfile::tempdir().unwrap();
            let primary = directory.path().join("Primary").join("codex-monitor.exe");
            let secondary = directory.path().join("Secondary").join("codex-monitor.exe");
            let shortcut = directory.path().join("CodexMonitor.lnk");
            synthetic_executable(&primary);
            synthetic_executable(&secondary);
            let primary = fs::canonicalize(primary).unwrap();
            let secondary = fs::canonicalize(secondary).unwrap();

            reconcile_shortcut(&shortcut, &primary, BuildChannel::LocalDevelopment).unwrap();
            let first = fs::metadata(&shortcut).unwrap().modified().unwrap();
            reconcile_shortcut(&shortcut, &primary, BuildChannel::LocalDevelopment).unwrap();
            assert_eq!(fs::metadata(&shortcut).unwrap().modified().unwrap(), first);

            reconcile_shortcut(&shortcut, &secondary, BuildChannel::LocalDevelopment).unwrap();
            let _com = initialize_com().unwrap();
            let moved = read_shortcut(&shortcut).unwrap();
            assert!(super::super::windows_paths_equal(&moved.target, &secondary));
            assert_eq!(moved.arguments, "");
        }

        #[test]
        fn shell_shortcut_channels_use_distinct_real_files_and_identities() {
            let directory = tempfile::tempdir().unwrap();
            let executable = directory.path().join("codex-monitor.exe");
            synthetic_executable(&executable);
            let executable = fs::canonicalize(executable).unwrap();
            let channels = [
                BuildChannel::Production,
                BuildChannel::UnsignedDevelopment,
                BuildChannel::LocalDevelopment,
            ];
            for channel in channels {
                let shortcut = directory.path().join(channel.shortcut_name());
                reconcile_shortcut(&shortcut, &executable, channel).unwrap();
                let _com = initialize_com().unwrap();
                assert_eq!(
                    read_shortcut(&shortcut).unwrap().app_user_model_id,
                    channel.app_user_model_id()
                );
            }
            assert_eq!(
                fs::read_dir(directory.path())
                    .unwrap()
                    .filter_map(Result::ok)
                    .filter(|entry| entry.path().extension().is_some_and(|value| value == "lnk"))
                    .count(),
                channels.len()
            );
        }

        #[test]
        fn explicit_cleanup_only_removes_an_owned_shortcut() {
            let directory = tempfile::tempdir().unwrap();
            let executable = directory.path().join("codex-monitor.exe");
            let shortcut = directory.path().join("CodexMonitor.lnk");
            synthetic_executable(&executable);
            let executable = fs::canonicalize(executable).unwrap();
            reconcile_shortcut(&shortcut, &executable, BuildChannel::LocalDevelopment).unwrap();
            let _com = initialize_com().unwrap();
            cleanup_shortcut(&shortcut, BuildChannel::LocalDevelopment).unwrap();
            assert!(!shortcut.exists());
        }

        #[test]
        fn unsafe_shell_paths_are_rejected_before_com_writes() {
            let path = PathBuf::from(format!(
                r"C:\Synthetic\{}\codex-monitor.exe",
                "x".repeat(260)
            ));
            assert_eq!(path_text(&path), Err(PlatformRegistrationError::UnsafePath));
        }

        #[test]
        fn pending_shortcut_reservation_blocks_path_replacement() {
            let directory = tempfile::tempdir().unwrap();
            let shortcut = directory.path().join("CodexMonitor.lnk");
            let (pending, reservation) =
                reserve_pending_path(&shortcut, BuildChannel::LocalDevelopment).unwrap();
            assert!(fs::remove_file(&pending).is_err());
            drop(reservation);
            fs::remove_file(pending).unwrap();
        }

        #[test]
        fn ordinary_quit_classification_preserves_the_registered_shortcut() {
            let directory = tempfile::tempdir().unwrap();
            let executable = directory.path().join("codex-monitor.exe");
            let shortcut = directory.path().join("CodexMonitor.lnk");
            synthetic_executable(&executable);
            let executable = fs::canonicalize(executable).unwrap();
            reconcile_shortcut(&shortcut, &executable, BuildChannel::LocalDevelopment).unwrap();

            assert!(!super::super::cleanup_requested(&[
                OsString::from("codex-monitor.exe"),
                OsString::from("--quit"),
            ]));
            let _com = initialize_com().unwrap();
            assert_eq!(
                read_shortcut(&shortcut).unwrap().app_user_model_id,
                BuildChannel::LocalDevelopment.app_user_model_id()
            );
        }

        #[test]
        fn atomic_replace_failure_preserves_the_existing_shortcut() {
            let directory = tempfile::tempdir().unwrap();
            let primary = directory.path().join("Primary").join("codex-monitor.exe");
            let secondary = directory.path().join("Secondary").join("codex-monitor.exe");
            let shortcut = directory.path().join("CodexMonitor.lnk");
            synthetic_executable(&primary);
            synthetic_executable(&secondary);
            let primary = fs::canonicalize(primary).unwrap();
            let secondary = fs::canonicalize(secondary).unwrap();
            reconcile_shortcut(&shortcut, &primary, BuildChannel::LocalDevelopment).unwrap();

            let mut permissions = fs::metadata(&shortcut).unwrap().permissions();
            permissions.set_readonly(true);
            fs::set_permissions(&shortcut, permissions).unwrap();
            let result = reconcile_shortcut(&shortcut, &secondary, BuildChannel::LocalDevelopment);

            let mut permissions = fs::metadata(&shortcut).unwrap().permissions();
            #[expect(
                clippy::permissions_set_readonly_false,
                reason = "this test is Windows-only and restores a file attribute, not Unix mode bits"
            )]
            permissions.set_readonly(false);
            fs::set_permissions(&shortcut, permissions).unwrap();
            assert_eq!(result, Err(PlatformRegistrationError::AtomicReplaceFailed));
            let _com = initialize_com().unwrap();
            let preserved = read_shortcut(&shortcut).unwrap();
            assert!(super::super::windows_paths_equal(
                &preserved.target,
                &primary
            ));
        }

        #[test]
        fn cleanup_rejects_untrusted_autostart_command_shapes() {
            let command = OsStr::new(r#""C:\Synthetic\codex-monitor.exe" --background"#);
            assert!(owned_autostart_registration(command, Some(command)));
            assert!(!owned_autostart_registration(command, None));
            assert!(!owned_autostart_registration(
                command,
                Some(OsStr::new(r#""C:\Synthetic\other.exe" --background"#))
            ));
            for command in [
                r"C:\Synthetic\codex-monitor.exe --background",
                r#""C:\Synthetic\codex-monitor.exe" --quit"#,
                r#""relative.exe" --background"#,
                r#""C:\Synthetic\codex-monitor.exe" --background --extra"#,
            ] {
                assert!(!owned_autostart_command(OsStr::new(command)));
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use super::{BuildChannel, PlatformRegistrationError};

    pub(super) const fn prepare(_channel: BuildChannel) -> Result<(), PlatformRegistrationError> {
        Ok(())
    }

    pub(super) const fn cleanup(_channel: BuildChannel) -> Result<(), PlatformRegistrationError> {
        Err(PlatformRegistrationError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(channel: BuildChannel, target: &str) -> ShortcutRecord {
        ShortcutRecord {
            target: PathBuf::from(target),
            arguments: String::new(),
            icon: PathBuf::from(target),
            description: APPLICATION_NAME.to_owned(),
            app_user_model_id: channel.app_user_model_id().to_owned(),
        }
    }

    #[test]
    fn cleanup_argument_is_closed_and_cannot_be_combined() {
        assert!(cleanup_requested(&[
            OsString::from("codex-monitor"),
            OsString::from(CLEANUP_ARGUMENT),
        ]));
        assert!(!cleanup_requested(&[OsString::from("codex-monitor")]));
        assert!(!cleanup_requested(&[
            OsString::from("codex-monitor"),
            OsString::from(CLEANUP_ARGUMENT),
            OsString::from("--background"),
        ]));
        assert!(!cleanup_requested(&[
            OsString::from("codex-monitor"),
            OsString::from("--quit"),
        ]));
    }

    #[test]
    fn reconciliation_is_idempotent_and_channel_scoped() {
        let target = Path::new(r"C:\Synthetic\Primary\codex-monitor.exe");
        assert_eq!(
            reconciliation_action(None, target, BuildChannel::Production),
            ReconciliationAction::Replace
        );
        assert_eq!(
            reconciliation_action(
                Some(&record(
                    BuildChannel::Production,
                    r"c:\synthetic\primary\CODEX-MONITOR.EXE",
                )),
                target,
                BuildChannel::Production,
            ),
            ReconciliationAction::Keep
        );
        assert_eq!(
            reconciliation_action(
                Some(&record(
                    BuildChannel::Production,
                    r"C:\Synthetic\Old\codex-monitor.exe",
                )),
                target,
                BuildChannel::Production,
            ),
            ReconciliationAction::Replace
        );
        assert_eq!(
            reconciliation_action(
                Some(&record(
                    BuildChannel::LocalDevelopment,
                    target.to_str().unwrap()
                )),
                target,
                BuildChannel::Production,
            ),
            ReconciliationAction::Refuse
        );
    }

    #[test]
    fn cleanup_ownership_requires_the_full_shortcut_shape() {
        let mut owned = record(BuildChannel::Production, r"C:\Synthetic\codex-monitor.exe");
        assert!(owned_for_cleanup(&owned, BuildChannel::Production));
        owned.arguments = "--background".to_owned();
        assert!(!owned_for_cleanup(&owned, BuildChannel::Production));
        assert!(!owned_for_cleanup(
            &record(
                BuildChannel::UnsignedDevelopment,
                r"C:\Synthetic\codex-monitor.exe",
            ),
            BuildChannel::Production,
        ));
    }

    #[test]
    fn stable_errors_do_not_contain_paths() {
        for error in [
            PlatformRegistrationError::Unsupported,
            PlatformRegistrationError::InvalidExecutable,
            PlatformRegistrationError::UnsafePath,
            PlatformRegistrationError::ShellUnavailable,
            PlatformRegistrationError::OwnershipConflict,
            PlatformRegistrationError::AtomicReplaceFailed,
            PlatformRegistrationError::CleanupFailed,
        ] {
            assert_eq!(error.to_string(), error.stable_code());
            assert!(!error.to_string().contains(['\\', '/']));
        }
    }
}
