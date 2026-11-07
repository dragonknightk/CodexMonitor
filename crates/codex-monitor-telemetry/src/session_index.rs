//! Optional, read-only display-name resolution from `session_index.jsonl`.
//!
//! The resolver keeps accepted names only in memory. It has no storage API and
//! never exposes raw JSON, paths, identifiers, or I/O error strings through its
//! diagnostic surface.

use std::collections::HashMap;
use std::fs::{self, File, Metadata};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use serde_json::Value;

/// Maximum bytes read from one session-index file.
pub const MAX_SESSION_INDEX_FILE_BYTES: usize = 16 * 1024 * 1024;
/// Maximum bytes accepted for one JSONL line, excluding the LF separator.
pub const MAX_SESSION_INDEX_LINE_BYTES: usize = 64 * 1024;
/// Maximum bytes accepted for one session or conversation identifier.
pub const MAX_SESSION_INDEX_ID_BYTES: usize = 256;
/// Maximum UTF-8 bytes accepted for one trimmed display name.
pub const MAX_SESSION_DISPLAY_NAME_BYTES: usize = 512;
/// Maximum Unicode characters exposed by the Dashboard display-name schema.
pub const MAX_SESSION_DISPLAY_NAME_CHARACTERS: usize = 256;
/// Maximum unique identifiers retained in the in-memory index.
pub const MAX_SESSION_INDEX_ENTRIES: usize = 50_000;

/// Independent bounds for one optional session-index file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionIndexLimits {
    file_bytes: usize,
    line_bytes: usize,
    id_bytes: usize,
    name_bytes: usize,
    entries: usize,
}

impl SessionIndexLimits {
    /// Construct a coherent non-zero limit set.
    #[must_use]
    pub const fn new(
        max_file_bytes: usize,
        max_line_bytes: usize,
        max_id_bytes: usize,
        max_name_bytes: usize,
        max_entries: usize,
    ) -> Option<Self> {
        if max_file_bytes == 0
            || max_line_bytes == 0
            || max_id_bytes == 0
            || max_name_bytes == 0
            || max_entries == 0
            || max_line_bytes > max_file_bytes
            || max_id_bytes > max_line_bytes
            || max_name_bytes > max_line_bytes
        {
            return None;
        }
        Some(Self {
            file_bytes: max_file_bytes,
            line_bytes: max_line_bytes,
            id_bytes: max_id_bytes,
            name_bytes: max_name_bytes,
            entries: max_entries,
        })
    }
}

impl Default for SessionIndexLimits {
    fn default() -> Self {
        Self {
            file_bytes: MAX_SESSION_INDEX_FILE_BYTES,
            line_bytes: MAX_SESSION_INDEX_LINE_BYTES,
            id_bytes: MAX_SESSION_INDEX_ID_BYTES,
            name_bytes: MAX_SESSION_DISPLAY_NAME_BYTES,
            entries: MAX_SESSION_INDEX_ENTRIES,
        }
    }
}

/// Closed resolver state suitable for privacy-safe diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionIndexState {
    /// A stable file was loaded; malformed lines may have been skipped.
    Loaded,
    /// The optional file does not exist.
    Missing,
    /// The path is not a regular readable file.
    Unavailable,
    /// The file exceeded the configured byte bound.
    FileTooLarge,
    /// Metadata changed while the bounded read was in progress.
    ChangedDuringRead,
}

/// Safe summary of one refresh attempt or cached index reuse.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionIndexRefresh {
    state: SessionIndexState,
    entry_count: usize,
    rejected_lines: usize,
    duplicate_ids: usize,
    reloaded: bool,
}

impl SessionIndexRefresh {
    #[must_use]
    /// Return the closed load state.
    pub const fn state(self) -> SessionIndexState {
        self.state
    }

    #[must_use]
    /// Return the unique names retained after the load.
    pub const fn entry_count(self) -> usize {
        self.entry_count
    }

    #[must_use]
    /// Return the count of malformed or out-of-bounds lines skipped.
    pub const fn rejected_lines(self) -> usize {
        self.rejected_lines
    }

    #[must_use]
    /// Return how many accepted lines replaced an earlier ID.
    pub const fn duplicate_ids(self) -> usize {
        self.duplicate_ids
    }

