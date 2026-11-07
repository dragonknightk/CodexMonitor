//! Process-local opaque cursor storage for authenticated Dashboard queries.
//!
//! Browser-visible cursor values contain only a format version and an
//! OS-generated random token. Query state and keyset boundaries remain in this
//! bounded in-memory store and disappear when the process exits.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use codex_monitor_core::{AlertId, ConversationId, ModelCallId, TokenCount, UtcTimestamp};
use sha2::{Digest as _, Sha256};

use super::session::AuthenticatedSession;

const CURSOR_VERSION: u8 = 1;
const CURSOR_RANDOM_BYTES: usize = 32;
const CURSOR_PAYLOAD_BYTES: usize = 1 + CURSOR_RANDOM_BYTES;
const CURSOR_ENCODED_BYTES: usize = 44;

/// Maximum number of replayable cursors retained by one process.
pub(super) const MAX_CURSOR_RECORDS: usize = 4_096;

type CursorTokenDigest = [u8; 32];

/// Digest of the handler's canonical serialization of normalized filters.
pub(super) type NormalizedFilterDigest = [u8; 32];

/// Query endpoint to which an opaque cursor is bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CursorEndpoint {
    ModelCalls,
    Conversations,
    Alerts,
}

/// Total ordering to which an opaque cursor is bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CursorSort {
    EventAtDesc,
    EventAtAsc,
    InputTokensDesc,
    OutputTokensDesc,
    ReasoningTokensDesc,
    TotalTokensDesc,
    ConversationLastObservedAtDesc,
    AlertCreatedAtDesc,
}

/// Typed exclusive keyset boundary retained only in process memory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CursorBoundary {
    ModelCallEvent {
        event_at: UtcTimestamp,
        id: ModelCallId,
    },
    ModelCallTokens {
        tokens: Option<TokenCount>,
        event_at: UtcTimestamp,
        id: ModelCallId,
    },
    Conversation {
        last_observed_at: UtcTimestamp,
        id: ConversationId,
    },
    Alert {
        created_at: UtcTimestamp,
        id: AlertId,
    },
}

/// Closed cursor-issuance failures that never carry token or boundary data.
#[derive(Debug, thiserror::Error)]
pub(super) enum CursorIssueError {
    #[error("operating-system cursor entropy is unavailable")]
    Entropy(#[source] getrandom::Error),
    #[error("the process-local cursor state is unavailable")]
    StateUnavailable,
    #[error("the cursor generator produced a duplicate token")]
    TokenCollision,
    #[error("the cursor boundary does not match its endpoint and sort")]
    BoundaryMismatch,
}

/// Closed cursor-resolution failures suitable for stable API error mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(super) enum CursorResolveError {
    #[error("the cursor is malformed or no longer available")]
    Malformed,
    #[error("the cursor version is unsupported")]
    VersionUnsupported,
    #[error("the cursor belongs to another endpoint")]
    EndpointMismatch,
    #[error("the cursor belongs to another filter set")]
    FilterMismatch,
    #[error("the cursor belongs to another sort")]
    SortMismatch,
    #[error("the process-local cursor state is unavailable")]
    StateUnavailable,
}

/// Bounded, replayable owner of opaque process-local cursor records.
#[derive(Clone, Default)]
pub(super) struct CursorStore {
    state: Arc<Mutex<CursorState>>,
}

#[derive(Default)]
struct CursorState {
    records: HashMap<CursorTokenDigest, CursorRecord>,
    fifo: VecDeque<CursorTokenDigest>,
}

#[derive(Clone, Copy)]
struct CursorRecord {
    session_binding: [u8; 32],
    endpoint: CursorEndpoint,
    sort: CursorSort,
    filter_digest: NormalizedFilterDigest,
    boundary: CursorBoundary,
}

impl CursorStore {
    /// Issue a replayable opaque cursor using fresh operating-system entropy.
    pub(super) fn issue(
        &self,
        session: &AuthenticatedSession,
        endpoint: CursorEndpoint,
        sort: CursorSort,
        filter_digest: NormalizedFilterDigest,
        boundary: CursorBoundary,
    ) -> Result<String, CursorIssueError> {
        self.issue_with(session, endpoint, sort, filter_digest, boundary, |random| {
            getrandom::fill(random).map_err(CursorIssueError::Entropy)
        })
    }

