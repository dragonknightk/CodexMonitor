use std::path::PathBuf;

use super::{StorageConfig, StorageReader, StorageResult, StorageWriter};

/// Open a migrated database and its sole writer for cross-crate integration tests.
///
/// # Errors
///
/// Returns a storage error when the database cannot be migrated or the writer
/// thread cannot start.
pub fn open_test_database(database_path: PathBuf) -> StorageResult<(StorageReader, StorageWriter)> {
    let writer = StorageWriter::open(StorageConfig::new(database_path.clone()))?;
    let reader = StorageReader::new(database_path);
    Ok((reader, writer))
}
