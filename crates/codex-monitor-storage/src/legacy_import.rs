use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rusqlite::config::DbConfig;
use rusqlite::limits::Limit;
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use thiserror::Error;

use super::migrations;

mod importer;

pub(crate) use importer::import_legacy_database;
pub use importer::{LegacyImportOutcome, LegacyImportStatus};

const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";
const MAX_SOURCE_RELATIONS: usize = 64;
const MAX_SCHEMA_OBJECTS: usize = 128;
const MAX_RELATION_NAME_BYTES: usize = 128;
const MAX_SOURCE_COLUMNS: usize = 32;
const MAX_SOURCE_INDEXES: usize = 8;
const MAX_COLUMN_NAME_BYTES: usize = 64;
const MAX_DECLARED_TYPE_BYTES: usize = 32;
const SOURCE_BUSY_TIMEOUT: Duration = Duration::from_millis(250);
const SCHEMA_PROGRESS_INTERVAL: i32 = 1_000;
const MAX_SCHEMA_PROGRESS_CALLBACKS: u64 = 10_000;
const MAX_SCHEMA_INSPECTION_TIME: Duration = Duration::from_secs(5);

const TOKEN_EVENTS_TABLE: &str = "codex_token_events";
const CONVERSATIONS_TABLE: &str = "codex_conversations";
const RAW_RECORDS_TABLE: &str = "otel_log_records";
const CURRENT_SCHEMA_SENTINELS: [&str; 5] = [
    "schema_migrations",
    "telemetry_field_registry",
    "model_calls",
    "model_call_observations",
    "legacy_import_audit",
];

/// Recognized Python database schema that is safe to pass to the importer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LegacySchemaKind {
    StructuredV1,
    RawLegacy,
}

impl LegacySchemaKind {
    /// Stable value accepted by the privacy-safe import audit table.
    #[must_use]
    pub const fn audit_code(self) -> &'static str {
        match self {
            Self::StructuredV1 => "structured_v1",
            Self::RawLegacy => "raw_legacy",
        }
    }
}

/// Closed result of inspecting a selected `SQLite` source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LegacySchemaDetection {
    Legacy(LegacySchemaKind),
    CurrentDatabase,
    Unsupported,
}

/// Privacy-safe failure while opening or bounding an untrusted `SQLite` source.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum LegacySchemaDetectionError {
    #[error("legacy database source is unavailable")]
    SourceUnavailable,
    #[error("legacy database source is not a valid SQLite database")]
    InvalidDatabase,
    #[error("legacy database schema exceeds inspection limits")]
    SchemaTooComplex,
    #[error("legacy database contains executable schema objects")]
    ExecutableSchema,
}

/// Inspect one selected `SQLite` file without executing SQL stored in that file.
///
/// Every application statement issued by this function is a compiled-in
/// constant. The source is opened in a bounded read-only transaction with
/// defensive `SQLite` settings. Application code never selects the `sql` column
/// from `sqlite_schema` or passes stored schema text to `prepare`/`execute`;
/// `SQLite`'s own internal schema parsing runs under the inspection budget.
///
/// # Errors
///
/// Returns a closed error when the source path is unsafe, the file is malformed,
/// the schema is too complex, or executable schema objects are present.
pub fn detect_legacy_schema(
    source: &Path,
) -> Result<LegacySchemaDetection, LegacySchemaDetectionError> {
    let mut connection = open_source_connection(source)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Deferred)
        .map_err(invalid_database)?;
    let detection = inspect_source_schema(&transaction)?;
    transaction.rollback().map_err(invalid_database)?;
    Ok(detection)
}

fn open_source_connection(source: &Path) -> Result<Connection, LegacySchemaDetectionError> {
    validate_source_file(source)?;
    let connection = Connection::open_with_flags(source, source_open_flags())
        .map_err(|_| LegacySchemaDetectionError::InvalidDatabase)?;
    harden_source_connection(&connection)?;
    Ok(connection)
}

fn validate_source_file(source: &Path) -> Result<(), LegacySchemaDetectionError> {
    if !source.is_absolute() {
        return Err(LegacySchemaDetectionError::SourceUnavailable);
    }
    let metadata =
        fs::symlink_metadata(source).map_err(|_| LegacySchemaDetectionError::SourceUnavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(LegacySchemaDetectionError::SourceUnavailable);
    }

    let mut header = [0_u8; SQLITE_HEADER.len()];
    File::open(source)
        .map_err(|_| LegacySchemaDetectionError::SourceUnavailable)?
        .read_exact(&mut header)
        .map_err(|_| LegacySchemaDetectionError::InvalidDatabase)?;
    if &header != SQLITE_HEADER {
        return Err(LegacySchemaDetectionError::InvalidDatabase);
    }
    Ok(())
}

fn source_open_flags() -> OpenFlags {
    OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
        | OpenFlags::SQLITE_OPEN_NOFOLLOW
        | OpenFlags::SQLITE_OPEN_EXRESCODE
}

fn harden_source_connection(connection: &Connection) -> Result<(), LegacySchemaDetectionError> {
    if !connection.is_readonly("main").map_err(invalid_database)? {
        return Err(LegacySchemaDetectionError::InvalidDatabase);
    }
    for (config, enabled) in [
        (DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true),
        (DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false),
        (DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false),
        (DbConfig::SQLITE_DBCONFIG_ENABLE_VIEW, false),
        (DbConfig::SQLITE_DBCONFIG_ENABLE_FTS3_TOKENIZER, false),
        (DbConfig::SQLITE_DBCONFIG_WRITABLE_SCHEMA, false),
        (DbConfig::SQLITE_DBCONFIG_DQS_DDL, false),
        (DbConfig::SQLITE_DBCONFIG_DQS_DML, false),
        (DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true),
    ] {
        connection
            .set_db_config(config, enabled)
            .map_err(invalid_database)?;
    }
    for (limit, value) in [
        (Limit::SQLITE_LIMIT_LENGTH, 1_048_576),
        (Limit::SQLITE_LIMIT_SQL_LENGTH, 65_536),
        (Limit::SQLITE_LIMIT_COLUMN, 64),
        (Limit::SQLITE_LIMIT_EXPR_DEPTH, 32),
        (Limit::SQLITE_LIMIT_COMPOUND_SELECT, 3),
        (Limit::SQLITE_LIMIT_VDBE_OP, 100_000),
        (Limit::SQLITE_LIMIT_FUNCTION_ARG, 8),
        (Limit::SQLITE_LIMIT_ATTACHED, 0),
        (Limit::SQLITE_LIMIT_LIKE_PATTERN_LENGTH, 128),
        (Limit::SQLITE_LIMIT_VARIABLE_NUMBER, 16),
        (Limit::SQLITE_LIMIT_TRIGGER_DEPTH, 1),
        (Limit::SQLITE_LIMIT_WORKER_THREADS, 0),
    ] {
        connection
            .set_limit(limit, value)
            .map_err(invalid_database)?;
    }
    connection
        .pragma_update(None, "query_only", true)
        .map_err(invalid_database)?;
    connection
        .pragma_update(None, "mmap_size", 0_i64)
        .map_err(invalid_database)?;
    connection
        .pragma_update(None, "cell_size_check", true)
        .map_err(invalid_database)?;
    connection
        .pragma_update(None, "temp_store", "MEMORY")
        .map_err(invalid_database)?;
    connection
        .busy_timeout(SOURCE_BUSY_TIMEOUT)
        .map_err(invalid_database)
}

