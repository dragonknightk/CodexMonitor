use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration as MonotonicDuration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use codex_monitor_core::UtcTimestamp;
use sha2::{Digest as _, Sha256};
use time::Duration;
use tokio::sync::watch;

pub const BOOTSTRAP_TOKEN_TTL: Duration = Duration::seconds(60);
pub const SESSION_TTL: Duration = Duration::hours(12);
pub const MAX_PENDING_BOOTSTRAP_TOKENS: usize = 64;
pub const MAX_ACTIVE_SESSIONS: usize = 128;

const SECRET_BYTES: usize = 32;
const MIN_CREDENTIAL_LENGTH: usize = 32;
const MAX_CREDENTIAL_LENGTH: usize = 256;

type CredentialDigest = [u8; 32];

/// Process-local owner of one-time bootstrap credentials and browser sessions.
#[derive(Clone, Default)]
pub struct DashboardSessions {
    state: Arc<Mutex<SessionState>>,
}

/// One-time query credential returned only to the resident host.
pub struct BootstrapToken {
    query_value: String,
    expires_at: UtcTimestamp,
}

/// Session metadata attached to an authenticated request.
#[derive(Clone)]
pub struct AuthenticatedSession {
    csrf_proof: String,
    csrf_digest: CredentialDigest,
    expires_at: UtcTimestamp,
    expires_at_monotonic: Instant,
    revocation: watch::Receiver<bool>,
}

/// Unforgeable request extension produced only after session, Origin and CSRF
/// validation have all succeeded for one state-changing request.
#[derive(Clone, Copy, Debug)]
pub struct AuthorizedStateChange {
    _private: (),
}

pub(super) struct EstablishedSession {
    cookie_value: String,
}

#[derive(Default)]
struct SessionState {
    pending: HashMap<CredentialDigest, PendingBootstrap>,
    sessions: HashMap<CredentialDigest, SessionRecord>,
}

struct PendingBootstrap {
    expires_at_monotonic: Instant,
    session_digest: CredentialDigest,
    session_value: String,
    csrf_proof: String,
    csrf_digest: CredentialDigest,
}

struct SessionRecord {
    csrf_proof: String,
    csrf_digest: CredentialDigest,
    expires_at: UtcTimestamp,
    expires_at_monotonic: Instant,
    revocation: watch::Sender<bool>,
}

#[derive(Clone, Copy)]
struct ClockSnapshot {
    utc: UtcTimestamp,
    monotonic: Instant,
}

/// Closed internal failures that never include credential material.
#[derive(Debug, thiserror::Error)]
pub enum DashboardSessionError {
    #[error("operating-system credential entropy is unavailable")]
    Entropy(#[source] getrandom::Error),
    #[error("a credential lifetime exceeds the supported clock range")]
    ClockOverflow,
    #[error("the process-local session state is unavailable")]
    StateUnavailable,
    #[error("the credential generator produced a duplicate secret")]
    CredentialCollision,
}

/// Result of validating and consuming a one-time bootstrap credential.
#[derive(Debug, thiserror::Error)]
pub enum BootstrapConsumeError {
    #[error("the bootstrap credential is invalid")]
    Invalid,
    #[error(transparent)]
    Internal(#[from] DashboardSessionError),
}

/// Result of validating a process-local session credential.
#[derive(Debug, thiserror::Error)]
pub enum SessionAuthenticationError {
    #[error("the session credential is not current")]
    Unauthorized,
    #[error(transparent)]
    Internal(#[from] DashboardSessionError),
}

impl fmt::Debug for BootstrapToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BootstrapToken")
            .field("query_value", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl fmt::Debug for AuthenticatedSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthenticatedSession")
            .field("csrf_proof", &"<redacted>")
            .field("csrf_digest", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("expires_at_monotonic", &"<redacted>")
            .field("revocation", &"<redacted>")
            .finish()
    }
}

impl BootstrapToken {
    /// Return the opaque value for the local bootstrap URL query parameter.
    pub fn query_value(&self) -> &str {
        &self.query_value
    }

