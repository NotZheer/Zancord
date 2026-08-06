//! Pipeline integration tests: capture ring → processing → Opus → transport
//! sender, including transport-sender swaps after a mesh rebuild (reconnect).

use rtrb::RingBuffer;
use tokio::sync::mpsc;
use zancord_audio::capture::MicCapture;
use zancord_audio::codec::OpusEncoder;
use zancord_audio::pipeline::{AudioControl, AudioEvent, AudioPipeline, PipelineConfig};
use zancord_audio::IncomingAudioKind;
use zancord_protocol::EncodedAudioFrame;

/// A 20 ms mono frame of loud samples (0.5 peak, well above the -40 dB gate).
const FRAME_SAMPLES: usize = 960; // 48 kHz * 20 ms

fn push_frame(producer: &mut rtrb::Producer<f32>) {
    for _ in 0..FRAME_SAMPLES {
        producer.push(0.5).expect("ring has capacity");
    }
}

#[tokio::test]
async fn set_mesh_sender_reroutes_captured_frames() {
    let (mut producer, consumer) = RingBuffer::<f32>::new(FRAME_SAMPLES * 4);
    let capture = MicCapture::from_ring(consumer, 48_000, 1);

    let (tx1, mut rx1) = mpsc::channel::<EncodedAudioFrame>(16);
    let (tx2, mut rx2) = mpsc::channel::<EncodedAudioFrame>(16);
    let (_in_tx, in_rx) =
        mpsc::channel::<(String, zancord_audio::IncomingAudioKind, EncodedAudioFrame)>(16);
    let (control_tx, control_rx) = mpsc::channel(16);
    let (_event_tx, _event_rx) = mpsc::channel::<AudioEvent>(16);

    let mut pipeline = AudioPipeline::with_io(
        Some(capture),
        None,
        PipelineConfig::default(),
        tx1,
        in_rx,
        control_rx,
        _event_tx,
    )
    .expect("pipeline builds");

    // Before the swap, frames land on the original sender.
    push_frame(&mut producer);
    push_frame(&mut producer);
    pipeline.tick().expect("tick");
    assert!(rx1.try_recv().is_ok(), "frames flow to the original sender");

    // Swap the transport sender (what the app does after a mesh rebuild).
    control_tx
        .send(AudioControl::SetMeshSender(tx2))
        .await
        .expect("control send");

    push_frame(&mut producer);
    push_frame(&mut producer);
    pipeline.tick().expect("tick");
    assert!(
        rx2.try_recv().is_ok(),
        "frames flow to the swapped sender after SetMeshSender"
    );
    assert!(
        rx1.try_recv().is_err(),
        "the original sender receives nothing after the swap"
    );
}

#[tokio::test]
async fn speaking_events_follow_voice_activity() {
    let (mut producer, consumer) = RingBuffer::<f32>::new(FRAME_SAMPLES * 4);
    let capture = MicCapture::from_ring(consumer, 48_000, 1);
    let (_tx, _rx) = mpsc::channel::<EncodedAudioFrame>(16);
    let (in_tx, in_rx) = mpsc::channel(16);
    let (control_tx, control_rx) = mpsc::channel(16);
    let (event_tx, mut event_rx) = mpsc::channel::<AudioEvent>(16);

    let mut pipeline = AudioPipeline::with_io(
        Some(capture),
        None,
        PipelineConfig::default(),
        _tx,
        in_rx,
        control_rx,
        event_tx,
    )
    .expect("pipeline builds");

    // Encode a gate-realistic envelope: loud speech, then a 50 ms-style
    // release ramp (the noise gate ramps gain over RELEASE_MS rather than
    // cutting), then exact zeros while closed.
    let mut encoder = OpusEncoder::new(32_000).expect("encoder");
    let mut packets = Vec::new();
    for (seq, amp) in [0.5f32, 0.2, 0.05, 0.0, 0.0, 0.0].into_iter().enumerate() {
        let pcm: Vec<i16> = if amp == 0.0 {
            vec![0i16; FRAME_SAMPLES]
        } else {
            let phase = 2.0 * std::f32::consts::PI * 440.0 / 48_000.0;
            (0..FRAME_SAMPLES)
                .map(|i| (amp * 32767.0 * (phase * i as f32).sin()) as i16)
                .collect()
        };
        packets.push(encoder.encode(&pcm).expect("encode"));
        let _ = seq;
    }
    let frame = |data: Vec<u8>, sequence: u64| EncodedAudioFrame {
        data,
        sequence,
        timestamp_ms: 0,
    };

    // Loud frame → ring on.
    in_tx
        .send((
            "peer-a".into(),
            IncomingAudioKind::Mic,
            frame(packets[0].clone(), 1),
        ))
        .await
        .expect("send");
    pipeline.tick().expect("tick");
    assert_eq!(
        event_rx.try_recv().ok(),
        Some(AudioEvent::Speaking {
            peer: "peer-a".into(),
            speaking: true,
        })
    );

    // Ramp + zero frames → the ring turns off once the release drops below
    // VAD_OFF (the codec's transition residual keeps the first zero frame(s)
    // above the threshold — real pauses release smoothly).
    for (seq, pkt) in packets.iter().enumerate().skip(1) {
        in_tx
            .send((
                "peer-a".into(),
                IncomingAudioKind::Mic,
                frame(pkt.clone(), seq as u64 + 1),
            ))
            .await
            .expect("send");
        pipeline.tick().expect("tick");
    }
    assert_eq!(
        event_rx.try_recv().ok(),
        Some(AudioEvent::Speaking {
            peer: "peer-a".into(),
            speaking: false,
        })
    );

    // Another zero frame → no transition event.
    in_tx
        .send((
            "peer-a".into(),
            IncomingAudioKind::Mic,
            frame(packets[5].clone(), 7),
        ))
        .await
        .expect("send");
    pipeline.tick().expect("tick");
    assert!(
        event_rx.try_recv().is_err(),
        "no event when the speaking state doesn't change"
    );
}
