-- Replace the pre-runtime action placeholders with a fenced transactional Outbox.
--
-- Version-zero idempotency material preserves pre-v6 development audit rows
-- exactly as bytes. Those rows are made terminal before the new worker can run;
-- every executable request uses the version-one 32-byte digest.

DROP TRIGGER action_outbox_historical_insert_guard;
DROP TRIGGER action_outbox_historical_update_guard;

CREATE TABLE action_outbox_v6 (
    id TEXT PRIMARY KEY,
    alert_id TEXT NOT NULL,
    action_type TEXT NOT NULL CHECK (
        length(action_type) BETWEEN 1 AND 64
        AND substr(action_type, 1, 1) GLOB '[a-z]'
        AND substr(action_type, -1, 1) GLOB '[a-z0-9]'
        AND action_type NOT GLOB '*[^a-z0-9_]*'
        AND action_type NOT GLOB '*__*'
    ),
    parameter_schema_version INTEGER NOT NULL
        CHECK (parameter_schema_version BETWEEN 1 AND 65535),
    idempotency_key_version INTEGER NOT NULL
        CHECK (idempotency_key_version BETWEEN 0 AND 65535),
    idempotency_key_bytes BLOB NOT NULL,
    state TEXT NOT NULL CHECK (
        state IN ('pending', 'started', 'completed', 'failed', 'suppressed', 'unavailable')
    ),
    model TEXT CHECK (model IS NULL OR length(model) BETWEEN 1 AND 128),
    reasoning_output_tokens INTEGER NOT NULL
        CHECK (reasoning_output_tokens BETWEEN 0 AND 1000000000000000),
    pattern_n INTEGER NOT NULL CHECK (pattern_n > 0),
    created_at TEXT NOT NULL,
    not_before TEXT NOT NULL,
    lease_generation INTEGER NOT NULL DEFAULT 0 CHECK (lease_generation >= 0),
    lease_owner TEXT CHECK (lease_owner IS NULL OR length(lease_owner) BETWEEN 1 AND 128),
    lease_expires_at TEXT,
    started_at TEXT,
    completed_at TEXT,
    last_error_code TEXT CHECK (last_error_code IS NULL OR last_error_code IN (
        'invalid_parameters',
        'permission_denied',
        'notification_unavailable',
        'notification_dispatch_failed',
        'indeterminate_after_crash',
        'unknown_action_type',
        'internal_queue_closed'
    )),
    suppression_reason TEXT CHECK (suppression_reason IS NULL OR suppression_reason IN (
        'notifications_disabled',
        'above_threshold',
        'permission_denied',
        'coalesced',
        'historical_event',
        'pre_outbox_upgrade'
    )),
    UNIQUE (idempotency_key_version, idempotency_key_bytes),
    UNIQUE (alert_id, action_type, parameter_schema_version),
    UNIQUE (id, lease_generation),
    FOREIGN KEY (alert_id) REFERENCES alerts (id) ON DELETE CASCADE,
    CHECK (created_at <= not_before),
    CHECK (started_at IS NULL OR started_at >= created_at),
    CHECK (completed_at IS NULL OR completed_at >= COALESCE(started_at, created_at)),
    CHECK (
        (idempotency_key_version = 1 AND length(idempotency_key_bytes) = 32)
        OR
        (idempotency_key_version = 0
            AND length(idempotency_key_bytes) BETWEEN 1 AND 256
            AND state IN ('completed', 'failed', 'suppressed', 'unavailable'))
    ),
    CHECK (
        (state = 'pending'
            AND started_at IS NULL
            AND completed_at IS NULL
            AND last_error_code IS NULL
            AND suppression_reason IS NULL)
        OR
        (state = 'started'
            AND started_at IS NOT NULL
            AND completed_at IS NULL
            AND last_error_code IS NULL
            AND suppression_reason IS NULL)
        OR
        (state = 'completed'
            AND started_at IS NOT NULL
            AND completed_at IS NOT NULL
            AND last_error_code IS NULL
            AND suppression_reason IS NULL)
        OR
        (state = 'failed'
            AND started_at IS NOT NULL
            AND completed_at IS NOT NULL
            AND last_error_code IS NOT NULL
            AND suppression_reason IS NULL)
        OR
        (state = 'suppressed'
            AND started_at IS NULL
            AND completed_at IS NOT NULL
            AND last_error_code IS NULL
            AND suppression_reason IS NOT NULL)
        OR
        (state = 'unavailable'
            AND completed_at IS NOT NULL
            AND last_error_code IS NOT NULL
            AND suppression_reason IS NULL)
    ),
    CHECK (
        (state = 'pending'
            AND (
                (lease_owner IS NULL AND lease_expires_at IS NULL)
                OR
                (lease_generation > 0
                    AND lease_owner IS NOT NULL
                    AND lease_expires_at IS NOT NULL
                    AND lease_expires_at > not_before)
            ))
        OR
        (state = 'started'
            AND lease_generation > 0
            AND lease_owner IS NOT NULL
            AND lease_expires_at IS NOT NULL
            AND lease_expires_at > not_before)
        OR
        (state IN ('completed', 'failed', 'suppressed', 'unavailable')
            AND lease_owner IS NULL
            AND lease_expires_at IS NULL)
    )
) STRICT;