fn inspect_source_schema(
    connection: &Connection,
) -> Result<LegacySchemaDetection, LegacySchemaDetectionError> {
    let interrupted = Arc::new(AtomicBool::new(false));
    let callback_count = Arc::new(AtomicU64::new(0));
    let deadline = Instant::now() + MAX_SCHEMA_INSPECTION_TIME;
    let callback_interrupted = Arc::clone(&interrupted);
    let callback_count_clone = Arc::clone(&callback_count);
    connection
        .progress_handler(
            SCHEMA_PROGRESS_INTERVAL,
            Some(move || {
                let exhausted = callback_count_clone.fetch_add(1, Ordering::Relaxed)
                    >= MAX_SCHEMA_PROGRESS_CALLBACKS;
                let timed_out = Instant::now() >= deadline;
                if exhausted || timed_out {
                    callback_interrupted.store(true, Ordering::Relaxed);
                    true
                } else {
                    false
                }
            }),
        )
        .map_err(invalid_database)?;
    let result = inspect_source_schema_bounded(connection);
    let clear_result = connection.progress_handler(0, None::<fn() -> bool>);
    if interrupted.load(Ordering::Relaxed) {
        return Err(LegacySchemaDetectionError::SchemaTooComplex);
    }
    clear_result.map_err(invalid_database)?;
    result
}

