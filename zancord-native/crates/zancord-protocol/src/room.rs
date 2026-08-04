//! Room & peer state types shared by server and clients.

use serde::{Deserialize, Serialize};

use crate::peer::PeerId;

/// Maximum peers in a room (full mesh, including self).
pub const MAX_ROOM_SIZE: usize = 6;

/// Default room installed PWAs/native clients join automatically.
pub const DEFAULT_ROOM: &str = "zancord-room";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerInfo {
    pub id: PeerId,
    pub username: String,
    pub media_state: MediaStatePayload,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct MediaStatePayload {
    pub mic_enabled: bool,
    pub camera_enabled: bool,
    pub screen_sharing: bool,
    pub deafened: bool,
}

/// Validates a room id against `^[a-zA-Z0-9_-]{1,64}$`.
pub fn is_valid_room_id(room_id: &str) -> bool {
    !room_id.is_empty()
        && room_id.len() <= 64
        && room_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Sanitizes a username: trims whitespace, strips control characters, caps at 24 chars.
pub fn sanitize_username(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(24)
        .collect();
    if cleaned.is_empty() {
        "Guest".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_id_validation() {
        assert!(is_valid_room_id("zancord-room"));
        assert!(is_valid_room_id("a_1-B"));
        assert!(is_valid_room_id(&"x".repeat(64)));
        assert!(!is_valid_room_id(""));
        assert!(!is_valid_room_id("has space"));
        assert!(!is_valid_room_id("has/slash"));
        assert!(!is_valid_room_id(&"x".repeat(65)));
    }

    #[test]
    fn username_sanitization() {
        assert_eq!(sanitize_username("  alice  "), "alice");
        assert_eq!(sanitize_username("a\u{1b}b"), "ab"); // strip ESC
        assert_eq!(sanitize_username(&"a".repeat(40)), "a".repeat(24));
        assert_eq!(sanitize_username("   "), "Guest");
    }

    #[test]
    fn media_state_defaults_to_disabled() {
        let s = MediaStatePayload::default();
        assert!(!s.mic_enabled && !s.camera_enabled && !s.screen_sharing && !s.deafened);
    }
}
