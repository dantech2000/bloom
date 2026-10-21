// SPDX-License-Identifier: AGPL-3.0-or-later
//! The switches of the environment (`BLOOM_*`), read once at the first use.
//! They are for tests and measurements; a user sets none of them. A new
//! switch goes into [`Options`], so this file lists them all. Two stay
//! out: `BLOOM_TEST_REAL_AUDIO` and the `BLOOM_CI_*` pair belong to the
//! tests of the player (`src/player.rs`).

use std::{path::PathBuf, sync::OnceLock};

pub struct Options {
    /// `BLOOM_TEST_NAME`: this is a test instance of that name. It has its
    /// own sockets, shows the name in its window and does not take the
    /// keyboard focus.
    pub test_name: Option<String>,
    /// `BLOOM_CONFIG_PATH`: the config file, in place of the user's.
    pub config_path: Option<PathBuf>,
    /// `BLOOM_CONFIG_READONLY`: no save of the config reaches the file.
    pub config_readonly: bool,
    /// `BLOOM_DEBUG_SOCK`: the debug channel listens on this socket.
    pub debug_sock: Option<String>,
    /// `BLOOM_MUTED`: the player starts muted and stays muted.
    pub muted: bool,
    /// `BLOOM_NO_REPORT`: no playback report goes to the server.
    pub no_report: bool,
    /// `BLOOM_NO_SOCKET`: no socket to the server (SyncPlay, live events).
    pub no_socket: bool,
    /// `BLOOM_OFFLINE`: the app starts in its offline mode.
    pub offline: bool,
    /// `BLOOM_AUTO_PAUSE`: a read-only instance pauses out of view as well.
    pub auto_pause: bool,
    /// `BLOOM_ALLOW_SUBTITLE_WRITE=1`: the debug channel may add and delete
    /// subtitles on the server.
    pub allow_subtitle_write: bool,
    /// `BLOOM_CHROMECAST_MOCK`: a Chromecast in this process, for tests.
    pub chromecast_mock: bool,
    /// `BLOOM_HERO_PLAY_INACTIVE`: the trailer of the hero plays while the
    /// app is not the active one.
    pub hero_play_inactive: bool,
    /// `BLOOM_APPEARANCE=light|dark|system`.
    pub appearance: Option<String>,
    /// `BLOOM_GLASS=liquid|frosted`: wins over the setting of the user.
    pub glass: Option<String>,
    /// `BLOOM_NO_GLASS`: a plain tint with no blur pass, for a benchmark.
    pub no_glass: bool,
    /// `BLOOM_NO_DISK_CACHE`: images are not kept on the disk.
    pub no_disk_cache: bool,
    /// `BLOOM_CACHE_DIR`: the folder of the image cache.
    pub cache_dir: Option<PathBuf>,
    /// `BLOOM_DOWNLOAD_DIR`: the folder of the downloads.
    pub download_dir: Option<PathBuf>,
    /// `BLOOM_DOWNLOAD_LIMIT_BYTES`, `BLOOM_DOWNLOAD_THROTTLE_BPS`: a
    /// download stops at this size, and runs no faster than this.
    pub download_limit_bytes: Option<u64>,
    pub download_throttle_bps: Option<u64>,
    /// `BLOOM_FRAME_PHASE_MS`: when after its tick a video frame is drawn.
    pub frame_phase_ms: Option<u64>,
    /// `BLOOM_SERVER_URL`: the address the session uses in place of the
    /// server's, for a test of another path to the same server (its LAN
    /// address, its Tailscale address) with the stored session.
    pub server_url: Option<String>,
    /// `BLOOM_MPV_OPTS=name=value,name=value`: mpv options set at the start
    /// of each player core after the app's own, for a test of an option
    /// that only takes effect at the start (a shader cache, a renderer).
    pub mpv_opts: Vec<(String, String)>,
    /// `BLOOM_PACING_TRACE`: the pacing trace is on from the start.
    pub pacing_trace: bool,
    /// `BLOOM_PACING_LOG`: every settled frame is written to this file.
    pub pacing_log: Option<PathBuf>,
}

impl Options {
    fn from_env() -> Self {
        let set = |name: &str| std::env::var_os(name).is_some();
        let text = |name: &str| std::env::var(name).ok();
        let path = |name: &str| std::env::var_os(name).map(PathBuf::from);
        let number = |name: &str| std::env::var(name).ok().and_then(|value| value.parse::<u64>().ok());
        Self {
            test_name: text("BLOOM_TEST_NAME"),
            config_path: path("BLOOM_CONFIG_PATH"),
            config_readonly: set("BLOOM_CONFIG_READONLY"),
            debug_sock: text("BLOOM_DEBUG_SOCK"),
            muted: set("BLOOM_MUTED"),
            no_report: set("BLOOM_NO_REPORT"),
            no_socket: set("BLOOM_NO_SOCKET"),
            offline: set("BLOOM_OFFLINE"),
            auto_pause: set("BLOOM_AUTO_PAUSE"),
            allow_subtitle_write: text("BLOOM_ALLOW_SUBTITLE_WRITE").as_deref() == Some("1"),
            chromecast_mock: set("BLOOM_CHROMECAST_MOCK"),
            hero_play_inactive: set("BLOOM_HERO_PLAY_INACTIVE"),
            appearance: text("BLOOM_APPEARANCE"),
            glass: text("BLOOM_GLASS"),
            no_glass: set("BLOOM_NO_GLASS"),
            no_disk_cache: set("BLOOM_NO_DISK_CACHE"),
            cache_dir: path("BLOOM_CACHE_DIR"),
            download_dir: path("BLOOM_DOWNLOAD_DIR"),
            download_limit_bytes: number("BLOOM_DOWNLOAD_LIMIT_BYTES"),
            download_throttle_bps: number("BLOOM_DOWNLOAD_THROTTLE_BPS"),
            frame_phase_ms: number("BLOOM_FRAME_PHASE_MS"),
            server_url: std::env::var("BLOOM_SERVER_URL").ok().filter(|v| !v.is_empty()),
            mpv_opts: std::env::var("BLOOM_MPV_OPTS")
                .ok()
                .map(|list| {
                    list.split(',')
                        .filter_map(|pair| pair.split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
                        .collect()
                })
                .unwrap_or_default(),
            pacing_trace: set("BLOOM_PACING_TRACE"),
            pacing_log: path("BLOOM_PACING_LOG"),
        }
    }
}

/// The switches this process started with.
pub fn options() -> &'static Options {
    static OPTIONS: OnceLock<Options> = OnceLock::new();
    OPTIONS.get_or_init(Options::from_env)
}
