//! Bounded process-local change journal for Dashboard resynchronization.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use codex_monitor_core::{AlertId, ModelCallId, SourceContractVersion, SourceId, UtcTimestamp};
use sha2::{Digest as _, Sha256};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, broadcast};

use super::StoredSettingsDocument;

const STREAM_CURSOR_VERSION: u8 = 1;
const STREAM_GENERATION_BYTES: usize = 16;
const STREAM_SEQUENCE_BYTES: usize = size_of::<u64>();
const STREAM_CURSOR_TAG_BYTES: usize = 16;
const STREAM_CURSOR_PAYLOAD_BYTES: usize =
    1 + STREAM_GENERATION_BYTES + STREAM_SEQUENCE_BYTES + STREAM_CURSOR_TAG_BYTES;
const STREAM_CURSOR_ENCODED_BYTES: usize = 55;
const STREAM_SECRET_BYTES: usize = 32;

/// Maximum number of globally ordered changes available for replay.
pub(crate) const MAX_STREAM_REPLAY_RECORDS: usize = 4_096;
/// Maximum number of changes an established subscriber may lag before overflow.
pub(crate) const MAX_STREAM_LIVE_RECORDS: usize = 256;
/// Maximum number of concurrent long-lived Dashboard streams.
pub(crate) const MAX_STREAM_SUBSCRIPTIONS: usize = 64;

/// One durable change that can be represented by a complete Dashboard DTO.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StorageChange {
    ModelCall(ModelCallId),
    SourceHealth {
        source_id: SourceId,
        source_contract_version: SourceContractVersion,
    },
    Alert(AlertId),
    Settings(StoredSettingsDocument),
    /// A committed change has no incremental v1 DTO; clients must REST-resynchronize.
    ResyncRequired,
}

/// One globally ordered, post-commit journal record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageChangeRecord {
    pub event_id: String,
    pub emitted_at: UtcTimestamp,
    pub change: StorageChange,
    sequence: u64,
}

/// Terminal control conditions sent in-band before an SSE connection closes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamControlReason {
    Overflow,
    MissedEvent,
    ServerRestart,
}

/// Result of atomically joining replay to the live stream.
pub enum StreamSubscriptionStart {
    Active(StorageChangeSubscription),
    Control(StreamControlReason),
}

/// Closed failures that are returned before an SSE response begins.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum StreamSubscribeError {
    #[error("the stream cursor is malformed")]
    Malformed,
    #[error("the stream cursor version is unsupported")]
    VersionUnsupported,
    #[error("the stream subscription capacity is exhausted")]
    Capacity,
}

/// Next item from one replay-plus-live subscription.
pub enum StreamReceive {
    Change(Arc<StorageChangeRecord>),
    Overflow,
    Closed,
}

/// Cloneable owner of one process generation's bounded change journal.
#[derive(Clone)]
pub struct StorageChangeFeed {
    inner: Arc<StorageChangeFeedInner>,
}

struct StorageChangeFeedInner {
    generation: [u8; STREAM_GENERATION_BYTES],
    secret: [u8; STREAM_SECRET_BYTES],
    state: Mutex<StorageChangeFeedState>,
    live: broadcast::Sender<Arc<StorageChangeRecord>>,
    subscriptions: Arc<Semaphore>,
    replay_capacity: usize,
}

#[derive(Default)]
struct StorageChangeFeedState {
    sequence: u64,
    journal: VecDeque<Arc<StorageChangeRecord>>,
}

/// One bounded replay followed by a bounded live broadcast receiver.
pub struct StorageChangeSubscription {
    replay: VecDeque<Arc<StorageChangeRecord>>,
    live: broadcast::Receiver<Arc<StorageChangeRecord>>,
    _permit: OwnedSemaphorePermit,
}

struct DecodedCursor {
    generation: [u8; STREAM_GENERATION_BYTES],
    sequence: u64,
    tag: [u8; STREAM_CURSOR_TAG_BYTES],
}

