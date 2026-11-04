//! Per-key rate limiting (ADR-031).
//!
//! A token bucket: each key holds up to `burst` tokens, a request spends
//! one, and tokens refill steadily at `requests_per_minute`. A quiet key
//! can burst; a busy key is held to the steady rate; and no moment lets
//! through more than the bucket holds.
//!
//! Buckets live in memory: a limit protects the next few seconds, so
//! being a little generous after a restart costs little, and it adds no
//! database work or failure mode to the request path. Each server counts
//! separately, so running several needs a shared `RateLimiter`.
//!
//! Only authenticated keys reach this code, so the number of buckets is
//! bounded by the number of real keys.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

// Tokio's clock, so tests can pause and advance it.
use tokio::time::Instant;
use uuid::Uuid;

use crate::domain::BoxFuture;

/// The limit applied to each key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimit {
    /// The steady rate tokens refill at.
    pub requests_per_minute: u32,
    /// How many tokens a key can hold: the largest burst.
    pub burst: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allowed,
    /// Out of tokens; one will be available after `retry_after`.
    Limited {
        retry_after: Duration,
    },
}

/// The seam between the request path and the counters. Asynchronous so a
/// shared implementation can replace the in-memory one; such a store must
/// apply its own deadline.
pub trait RateLimiter: Send + Sync {
    /// Spends one token for `key`, if it has one.
    fn check(&self, key: Uuid) -> BoxFuture<'_, Decision>;
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    /// Fractional, because tokens refill continuously.
    tokens: f64,
    refilled_at: Instant,
}

/// `RateLimiter` holding one token bucket per key in memory.
#[derive(Debug)]
pub struct TokenBucketLimiter {
    limit: RateLimit,
    buckets: Mutex<HashMap<Uuid, Bucket>>,
}

impl TokenBucketLimiter {
    pub fn new(limit: RateLimit) -> Self {
        Self {
            limit,
            buckets: Mutex::default(),
        }
    }

    fn take(&self, key: Uuid, now: Instant) -> Decision {
        let capacity = f64::from(self.limit.burst);
        let per_second = f64::from(self.limit.requests_per_minute) / 60.0;

        // A panic elsewhere must not stop rate limiting, and a bucket is
        // never left half-updated, so a poisoned lock is still usable.
        let mut buckets = self.buckets.lock().unwrap_or_else(PoisonError::into_inner);
        let bucket = buckets.entry(key).or_insert(Bucket {
            tokens: capacity,
            refilled_at: now,
        });

        let elapsed = now.duration_since(bucket.refilled_at).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * per_second).min(capacity);
        bucket.refilled_at = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Decision::Allowed
        } else {
            Decision::Limited {
                retry_after: Duration::from_secs_f64((1.0 - bucket.tokens) / per_second),
            }
        }
    }
}

impl RateLimiter for TokenBucketLimiter {
    fn check(&self, key: Uuid) -> BoxFuture<'_, Decision> {
        let decision = self.take(key, Instant::now());
        Box::pin(std::future::ready(decision))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: Uuid = Uuid::from_u128(1);

    fn limiter(requests_per_minute: u32, burst: u32) -> TokenBucketLimiter {
        TokenBucketLimiter::new(RateLimit {
            requests_per_minute,
            burst,
        })
    }

    /// How many requests get through. Capped, so a limiter that never
    /// refuses fails the test instead of hanging it.
    async fn allowed(limiter: &TokenBucketLimiter, key: Uuid) -> usize {
        let mut count = 0;
        while count < 1_000 && limiter.check(key).await == Decision::Allowed {
            count += 1;
        }
        count
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_bucket_allows_exactly_its_burst() {
        assert_eq!(allowed(&limiter(60, 5), KEY).await, 5);
    }

    #[tokio::test(start_paused = true)]
    async fn tokens_refill_at_the_steady_rate() {
        let limiter = limiter(60, 5);
        allowed(&limiter, KEY).await;

        tokio::time::advance(Duration::from_secs(3)).await;

        assert_eq!(allowed(&limiter, KEY).await, 3, "one token per second");
    }

    #[tokio::test(start_paused = true)]
    async fn an_idle_key_never_holds_more_than_its_burst() {
        let limiter = limiter(60, 5);
        allowed(&limiter, KEY).await;

        tokio::time::advance(Duration::from_secs(3600)).await;

        assert_eq!(allowed(&limiter, KEY).await, 5);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_after_is_when_the_next_token_arrives() {
        let limiter = limiter(30, 1);
        assert_eq!(limiter.check(KEY).await, Decision::Allowed);

        assert_eq!(
            limiter.check(KEY).await,
            Decision::Limited {
                retry_after: Duration::from_secs(2)
            },
            "30 per minute is one every 2 seconds"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_limited_request_spends_nothing() {
        let limiter = limiter(60, 1);
        limiter.check(KEY).await;
        for _ in 0..50 {
            assert_ne!(limiter.check(KEY).await, Decision::Allowed);
        }

        tokio::time::advance(Duration::from_secs(1)).await;

        assert_eq!(limiter.check(KEY).await, Decision::Allowed);
    }

    #[tokio::test(start_paused = true)]
    async fn keys_have_separate_buckets() {
        let limiter = limiter(60, 2);
        allowed(&limiter, KEY).await;

        assert_eq!(allowed(&limiter, Uuid::from_u128(2)).await, 2);
    }
}
