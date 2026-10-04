// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Persistent app configuration: servers, per-server profiles, active selection.
//!
//! Stored as JSON under the OS config directory with owner-only permissions,
//! because it holds Jellyfin access tokens.

use std::{fs, path::PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

/// The client name the server sees (see `brand.rs`).
pub const APP_NAME: &str = crate::brand::NAME;
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Config {
    /// Stable per-installation id sent to Jellyfin as `DeviceId`.
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub servers: Vec<Server>,
    /// Server id and user id of the profile to open on launch.
    #[serde(default)]
    pub active: Option<(String, String)>,
    #[serde(default)]
    pub dark: Option<bool>,
    /// Trailer video behind the home hero; on unless set to false.
    #[serde(default)]
    pub hero_video: Option<bool>,
    /// Quality tags on poster cards. Unset follows the user's setting in the
    /// Jellyfin Enhanced plugin.
    #[serde(default)]
    pub quality_tags: Option<bool>,
    /// Features of the Jellyfin Enhanced plugin set in this app, by name.
    /// One that is not here follows the user's setting in the plugin.
    #[serde(default)]
    pub enhanced: std::collections::HashMap<String, bool>,
    /// SyncPlay: correct the position when it drifts from the group. On
    /// unless set to false.
    #[serde(default)]
    pub sync_correction: Option<bool>,
    /// Other devices may control this app ("Play On"). On unless set to
    /// false.
    #[serde(default)]
    pub remote_control: Option<bool>,
    /// SyncPlay: milliseconds this player plays later (or, negative,
    /// earlier) than the group, for a sound output with a delay of its own.
    #[serde(default)]
    pub sync_offset_ms: f64,
    /// Look of the subtitles in the player, as set in this app.
    #[serde(default)]
    pub subtitle_look: SubtitleLook,
    /// Bits per second the player may stream; above it the server
    /// transcodes. Unset is no limit ("Auto").
    #[serde(default)]
    pub max_bitrate: Option<u64>,
    /// "Auto" measures the connection and lowers the quality when the
    /// file would not keep up. On unless set to false.
    #[serde(default)]
    pub adaptive_quality: Option<bool>,
    /// The speed of the connection at the last measurement, in bits per
    /// second, and when (seconds since the Unix epoch).
    #[serde(default)]
    pub link_bps: Option<u64>,
    #[serde(default)]
    pub link_measured_at: Option<u64>,
    /// Window position and size at the last run: x, y, width, height.
    #[serde(default)]
    pub window: Option<[f32; 4]>,
    /// Place of the picture-in-picture window, in screen coordinates with
    /// the origin at the bottom left: x, y, width, height.
    #[serde(default)]
    pub pip_window: Option<[f64; 4]>,
    /// Space the downloads may take, in GB; none for no limit.
    #[serde(default)]
    pub download_limit_gb: Option<u64>,
    /// Downloads that run at the same time: 1 or 2.
    #[serde(default)]
    pub download_parallel: Option<u8>,
}

/// Look of the subtitles. A part that is not set follows the user's
/// subtitle style in the Jellyfin Enhanced plugin, or the player's own look.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SubtitleLook {
    /// Size against the normal size.
    #[serde(default)]
    pub scale: Option<f64>,
    /// Text colour as "#AARRGGBB".
    #[serde(default)]
    pub color: Option<String>,
    /// A dark box behind the text.
    #[serde(default)]
    pub background: Option<bool>,
    /// Place from the top, in percent; 100 is the lower edge.
    #[serde(default)]
    pub position: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Server {
    /// Jellyfin's own server id from `/System/Info/Public`.
    pub id: String,
    pub name: String,
    /// Base URL without trailing slash, e.g. `https://media.example.com`.
    pub url: String,
    #[serde(default)]
    pub profiles: Vec<Profile>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Profile {
    pub user_id: String,
    pub name: String,
    pub token: String,
    #[serde(default)]
    pub image_tag: Option<String>,
}

impl Config {
    pub fn path() -> PathBuf {
        // A test instance can use its own file, for example an empty one
        // to see the sign-in page.
        if let Some(path) = std::env::var_os("BLOOM_CONFIG_PATH") {
            return PathBuf::from(path);
        }
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(crate::brand::FOLDER)
            .join("config.json")
    }

    pub fn load() -> Self {
        let mut config = fs::read(Self::path())
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Config>(&bytes).ok())
            .unwrap_or_default();
        if config.device_id.is_empty() {
            config.device_id = uuid::Uuid::new_v4().to_string();
        }
        config
    }

    pub fn save(&self) -> Result<()> {
        // A second instance started for tests must not change the settings
        // of the instance the user runs.
        if std::env::var_os("BLOOM_CONFIG_READONLY").is_some() {
            return Ok(());
        }
        let path = Self::path();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
        let json = serde_json::to_vec_pretty(self)?;
        fs::write(&path, json).with_context(|| format!("write {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    pub fn server(&self, id: &str) -> Option<&Server> {
        self.servers.iter().find(|s| s.id == id)
    }

    pub fn server_mut(&mut self, id: &str) -> Option<&mut Server> {
        self.servers.iter_mut().find(|s| s.id == id)
    }

    /// Inserts or replaces a server by id, keeping existing profiles.
    pub fn upsert_server(&mut self, server: Server) {
        match self.server_mut(&server.id) {
            Some(existing) => {
                existing.name = server.name;
                existing.url = server.url;
            }
            None => self.servers.push(server),
        }
    }

    pub fn upsert_profile(&mut self, server_id: &str, profile: Profile) {
        if let Some(server) = self.server_mut(server_id) {
            server.profiles.retain(|p| p.user_id != profile.user_id);
            server.profiles.push(profile);
        }
    }

    pub fn remove_profile(&mut self, server_id: &str, user_id: &str) {
        if let Some(server) = self.server_mut(server_id) {
            server.profiles.retain(|p| p.user_id != user_id);
        }
        if self
            .active
            .as_ref()
            .is_some_and(|(s, u)| s == server_id && u == user_id)
        {
            self.active = None;
        }
    }

    pub fn remove_server(&mut self, server_id: &str) {
        self.servers.retain(|s| s.id != server_id);
        if self.active.as_ref().is_some_and(|(s, _)| s == server_id) {
            self.active = None;
        }
    }
}

/// Normalizes user input like `media.example.com:8096/` into a base URL.
pub fn normalize_url(input: &str) -> String {
    let trimmed = input.trim().trim_end_matches('/');
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_urls() {
        assert_eq!(
            normalize_url(" media.example.com:8096/ "),
            "http://media.example.com:8096"
        );
        assert_eq!(
            normalize_url("https://x.example.com/"),
            "https://x.example.com"
        );
    }

    #[test]
    fn profiles_round_trip() {
        let mut config = Config::default();
        config.upsert_server(Server {
            id: "s".into(),
            name: "S".into(),
            url: "http://s".into(),
            profiles: vec![],
        });
        config.upsert_profile(
            "s",
            Profile {
                user_id: "u".into(),
                name: "U".into(),
                token: "t".into(),
                image_tag: None,
            },
        );
        config.upsert_profile(
            "s",
            Profile {
                user_id: "u".into(),
                name: "U2".into(),
                token: "t2".into(),
                image_tag: None,
            },
        );
        assert_eq!(config.server("s").unwrap().profiles.len(), 1);
        assert_eq!(config.server("s").unwrap().profiles[0].name, "U2");
        config.active = Some(("s".into(), "u".into()));
        config.remove_profile("s", "u");
        assert!(config.active.is_none());
        config.upsert_server(Server {
            id: "s".into(),
            name: "Renamed".into(),
            url: "http://s2".into(),
            profiles: vec![],
        });
        assert_eq!(config.servers.len(), 1);
        assert_eq!(config.server("s").unwrap().name, "Renamed");
    }
}