    /// Resolve an opaque cursor without consuming it.
    pub(super) fn resolve(
        &self,
        encoded: &str,
        session: &AuthenticatedSession,
        endpoint: CursorEndpoint,
        sort: CursorSort,
        filter_digest: NormalizedFilterDigest,
    ) -> Result<CursorBoundary, CursorResolveError> {
        let random = decode_cursor(encoded)?;
        let token_digest = token_digest(&random);
        let state = self.lock_for_resolve()?;
        let Some(record) = state.records.get(&token_digest) else {
            return Err(CursorResolveError::Malformed);
        };

        if !fixed_time_digest_equal(&record.session_binding, &session.cursor_binding()) {
            return Err(CursorResolveError::Malformed);
        }
        if record.endpoint != endpoint {
            return Err(CursorResolveError::EndpointMismatch);
        }
        if !fixed_time_digest_equal(&record.filter_digest, &filter_digest) {
            return Err(CursorResolveError::FilterMismatch);
        }
        if record.sort != sort {
            return Err(CursorResolveError::SortMismatch);
        }
        Ok(record.boundary)
    }

    fn issue_with<F>(
        &self,
        session: &AuthenticatedSession,
        endpoint: CursorEndpoint,
        sort: CursorSort,
        filter_digest: NormalizedFilterDigest,
        boundary: CursorBoundary,
        fill_random: F,
    ) -> Result<String, CursorIssueError>
    where
        F: FnOnce(&mut [u8; CURSOR_RANDOM_BYTES]) -> Result<(), CursorIssueError>,
    {
        if !boundary_matches(endpoint, sort, boundary) {
            return Err(CursorIssueError::BoundaryMismatch);
        }

        let mut random = [0_u8; CURSOR_RANDOM_BYTES];
        fill_random(&mut random)?;
        let digest = token_digest(&random);
        let record = CursorRecord {
            session_binding: session.cursor_binding(),
            endpoint,
            sort,
            filter_digest,
            boundary,
        };

        self.lock_for_issue()?.insert(digest, record)?;

        let mut payload = [0_u8; CURSOR_PAYLOAD_BYTES];
        payload[0] = CURSOR_VERSION;
        payload[1..].copy_from_slice(&random);
        Ok(URL_SAFE_NO_PAD.encode(payload))
    }

    fn lock_for_issue(&self) -> Result<MutexGuard<'_, CursorState>, CursorIssueError> {
        self.state
            .lock()
            .map_err(|_| CursorIssueError::StateUnavailable)
    }

    fn lock_for_resolve(&self) -> Result<MutexGuard<'_, CursorState>, CursorResolveError> {
        self.state
            .lock()
            .map_err(|_| CursorResolveError::StateUnavailable)
    }
}

impl CursorState {
    fn insert(
        &mut self,
        digest: CursorTokenDigest,
        record: CursorRecord,
    ) -> Result<(), CursorIssueError> {
        if self.records.contains_key(&digest) {
            return Err(CursorIssueError::TokenCollision);
        }
        if self.records.len() >= MAX_CURSOR_RECORDS {
            let oldest = self
                .fifo
                .pop_front()
                .ok_or(CursorIssueError::StateUnavailable)?;
            if self.records.remove(&oldest).is_none() {
                return Err(CursorIssueError::StateUnavailable);
            }
        }
        self.records.insert(digest, record);
        self.fifo.push_back(digest);
        Ok(())
    }
}

fn decode_cursor(encoded: &str) -> Result<[u8; CURSOR_RANDOM_BYTES], CursorResolveError> {
    if encoded.len() != CURSOR_ENCODED_BYTES
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(CursorResolveError::Malformed);
    }

    let payload = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| CursorResolveError::Malformed)?;
    let payload: [u8; CURSOR_PAYLOAD_BYTES] = payload
        .try_into()
        .map_err(|_| CursorResolveError::Malformed)?;
    if payload[0] != CURSOR_VERSION {
        return Err(CursorResolveError::VersionUnsupported);
    }
    payload[1..]
        .try_into()
        .map_err(|_| CursorResolveError::Malformed)
}

fn token_digest(random: &[u8; CURSOR_RANDOM_BYTES]) -> CursorTokenDigest {
    Sha256::digest(random).into()
}

