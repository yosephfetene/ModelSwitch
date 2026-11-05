//! Per-key concurrency limits (ADR-032).
//!
//! A rate limit counts arrivals; this counts requests still running. A
//! slow provider turns a modest arrival rate into a pile of open requests,
//! so each key gets a fixed number of seats, and a request holds one from
//! start to finish.
//!
//! A `Seat` gives itself back when dropped, so a request cannot leak one
//! however it ends: success, failure, a panic, or the client going away.
//!
//! Requests running on a server are that server's own load, so these
//! counts belong in memory, with no shared store to replace them. A key
//! with no running requests has no entry, so memory follows current work.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use uuid::Uuid;

type Running = Arc<Mutex<HashMap<Uuid, u32>>>;

/// A panic elsewhere must not stop the counting, and a count is never
/// left half-updated, so a poisoned lock is still usable.
fn lock(running: &Running) -> MutexGuard<'_, HashMap<Uuid, u32>> {
    running.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Counts running requests per key.
#[derive(Debug)]
pub struct ConcurrencyLimiter {
    seats_per_key: u32,
    running: Running,
}

impl ConcurrencyLimiter {
    pub fn new(seats_per_key: u32) -> Self {
        Self {
            seats_per_key,
            running: Running::default(),
        }
    }

    /// Takes a seat for `key`, or `None` if all of its seats are taken.
    /// Never waits: a seat may not free up until a provider's deadline,
    /// and a queue would rebuild the pile-up this limit prevents.
    pub fn try_acquire(&self, key: Uuid) -> Option<Seat> {
        let mut running = lock(&self.running);
        let count = running.entry(key).or_default();
        if *count >= self.seats_per_key {
            return None;
        }
        *count += 1;
        Some(Seat {
            key,
            running: Arc::clone(&self.running),
        })
    }
}

/// One running request's place. Dropping it frees the seat.
#[derive(Debug)]
#[must_use = "dropping a seat frees it at once"]
pub struct Seat {
    key: Uuid,
    running: Running,
}

impl Drop for Seat {
    fn drop(&mut self) {
        let mut running = lock(&self.running);
        if let Some(count) = running.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                running.remove(&self.key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: Uuid = Uuid::from_u128(1);

    /// Takes every seat `key` has left. Capped, so a limiter that never
    /// refuses fails the test instead of hanging it.
    fn take_all(limiter: &ConcurrencyLimiter, key: Uuid) -> Vec<Seat> {
        std::iter::from_fn(|| limiter.try_acquire(key))
            .take(1_000)
            .collect()
    }

    #[test]
    fn a_key_gets_exactly_its_seats() {
        assert_eq!(take_all(&ConcurrencyLimiter::new(3), KEY).len(), 3);
    }

    #[test]
    fn a_dropped_seat_can_be_taken_again() {
        let limiter = ConcurrencyLimiter::new(3);
        let mut seats = take_all(&limiter, KEY);

        seats.pop();

        assert_eq!(take_all(&limiter, KEY).len(), 1);
    }

    #[test]
    fn a_refusal_takes_no_seat() {
        let limiter = ConcurrencyLimiter::new(1);
        let seat = limiter.try_acquire(KEY);
        for _ in 0..50 {
            assert!(limiter.try_acquire(KEY).is_none());
        }

        drop(seat);

        assert_eq!(take_all(&limiter, KEY).len(), 1);
    }

    #[test]
    fn keys_have_separate_seats() {
        let limiter = ConcurrencyLimiter::new(2);
        let _taken = take_all(&limiter, KEY);

        assert_eq!(take_all(&limiter, Uuid::from_u128(2)).len(), 2);
    }

    #[test]
    fn a_key_with_nothing_running_has_no_entry() {
        let limiter = ConcurrencyLimiter::new(2);

        drop(take_all(&limiter, KEY));

        assert!(lock(&limiter.running).is_empty());
    }

    #[test]
    fn a_panic_gives_the_seat_back() {
        let limiter = ConcurrencyLimiter::new(1);
        let seat = limiter.try_acquire(KEY);

        let result = std::thread::spawn(move || {
            let _seat = seat;
            panic!("request panicked");
        })
        .join();

        assert!(result.is_err());
        assert_eq!(take_all(&limiter, KEY).len(), 1);
    }
}
