// SPDX-License-Identifier: AGPL-3.0-or-later
//! The index on the disk and the folder it lists: reading it at the
//! start, the lock of the folder, and what the folder holds that the
//! index does not say.

use super::*;

/// The index, as the engine starts on it: what the run before left
/// unfinished goes on, and the part files say how far it got. Each entry
/// gets a generation after `generations`.
pub(super) fn load_entries(dir: &Path, generations: &mut u64) -> Vec<Entry> {
    let mut entries = load_index(dir);
    for entry in &mut entries {
        // A download that was under way, and one that gave up (the cause
        // may be gone), go on.
        if matches!(entry.state, EntryState::Downloading | EntryState::Failed) {
            entry.requeue();
        }
        // The part file knows better than the index how far it got.
        if entry.state != EntryState::Done
            && let Some(file) = &entry.file
        {
            let part = dir.join(&entry.server_id).join(&entry.item_id).join(format!("{file}.part"));
            if let Ok(meta) = fs::metadata(part) {
                entry.done = meta.len();
            }
        }
        entry.speed = 0.;
        entry.cancel = Arc::new(AtomicBool::new(false));
        *generations += 1;
        entry.generation = Generation(*generations);
        entry.admitted = false;
    }
    entries
}

/// The folders of a server or of its items.
pub(super) fn subfolders(dir: &Path) -> Vec<fs::DirEntry> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .collect()
}

/// Puts right what the disk holds and the index does not say, once, when
/// the engine has the folder. The folder of an item with a `meta.json`
/// and no entry is a download whose entry was lost (an index that did not
/// read, or one written over): it gets an entry again, done or paused,
/// and no file of it is touched. Before, such a folder kept its
/// gigabytes for good, counted nowhere. A folder that a removal took out
/// of the way and did not delete (the app ended first) goes now.
pub(super) fn recover(inner: &Inner) {
    let (dir, known) = {
        let s = inner.lock();
        let known: HashSet<(String, String)> =
            s.entries.iter().map(|e| (e.server_id.clone(), e.item_id.clone())).collect();
        (s.dir.clone(), known)
    };
    let (mut found, mut gone) = (Vec::new(), Vec::new());
    for server in subfolders(&dir) {
        let server_id = server.file_name().to_string_lossy().into_owned();
        if check_id("server id", &server_id).is_err() {
            continue;
        }
        for item in subfolders(&server.path()) {
            let item_id = item.file_name().to_string_lossy().into_owned();
            if item_id.ends_with(GONE) {
                gone.push(item.path());
            } else if check_id("item id", &item_id).is_ok()
                && !known.contains(&(server_id.clone(), item_id.clone()))
                && let Some(entry) = Entry::from_folder(&server_id, &item_id, &item.path())
            {
                found.push(entry);
            }
        }
    }
    if !found.is_empty() {
        let mut s = inner.lock();
        let mut adopted = false;
        for mut entry in found {
            // Added or removed since the look at the disk: the user's
            // word stands.
            if s.at(Some(&entry.server_id), &entry.item_id).is_some()
                || s.tombstoned(&entry.server_id, &entry.item_id)
                || !s.folder(&entry).join(META_FILE).is_file()
            {
                continue;
            }
            log::info!("a download on the disk had no entry and has one again: {}", entry.title());
            entry.generation = s.next_generation();
            s.entries.push(entry);
            adopted = true;
        }
        inner.commit(s, adopted);
    }
    for folder in gone {
        let _ = fs::remove_dir_all(folder);
    }
}

pub(super) fn load_index(dir: &Path) -> Vec<Entry> {
    // A file that does not read as the index, whole or at one entry, is
    // kept as `<name>.broken` before the next save writes over it: the
    // folders of its downloads are still on the disk. An entry this build
    // cannot read (a state of a newer one) costs that entry, not the list.
    let path = dir.join(INDEX_FILE);
    let Ok(bytes) = fs::read(&path) else {
        return Vec::new();
    };
    let listed: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap_or_else(|err| {
        log::warn!("downloads index does not parse ({err}); a copy is kept");
        crate::config::keep_broken_copy(&path, &bytes);
        Vec::new()
    });
    let count = listed.len();
    let entries: Vec<Entry> = listed
        .into_iter()
        .filter_map(|entry| match serde_json::from_value(entry) {
            Ok(entry) => Some(entry),
            Err(err) => {
                log::warn!("download entry not read: {err}");
                None
            }
        })
        .collect();
    if entries.len() != count {
        crate::config::keep_broken_copy(&path, &bytes);
    }
    // An entry with an unsafe id or file name is refused, not repaired. Its
    // folder is left alone.
    entries
        .into_iter()
        .filter(|entry| match entry.validate() {
            Ok(()) => true,
            Err(err) => {
                log::warn!("download entry refused: {err:#}");
                false
            }
        })
        .collect()
}

/// Takes the lock of the folder, so one process downloads into it at a
/// time. The lock is the open file: it goes when the file is dropped, and
/// with the process. `None` when another process holds it.
pub(super) fn lock_folder(dir: &Path) -> Option<fs::File> {
    let file = fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(LOCK_FILE))
        .ok()?;
    file.try_lock().ok()?;
    Some(file)
}
