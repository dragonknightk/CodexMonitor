-- Rule, alert, action, and settings schema.
-- Action lease and attempt columns are SQLite worker infrastructure metadata.

CREATE TABLE rules (
    rule_id TEXT NOT NULL,
    rule_version INTEGER NOT NULL CHECK (rule_version > 0),
    rule_kind TEXT NOT NULL CHECK (rule_kind = 'suspected_reasoning_limit_pattern'),
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    notify_max_n INTEGER CHECK (notify_max_n IS NULL OR notify_max_n > 0),
    notify_all INTEGER NOT NULL CHECK (notify_all IN (0, 1)),
    last_evaluated_at TEXT,
    last_error_code TEXT CHECK (last_error_code IS NULL OR last_error_code IN (
        'persistence_commit_failed',
        'internal_queue_closed'
    )),
    PRIMARY KEY (rule_id, rule_version),
    CHECK (NOT (notify_all = 1 AND notify_max_n IS NOT NULL))
) STRICT, WITHOUT ROWID;

CREATE TABLE rule_evaluations (
    id TEXT PRIMARY KEY,
    rule_id TEXT NOT NULL,
    rule_version INTEGER NOT NULL,
    model_call_id TEXT NOT NULL,
    matched INTEGER NOT NULL CHECK (matched IN (0, 1)),
    reasoning_output_tokens INTEGER CHECK (reasoning_output_tokens BETWEEN 0 AND 1000000000000000),
    pattern_n INTEGER CHECK (pattern_n IS NULL OR pattern_n > 0),
    event_at TEXT NOT NULL,
    source_quality TEXT CHECK (source_quality IN ('inferred', 'direct')),
    evaluated_at TEXT NOT NULL,
    UNIQUE (rule_id, rule_version, model_call_id),
    UNIQUE (id, model_call_id, rule_id, rule_version),
    FOREIGN KEY (rule_id, rule_version) REFERENCES rules (rule_id, rule_version),
    FOREIGN KEY (model_call_id) REFERENCES model_calls (id) ON DELETE CASCADE,
    CHECK (
        (matched = 0 AND pattern_n IS NULL)
        OR (matched = 1 AND reasoning_output_tokens IS NOT NULL AND pattern_n IS NOT NULL)
    )
) STRICT;

CREATE TABLE alerts (
    id TEXT PRIMARY KEY,
    evaluation_id TEXT NOT NULL UNIQUE,
    model_call_id TEXT NOT NULL,
    rule_id TEXT NOT NULL,
    rule_version INTEGER NOT NULL,
    reasoning_output_tokens INTEGER NOT NULL CHECK (reasoning_output_tokens BETWEEN 0 AND 1000000000000000),
    pattern_n INTEGER NOT NULL CHECK (pattern_n > 0),
    event_at TEXT NOT NULL,
    source_quality TEXT NOT NULL CHECK (source_quality IN ('inferred', 'direct')),
    created_at TEXT NOT NULL,
    FOREIGN KEY (evaluation_id, model_call_id, rule_id, rule_version)
        REFERENCES rule_evaluations (id, model_call_id, rule_id, rule_version)
        ON DELETE CASCADE
) STRICT;

