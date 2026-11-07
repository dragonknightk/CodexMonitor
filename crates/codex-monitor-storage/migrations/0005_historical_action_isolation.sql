-- Make the historical/realtime boundary durable and fail closed.

-- Calls written before durable rule work existed have no work or evaluation
-- identity. Classify that upgrade-only population as historical so startup does
-- not reinterpret old rows as new realtime events.
UPDATE model_calls AS model_call
SET origin = 'historical'
WHERE model_call.origin = 'realtime'
  AND NOT EXISTS (
      SELECT 1 FROM domain_work AS work
      WHERE work.model_call_id = model_call.id
  )
  AND NOT EXISTS (
      SELECT 1 FROM rule_evaluations AS evaluation
      WHERE evaluation.model_call_id = model_call.id
  );

-- Fail closed if an earlier development build left historical action work.
-- Started attempts become indeterminate and are never retried; their parent
-- actions become terminal suppressed audit records.
UPDATE action_attempts AS attempt
SET state = 'indeterminate',
    completed_at = COALESCE(attempt.completed_at, attempt.started_at)
WHERE attempt.state = 'started'
  AND EXISTS (
      SELECT 1
      FROM action_outbox AS action
      INNER JOIN alerts AS alert ON alert.id = action.alert_id
      INNER JOIN model_calls AS model_call ON model_call.id = alert.model_call_id
      WHERE action.id = attempt.action_id
        AND model_call.origin = 'historical'
  );

UPDATE action_outbox AS action
SET state = 'suppressed',
    lease_owner = NULL,
    lease_expires_at = NULL,
    completed_at = COALESCE(action.completed_at, action.started_at, action.created_at),
    last_error_code = NULL
WHERE action.state IN ('pending', 'started')
  AND EXISTS (
      SELECT 1
      FROM alerts AS alert
      INNER JOIN model_calls AS model_call ON model_call.id = alert.model_call_id
      WHERE alert.id = action.alert_id
        AND model_call.origin = 'historical'
  );

DELETE FROM domain_work
WHERE EXISTS (
    SELECT 1 FROM model_calls
    WHERE model_calls.id = domain_work.model_call_id
      AND model_calls.origin = 'historical'
);

CREATE TRIGGER model_calls_origin_immutable_guard
BEFORE UPDATE OF origin ON model_calls
WHEN NEW.origin <> OLD.origin
BEGIN
    SELECT RAISE(ABORT, 'model call origin is immutable');
END;

CREATE TRIGGER domain_work_historical_insert_guard
BEFORE INSERT ON domain_work
WHEN EXISTS (
    SELECT 1 FROM model_calls
    WHERE model_calls.id = NEW.model_call_id
      AND model_calls.origin = 'historical'
)
BEGIN
    SELECT RAISE(ABORT, 'historical model call cannot enter realtime work');
END;

CREATE TRIGGER domain_work_historical_update_guard
BEFORE UPDATE OF model_call_id, work_kind ON domain_work
WHEN EXISTS (
    SELECT 1 FROM model_calls
    WHERE model_calls.id = NEW.model_call_id
      AND model_calls.origin = 'historical'
)
BEGIN
    SELECT RAISE(ABORT, 'historical model call cannot enter realtime work');
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