-- No pre-v6 producer implemented the registered typed action contract. Preserve
-- its rows for audit, but make every pending or started request terminal so an
-- upgrade can never turn development data into a user-visible notification.
INSERT INTO action_outbox_v6 (
    id,
    alert_id,
    action_type,
    parameter_schema_version,
    idempotency_key_version,
    idempotency_key_bytes,
    state,
    model,
    reasoning_output_tokens,
    pattern_n,
    created_at,
    not_before,
    lease_generation,
    lease_owner,
    lease_expires_at,
    started_at,
    completed_at,
    last_error_code,
    suppression_reason
)
SELECT
    action.id,
    action.alert_id,
    action.action_type,
    1,
    0,
    CAST(action.idempotency_key AS BLOB),
    CASE action.state
        WHEN 'pending' THEN 'suppressed'
        WHEN 'started' THEN 'failed'
        ELSE action.state
    END,
    action.model,
    action.reasoning_output_tokens,
    action.pattern_n,
    action.created_at,
    max(action.created_at, action.not_before),
    0,
    NULL,
    NULL,
    CASE
        WHEN action.state IN ('started', 'completed', 'failed')
            THEN COALESCE(action.started_at, action.completed_at, action.created_at)
        WHEN action.state = 'unavailable' AND action.started_at IS NOT NULL
            THEN action.started_at
        ELSE NULL
    END,
    CASE
        WHEN action.state = 'pending' THEN action.created_at
        WHEN action.state = 'started' THEN COALESCE(action.started_at, action.created_at)
        ELSE COALESCE(action.completed_at, action.started_at, action.created_at)
    END,
    CASE
        WHEN action.state = 'started' THEN 'indeterminate_after_crash'
        WHEN action.state = 'failed' THEN
            CASE action.last_error_code
                WHEN 'source_unavailable' THEN 'notification_unavailable'
                WHEN 'internal_queue_closed' THEN 'internal_queue_closed'
                ELSE 'notification_dispatch_failed'
            END
        WHEN action.state = 'unavailable' THEN
            CASE action.last_error_code
                WHEN 'persistence_commit_failed' THEN 'notification_dispatch_failed'
                WHEN 'internal_queue_closed' THEN 'internal_queue_closed'
                ELSE 'notification_unavailable'
            END
        ELSE NULL
    END,
    CASE
        WHEN action.state = 'pending' THEN 'pre_outbox_upgrade'
        WHEN action.state = 'suppressed' AND EXISTS (
            SELECT 1
            FROM alerts AS alert
            INNER JOIN model_calls AS model_call ON model_call.id = alert.model_call_id
            WHERE alert.id = action.alert_id
              AND model_call.origin = 'historical'
        ) THEN 'historical_event'
        WHEN action.state = 'suppressed' THEN 'pre_outbox_upgrade'
        ELSE NULL
    END
FROM action_outbox AS action;

CREATE TABLE action_attempts_v6 (
    action_id TEXT NOT NULL,
    attempt_number INTEGER NOT NULL CHECK (attempt_number > 0),
    lease_generation INTEGER NOT NULL CHECK (lease_generation >= 0),
    state TEXT NOT NULL CHECK (
        state IN ('started', 'completed', 'failed', 'unavailable', 'indeterminate')
    ),
    started_at TEXT NOT NULL,
    completed_at TEXT,
    safe_error_code TEXT CHECK (safe_error_code IS NULL OR safe_error_code IN (
        'invalid_parameters',
        'permission_denied',
        'notification_unavailable',
        'notification_dispatch_failed',
        'indeterminate_after_crash',
        'unknown_action_type',
        'internal_queue_closed'
    )),
    PRIMARY KEY (action_id, attempt_number),
    FOREIGN KEY (action_id, lease_generation)
        REFERENCES action_outbox_v6 (id, lease_generation) ON DELETE CASCADE,
    CHECK (completed_at IS NULL OR completed_at >= started_at),
    CHECK (
        (state = 'started'
            AND lease_generation > 0
            AND completed_at IS NULL
            AND safe_error_code IS NULL)
        OR
        (state = 'completed'
            AND completed_at IS NOT NULL
            AND safe_error_code IS NULL)
        OR
        (state IN ('failed', 'unavailable', 'indeterminate')
            AND completed_at IS NOT NULL
            AND safe_error_code IS NOT NULL)
    ),
    CHECK (lease_generation > 0 OR state <> 'started')
) STRICT, WITHOUT ROWID;

