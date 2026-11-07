-- CodexMonitor storage schema v1.
--
-- Columns used only for SQLite coordination (migration checksums, projection
-- references, work state, and foreign keys) are infrastructure metadata. They
-- do not authorize additional telemetry fields.

CREATE TABLE schema_migrations (
    version INTEGER PRIMARY KEY CHECK (version > 0),
    name TEXT NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 128),
    sha256 TEXT NOT NULL CHECK (length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'),
    sql_bytes INTEGER NOT NULL CHECK (sql_bytes > 0),
    applied_at TEXT NOT NULL
) STRICT;

CREATE TABLE telemetry_field_registry (
    field_id TEXT PRIMARY KEY,
    persisted INTEGER NOT NULL CHECK (persisted IN (0, 1))
) STRICT, WITHOUT ROWID;

INSERT INTO telemetry_field_registry (field_id, persisted) VALUES
    ('conversation.first_observed_at', 1),
    ('conversation.id', 1),
    ('conversation.last_observed_at', 1),
    ('conversation.resolved_display_name', 0),
    ('conversation.source_id', 1),
    ('conversation.thread_id', 1),
    ('model_call.cached_input_tokens', 1),
    ('model_call.canonical_identity', 1),
    ('model_call.client', 1),
    ('model_call.event_at', 1),
    ('model_call.event_name', 1),
    ('model_call.id', 1),
    ('model_call.input_tokens', 1),
    ('model_call.model', 1),
    ('model_call.output_tokens', 1),
    ('model_call.reasoning_effort', 1),
    ('model_call.reasoning_output_tokens', 1),
    ('model_call.stream_event_kind', 1),
    ('model_call.surface', 1),
    ('model_call.temporal_class', 1),
    ('model_call_observation.field_provenance', 1),
    ('model_call_observation.field_quality', 1),
    ('model_call_observation.id', 1),
    ('model_call_observation.observed_at', 1),
    ('model_call_observation.source_contract_version', 1),
    ('model_call_observation.source_event_identity', 1),
    ('model_call_observation.source_event_identity_version', 1),
    ('model_call_observation.source_id', 1),
    ('source_health.last_accepted_at', 1),
    ('source_health.last_error_code', 1),
    ('source_health.last_error_occurred_at', 1),
    ('source_health.source_contract_version', 1),
    ('source_health.source_id', 1),
    ('source_health.status', 1),
    ('source_health.supported_fields', 1),
    ('source_health.updated_at', 1),
    ('turn.event_at', 1),
    ('turn.id', 1),
    ('turn.source_id', 1);

CREATE TABLE conversations (
    id TEXT PRIMARY KEY,
    source_id TEXT,
    source_provenance_source_id TEXT,
    source_provenance_contract_version INTEGER,
    source_provenance_observed_at TEXT,
    source_quality TEXT CHECK (source_quality IN ('inferred', 'direct')),
    source_priority INTEGER CHECK (source_priority BETWEEN 0 AND 65535),
    thread_id TEXT,
    thread_provenance_source_id TEXT,
    thread_provenance_contract_version INTEGER,
    thread_provenance_observed_at TEXT,
    thread_quality TEXT CHECK (thread_quality IN ('inferred', 'direct')),
    thread_priority INTEGER CHECK (thread_priority BETWEEN 0 AND 65535),
    first_observed_at TEXT NOT NULL,
    last_observed_at TEXT NOT NULL,
    CHECK (first_observed_at <= last_observed_at),
    CHECK (
        (source_id IS NULL
            AND source_provenance_source_id IS NULL
            AND source_provenance_contract_version IS NULL
            AND source_provenance_observed_at IS NULL
            AND source_quality IS NULL
            AND source_priority IS NULL)
        OR
        (source_id IS NOT NULL
            AND source_provenance_source_id IS NOT NULL
            AND source_provenance_contract_version > 0
            AND source_provenance_observed_at IS NOT NULL
            AND source_quality IS NOT NULL
            AND source_priority IS NOT NULL)
    ),
    CHECK (
        (thread_id IS NULL
            AND thread_provenance_source_id IS NULL
            AND thread_provenance_contract_version IS NULL
            AND thread_provenance_observed_at IS NULL
            AND thread_quality IS NULL
            AND thread_priority IS NULL)
        OR
        (thread_id IS NOT NULL
            AND thread_provenance_source_id IS NOT NULL
            AND thread_provenance_contract_version > 0
            AND thread_provenance_observed_at IS NOT NULL
            AND thread_quality IS NOT NULL
            AND thread_priority IS NOT NULL)
    )
) STRICT;