    #[must_use]
    /// Return whether this operation read or re-evaluated the source file.
    pub const fn reloaded(self) -> bool {
        self.reloaded
    }

    const fn empty(state: SessionIndexState) -> Self {
        Self {
            state,
            entry_count: 0,
            rejected_lines: 0,
            duplicate_ids: 0,
            reloaded: true,
        }
    }

    const fn cached(mut self) -> Self {
        self.reloaded = false;
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileSignature {
    length: u64,
    modified: Option<SystemTime>,
}

impl FileSignature {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FileProbe {
    Missing,
    Present(FileSignature),
    Unavailable,
}

struct LoadedIndex {
    signature: Option<FileSignature>,
    names: HashMap<String, String>,
    report: SessionIndexRefresh,
}

struct CachedIndex {
    initialized: bool,
    signature: Option<FileSignature>,
    names: HashMap<String, String>,
    report: SessionIndexRefresh,
}

impl Default for CachedIndex {
    fn default() -> Self {
        Self {
            initialized: false,
            signature: None,
            names: HashMap::new(),
            report: SessionIndexRefresh::empty(SessionIndexState::Missing),
        }
    }
}

impl CachedIndex {
    fn replace(&mut self, loaded: LoadedIndex) -> SessionIndexRefresh {
        self.initialized = true;
        self.signature = loaded.signature;
        self.names = loaded.names;
        self.report = loaded.report;
        loaded.report
    }

    fn can_reuse(&self, probe: FileProbe) -> bool {
        if !self.initialized {
            return false;
        }
        match (probe, self.report.state, self.signature) {
            (FileProbe::Missing, SessionIndexState::Missing, None) => true,
            (FileProbe::Present(current), SessionIndexState::Loaded, Some(previous)) => {
                current == previous
            }
            _ => false,
        }
    }
}

/// Thread-safe, memory-only resolver for optional session display names.
pub struct SessionIndexResolver {
    path: PathBuf,
    limits: SessionIndexLimits,
    cache: Mutex<CachedIndex>,
}

impl SessionIndexResolver {
    #[must_use]
    /// Construct a resolver with the documented default bounds.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self::with_limits(path, SessionIndexLimits::default())
    }

    #[must_use]
    /// Construct a resolver with an explicit validated limit set.
    pub fn with_limits(path: impl Into<PathBuf>, limits: SessionIndexLimits) -> Self {
        Self {
            path: path.into(),
            limits,
            cache: Mutex::new(CachedIndex::default()),
        }
    }

    /// Resolve the first valid candidate ID in caller-provided precedence order.
    ///
    /// The file is refreshed when its length or modification time changes. The
    /// returned name is owned so no reference to the cache escapes this boundary.
    #[must_use]
    pub fn resolve(&self, candidate_ids: &[&str]) -> Option<String> {
        let mut cache = self.lock_cache();
        self.refresh_if_changed_locked(&mut cache);
        resolve_candidates(&cache.names, candidate_ids, self.limits.id_bytes)
    }

    /// Resolve one display name for each caller record under one cache lock.
    ///
    /// Each outer element represents one record. Its inner identifiers are
    /// examined in caller-provided precedence order after the cache has been
    /// refreshed at most once for the complete batch.
    #[must_use]
    pub fn resolve_batch<T>(&self, candidate_groups: &[Vec<T>]) -> Vec<Option<String>>
    where
        T: AsRef<str>,
    {
        let mut cache = self.lock_cache();
        self.refresh_if_changed_locked(&mut cache);
        candidate_groups
            .iter()
            .map(|candidates| resolve_candidates(&cache.names, candidates, self.limits.id_bytes))
            .collect()
    }

    /// Find stable candidate identifiers whose resolved display name contains a query.
    ///
    /// Matching lowercases both operands with Unicode case conversion and then
    /// performs a non-empty substring test. It intentionally performs no Unicode
    /// normalization. Results are bounded by the loaded index entry limit and
    /// returned in identifier lexicographic order.
    #[must_use]
    pub fn find_identifiers_by_display_name(&self, query: &str) -> Vec<String> {
        let query = query.to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }

        let mut cache = self.lock_cache();
        self.refresh_if_changed_locked(&mut cache);
        let mut identifiers = cache
            .names
            .iter()
            .filter(|(_, name)| name.to_lowercase().contains(&query))
            .map(|(identifier, _)| identifier.clone())
            .collect::<Vec<_>>();
        identifiers.sort_unstable();
        identifiers
    }

    /// Unconditionally reload the optional file and replace the in-memory cache.
    #[must_use]
    pub fn refresh(&self) -> SessionIndexRefresh {
        let mut cache = self.lock_cache();
        cache.replace(load_index(&self.path, self.limits))
    }

    /// Refresh only when the file metadata changed since the last successful load.
    #[must_use]
    pub fn refresh_if_changed(&self) -> SessionIndexRefresh {
        let mut cache = self.lock_cache();
        self.refresh_if_changed_locked(&mut cache)
    }

    fn refresh_if_changed_locked(&self, cache: &mut CachedIndex) -> SessionIndexRefresh {
        let probe = probe_file(&self.path);
        if cache.can_reuse(probe) {
            return cache.report.cached();
        }
        cache.replace(load_index(&self.path, self.limits))
    }

    fn lock_cache(&self) -> MutexGuard<'_, CachedIndex> {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn resolve_candidates<T>(
    names: &HashMap<String, String>,
    candidate_ids: &[T],
    max_id_bytes: usize,
) -> Option<String>
where
    T: AsRef<str>,
{
    candidate_ids
        .iter()
        .map(AsRef::as_ref)
        .filter(|value| valid_identifier(value, max_id_bytes))
        .find_map(|value| names.get(value).cloned())
}

fn probe_file(path: &Path) -> FileProbe {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {
            FileProbe::Present(FileSignature::from_metadata(&metadata))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => FileProbe::Missing,
        Ok(_) | Err(_) => FileProbe::Unavailable,
    }
}

fn load_index(path: &Path, limits: SessionIndexLimits) -> LoadedIndex {
    let path_metadata = match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return empty_load(SessionIndexState::Missing, None);
        }
        Ok(_) | Err(_) => return empty_load(SessionIndexState::Unavailable, None),
    };
    let path_signature = FileSignature::from_metadata(&path_metadata);
    if path_metadata.len() > usize_to_u64(limits.file_bytes) {
        return empty_load(SessionIndexState::FileTooLarge, Some(path_signature));
    }

    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return empty_load(SessionIndexState::Missing, None);
        }
        Err(_) => return empty_load(SessionIndexState::Unavailable, None),
    };
    let metadata_before = match file.metadata() {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) | Err(_) => return empty_load(SessionIndexState::Unavailable, None),
    };
    let signature_before = FileSignature::from_metadata(&metadata_before);
    if signature_before != path_signature {
        return empty_load(SessionIndexState::ChangedDuringRead, None);
    }
    if metadata_before.len() > usize_to_u64(limits.file_bytes) {
        return empty_load(SessionIndexState::FileTooLarge, Some(signature_before));
    }

    let read_limit = usize_to_u64(limits.file_bytes).saturating_add(1);
    let initial_capacity = usize::try_from(metadata_before.len())
        .unwrap_or(limits.file_bytes)
        .min(limits.file_bytes);
    let mut bytes = Vec::with_capacity(initial_capacity);
    let mut reader = file.take(read_limit);
    if reader.read_to_end(&mut bytes).is_err() {
        return empty_load(SessionIndexState::Unavailable, Some(signature_before));
    }
    if bytes.len() > limits.file_bytes {
        return empty_load(SessionIndexState::FileTooLarge, Some(signature_before));
    }

    let signature_after = match reader.get_ref().metadata() {
        Ok(metadata) if metadata.is_file() => FileSignature::from_metadata(&metadata),
        Ok(_) | Err(_) => {
            return empty_load(SessionIndexState::Unavailable, Some(signature_before));
        }
    };
    if signature_before != signature_after {
        return empty_load(SessionIndexState::ChangedDuringRead, None);
    }

    let (names, rejected_lines, duplicate_ids) = parse_lines(&bytes, limits);
    LoadedIndex {
        signature: Some(signature_after),
        report: SessionIndexRefresh {
            state: SessionIndexState::Loaded,
            entry_count: names.len(),
            rejected_lines,
            duplicate_ids,
            reloaded: true,
        },
        names,
    }
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn empty_load(state: SessionIndexState, signature: Option<FileSignature>) -> LoadedIndex {
    LoadedIndex {
        signature,
        names: HashMap::new(),
        report: SessionIndexRefresh::empty(state),
    }
}