    /// Return the display deadline corresponding to the monotonic token TTL.
    pub const fn expires_at(&self) -> UtcTimestamp {
        self.expires_at
    }
}

impl AuthenticatedSession {
    /// Return the proof required by authenticated state-changing requests.
    pub fn csrf_proof(&self) -> &str {
        &self.csrf_proof
    }

    /// Return the fixed absolute session deadline exposed by the local API.
    pub const fn expires_at(&self) -> UtcTimestamp {
        self.expires_at
    }

    /// Return a process-local binding for privacy-safe in-memory cursor records.
    pub(super) const fn cursor_binding(&self) -> [u8; 32] {
        self.csrf_digest
    }

    pub(super) const fn monotonic_deadline(&self) -> Instant {
        self.expires_at_monotonic
    }

    pub(super) fn revocation(&self) -> watch::Receiver<bool> {
        self.revocation.clone()
    }

    pub(super) fn authorize_state_change(&self, candidate: &str) -> Option<AuthorizedStateChange> {
        if !is_credential(candidate) {
            return None;
        }
        fixed_time_digest_equal(&self.csrf_digest, &credential_digest(candidate))
            .then_some(AuthorizedStateChange { _private: () })
    }
}

impl EstablishedSession {
    pub(super) fn cookie_value(&self) -> &str {
        &self.cookie_value
    }
}

impl DashboardSessions {
    /// Create a short-lived one-time bootstrap credential from OS entropy.
    ///
    /// The future session ID and CSRF proof are generated at the same time, so
    /// consuming the token needs no fallible entropy operation and can perform
    /// its entire state transition under one lock.
    ///
    /// # Errors
    ///
    /// Returns a closed error if entropy, clock arithmetic, or in-memory state
    /// is unavailable.
    pub fn issue_bootstrap(&self) -> Result<BootstrapToken, DashboardSessionError> {
        self.issue_bootstrap_with(ClockSnapshot::now(), GeneratedSecrets::new)
    }

    fn issue_bootstrap_with<F>(
        &self,
        now: ClockSnapshot,
        generate: F,
    ) -> Result<BootstrapToken, DashboardSessionError>
    where
        F: FnOnce() -> Result<GeneratedSecrets, DashboardSessionError>,
    {
        self.issue_bootstrap_at(now, generate()?)
    }

    fn issue_bootstrap_at(
        &self,
        now: ClockSnapshot,
        secrets: GeneratedSecrets,
    ) -> Result<BootstrapToken, DashboardSessionError> {
        let expires_at = utc_expiration(now.utc, BOOTSTRAP_TOKEN_TTL)?;
        let expires_at_monotonic = monotonic_expiration(now.monotonic, BOOTSTRAP_TOKEN_TTL)?;
        let token_digest = credential_digest(&secrets.bootstrap);
        let session_digest = credential_digest(&secrets.session);
        let csrf_digest = credential_digest(&secrets.csrf);

        let mut state = self.lock_state()?;
        state.prune(now.monotonic);
        if token_digest == session_digest
            || token_digest == csrf_digest
            || session_digest == csrf_digest
            || state.credential_digest_exists(&token_digest)
            || state.credential_digest_exists(&session_digest)
            || state.credential_digest_exists(&csrf_digest)
        {
            return Err(DashboardSessionError::CredentialCollision);
        }
        state.evict_pending_if_full();
        state.pending.insert(
            token_digest,
            PendingBootstrap {
                expires_at_monotonic,
                session_digest,
                session_value: secrets.session,
                csrf_proof: secrets.csrf,
                csrf_digest,
            },
        );

        Ok(BootstrapToken {
            query_value: secrets.bootstrap,
            expires_at,
        })
    }

    pub(super) fn consume_bootstrap(
        &self,
        token: &str,
    ) -> Result<EstablishedSession, BootstrapConsumeError> {
        self.consume_bootstrap_at(token, ClockSnapshot::now())
    }

