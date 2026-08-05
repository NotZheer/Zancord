//! Zancord UI harness (Phase 4): renders the Slint main window with mock
//! state so the design system and components can be exercised before the
//! orchestrator (Phase 4.10) wires real subsystems in.

use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

slint::include_modules!();

/// Builds a small solid-color RGBA image to stand in for a video frame.
fn mock_frame(r: u8, g: u8, b: u8) -> Image {
    const W: usize = 160;
    const H: usize = 90;
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(W as u32, H as u32);
    let bytes = buf.make_mut_bytes();
    for y in 0..H {
        for x in 0..W {
            let i = (y * W + x) * 4;
            let shade = (x * 255 / W) as u8;
            bytes[i] = r.saturating_add(shade / 3);
            bytes[i + 1] = g.saturating_add(shade / 3);
            bytes[i + 2] = b.saturating_add(shade / 3);
            bytes[i + 3] = 255;
        }
    }
    Image::from_rgba8(buf)
}

fn main() -> anyhow::Result<()> {
    let window = MainWindow::new()?;

    window.set_room_name("zancord-room".into());
    window.set_peer_count(2);

    window.set_peers(ModelRc::new(VecModel::from(vec![
        PeerData {
            id: "peer-1".into(),
            username: "Alice".into(),
            initial: "A".into(),
            frame: mock_frame(124, 58, 237),
            is_speaking: true,
            is_muted: false,
            has_video: true,
            is_screen_share: false,
        },
        PeerData {
            id: "peer-2".into(),
            username: "Bob".into(),
            initial: "B".into(),
            frame: Image::default(),
            is_speaking: false,
            is_muted: true,
            has_video: false,
            is_screen_share: true,
        },
    ])));

    window.set_local_video_frame(mock_frame(34, 197, 94));
    window.set_local_has_video(true);
    window.set_mic_enabled(true);
    window.set_camera_enabled(true);
    window.set_screen_sharing(false);
    window.set_deafened(false);

    window.set_chat_messages(ModelRc::new(VecModel::from(vec![
        ChatMessage {
            sender: "Alice".into(),
            content: "hey, can you hear me?".into(),
            timestamp: "14:02".into(),
            is_self: false,
        },
        ChatMessage {
            sender: "You".into(),
            content: "loud and clear!".into(),
            timestamp: "14:03".into(),
            is_self: true,
        },
    ])));

    window.set_toasts(ModelRc::new(VecModel::from(vec![Toast {
        text: "Connected to zancord-room".into(),
        kind: "success".into(),
    }])));

    // Callback stubs: the orchestrator (Phase 4.10) replaces these with real
    // subsystem dispatch.
    let w = window.as_weak();
    window.on_toggle_mic(move || {
        println!("[ui] toggle_mic");
        if let Some(w) = w.upgrade() {
            w.set_mic_enabled(!w.get_mic_enabled());
        }
    });
    let w = window.as_weak();
    window.on_toggle_camera(move || {
        println!("[ui] toggle_camera");
        if let Some(w) = w.upgrade() {
            w.set_camera_enabled(!w.get_camera_enabled());
        }
    });
    let w = window.as_weak();
    window.on_toggle_screen_share(move || {
        println!("[ui] toggle_screen_share");
        if let Some(w) = w.upgrade() {
            w.set_screen_sharing(!w.get_screen_sharing());
        }
    });
    let w = window.as_weak();
    window.on_toggle_deafen(move || {
        println!("[ui] toggle_deafen");
        if let Some(w) = w.upgrade() {
            w.set_deafened(!w.get_deafened());
        }
    });
    window.on_leave_room(|| {
        println!("[ui] leave_room");
        std::process::exit(0);
    });
    window.on_send_chat(|content| {
        println!("[ui] send_chat: {content}");
    });
    window.on_copy_invite_link(|| {
        println!("[ui] copy_invite_link");
    });
    window.on_toggle_chat(|| {
        println!("[ui] toggle_chat");
    });

    window.run()?;
    Ok(())
}