fn parse_lines(
    bytes: &[u8],
    limits: SessionIndexLimits,
) -> (HashMap<String, String>, usize, usize) {
    let mut names = HashMap::new();
    let mut rejected_lines = 0;
    let mut duplicate_ids = 0;
    let mut start = 0;

    while start < bytes.len() {
        let relative_end = bytes[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .unwrap_or(bytes.len() - start);
        let end = start + relative_end;
        let line = &bytes[start..end];
        start = if end < bytes.len() { end + 1 } else { end };

        if line.len() > limits.line_bytes {
            rejected_lines += 1;
            continue;
        }
        let Ok(Value::Object(record)) = serde_json::from_slice::<Value>(line) else {
            rejected_lines += 1;
            continue;
        };
        let Some(identifier) = record.get("id").and_then(Value::as_str) else {
            rejected_lines += 1;
            continue;
        };
        let Some(name) = record.get("thread_name").and_then(Value::as_str) else {
            rejected_lines += 1;
            continue;
        };
        if !valid_identifier(identifier, limits.id_bytes) {
            rejected_lines += 1;
            continue;
        }
        let Some(name) = validated_display_name(name, limits.name_bytes) else {
            rejected_lines += 1;
            continue;
        };
        if !names.contains_key(identifier) && names.len() >= limits.entries {
            rejected_lines += 1;
            continue;
        }
        if names.insert(identifier.to_owned(), name).is_some() {
            duplicate_ids += 1;
        }
    }

    (names, rejected_lines, duplicate_ids)
}

fn valid_identifier(value: &str, max_bytes: usize) -> bool {
    if value.is_empty() || value.len() > max_bytes {
        return false;
    }
    value.char_indices().all(|(index, character)| {
        if index == 0 {
            character.is_ascii_alphanumeric()
        } else {
            character.is_ascii_alphanumeric()
                || matches!(character, '.' | '_' | ':' | '+' | '(' | ')' | '-')
        }
    })
}

fn validated_display_name(value: &str, max_bytes: usize) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > max_bytes
        || value.chars().count() > MAX_SESSION_DISPLAY_NAME_CHARACTERS
        || value.chars().any(is_unsafe_display_character)
    {
        return None;
    }
    Some(value.to_owned())
}