    fn consume_bootstrap_at(
        &self,
        token: &str,
        now: ClockSnapshot,
    ) -> Result<EstablishedSession, BootstrapConsumeError> {
        if !is_credential(token) {
            return Err(BootstrapConsumeError::Invalid);
        }
        let session_expires_at = utc_expiration(now.utc, SESSION_TTL)?;
        let session_expires_at_monotonic = monotonic_expiration(now.monotonic, SESSION_TTL)?;
        let token_digest = credential_digest(token);

        let mut state = self.lock_state()?;
        state.prune(now.monotonic);
        let Some(pending) = state.pending.remove(&token_digest) else {
            return Err(BootstrapConsumeError::Invalid);
        };

        state.evict_session_if_full();
        let (revocation, _) = watch::channel(false);
        state.sessions.insert(
            pending.session_digest,
            SessionRecord {
                csrf_proof: pending.csrf_proof,
                csrf_digest: pending.csrf_digest,
                expires_at: session_expires_at,
                expires_at_monotonic: session_expires_at_monotonic,
                revocation,
            },
        );

        Ok(EstablishedSession {
            cookie_value: pending.session_value,
        })
    }

    /// Validate a raw cookie value against the current process-local sessions.
    /// Reading a session never extends its absolute deadline.
    ///
    /// # Errors
    ///
    /// Returns [`SessionAuthenticationError::Unauthorized`] for malformed,
    /// unknown, expired, evicted, or previous-process credentials.
    pub fn authenticate_session(
        &self,
        cookie_value: &str,
    ) -> Result<AuthenticatedSession, SessionAuthenticationError> {
        self.authenticate_session_at(cookie_value, ClockSnapshot::now())
    }

    /// Revalidate a long-lived authenticated stream against expiry and eviction.
    pub(super) fn is_current_session(
        &self,
        authenticated: &AuthenticatedSession,
    ) -> Result<bool, DashboardSessionError> {
        let now = ClockSnapshot::now();
        let mut state = self.lock_state()?;
        state.prune(now.monotonic);
        Ok(state.sessions.values().any(|session| {
            session.expires_at == authenticated.expires_at
                && fixed_time_digest_equal(&session.csrf_digest, &authenticated.csrf_digest)
        }))
    }

    fn authenticate_session_at(
        &self,
        cookie_value: &str,
        now: ClockSnapshot,
    ) -> Result<AuthenticatedSession, SessionAuthenticationError> {
        if !is_credential(cookie_value) {
            return Err(SessionAuthenticationError::Unauthorized);
        }
        let digest = credential_digest(cookie_value);
        let mut state = self.lock_state()?;
        state.prune(now.monotonic);
        let Some(session) = state.sessions.get(&digest) else {
            return Err(SessionAuthenticationError::Unauthorized);
        };
        Ok(AuthenticatedSession {
            csrf_proof: session.csrf_proof.clone(),
            csrf_digest: session.csrf_digest,
            expires_at: session.expires_at,
            expires_at_monotonic: session.expires_at_monotonic,
            revocation: session.revocation.subscribe(),
        })
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, SessionState>, DashboardSessionError> {
        self.state
            .lock()
            .map_err(|_| DashboardSessionError::StateUnavailable)
    }
}

impl ClockSnapshot {
    fn now() -> Self {
        Self {
            utc: UtcTimestamp::now(),
            monotonic: Instant::now(),
        }
    }
}

struct GeneratedSecrets {
    bootstrap: String,
    session: String,
    csrf: String,
}

impl GeneratedSecrets {
    fn new() -> Result<Self, DashboardSessionError> {
        Ok(Self {
            bootstrap: generate_secret()?,
            session: generate_secret()?,
            csrf: generate_secret()?,
        })
    }

    #[cfg(test)]
    fn fixed(bootstrap: char, session: char, csrf: char) -> Self {
        Self {
            bootstrap: fixed_secret(bootstrap),
            session: fixed_secret(session),
            csrf: fixed_secret(csrf),
        }
    }
}

impl SessionState {
    fn prune(&mut self, now: Instant) {
        self.pending
            .retain(|_, pending| now < pending.expires_at_monotonic);
        self.sessions.retain(|_, session| {
            let current = now < session.expires_at_monotonic;
            if !current {
                session.revoke();
            }
            current
        });
    }