impl StorageChangeFeed {
    pub(crate) fn new() -> Result<Self, getrandom::Error> {
        let mut entropy = [0_u8; STREAM_GENERATION_BYTES + STREAM_SECRET_BYTES];
        getrandom::fill(&mut entropy)?;
        let mut generation = [0_u8; STREAM_GENERATION_BYTES];
        generation.copy_from_slice(&entropy[..STREAM_GENERATION_BYTES]);
        let mut secret = [0_u8; STREAM_SECRET_BYTES];
        secret.copy_from_slice(&entropy[STREAM_GENERATION_BYTES..]);
        Ok(Self::with_parts(
            generation,
            secret,
            MAX_STREAM_REPLAY_RECORDS,
            MAX_STREAM_LIVE_RECORDS,
            MAX_STREAM_SUBSCRIPTIONS,
        ))
    }

    fn with_parts(
        generation: [u8; STREAM_GENERATION_BYTES],
        secret: [u8; STREAM_SECRET_BYTES],
        replay_capacity: usize,
        live_capacity: usize,
        subscription_capacity: usize,
    ) -> Self {
        debug_assert!(replay_capacity > 0);
        debug_assert!(live_capacity > 0);
        debug_assert!(subscription_capacity > 0);
        let (live, _) = broadcast::channel(live_capacity);
        Self {
            inner: Arc::new(StorageChangeFeedInner {
                generation,
                secret,
                state: Mutex::new(StorageChangeFeedState::default()),
                live,
                subscriptions: Arc::new(Semaphore::new(subscription_capacity)),
                replay_capacity,
            }),
        }
    }

    /// Publish one change after its `SQLite` transaction has committed.
    pub(crate) fn publish(&self, change: StorageChange) {
        let mut state = self.lock_state();
        state.sequence = state
            .sequence
            .checked_add(1)
            .expect("Dashboard stream sequence exhausted");
        let record = Arc::new(StorageChangeRecord {
            event_id: self.encode_cursor(state.sequence),
            emitted_at: UtcTimestamp::now(),
            change,
            sequence: state.sequence,
        });
        state.journal.push_back(Arc::clone(&record));
        if state.journal.len() > self.inner.replay_capacity {
            state.journal.pop_front();
        }
        let _ = self.inner.live.send(record);
    }

    /// Return the opaque exclusive replay boundary at the current high-water mark.
    pub fn current_cursor(&self) -> String {
        self.encode_cursor(self.lock_state().sequence)
    }

    /// Atomically capture replay records and register for all later live records.
    ///
    /// # Errors
    ///
    /// 恢复游标无效、版本不受支持或进程内订阅容量耗尽时返回错误。
    pub fn subscribe(
        &self,
        after_event_id: Option<&str>,
    ) -> Result<StreamSubscriptionStart, StreamSubscribeError> {
        let decoded = after_event_id.map(decode_cursor).transpose()?;
        if decoded
            .as_ref()
            .is_some_and(|cursor| cursor.generation != self.inner.generation)
        {
            return Ok(StreamSubscriptionStart::Control(
                StreamControlReason::ServerRestart,
            ));
        }
        if let Some(cursor) = decoded.as_ref()
            && !fixed_time_equal(&cursor.tag, &self.cursor_tag(cursor.sequence))
        {
            return Err(StreamSubscribeError::Malformed);
        }

        let permit = Arc::clone(&self.inner.subscriptions)
            .try_acquire_owned()
            .map_err(|_| StreamSubscribeError::Capacity)?;
        let state = self.lock_state();
        let after_sequence = decoded
            .as_ref()
            .map_or(state.sequence, |cursor| cursor.sequence);
        if after_sequence > state.sequence {
            return Err(StreamSubscribeError::Malformed);
        }
        if let Some(oldest) = state.journal.front()
            && after_sequence.saturating_add(1) < oldest.sequence
        {
            return Ok(StreamSubscriptionStart::Control(
                StreamControlReason::MissedEvent,
            ));
        }
        if state.journal.iter().any(|record| {
            record.sequence > after_sequence
                && matches!(&record.change, StorageChange::ResyncRequired)
        }) {
            return Ok(StreamSubscriptionStart::Control(
                StreamControlReason::MissedEvent,
            ));
        }
        let live = self.inner.live.subscribe();
        let replay = state
            .journal
            .iter()
            .filter(|record| record.sequence > after_sequence)
            .cloned()
            .collect();
        drop(state);
        Ok(StreamSubscriptionStart::Active(StorageChangeSubscription {
            replay,
            live,
            _permit: permit,
        }))
    }