-- Stable source-local keys are kept separately from the precedence-selected
-- conversation projection. This prevents a higher-priority future source from
-- erasing the lookup needed to correlate later OTLP calls.
CREATE TABLE conversation_source_keys (
    telemetry_source_id TEXT NOT NULL,
    source_contract_version INTEGER NOT NULL CHECK (source_contract_version > 0),
    key_kind TEXT NOT NULL CHECK (key_kind IN ('conversation', 'thread')),
    key_value TEXT NOT NULL CHECK (length(key_value) BETWEEN 1 AND 512),
    conversation_id TEXT NOT NULL,
    first_observed_at TEXT NOT NULL,
    last_observed_at TEXT NOT NULL,
    PRIMARY KEY (telemetry_source_id, source_contract_version, key_kind, key_value),
    FOREIGN KEY (conversation_id) REFERENCES conversations (id) ON DELETE CASCADE,
    CHECK (first_observed_at <= last_observed_at)
) STRICT, WITHOUT ROWID;

CREATE TABLE turns (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    source_id TEXT,
    source_provenance_source_id TEXT,
    source_provenance_contract_version INTEGER,
    source_provenance_observed_at TEXT,
    source_quality TEXT CHECK (source_quality IN ('inferred', 'direct')),
    source_priority INTEGER CHECK (source_priority BETWEEN 0 AND 65535),
    event_at TEXT,
    event_provenance_source_id TEXT,
    event_provenance_contract_version INTEGER,
    event_provenance_observed_at TEXT,
    event_quality TEXT CHECK (event_quality IN ('inferred', 'direct')),
    event_priority INTEGER CHECK (event_priority BETWEEN 0 AND 65535),
    UNIQUE (id, conversation_id),
    FOREIGN KEY (conversation_id) REFERENCES conversations (id) ON DELETE CASCADE,
    CHECK (
        (source_id IS NULL
            AND source_provenance_source_id IS NULL
            AND source_provenance_contract_version IS NULL
            AND source_provenance_observed_at IS NULL
            AND source_quality IS NULL
            AND source_priority IS NULL)
        OR
        (source_id IS NOT NULL
            AND source_provenance_source_id IS NOT NULL
            AND source_provenance_contract_version > 0
            AND source_provenance_observed_at IS NOT NULL
            AND source_quality IS NOT NULL
            AND source_priority IS NOT NULL)
    ),
    CHECK (
        (event_at IS NULL
            AND event_provenance_source_id IS NULL
            AND event_provenance_contract_version IS NULL
            AND event_provenance_observed_at IS NULL
            AND event_quality IS NULL
            AND event_priority IS NULL)
        OR
        (event_at IS NOT NULL
            AND event_provenance_source_id IS NOT NULL
            AND event_provenance_contract_version > 0
            AND event_provenance_observed_at IS NOT NULL
            AND event_quality IS NOT NULL
            AND event_priority IS NOT NULL)
    )
) STRICT;

CREATE TABLE model_calls (
    id TEXT PRIMARY KEY,
    canonical_source_id TEXT NOT NULL,
    canonical_source_contract_version INTEGER NOT NULL CHECK (canonical_source_contract_version > 0),
    canonical_identity_version INTEGER NOT NULL CHECK (canonical_identity_version > 0),
    canonical_identity_kind TEXT NOT NULL CHECK (canonical_identity_kind IN ('stable', 'fallback_fingerprint')),
    canonical_identity_digest BLOB NOT NULL CHECK (length(canonical_identity_digest) = 32),
    event_at TEXT NOT NULL,
    event_time_observation_id TEXT NOT NULL,
    relationship_kind TEXT CHECK (relationship_kind IN ('conversation', 'turn')),
    conversation_id TEXT,
    turn_id TEXT,
    relationship_observation_id TEXT,
    origin TEXT NOT NULL CHECK (origin IN ('realtime', 'historical')),
    UNIQUE (
        canonical_source_id,
        canonical_source_contract_version,
        canonical_identity_version,
        canonical_identity_kind,
        canonical_identity_digest
    ),
    UNIQUE (id, event_time_observation_id),
    FOREIGN KEY (conversation_id) REFERENCES conversations (id),
    FOREIGN KEY (turn_id, conversation_id) REFERENCES turns (id, conversation_id),
    FOREIGN KEY (event_time_observation_id, id)
        REFERENCES model_call_observations (id, model_call_id) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (relationship_observation_id, id)
        REFERENCES model_call_observation_relationships (observation_id, model_call_id)
        DEFERRABLE INITIALLY DEFERRED,
    CHECK (
        (relationship_kind IS NULL
            AND conversation_id IS NULL
            AND turn_id IS NULL
            AND relationship_observation_id IS NULL)
        OR
        (relationship_kind = 'conversation'
            AND conversation_id IS NOT NULL
            AND turn_id IS NULL
            AND relationship_observation_id IS NOT NULL)
        OR
        (relationship_kind = 'turn'
            AND conversation_id IS NOT NULL
            AND turn_id IS NOT NULL
            AND relationship_observation_id IS NOT NULL)
    )
) STRICT;

