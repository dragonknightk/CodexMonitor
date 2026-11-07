-- Persist notification coalescing decisions without changing the one-alert,
-- one-action Outbox contract. Existing v6 actions remain direct audit records;
-- this migration deliberately does not infer or backfill coalescing groups.

CREATE TABLE notification_coalescing_windows (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    leader_action_id TEXT UNIQUE,
    opened_at TEXT NOT NULL,
    closes_at TEXT NOT NULL,
    FOREIGN KEY (conversation_id) REFERENCES conversations (id) ON DELETE CASCADE,
    FOREIGN KEY (leader_action_id) REFERENCES action_outbox (id) ON DELETE SET NULL,
    CHECK (
        unixepoch(opened_at) IS NOT NULL
        AND unixepoch(closes_at) IS NOT NULL
        AND unixepoch(closes_at) - unixepoch(opened_at) = 30
        AND substr(closes_at, 20) = substr(opened_at, 20)
    )
) STRICT;

CREATE TABLE notification_coalescing_members (
    action_id TEXT PRIMARY KEY,
    window_id TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('leader', 'follower')),
    joined_at TEXT NOT NULL,
    FOREIGN KEY (action_id) REFERENCES action_outbox (id) ON DELETE CASCADE,
    FOREIGN KEY (window_id)
        REFERENCES notification_coalescing_windows (id) ON DELETE CASCADE
) STRICT;

CREATE INDEX notification_coalescing_windows_conversation_interval_idx
    ON notification_coalescing_windows (conversation_id, opened_at, closes_at, id);
CREATE INDEX notification_coalescing_windows_closes_at_idx
    ON notification_coalescing_windows (closes_at, id);
CREATE INDEX notification_coalescing_members_window_joined_idx
    ON notification_coalescing_members (window_id, joined_at, action_id);
CREATE UNIQUE INDEX notification_coalescing_members_one_leader_idx
    ON notification_coalescing_members (window_id) WHERE role = 'leader';

-- A window is opened only after its delayed leader action exists. The action's
-- alert supplies the canonical conversation; neither an uncorrelated call nor a
-- historical/terminal action can become a leader.
CREATE TRIGGER notification_coalescing_windows_leader_insert_guard
BEFORE INSERT ON notification_coalescing_windows
WHEN NEW.leader_action_id IS NULL
  OR NOT EXISTS (
      SELECT 1
      FROM action_outbox AS action
      INNER JOIN alerts AS alert ON alert.id = action.alert_id
      INNER JOIN model_calls AS model_call ON model_call.id = alert.model_call_id
      WHERE action.id = NEW.leader_action_id
        AND action.action_type = 'system_notification'
        AND action.parameter_schema_version = 1
        AND action.state = 'pending'
        AND action.created_at = NEW.opened_at
        AND action.not_before = NEW.closes_at
        AND model_call.origin = 'realtime'
        AND model_call.conversation_id = NEW.conversation_id
  )
BEGIN
    SELECT RAISE(ABORT, 'invalid notification coalescing leader');
END;

-- Half-open intervals [opened_at, closes_at) may touch, but two windows for the
-- same canonical conversation may never overlap.
CREATE TRIGGER notification_coalescing_windows_overlap_insert_guard
BEFORE INSERT ON notification_coalescing_windows
WHEN EXISTS (
    SELECT 1
    FROM notification_coalescing_windows AS existing
    WHERE existing.conversation_id = NEW.conversation_id
      AND NEW.opened_at < existing.closes_at
      AND existing.opened_at < NEW.closes_at
)
BEGIN
    SELECT RAISE(ABORT, 'notification coalescing windows overlap');
END;

CREATE TRIGGER notification_coalescing_windows_identity_immutable_guard
BEFORE UPDATE OF id, conversation_id, opened_at, closes_at
ON notification_coalescing_windows
BEGIN
    SELECT RAISE(ABORT, 'notification coalescing window is immutable');
END;