const fn is_unsafe_display_character(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "codex-monitor-session-index-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn index_path(&self) -> PathBuf {
            self.0.join("session_index.jsonl")
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn limits(
        max_file_bytes: usize,
        max_line_bytes: usize,
        max_id_bytes: usize,
        max_name_bytes: usize,
        max_entries: usize,
    ) -> SessionIndexLimits {
        SessionIndexLimits::new(
            max_file_bytes,
            max_line_bytes,
            max_id_bytes,
            max_name_bytes,
            max_entries,
        )
        .unwrap()
    }

    #[test]
    fn resolves_trimmed_names_in_candidate_precedence_order() {
        let directory = TestDirectory::new();
        let path = directory.index_path();
        fs::write(
            &path,
            concat!(
                "{\"id\":\"session-1\",\"thread_name\":\"  First 会话  \"}\n",
                "{\"id\":\"conversation-2\",\"thread_name\":\"Second\"}\n"
            ),
        )
        .unwrap();
        let resolver = SessionIndexResolver::new(path);

        assert_eq!(
            resolver.resolve(&["missing", "conversation-2", "session-1"]),
            Some("Second".to_owned())
        );
        assert_eq!(
            resolver.resolve(&["session-1"]),
            Some("First 会话".to_owned())
        );
        let cached = resolver.refresh_if_changed();
        assert_eq!(cached.state(), SessionIndexState::Loaded);
        assert_eq!(cached.entry_count(), 2);
        assert!(!cached.reloaded());
    }

    #[test]
    fn batch_resolution_refreshes_once_and_preserves_each_candidate_precedence() {
        let directory = TestDirectory::new();
        let path = directory.index_path();
        fs::write(
            &path,
            concat!(
                "{\"id\":\"session-1\",\"thread_name\":\"First\"}\n",
                "{\"id\":\"conversation-2\",\"thread_name\":\"Second\"}\n"
            ),
        )
        .unwrap();
        let resolver = SessionIndexResolver::new(path);
        let candidates = vec![
            vec!["missing", "conversation-2", "session-1"],
            vec!["invalid id", "session-1"],
            Vec::new(),
        ];

        assert_eq!(
            resolver.resolve_batch(&candidates),
            vec![Some("Second".to_owned()), Some("First".to_owned()), None]
        );
        let cached = resolver.refresh_if_changed();
        assert_eq!(cached.state(), SessionIndexState::Loaded);
        assert!(!cached.reloaded());
    }

    #[test]
    fn display_name_reverse_lookup_is_casefolded_substring_but_not_normalized() {
        let directory = TestDirectory::new();
        let path = directory.index_path();
        fs::write(
            &path,
            concat!(
                "{\"id\":\"zeta-id\",\"thread_name\":\"Alpha Mission\"}\n",
                "{\"id\":\"alpha-id\",\"thread_name\":\"ALPHABET\"}\n",
                "{\"id\":\"unicode-id\",\"thread_name\":\"ÄPFEL 会话\"}\n",
                "{\"id\":\"composed-id\",\"thread_name\":\"Café\"}\n"
            ),
        )
        .unwrap();
        let resolver = SessionIndexResolver::new(path);

        assert_eq!(
            resolver.find_identifiers_by_display_name("alpha"),
            vec!["alpha-id".to_owned(), "zeta-id".to_owned()]
        );
        assert_eq!(
            resolver.find_identifiers_by_display_name("ALPHA MISSION"),
            vec!["zeta-id".to_owned()]
        );
        assert_eq!(
            resolver.find_identifiers_by_display_name("äpf"),
            vec!["unicode-id".to_owned()]
        );
        assert_eq!(
            resolver.find_identifiers_by_display_name("会"),
            vec!["unicode-id".to_owned()]
        );
        assert_eq!(
            resolver.find_identifiers_by_display_name("CAFÉ"),
            vec!["composed-id".to_owned()]
        );
        assert!(
            resolver
                .find_identifiers_by_display_name("cafe\u{301}")
                .is_empty()
        );
        assert!(resolver.find_identifiers_by_display_name("").is_empty());
    }

    #[test]
    fn batch_and_reverse_lookup_fail_safely_for_missing_or_malformed_indexes() {
        let missing_directory = TestDirectory::new();
        let missing = SessionIndexResolver::new(missing_directory.index_path());
        assert_eq!(missing.resolve_batch(&[vec!["session-1"]]), vec![None]);
        assert!(
            missing
                .find_identifiers_by_display_name("session")
                .is_empty()
        );

        let malformed_directory = TestDirectory::new();
        let malformed_path = malformed_directory.index_path();
        fs::write(&malformed_path, "{synthetic-malformed-json}\n").unwrap();
        let malformed = SessionIndexResolver::new(malformed_path);
        assert_eq!(malformed.resolve_batch(&[vec!["session-1"]]), vec![None]);
        assert!(
            malformed
                .find_identifiers_by_display_name("synthetic")
                .is_empty()
        );
    }

    #[test]
    fn missing_file_is_an_empty_safe_cache() {
        let directory = TestDirectory::new();
        let resolver = SessionIndexResolver::new(directory.index_path());

        assert_eq!(resolver.resolve(&["session-1"]), None);
        let cached = resolver.refresh_if_changed();
        assert_eq!(cached.state(), SessionIndexState::Missing);
        assert_eq!(cached.entry_count(), 0);
        assert_eq!(cached.rejected_lines(), 0);
        assert!(!cached.reloaded());
    }

    #[test]
    fn removing_a_loaded_file_clears_cached_names() {
        let directory = TestDirectory::new();
        let path = directory.index_path();
        fs::write(&path, "{\"id\":\"session-1\",\"thread_name\":\"First\"}\n").unwrap();
        let resolver = SessionIndexResolver::new(path.clone());
        assert_eq!(resolver.resolve(&["session-1"]), Some("First".to_owned()));

        fs::remove_file(path).unwrap();
        let report = resolver.refresh_if_changed();
        assert_eq!(report.state(), SessionIndexState::Missing);
        assert_eq!(report.entry_count(), 0);
        assert_eq!(resolver.resolve(&["session-1"]), None);
    }

    #[test]
    fn malformed_line_does_not_hide_other_valid_names() {
        let directory = TestDirectory::new();
        let path = directory.index_path();
        fs::write(
            &path,
            concat!(
                "{\"id\":\"session-1\",\"thread_name\":\"First\"}\n",
                "{synthetic-secret-not-json}\n",
                "{\"id\":\"session-2\",\"thread_name\":\"Second\"}\n"
            ),
        )
        .unwrap();
        let resolver = SessionIndexResolver::new(path);

        let report = resolver.refresh();
        assert_eq!(report.state(), SessionIndexState::Loaded);
        assert_eq!(report.entry_count(), 2);
        assert_eq!(report.rejected_lines(), 1);
        assert_eq!(resolver.resolve(&["session-2"]), Some("Second".to_owned()));
        assert!(!format!("{report:?}").contains("synthetic-secret"));
    }

    #[test]
    fn duplicate_identifier_uses_the_last_valid_name() {
        let directory = TestDirectory::new();
        let path = directory.index_path();
        fs::write(
            &path,
            concat!(
                "{\"id\":\"session-1\",\"thread_name\":\"Old\"}\n",
                "{\"id\":\"session-1\",\"thread_name\":\"New\"}\n"
            ),
        )
        .unwrap();
        let resolver = SessionIndexResolver::new(path);

        let report = resolver.refresh();
        assert_eq!(report.entry_count(), 1);
        assert_eq!(report.duplicate_ids(), 1);
        assert_eq!(resolver.resolve(&["session-1"]), Some("New".to_owned()));
    }

    #[test]
    fn overlong_name_and_line_are_skipped_independently() {
        let directory = TestDirectory::new();
        let path = directory.index_path();
        let long_name = "n".repeat(17);
        let long_line_value = "x".repeat(100);
        fs::write(
            &path,
            format!(
                "{{\"id\":\"long-name\",\"thread_name\":\"{long_name}\"}}\n\
                 {{\"id\":\"long-line\",\"thread_name\":\"ok\",\"padding\":\"{long_line_value}\"}}\n\
                 {{\"id\":\"valid\",\"thread_name\":\"Accepted\"}}\n"
            ),
        )
        .unwrap();
        let resolver = SessionIndexResolver::with_limits(path, limits(512, 96, 32, 16, 10));

        let report = resolver.refresh();
        assert_eq!(report.state(), SessionIndexState::Loaded);
        assert_eq!(report.entry_count(), 1);
        assert_eq!(report.rejected_lines(), 2);
        assert_eq!(resolver.resolve(&["long-name", "long-line"]), None);
        assert_eq!(resolver.resolve(&["valid"]), Some("Accepted".to_owned()));
    }

    #[test]
    fn display_names_never_exceed_the_dashboard_character_contract() {
        let directory = TestDirectory::new();
        let path = directory.index_path();
        fs::write(
            &path,
            format!(
                "{{\"id\":\"too-many\",\"thread_name\":\"{}\"}}\n\
                 {{\"id\":\"exact\",\"thread_name\":\"{}\"}}\n",
                "a".repeat(MAX_SESSION_DISPLAY_NAME_CHARACTERS + 1),
                "会".repeat(MAX_SESSION_DISPLAY_NAME_CHARACTERS)
            ),
        )
        .unwrap();
        let resolver = SessionIndexResolver::with_limits(path, limits(4_096, 2_048, 32, 1_024, 10));

        let report = resolver.refresh();
        assert_eq!(report.entry_count(), 1);
        assert_eq!(report.rejected_lines(), 1);
        assert_eq!(
            resolver.resolve(&["exact"]),
            Some("会".repeat(MAX_SESSION_DISPLAY_NAME_CHARACTERS))
        );
    }

    #[test]
    fn oversized_file_is_not_partially_loaded() {
        let directory = TestDirectory::new();
        let path = directory.index_path();
        fs::write(
            &path,
            concat!(
                "{\"id\":\"session-1\",\"thread_name\":\"First\"}\n",
                "{\"padding\":\"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"}\n"
            ),
        )
        .unwrap();
        let resolver = SessionIndexResolver::with_limits(path, limits(64, 64, 32, 32, 10));

        let report = resolver.refresh();
        assert_eq!(report.state(), SessionIndexState::FileTooLarge);
        assert_eq!(report.entry_count(), 0);
        assert_eq!(resolver.resolve(&["session-1"]), None);
    }

    #[test]
    fn non_string_and_unsafe_fields_are_rejected() {
        let directory = TestDirectory::new();
        let path = directory.index_path();
        fs::write(
            &path,
            concat!(
                "{\"id\":42,\"thread_name\":\"Number ID\"}\n",
                "{\"id\":\"object-name\",\"thread_name\":{}}\n",
                "{\"id\":\"null-name\",\"thread_name\":null}\n",
                "{\"id\":\"unsafe-name\",\"thread_name\":\"line\\nbreak\"}\n",
                "{\"id\":\"bad id\",\"thread_name\":\"Bad ID\"}\n",
                "{\"id\":\"valid-id\",\"thread_name\":\"Valid\"}\n"
            ),
        )
        .unwrap();
        let resolver = SessionIndexResolver::new(path);

        let report = resolver.refresh();
        assert_eq!(report.entry_count(), 1);
        assert_eq!(report.rejected_lines(), 5);
        assert_eq!(resolver.resolve(&["valid-id"]), Some("Valid".to_owned()));
    }

    #[test]
    fn explicit_and_metadata_refresh_replace_the_cache_after_file_changes() {
        let directory = TestDirectory::new();
        let path = directory.index_path();
        fs::write(&path, "{\"id\":\"session-1\",\"thread_name\":\"First\"}\n").unwrap();
        let resolver = SessionIndexResolver::new(path.clone());
        assert_eq!(resolver.resolve(&["session-1"]), Some("First".to_owned()));

        fs::write(
            &path,
            "{\"id\":\"session-2\",\"thread_name\":\"Second, longer\"}\n",
        )
        .unwrap();
        let metadata_refresh = resolver.refresh_if_changed();
        assert!(metadata_refresh.reloaded());
        assert_eq!(resolver.resolve(&["session-1"]), None);
        assert_eq!(
            resolver.resolve(&["session-2"]),
            Some("Second, longer".to_owned())
        );

        fs::write(&path, "{\"id\":\"session-3\",\"thread_name\":\"Third\"}\n").unwrap();
        let forced_refresh = resolver.refresh();
        assert!(forced_refresh.reloaded());
        assert_eq!(resolver.resolve(&["session-2"]), None);
        assert_eq!(resolver.resolve(&["session-3"]), Some("Third".to_owned()));
    }

    #[test]
    fn entry_limit_keeps_memory_bounded_but_allows_duplicate_updates() {
        let directory = TestDirectory::new();
        let path = directory.index_path();
        fs::write(
            &path,
            concat!(
                "{\"id\":\"session-1\",\"thread_name\":\"First\"}\n",
                "{\"id\":\"session-2\",\"thread_name\":\"Rejected\"}\n",
                "{\"id\":\"session-1\",\"thread_name\":\"Updated\"}\n"
            ),
        )
        .unwrap();
        let resolver = SessionIndexResolver::with_limits(path, limits(512, 256, 32, 32, 1));

        let report = resolver.refresh();
        assert_eq!(report.entry_count(), 1);
        assert_eq!(report.rejected_lines(), 1);
        assert_eq!(report.duplicate_ids(), 1);
        assert_eq!(resolver.resolve(&["session-1"]), Some("Updated".to_owned()));
    }

    #[test]
    fn invalid_limit_sets_are_unrepresentable() {
        assert_eq!(SessionIndexLimits::new(0, 1, 1, 1, 1), None);
        assert_eq!(SessionIndexLimits::new(8, 9, 1, 1, 1), None);
        assert_eq!(SessionIndexLimits::new(8, 8, 9, 1, 1), None);
        assert_eq!(SessionIndexLimits::new(8, 8, 1, 9, 1), None);
        assert_eq!(SessionIndexLimits::new(8, 8, 1, 1, 0), None);
    }

    #[test]
    fn directory_path_reports_only_a_closed_unavailable_state() {
        let directory = TestDirectory::new();
        let resolver = SessionIndexResolver::new(directory.0.clone());

        let report = resolver.refresh();
        assert_eq!(report.state(), SessionIndexState::Unavailable);
        assert_eq!(
            format!("{report:?}"),
            "SessionIndexRefresh { state: Unavailable, entry_count: 0, rejected_lines: 0, duplicate_ids: 0, reloaded: true }"
        );
    }
}