INSERT INTO action_attempts_v6 (
    action_id,
    attempt_number,
    lease_generation,
    state,
    started_at,
    completed_at,
    safe_error_code
)
SELECT
    attempt.action_id,
    attempt.attempt_number,
    0,
    CASE attempt.state
        WHEN 'started' THEN 'indeterminate'
        ELSE attempt.state
    END,
    attempt.started_at,
    CASE
        WHEN attempt.state = 'started' THEN attempt.started_at
        ELSE COALESCE(attempt.completed_at, attempt.started_at)
    END,
    CASE
        WHEN attempt.state IN ('started', 'indeterminate') THEN 'indeterminate_after_crash'
        WHEN attempt.state = 'failed' THEN
            CASE attempt.safe_error_code
                WHEN 'source_unavailable' THEN 'notification_unavailable'
                WHEN 'internal_queue_closed' THEN 'internal_queue_closed'
                ELSE 'notification_dispatch_failed'
            END
        WHEN attempt.state = 'unavailable' THEN
            CASE attempt.safe_error_code
                WHEN 'persistence_commit_failed' THEN 'notification_dispatch_failed'
                WHEN 'internal_queue_closed' THEN 'internal_queue_closed'
                ELSE 'notification_unavailable'
            END
        ELSE NULL
    END
FROM action_attempts AS attempt;

DROP TABLE action_attempts;
DROP TABLE action_outbox;
ALTER TABLE action_outbox_v6 RENAME TO action_outbox;
ALTER TABLE action_attempts_v6 RENAME TO action_attempts;

CREATE INDEX action_outbox_claim_idx
    ON action_outbox (state, not_before, created_at, id);
CREATE INDEX action_outbox_lease_expiry_idx
    ON action_outbox (state, lease_expires_at, id);
CREATE INDEX action_outbox_started_recovery_idx
    ON action_outbox (started_at, id) WHERE state = 'started';
CREATE UNIQUE INDEX action_attempts_live_generation_idx
    ON action_attempts (action_id, lease_generation) WHERE lease_generation > 0;

CREATE TRIGGER action_outbox_initial_state_guard
BEFORE INSERT ON action_outbox
WHEN NEW.state NOT IN ('pending', 'suppressed')
  OR NEW.lease_generation <> 0
  OR NEW.lease_owner IS NOT NULL
  OR NEW.lease_expires_at IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'invalid initial action state');
END;

CREATE TRIGGER action_outbox_request_immutable_guard
BEFORE UPDATE OF
    id,
    alert_id,
    action_type,
    parameter_schema_version,
    idempotency_key_version,
    idempotency_key_bytes,
    model,
    reasoning_output_tokens,
    pattern_n,
    created_at,
    not_before
ON action_outbox
BEGIN
    SELECT RAISE(ABORT, 'action request is immutable');
END;

CREATE TRIGGER action_outbox_terminal_immutable_guard
BEFORE UPDATE ON action_outbox
WHEN OLD.state IN ('completed', 'failed', 'suppressed', 'unavailable')
BEGIN
    SELECT RAISE(ABORT, 'terminal action is immutable');
END;

CREATE TRIGGER action_outbox_started_at_immutable_guard
BEFORE UPDATE OF started_at ON action_outbox
WHEN OLD.started_at IS NOT NULL
 AND NEW.started_at IS NOT OLD.started_at
BEGIN
    SELECT RAISE(ABORT, 'action start time is immutable');
END;

CREATE TRIGGER action_outbox_state_transition_guard
BEFORE UPDATE OF state ON action_outbox
WHEN NEW.state <> OLD.state
 AND NOT (
    (OLD.state = 'pending' AND NEW.state IN ('started', 'suppressed', 'unavailable'))
    OR
    (OLD.state = 'started' AND NEW.state IN ('completed', 'failed', 'unavailable'))
 )
BEGIN
    SELECT RAISE(ABORT, 'invalid action state transition');
END;

