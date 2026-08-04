//! WebRTC negotiation (Phase 1D.3) — single-offerer scheme.
//!
//! The browser's perfect-negotiation pattern relies on JSEP *rollback*, which
//! webrtc-rs 0.12 does not implement (its signaling state machine rejects both
//! local and remote rollbacks). Zancord therefore uses a deterministic
//! single-offerer scheme that makes glare impossible by construction:
//!
//! - The peer with the lexicographically smaller id is the **offerer** and the
//!   only peer that ever creates offers for the lifetime of the connection.
//! - The other peer only answers. When it needs a renegotiation (track
//!   add/remove), it sends a `Renegotiate` request; the offerer then starts a
//!   new offer/answer cycle.
//! - All negotiation for a peer pair is serialized by an internal mutex.
//!
//! `handle_offer` is still defensive: an unexpected offer while we are making
//! one is logged and ignored rather than crashing the connection.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, warn};
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::signaling_state::RTCSignalingState;
use webrtc::peer_connection::RTCPeerConnection;

use zancord_protocol::SignalMessage;

/// Lexicographic offerer rule: the smaller peer id is the offerer.
pub(crate) fn is_offerer(local_id: &str, peer_id: &str) -> bool {
    local_id < peer_id
}

/// Negotiation state machine for one peer pair.
#[derive(Clone)]
pub struct Negotiator {
    local_id: String,
    peer_id: String,
    is_offerer: bool,
    /// Serializes all offer/answer operations for this peer pair.
    lock: Arc<Mutex<()>>,
    making_offer: Arc<AtomicBool>,
    /// Set while a `Renegotiate` request is in flight (dedupes renegotiation
    /// storms from `on_negotiation_needed`). Cleared when an offer arrives.
    renegotiate_pending: Arc<AtomicBool>,
    pc: Arc<RTCPeerConnection>,
}

impl Negotiator {
    /// Creates a negotiator; the smaller id is the offerer.
    pub fn new(local_id: String, peer_id: String, pc: Arc<RTCPeerConnection>) -> Self {
        Self {
            is_offerer: is_offerer(&local_id, &peer_id),
            local_id,
            peer_id,
            lock: Arc::new(Mutex::new(())),
            making_offer: Arc::new(AtomicBool::new(false)),
            renegotiate_pending: Arc::new(AtomicBool::new(false)),
            pc,
        }
    }

    /// Whether this side creates offers for the peer pair.
    pub fn is_offerer(&self) -> bool {
        self.is_offerer
    }

    /// Whether a renegotiation request is currently in flight.
    pub fn renegotiate_pending(&self) -> bool {
        self.renegotiate_pending.load(Ordering::SeqCst)
    }

    /// `on_negotiation_needed` handler: the offerer creates and sends an offer;
    /// the other side requests renegotiation instead. Serialized against other
    /// negotiation operations.
    pub async fn on_negotiation_needed(
        &self,
        signaling_tx: &mpsc::Sender<SignalMessage>,
    ) -> Result<()> {
        let _guard = self.lock.lock().await;

        if !self.is_offerer {
            // Never offer; ask the offerer to renegotiate instead.
            if self.renegotiate_pending.swap(true, Ordering::SeqCst) {
                return Ok(()); // already requested; offerer will act on it
            }
            signaling_tx
                .send(SignalMessage::Renegotiate {
                    target: self.peer_id.clone(),
                    sender: self.local_id.clone(),
                })
                .await?;
            debug!(
                local = %self.local_id,
                peer = %self.peer_id,
                "requested renegotiation from offerer"
            );
            return Ok(());
        }

        // Offerer: skip if a cycle is already in flight (negotiation-needed
        // fires on every track add; the in-flight offer already captured it).
        if self.making_offer.load(Ordering::SeqCst)
            || self.pc.signaling_state() != RTCSignalingState::Stable
        {
            debug!(
                local = %self.local_id,
                peer = %self.peer_id,
                "offer skipped: cycle already in flight"
            );
            return Ok(());
        }

        self.make_offer(signaling_tx).await
    }

    /// Handles a `Renegotiate` request: the offerer starts a new offer/answer
    /// cycle. Ignored when received by the non-offerer or while an offer is
    /// already in flight (the in-flight cycle covers the requested change).
    pub async fn handle_renegotiate(
        &self,
        signaling_tx: &mpsc::Sender<SignalMessage>,
    ) -> Result<()> {
        if !self.is_offerer {
            debug!(local = %self.local_id, peer = %self.peer_id, "ignoring renegotiate request (not the offerer)");
            return Ok(());
        }
        let _guard = self.lock.lock().await;
        if self.making_offer.load(Ordering::SeqCst)
            || self.pc.signaling_state() != RTCSignalingState::Stable
        {
            debug!(
                local = %self.local_id,
                peer = %self.peer_id,
                "renegotiate ignored: offer already in flight"
            );
            return Ok(());
        }
        self.make_offer(signaling_tx).await
    }

    async fn make_offer(&self, signaling_tx: &mpsc::Sender<SignalMessage>) -> Result<()> {
        self.making_offer.store(true, Ordering::SeqCst);
        let result = self.make_offer_inner(signaling_tx).await;
        self.making_offer.store(false, Ordering::SeqCst);
        result
    }

    async fn make_offer_inner(&self, signaling_tx: &mpsc::Sender<SignalMessage>) -> Result<()> {
        let offer = self.pc.create_offer(None).await?;
        self.pc.set_local_description(offer.clone()).await?;
        signaling_tx
            .send(SignalMessage::Offer {
                target: self.peer_id.clone(),
                sender: self.local_id.clone(),
                sdp: offer.sdp,
            })
            .await?;
        debug!(local = %self.local_id, peer = %self.peer_id, "sent offer");
        Ok(())
    }

    /// Handles a remote offer. In the single-offerer scheme this only happens
    /// from the offerer; a colliding offer (while we are making one) is logged
    /// and ignored — webrtc-rs cannot roll back.
    pub async fn handle_offer(
        &self,
        sdp: String,
        signaling_tx: &mpsc::Sender<SignalMessage>,
    ) -> Result<()> {
        let _guard = self.lock.lock().await;

        self.renegotiate_pending.store(false, Ordering::SeqCst);

        let collision = self.making_offer.load(Ordering::SeqCst)
            || self.pc.signaling_state() != RTCSignalingState::Stable;
        if collision {
            warn!(
                local = %self.local_id,
                peer = %self.peer_id,
                "glare: ignoring colliding remote offer (no rollback support in webrtc-rs)"
            );
            return Ok(());
        }

        self.pc
            .set_remote_description(RTCSessionDescription::offer(sdp)?)
            .await?;

        let answer = self.pc.create_answer(None).await?;
        self.pc.set_local_description(answer.clone()).await?;
        signaling_tx
            .send(SignalMessage::Answer {
                target: self.peer_id.clone(),
                sender: self.local_id.clone(),
                sdp: answer.sdp,
            })
            .await?;
        debug!(local = %self.local_id, peer = %self.peer_id, "sent answer");
        Ok(())
    }

    /// Handles a remote answer.
    pub async fn handle_answer(&self, sdp: String) -> Result<()> {
        let _guard = self.lock.lock().await;
        self.pc
            .set_remote_description(RTCSessionDescription::answer(sdp)?)
            .await?;
        debug!(local = %self.local_id, peer = %self.peer_id, "applied remote answer");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offerer_is_lexicographic() {
        assert!(is_offerer("alice", "bob"));
        assert!(!is_offerer("bob", "alice"));
        assert!(!is_offerer("same", "same"));
    }
}
