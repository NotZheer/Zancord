//! In-process WebRTC mesh integration tests (Phase 1D.8):
//! - two peers exchange Opus frames end-to-end through the full mesh,
//! - perfect negotiation resolves simultaneous offers (polite rollback),
//! - a three-peer mesh connects all pairs.
//!
//! All connections use loopback host candidates — no STUN/TURN, matching the
//! Tailscale-only runtime model.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};
use tokio::time::timeout;
use webrtc::peer_connection::signaling_state::RTCSignalingState;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

use zancord_protocol::{EncodedAudioFrame, MediaStatePayload, PeerInfo, SignalMessage};
use zancord_transport::engine;
use zancord_transport::mesh::{IceState, MeshEvent, MeshManager};
use zancord_transport::negotiation::Negotiator;
use zancord_transport::tracks::TrackKind;

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,zancord_transport=debug,webrtc=warn".into()),
        )
        .with_test_writer()
        .try_init();
}

fn peer_info(id: &str) -> PeerInfo {
    PeerInfo {
        id: id.to_owned(),
        username: id.to_owned(),
        media_state: MediaStatePayload::default(),
    }
}

/// Pumps signaling in both directions between two meshes until both reach an
/// ICE `Connected`/`Completed` state AND signaling goes quiet (all SDP/candidates
/// flushed). Panics on timeout.
///
/// The quiet tail matters: with trickle ICE, connectivity can be established
/// before the remote answer is applied, and a frame written before the track is
/// bound is silently dropped.
async fn pump_until_ice_connected(
    a_sig_rx: &mut mpsc::Receiver<SignalMessage>,
    b: &mut MeshManager,
    b_sig_rx: &mut mpsc::Receiver<SignalMessage>,
    a: &mut MeshManager,
    a_events: &mut broadcast::Receiver<MeshEvent>,
    b_events: &mut broadcast::Receiver<MeshEvent>,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut a_ice = IceState::New;
    let mut b_ice = IceState::New;
    let mut idle_after_connect = 0u32;
    loop {
        let mut received_signal = false;
        tokio::select! {
            msg = a_sig_rx.recv() => {
                if let Some(msg) = msg { b.handle_signal(msg).await.expect("b handles signal"); }
                received_signal = true;
            }
            msg = b_sig_rx.recv() => {
                if let Some(msg) = msg { a.handle_signal(msg).await.expect("a handles signal"); }
                received_signal = true;
            }
            ev = a_events.recv() => {
                if let Ok(MeshEvent::IceStateChanged { state, .. }) = ev { a_ice = state; }
            }
            ev = b_events.recv() => {
                if let Ok(MeshEvent::IceStateChanged { state, .. }) = ev { b_ice = state; }
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
        if received_signal {
            idle_after_connect = 0;
        }
        let a_connected = matches!(a_ice, IceState::Connected | IceState::Completed);
        let b_connected = matches!(b_ice, IceState::Connected | IceState::Completed);
        if a_connected && b_connected {
            if received_signal {
                continue;
            }
            idle_after_connect += 1;
            if idle_after_connect >= 3 {
                return; // connected and signaling quiet for 300 ms
            }
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("ICE did not connect in time: a={a_ice:?} b={b_ice:?}");
        }
    }
}

#[tokio::test]
async fn two_peers_exchange_opus_frames() {
    init_tracing();
    let (a_sig_tx, mut a_sig_rx) = mpsc::channel(1024);
    let (b_sig_tx, mut b_sig_rx) = mpsc::channel(1024);
    let mut a = MeshManager::new("aaa".to_owned(), a_sig_tx, 5).expect("mesh a");
    let mut b = MeshManager::new("bbb".to_owned(), b_sig_tx, 5).expect("mesh b");
    let mut a_events = a.event_rx();
    let mut b_events = b.event_rx();

    a.handle_peer_joined(peer_info("bbb"))
        .await
        .expect("a joins b");
    b.handle_peer_joined(peer_info("aaa"))
        .await
        .expect("b joins a");

    pump_until_ice_connected(
        &mut a_sig_rx,
        &mut b,
        &mut b_sig_rx,
        &mut a,
        &mut a_events,
        &mut b_events,
    )
    .await;

    // Take the per-peer receive channels, then push frames in both directions.
    let mut b_audio = b.take_incoming_audio("aaa").expect("b audio channel");
    let mut a_audio = a.take_incoming_audio("bbb").expect("a audio channel");

    let frame = EncodedAudioFrame {
        data: vec![1, 2, 3, 4, 5],
        sequence: 1,
        timestamp_ms: 20,
    };
    a.audio_tx()
        .send(frame.clone())
        .await
        .expect("a sends frame");
    let got = timeout(Duration::from_secs(5), b_audio.recv())
        .await
        .expect("b receives frame")
        .expect("channel open");
    assert_eq!(got.data, frame.data, "a -> b payload");

    let frame2 = EncodedAudioFrame {
        data: vec![9, 8, 7],
        sequence: 7,
        timestamp_ms: 40,
    };
    b.audio_tx()
        .send(frame2.clone())
        .await
        .expect("b sends frame");
    let got2 = timeout(Duration::from_secs(5), a_audio.recv())
        .await
        .expect("a receives frame")
        .expect("channel open");
    assert_eq!(got2.data, frame2.data, "b -> a payload");

    a.shutdown().await.expect("a shuts down");
    b.shutdown().await.expect("b shuts down");
}

/// Regression test for the one-way-audio bug: the answering side's track binds
/// BEFORE ICE/DTLS completes, so frames written in that window used to kill the
/// send loop (write_sample errors) and silence the answerer forever.
#[tokio::test]
async fn early_audio_writes_do_not_kill_the_send_loop() {
    let (a_sig_tx, mut a_sig_rx) = mpsc::channel(1024);
    let (b_sig_tx, mut b_sig_rx) = mpsc::channel(1024);
    let mut a = MeshManager::new("aaa".to_owned(), a_sig_tx, 5).expect("mesh a");
    let mut b = MeshManager::new("bbb".to_owned(), b_sig_tx, 5).expect("mesh b");
    let mut a_events = a.event_rx();
    let mut b_events = b.event_rx();

    // Pump Opus frames into a's track from the very start — including while
    // the SDP exchange is still in flight (a is the ANSWERER here: aaa < bbb,
    // so b offers and a answers).
    let early_tx = a.audio_tx();
    let early_writer = tokio::spawn(async move {
        let mut seq = 0u64;
        loop {
            if early_tx
                .send(EncodedAudioFrame {
                    data: vec![0xde, 0xad, 0xbe, 0xef],
                    sequence: seq,
                    timestamp_ms: seq * 20,
                })
                .await
                .is_err()
            {
                return;
            }
            seq += 1;
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });

    a.handle_peer_joined(peer_info("bbb"))
        .await
        .expect("a joins b");
    b.handle_peer_joined(peer_info("aaa"))
        .await
        .expect("b joins a");

    pump_until_ice_connected(
        &mut a_sig_rx,
        &mut b,
        &mut b_sig_rx,
        &mut a,
        &mut a_events,
        &mut b_events,
    )
    .await;

    // The answerer's send loop must have survived the negotiation window.
    // Early frames that got through after ICE connected keep flushing through
    // the webrtc pipeline (and the writer keeps pumping every 5 ms), so wait
    // for the marker frame by payload instead of draining a fixed window.
    // Note: `EncodedAudioFrame::sequence` is NOT preserved on the wire — the
    // receive loop reports the RTP sequence number — so it cannot be used to
    // distinguish early frames here.
    let frame = EncodedAudioFrame {
        data: vec![1, 2, 3],
        sequence: 10_000,
        timestamp_ms: 20,
    };
    a.audio_tx()
        .send(frame.clone())
        .await
        .expect("a sends frame");
    let mut b_audio = b.take_incoming_audio("aaa").expect("b audio channel");
    let got = timeout(Duration::from_secs(5), async {
        loop {
            match b_audio.recv().await {
                Some(f) if f.data == frame.data => break f,
                Some(_) => continue, // early frame still flushing
                None => panic!("audio channel closed before marker frame arrived"),
            }
        }
    })
    .await
    .expect("answerer's marker frame must arrive within 5s");
    assert_eq!(got.data, frame.data);

    early_writer.abort();
    a.shutdown().await.expect("a shuts down");
    b.shutdown().await.expect("b shuts down");
}

/// Screen-audio must travel on its own channel: mic frames and screen-audio
/// frames from the same peer must not mix.
#[tokio::test]
async fn screen_audio_routes_to_a_dedicated_channel() {
    let (a_sig_tx, mut a_sig_rx) = mpsc::channel(1024);
    let (b_sig_tx, mut b_sig_rx) = mpsc::channel(1024);
    let mut a = MeshManager::new("aaa".to_owned(), a_sig_tx, 5).expect("mesh a");
    let mut b = MeshManager::new("bbb".to_owned(), b_sig_tx, 5).expect("mesh b");
    let mut a_events = a.event_rx();
    let mut b_events = b.event_rx();

    // Attach the screen-audio track BEFORE the peers connect, so it is part
    // of the initial SDP exchange.
    a.set_screen_enabled(true).await.expect("a screen on");
    b.set_screen_enabled(true).await.expect("b screen on");

    a.handle_peer_joined(peer_info("bbb"))
        .await
        .expect("a joins b");
    b.handle_peer_joined(peer_info("aaa"))
        .await
        .expect("b joins a");

    pump_until_ice_connected(
        &mut a_sig_rx,
        &mut b,
        &mut b_sig_rx,
        &mut a,
        &mut a_events,
        &mut b_events,
    )
    .await;

    let mut b_mic = b.take_incoming_audio("aaa").expect("b mic channel");
    let mut b_screen = b
        .take_incoming_screen_audio("aaa")
        .expect("b screen-audio channel");

    // Distinct payloads on the two send channels must arrive on the matching
    // receive channels.
    a.audio_tx()
        .send(EncodedAudioFrame {
            data: vec![0x01, 0x02, 0x03],
            sequence: 1,
            timestamp_ms: 20,
        })
        .await
        .expect("a sends mic frame");
    a.screen_audio_tx()
        .send(EncodedAudioFrame {
            data: vec![0xAA, 0xBB, 0xCC],
            sequence: 1,
            timestamp_ms: 20,
        })
        .await
        .expect("a sends screen-audio frame");

    let mic_frame = timeout(Duration::from_secs(5), b_mic.recv())
        .await
        .expect("mic frame arrives")
        .expect("mic channel open");
    let screen_frame = timeout(Duration::from_secs(5), b_screen.recv())
        .await
        .expect("screen-audio frame arrives")
        .expect("screen-audio channel open");
    assert_eq!(mic_frame.data, vec![0x01, 0x02, 0x03]);
    assert_eq!(screen_frame.data, vec![0xAA, 0xBB, 0xCC]);

    a.shutdown().await.expect("a shuts down");
    b.shutdown().await.expect("b shuts down");
}

/// Drives one signaling message into the receiving negotiator.
async fn deliver(sig_tx: &mpsc::Sender<SignalMessage>, msg: SignalMessage, neg: &Negotiator) {
    match msg {
        SignalMessage::Offer { sdp, .. } => {
            neg.handle_offer(sdp, sig_tx).await.expect("offer handled");
        }
        SignalMessage::Answer { sdp, .. } => {
            neg.handle_answer(sdp).await.expect("answer handled");
        }
        SignalMessage::Renegotiate { .. } => {
            neg.handle_renegotiate(sig_tx)
                .await
                .expect("renegotiate handled");
        }
        other => panic!("unexpected signaling message: {other:?}"),
    }
}

#[tokio::test]
async fn only_offerer_creates_offers_and_renegotiation_flows() {
    let api = engine::build_api().expect("api");
    let pc_a = Arc::new(
        api.new_peer_connection(Default::default())
            .await
            .expect("pc a"),
    );
    let pc_b = Arc::new(
        api.new_peer_connection(Default::default())
            .await
            .expect("pc b"),
    );

    // One audio track per side so offers carry real media sections.
    let track = |id: &str| {
        Arc::new(TrackLocalStaticSample::new(
            TrackKind::Mic.codec(),
            id.to_owned(),
            "zancord-audio".to_owned(),
        ))
    };
    let _ = pc_a.add_track(track("mic-a")).await.expect("track a");
    let _ = pc_b.add_track(track("mic-b")).await.expect("track b");

    let (a_sig_tx, mut a_sig_rx) = mpsc::channel(64);
    let (b_sig_tx, mut b_sig_rx) = mpsc::channel(64);
    // aaa < bbb → a is the offerer; b must never create an offer.
    let neg_a = Negotiator::new("aaa".to_owned(), "bbb".to_owned(), pc_a.clone());
    let neg_b = Negotiator::new("bbb".to_owned(), "aaa".to_owned(), pc_b.clone());
    assert!(neg_a.is_offerer());
    assert!(!neg_b.is_offerer());

    // Both sides experience negotiation-needed simultaneously (the glare
    // scenario): the offerer offers, the non-offerer requests renegotiation.
    let (ra, rb) = tokio::join!(
        neg_a.on_negotiation_needed(&a_sig_tx),
        neg_b.on_negotiation_needed(&b_sig_tx),
    );
    ra.expect("a offers");
    rb.expect("b requests renegotiation");
    assert!(
        neg_b.renegotiate_pending(),
        "b has a pending renegotiate request"
    );

    // Cross-deliver until both sides reach stable.
    for _ in 0..200 {
        let mut did_work = false;
        if let Ok(Some(msg)) = timeout(Duration::from_millis(50), a_sig_rx.recv()).await {
            deliver(&b_sig_tx, msg, &neg_b).await;
            did_work = true;
        }
        if let Ok(Some(msg)) = timeout(Duration::from_millis(50), b_sig_rx.recv()).await {
            deliver(&a_sig_tx, msg, &neg_a).await;
            did_work = true;
        }
        if pc_a.signaling_state() == RTCSignalingState::Stable
            && pc_b.signaling_state() == RTCSignalingState::Stable
        {
            break;
        }
        if !did_work {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    assert_eq!(pc_a.signaling_state(), RTCSignalingState::Stable);
    assert_eq!(pc_b.signaling_state(), RTCSignalingState::Stable);

    // Renegotiation: the non-offerer needs a new cycle; only the offerer may
    // create an offer.
    let (ra, rb) = tokio::join!(
        neg_a.on_negotiation_needed(&a_sig_tx),
        neg_b.on_negotiation_needed(&b_sig_tx),
    );
    ra.expect("a offers again");
    rb.expect("b re-requests");
    let mut offer_count = 0;
    for _ in 0..200 {
        let mut did_work = false;
        if let Ok(Some(msg)) = timeout(Duration::from_millis(50), a_sig_rx.recv()).await {
            if matches!(msg, SignalMessage::Offer { .. }) {
                offer_count += 1;
            }
            deliver(&b_sig_tx, msg, &neg_b).await;
            did_work = true;
        }
        if let Ok(Some(msg)) = timeout(Duration::from_millis(50), b_sig_rx.recv()).await {
            assert!(
                !matches!(msg, SignalMessage::Offer { .. }),
                "non-offerer must never create an offer"
            );
            deliver(&a_sig_tx, msg, &neg_a).await;
            did_work = true;
        }
        if pc_a.signaling_state() == RTCSignalingState::Stable
            && pc_b.signaling_state() == RTCSignalingState::Stable
        {
            break;
        }
        if !did_work {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    assert_eq!(pc_a.signaling_state(), RTCSignalingState::Stable);
    assert_eq!(pc_b.signaling_state(), RTCSignalingState::Stable);
    assert!(offer_count >= 1, "offerer must re-offer on renegotiation");

    pc_a.close().await.expect("close a");
    pc_b.close().await.expect("close b");
}

/// Routes a directed signal to the owning mesh; non-directed messages are dropped.
async fn dispatch(
    msg: SignalMessage,
    a: &mut MeshManager,
    b: &mut MeshManager,
    c: &mut MeshManager,
) {
    match msg.target() {
        Some("aaa") => a.handle_signal(msg).await.expect("a handles signal"),
        Some("bbb") => b.handle_signal(msg).await.expect("b handles signal"),
        Some("ccc") => c.handle_signal(msg).await.expect("c handles signal"),
        _ => {}
    }
}

#[tokio::test]
async fn three_peer_mesh_connects_all_pairs() {
    let (a_sig_tx, mut a_sig_rx) = mpsc::channel(1024);
    let (b_sig_tx, mut b_sig_rx) = mpsc::channel(1024);
    let (c_sig_tx, mut c_sig_rx) = mpsc::channel(1024);
    let mut a = MeshManager::new("aaa".to_owned(), a_sig_tx, 5).expect("mesh a");
    let mut b = MeshManager::new("bbb".to_owned(), b_sig_tx, 5).expect("mesh b");
    let mut c = MeshManager::new("ccc".to_owned(), c_sig_tx, 5).expect("mesh c");
    let mut a_events = a.event_rx();
    let mut b_events = b.event_rx();
    let mut c_events = c.event_rx();

    // Room join order: a first, then b, then c.
    for id in ["bbb", "ccc"] {
        a.handle_peer_joined(peer_info(id)).await.expect("a joins");
    }
    for id in ["aaa", "ccc"] {
        b.handle_peer_joined(peer_info(id)).await.expect("b joins");
    }
    for id in ["aaa", "bbb"] {
        c.handle_peer_joined(peer_info(id)).await.expect("c joins");
    }

    let mut ice_states: HashMap<String, IceState> = HashMap::new();
    let all_pairs = ["a-bbb", "a-ccc", "b-aaa", "b-ccc", "c-aaa", "c-bbb"];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        for (prefix, events) in [
            ('a', &mut a_events),
            ('b', &mut b_events),
            ('c', &mut c_events),
        ] {
            while let Ok(ev) = events.try_recv() {
                if let MeshEvent::IceStateChanged { peer_id, state } = ev {
                    ice_states.insert(format!("{prefix}-{peer_id}"), state);
                }
            }
        }
        if all_pairs.iter().all(|k| {
            matches!(
                ice_states.get(*k),
                Some(IceState::Connected | IceState::Completed)
            )
        }) {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("3-peer mesh did not connect: {ice_states:?}");
        }
        tokio::select! {
            msg = a_sig_rx.recv() => { if let Some(m) = msg { dispatch(m, &mut a, &mut b, &mut c).await; } }
            msg = b_sig_rx.recv() => { if let Some(m) = msg { dispatch(m, &mut a, &mut b, &mut c).await; } }
            msg = c_sig_rx.recv() => { if let Some(m) = msg { dispatch(m, &mut a, &mut b, &mut c).await; } }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }

    assert_eq!(a.peer_count(), 2);
    assert_eq!(b.peer_count(), 2);
    assert_eq!(c.peer_count(), 2);

    a.shutdown().await.expect("a shuts down");
    b.shutdown().await.expect("b shuts down");
    c.shutdown().await.expect("c shuts down");
}
