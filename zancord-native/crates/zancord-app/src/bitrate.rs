//! Receiver-congestion policy (Phase 6): REMB hints from remote peers
//! (`RtcpFeedback::BitrateHint`) become an encoder bitrate target plus a
//! frame-skip ratio.
//!
//! webrtc-rs has no sender-side `maxBitrate`/`degradationPreference` knobs
//! (the browser `setParameters` API doesn't exist here), and openh264 applies
//! recorded bitrate changes only on encoder re-initialization — so the
//! immediate congestion control is the frame-skip: the encoder keeps its
//! target, but the wire rate drops proportionally when the slowest receiver
//! is the bottleneck.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Never skip more than this many frames (keeps a watchable minimum fps).
const MAX_FRAME_SKIP: u32 = 5;
/// Hints older than this are ignored, so a quiet receiver recovers.
const HINT_TTL: Duration = Duration::from_secs(5);
/// Never push the encoder target below this (avoids quality collapse).
const FLOOR_BPS: u32 = 200_000;

/// What one video session should do right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CongestionPolicy {
    /// Encoder target bitrate (applied by openh264 on next re-init).
    pub encoder_bps: u32,
    /// Send one of every N captured frames (1 = no skipping).
    pub frame_skip: u32,
}

impl CongestionPolicy {
    pub fn unconstrained(target_bps: u32) -> Self {
        Self {
            encoder_bps: target_bps,
            frame_skip: 1,
        }
    }
}

/// Tracks per-peer REMB hints for one video session (min across peers, with
/// staleness expiry).
pub struct CongestionState {
    target_bps: u32,
    hints: HashMap<String, (u32, Instant)>,
}

impl CongestionState {
    pub fn new(target_bps: u32) -> Self {
        Self {
            target_bps,
            hints: HashMap::new(),
        }
    }

    /// Records a fresh hint from `peer` and returns the policy to apply now.
    pub fn update(&mut self, peer: &str, remb_bps: u32, now: Instant) -> CongestionPolicy {
        self.hints.insert(peer.to_owned(), (remb_bps, now));
        self.policy(now)
    }

    /// The policy from the lowest fresh hint across all peers; the full target
    /// when no peer is congested.
    pub fn policy(&self, now: Instant) -> CongestionPolicy {
        let mut min_bps = self.target_bps;
        for (bps, at) in self.hints.values() {
            if now.duration_since(*at) <= HINT_TTL {
                min_bps = min_bps.min(*bps);
            }
        }
        let remb = min_bps.max(FLOOR_BPS);
        if remb >= self.target_bps {
            return CongestionPolicy::unconstrained(self.target_bps);
        }
        let frame_skip = (self.target_bps / remb.max(1)).clamp(1, MAX_FRAME_SKIP);
        CongestionPolicy {
            encoder_bps: remb,
            frame_skip,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Instant {
        Instant::now()
    }

    #[test]
    fn no_hints_means_full_target() {
        let state = CongestionState::new(2_000_000);
        assert_eq!(
            state.policy(now()),
            CongestionPolicy::unconstrained(2_000_000)
        );
    }

    #[test]
    fn hint_above_target_is_ignored() {
        let mut state = CongestionState::new(2_000_000);
        assert_eq!(
            state.update("peer-a", 8_000_000, now()),
            CongestionPolicy::unconstrained(2_000_000)
        );
    }

    #[test]
    fn low_hint_cuts_bitrate_and_skips_frames() {
        let mut state = CongestionState::new(2_000_000);
        // REMB at half the target: encoder target halves, wire rate halves.
        assert_eq!(
            state.update("peer-a", 1_000_000, now()),
            CongestionPolicy {
                encoder_bps: 1_000_000,
                frame_skip: 2,
            }
        );
    }

    #[test]
    fn slowest_peer_wins() {
        let mut state = CongestionState::new(2_000_000);
        state.update("peer-a", 1_000_000, now());
        assert_eq!(
            state.update("peer-b", 500_000, now()),
            CongestionPolicy {
                encoder_bps: 500_000,
                frame_skip: 4,
            }
        );
    }

    #[test]
    fn frame_skip_is_capped() {
        let mut state = CongestionState::new(2_000_000);
        let policy = state.update("peer-a", 100_000, now());
        assert_eq!(policy.encoder_bps, FLOOR_BPS); // floored
        assert_eq!(policy.frame_skip, MAX_FRAME_SKIP); // capped
    }

    #[test]
    fn stale_hint_expires() {
        let t0 = now();
        let mut state = CongestionState::new(2_000_000);
        state.update("peer-a", 500_000, t0);
        let later = t0 + HINT_TTL + Duration::from_millis(1);
        assert_eq!(
            state.policy(later),
            CongestionPolicy::unconstrained(2_000_000)
        );
    }
}
