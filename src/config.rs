// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Persistent app configuration: servers, per-server profiles, active selection.
//!
//! Stored as JSON under the OS config directory with owner-only permissions,
//! because it holds Jellyfin access tokens.

use std::{
    fs,
    path::{Path, PathBuf},
};

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
    /// The debug channel: a socket in the folder of the app through which a
    /// tool on this Mac can drive the app. Off unless set to true.
    #[serde(default)]
    pub debug_channel: Option<bool>,
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
        Self::load_from(&Self::path())
    }

    /// A missing file is the first start: the default. A file that is there
    /// but does not parse is copied to `<name>.broken` first (the next save
    /// would overwrite the servers and tokens in it), and a field of the
    /// wrong type is dropped alone, so the rest of the config stays.
    fn load_from(path: &Path) -> Self {
        let mut config = match fs::read(path) {
            Ok(bytes) => match serde_json::from_slice::<Config>(&bytes) {
                Ok(config) => config,
                Err(err) => {
                    // The message has a line and column, never file content.
                    log::warn!("config {} does not parse ({err}); keeping a copy", path.display());
                    keep_broken_copy(path, &bytes);
                    salvage(&bytes)
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(err) => {
                log::warn!("config {} cannot be read: {err}", path.display());
                Config::default()
            }
        };
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
        self.save_to(&Self::path())
    }

    fn save_to(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(self)?;
        write_private(path, &json).with_context(|| format!("write {}", path.display()))
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

/// Writes `bytes` to a temp file in the same folder (mode 0600 from the
/// start, the file holds tokens), syncs it, and renames it over `path`: a
/// crash or a full disk leaves the old file whole.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(".tmp{}", std::process::id()));
    let tmp = path.with_file_name(tmp_name);
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Copies an unreadable config to `<name>.broken`. An existing copy with the
/// same content stays; one with other content is replaced (the latest
/// failure is the one worth looking at).
fn keep_broken_copy(path: &Path, bytes: &[u8]) {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".broken");
    let copy = path.with_file_name(name);
    if fs::read(&copy).is_ok_and(|old| old == bytes) {
        return;
    }
    if let Err(err) = write_private(&copy, bytes) {
        log::warn!("cannot keep a copy of the broken config: {err:#}");
    }
}

/// Keeps every top-level field of a JSON object that fits its type; the
/// others take their default. Not an object: the default.
fn salvage(bytes: &[u8]) -> Config {
    let Ok(serde_json::Value::Object(fields)) = serde_json::from_slice(bytes) else {
        return Config::default();
    };
    let mut kept = serde_json::Map::new();
    for (key, value) in fields {
        let mut trial = kept.clone();
        trial.insert(key.clone(), value.clone());
        if serde_json::from_value::<Config>(trial.into()).is_ok() {
            kept.insert(key, value);
        }
    }
    serde_json::from_value(kept.into()).unwrap_or_default()
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

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bloom-config-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample() -> Config {
        let mut config = Config::default();
        config.device_id = "dev".into();
        config.upsert_server(Server {
            id: "s".into(),
            name: "S".into(),
            url: "http://s".into(),
            profiles: vec![],
        });
        config.upsert_profile(
            "s",
            Profile { user_id: "u".into(), name: "U".into(), token: "secret".into(), image_tag: None },
        );
        config
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = temp_dir("round");
        let path = dir.join("config.json");
        sample().save_to(&path).unwrap();
        let loaded = Config::load_from(&path);
        assert_eq!(loaded.device_id, "dev");
        assert_eq!(loaded.server("s").unwrap().profiles[0].token, "secret");
        assert!(!dir.join("config.json.broken").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_the_default_and_makes_no_copy() {
        let dir = temp_dir("missing");
        let loaded = Config::load_from(&dir.join("config.json"));
        assert!(loaded.servers.is_empty());
        assert!(!loaded.device_id.is_empty());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn truncated_file_is_kept_as_broken() {
        let dir = temp_dir("broken");
        let path = dir.join("config.json");
        sample().save_to(&path).unwrap();
        let whole = fs::read(&path).unwrap();
        let cut = &whole[..whole.len() / 2];
        fs::write(&path, cut).unwrap();
        let loaded = Config::load_from(&path);
        assert!(loaded.servers.is_empty());
        assert_eq!(fs::read(dir.join("config.json.broken")).unwrap(), cut);
        // The same content again leaves the copy; other content replaces it.
        Config::load_from(&path);
        fs::write(&path, b"{ not json").unwrap();
        Config::load_from(&path);
        assert_eq!(fs::read(dir.join("config.json.broken")).unwrap(), b"{ not json");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrong_type_drops_one_field_only() {
        let dir = temp_dir("lenient");
        let path = dir.join("config.json");
        let mut value = serde_json::to_value(sample()).unwrap();
        value["dark"] = serde_json::json!("yes");
        value["unknown_future_field"] = serde_json::json!(1);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let loaded = Config::load_from(&path);
        assert_eq!(loaded.dark, None);
        assert_eq!(loaded.servers.len(), 1);
        assert!(dir.join("config.json.broken").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_and_missing_fields_load() {
        let dir = temp_dir("fields");
        let path = dir.join("config.json");
        fs::write(&path, br#"{"device_id":"d","from_the_future":{"a":1}}"#).unwrap();
        let loaded = Config::load_from(&path);
        assert_eq!(loaded.device_id, "d");
        assert!(!dir.join("config.json.broken").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_is_private_and_leaves_no_temp_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("mode");
        let path = dir.join("config.json");
        sample().save_to(&path).unwrap();
        sample().save_to(&path).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("config.json")]);
        // A broken copy is private too.
        fs::write(&path, b"{").unwrap();
        Config::load_from(&path);
        let mode = fs::metadata(dir.join("config.json.broken")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = fs::remove_dir_all(&dir);
    }
}