fn fixed_time_digest_equal(expected: &[u8; 32], candidate: &[u8; 32]) -> bool {
    expected
        .iter()
        .zip(candidate)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

const fn boundary_matches(
    endpoint: CursorEndpoint,
    sort: CursorSort,
    boundary: CursorBoundary,
) -> bool {
    matches!(
        (endpoint, sort, boundary),
        (
            CursorEndpoint::ModelCalls,
            CursorSort::EventAtDesc | CursorSort::EventAtAsc,
            CursorBoundary::ModelCallEvent { .. }
        ) | (
            CursorEndpoint::ModelCalls,
            CursorSort::InputTokensDesc
                | CursorSort::OutputTokensDesc
                | CursorSort::ReasoningTokensDesc
                | CursorSort::TotalTokensDesc,
            CursorBoundary::ModelCallTokens { .. }
        ) | (
            CursorEndpoint::Conversations,
            CursorSort::ConversationLastObservedAtDesc,
            CursorBoundary::Conversation { .. }
        ) | (
            CursorEndpoint::Alerts,
            CursorSort::AlertCreatedAtDesc,
            CursorBoundary::Alert { .. }
        )
    )
}

#[cfg(test)]
mod tests {
    use std::thread;

    use uuid::Uuid;

    use super::*;
    use crate::session::DashboardSessions;

    const FILTER_A: NormalizedFilterDigest = [0x11; 32];
    const FILTER_B: NormalizedFilterDigest = [0x22; 32];

    fn at(seconds: i64) -> UtcTimestamp {
        UtcTimestamp::from_unix_timestamp_nanos(i128::from(seconds) * 1_000_000_000).unwrap()
    }

    fn model_call_id(value: u128) -> ModelCallId {
        ModelCallId::from_uuid(Uuid::from_u128(value))
    }

    fn conversation_id(value: u128) -> ConversationId {
        ConversationId::from_uuid(Uuid::from_u128(value))
    }

    fn alert_id(value: u128) -> AlertId {
        AlertId::from_uuid(Uuid::from_u128(value))
    }

    fn authenticated_session(sessions: &DashboardSessions) -> AuthenticatedSession {
        let bootstrap = sessions.issue_bootstrap().unwrap();
        let established = sessions.consume_bootstrap(bootstrap.query_value()).unwrap();
        sessions
            .authenticate_session(established.cookie_value())
            .unwrap()
    }

    fn model_event_boundary() -> CursorBoundary {
        CursorBoundary::ModelCallEvent {
            event_at: at(1_000),
            id: model_call_id(1),
        }
    }

    fn issue_with_random(
        store: &CursorStore,
        session: &AuthenticatedSession,
        endpoint: CursorEndpoint,
        sort: CursorSort,
        filter_digest: NormalizedFilterDigest,
        boundary: CursorBoundary,
        random: [u8; CURSOR_RANDOM_BYTES],
    ) -> Result<String, CursorIssueError> {
        store.issue_with(
            session,
            endpoint,
            sort,
            filter_digest,
            boundary,
            |destination| {
                *destination = random;
                Ok(())
            },
        )
    }

    #[test]
    fn issued_cursor_resolves_repeatedly_without_exposing_boundary_bytes() {
        let sessions = DashboardSessions::default();
        let session = authenticated_session(&sessions);
        let store = CursorStore::default();
        let random = [0x5a; CURSOR_RANDOM_BYTES];
        let boundary = model_event_boundary();
        let cursor = issue_with_random(
            &store,
            &session,
            CursorEndpoint::ModelCalls,
            CursorSort::EventAtDesc,
            FILTER_A,
            boundary,
            random,
        )
        .unwrap();

        assert_eq!(cursor.len(), CURSOR_ENCODED_BYTES);
        assert!(!cursor.contains(&model_call_id(1).to_string()));
        assert!(!cursor.contains(&at(1_000).to_string()));
        let decoded = URL_SAFE_NO_PAD.decode(&cursor).unwrap();
        assert_eq!(decoded[0], CURSOR_VERSION);
        assert_eq!(&decoded[1..], &random);
        for _ in 0..2 {
            assert_eq!(
                store
                    .resolve(
                        &cursor,
                        &session,
                        CursorEndpoint::ModelCalls,
                        CursorSort::EventAtDesc,
                        FILTER_A,
                    )
                    .unwrap(),
                boundary
            );
        }
    }

    #[test]
    fn every_supported_sort_round_trips_its_typed_boundary() {
        let sessions = DashboardSessions::default();
        let session = authenticated_session(&sessions);
        let store = CursorStore::default();
        let token_boundary = CursorBoundary::ModelCallTokens {
            tokens: Some(TokenCount::new(42).unwrap()),
            event_at: at(2_000),
            id: model_call_id(2),
        };
        let cases = [
            (
                CursorEndpoint::ModelCalls,
                CursorSort::EventAtDesc,
                model_event_boundary(),
            ),
            (
                CursorEndpoint::ModelCalls,
                CursorSort::EventAtAsc,
                model_event_boundary(),
            ),
            (
                CursorEndpoint::ModelCalls,
                CursorSort::InputTokensDesc,
                token_boundary,
            ),
            (
                CursorEndpoint::ModelCalls,
                CursorSort::OutputTokensDesc,
                token_boundary,
            ),
            (
                CursorEndpoint::ModelCalls,
                CursorSort::ReasoningTokensDesc,
                token_boundary,
            ),
            (
                CursorEndpoint::ModelCalls,
                CursorSort::TotalTokensDesc,
                CursorBoundary::ModelCallTokens {
                    tokens: None,
                    event_at: at(2_000),
                    id: model_call_id(2),
                },
            ),
            (
                CursorEndpoint::Conversations,
                CursorSort::ConversationLastObservedAtDesc,
                CursorBoundary::Conversation {
                    last_observed_at: at(3_000),
                    id: conversation_id(3),
                },
            ),
            (
                CursorEndpoint::Alerts,
                CursorSort::AlertCreatedAtDesc,
                CursorBoundary::Alert {
                    created_at: at(4_000),
                    id: alert_id(4),
                },
            ),
        ];

        for (index, (endpoint, sort, boundary)) in cases.into_iter().enumerate() {
            let mut random = [0_u8; CURSOR_RANDOM_BYTES];
            random[..8].copy_from_slice(&u64::try_from(index + 1).unwrap().to_be_bytes());
            let cursor =
                issue_with_random(&store, &session, endpoint, sort, FILTER_A, boundary, random)
                    .unwrap();
            assert_eq!(
                store
                    .resolve(&cursor, &session, endpoint, sort, FILTER_A)
                    .unwrap(),
                boundary
            );
        }
    }

    #[test]
    fn malformed_tampered_unknown_and_cross_session_cursors_are_indistinguishable() {
        let sessions = DashboardSessions::default();
        let owner = authenticated_session(&sessions);
        let other = authenticated_session(&sessions);
        let store = CursorStore::default();
        let cursor = store
            .issue(
                &owner,
                CursorEndpoint::ModelCalls,
                CursorSort::EventAtDesc,
                FILTER_A,
                model_event_boundary(),
            )
            .unwrap();
        let mut tampered = cursor.clone().into_bytes();
        let last = tampered.last_mut().unwrap();
        *last = if *last == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(tampered).unwrap();
        let unknown_payload = [CURSOR_VERSION; CURSOR_PAYLOAD_BYTES];
        let unknown = URL_SAFE_NO_PAD.encode(unknown_payload);

        for candidate in [
            "short",
            "invalid=padding",
            tampered.as_str(),
            unknown.as_str(),
        ] {
            assert_eq!(
                store.resolve(
                    candidate,
                    &owner,
                    CursorEndpoint::ModelCalls,
                    CursorSort::EventAtDesc,
                    FILTER_A,
                ),
                Err(CursorResolveError::Malformed)
            );
        }
        assert_eq!(
            store.resolve(
                &cursor,
                &other,
                CursorEndpoint::ModelCalls,
                CursorSort::EventAtDesc,
                FILTER_A,
            ),
            Err(CursorResolveError::Malformed)
        );
    }

    #[test]
    fn mismatch_precedence_is_endpoint_then_filter_then_sort() {
        let sessions = DashboardSessions::default();
        let session = authenticated_session(&sessions);
        let store = CursorStore::default();
        let cursor = store
            .issue(
                &session,
                CursorEndpoint::ModelCalls,
                CursorSort::EventAtDesc,
                FILTER_A,
                model_event_boundary(),
            )
            .unwrap();

        assert_eq!(
            store.resolve(
                &cursor,
                &session,
                CursorEndpoint::Conversations,
                CursorSort::ConversationLastObservedAtDesc,
                FILTER_B,
            ),
            Err(CursorResolveError::EndpointMismatch)
        );
        assert_eq!(
            store.resolve(
                &cursor,
                &session,
                CursorEndpoint::ModelCalls,
                CursorSort::EventAtAsc,
                FILTER_B,
            ),
            Err(CursorResolveError::FilterMismatch)
        );
        assert_eq!(
            store.resolve(
                &cursor,
                &session,
                CursorEndpoint::ModelCalls,
                CursorSort::EventAtAsc,
                FILTER_A,
            ),
            Err(CursorResolveError::SortMismatch)
        );
    }

    #[test]
    fn unsupported_version_is_reported_before_store_lookup() {
        let sessions = DashboardSessions::default();
        let session = authenticated_session(&sessions);
        let store = CursorStore::default();
        let mut payload = [0x33; CURSOR_PAYLOAD_BYTES];
        payload[0] = CURSOR_VERSION + 1;
        let cursor = URL_SAFE_NO_PAD.encode(payload);

        assert_eq!(
            store.resolve(
                &cursor,
                &session,
                CursorEndpoint::ModelCalls,
                CursorSort::EventAtDesc,
                FILTER_A,
            ),
            Err(CursorResolveError::VersionUnsupported)
        );
    }

    #[test]
    fn fifo_capacity_evicts_exactly_the_oldest_cursor() {
        let sessions = DashboardSessions::default();
        let session = authenticated_session(&sessions);
        let store = CursorStore::default();
        let mut first = None;
        let mut second = None;
        let mut newest = None;

        for index in 0..=MAX_CURSOR_RECORDS {
            let mut random = [0_u8; CURSOR_RANDOM_BYTES];
            random[..8].copy_from_slice(&u64::try_from(index).unwrap().to_be_bytes());
            let cursor = issue_with_random(
                &store,
                &session,
                CursorEndpoint::ModelCalls,
                CursorSort::EventAtDesc,
                FILTER_A,
                model_event_boundary(),
                random,
            )
            .unwrap();
            if index == 0 {
                first = Some(cursor.clone());
            } else if index == 1 {
                second = Some(cursor.clone());
            }
            newest = Some(cursor);
        }

        assert_eq!(
            store.lock_for_issue().unwrap().records.len(),
            MAX_CURSOR_RECORDS
        );
        assert_eq!(
            store.resolve(
                first.as_deref().unwrap(),
                &session,
                CursorEndpoint::ModelCalls,
                CursorSort::EventAtDesc,
                FILTER_A,
            ),
            Err(CursorResolveError::Malformed)
        );
        for cursor in [second.as_deref().unwrap(), newest.as_deref().unwrap()] {
            assert!(
                store
                    .resolve(
                        cursor,
                        &session,
                        CursorEndpoint::ModelCalls,
                        CursorSort::EventAtDesc,
                        FILTER_A,
                    )
                    .is_ok()
            );
        }
    }

    #[test]
    fn entropy_collision_boundary_and_state_failures_are_closed_and_non_mutating() {
        let sessions = DashboardSessions::default();
        let session = authenticated_session(&sessions);
        let store = CursorStore::default();
        let entropy = store.issue_with(
            &session,
            CursorEndpoint::ModelCalls,
            CursorSort::EventAtDesc,
            FILTER_A,
            model_event_boundary(),
            |_| Err(CursorIssueError::Entropy(getrandom::Error::UNEXPECTED)),
        );
        assert!(matches!(entropy, Err(CursorIssueError::Entropy(_))));
        assert!(store.lock_for_issue().unwrap().records.is_empty());

        let random = [0x44; CURSOR_RANDOM_BYTES];
        let cursor = issue_with_random(
            &store,
            &session,
            CursorEndpoint::ModelCalls,
            CursorSort::EventAtDesc,
            FILTER_A,
            model_event_boundary(),
            random,
        )
        .unwrap();
        let collision = issue_with_random(
            &store,
            &session,
            CursorEndpoint::ModelCalls,
            CursorSort::EventAtDesc,
            FILTER_A,
            model_event_boundary(),
            random,
        )
        .unwrap_err();
        assert!(matches!(collision, CursorIssueError::TokenCollision));
        assert_eq!(store.lock_for_issue().unwrap().records.len(), 1);

        let boundary_mismatch = store
            .issue(
                &session,
                CursorEndpoint::Alerts,
                CursorSort::AlertCreatedAtDesc,
                FILTER_A,
                model_event_boundary(),
            )
            .unwrap_err();
        assert!(matches!(
            boundary_mismatch,
            CursorIssueError::BoundaryMismatch
        ));

        let token_text = cursor;
        let id_text = model_call_id(1).to_string();
        for error in [&collision, &boundary_mismatch] {
            let debug = format!("{error:?}");
            let display = error.to_string();
            assert!(!debug.contains(&token_text));
            assert!(!debug.contains(&id_text));
            assert!(!display.contains(&token_text));
            assert!(!display.contains(&id_text));
        }

        let poisoned = store.clone();
        let thread = thread::spawn(move || {
            let _guard = poisoned.state.lock().unwrap();
            panic!("synthetic cursor-state poison");
        });
        assert!(thread.join().is_err());
        assert_eq!(
            store.resolve(
                &token_text,
                &session,
                CursorEndpoint::ModelCalls,
                CursorSort::EventAtDesc,
                FILTER_A,
            ),
            Err(CursorResolveError::StateUnavailable)
        );
        assert!(matches!(
            store.issue(
                &session,
                CursorEndpoint::ModelCalls,
                CursorSort::EventAtDesc,
                FILTER_A,
                model_event_boundary(),
            ),
            Err(CursorIssueError::StateUnavailable)
        ));
    }
}
