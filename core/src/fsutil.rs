// SPDX-License-Identifier: AGPL-3.0-or-later
//! Files that are whole or not there. A file of the app that another part
//! reads later (the index of the downloads, a `meta.json`, the positions
//! of offline plays, an image, a subtitle) is written to a name beside
//! it, synced, and renamed into its place: a crash, a power cut or a full
//! disk leaves the file as it was, never cut short and never empty.

use std::{
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
};

/// The name the bytes go to first: `<name>.tmp<pid>` in the same folder.
/// The process id keeps two instances of the app off one name.
fn partial_of(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".tmp{}", std::process::id()));
    path.with_file_name(name)
}

/// Writes `bytes` to `path`, whole or not at all.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic_with(path, None, |file| file.write_all(bytes))
}

/// The same for a file that comes in pieces: `fill` writes it. An error of
/// `fill` leaves `path` as it was. `mode` is the mode of the file on Unix
/// (0o600 for a file that holds tokens), set when the file is made, so it
/// is at no time open to others; `None` leaves it to the umask.
pub fn write_atomic_with<T>(
    path: &Path,
    mode: Option<u32>,
    fill: impl FnOnce(&mut fs::File) -> io::Result<T>,
) -> io::Result<T> {
    let partial = partial_of(path);
    // One a run before left has the mode it was made with.
    let _ = fs::remove_file(&partial);
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let result = (|| {
        let mut file = options.open(&partial)?;
        let out = fill(&mut file)?;
        file.sync_all()?;
        fs::rename(&partial, path)?;
        Ok(out)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&partial);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bloom-fsutil-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|f| f.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_write_puts_the_file_in_place_and_leaves_no_other() {
        let dir = temp("write");
        let path = dir.join("index.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(names(&dir), ["index.json"]);
        let _ = fs::remove_dir_all(dir);
    }

    /// A file with a mode has it from the first byte on, also over a
    /// file that was open to all and over a leftover of a run before.
    #[cfg(unix)]
    #[test]
    fn a_file_with_a_mode_is_made_with_it() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = temp("mode");
        let path = dir.join("config.json");
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(partial_of(&path), b"left over").unwrap();
        let seen = write_atomic_with(&path, Some(0o600), |file| {
            file.write_all(b"token")?;
            Ok(mode(&partial_of(&path)))
        })
        .unwrap();
        assert_eq!(seen, 0o600, "the file was open to others while it was written");
        assert_eq!(mode(&path), 0o600);
        assert_eq!(fs::read(&path).unwrap(), b"token");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_write_that_fails_leaves_the_file_as_it_was() {
        let dir = temp("fail");
        let path = dir.join("meta.json");
        write_atomic(&path, b"whole").unwrap();
        let failed = write_atomic_with(&path, None, |file| {
            file.write_all(b"ha")?;
            Err::<(), _>(io::Error::other("the line was cut"))
        });
        assert!(failed.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"whole");
        assert_eq!(names(&dir), ["meta.json"]);
        let _ = fs::remove_dir_all(dir);
    }
}
