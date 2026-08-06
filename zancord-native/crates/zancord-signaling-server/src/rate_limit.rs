//! Per-peer token-bucket rate limiting (Phase 1A.4):
//! signal 200/s, chat 5/s, state 10/s, join-room 3 per 10s.

use std::time::Instant;

/// Message categories that are rate-limited independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MessageKind {
    /// WebRTC signaling: offers, answers, ice candidates (and misc traffic).
    Signal,
    /// Chat messages.
    Chat,
    /// Media state updates.
    State,
    /// Room join attempts.
    Join,
}

/// Token bucket with continuous refill; `take_at` accepts an explicit clock so
/// unit tests are deterministic without sleeping.
#[derive(Debug)]
pub struct TokenBucket {
    capacity: f64,
    refill_per_sec: f64,
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    /// `capacity` = burst size, `refill_per_sec` = steady-state rate.
    pub fn new(capacity: f64, refill_per_sec: f64) -> Self {
        Self {
            capacity,
            refill_per_sec,
            tokens: capacity,
            last_refill: Instant::now(),
        }
    }

    /// Tries to consume one token as of `now`. Refills tokens accrued since
    /// the last call, capped at `capacity`.
    pub fn take_at(&mut self, now: Instant) -> bool {
        let elapsed = now
            .saturating_duration_since(self.last_refill)
            .as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        self.last_refill = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Tries to consume one token now.
    pub fn take(&mut self) -> bool {
        self.take_at(Instant::now())
    }
}

/// Per-peer rate limiter: one independent bucket per `MessageKind`.
#[derive(Debug)]
pub struct RateLimiter {
    buckets: [TokenBucket; 4],
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimiter {
    /// Rates: signal 200/s (WebRTC ICE trickle bursts + renegotiation easily
    /// exceed 30/s on macOS, where candidate gathering is bursty), chat 5/s,
    /// state 10/s, join 3/10s.
    pub fn new() -> Self {
        Self {
            buckets: [
                TokenBucket::new(200.0, 200.0),
                TokenBucket::new(5.0, 5.0),
                TokenBucket::new(10.0, 10.0),
                TokenBucket::new(3.0, 3.0 / 10.0),
            ],
        }
    }

    /// Returns `true` if a message of `kind` is allowed right now.
    pub fn allow(&mut self, kind: MessageKind) -> bool {
        self.allow_at(kind, Instant::now())
    }

    /// Test-friendly variant of [`Self::allow`] with a fixed clock.
    fn allow_at(&mut self, kind: MessageKind, now: Instant) -> bool {
        self.buckets[kind as usize].take_at(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn start() -> Instant {
        Instant::now()
    }

    #[test]
    fn bucket_accepts_burst_then_throttles() {
        let mut b = TokenBucket::new(3.0, 1.0);
        let t0 = start();
        for _ in 0..3 {
            assert!(b.take_at(t0));
        }
        assert!(!b.take_at(t0));
    }

    #[test]
    fn bucket_refills_over_time_and_caps_capacity() {
        let mut b = TokenBucket::new(1.0, 1.0);
        let t0 = start();
        assert!(b.take_at(t0));
        assert!(!b.take_at(t0 + Duration::from_millis(500)));
        assert!(b.take_at(t0 + Duration::from_secs(1)));

        // Long idle time never accumulates beyond capacity.
        let mut capped = TokenBucket::new(2.0, 10.0);
        let t1 = start();
        assert!(capped.take_at(t1 + Duration::from_secs(100)));
        assert!(capped.take_at(t1 + Duration::from_secs(100)));
        assert!(!capped.take_at(t1 + Duration::from_secs(100)));
    }

    #[test]
    fn rate_limiter_enforces_per_kind_limits() {
        let mut rl = RateLimiter::new();
        let t0 = start();
        for _ in 0..200 {
            assert!(rl.allow_at(MessageKind::Signal, t0));
        }
        assert!(!rl.allow_at(MessageKind::Signal, t0));
        for _ in 0..5 {
            assert!(rl.allow_at(MessageKind::Chat, t0));
        }
        assert!(!rl.allow_at(MessageKind::Chat, t0));
        for _ in 0..10 {
            assert!(rl.allow_at(MessageKind::State, t0));
        }
        assert!(!rl.allow_at(MessageKind::State, t0));
        for _ in 0..3 {
            assert!(rl.allow_at(MessageKind::Join, t0));
        }
        assert!(!rl.allow_at(MessageKind::Join, t0));

        // Kinds are independent: exhausting signal doesn't reset chat.
        assert!(!rl.allow_at(MessageKind::Chat, t0));

        // Join recovers after 10s: the bucket refills to capacity (3), so all
        // three tokens are available again.
        let t1 = t0 + Duration::from_secs(10);
        assert!(rl.allow_at(MessageKind::Join, t1));
        assert!(rl.allow_at(MessageKind::Join, t1));
        assert!(rl.allow_at(MessageKind::Join, t1));
        assert!(!rl.allow_at(MessageKind::Join, t1));
    }
}
