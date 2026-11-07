use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use super::{StorageError, StorageResult};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformFamily {
    Windows,
    MacOs,
    Unix,
}

#[derive(Clone, Debug, Default)]
pub struct AppDataEnvironment {
    pub local_app_data: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
}

/// Current-user paths owned by the resident application.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppDataPaths {
    pub data_directory: PathBuf,
    pub database_file: PathBuf,
    pub settings_file: PathBuf,
    pub logs_directory: PathBuf,
}

impl AppDataPaths {
    /// Resolve paths for the process's current user and operating system.
    ///
    /// # Errors
    ///
    /// Returns an error when the required current-user environment directory is
    /// missing or is not absolute.
    pub fn resolve_current_user() -> StorageResult<Self> {
        let environment = AppDataEnvironment {
            local_app_data: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
            home: std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from),
            xdg_data_home: std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
        };
        let platform = if cfg!(target_os = "windows") {
            PlatformFamily::Windows
        } else if cfg!(target_os = "macos") {
            PlatformFamily::MacOs
        } else {
            PlatformFamily::Unix
        };
        Self::resolve(platform, &environment)
    }

    /// Resolve deterministic application paths for a platform and environment.
    ///
    /// # Errors
    ///
    /// Returns an error when the platform's required current-user directory is
    /// missing or is not absolute.
    pub fn resolve(
        platform: PlatformFamily,
        environment: &AppDataEnvironment,
    ) -> StorageResult<Self> {
        let data_directory = match platform {
            PlatformFamily::Windows => {
                required_absolute(environment.local_app_data.as_deref(), "LOCALAPPDATA")?
                    .join("CodexMonitor")
            }
            PlatformFamily::MacOs => required_absolute(environment.home.as_deref(), "HOME")?
                .join("Library")
                .join("Application Support")
                .join("CodexMonitor"),
            PlatformFamily::Unix => match environment.xdg_data_home.as_deref() {
                Some(path) => required_absolute(Some(path), "XDG_DATA_HOME")?.join("codex-monitor"),
                None => required_absolute(environment.home.as_deref(), "HOME")?
                    .join(".local")
                    .join("share")
                    .join("codex-monitor"),
            },
        };

        Ok(Self {
            database_file: data_directory.join("codex-monitor.sqlite3"),
            settings_file: data_directory.join("settings.toml"),
            logs_directory: data_directory.join("logs"),
            data_directory,
        })
    }

    /// Create and restrict the data directories and database file.
    ///
    /// # Errors
    ///
    /// Returns an error for filesystem failures or when any managed path is a
    /// symbolic link.
    pub fn prepare(&self) -> StorageResult<()> {
        create_private_directory(&self.data_directory, "data directory")?;
        create_private_directory(&self.logs_directory, "logs directory")?;
        reject_symlink_if_present(&self.database_file, "database file")?;
        reject_symlink_if_present(&self.settings_file, "settings file")?;
        touch_private_file(&self.database_file, "database file")?;
        Ok(())
    }

    /// Create and restrict the settings file.
    ///
    /// # Errors
    ///
    /// Returns an error for filesystem failures or when the settings path is a
    /// symbolic link.
    pub fn secure_settings_file(&self) -> StorageResult<()> {
        touch_private_file(&self.settings_file, "settings file")
    }

    /// Create and restrict one log file below the managed logs directory.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid file name, filesystem failure, or a
    /// symbolic-link destination.
    pub fn secure_log_file(&self, file_name: &str) -> StorageResult<PathBuf> {
        if file_name.is_empty()
            || file_name.contains(['/', '\\'])
            || matches!(file_name, "." | "..")
        {
            return Err(StorageError::AppDataUnavailable {
                component: "log file name",
            });
        }
        let path = self.logs_directory.join(file_name);
        touch_private_file(&path, "log file")?;
        Ok(path)
    }
}

fn required_absolute<'a>(
    path: Option<&'a Path>,
    component: &'static str,
) -> StorageResult<&'a Path> {
    let path = path.ok_or(StorageError::AppDataUnavailable { component })?;
    if !path.is_absolute() {
        return Err(StorageError::AppDataPathNotAbsolute { component });
    }
    Ok(path)
}

fn reject_symlink_if_present(path: &Path, component: &'static str) -> StorageResult<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(StorageError::AppDataSymlink { component })
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(StorageError::AppDataIo { component, source }),
    }
}

fn create_private_directory(path: &Path, component: &'static str) -> StorageResult<()> {
    fs::create_dir_all(path).map_err(|source| StorageError::AppDataIo { component, source })?;
    reject_symlink_if_present(path, component)?;
    restrict_directory(path, component)
}

fn touch_private_file(path: &Path, component: &'static str) -> StorageResult<()> {
    reject_symlink_if_present(path, component)?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| StorageError::AppDataIo { component, source })?;
    restrict_file(path, component)
}

#[cfg(unix)]
fn restrict_directory(path: &Path, component: &'static str) -> StorageResult<()> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|source| StorageError::AppDataIo { component, source })
}

#[cfg(windows)]
fn restrict_directory(path: &Path, component: &'static str) -> StorageResult<()> {
    restrict_windows_acl(path, component, true)
}

