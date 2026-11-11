//! Dashboard login for the operator (ADR-036).
//!
//! One admin token, read from the environment, is checked once at login.
//! After that the browser holds only a random session ID, in a cookie
//! scripts cannot read, that expires. Only hashes are kept, so this
//! process's memory holds neither the token nor a usable session ID.
//!
//! Sessions and the failed-login count live in memory: a restart logs the
//! operator out, which costs one login, and adds no database work.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
// Tokio's clock, so tests can pause and advance it.
use tokio::time::Instant;

/// Shortest admin token accepted at startup.
pub const MIN_TOKEN_CHARS: usize = 32;
/// Most sessions kept at once. Past this, logging in ends the session
/// closest to expiring, so repeated logins cannot grow memory.
pub const MAX_SESSIONS: usize = 64;
/// Failed logins allowed within `FAILED_LOGIN_WINDOW`. Past this, every
/// login is refused until the oldest failure leaves the window.
pub const MAX_FAILED_LOGINS: usize = 5;
pub const FAILED_LOGIN_WINDOW: Duration = Duration::from_secs(60);

type Hash = [u8; 32];

fn hash(value: &str) -> Hash {
    Sha256::digest(value.as_bytes()).into()
}

/// Compares every byte, so the time taken does not show where two hashes
/// first differ.
fn same(a: &Hash, b: &Hash) -> bool {
    a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// A poisoned lock is still usable: a panic elsewhere must not lock the
/// operator out, and no update here is left half-done.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The configured admin token, kept only as its hash.
#[derive(Clone)]
pub struct AdminToken(Hash);

impl AdminToken {
    pub fn new(token: &SecretString) -> Self {
        Self(hash(token.expose_secret()))
    }

    /// Whether `presented` is the admin token. Hashing first makes both
    /// sides the same length, and the compare checks every byte.
    pub fn matches(&self, presented: &str) -> bool {
        same(&self.0, &hash(presented))
    }
}

impl fmt::Debug for AdminToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AdminToken([REDACTED])")
    }
}

/// Logged-in sessions, by the hash of their ID. A session lasts a fixed
/// time from login; using it does not extend it.
#[derive(Debug)]
pub struct Sessions {
    ttl: Duration,
    expiries: Mutex<HashMap<Hash, Instant>>,
}