    fn encode_cursor(&self, sequence: u64) -> String {
        let mut payload = [0_u8; STREAM_CURSOR_PAYLOAD_BYTES];
        payload[0] = STREAM_CURSOR_VERSION;
        payload[1..=STREAM_GENERATION_BYTES].copy_from_slice(&self.inner.generation);
        let sequence_start = 1 + STREAM_GENERATION_BYTES;
        let tag_start = sequence_start + STREAM_SEQUENCE_BYTES;
        payload[sequence_start..tag_start].copy_from_slice(&sequence.to_be_bytes());
        payload[tag_start..].copy_from_slice(&self.cursor_tag(sequence));
        URL_SAFE_NO_PAD.encode(payload)
    }

    fn cursor_tag(&self, sequence: u64) -> [u8; STREAM_CURSOR_TAG_BYTES] {
        let mut digest = Sha256::new();
        digest.update(self.inner.secret);
        digest.update([STREAM_CURSOR_VERSION]);
        digest.update(self.inner.generation);
        digest.update(sequence.to_be_bytes());
        digest.update(self.inner.secret);
        let digest = digest.finalize();
        let mut tag = [0_u8; STREAM_CURSOR_TAG_BYTES];
        tag.copy_from_slice(&digest[..STREAM_CURSOR_TAG_BYTES]);
        tag
    }

    fn lock_state(&self) -> MutexGuard<'_, StorageChangeFeedState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl StorageChangeSubscription {
    pub async fn recv(&mut self) -> StreamReceive {
        if let Some(record) = self.replay.pop_front() {
            return StreamReceive::Change(record);
        }
        match self.live.recv().await {
            Ok(record) => StreamReceive::Change(record),
            Err(broadcast::error::RecvError::Lagged(_)) => StreamReceive::Overflow,
            Err(broadcast::error::RecvError::Closed) => StreamReceive::Closed,
        }
    }
}

fn decode_cursor(encoded: &str) -> Result<DecodedCursor, StreamSubscribeError> {
    if encoded.len() != STREAM_CURSOR_ENCODED_BYTES {
        return Err(StreamSubscribeError::Malformed);
    }
    let payload = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| StreamSubscribeError::Malformed)?;
    if payload.len() != STREAM_CURSOR_PAYLOAD_BYTES {
        return Err(StreamSubscribeError::Malformed);
    }
    if payload[0] != STREAM_CURSOR_VERSION {
        return Err(StreamSubscribeError::VersionUnsupported);
    }
    let mut generation = [0_u8; STREAM_GENERATION_BYTES];
    generation.copy_from_slice(&payload[1..=STREAM_GENERATION_BYTES]);
    let sequence_start = 1 + STREAM_GENERATION_BYTES;
    let tag_start = sequence_start + STREAM_SEQUENCE_BYTES;
    let sequence = u64::from_be_bytes(
        payload[sequence_start..tag_start]
            .try_into()
            .expect("stream cursor sequence has a fixed width"),
    );
    let mut tag = [0_u8; STREAM_CURSOR_TAG_BYTES];
    tag.copy_from_slice(&payload[tag_start..]);
    Ok(DecodedCursor {
        generation,
        sequence,
        tag,
    })
}

