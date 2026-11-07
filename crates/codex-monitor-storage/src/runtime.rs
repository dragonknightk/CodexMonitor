use super::{AppDataPaths, StorageConfig, StorageReader, StorageResult, StorageWriter};

/// Storage resources rooted in the operating system's current-user data directory.
pub struct CurrentUserStorage {
    paths: AppDataPaths,
    reader: StorageReader,
    writer: StorageWriter,
}

impl CurrentUserStorage {
    /// Resolve, secure, migrate, and open all current-user storage resources.
    ///
    /// # Errors
    ///
    /// Returns an error when current-user paths are unavailable, permissions
    /// cannot be restricted, or the database cannot be opened and migrated.
    pub fn open() -> StorageResult<Self> {
        Self::open_paths(AppDataPaths::resolve_current_user()?)
    }

    /// 使用已经解析的应用数据路径准备并打开全部存储资源。
    ///
    /// # Errors
    ///
    /// 路径无法准备、权限无法收紧，或数据库无法打开和迁移时返回错误。
    pub fn open_paths(paths: AppDataPaths) -> StorageResult<Self> {
        paths.prepare()?;
        paths.secure_settings_file()?;
        let writer = StorageWriter::open(StorageConfig::new(paths.database_file.clone()))?;
        let reader = StorageReader::new(paths.database_file.clone());
        Ok(Self {
            paths,
            reader,
            writer,
        })
    }

    #[must_use]
    pub const fn paths(&self) -> &AppDataPaths {
        &self.paths
    }

    #[must_use]
    pub const fn reader(&self) -> &StorageReader {
        &self.reader
    }

    #[must_use]
    pub const fn writer(&self) -> &StorageWriter {
        &self.writer
    }

    /// Drain pending storage writes and stop the dedicated writer thread.
    ///
    /// # Errors
    ///
    /// Returns an error when the writer actor has already stopped or its thread
    /// cannot be joined.
    pub async fn shutdown(&self) -> StorageResult<()> {
        self.writer.shutdown().await
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::super::{AppDataEnvironment, PlatformFamily};
    use super::*;

    #[tokio::test]
    async fn opens_every_managed_resource_from_one_secured_root() {
        let root = tempdir().unwrap();
        let paths = AppDataPaths::resolve(
            PlatformFamily::Unix,
            &AppDataEnvironment {
                home: Some(root.path().to_path_buf()),
                ..AppDataEnvironment::default()
            },
        )
        .unwrap();
        let storage = CurrentUserStorage::open_paths(paths.clone()).unwrap();

        assert_eq!(storage.paths(), &paths);
        assert_eq!(storage.reader().database_path(), paths.database_file);
        assert_eq!(storage.writer().database_path(), paths.database_file);
        assert!(paths.settings_file.is_file());
        storage.shutdown().await.unwrap();
    }
}
