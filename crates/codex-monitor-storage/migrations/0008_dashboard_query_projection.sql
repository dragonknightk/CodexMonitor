-- Bounded Dashboard reads use a transaction-maintained, one-row-per-call
-- projection.  The EAV observation tables remain the provenance authority.

CREATE TABLE model_call_dashboard_projection (
    model_call_id TEXT PRIMARY KEY,
    event_at TEXT NOT NULL,
    conversation_id TEXT,
    turn_id TEXT,
    temporal_class TEXT NOT NULL CHECK (temporal_class IN ('realtime', 'historical')),
    conversation_source_id TEXT CHECK (
        conversation_source_id IS NULL OR length(conversation_source_id) BETWEEN 1 AND 256
    ),
    thread_source_id TEXT CHECK (
        thread_source_id IS NULL OR length(thread_source_id) BETWEEN 1 AND 256
    ),
    model TEXT CHECK (model IS NULL OR length(model) BETWEEN 1 AND 128),
    reasoning_effort TEXT CHECK (
        reasoning_effort IS NULL
        OR reasoning_effort IN ('none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max', 'ultra')
    ),
    input_tokens INTEGER CHECK (input_tokens BETWEEN 0 AND 1000000000000000),
    cached_input_tokens INTEGER CHECK (cached_input_tokens BETWEEN 0 AND 1000000000000000),
    output_tokens INTEGER CHECK (output_tokens BETWEEN 0 AND 1000000000000000),
    reasoning_output_tokens INTEGER CHECK (
        reasoning_output_tokens BETWEEN 0 AND 1000000000000000
    ),
    client TEXT CHECK (client IS NULL OR length(client) BETWEEN 1 AND 128),
    surface TEXT CHECK (surface IS NULL OR length(surface) BETWEEN 1 AND 64),
    FOREIGN KEY (model_call_id) REFERENCES model_calls (id) ON DELETE CASCADE,
    FOREIGN KEY (conversation_id) REFERENCES conversations (id),
    FOREIGN KEY (turn_id, conversation_id) REFERENCES turns (id, conversation_id),
    CHECK (turn_id IS NULL OR conversation_id IS NOT NULL)
) STRICT;

WITH selected AS (
    SELECT
        projection.model_call_id,
        max(CASE WHEN projection.field_id = 'conversation.source_id' THEN field.text_value END)
            AS conversation_source_id,
        max(CASE WHEN projection.field_id = 'conversation.thread_id' THEN field.text_value END)
            AS thread_source_id,
        max(CASE WHEN projection.field_id = 'model_call.model' THEN field.text_value END)
            AS model,
        max(CASE WHEN projection.field_id = 'model_call.reasoning_effort' THEN field.text_value END)
            AS reasoning_effort,
        max(CASE WHEN projection.field_id = 'model_call.input_tokens' THEN field.integer_value END)
            AS input_tokens,
        max(CASE WHEN projection.field_id = 'model_call.cached_input_tokens' THEN field.integer_value END)
            AS cached_input_tokens,
        max(CASE WHEN projection.field_id = 'model_call.output_tokens' THEN field.integer_value END)
            AS output_tokens,
        max(CASE WHEN projection.field_id = 'model_call.reasoning_output_tokens' THEN field.integer_value END)
            AS reasoning_output_tokens,
        max(CASE WHEN projection.field_id = 'model_call.client' THEN field.text_value END)
            AS client,
        max(CASE WHEN projection.field_id = 'model_call.surface' THEN field.text_value END)
            AS surface
    FROM model_call_projection_fields AS projection
    JOIN model_call_observation_fields AS field
      ON field.observation_id = projection.observation_id
     AND field.field_id = projection.field_id
    GROUP BY projection.model_call_id
)
INSERT INTO model_call_dashboard_projection (
    model_call_id, event_at, conversation_id, turn_id, temporal_class,
    conversation_source_id, thread_source_id, model, reasoning_effort,
    input_tokens, cached_input_tokens, output_tokens, reasoning_output_tokens,
    client, surface
)
SELECT
    model_call.id,
    model_call.event_at,
    model_call.conversation_id,
    model_call.turn_id,
    model_call.origin,
    selected.conversation_source_id,
    selected.thread_source_id,
    selected.model,
    selected.reasoning_effort,
    selected.input_tokens,
    selected.cached_input_tokens,
    selected.output_tokens,
    selected.reasoning_output_tokens,
    selected.client,
    selected.surface
FROM model_calls AS model_call
LEFT JOIN selected ON selected.model_call_id = model_call.id;

-- Attempt an impossible projection row only when the backfill is incomplete.
-- The invalid temporal class closes the migration without a temporary schema object.
INSERT INTO model_call_dashboard_projection (
    model_call_id, event_at, temporal_class
)
SELECT
    '__dashboard_projection_backfill_guard__',
    '1970-01-01T00:00:00.000000000Z',
    '__invalid__'
WHERE (SELECT count(*) FROM model_calls)
   != (SELECT count(*) FROM model_call_dashboard_projection);