fn fixed_time_equal(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            })
            == 0
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    use super::*;

    fn feed(replay: usize, live: usize, subscriptions: usize) -> StorageChangeFeed {
        StorageChangeFeed::with_parts([1; 16], [2; 32], replay, live, subscriptions)
    }

    #[tokio::test]
    async fn replay_is_exclusive_and_joins_live_without_a_gap() {
        let feed = feed(4, 4, 2);
        let boundary = feed.current_cursor();
        let first = ModelCallId::new();
        let second = ModelCallId::new();
        feed.publish(StorageChange::ModelCall(first));
        let StreamSubscriptionStart::Active(mut subscription) =
            feed.subscribe(Some(&boundary)).unwrap()
        else {
            panic!("expected active subscription");
        };
        feed.publish(StorageChange::ModelCall(second));

        let StreamReceive::Change(replayed) = subscription.recv().await else {
            panic!("expected replayed record");
        };
        let StreamReceive::Change(live) = subscription.recv().await else {
            panic!("expected live record");
        };
        assert_eq!(replayed.change, StorageChange::ModelCall(first));
        assert_eq!(live.change, StorageChange::ModelCall(second));
    }

    #[tokio::test]
    async fn concurrent_subscribe_and_publish_deliver_exactly_once() {
        for _ in 0..64 {
            let feed = feed(4, 4, 2);
            let boundary = feed.current_cursor();
            let expected = ModelCallId::new();
            let barrier = Arc::new(Barrier::new(3));
            let subscriber_feed = feed.clone();
            let subscriber_barrier = Arc::clone(&barrier);
            let subscriber = tokio::task::spawn_blocking(move || {
                subscriber_barrier.wait();
                subscriber_feed.subscribe(Some(&boundary)).unwrap()
            });
            let publisher_feed = feed.clone();
            let publisher_barrier = Arc::clone(&barrier);
            let publisher = tokio::task::spawn_blocking(move || {
                publisher_barrier.wait();
                publisher_feed.publish(StorageChange::ModelCall(expected));
            });
            barrier.wait();
            let StreamSubscriptionStart::Active(mut subscription) = subscriber.await.unwrap()
            else {
                panic!("concurrent current-generation subscription must stay active");
            };
            publisher.await.unwrap();
            let StreamReceive::Change(record) = subscription.recv().await else {
                panic!("concurrent publication must be replayed or delivered live");
            };
            assert_eq!(record.change, StorageChange::ModelCall(expected));
            assert!(
                tokio::time::timeout(Duration::from_millis(1), subscription.recv())
                    .await
                    .is_err(),
                "replay/live handoff must not duplicate a record"
            );
        }
    }

    #[tokio::test]
    async fn missing_cursor_starts_after_the_current_high_water_mark() {
        let feed = feed(4, 4, 2);
        feed.publish(StorageChange::ModelCall(ModelCallId::new()));
        let StreamSubscriptionStart::Active(mut subscription) = feed.subscribe(None).unwrap()
        else {
            panic!("cursorless subscription must be active");
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(1), subscription.recv())
                .await
                .is_err()
        );

        let expected = ModelCallId::new();
        feed.publish(StorageChange::ModelCall(expected));
        let StreamReceive::Change(record) = subscription.recv().await else {
            panic!("cursorless subscription must receive later live records");
        };
        assert_eq!(record.change, StorageChange::ModelCall(expected));
    }

    #[test]
    fn an_expired_current_generation_cursor_requires_resynchronization() {
        let feed = feed(2, 4, 2);
        let boundary = feed.current_cursor();
        for _ in 0..3 {
            feed.publish(StorageChange::ResyncRequired);
        }

        assert!(matches!(
            feed.subscribe(Some(&boundary)).unwrap(),
            StreamSubscriptionStart::Control(StreamControlReason::MissedEvent)
        ));
    }

    #[tokio::test]
    async fn replay_boundary_immediately_before_oldest_is_complete() {
        let feed = feed(2, 4, 2);
        let too_old = feed.current_cursor();
        feed.publish(StorageChange::ModelCall(ModelCallId::new()));
        let exact_boundary = feed.current_cursor();
        let expected = [ModelCallId::new(), ModelCallId::new()];
        for id in expected {
            feed.publish(StorageChange::ModelCall(id));
        }

        let StreamSubscriptionStart::Active(mut subscription) =
            feed.subscribe(Some(&exact_boundary)).unwrap()
        else {
            panic!("the cursor immediately before oldest must remain replayable");
        };
        for id in expected {
            let StreamReceive::Change(record) = subscription.recv().await else {
                panic!("the complete lower-bound replay must contain both records");
            };
            assert_eq!(record.change, StorageChange::ModelCall(id));
        }
        assert!(matches!(
            feed.subscribe(Some(&too_old)).unwrap(),
            StreamSubscriptionStart::Control(StreamControlReason::MissedEvent)
        ));
    }

    #[test]
    fn replay_never_returns_a_partial_prefix_before_a_resync_marker() {
        let feed = feed(4, 4, 2);
        let boundary = feed.current_cursor();
        feed.publish(StorageChange::ModelCall(ModelCallId::new()));
        feed.publish(StorageChange::ResyncRequired);

        assert!(matches!(
            feed.subscribe(Some(&boundary)).unwrap(),
            StreamSubscriptionStart::Control(StreamControlReason::MissedEvent)
        ));
    }

    #[tokio::test]
    async fn a_lagged_live_receiver_reports_overflow() {
        let feed = feed(8, 2, 2);
        let StreamSubscriptionStart::Active(mut subscription) = feed.subscribe(None).unwrap()
        else {
            panic!("expected active subscription");
        };
        for _ in 0..3 {
            feed.publish(StorageChange::ResyncRequired);
        }

        assert!(matches!(subscription.recv().await, StreamReceive::Overflow));
    }

    #[test]
    fn another_process_generation_reports_server_restart() {
        let previous = StorageChangeFeed::with_parts([3; 16], [4; 32], 2, 2, 2);
        let current = feed(2, 2, 2);

        assert!(matches!(
            current.subscribe(Some(&previous.current_cursor())).unwrap(),
            StreamSubscriptionStart::Control(StreamControlReason::ServerRestart)
        ));
    }

    #[test]
    fn subscription_slots_are_released_on_drop() {
        let feed = feed(2, 2, 1);
        let StreamSubscriptionStart::Active(subscription) = feed.subscribe(None).unwrap() else {
            panic!("expected active subscription");
        };
        assert!(matches!(
            feed.subscribe(None),
            Err(StreamSubscribeError::Capacity)
        ));
        drop(subscription);
        assert!(matches!(
            feed.subscribe(None),
            Ok(StreamSubscriptionStart::Active(_))
        ));
    }

    #[test]
    fn cursor_format_version_and_authentication_are_closed() {
        let feed = feed(2, 2, 1);
        assert!(matches!(
            feed.subscribe(Some("not-a-cursor")),
            Err(StreamSubscribeError::Malformed)
        ));

        let mut payload = URL_SAFE_NO_PAD.decode(feed.current_cursor()).unwrap();
        payload[0] = 2;
        let unsupported = URL_SAFE_NO_PAD.encode(&payload);
        assert!(matches!(
            feed.subscribe(Some(&unsupported)),
            Err(StreamSubscribeError::VersionUnsupported)
        ));

        payload[0] = STREAM_CURSOR_VERSION;
        let tag_index = payload.len() - 1;
        payload[tag_index] ^= 1;
        let forged = URL_SAFE_NO_PAD.encode(payload);
        assert!(matches!(
            feed.subscribe(Some(&forged)),
            Err(StreamSubscribeError::Malformed)
        ));
    }
}
