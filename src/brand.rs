// SPDX-License-Identifier: AGPL-3.0-or-later
//! The name of the app, in one place. To rename the app, change `NAME` and
//! `FOLDER` here (and add the folder before to `OLD_FOLDERS`); `dev/bundle`
//! reads the same lines.

use std::path::Path;

/// The name people see: the menu bar, the window, the Devices list of the
/// server.
pub const NAME: &str = "Bloom";
/// The line under the name.
pub const TAGLINE: &str = "Jellyfin client";
/// The folder of the app in `~/Library/Application Support` and
/// `~/Library/Caches`.
pub const FOLDER: &str = "bloom";
/// Folders of earlier names. Their content moves to `FOLDER` at the start.
const OLD_FOLDERS: [&str; 1] = ["jellyui"];

/// Moves the folders of an earlier name of the app to the folder of this
/// name, once: the sign-in, the settings, the downloads and the image cache
/// stay. Does nothing when the new folder is there.
pub fn migrate_folders() {
    // A test instance with a config file of its own leaves the folders of
    // the user alone.
    if std::env::var_os("BLOOM_CONFIG_PATH").is_some() {
        return;
    }
    for base in [dirs::config_dir(), dirs::cache_dir()].into_iter().flatten() {
        migrate(&base);
    }
}

fn migrate(base: &Path) {
    let new = base.join(FOLDER);
    if new.exists() {
        return;
    }
    for old in OLD_FOLDERS {
        let old = base.join(old);
        if old.is_dir() {
            match std::fs::rename(&old, &new) {
                Ok(()) => log::info!("moved {} to {}", old.display(), new.display()),
                Err(err) => log::warn!("could not move {}: {err}", old.display()),
            }
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folder_of_the_old_name_moves_once() {
        let base = std::env::temp_dir().join(format!("brand-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("jellyui")).unwrap();
        std::fs::write(base.join("jellyui/config.json"), b"{}").unwrap();
        migrate(&base);
        assert!(base.join(FOLDER).join("config.json").exists());
        assert!(!base.join("jellyui").exists());
        // A folder of the old name that comes back later is left alone.
        std::fs::create_dir_all(base.join("jellyui")).unwrap();
        migrate(&base);
        assert!(base.join("jellyui").exists());
        let _ = std::fs::remove_dir_all(&base);
    }
}
