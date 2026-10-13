// SPDX-License-Identifier: AGPL-3.0-or-later
//! Downloads for offline use, without the app: the engine that fetches and
//! keeps the files, and the positions of offline plays. The app adds the
//! page, the notices and the offline mode (`src/downloads`).

use std::{path::PathBuf, sync::OnceLock};

pub mod engine;
pub mod offline;
mod stall;

/// The engine of this process, from the time the app opens it
/// (`src/downloads`).
pub static ENGINE: OnceLock<engine::Engine> = OnceLock::new();

/// The artwork of a download for an image URL of the server; see
/// [`engine::Engine::local_image`]. The image loader asks before the network.
pub fn local_image(url: &str) -> Option<PathBuf> {
    ENGINE.get()?.local_image(url)
}

/// Folder of the downloads.
pub fn dir() -> PathBuf {
    if let Some(dir) = &crate::options::options().download_dir {
        return dir.clone();
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(crate::brand::FOLDER)
        .join("downloads")
}