CREATE TABLE model_call_observations (
    id TEXT PRIMARY KEY,
    model_call_id TEXT NOT NULL,
    source_id TEXT NOT NULL,
    source_contract_version INTEGER NOT NULL CHECK (source_contract_version > 0),
    identity_version INTEGER NOT NULL CHECK (identity_version > 0),
    identity_kind TEXT NOT NULL CHECK (identity_kind IN ('stable', 'fallback_fingerprint')),
    identity_digest BLOB NOT NULL CHECK (length(identity_digest) = 32),
    event_at TEXT NOT NULL,
    event_provenance_observed_at TEXT NOT NULL,
    event_quality TEXT NOT NULL CHECK (event_quality IN ('inferred', 'direct')),
    event_priority INTEGER NOT NULL CHECK (event_priority BETWEEN 0 AND 65535),
    observed_at TEXT NOT NULL,
    origin TEXT NOT NULL CHECK (origin IN ('realtime', 'historical')),
    UNIQUE (id, model_call_id),
    UNIQUE (
        source_id,
        source_contract_version,
        identity_version,
        identity_kind,
        identity_digest
    ),
    FOREIGN KEY (model_call_id) REFERENCES model_calls (id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE TABLE model_call_observation_relationships (
    observation_id TEXT PRIMARY KEY,
    model_call_id TEXT NOT NULL,
    relationship_kind TEXT NOT NULL CHECK (relationship_kind IN ('conversation', 'turn')),
    conversation_id TEXT NOT NULL,
    turn_id TEXT,
    provenance_observed_at TEXT NOT NULL,
    quality TEXT NOT NULL CHECK (quality IN ('inferred', 'direct')),
    source_priority INTEGER NOT NULL CHECK (source_priority BETWEEN 0 AND 65535),
    UNIQUE (observation_id, model_call_id),
    FOREIGN KEY (observation_id, model_call_id)
        REFERENCES model_call_observations (id, model_call_id) ON DELETE CASCADE,
    FOREIGN KEY (conversation_id) REFERENCES conversations (id),
    FOREIGN KEY (turn_id, conversation_id) REFERENCES turns (id, conversation_id),
    CHECK (
        (relationship_kind = 'conversation' AND turn_id IS NULL)
        OR (relationship_kind = 'turn' AND turn_id IS NOT NULL)
    )
) STRICT;

CREATE TABLE model_call_observation_fields (
    observation_id TEXT NOT NULL,
    field_id TEXT NOT NULL,
    text_value TEXT,
    integer_value INTEGER,
    provenance_observed_at TEXT NOT NULL,
    quality TEXT NOT NULL CHECK (quality IN ('inferred', 'direct')),
    source_priority INTEGER NOT NULL CHECK (source_priority BETWEEN 0 AND 65535),
    PRIMARY KEY (observation_id, field_id),
    FOREIGN KEY (observation_id) REFERENCES model_call_observations (id) ON DELETE CASCADE,
    FOREIGN KEY (field_id) REFERENCES telemetry_field_registry (field_id),
    CHECK (field_id IN (
        'conversation.source_id',
        'conversation.thread_id',
        'turn.source_id',
        'model_call.model',
        'model_call.reasoning_effort',
        'model_call.input_tokens',
        'model_call.cached_input_tokens',
        'model_call.output_tokens',
        'model_call.reasoning_output_tokens',
        'model_call.surface',
        'model_call.client',
        'model_call.event_name',
        'model_call.stream_event_kind'
    )),
    CHECK (
        (field_id IN (
            'model_call.input_tokens',
            'model_call.cached_input_tokens',
            'model_call.output_tokens',
            'model_call.reasoning_output_tokens'
        )
            AND integer_value BETWEEN 0 AND 1000000000000000
            AND text_value IS NULL)
        OR
        (field_id NOT IN (
            'model_call.input_tokens',
            'model_call.cached_input_tokens',
            'model_call.output_tokens',
            'model_call.reasoning_output_tokens'
        )
            AND text_value IS NOT NULL
            AND integer_value IS NULL)
    )
) STRICT, WITHOUT ROWID;

CREATE TABLE model_call_projection_fields (
    model_call_id TEXT NOT NULL,
    field_id TEXT NOT NULL,
    observation_id TEXT NOT NULL,
    PRIMARY KEY (model_call_id, field_id),
    FOREIGN KEY (model_call_id) REFERENCES model_calls (id) ON DELETE CASCADE,
    FOREIGN KEY (observation_id, model_call_id)
        REFERENCES model_call_observations (id, model_call_id) ON DELETE CASCADE,
    FOREIGN KEY (observation_id, field_id)
        REFERENCES model_call_observation_fields (observation_id, field_id) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE TABLE source_checkpoints (
    source_id TEXT NOT NULL,
    source_contract_version INTEGER NOT NULL CHECK (source_contract_version > 0),
    checkpoint_kind TEXT NOT NULL CHECK (checkpoint_kind IN ('otlp_receiver', 'sessions_file')),
    generation INTEGER NOT NULL CHECK (generation >= 0),
    offset_bytes INTEGER CHECK (offset_bytes >= 0),
    fingerprint_digest BLOB CHECK (fingerprint_digest IS NULL OR length(fingerprint_digest) = 32),
    updated_at TEXT NOT NULL,
    PRIMARY KEY (source_id, source_contract_version)
) STRICT, WITHOUT ROWID;

CREATE TABLE source_health (
    source_id TEXT NOT NULL,
    source_contract_version INTEGER NOT NULL CHECK (source_contract_version > 0),
    status TEXT NOT NULL CHECK (status IN ('starting', 'healthy', 'degraded', 'unavailable', 'stopped')),
    last_accepted_at TEXT,
    last_error_code TEXT CHECK (last_error_code IN (
        'otlp_bind_failed',
        'otlp_invalid_content_type',
        'otlp_compressed_body_too_large',
        'otlp_decompressed_body_too_large',
        'otlp_gzip_decode_failed',
        'otlp_json_decode_failed',
        'otlp_protobuf_decode_failed',
        'otlp_decode_timed_out',
        'persistence_commit_failed',
        'source_checkpoint_invalid',
        'source_unavailable',
        'internal_queue_closed'
    )),
    last_error_occurred_at TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (source_id, source_contract_version),
    CHECK (
        (last_error_code IS NULL AND last_error_occurred_at IS NULL)
        OR (last_error_code IS NOT NULL AND last_error_occurred_at IS NOT NULL)
    )
) STRICT, WITHOUT ROWID;

CREATE TABLE source_health_supported_fields (
    source_id TEXT NOT NULL,
    source_contract_version INTEGER NOT NULL,
    field_id TEXT NOT NULL,
    PRIMARY KEY (source_id, source_contract_version, field_id),
    FOREIGN KEY (source_id, source_contract_version)
        REFERENCES source_health (source_id, source_contract_version) ON DELETE CASCADE,
    FOREIGN KEY (field_id) REFERENCES telemetry_field_registry (field_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE domain_work (
    id TEXT PRIMARY KEY,
    work_kind TEXT NOT NULL CHECK (work_kind = 'model_call.completed'),
    model_call_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'started', 'completed')),
    created_at TEXT NOT NULL,
    started_at TEXT,
    completed_at TEXT,
    UNIQUE (work_kind, model_call_id),
    FOREIGN KEY (model_call_id) REFERENCES model_calls (id) ON DELETE CASCADE,
    CHECK (
        (state = 'pending' AND started_at IS NULL AND completed_at IS NULL)
        OR (state = 'started' AND started_at IS NOT NULL AND completed_at IS NULL)
        OR (state = 'completed' AND started_at IS NOT NULL AND completed_at IS NOT NULL)
    )
) STRICT;

CREATE INDEX model_calls_event_at_idx ON model_calls (event_at DESC, id DESC);
CREATE INDEX model_calls_conversation_idx ON model_calls (conversation_id, event_at DESC, id DESC);
CREATE INDEX observations_model_call_idx ON model_call_observations (model_call_id, observed_at, id);
CREATE INDEX domain_work_state_idx ON domain_work (state, created_at, id);
CREATE INDEX conversation_source_keys_conversation_idx
    ON conversation_source_keys (conversation_id, key_kind);

-- These cyclic projection references are intentionally deferred until commit.
-- They prove that a projection can reference only an observation of the same call.
CREATE TRIGGER model_calls_projection_reference_guard
BEFORE UPDATE OF event_time_observation_id, relationship_observation_id ON model_calls
BEGIN
    SELECT CASE
        WHEN NOT EXISTS (
            SELECT 1
            FROM model_call_observations AS observation
            WHERE observation.id = NEW.event_time_observation_id
              AND observation.model_call_id = NEW.id
        )
        THEN RAISE(ABORT, 'invalid event-time projection reference')
    END;
    SELECT CASE
        WHEN NEW.relationship_observation_id IS NOT NULL
         AND NOT EXISTS (
            SELECT 1
            FROM model_call_observation_relationships AS relationship
            WHERE relationship.observation_id = NEW.relationship_observation_id
              AND relationship.model_call_id = NEW.id
        )
        THEN RAISE(ABORT, 'invalid relationship projection reference')
    END;
END;