CREATE TRIGGER action_outbox_lease_transition_guard
BEFORE UPDATE OF lease_generation, lease_owner, lease_expires_at ON action_outbox
WHEN (
        NEW.lease_generation <> OLD.lease_generation
        OR NEW.lease_owner IS NOT OLD.lease_owner
        OR NEW.lease_expires_at IS NOT OLD.lease_expires_at
     )
 AND NOT (
    -- Claim or reclaim pending work with a new fencing generation.
    (OLD.state = 'pending'
        AND NEW.state = 'pending'
        AND NEW.lease_generation = OLD.lease_generation + 1
        AND NEW.lease_owner IS NOT NULL
        AND NEW.lease_expires_at IS NOT NULL)
    OR
    -- Release an owned pending claim without rewinding its generation.
    (OLD.state = 'pending'
        AND NEW.state = 'pending'
        AND NEW.lease_generation = OLD.lease_generation
        AND OLD.lease_owner IS NOT NULL
        AND NEW.lease_owner IS NULL
        AND NEW.lease_expires_at IS NULL)
    OR
    -- Finish or suppress claimed work and clear its ephemeral lease.
    (OLD.state IN ('pending', 'started')
        AND NEW.state IN ('completed', 'failed', 'suppressed', 'unavailable')
        AND NEW.lease_generation = OLD.lease_generation
        AND NEW.lease_owner IS NULL
        AND NEW.lease_expires_at IS NULL)
 )
BEGIN
    SELECT RAISE(ABORT, 'invalid action lease transition');
END;

CREATE TRIGGER action_attempts_insert_guard
BEFORE INSERT ON action_attempts
WHEN NEW.state <> 'started'
  OR NEW.lease_generation <= 0
  OR NEW.attempt_number <> COALESCE((
      SELECT max(attempt.attempt_number) + 1
      FROM action_attempts AS attempt
      WHERE attempt.action_id = NEW.action_id
  ), 1)
  OR NOT EXISTS (
      SELECT 1
      FROM action_outbox AS action
      WHERE action.id = NEW.action_id
        AND action.state = 'started'
        AND action.lease_generation = NEW.lease_generation
        AND action.started_at = NEW.started_at
  )
BEGIN
    SELECT RAISE(ABORT, 'invalid initial action attempt');
END;

CREATE TRIGGER action_attempts_identity_immutable_guard
BEFORE UPDATE OF action_id, attempt_number, lease_generation, started_at ON action_attempts
BEGIN
    SELECT RAISE(ABORT, 'action attempt identity is immutable');
END;

CREATE TRIGGER action_attempts_terminal_immutable_guard
BEFORE UPDATE ON action_attempts
WHEN OLD.state <> 'started'
BEGIN
    SELECT RAISE(ABORT, 'terminal action attempt is immutable');
END;

CREATE TRIGGER action_attempts_state_transition_guard
BEFORE UPDATE OF state ON action_attempts
WHEN OLD.state <> 'started'
  OR NEW.state NOT IN ('completed', 'failed', 'unavailable', 'indeterminate')
BEGIN
    SELECT RAISE(ABORT, 'invalid action attempt state transition');
END;

CREATE TRIGGER action_outbox_terminal_attempt_guard
BEFORE UPDATE OF state ON action_outbox
WHEN OLD.state = 'started'
 AND NEW.state IN ('completed', 'failed', 'unavailable')
 AND NOT EXISTS (
     SELECT 1
     FROM action_attempts AS attempt
     WHERE attempt.action_id = OLD.id
       AND attempt.lease_generation = OLD.lease_generation
       AND attempt.completed_at = NEW.completed_at
       AND (
           (NEW.state = 'completed' AND attempt.state = 'completed')
           OR (NEW.state = 'failed'
               AND attempt.state IN ('failed', 'indeterminate')
               AND attempt.safe_error_code = NEW.last_error_code)
           OR (NEW.state = 'unavailable'
               AND attempt.state = 'unavailable'
               AND attempt.safe_error_code = NEW.last_error_code)
       )
 )
BEGIN
    SELECT RAISE(ABORT, 'terminal action requires a matching attempt');
END;

CREATE TRIGGER action_outbox_historical_insert_guard
BEFORE INSERT ON action_outbox
WHEN EXISTS (
    SELECT 1
    FROM alerts AS alert
    INNER JOIN model_calls AS model_call ON model_call.id = alert.model_call_id
    WHERE alert.id = NEW.alert_id
      AND model_call.origin = 'historical'
)
BEGIN
    SELECT RAISE(ABORT, 'historical alert cannot enter the action outbox');
END;

CREATE TRIGGER action_outbox_historical_update_guard
BEFORE UPDATE OF alert_id ON action_outbox
WHEN EXISTS (
    SELECT 1
    FROM alerts AS alert
    INNER JOIN model_calls AS model_call ON model_call.id = alert.model_call_id
    WHERE alert.id = NEW.alert_id
      AND model_call.origin = 'historical'
)
BEGIN
    SELECT RAISE(ABORT, 'historical alert cannot enter the action outbox');
END;
