//! Persistent configuration (Phase 4.11): user preferences saved as JSON in
//! the platform config directory.
//!
//! - macOS: `~/Library/Application Support/Zancord/config.json`
//! - Linux: `~/.config/zancord/config.json`

use std::path::PathBuf;

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// User preferences. All fields optional so older configs stay loadable;
/// `serde(default)` keeps new fields from breaking existing files.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub username: Option<String>,
    /// Last signaling endpoint (ws://host:port) — prefills the join screen.
    pub last_endpoint: Option<String>,
    pub last_room: Option<String>,
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub camera_index: Option<u32>,
    /// Last capture source id chosen in the screen picker (display:N / window:N).
    pub screen_source_id: Option<String>,
    pub noise_gate_enabled: bool,
    pub noise_gate_threshold_db: f32,
    pub hpf_enabled: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            username: None,
            last_endpoint: None,
            last_room: None,
            input_device: None,
            output_device: None,
            camera_index: None,
            screen_source_id: None,
            noise_gate_enabled: true,
            noise_gate_threshold_db: -40.0,
            hpf_enabled: true,
        }
    }
}

impl AppConfig {
    /// The platform config file path (creating the parent directory is up to
    /// the caller via [`Self::save`]).
    pub fn path() -> anyhow::Result<PathBuf> {
        let dir = directories::ProjectDirs::from("", "", "Zancord")
            .context("no platform config directory available")?
            .config_dir()
            .to_path_buf();
        Ok(dir.join("config.json"))
    }

    /// Loads the config, or `Default` when missing/corrupt (corruption logs a
    /// warning and falls back to defaults rather than failing startup).
    pub fn load() -> Self {
        Self::load_from(&Self::path().unwrap_or_default())
    }

    /// Loads from an explicit path (used by tests).
    pub fn load_from(path: &std::path::Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        match serde_json::from_str(&raw) {
            Ok(config) => config,
            Err(err) => {
                tracing::warn!(error = %err, path = %path.display(), "corrupt config; using defaults");
                Self::default()
            }
        }
    }

    /// Saves the config, creating parent directories as needed.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path()?;
        self.save_to(&path)
    }

    /// Saves to an explicit path (used by tests).
    pub fn save_to(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating config dir {}", parent.display()))?;
        }
        let raw = serde_json::to_string_pretty(self).context("serializing config")?;
        std::fs::write(path, raw).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("zancord-config-test-{}", std::process::id()));
        let path = dir.join("config.json");
        let config = AppConfig {
            username: Some("alice".into()),
            last_endpoint: Some("ws://100.64.0.1:3000".into()),
            last_room: Some("zancord-room".into()),
            input_device: Some("hw:0".into()),
            output_device: Some("pulse".into()),
            camera_index: Some(2),
            screen_source_id: Some("display:1".into()),
            ..Default::default()
        };
        config.save_to(&path).expect("saves");
        let loaded = AppConfig::load_from(&path);
        assert_eq!(loaded.username.as_deref(), Some("alice"));
        assert_eq!(
            loaded.last_endpoint.as_deref(),
            Some("ws://100.64.0.1:3000")
        );
        assert_eq!(loaded.last_room.as_deref(), Some("zancord-room"));
        assert_eq!(loaded.input_device.as_deref(), Some("hw:0"));
        assert_eq!(loaded.output_device.as_deref(), Some("pulse"));
        assert_eq!(loaded.camera_index, Some(2));
        assert_eq!(loaded.screen_source_id.as_deref(), Some("display:1"));
        assert!(
            loaded.noise_gate_enabled,
            "defaults apply for missing fields"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_default() {
        let dir =
            std::env::temp_dir().join(format!("zancord-config-missing-{}", std::process::id()));
        let path = dir.join("config.json");
        let loaded = AppConfig::load_from(&path);
        assert_eq!(loaded.username, None);
        assert!(loaded.hpf_enabled);
    }

    #[test]
    fn corrupt_file_falls_back_to_defaults() {
        let dir = std::env::temp_dir().join(format!("zancord-config-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.json");
        std::fs::write(&path, "{ not json").expect("write garbage");
        let loaded = AppConfig::load_from(&path);
        assert_eq!(loaded.input_device, None);
        assert_eq!(loaded.noise_gate_threshold_db, -40.0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