fn inspect_source_schema_bounded(
    connection: &Connection,
) -> Result<LegacySchemaDetection, LegacySchemaDetectionError> {
    let objects = SchemaObjectInventory::load(connection)?;
    if objects.has_preflight_executable_relations() {
        return Err(LegacySchemaDetectionError::ExecutableSchema);
    }
    let relations = SchemaInventory::load(connection)?;
    if relations.has_current_sentinels() {
        if current_metadata_tables_match(connection, &relations)?
            && migrations::matches_current_schema_bounded(connection)
                .map_err(|_| LegacySchemaDetectionError::InvalidDatabase)?
        {
            return Ok(LegacySchemaDetection::CurrentDatabase);
        }
        return Ok(LegacySchemaDetection::Unsupported);
    }
    if relations.has_executable_relations() {
        return Err(LegacySchemaDetectionError::ExecutableSchema);
    }
    if !relations.has_only_known_legacy_tables() {
        return Ok(LegacySchemaDetection::Unsupported);
    }

    if objects.has_executable_objects() {
        return Err(LegacySchemaDetectionError::ExecutableSchema);
    }
    if !objects.has_only_known_legacy_objects() {
        return Ok(LegacySchemaDetection::Unsupported);
    }
    if source_application_id(connection)? != 0
        || legacy_tables_have_foreign_keys(connection)?
        || !legacy_indexes_match(connection)?
    {
        return Ok(LegacySchemaDetection::Unsupported);
    }

    let token_columns = load_columns(connection, LegacyTable::TokenEvents)?;
    let conversation_columns = load_columns(connection, LegacyTable::Conversations)?;
    let token_variant = table_variant(&token_columns, &TOKEN_STRUCTURED_COLUMNS);
    let conversation_variant = table_variant(&conversation_columns, &CONVERSATION_COLUMNS);
    let (Some(token_variant), Some(conversation_variant)) = (token_variant, conversation_variant)
    else {
        return Ok(LegacySchemaDetection::Unsupported);
    };

    let has_raw_records = relations.has_regular_table(RAW_RECORDS_TABLE);
    if has_raw_records {
        let raw_columns = load_columns(connection, LegacyTable::RawRecords)?;
        if raw_columns.is_empty() || !raw_columns.iter().all(ColumnShape::is_ordinary) {
            return Ok(LegacySchemaDetection::Unsupported);
        }
    }
    let contains_raw_schema = has_raw_records
        || token_variant == TableVariant::ContainsPrivateColumns
        || conversation_variant == TableVariant::ContainsPrivateColumns;
    let user_version = source_user_version(connection)?;

    Ok(match (user_version, contains_raw_schema) {
        (1, false) => LegacySchemaDetection::Legacy(LegacySchemaKind::StructuredV1),
        (0 | 1, true) => LegacySchemaDetection::Legacy(LegacySchemaKind::RawLegacy),
        _ => LegacySchemaDetection::Unsupported,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RelationKind {
    Table,
    View,
    Virtual,
    Shadow,
    Unknown,
}

struct SchemaRelation {
    name: String,
    kind: RelationKind,
    column_count: i64,
    without_rowid: bool,
    strict: bool,
}

impl SchemaRelation {
    fn is_legacy_rowid_table(&self) -> bool {
        self.kind == RelationKind::Table
            && !self.without_rowid
            && !self.strict
            && usize::try_from(self.column_count).is_ok_and(|count| count <= MAX_SOURCE_COLUMNS)
    }
}

struct SchemaInventory {
    relations: Vec<SchemaRelation>,
}

impl SchemaInventory {
    fn load(connection: &Connection) -> Result<Self, LegacySchemaDetectionError> {
        let limit = i64::try_from(MAX_SOURCE_RELATIONS + 1).expect("relation limit fits i64");
        let mut statement = connection
            .prepare(
                "SELECT name, type, ncol, wr, strict
                 FROM pragma_table_list
                 WHERE schema = 'main' AND name NOT GLOB 'sqlite_*'
                 LIMIT ?1",
            )
            .map_err(invalid_database)?;
        let rows = statement
            .query_map([limit], |row| {
                let kind = match row.get::<_, String>(1)?.as_str() {
                    "table" => RelationKind::Table,
                    "view" => RelationKind::View,
                    "virtual" => RelationKind::Virtual,
                    "shadow" => RelationKind::Shadow,
                    _ => RelationKind::Unknown,
                };
                Ok(SchemaRelation {
                    name: row.get(0)?,
                    kind,
                    column_count: row.get(2)?,
                    without_rowid: row.get(3)?,
                    strict: row.get(4)?,
                })
            })
            .map_err(invalid_database)?;
        let relations = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(invalid_database)?;
        if relations.len() > MAX_SOURCE_RELATIONS
            || relations
                .iter()
                .any(|relation| relation.name.len() > MAX_RELATION_NAME_BYTES)
        {
            return Err(LegacySchemaDetectionError::SchemaTooComplex);
        }
        Ok(Self { relations })
    }

    fn relation(&self, name: &str) -> Option<&SchemaRelation> {
        self.relations.iter().find(|relation| relation.name == name)
    }

    fn has_regular_table(&self, name: &str) -> bool {
        self.relation(name)
            .is_some_and(SchemaRelation::is_legacy_rowid_table)
    }

    fn has_current_sentinels(&self) -> bool {
        CURRENT_SCHEMA_SENTINELS.iter().all(|name| {
            self.relation(name)
                .is_some_and(|relation| relation.kind == RelationKind::Table)
        }) && ![TOKEN_EVENTS_TABLE, CONVERSATIONS_TABLE, RAW_RECORDS_TABLE]
            .iter()
            .any(|name| self.relation(name).is_some())
    }

    fn has_executable_relations(&self) -> bool {
        self.relations
            .iter()
            .any(|relation| relation.kind != RelationKind::Table)
    }

    fn has_only_known_legacy_tables(&self) -> bool {
        self.has_regular_table(TOKEN_EVENTS_TABLE)
            && self.has_regular_table(CONVERSATIONS_TABLE)
            && self.relations.iter().all(|relation| {
                matches!(
                    relation.name.as_str(),
                    TOKEN_EVENTS_TABLE | CONVERSATIONS_TABLE | RAW_RECORDS_TABLE
                ) && relation.is_legacy_rowid_table()
            })
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum SchemaObjectKind {
    Table,
    Index,
    View,
    Trigger,
    Unknown,
}

struct SchemaObject {
    name: String,
    table_name: String,
    kind: SchemaObjectKind,
    root_page: i64,
}

struct SchemaObjectInventory {
    objects: Vec<SchemaObject>,
}

impl SchemaObjectInventory {
    fn load(connection: &Connection) -> Result<Self, LegacySchemaDetectionError> {
        let limit = i64::try_from(MAX_SCHEMA_OBJECTS + 1).expect("object limit fits i64");
        let mut statement = connection
            .prepare(
                "SELECT type, name, tbl_name, rootpage
                 FROM sqlite_schema
                 WHERE name NOT GLOB 'sqlite_*'
                 LIMIT ?1",
            )
            .map_err(invalid_database)?;
        let rows = statement
            .query_map([limit], |row| {
                let kind = match row.get::<_, String>(0)?.as_str() {
                    "table" => SchemaObjectKind::Table,
                    "index" => SchemaObjectKind::Index,
                    "view" => SchemaObjectKind::View,
                    "trigger" => SchemaObjectKind::Trigger,
                    _ => SchemaObjectKind::Unknown,
                };
                Ok(SchemaObject {
                    name: row.get(1)?,
                    table_name: row.get(2)?,
                    kind,
                    root_page: row.get(3)?,
                })
            })
            .map_err(invalid_database)?;
        let objects = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(invalid_database)?;
        if objects.len() > MAX_SCHEMA_OBJECTS
            || objects.iter().any(|object| {
                object.name.len() > MAX_RELATION_NAME_BYTES
                    || object.table_name.len() > MAX_RELATION_NAME_BYTES
            })
        {
            return Err(LegacySchemaDetectionError::SchemaTooComplex);
        }
        Ok(Self { objects })
    }

    fn has_preflight_executable_relations(&self) -> bool {
        self.objects.iter().any(|object| match object.kind {
            SchemaObjectKind::Table => object.root_page == 0,
            SchemaObjectKind::View | SchemaObjectKind::Unknown => true,
            SchemaObjectKind::Index | SchemaObjectKind::Trigger => false,
        })
    }

    fn has_executable_objects(&self) -> bool {
        self.objects.iter().any(|object| {
            matches!(
                object.kind,
                SchemaObjectKind::View | SchemaObjectKind::Trigger | SchemaObjectKind::Unknown
            )
        })
    }

    fn has_only_known_legacy_objects(&self) -> bool {
        self.objects.iter().all(|object| match object.kind {
            SchemaObjectKind::Table => {
                matches!(
                    object.name.as_str(),
                    TOKEN_EVENTS_TABLE | CONVERSATIONS_TABLE | RAW_RECORDS_TABLE
                ) && object.table_name == object.name
            }
            SchemaObjectKind::Index => LegacyIndex::from_name(&object.name)
                .is_some_and(|index| index.table_name() == object.table_name),
            SchemaObjectKind::View | SchemaObjectKind::Trigger | SchemaObjectKind::Unknown => false,
        })
    }
}

#[derive(Clone, Copy)]
enum LegacyTable {
    TokenEvents,
    Conversations,
    RawRecords,
}

impl LegacyTable {
    const fn columns_sql(self) -> &'static str {
        match self {
            Self::TokenEvents => {
                "SELECT name, type, \"notnull\", pk, hidden,
                        dflt_value IS NOT NULL
                 FROM pragma_table_xinfo('codex_token_events')
                 ORDER BY cid LIMIT 33"
            }
            Self::Conversations => {
                "SELECT name, type, \"notnull\", pk, hidden,
                        dflt_value IS NOT NULL
                 FROM pragma_table_xinfo('codex_conversations')
                 ORDER BY cid LIMIT 33"
            }
            Self::RawRecords => {
                "SELECT name, type, \"notnull\", pk, hidden,
                        dflt_value IS NOT NULL
                 FROM pragma_table_xinfo('otel_log_records')
                 ORDER BY cid LIMIT 33"
            }
        }
    }
}

struct ColumnShape {
    name: String,
    declared_type: String,
    not_null: bool,
    primary_key_position: i64,
    hidden: i64,
    has_default: bool,
}

impl ColumnShape {
    const fn is_ordinary(&self) -> bool {
        self.hidden == 0
    }

    fn matches(&self, expected: &ExpectedColumn) -> bool {
        self.name == expected.name
            && self
                .declared_type
                .trim()
                .eq_ignore_ascii_case(expected.declared_type)
            && self.not_null == expected.not_null
            && self.primary_key_position == expected.primary_key_position
            && self.is_ordinary()
            && !self.has_default
    }
}

fn load_columns(
    connection: &Connection,
    table: LegacyTable,
) -> Result<Vec<ColumnShape>, LegacySchemaDetectionError> {
    let mut statement = connection
        .prepare(table.columns_sql())
        .map_err(invalid_database)?;
    let rows = statement
        .query_map([], |row| {
            Ok(ColumnShape {
                name: row.get(0)?,
                declared_type: row.get(1)?,
                not_null: row.get(2)?,
                primary_key_position: row.get(3)?,
                hidden: row.get(4)?,
                has_default: row.get(5)?,
            })
        })
        .map_err(invalid_database)?;
    let columns = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(invalid_database)?;
    if columns.len() > MAX_SOURCE_COLUMNS
        || columns.iter().any(|column| {
            column.name.len() > MAX_COLUMN_NAME_BYTES
                || column.declared_type.len() > MAX_DECLARED_TYPE_BYTES
        })
    {
        return Err(LegacySchemaDetectionError::SchemaTooComplex);
    }
    Ok(columns)
}

#[derive(Clone, Copy)]
enum CurrentMetadataTable {
    SchemaMigrations,
    TelemetryFieldRegistry,
}

impl CurrentMetadataTable {
    const fn columns_sql(self) -> &'static str {
        match self {
            Self::SchemaMigrations => {
                "SELECT name, type, \"notnull\", pk, hidden,
                        dflt_value IS NOT NULL
                 FROM pragma_table_xinfo('schema_migrations')
                 ORDER BY cid LIMIT 7"
            }
            Self::TelemetryFieldRegistry => {
                "SELECT name, type, \"notnull\", pk, hidden,
                        dflt_value IS NOT NULL
                 FROM pragma_table_xinfo('telemetry_field_registry')
                 ORDER BY cid LIMIT 4"
            }
        }
    }
}

fn load_current_columns(
    connection: &Connection,
    table: CurrentMetadataTable,
) -> Result<Vec<ColumnShape>, LegacySchemaDetectionError> {
    let mut statement = connection
        .prepare(table.columns_sql())
        .map_err(invalid_database)?;
    let rows = statement
        .query_map([], |row| {
            Ok(ColumnShape {
                name: row.get(0)?,
                declared_type: row.get(1)?,
                not_null: row.get(2)?,
                primary_key_position: row.get(3)?,
                hidden: row.get(4)?,
                has_default: row.get(5)?,
            })
        })
        .map_err(invalid_database)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(invalid_database)
}

fn current_metadata_tables_match(
    connection: &Connection,
    relations: &SchemaInventory,
) -> Result<bool, LegacySchemaDetectionError> {
    let migrations_relation = relations.relation("schema_migrations");
    let registry_relation = relations.relation("telemetry_field_registry");
    if !migrations_relation.is_some_and(|relation| {
        relation.kind == RelationKind::Table
            && relation.column_count == 5
            && !relation.without_rowid
            && relation.strict
    }) || !registry_relation.is_some_and(|relation| {
        relation.kind == RelationKind::Table
            && relation.column_count == 2
            && relation.without_rowid
            && relation.strict
    }) {
        return Ok(false);
    }
    let migrations = load_current_columns(connection, CurrentMetadataTable::SchemaMigrations)?;
    let registry = load_current_columns(connection, CurrentMetadataTable::TelemetryFieldRegistry)?;
    Ok(
        columns_match_exactly(&migrations, &SCHEMA_MIGRATION_COLUMNS)
            && columns_match_exactly(&registry, &FIELD_REGISTRY_COLUMNS),
    )
}

#[derive(Clone, Copy)]
struct ExpectedColumn {
    name: &'static str,
    declared_type: &'static str,
    not_null: bool,
    primary_key_position: i64,
}

const fn column(
    name: &'static str,
    declared_type: &'static str,
    not_null: bool,
    primary_key_position: i64,
) -> ExpectedColumn {
    ExpectedColumn {
        name,
        declared_type,
        not_null,
        primary_key_position,
    }
}

const TOKEN_STRUCTURED_COLUMNS: [ExpectedColumn; 15] = [
    column("id", "INTEGER", false, 1),
    column("created_at", "TEXT", true, 0),
    column("received_at", "TEXT", true, 0),
    column("conversation_id", "TEXT", false, 0),
    column("thread_id", "TEXT", false, 0),
    column("model", "TEXT", false, 0),
    column("reasoning_effort", "TEXT", false, 0),
    column("input_tokens", "INTEGER", false, 0),
    column("cached_input_tokens", "INTEGER", false, 0),
    column("output_tokens", "INTEGER", false, 0),
    column("reasoning_output_tokens", "INTEGER", false, 0),
    column("raw_event_name", "TEXT", false, 0),
    column("stream_event_kind", "TEXT", false, 0),
    column("surface", "TEXT", false, 0),
    column("client", "TEXT", false, 0),
];

const CONVERSATION_COLUMNS: [ExpectedColumn; 8] = [
    column("conversation_id", "TEXT", false, 1),
    column("first_seen_at", "TEXT", true, 0),
    column("last_seen_at", "TEXT", true, 0),
    column("model", "TEXT", false, 0),
    column("reasoning_effort", "TEXT", false, 0),
    column("surface", "TEXT", false, 0),
    column("client", "TEXT", false, 0),
    column("raw_event_name", "TEXT", false, 0),
];

const SCHEMA_MIGRATION_COLUMNS: [ExpectedColumn; 5] = [
    column("version", "INTEGER", false, 1),
    column("name", "TEXT", true, 0),
    column("sha256", "TEXT", true, 0),
    column("sql_bytes", "INTEGER", true, 0),
    column("applied_at", "TEXT", true, 0),
];

const FIELD_REGISTRY_COLUMNS: [ExpectedColumn; 2] = [
    column("field_id", "TEXT", true, 1),
    column("persisted", "INTEGER", true, 0),
];

#[derive(Clone, Copy, Eq, PartialEq)]
enum TableVariant {
    Structured,
    ContainsPrivateColumns,
}

fn table_variant(actual: &[ColumnShape], expected: &[ExpectedColumn]) -> Option<TableVariant> {
    let all_expected_columns_match = expected.iter().all(|expected| {
        actual
            .iter()
            .find(|actual| actual.name == expected.name)
            .is_some_and(|actual| actual.matches(expected))
    });
    if actual.len() == expected.len()
        && actual.iter().all(ColumnShape::is_ordinary)
        && all_expected_columns_match
    {
        return Some(TableVariant::Structured);
    }
    if actual.len() > expected.len()
        && actual.iter().all(ColumnShape::is_ordinary)
        && all_expected_columns_match
    {
        return Some(TableVariant::ContainsPrivateColumns);
    }
    None
}

fn columns_match_exactly(actual: &[ColumnShape], expected: &[ExpectedColumn]) -> bool {
    actual.len() == expected.len()
        && actual
            .iter()
            .zip(expected)
            .all(|(actual, expected)| actual.matches(expected))
}

#[derive(Clone, Copy)]
enum LegacyIndex {
    TokenCreatedAt,
    TokenThreadId,
    TokenConversationId,
    ConversationModel,
    ConversationPrimaryKey,
}

impl LegacyIndex {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "idx_codex_token_events_created_at" => Some(Self::TokenCreatedAt),
            "idx_codex_token_events_thread_id" => Some(Self::TokenThreadId),
            "idx_codex_token_events_conversation_id" => Some(Self::TokenConversationId),
            "idx_codex_conversations_model" => Some(Self::ConversationModel),
            "sqlite_autoindex_codex_conversations_1" => Some(Self::ConversationPrimaryKey),
            _ => None,
        }
    }

    const fn table_name(self) -> &'static str {
        match self {
            Self::TokenCreatedAt | Self::TokenThreadId | Self::TokenConversationId => {
                TOKEN_EVENTS_TABLE
            }
            Self::ConversationModel | Self::ConversationPrimaryKey => CONVERSATIONS_TABLE,
        }
    }

    const fn column_name(self) -> &'static str {
        match self {
            Self::TokenCreatedAt => "created_at",
            Self::TokenThreadId => "thread_id",
            Self::TokenConversationId | Self::ConversationPrimaryKey => "conversation_id",
            Self::ConversationModel => "model",
        }
    }

    const fn columns_sql(self) -> &'static str {
        match self {
            Self::TokenCreatedAt => {
                "SELECT name, desc, coll
                 FROM pragma_index_xinfo('idx_codex_token_events_created_at')
                 WHERE key = 1 ORDER BY seqno LIMIT 2"
            }
            Self::TokenThreadId => {
                "SELECT name, desc, coll
                 FROM pragma_index_xinfo('idx_codex_token_events_thread_id')
                 WHERE key = 1 ORDER BY seqno LIMIT 2"
            }
            Self::TokenConversationId => {
                "SELECT name, desc, coll
                 FROM pragma_index_xinfo('idx_codex_token_events_conversation_id')
                 WHERE key = 1 ORDER BY seqno LIMIT 2"
            }
            Self::ConversationModel => {
                "SELECT name, desc, coll
                 FROM pragma_index_xinfo('idx_codex_conversations_model')
                 WHERE key = 1 ORDER BY seqno LIMIT 2"
            }
            Self::ConversationPrimaryKey => {
                "SELECT name, desc, coll
                 FROM pragma_index_xinfo('sqlite_autoindex_codex_conversations_1')
                 WHERE key = 1 ORDER BY seqno LIMIT 2"
            }
        }
    }

    const fn is_primary_key(self) -> bool {
        matches!(self, Self::ConversationPrimaryKey)
    }
}

