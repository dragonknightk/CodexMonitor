use codex_monitor_core::UtcTimestamp;
use rusqlite::{Connection, OptionalExtension as _, Transaction, TransactionBehavior, params};
use time::Duration;

use super::timestamp::timestamp_text;
use super::{
    MAX_RETENTION_BATCH_SIZE, RetentionOutcome, RetentionRequest, StorageError, StorageResult,
};

const SELECT_ELIGIBLE_MODEL_CALLS: &str = "SELECT model_call.id
     FROM model_calls AS model_call
     WHERE model_call.event_at < ?1
       AND NOT EXISTS (
            SELECT 1 FROM domain_work AS work
            WHERE work.model_call_id = model_call.id
              AND work.state IN ('pending', 'started')
       )
       AND NOT EXISTS (
            SELECT 1
             FROM alerts AS alert
             JOIN action_outbox AS action ON action.alert_id = alert.id
             WHERE alert.model_call_id = model_call.id
               AND action.state IN ('pending', 'started')
       )
     ORDER BY model_call.event_at, model_call.id
     LIMIT ?2";

const DELETE_ORPHAN_TURNS: &str = "DELETE FROM turns
     WHERE id IN (
        SELECT turn.id
        FROM turns AS turn
        JOIN conversations AS conversation ON conversation.id = turn.conversation_id
        WHERE NOT EXISTS (
            SELECT 1 FROM model_calls AS model_call WHERE model_call.turn_id = turn.id
        )
          AND conversation.last_observed_at < ?1
        ORDER BY turn.id
        LIMIT ?2
     )";

const DELETE_ORPHAN_CONVERSATIONS: &str = "DELETE FROM conversations
     WHERE id IN (
        SELECT conversation.id
        FROM conversations AS conversation
        WHERE conversation.last_observed_at < ?1
          AND NOT EXISTS (
            SELECT 1 FROM turns AS turn
            WHERE turn.conversation_id = conversation.id
          )
          AND NOT EXISTS (
            SELECT 1 FROM model_calls AS model_call
            WHERE model_call.conversation_id = conversation.id
          )
        ORDER BY conversation.last_observed_at, conversation.id
        LIMIT ?2
     )";

const HAS_ELIGIBLE_ORPHANS: &str = "SELECT 1
     WHERE EXISTS (
        SELECT 1
        FROM turns AS turn
        JOIN conversations AS conversation ON conversation.id = turn.conversation_id
        WHERE NOT EXISTS (
            SELECT 1 FROM model_calls AS model_call WHERE model_call.turn_id = turn.id
        )
          AND conversation.last_observed_at < ?1
     ) OR EXISTS (
        SELECT 1 FROM conversations AS conversation
        WHERE conversation.last_observed_at < ?1
          AND NOT EXISTS (
            SELECT 1 FROM turns AS turn
            WHERE turn.conversation_id = conversation.id
          )
          AND NOT EXISTS (
            SELECT 1 FROM model_calls AS model_call
            WHERE model_call.conversation_id = conversation.id
          )
     )";

pub(crate) fn apply_retention(
    connection: &mut Connection,
    request: RetentionRequest,
) -> StorageResult<RetentionOutcome> {
    let Some(days) = request.policy.days() else {
        return Ok(RetentionOutcome::default());
    };
    let cutoff = UtcTimestamp::new(request.now.as_offset_datetime() - Duration::days(days));
    let cutoff = timestamp_text(cutoff);
    let batch_size = i64::from(request.batch_size.get());
    let batch_size_usize = usize::try_from(request.batch_size.get()).map_err(|_| {
        StorageError::InvalidRetentionBatch {
            max: MAX_RETENTION_BATCH_SIZE,
        }
    })?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (model_call_ids, more_model_calls) =
        select_model_calls(&transaction, &cutoff, batch_size, batch_size_usize)?;
    for model_call_id in &model_call_ids {
        transaction.execute("DELETE FROM model_calls WHERE id = ?1", [model_call_id])?;
    }
    let (deleted_orphan_turns, deleted_orphan_conversations, more_orphans) =
        delete_orphans(&transaction, &cutoff, batch_size)?;
    transaction.commit()?;
    Ok(RetentionOutcome {
        deleted_model_calls: bounded_count(model_call_ids.len())?,
        deleted_orphan_turns: bounded_count(deleted_orphan_turns)?,
        deleted_orphan_conversations: bounded_count(deleted_orphan_conversations)?,
        more_eligible: more_model_calls || more_orphans,
    })
}

