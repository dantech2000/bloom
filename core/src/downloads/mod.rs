// SPDX-License-Identifier: AGPL-3.0-or-later
//! Downloads for offline use, without the app: the engine that fetches and
//! keeps the files, and the positions of offline plays. The app adds the
//! page, the notices and the offline mode (`src/downloads`).

use std::path::PathBuf;

pub mod engine;
pub mod offline;
mod stall;

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