-- Alert evidence must remain the exact provenance that caused the historical
-- rule match, even if a later observation changes the canonical projection.
CREATE TABLE alert_evidence_provenance (
    alert_id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL CHECK (
        length(source_id) BETWEEN 1 AND 64
        AND substr(source_id, 1, 1) GLOB '[a-z0-9]'
        AND substr(source_id, -1, 1) GLOB '[a-z0-9]'
        AND source_id NOT GLOB '*[^a-z0-9._-]*'
        AND source_id NOT GLOB '*[._-][._-]*'
    ),
    source_contract_version INTEGER NOT NULL
        CHECK (source_contract_version BETWEEN 1 AND 65535),
    observed_at TEXT NOT NULL,
    quality TEXT NOT NULL CHECK (quality IN ('inferred', 'direct')),
    FOREIGN KEY (alert_id) REFERENCES alerts (id) ON DELETE CASCADE
) STRICT;

WITH candidates AS (
    SELECT DISTINCT
        alert.id AS alert_id,
        observation.source_id,
        observation.source_contract_version,
        field.provenance_observed_at AS observed_at,
        field.quality
    FROM alerts AS alert
    JOIN model_call_observations AS observation
      ON observation.model_call_id = alert.model_call_id
    JOIN model_call_observation_fields AS field
      ON field.observation_id = observation.id
     AND field.field_id = 'model_call.reasoning_output_tokens'
     AND field.integer_value = alert.reasoning_output_tokens
     AND field.quality = alert.source_quality
), unique_candidates AS (
    SELECT
        alert_id,
        min(source_id) AS source_id,
        min(source_contract_version) AS source_contract_version,
        min(observed_at) AS observed_at,
        min(quality) AS quality
    FROM candidates
    GROUP BY alert_id
    HAVING count(*) = 1
)
INSERT INTO alert_evidence_provenance (
    alert_id, source_id, source_contract_version, observed_at, quality
)
SELECT alert_id, source_id, source_contract_version, observed_at, quality
FROM unique_candidates;

-- As above, a mismatched count attempts one impossible row and aborts atomically.
INSERT INTO alert_evidence_provenance (
    alert_id, source_id, source_contract_version, observed_at, quality
)
SELECT
    '__alert_provenance_backfill_guard__',
    '__invalid__',
    1,
    '1970-01-01T00:00:00.000000000Z',
    '__invalid__'
WHERE (SELECT count(*) FROM alerts)
   != (SELECT count(*) FROM alert_evidence_provenance);

CREATE TRIGGER alert_evidence_provenance_immutable_guard
BEFORE UPDATE ON alert_evidence_provenance
BEGIN
    SELECT RAISE(ABORT, 'alert evidence provenance is immutable');
END;

CREATE INDEX model_call_dashboard_event_desc_idx
    ON model_call_dashboard_projection (event_at DESC, model_call_id DESC);
CREATE INDEX model_call_dashboard_event_asc_idx
    ON model_call_dashboard_projection (event_at ASC, model_call_id ASC);
CREATE INDEX model_call_dashboard_input_tokens_idx
    ON model_call_dashboard_projection (
        input_tokens IS NULL, input_tokens DESC, event_at DESC, model_call_id DESC
    );
CREATE INDEX model_call_dashboard_output_tokens_idx
    ON model_call_dashboard_projection (
        output_tokens IS NULL, output_tokens DESC, event_at DESC, model_call_id DESC
    );
CREATE INDEX model_call_dashboard_reasoning_tokens_idx
    ON model_call_dashboard_projection (
        reasoning_output_tokens IS NULL,
        reasoning_output_tokens DESC,
        event_at DESC,
        model_call_id DESC
    );
CREATE INDEX model_call_dashboard_conversation_idx
    ON model_call_dashboard_projection (conversation_id, event_at DESC, model_call_id DESC);
CREATE INDEX model_call_dashboard_conversation_source_idx
    ON model_call_dashboard_projection (
        conversation_source_id, event_at DESC, model_call_id DESC
    );
CREATE INDEX model_call_dashboard_thread_source_idx
    ON model_call_dashboard_projection (thread_source_id, event_at DESC, model_call_id DESC);
CREATE INDEX model_call_dashboard_model_idx
    ON model_call_dashboard_projection (model, event_at DESC, model_call_id DESC);
CREATE INDEX model_call_dashboard_client_idx
    ON model_call_dashboard_projection (client, event_at DESC, model_call_id DESC);
CREATE INDEX model_call_dashboard_temporal_idx
    ON model_call_dashboard_projection (temporal_class, event_at DESC, model_call_id DESC);
CREATE INDEX conversations_last_observed_idx
    ON conversations (last_observed_at DESC, id DESC);
CREATE INDEX conversation_source_keys_value_idx
    ON conversation_source_keys (key_value, conversation_id);
CREATE INDEX alerts_created_at_idx
    ON alerts (created_at DESC, id DESC);
CREATE INDEX alerts_model_call_created_idx
    ON alerts (model_call_id, created_at DESC, id DESC);
CREATE INDEX observations_source_call_idx
    ON model_call_observations (source_id, model_call_id);
CREATE INDEX action_outbox_state_alert_idx
    ON action_outbox (state, alert_id);
CREATE INDEX action_outbox_alert_notification_idx
    ON action_outbox (alert_id, action_type, parameter_schema_version DESC, id DESC);