#[cfg(unix)]
fn restrict_file(path: &Path, component: &'static str) -> StorageResult<()> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|source| StorageError::AppDataIo { component, source })
}

#[cfg(windows)]
fn restrict_file(path: &Path, component: &'static str) -> StorageResult<()> {
    restrict_windows_acl(path, component, false)
}

#[cfg(windows)]
fn restrict_windows_acl(
    path: &Path,
    component: &'static str,
    directory: bool,
) -> StorageResult<()> {
    use windows_permissions::constants::{SeObjectType::SE_FILE_OBJECT, SecurityInformation};
    use windows_permissions::utilities::current_process_sid;
    use windows_permissions::wrappers::SetNamedSecurityInfo;
    use windows_permissions::{LocalBox, SecurityDescriptor};

    let current_user =
        current_process_sid().map_err(|source| StorageError::AppDataIo { component, source })?;
    let inheritance = if directory { "OICI" } else { "" };
    let descriptor_text = format!("D:P(A;{inheritance};FA;;;{current_user})");
    let descriptor: LocalBox<SecurityDescriptor> = descriptor_text
        .parse()
        .map_err(|source| StorageError::AppDataIo { component, source })?;
    let dacl = descriptor.dacl().ok_or_else(|| StorageError::AppDataIo {
        component,
        source: std::io::Error::other("generated security descriptor has no DACL"),
    })?;
    SetNamedSecurityInfo(
        path.as_os_str(),
        SE_FILE_OBJECT,
        SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
        None,
        None,
        Some(dacl),
        None,
    )
    .map_err(|source| StorageError::AppDataIo { component, source })
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[cfg(windows)]
    fn assert_current_user_only(path: &Path) {
        use windows_permissions::constants::{
            AccessRights, SeObjectType::SE_FILE_OBJECT, SecurityInformation,
        };
        use windows_permissions::utilities::current_process_sid;
        use windows_permissions::wrappers::GetNamedSecurityInfo;

        let descriptor =
            GetNamedSecurityInfo(path.as_os_str(), SE_FILE_OBJECT, SecurityInformation::Dacl)
                .unwrap();
        let dacl = descriptor.dacl().unwrap();
        assert_eq!(dacl.len(), 1);
        let ace = dacl.get_ace(0).unwrap();
        assert_eq!(ace.sid(), Some(&*current_process_sid().unwrap()));
        assert_eq!(ace.mask(), AccessRights::FileAllAccess);
    }

    #[cfg(unix)]
    fn assert_unix_mode(path: &Path, expected: u32) {
        use std::os::unix::fs::PermissionsExt as _;

        let actual = fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(actual, expected);
    }

    #[test]
    fn resolves_all_platforms_without_cwd_fallback() {
        let root = tempdir().unwrap();
        let environment = AppDataEnvironment {
            local_app_data: Some(root.path().join("local")),
            home: Some(root.path().join("home")),
            xdg_data_home: Some(root.path().join("xdg")),
        };

        assert_eq!(
            AppDataPaths::resolve(PlatformFamily::Windows, &environment)
                .unwrap()
                .data_directory,
            root.path().join("local").join("CodexMonitor")
        );
        assert_eq!(
            AppDataPaths::resolve(PlatformFamily::MacOs, &environment)
                .unwrap()
                .data_directory,
            root.path()
                .join("home")
                .join("Library")
                .join("Application Support")
                .join("CodexMonitor")
        );
        assert_eq!(
            AppDataPaths::resolve(PlatformFamily::Unix, &environment)
                .unwrap()
                .data_directory,
            root.path().join("xdg").join("codex-monitor")
        );

        let missing = AppDataEnvironment::default();
        assert!(AppDataPaths::resolve(PlatformFamily::Windows, &missing).is_err());
        assert!(AppDataPaths::resolve(PlatformFamily::MacOs, &missing).is_err());
        assert!(AppDataPaths::resolve(PlatformFamily::Unix, &missing).is_err());
    }

    #[test]
    fn prepares_only_synthetic_private_paths() {
        let root = tempdir().unwrap();
        let paths = AppDataPaths::resolve(
            PlatformFamily::Unix,
            &AppDataEnvironment {
                home: Some(root.path().to_path_buf()),
                ..AppDataEnvironment::default()
            },
        )
        .unwrap();

        paths.prepare().unwrap();
        paths.secure_settings_file().unwrap();
        let log = paths.secure_log_file("engine.log").unwrap();

        assert!(paths.database_file.is_file());
        assert!(paths.settings_file.is_file());
        assert!(log.is_file());

        #[cfg(windows)]
        for path in [
            &paths.data_directory,
            &paths.logs_directory,
            &paths.database_file,
            &paths.settings_file,
            &log,
        ] {
            assert_current_user_only(path);
        }

        #[cfg(unix)]
        {
            assert_unix_mode(&paths.data_directory, 0o700);
            assert_unix_mode(&paths.logs_directory, 0o700);
            assert_unix_mode(&paths.database_file, 0o600);
            assert_unix_mode(&paths.settings_file, 0o600);
            assert_unix_mode(&log, 0o600);
        }
    }
}