-- ON DELETE SET NULL is the only permitted leader change. During that foreign-
-- key action SQLite has already removed the parent action, which distinguishes
-- retention cleanup from a direct mutation.
CREATE TRIGGER notification_coalescing_windows_leader_immutable_guard
BEFORE UPDATE OF leader_action_id ON notification_coalescing_windows
WHEN NOT (
    OLD.leader_action_id IS NOT NULL
    AND NEW.leader_action_id IS NULL
    AND NOT EXISTS (
        SELECT 1 FROM action_outbox AS action WHERE action.id = OLD.leader_action_id
    )
)
BEGIN
    SELECT RAISE(ABORT, 'notification coalescing leader is immutable');
END;

CREATE TRIGGER notification_coalescing_windows_nonempty_delete_guard
BEFORE DELETE ON notification_coalescing_windows
WHEN EXISTS (
    SELECT 1
    FROM notification_coalescing_members AS member
    WHERE member.window_id = OLD.id
)
BEGIN
    SELECT RAISE(ABORT, 'notification coalescing window still has members');
END;

-- A leader member repeats the validated delayed action at opened_at. A follower
-- is an immediately terminal coalesced action whose join time falls inside the
-- same half-open interval. Every member's alert must resolve to the window's
-- canonical conversation.
CREATE TRIGGER notification_coalescing_members_insert_guard
BEFORE INSERT ON notification_coalescing_members
WHEN NOT EXISTS (
    SELECT 1
    FROM notification_coalescing_windows AS window
    INNER JOIN action_outbox AS action ON action.id = NEW.action_id
    INNER JOIN alerts AS alert ON alert.id = action.alert_id
    INNER JOIN model_calls AS model_call ON model_call.id = alert.model_call_id
    WHERE window.id = NEW.window_id
      AND action.action_type = 'system_notification'
      AND action.parameter_schema_version = 1
      AND model_call.origin = 'realtime'
      AND model_call.conversation_id = window.conversation_id
      AND (
          (
              NEW.role = 'leader'
              AND NEW.action_id = window.leader_action_id
              AND NEW.joined_at = window.opened_at
              AND action.state = 'pending'
              AND action.created_at = window.opened_at
              AND action.not_before = window.closes_at
          )
          OR
          (
              NEW.role = 'follower'
              AND NEW.action_id <> window.leader_action_id
              AND NEW.joined_at >= window.opened_at
              AND NEW.joined_at < window.closes_at
              AND action.state = 'suppressed'
              AND action.suppression_reason = 'coalesced'
              AND action.created_at = NEW.joined_at
              AND action.not_before = NEW.joined_at
              AND action.completed_at = NEW.joined_at
              AND EXISTS (
                  SELECT 1
                  FROM notification_coalescing_members AS leader
                  WHERE leader.window_id = window.id
                    AND leader.role = 'leader'
                    AND leader.action_id = window.leader_action_id
              )
          )
      )
)
BEGIN
    SELECT RAISE(ABORT, 'invalid notification coalescing member');
END;

CREATE TRIGGER notification_coalescing_members_immutable_guard
BEFORE UPDATE ON notification_coalescing_members
BEGIN
    SELECT RAISE(ABORT, 'notification coalescing member is immutable');
END;

-- Membership is removed only as the referenced terminal action is retained out.
-- The parent action is already absent when its ON DELETE CASCADE reaches here.
CREATE TRIGGER notification_coalescing_members_retention_delete_guard
BEFORE DELETE ON notification_coalescing_members
WHEN EXISTS (
    SELECT 1 FROM action_outbox AS action WHERE action.id = OLD.action_id
)
BEGIN
    SELECT RAISE(ABORT, 'notification coalescing member requires its action');
END;

CREATE TRIGGER notification_coalescing_members_empty_window_cleanup
AFTER DELETE ON notification_coalescing_members
WHEN NOT EXISTS (
    SELECT 1
    FROM notification_coalescing_members AS remaining
    WHERE remaining.window_id = OLD.window_id
)
BEGIN
    DELETE FROM notification_coalescing_windows WHERE id = OLD.window_id;
END;