fn select_model_calls(
    transaction: &Transaction<'_>,
    cutoff: &str,
    batch_size: i64,
    batch_size_usize: usize,
) -> StorageResult<(Vec<String>, bool)> {
    let mut statement = transaction.prepare(SELECT_ELIGIBLE_MODEL_CALLS)?;
    let mut ids = statement
        .query_map(params![cutoff, batch_size + 1], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let more_eligible = ids.len() > batch_size_usize;
    ids.truncate(batch_size_usize);
    Ok((ids, more_eligible))
}

fn delete_orphans(
    transaction: &Transaction<'_>,
    cutoff: &str,
    batch_size: i64,
) -> StorageResult<(usize, usize, bool)> {
    let deleted_turns = transaction.execute(DELETE_ORPHAN_TURNS, params![cutoff, batch_size])?;
    let deleted_conversations =
        transaction.execute(DELETE_ORPHAN_CONVERSATIONS, params![cutoff, batch_size])?;
    let more_eligible = transaction
        .query_row(HAS_ELIGIBLE_ORPHANS, [cutoff], |_| Ok(true))
        .optional()?
        .unwrap_or(false);
    Ok((deleted_turns, deleted_conversations, more_eligible))
}

fn bounded_count(value: usize) -> StorageResult<u32> {
    u32::try_from(value).map_err(|_| StorageError::InvalidRetentionBatch {
        max: MAX_RETENTION_BATCH_SIZE,
    })
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::super::migrations::open_and_migrate;
    use super::*;

    #[test]
    fn unlimited_retention_is_a_noop_without_opening_a_transaction() {
        let mut connection = Connection::open_in_memory().unwrap();
        let request = RetentionRequest::new(
            super::super::RetentionPolicy::Unlimited,
            UtcTimestamp::parse_rfc3339("2026-01-01T00:00:00Z").unwrap(),
            10,
        )
        .unwrap();

        assert_eq!(
            apply_retention(&mut connection, request).unwrap(),
            RetentionOutcome::default()
        );
    }

    #[test]
    fn orphan_cleanup_respects_the_retention_cutoff() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("orphan-cutoff.sqlite3");
        let mut connection = open_and_migrate(&database).unwrap();
        connection
            .execute_batch(
                "INSERT INTO conversations (id, first_observed_at, last_observed_at) VALUES
                    ('old-conversation',
                     '2025-01-01T00:00:00.000000000Z',
                     '2025-01-01T00:00:00.000000000Z'),
                    ('recent-conversation',
                     '2026-12-31T00:00:00.000000000Z',
                     '2026-12-31T00:00:00.000000000Z');
                 INSERT INTO turns (id, conversation_id) VALUES
                    ('old-turn', 'old-conversation'),
                    ('recent-turn', 'recent-conversation');",
            )
            .unwrap();
        let request = RetentionRequest::new(
            super::super::RetentionPolicy::Days30,
            UtcTimestamp::parse_rfc3339("2027-01-01T00:00:00Z").unwrap(),
            10,
        )
        .unwrap();

        let outcome = apply_retention(&mut connection, request).unwrap();
        assert_eq!(outcome.deleted_orphan_turns, 1);
        assert_eq!(outcome.deleted_orphan_conversations, 1);
        let remaining = connection
            .query_row(
                "SELECT
                    (SELECT group_concat(id) FROM turns),
                    (SELECT group_concat(id) FROM conversations)",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .unwrap();
        assert_eq!(
            remaining,
            ("recent-turn".to_owned(), "recent-conversation".to_owned())
        );
    }
}