    fn credential_digest_exists(&self, digest: &CredentialDigest) -> bool {
        self.pending.contains_key(digest)
            || self.sessions.contains_key(digest)
            || self
                .pending
                .values()
                .any(|pending| pending.session_digest == *digest || pending.csrf_digest == *digest)
            || self
                .sessions
                .values()
                .any(|session| session.csrf_digest == *digest)
    }

    fn evict_pending_if_full(&mut self) {
        if self.pending.len() < MAX_PENDING_BOOTSTRAP_TOKENS {
            return;
        }
        if let Some(digest) = self
            .pending
            .iter()
            .min_by_key(|(digest, pending)| (pending.expires_at_monotonic, **digest))
            .map(|(digest, _)| *digest)
        {
            self.pending.remove(&digest);
        }
    }

    fn evict_session_if_full(&mut self) {
        if self.sessions.len() < MAX_ACTIVE_SESSIONS {
            return;
        }
        if let Some(digest) = self
            .sessions
            .iter()
            .min_by_key(|(digest, session)| (session.expires_at_monotonic, **digest))
            .map(|(digest, _)| *digest)
            && let Some(session) = self.sessions.remove(&digest)
        {
            session.revoke();
        }
    }
}

impl SessionRecord {
    fn revoke(&self) {
        self.revocation.send_replace(true);
    }
}

fn generate_secret() -> Result<String, DashboardSessionError> {
    let mut bytes = [0_u8; SECRET_BYTES];
    getrandom::fill(&mut bytes).map_err(DashboardSessionError::Entropy)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn credential_digest(value: &str) -> CredentialDigest {
    Sha256::digest(value.as_bytes()).into()
}

fn is_credential(value: &str) -> bool {
    (MIN_CREDENTIAL_LENGTH..=MAX_CREDENTIAL_LENGTH).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn fixed_time_digest_equal(expected: &CredentialDigest, candidate: &CredentialDigest) -> bool {
    expected
        .iter()
        .zip(candidate)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn utc_expiration(now: UtcTimestamp, ttl: Duration) -> Result<UtcTimestamp, DashboardSessionError> {
    now.as_offset_datetime()
        .checked_add(ttl)
        .map(UtcTimestamp::new)
        .ok_or(DashboardSessionError::ClockOverflow)
}

fn monotonic_expiration(now: Instant, ttl: Duration) -> Result<Instant, DashboardSessionError> {
    let seconds =
        u64::try_from(ttl.whole_seconds()).map_err(|_| DashboardSessionError::ClockOverflow)?;
    now.checked_add(MonotonicDuration::from_secs(seconds))
        .ok_or(DashboardSessionError::ClockOverflow)
}

#[cfg(test)]
fn fixed_secret(character: char) -> String {
    std::iter::repeat_n(character, 43).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Barrier;
    use std::thread;

    use super::*;

    fn at(seconds: i64) -> UtcTimestamp {
        UtcTimestamp::from_unix_timestamp_nanos(i128::from(seconds) * 1_000_000_000).unwrap()
    }

    fn clock(utc_seconds: i64, monotonic: Instant) -> ClockSnapshot {
        ClockSnapshot {
            utc: at(utc_seconds),
            monotonic,
        }
    }

    fn indexed_secret(prefix: char, index: usize) -> String {
        format!("{prefix}{index:0>42}")
    }

    fn indexed_secrets(index: usize) -> GeneratedSecrets {
        GeneratedSecrets {
            bootstrap: indexed_secret('B', index),
            session: indexed_secret('S', index),
            csrf: indexed_secret('C', index),
        }
    }

    #[test]
    fn generated_credentials_are_independent_url_safe_256_bit_values() {
        let first = GeneratedSecrets::new().unwrap();
        let second = GeneratedSecrets::new().unwrap();
        let values = [
            first.bootstrap,
            first.session,
            first.csrf,
            second.bootstrap,
            second.session,
            second.csrf,
        ];

        assert_eq!(values.iter().collect::<HashSet<_>>().len(), values.len());
        for value in values {
            assert_eq!(value.len(), 43);
            assert!(is_credential(&value));
            assert!(!value.contains('='));
        }
    }

    #[test]
    fn debug_output_never_contains_bootstrap_or_csrf_secrets() {
        let sessions = DashboardSessions::default();
        let token = sessions.issue_bootstrap().unwrap();
        let raw_token = token.query_value().to_owned();
        let established = sessions.consume_bootstrap(&raw_token).unwrap();
        let authenticated = sessions
            .authenticate_session(established.cookie_value())
            .unwrap();
        let csrf = authenticated.csrf_proof().to_owned();

        let token_debug = format!("{token:?}");
        let session_debug = format!("{authenticated:?}");
        assert!(!token_debug.contains(&raw_token));
        assert!(!session_debug.contains(&csrf));
        assert!(token_debug.contains("<redacted>"));
        assert!(session_debug.contains("<redacted>"));
    }

    #[test]
    fn entropy_failure_and_secret_collision_leave_state_unchanged() {
        let sessions = DashboardSessions::default();
        let now = ClockSnapshot::now();
        let failed = sessions.issue_bootstrap_with(now, || {
            Err(DashboardSessionError::Entropy(getrandom::Error::UNEXPECTED))
        });
        assert!(matches!(failed, Err(DashboardSessionError::Entropy(_))));
        assert!(sessions.lock_state().unwrap().pending.is_empty());

        let collision = sessions.issue_bootstrap_at(now, GeneratedSecrets::fixed('J', 'J', 'K'));
        assert!(matches!(
            collision,
            Err(DashboardSessionError::CredentialCollision)
        ));
        assert!(sessions.lock_state().unwrap().pending.is_empty());
    }

    #[test]
    fn exact_bootstrap_deadline_is_expired_and_cannot_be_reused() {
        let base = Instant::now();
        let before = DashboardSessions::default();
        let before_token = before
            .issue_bootstrap_at(clock(100, base), GeneratedSecrets::fixed('A', 'B', 'C'))
            .unwrap();
        assert!(
            before
                .consume_bootstrap_at(
                    before_token.query_value(),
                    clock(159, base + MonotonicDuration::from_secs(59)),
                )
                .is_ok()
        );
        assert!(matches!(
            before.consume_bootstrap_at(
                before_token.query_value(),
                clock(159, base + MonotonicDuration::from_secs(59)),
            ),
            Err(BootstrapConsumeError::Invalid)
        ));

        let exact = DashboardSessions::default();
        let exact_token = exact
            .issue_bootstrap_at(clock(100, base), GeneratedSecrets::fixed('D', 'E', 'F'))
            .unwrap();
        assert!(matches!(
            exact.consume_bootstrap_at(
                exact_token.query_value(),
                clock(160, base + MonotonicDuration::from_mins(1)),
            ),
            Err(BootstrapConsumeError::Invalid)
        ));
    }

    #[test]
    fn concurrent_consumers_linearize_to_exactly_one_session() {
        let sessions = DashboardSessions::default();
        let token = sessions.issue_bootstrap().unwrap().query_value().to_owned();
        let barrier = Arc::new(Barrier::new(16));
        let threads = (0..16)
            .map(|_| {
                let sessions = sessions.clone();
                let token = token.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    sessions.consume_bootstrap(&token).is_ok()
                })
            })
            .collect::<Vec<_>>();

        assert_eq!(
            threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .filter(|consumed| *consumed)
                .count(),
            1
        );
    }

    #[test]
    fn monotonic_deadlines_ignore_wall_clock_rollback_and_do_not_slide() {
        let base = Instant::now();
        let sessions = DashboardSessions::default();
        let token = sessions
            .issue_bootstrap_at(clock(1_000, base), GeneratedSecrets::fixed('G', 'H', 'I'))
            .unwrap();
        let established = sessions
            .consume_bootstrap_at(
                token.query_value(),
                clock(500, base + MonotonicDuration::from_secs(1)),
            )
            .unwrap();
        let cookie = established.cookie_value().to_owned();
        let fixed_expiration = sessions
            .authenticate_session_at(&cookie, clock(500, base + MonotonicDuration::from_secs(1)))
            .unwrap()
            .expires_at();

        let before_expiry = sessions
            .authenticate_session_at(
                &cookie,
                clock(100, base + MonotonicDuration::from_hours(12)),
            )
            .unwrap();
        assert_eq!(before_expiry.expires_at(), fixed_expiration);
        assert!(matches!(
            sessions.authenticate_session_at(
                &cookie,
                clock(
                    50,
                    base + MonotonicDuration::from_hours(12) + MonotonicDuration::from_secs(1),
                ),
            ),
            Err(SessionAuthenticationError::Unauthorized)
        ));
    }

    #[test]
    fn new_process_state_rejects_every_previous_process_credential() {
        let old_process = DashboardSessions::default();
        let pending = old_process.issue_bootstrap().unwrap();
        let pending_value = pending.query_value().to_owned();
        let active = old_process.issue_bootstrap().unwrap();
        let established = old_process.consume_bootstrap(active.query_value()).unwrap();

        let new_process = DashboardSessions::default();
        assert!(matches!(
            new_process.consume_bootstrap(&pending_value),
            Err(BootstrapConsumeError::Invalid)
        ));
        assert!(matches!(
            new_process.authenticate_session(established.cookie_value()),
            Err(SessionAuthenticationError::Unauthorized)
        ));
    }

    #[test]
    fn bounded_pending_state_evicts_the_earliest_deadline_deterministically() {
        let base = Instant::now();
        let sessions = DashboardSessions::default();
        let mut first_token = None;

        for index in 0..=MAX_PENDING_BOOTSTRAP_TOKENS {
            let token = sessions
                .issue_bootstrap_at(
                    clock(
                        1_000,
                        base + MonotonicDuration::from_millis(u64::try_from(index).unwrap()),
                    ),
                    indexed_secrets(index),
                )
                .unwrap();
            first_token.get_or_insert_with(|| token.query_value().to_owned());
        }

        let state = sessions.lock_state().unwrap();
        assert_eq!(state.pending.len(), MAX_PENDING_BOOTSTRAP_TOKENS);
        assert!(
            !state
                .pending
                .contains_key(&credential_digest(first_token.as_deref().unwrap()))
        );
    }

    #[test]
    fn bounded_active_state_evicts_the_earliest_session_deadline() {
        let base = Instant::now();
        let sessions = DashboardSessions::default();
        let mut first_cookie = None;
        let mut first_authenticated = None;

        for index in 0..=MAX_ACTIVE_SESSIONS {
            let offset = MonotonicDuration::from_millis(u64::try_from(index).unwrap());
            let token = sessions
                .issue_bootstrap_at(clock(2_000, base + offset), indexed_secrets(index))
                .unwrap();
            let established = sessions
                .consume_bootstrap_at(token.query_value(), clock(2_000, base + offset))
                .unwrap();
            if first_cookie.is_none() {
                first_authenticated = Some(
                    sessions
                        .authenticate_session_at(
                            established.cookie_value(),
                            clock(2_000, base + offset),
                        )
                        .unwrap(),
                );
                first_cookie = Some(established.cookie_value().to_owned());
            }
        }

        let state = sessions.lock_state().unwrap();
        assert_eq!(state.sessions.len(), MAX_ACTIVE_SESSIONS);
        drop(state);
        assert!(matches!(
            sessions.authenticate_session_at(
                first_cookie.as_deref().unwrap(),
                clock(
                    2_001,
                    base + MonotonicDuration::from_millis(
                        u64::try_from(MAX_ACTIVE_SESSIONS).unwrap()
                    )
                )
            ),
            Err(SessionAuthenticationError::Unauthorized)
        ));
        assert!(
            !sessions
                .is_current_session(first_authenticated.as_ref().unwrap())
                .unwrap()
        );
    }
}
