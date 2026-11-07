-- Backfill durable alert evidence and make notification threshold modes total.

-- Earlier schema accepted an empty threshold mode. Repair it to the v1 default
-- before adding guards that require exactly one of maximum-n or all-match.
UPDATE app_settings
SET notify_max_n = 1
WHERE notify_max_n IS NULL AND notify_all = 0;

UPDATE rules
SET notify_max_n = 1
WHERE notify_max_n IS NULL AND notify_all = 0;

CREATE TRIGGER rules_notification_threshold_insert_guard
BEFORE INSERT ON rules
WHEN (NEW.notify_all = 1) = (NEW.notify_max_n IS NOT NULL)
BEGIN
    SELECT RAISE(ABORT, 'invalid rule notification threshold');
END;

CREATE TRIGGER rules_notification_threshold_update_guard
BEFORE UPDATE OF notify_max_n, notify_all ON rules
WHEN (NEW.notify_all = 1) = (NEW.notify_max_n IS NOT NULL)
BEGIN
    SELECT RAISE(ABORT, 'invalid rule notification threshold');
END;

CREATE TRIGGER app_settings_notification_threshold_insert_guard
BEFORE INSERT ON app_settings
WHEN (NEW.notify_all = 1) = (NEW.notify_max_n IS NOT NULL)
BEGIN
    SELECT RAISE(ABORT, 'invalid application notification threshold');
END;

CREATE TRIGGER app_settings_notification_threshold_update_guard
BEFORE UPDATE OF notify_max_n, notify_all ON app_settings
WHEN (NEW.notify_all = 1) = (NEW.notify_max_n IS NOT NULL)
BEGIN
    SELECT RAISE(ABORT, 'invalid application notification threshold');
END;

-- Rule work completed before alert persistence was introduced will never be
-- claimed again. Its generated evaluation UUID is a stable deterministic alert
-- UUID for this one-time upgrade. No action is backfilled: old matches must not
-- enter the realtime action pipeline.
INSERT INTO alerts (
    id,
    evaluation_id,
    model_call_id,
    rule_id,
    rule_version,
    reasoning_output_tokens,
    pattern_n,
    event_at,
    source_quality,
    created_at
)
SELECT
    evaluation.id,
    evaluation.id,
    evaluation.model_call_id,
    evaluation.rule_id,
    evaluation.rule_version,
    evaluation.reasoning_output_tokens,
    evaluation.pattern_n,
    evaluation.event_at,
    evaluation.source_quality,
    evaluation.evaluated_at
FROM rule_evaluations AS evaluation
WHERE evaluation.matched = 1
  AND NOT EXISTS (
      SELECT 1 FROM alerts WHERE alerts.evaluation_id = evaluation.id
  );