impl Sessions {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            expiries: Mutex::default(),
        }
    }

    /// How long a new session lasts.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Starts a session and returns its ID: 32 random bytes in hex.
    pub fn start(&self, now: Instant) -> Result<SecretString, getrandom::Error> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)?;
        let id: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();

        let mut expiries = lock(&self.expiries);
        expiries.retain(|_, expires| *expires > now);
        if expiries.len() >= MAX_SESSIONS
            && let Some(soonest) = expiries
                .iter()
                .min_by_key(|(_, expires)| **expires)
                .map(|(key, _)| *key)
        {
            expiries.remove(&soonest);
        }
        expiries.insert(hash(&id), now + self.ttl);
        Ok(SecretString::from(id))
    }

    /// The time left on session `id`, or `None` if it is unknown, ended,
    /// or expired.
    pub fn remaining(&self, id: &str, now: Instant) -> Option<Duration> {
        let key = hash(id);
        let mut expiries = lock(&self.expiries);
        match expiries.get(&key) {
            Some(expires) if *expires > now => Some(*expires - now),
            Some(_) => {
                expiries.remove(&key);
                None
            }
            None => None,
        }
    }

    /// Ends session `id`, if it exists.
    pub fn end(&self, id: &str) {
        lock(&self.expiries).remove(&hash(id));
    }

    /// Sessions currently kept, expired ones included until next pruned.
    pub fn len(&self) -> usize {
        lock(&self.expiries).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Counts failed logins in a sliding window. The admin token is too long
/// to guess, so this mostly keeps a misbehaving script from hammering
/// the login, and makes any attempt visible in the logs and metrics.
#[derive(Debug, Default)]
pub struct LoginThrottle {
    failures: Mutex<VecDeque<Instant>>,
}

impl LoginThrottle {
    /// `Err` with the wait until a login may be tried again.
    pub fn check(&self, now: Instant) -> Result<(), Duration> {
        let mut failures = lock(&self.failures);
        while failures
            .front()
            .is_some_and(|failed| now.duration_since(*failed) >= FAILED_LOGIN_WINDOW)
        {
            failures.pop_front();
        }
        match failures.front() {
            Some(oldest) if failures.len() >= MAX_FAILED_LOGINS => {
                Err(FAILED_LOGIN_WINDOW - now.duration_since(*oldest))
            }
            _ => Ok(()),
        }
    }

    pub fn record_failure(&self, now: Instant) {
        lock(&self.failures).push_back(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "an-admin-token-that-is-long-enough-123";

    #[test]
    fn the_token_matches_only_itself() {
        let token = AdminToken::new(&SecretString::from(TOKEN));

        assert!(token.matches(TOKEN));
        assert!(!token.matches(""));
        assert!(!token.matches(&TOKEN[..TOKEN.len() - 1]));
        assert!(!token.matches(&format!("{TOKEN}x")));
    }

    #[test]
    fn hashes_compare_on_every_byte() {
        let a = [7u8; 32];
        let mut last = a;
        last[31] ^= 1;
        let mut first = a;
        first[0] ^= 0x80;

        assert!(same(&a, &a));
        assert!(!same(&a, &last));
        assert!(!same(&a, &first));
    }

    #[test]
    fn debug_output_hides_the_token() {
        let token = AdminToken::new(&SecretString::from(TOKEN));

        assert_eq!(format!("{token:?}"), "AdminToken([REDACTED])");
    }

    #[tokio::test(start_paused = true)]
    async fn a_session_lasts_its_ttl_and_no_longer() {
        let sessions = Sessions::new(Duration::from_secs(60));
        let id = sessions.start(Instant::now()).unwrap();
        let id = id.expose_secret();

        assert_eq!(id.len(), 64);
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(
            sessions.remaining(id, Instant::now()),
            Some(Duration::from_secs(60))
        );

        tokio::time::advance(Duration::from_secs(59)).await;
        assert_eq!(
            sessions.remaining(id, Instant::now()),
            Some(Duration::from_secs(1))
        );

        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(sessions.remaining(id, Instant::now()), None);
        assert!(sessions.is_empty(), "an expired session is dropped");
    }

    #[tokio::test(start_paused = true)]
    async fn an_ended_session_is_gone() {
        let sessions = Sessions::new(Duration::from_secs(60));
        let id = sessions.start(Instant::now()).unwrap();

        sessions.end(id.expose_secret());

        assert_eq!(sessions.remaining(id.expose_secret(), Instant::now()), None);
        assert_eq!(sessions.remaining("not-a-session", Instant::now()), None);
    }

    #[tokio::test(start_paused = true)]
    async fn session_ids_are_unique() {
        let sessions = Sessions::new(Duration::from_secs(60));
        let a = sessions.start(Instant::now()).unwrap();
        let b = sessions.start(Instant::now()).unwrap();

        assert_ne!(a.expose_secret(), b.expose_secret());
        assert_eq!(sessions.len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn sessions_are_capped_by_ending_the_one_closest_to_expiring() {
        let sessions = Sessions::new(Duration::from_secs(600));
        let oldest = sessions.start(Instant::now()).unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;
        let second = sessions.start(Instant::now()).unwrap();
        for _ in 2..MAX_SESSIONS {
            sessions.start(Instant::now()).unwrap();
        }
        assert_eq!(sessions.len(), MAX_SESSIONS);

        let newest = sessions.start(Instant::now()).unwrap();

        let now = Instant::now();
        assert_eq!(sessions.len(), MAX_SESSIONS);
        assert_eq!(sessions.remaining(oldest.expose_secret(), now), None);
        assert!(sessions.remaining(second.expose_secret(), now).is_some());
        assert!(sessions.remaining(newest.expose_secret(), now).is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn expired_sessions_are_pruned_on_login() {
        let sessions = Sessions::new(Duration::from_secs(60));
        for _ in 0..3 {
            sessions.start(Instant::now()).unwrap();
        }
        tokio::time::advance(Duration::from_secs(60)).await;

        sessions.start(Instant::now()).unwrap();

        assert_eq!(sessions.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn logins_are_refused_after_too_many_failures_until_the_window_passes() {
        let throttle = LoginThrottle::default();
        for _ in 0..MAX_FAILED_LOGINS - 1 {
            throttle.record_failure(Instant::now());
        }
        assert_eq!(throttle.check(Instant::now()), Ok(()));

        throttle.record_failure(Instant::now());
        assert_eq!(throttle.check(Instant::now()), Err(FAILED_LOGIN_WINDOW));
    }

    #[tokio::test(start_paused = true)]
    async fn the_wait_counts_down_from_the_oldest_failure() {
        let throttle = LoginThrottle::default();
        throttle.record_failure(Instant::now());
        tokio::time::advance(Duration::from_secs(10)).await;
        for _ in 1..MAX_FAILED_LOGINS {
            throttle.record_failure(Instant::now());
        }

        assert_eq!(throttle.check(Instant::now()), Err(Duration::from_secs(50)));

        tokio::time::advance(Duration::from_secs(50)).await;
        assert_eq!(throttle.check(Instant::now()), Ok(()));
    }
}