struct IndexShape {
    name: String,
    unique: bool,
    origin: String,
    partial: bool,
}

fn legacy_indexes_match(connection: &Connection) -> Result<bool, LegacySchemaDetectionError> {
    let token_indexes = load_indexes(connection, LegacyTable::TokenEvents)?;
    let conversation_indexes = load_indexes(connection, LegacyTable::Conversations)?;
    let mut saw_conversation_primary_key = false;
    for index in token_indexes.iter().chain(&conversation_indexes) {
        let Some(expected) = LegacyIndex::from_name(&index.name) else {
            return Ok(false);
        };
        let expected_origin = if expected.is_primary_key() { "pk" } else { "c" };
        let expected_unique = expected.is_primary_key();
        if index.origin != expected_origin
            || index.unique != expected_unique
            || index.partial
            || !index_columns_match(connection, expected)?
        {
            return Ok(false);
        }
        saw_conversation_primary_key |= expected.is_primary_key();
    }
    Ok(saw_conversation_primary_key)
}

fn legacy_tables_have_foreign_keys(
    connection: &Connection,
) -> Result<bool, LegacySchemaDetectionError> {
    for sql in [
        "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_list('codex_token_events'))",
        "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_list('codex_conversations'))",
        "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_list('otel_log_records'))",
    ] {
        if connection
            .query_row(sql, [], |row| row.get(0))
            .map_err(invalid_database)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn load_indexes(
    connection: &Connection,
    table: LegacyTable,
) -> Result<Vec<IndexShape>, LegacySchemaDetectionError> {
    let sql = match table {
        LegacyTable::TokenEvents => {
            "SELECT name, \"unique\", origin, partial
             FROM pragma_index_list('codex_token_events') LIMIT 9"
        }
        LegacyTable::Conversations => {
            "SELECT name, \"unique\", origin, partial
             FROM pragma_index_list('codex_conversations') LIMIT 9"
        }
        LegacyTable::RawRecords => unreachable!("raw records indexes are never inspected"),
    };
    let mut statement = connection.prepare(sql).map_err(invalid_database)?;
    let rows = statement
        .query_map([], |row| {
            Ok(IndexShape {
                name: row.get(0)?,
                unique: row.get(1)?,
                origin: row.get(2)?,
                partial: row.get(3)?,
            })
        })
        .map_err(invalid_database)?;
    let indexes = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(invalid_database)?;
    if indexes.len() > MAX_SOURCE_INDEXES
        || indexes
            .iter()
            .any(|index| index.name.len() > MAX_RELATION_NAME_BYTES)
    {
        return Err(LegacySchemaDetectionError::SchemaTooComplex);
    }
    Ok(indexes)
}

fn index_columns_match(
    connection: &Connection,
    index: LegacyIndex,
) -> Result<bool, LegacySchemaDetectionError> {
    let mut statement = connection
        .prepare(index.columns_sql())
        .map_err(invalid_database)?;
    let columns = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(invalid_database)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(invalid_database)?;
    Ok(columns
        == [(
            Some(index.column_name().to_owned()),
            false,
            "BINARY".to_owned(),
        )])
}

fn source_user_version(connection: &Connection) -> Result<i64, LegacySchemaDetectionError> {
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(invalid_database)
}

fn source_application_id(connection: &Connection) -> Result<i64, LegacySchemaDetectionError> {
    connection
        .pragma_query_value(None, "application_id", |row| row.get(0))
        .map_err(invalid_database)
}

fn invalid_database(_error: rusqlite::Error) -> LegacySchemaDetectionError {
    LegacySchemaDetectionError::InvalidDatabase
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use codex_monitor_core::{DataOrigin, UtcTimestamp};
    use tempfile::tempdir;

    use super::super::{ModelCallQuery, StorageConfig, StorageError, StorageReader, StorageWriter};
    use super::*;

    const STRUCTURED_SCHEMA: &str = "
        CREATE TABLE codex_token_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            created_at TEXT NOT NULL,
            received_at TEXT NOT NULL,
            conversation_id TEXT,
            thread_id TEXT,
            model TEXT,
            reasoning_effort TEXT,
            input_tokens INTEGER,
            cached_input_tokens INTEGER,
            output_tokens INTEGER,
            reasoning_output_tokens INTEGER,
            raw_event_name TEXT,
            stream_event_kind TEXT,
            surface TEXT,
            client TEXT
        );
        CREATE TABLE codex_conversations (
            conversation_id TEXT PRIMARY KEY,
            first_seen_at TEXT NOT NULL,
            last_seen_at TEXT NOT NULL,
            model TEXT,
            reasoning_effort TEXT,
            surface TEXT,
            client TEXT,
            raw_event_name TEXT
        );
        CREATE INDEX idx_codex_token_events_created_at
            ON codex_token_events(created_at);
        CREATE INDEX idx_codex_token_events_thread_id
            ON codex_token_events(thread_id);
        CREATE INDEX idx_codex_token_events_conversation_id
            ON codex_token_events(conversation_id);
        CREATE INDEX idx_codex_conversations_model
            ON codex_conversations(model);
    ";

    const RAW_SCHEMA: &str = "
        CREATE TABLE otel_log_records (
            id INTEGER PRIMARY KEY,
            raw_json TEXT NOT NULL
        );
        CREATE TABLE codex_token_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            created_at TEXT NOT NULL,
            received_at TEXT NOT NULL,
            conversation_id TEXT,
            thread_id TEXT,
            model TEXT,
            reasoning_effort TEXT,
            input_tokens INTEGER,
            cached_input_tokens INTEGER,
            output_tokens INTEGER,
            reasoning_output_tokens INTEGER,
            raw_event_name TEXT,
            stream_event_kind TEXT,
            surface TEXT,
            client TEXT,
            raw_json TEXT NOT NULL
        );
        CREATE TABLE codex_conversations (
            conversation_id TEXT PRIMARY KEY,
            first_seen_at TEXT NOT NULL,
            last_seen_at TEXT NOT NULL,
            model TEXT,
            reasoning_effort TEXT,
            reasoning_summary TEXT,
            sandbox_policy TEXT,
            approval_policy TEXT,
            surface TEXT,
            client TEXT,
            raw_event_name TEXT,
            raw_json TEXT NOT NULL
        );
        INSERT INTO otel_log_records VALUES (1, 'synthetic-private-raw-record');
    ";

    #[test]
    fn structured_and_raw_python_schemas_are_detected_without_source_writes() {
        let directory = tempdir().unwrap();
        let structured = directory.path().join("structured.sqlite3");
        create_database(&structured, STRUCTURED_SCHEMA, 1);
        let structured_before = fs::read(&structured).unwrap();
        assert_eq!(
            detect_legacy_schema(&structured),
            Ok(LegacySchemaDetection::Legacy(
                LegacySchemaKind::StructuredV1
            ))
        );
        assert_eq!(fs::read(&structured).unwrap(), structured_before);

        let raw = directory.path().join("raw.sqlite3");
        create_database(&raw, RAW_SCHEMA, 0);
        let raw_before = fs::read(&raw).unwrap();
        assert_eq!(
            detect_legacy_schema(&raw),
            Ok(LegacySchemaDetection::Legacy(LegacySchemaKind::RawLegacy))
        );
        assert_eq!(fs::read(&raw).unwrap(), raw_before);
    }

    #[test]
    fn additional_ordinary_private_columns_are_classified_as_raw_and_never_read() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("private-column.sqlite3");
        let schema = STRUCTURED_SCHEMA.replacen(
            "            client TEXT\n        );",
            "            client TEXT,\n            private_extension BLOB DEFAULT X'00'\n        );",
            1,
        );
        assert_ne!(schema, STRUCTURED_SCHEMA);
        create_database(&database, &schema, 1);

        assert_eq!(
            detect_legacy_schema(&database),
            Ok(LegacySchemaDetection::Legacy(LegacySchemaKind::RawLegacy))
        );
    }

    #[test]
    fn reordered_allowlisted_columns_remain_structured() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("reordered.sqlite3");
        let schema = STRUCTURED_SCHEMA.replacen(
            "            created_at TEXT NOT NULL,\n            received_at TEXT NOT NULL,",
            "            received_at TEXT NOT NULL,\n            created_at TEXT NOT NULL,",
            1,
        );
        assert_ne!(schema, STRUCTURED_SCHEMA);
        create_database(&database, &schema, 1);

        assert_eq!(
            detect_legacy_schema(&database),
            Ok(LegacySchemaDetection::Legacy(
                LegacySchemaKind::StructuredV1
            ))
        );
    }

    #[tokio::test]
    async fn current_rust_database_requires_the_current_migration_ledger() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("current.sqlite3");
        let writer = StorageWriter::open(StorageConfig::new(database.clone())).unwrap();

        assert_eq!(
            detect_legacy_schema(&database),
            Ok(LegacySchemaDetection::CurrentDatabase)
        );

        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn import_projects_valid_fields_as_historical_and_is_idempotent() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("legacy.sqlite3");
        let target = directory.path().join("current.sqlite3");
        create_database(&source, STRUCTURED_SCHEMA, 1);
        execute_database(
            &source,
            "
                INSERT INTO codex_conversations (
                    conversation_id, first_seen_at, last_seen_at, model,
                    reasoning_effort, surface, client, raw_event_name
                ) VALUES (
                    'conversation-a', '2025-01-01T00:00:01Z',
                    '2025-01-01T00:00:09Z', 'gpt-legacy', 'high',
                    'desktop', 'codex-cli', 'codex.conversation_starts'
                );
                INSERT INTO codex_token_events (
                    created_at, received_at, conversation_id, thread_id, model,
                    reasoning_effort, input_tokens, cached_input_tokens,
                    output_tokens, reasoning_output_tokens, raw_event_name,
                    stream_event_kind, surface, client
                ) VALUES (
                    '2025-01-01T00:00:10Z', '2025-01-01T00:00:11Z',
                    'conversation-a', 'thread-a', 'gpt-legacy', 'high (config)',
                    1000000000000001, 12.5, 'invalid', 516, 'other.event',
                    'response.completed', 'desktop', 'codex-cli'
                );
                INSERT INTO codex_token_events (
                    created_at, received_at, conversation_id, input_tokens,
                    cached_input_tokens, output_tokens, reasoning_output_tokens
                ) VALUES (
                    '2025-01-01T00:00:12Z', '2025-01-01T00:00:13Z',
                    'conversation-a', NULL, -1, 1.5, 'invalid'
                );
            ",
        );
        let source_before = fs::read(&source).unwrap();
        let imported_at = timestamp("2026-01-01T00:00:00Z");
        let writer = StorageWriter::open(StorageConfig::new(target.clone())).unwrap();

        let first = writer
            .import_legacy_database(source.clone(), imported_at)
            .await
            .unwrap();
        assert_eq!(first.status(), LegacyImportStatus::Imported);
        assert_eq!(first.schema(), LegacySchemaKind::StructuredV1);
        assert_eq!(first.imported_model_calls(), 1);
        assert_eq!(first.imported_conversations(), 1);

        let repeated = writer
            .import_legacy_database(source.clone(), timestamp("2026-01-02T00:00:00Z"))
            .await
            .unwrap();
        assert_eq!(repeated.status(), LegacyImportStatus::AlreadyImported);
        assert_eq!(repeated.imported_model_calls(), 1);
        assert_eq!(repeated.imported_conversations(), 1);

        assert_historical_projection(&target);

        writer.shutdown().await.unwrap();
        assert_eq!(fs::read(&source).unwrap(), source_before);
    }

    #[tokio::test]
    async fn conversation_only_source_imports_metadata_without_fabricating_a_call() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("conversation-only.sqlite3");
        let target = directory.path().join("current.sqlite3");
        create_database(&source, STRUCTURED_SCHEMA, 1);
        execute_database(
            &source,
            "
                INSERT INTO codex_conversations (
                    conversation_id, first_seen_at, last_seen_at
                ) VALUES (
                    'conversation-only', '2025-01-02T00:00:01Z',
                    '2025-01-02T00:00:09Z'
                );
            ",
        );
        let writer = StorageWriter::open(StorageConfig::new(target.clone())).unwrap();

        let outcome = writer
            .import_legacy_database(source, timestamp("2026-01-02T00:00:00Z"))
            .await
            .unwrap();
        assert_eq!(outcome.imported_model_calls(), 0);
        assert_eq!(outcome.imported_conversations(), 1);

        let connection = Connection::open(&target).unwrap();
        assert_eq!(scalar(&connection, "SELECT count(*) FROM conversations"), 1);
        assert_eq!(scalar(&connection, "SELECT count(*) FROM model_calls"), 0);
        assert_eq!(
            scalar(&connection, "SELECT count(*) FROM model_call_observations"),
            0
        );
        let range = connection
            .query_row(
                "SELECT first_observed_at, last_observed_at FROM conversations",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .unwrap();
        assert_eq!(
            range,
            (
                "2025-01-02T00:00:01.000000000Z".to_owned(),
                "2025-01-02T00:00:09.000000000Z".to_owned()
            )
        );
        drop(connection);
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn inverted_conversation_range_fails_closed_without_target_rows() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("inverted-range.sqlite3");
        let target = directory.path().join("current.sqlite3");
        create_database(&source, STRUCTURED_SCHEMA, 1);
        execute_database(
            &source,
            "
                INSERT INTO codex_conversations (
                    conversation_id, first_seen_at, last_seen_at
                ) VALUES (
                    'conversation-inverted', '2025-01-03T00:00:09Z',
                    '2025-01-03T00:00:01Z'
                );
            ",
        );
        let writer = StorageWriter::open(StorageConfig::new(target.clone())).unwrap();

        let error = writer
            .import_legacy_database(source, timestamp("2026-01-03T00:00:00Z"))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            StorageError::LegacyConversationTimeRangeInvalid
        ));

        let connection = Connection::open(&target).unwrap();
        assert_eq!(scalar(&connection, "SELECT count(*) FROM conversations"), 0);
        assert_eq!(
            scalar(&connection, "SELECT count(*) FROM legacy_import_audit"),
            0
        );
        drop(connection);
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn raw_only_content_never_enters_target_or_changes_import_identity() {
        const FIRST_PRIVATE: &str = "SYNTHETIC_FORBIDDEN_PROMPT_ALPHA";
        const SECOND_PRIVATE: &str = "SYNTHETIC_FORBIDDEN_PROMPT_BETA";

        let directory = tempdir().unwrap();
        let source = directory.path().join("raw.sqlite3");
        let target = directory.path().join("current.sqlite3");
        create_database(&source, RAW_SCHEMA, 0);
        execute_database(
            &source,
            &format!(
                "
                    INSERT INTO codex_conversations (
                        conversation_id, first_seen_at, last_seen_at, model,
                        reasoning_effort, surface, client, raw_event_name,
                        reasoning_summary, sandbox_policy, approval_policy, raw_json
                    ) VALUES (
                        'conversation-raw', '2025-02-01T00:00:01Z',
                        '2025-02-01T00:00:02Z', 'gpt-legacy', 'medium',
                        'desktop', 'codex-cli', 'codex.conversation_starts',
                        '{FIRST_PRIVATE}', '{FIRST_PRIVATE}', '{FIRST_PRIVATE}',
                        '{FIRST_PRIVATE}'
                    );
                    INSERT INTO codex_token_events (
                        created_at, received_at, conversation_id, model,
                        input_tokens, raw_event_name, stream_event_kind, surface,
                        client, raw_json
                    ) VALUES (
                        '2025-02-01T00:00:03Z', '2025-02-01T00:00:04Z',
                        'conversation-raw', 'gpt-legacy', 42,
                        'codex.api_call', 'response.completed', 'desktop',
                        'codex-cli', '{FIRST_PRIVATE}'
                    );
                    UPDATE otel_log_records SET raw_json = '{FIRST_PRIVATE}';
                "
            ),
        );
        let before_first_import = fs::read(&source).unwrap();
        let writer = StorageWriter::open(StorageConfig::new(target.clone())).unwrap();

        let first = writer
            .import_legacy_database(source.clone(), timestamp("2026-02-01T00:00:00Z"))
            .await
            .unwrap();
        assert_eq!(first.status(), LegacyImportStatus::Imported);
        assert_eq!(first.schema(), LegacySchemaKind::RawLegacy);
        assert_eq!(fs::read(&source).unwrap(), before_first_import);

        execute_database(
            &source,
            &format!(
                "
                    UPDATE codex_token_events SET raw_json = '{SECOND_PRIVATE}';
                    UPDATE codex_conversations SET
                        reasoning_summary = '{SECOND_PRIVATE}',
                        sandbox_policy = '{SECOND_PRIVATE}',
                        approval_policy = '{SECOND_PRIVATE}',
                        raw_json = '{SECOND_PRIVATE}';
                    UPDATE otel_log_records SET raw_json = '{SECOND_PRIVATE}';
                "
            ),
        );
        let before_second_import = fs::read(&source).unwrap();
        let repeated = writer
            .import_legacy_database(source.clone(), timestamp("2026-02-02T00:00:00Z"))
            .await
            .unwrap();
        assert_eq!(repeated.status(), LegacyImportStatus::AlreadyImported);
        assert_eq!(fs::read(&source).unwrap(), before_second_import);

        writer.shutdown().await.unwrap();
        let connection = Connection::open(&target).unwrap();
        assert_eq!(scalar(&connection, "SELECT count(*) FROM model_calls"), 1);
        assert_eq!(
            scalar(&connection, "SELECT count(*) FROM legacy_import_audit"),
            1
        );
        drop(connection);
        assert_file_family_omits(&target, FIRST_PRIVATE.as_bytes());
        assert_file_family_omits(&target, SECOND_PRIVATE.as_bytes());
    }

    #[tokio::test]
    async fn appended_snapshot_imports_only_new_rows_without_touching_old_conversations() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("append.sqlite3");
        let target = directory.path().join("current.sqlite3");
        create_database(&source, STRUCTURED_SCHEMA, 1);
        execute_database(
            &source,
            "
                INSERT INTO codex_conversations (
                    conversation_id, first_seen_at, last_seen_at
                ) VALUES (
                    'conversation-a', '2025-03-01T00:00:01Z',
                    '2025-03-01T00:00:02Z'
                );
                INSERT INTO codex_token_events (
                    created_at, received_at, conversation_id, input_tokens
                ) VALUES (
                    '2025-03-01T00:00:03Z', '2025-03-01T00:00:04Z',
                    'conversation-a', 10
                );
            ",
        );
        let first_imported_at = timestamp("2026-03-01T00:00:00Z");
        let writer = StorageWriter::open(StorageConfig::new(target.clone())).unwrap();
        let first = writer
            .import_legacy_database(source.clone(), first_imported_at)
            .await
            .unwrap();
        assert_eq!(first.imported_model_calls(), 1);

        execute_database(
            &source,
            "
                INSERT INTO codex_token_events (
                    created_at, received_at, conversation_id, input_tokens
                ) VALUES (
                    '2025-03-02T00:00:03Z', '2025-03-02T00:00:04Z',
                    'conversation-b', 20
                );
            ",
        );
        let second = writer
            .import_legacy_database(source.clone(), timestamp("2026-03-02T00:00:00Z"))
            .await
            .unwrap();
        assert_eq!(second.status(), LegacyImportStatus::Imported);
        assert_eq!(second.imported_model_calls(), 1);
        assert_eq!(second.imported_conversations(), 0);

        let connection = Connection::open(&target).unwrap();
        assert_eq!(scalar(&connection, "SELECT count(*) FROM model_calls"), 2);
        assert_eq!(
            scalar(&connection, "SELECT count(*) FROM legacy_import_audit"),
            2
        );
        let conversation_a_last = connection
            .query_row(
                "SELECT conversations.last_observed_at
                 FROM conversations
                 JOIN conversation_source_keys
                   ON conversation_source_keys.conversation_id = conversations.id
                 WHERE conversation_source_keys.telemetry_source_id = 'legacy-import'
                   AND conversation_source_keys.key_kind = 'conversation'
                   AND conversation_source_keys.key_value = 'conversation-a'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert_eq!(
            conversation_a_last,
            super::super::timestamp::timestamp_text(first_imported_at)
        );
        drop(connection);

        let repeated = writer
            .import_legacy_database(source, timestamp("2026-03-03T00:00:00Z"))
            .await
            .unwrap();
        assert_eq!(repeated.status(), LegacyImportStatus::AlreadyImported);
        writer.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn conversation_collision_rolls_back_the_entire_import() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("collision.sqlite3");
        let target = directory.path().join("current.sqlite3");
        create_database(&source, STRUCTURED_SCHEMA, 1);
        execute_database(
            &source,
            "
                INSERT INTO codex_conversations (
                    conversation_id, first_seen_at, last_seen_at
                ) VALUES
                    ('conversation-a', '2025-04-01T00:00:01Z', '2025-04-01T00:00:02Z'),
                    ('conversation-b', '2025-04-01T00:00:01Z', '2025-04-01T00:00:02Z');
                INSERT INTO codex_token_events (
                    created_at, received_at, conversation_id, thread_id, input_tokens
                ) VALUES
                    ('2025-04-01T00:00:03Z', '2025-04-01T00:00:04Z',
                     'conversation-a', 'shared-thread', 10),
                    ('2025-04-01T00:00:05Z', '2025-04-01T00:00:06Z',
                     'conversation-b', 'shared-thread', 20);
            ",
        );
        let source_before = fs::read(&source).unwrap();
        let writer = StorageWriter::open(StorageConfig::new(target.clone())).unwrap();

        let error = writer
            .import_legacy_database(source.clone(), timestamp("2026-04-01T00:00:00Z"))
            .await
            .unwrap_err();
        assert!(matches!(error, StorageError::ConversationIdentityCollision));

        let connection = Connection::open(&target).unwrap();
        assert_eq!(scalar(&connection, "SELECT count(*) FROM model_calls"), 0);
        assert_eq!(scalar(&connection, "SELECT count(*) FROM conversations"), 0);
        assert_eq!(
            scalar(&connection, "SELECT count(*) FROM legacy_import_audit"),
            0
        );
        drop(connection);
        writer.shutdown().await.unwrap();
        assert_eq!(fs::read(&source).unwrap(), source_before);
    }

    #[test]
    fn missing_columns_unknown_objects_and_wrong_versions_are_unsupported() {
        let directory = tempdir().unwrap();
        let missing_column = directory.path().join("missing-column.sqlite3");
        let schema = STRUCTURED_SCHEMA.replacen("            input_tokens INTEGER,\n", "", 1);
        assert_ne!(schema, STRUCTURED_SCHEMA);
        create_database(&missing_column, &schema, 1);
        assert_eq!(
            detect_legacy_schema(&missing_column),
            Ok(LegacySchemaDetection::Unsupported)
        );

        let unknown_table = directory.path().join("unknown-table.sqlite3");
        create_database(
            &unknown_table,
            &format!("{STRUCTURED_SCHEMA} CREATE TABLE unexpected (id INTEGER);"),
            1,
        );
        assert_eq!(
            detect_legacy_schema(&unknown_table),
            Ok(LegacySchemaDetection::Unsupported)
        );

        let unknown_index = directory.path().join("unknown-index.sqlite3");
        create_database(
            &unknown_index,
            &format!(
                "{STRUCTURED_SCHEMA} CREATE INDEX unexpected_index ON codex_token_events(model);"
            ),
            1,
        );
        assert_eq!(
            detect_legacy_schema(&unknown_index),
            Ok(LegacySchemaDetection::Unsupported)
        );

        let wrong_version = directory.path().join("wrong-version.sqlite3");
        create_database(&wrong_version, STRUCTURED_SCHEMA, 2);
        assert_eq!(
            detect_legacy_schema(&wrong_version),
            Ok(LegacySchemaDetection::Unsupported)
        );
    }

    #[test]
    fn executable_and_generated_schema_objects_are_rejected_or_unsupported() {
        let directory = tempdir().unwrap();
        let trigger_database = directory.path().join("trigger.sqlite3");
        let trigger_sql = format!(
            "{RAW_SCHEMA}
             CREATE TRIGGER source_mutation
             AFTER UPDATE ON codex_token_events
             BEGIN
                 DELETE FROM otel_log_records;
             END;"
        );
        create_database(&trigger_database, &trigger_sql, 0);
        let before = fs::read(&trigger_database).unwrap();
        assert_eq!(
            detect_legacy_schema(&trigger_database),
            Err(LegacySchemaDetectionError::ExecutableSchema)
        );
        assert_eq!(fs::read(&trigger_database).unwrap(), before);

        let view_database = directory.path().join("view.sqlite3");
        create_database(
            &view_database,
            &format!(
                "{STRUCTURED_SCHEMA} CREATE VIEW private_view AS SELECT * FROM codex_token_events;"
            ),
            1,
        );
        assert_eq!(
            detect_legacy_schema(&view_database),
            Err(LegacySchemaDetectionError::ExecutableSchema)
        );

        let generated_database = directory.path().join("generated.sqlite3");
        let generated_schema = STRUCTURED_SCHEMA.replacen(
            "            client TEXT\n        );",
            "            client TEXT,\n            generated_private TEXT AS (model)\n        );",
            1,
        );
        create_database(&generated_database, &generated_schema, 1);
        assert_eq!(
            detect_legacy_schema(&generated_database),
            Ok(LegacySchemaDetection::Unsupported)
        );

        let virtual_database = directory.path().join("virtual.sqlite3");
        create_database(
            &virtual_database,
            &format!("{STRUCTURED_SCHEMA} CREATE VIRTUAL TABLE private_fts USING fts5(secret);"),
            1,
        );
        assert_eq!(
            detect_legacy_schema(&virtual_database),
            Err(LegacySchemaDetectionError::ExecutableSchema)
        );

        let generated_current = directory.path().join("generated-current.sqlite3");
        create_database(
            &generated_current,
            "
                CREATE TABLE schema_migrations (
                    version INTEGER PRIMARY KEY,
                    name TEXT GENERATED ALWAYS AS (substr(sha256, 1, 8)) VIRTUAL,
                    sha256 TEXT NOT NULL,
                    sql_bytes INTEGER NOT NULL,
                    applied_at TEXT NOT NULL
                ) STRICT;
                CREATE TABLE telemetry_field_registry (
                    field_id TEXT PRIMARY KEY,
                    persisted INTEGER NOT NULL
                ) STRICT, WITHOUT ROWID;
                CREATE TABLE model_calls (id TEXT PRIMARY KEY) STRICT;
                CREATE TABLE model_call_observations (id TEXT PRIMARY KEY) STRICT;
                CREATE TABLE legacy_import_audit (id TEXT PRIMARY KEY) STRICT;
            ",
            0,
        );
        assert_eq!(
            detect_legacy_schema(&generated_current),
            Ok(LegacySchemaDetection::Unsupported)
        );
    }

    #[test]
    fn malformed_unsafe_and_oversized_sources_fail_with_closed_categories() {
        let directory = tempdir().unwrap();
        let malformed = directory.path().join("malformed.sqlite3");
        fs::write(&malformed, b"not a SQLite database").unwrap();
        assert_eq!(
            detect_legacy_schema(&malformed),
            Err(LegacySchemaDetectionError::InvalidDatabase)
        );
        assert_eq!(
            detect_legacy_schema(&directory.path().join("missing.sqlite3")),
            Err(LegacySchemaDetectionError::SourceUnavailable)
        );
        assert_eq!(
            detect_legacy_schema(Path::new("relative.sqlite3")),
            Err(LegacySchemaDetectionError::SourceUnavailable)
        );

        let oversized = directory.path().join("oversized.sqlite3");
        let connection = Connection::open(&oversized).unwrap();
        for index in 0..=MAX_SOURCE_RELATIONS {
            connection
                .execute(&format!("CREATE TABLE table_{index} (id INTEGER)"), [])
                .unwrap();
        }
        drop(connection);
        assert_eq!(
            detect_legacy_schema(&oversized),
            Err(LegacySchemaDetectionError::SchemaTooComplex)
        );
    }

    fn create_database(path: &PathBuf, schema: &str, user_version: i64) {
        let connection = Connection::open(path).unwrap();
        connection.execute_batch(schema).unwrap();
        connection
            .pragma_update(None, "user_version", user_version)
            .unwrap();
    }

    fn execute_database(path: &Path, sql: &str) {
        Connection::open(path).unwrap().execute_batch(sql).unwrap();
    }

    fn scalar(connection: &Connection, sql: &str) -> i64 {
        connection
            .query_row(sql, [], |row| row.get::<_, i64>(0))
            .unwrap()
    }

    fn assert_historical_projection(target: &Path) {
        let calls = StorageReader::new(target.to_path_buf())
            .model_calls(ModelCallQuery::default())
            .unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0.origin(), DataOrigin::Historical);
        assert_eq!(
            calls[0]
                .0
                .fields()
                .reasoning_output_tokens
                .as_ref()
                .map(|value| value.value().get()),
            Some(516)
        );

        let connection = Connection::open(target).unwrap();
        assert_eq!(scalar(&connection, "SELECT count(*) FROM model_calls"), 1);
        assert_eq!(
            scalar(
                &connection,
                "SELECT count(*) FROM model_call_observations
                 WHERE origin = 'historical' AND source_id = 'legacy-import'"
            ),
            1
        );
        for table in ["domain_work", "rule_evaluations", "alerts", "action_outbox"] {
            assert_eq!(
                scalar(&connection, &format!("SELECT count(*) FROM {table}")),
                0
            );
        }
        assert_eq!(
            scalar(&connection, "SELECT count(*) FROM legacy_import_audit"),
            1
        );
        let effort = connection
            .query_row(
                "SELECT text_value, quality FROM model_call_observation_fields
                 WHERE field_id = 'model_call.reasoning_effort'",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .unwrap();
        assert_eq!(effort, ("high".to_owned(), "inferred".to_owned()));
        let reasoning_tokens = connection
            .query_row(
                "SELECT integer_value, quality FROM model_call_observation_fields
                 WHERE field_id = 'model_call.reasoning_output_tokens'",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .unwrap();
        assert_eq!(reasoning_tokens, (516, "direct".to_owned()));
        assert_eq!(
            scalar(
                &connection,
                "SELECT count(*) FROM model_call_observation_fields
                 WHERE field_id IN (
                    'model_call.input_tokens', 'model_call.cached_input_tokens',
                    'model_call.output_tokens', 'model_call.event_name'
                 )"
            ),
            0
        );
        let first_observed_at = connection
            .query_row(
                "SELECT first_observed_at FROM conversations LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert_eq!(first_observed_at, "2025-01-01T00:00:01.000000000Z");
    }

    fn timestamp(value: &str) -> UtcTimestamp {
        UtcTimestamp::parse_rfc3339(value).unwrap()
    }

    fn assert_file_family_omits(database: &Path, marker: &[u8]) {
        let file_name = database.file_name().unwrap().to_string_lossy();
        for entry in fs::read_dir(database.parent().unwrap()).unwrap() {
            let entry = entry.unwrap();
            if !entry
                .file_name()
                .to_string_lossy()
                .starts_with(file_name.as_ref())
            {
                continue;
            }
            let bytes = fs::read(entry.path()).unwrap();
            assert!(
                !bytes.windows(marker.len()).any(|window| window == marker),
                "target database family contains a forbidden marker"
            );
        }
    }
}