CREATE TABLE action_outbox (
    id TEXT PRIMARY KEY,
    alert_id TEXT NOT NULL,
    action_type TEXT NOT NULL CHECK (action_type = 'system_notification'),
    idempotency_key TEXT NOT NULL UNIQUE,
    state TEXT NOT NULL CHECK (state IN ('pending', 'started', 'completed', 'failed', 'suppressed', 'unavailable')),
    model TEXT,
    reasoning_output_tokens INTEGER NOT NULL CHECK (reasoning_output_tokens BETWEEN 0 AND 1000000000000000),
    pattern_n INTEGER NOT NULL CHECK (pattern_n > 0),
    created_at TEXT NOT NULL,
    not_before TEXT NOT NULL,
    lease_owner TEXT,
    lease_expires_at TEXT,
    started_at TEXT,
    completed_at TEXT,
    last_error_code TEXT CHECK (last_error_code IS NULL OR last_error_code IN (
        'persistence_commit_failed',
        'source_unavailable',
        'internal_queue_closed'
    )),
    FOREIGN KEY (alert_id) REFERENCES alerts (id) ON DELETE CASCADE,
    CHECK (
        (state = 'pending' AND started_at IS NULL AND completed_at IS NULL)
        OR (state = 'started' AND started_at IS NOT NULL AND completed_at IS NULL)
        OR (state IN ('completed', 'failed', 'suppressed', 'unavailable') AND completed_at IS NOT NULL)
    ),
    CHECK (
        (lease_owner IS NULL AND lease_expires_at IS NULL)
        OR (lease_owner IS NOT NULL AND lease_expires_at IS NOT NULL)
    )
) STRICT;

CREATE TABLE action_attempts (
    action_id TEXT NOT NULL,
    attempt_number INTEGER NOT NULL CHECK (attempt_number > 0),
    state TEXT NOT NULL CHECK (state IN ('started', 'completed', 'failed', 'unavailable', 'indeterminate')),
    started_at TEXT NOT NULL,
    completed_at TEXT,
    safe_error_code TEXT CHECK (safe_error_code IS NULL OR safe_error_code IN (
        'persistence_commit_failed',
        'source_unavailable',
        'internal_queue_closed'
    )),
    PRIMARY KEY (action_id, attempt_number),
    FOREIGN KEY (action_id) REFERENCES action_outbox (id) ON DELETE CASCADE,
    CHECK (
        (state = 'started' AND completed_at IS NULL)
        OR (state <> 'started' AND completed_at IS NOT NULL)
    )
) STRICT, WITHOUT ROWID;

CREATE TABLE app_settings (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    revision INTEGER NOT NULL CHECK (revision > 0),
    retention_days INTEGER CHECK (retention_days IS NULL OR retention_days IN (30, 90, 365)),
    notifications_enabled INTEGER NOT NULL CHECK (notifications_enabled IN (0, 1)),
    notify_max_n INTEGER CHECK (notify_max_n IS NULL OR notify_max_n > 0),
    notify_all INTEGER NOT NULL CHECK (notify_all IN (0, 1)),
    locale TEXT NOT NULL CHECK (locale IN ('en-US', 'zh-CN')),
    login_autostart INTEGER NOT NULL CHECK (login_autostart IN (0, 1)),
    updated_at TEXT NOT NULL,
    CHECK (NOT (notify_all = 1 AND notify_max_n IS NOT NULL))
) STRICT;

-- Privacy-safe audit for the explicit legacy importer. Source paths, raw
-- schema text, SQL and payload content are intentionally unrepresentable.
CREATE TABLE legacy_import_audit (
    id TEXT PRIMARY KEY,
    source_fingerprint BLOB NOT NULL UNIQUE CHECK (length(source_fingerprint) = 32),
    detected_schema TEXT NOT NULL CHECK (detected_schema IN ('structured_v1', 'raw_legacy')),
    imported_model_calls INTEGER NOT NULL CHECK (imported_model_calls >= 0),
    imported_conversations INTEGER NOT NULL CHECK (imported_conversations >= 0),
    completed_at TEXT NOT NULL
) STRICT;

INSERT INTO app_settings (
    singleton,
    revision,
    retention_days,
    notifications_enabled,
    notify_max_n,
    notify_all,
    locale,
    login_autostart,
    updated_at
) VALUES (1, 1, 90, 1, 1, 0, 'en-US', 0, '1970-01-01T00:00:00.000000000Z');

CREATE INDEX rule_evaluations_model_call_idx ON rule_evaluations (model_call_id, evaluated_at);
CREATE INDEX alerts_event_at_idx ON alerts (event_at DESC, id DESC);
CREATE INDEX action_outbox_state_idx ON action_outbox (state, not_before, id);
