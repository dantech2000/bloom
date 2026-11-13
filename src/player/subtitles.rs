//! The subtitle files of a load, fetched on a thread of their own so the
//! worker never waits for the network.

use super::*;

/// How long a subtitle file may take to come. The server makes a text
/// subtitle of a transcode at the first request for it, out of the whole
/// file: about 80 s for a 9 GB episode (measured 2026-10-17).
pub(super) const SUBTITLE_WAIT: Duration = Duration::from_secs(180);
/// A subtitle file larger than this is not one. A PGS track of a film is
/// 20 to 60 MB, one of a 4K film about 100 MB.
pub(super) const SUBTITLE_LIMIT: u64 = 256 * 1024 * 1024;

/// Fetches the subtitle files of a load on a thread of its own and hands
/// each to the worker as `Cmd::AddSubtitle`, in their order. mpv's own
/// `sub-add` of an address holds its caller until the file is there, and
/// gives up after a minute: on the worker that held every event and
/// command for the time of a slow server, the stop of the user among them
/// (2026-10-17, a transcode of an episode whose subtitles the server
/// first had to make). A load that was replaced or stopped meanwhile gets
/// no files. An image track that does not start shown is left for the
/// moment the user chooses it (`Cmd::FetchSubtitle`): a film's ten PGS
/// tracks are 300 MB, which would go with the video over the link. A file
/// that is on its way already is not fetched again (two fetches would
/// write one file, the second over what mpv reads of the first): a choice
/// of it is kept in its `Fetch` and counts when the file comes.
pub(super) fn fetch_subtitles(
    shared: Arc<Shared>,
    tx: mpsc::Sender<Cmd>,
    client: Client,
    generation: u64,
    subtitles: Vec<SubtitleFile>,
) {
    let subtitles = wanted_files(&mut shared.fetching.lock().unwrap(), generation, subtitles);
    for sub in &subtitles {
        set_pending(&shared, generation, sub.index, true);
    }
    if subtitles.is_empty() {
        return;
    }
    let _ = thread::Builder::new().name("subtitles".into()).spawn(move || {
        let dir = std::env::temp_dir().join("bloom-subtitles");
        let live = || shared.load_gen.load(Ordering::Acquire) == generation;
        for sub in subtitles {
            if !live() {
                return;
            }
            // The extension tells mpv the format ahead of the content.
            let path = dir.join(format!("{}-{generation}-{}.{}", std::process::id(), sub.index, subtitle_extension(&sub.url)));
            let fetched = std::fs::create_dir_all(&dir)
                .map_err(anyhow::Error::from)
                .and_then(|()| fetch_subtitle(&client, &sub.url, &path, &live));
            // The file may have been chosen while it was on its way.
            let select = fetch_over(&mut shared.fetching.lock().unwrap(), generation, sub.index).unwrap_or(sub.select);
            if let Err(err) = fetched {
                log::warn!("subtitle {:?} not fetched: {err:#}", sub.title);
                let _ = std::fs::remove_file(&path);
                set_pending(&shared, generation, sub.index, false);
                let mut status = shared.status.lock().unwrap();
                if select && status.generation == generation {
                    status.failed_subs.push(sub.index);
                    status.tracks_version += 1;
                }
                continue;
            }
            let sent = tx.send(Cmd::AddSubtitle {
                generation,
                index: sub.index,
                path,
                select,
                title: sub.title,
                lang: sub.lang,
            });
            // No worker is there to add the file, or to remove it later.
            if let Err(mpsc::SendError(cmd)) = sent {
                if let Cmd::AddSubtitle { path, .. } = cmd {
                    let _ = std::fs::remove_file(path);
                }
                return;
            }
            // The worker sleeps between rounds.
            shared.wake();
        }
    });
}

/// The files of a load to fetch now: those that start shown and the text
/// ones (an image track waits for its choice), less the ones on their way
/// already, whose `Fetch` takes the choice instead. The list forgets the
/// loads before this one.
pub(super) fn wanted_files(fetching: &mut Vec<Fetch>, generation: u64, subtitles: Vec<SubtitleFile>) -> Vec<SubtitleFile> {
    fetching.retain(|fetch| fetch.generation == generation);
    subtitles
        .into_iter()
        .filter(|sub| sub.select || !sub.graphical)
        .filter(|sub| match fetching.iter_mut().find(|fetch| fetch.index == sub.index) {
            Some(fetch) => {
                fetch.select |= sub.select;
                false
            }
            None => {
                fetching.push(Fetch { generation, index: sub.index, select: sub.select });
                true
            }
        })
        .collect()
}

/// A fetch is over: whether its file is shown, as asked at the start or
/// chosen since. None when a later load took the list.
pub(super) fn fetch_over(fetching: &mut Vec<Fetch>, generation: u64, index: i64) -> Option<bool> {
    let at = fetching.iter().position(|fetch| fetch.generation == generation && fetch.index == index)?;
    Some(fetching.swap_remove(at).select)
}

/// Marks a subtitle file of the load `generation` as on its way, or as
/// come: the menu says "Loading…" meanwhile. Nothing for a status that is
/// of another load by now: the same item loaded again has the same indexes.
pub(super) fn set_pending(shared: &Shared, generation: u64, index: i64, pending: bool) {
    let mut status = shared.status.lock().unwrap();
    if status.generation != generation {
        return;
    }
    let was = status.pending_subs.contains(&index);
    match (pending, was) {
        (true, false) => status.pending_subs.push(index),
        (false, true) => status.pending_subs.retain(|i| *i != index),
        _ => return,
    }
    status.tracks_version += 1;
}

/// One subtitle file into `path`, with the token in the header as every
/// request. The body goes to the file as it comes: an image track is tens
/// of MB, and the memory of the app need not hold it. It goes in pieces,
/// and the transfer ends when `live` says the load is over: the rest of a
/// 60 MB track is of no use to an item that was stopped.
pub(super) fn fetch_subtitle(client: &Client, url: &str, path: &std::path::Path, live: &dyn Fn() -> bool) -> Result<()> {
    use std::io::{Read as _, Write as _};
    let mut response = client
        .agent()
        .get(url)
        .config()
        .timeout_global(Some(SUBTITLE_WAIT))
        .build()
        .header("Authorization", client.auth_header())
        .call()?;
    let mut reader = response.body_mut().with_config().limit(SUBTITLE_LIMIT).reader();
    let mut file = std::fs::File::create(path)?;
    let mut piece = vec![0u8; 64 * 1024];
    loop {
        let read = match reader.read(&mut piece) {
            Ok(0) => return Ok(()),
            Ok(read) => read,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err.into()),
        };
        if !live() {
            return Err(anyhow!("the item is over"));
        }
        file.write_all(&piece[..read])?;
    }
}

/// The extension of the file written for a subtitle address: the format
/// the server names, as mpv knows it. The server's names for the two
/// image formats are not file extensions (ffmpeg refuses `.pgssub`), and
/// a VobSub comes in an `.mks`. A text format keeps its name; anything
/// else is taken for SubRip, the format the server converts to.
pub(super) fn subtitle_extension(url: &str) -> String {
    let name = url.split(['?', '#']).next().unwrap_or(url);
    let extension = match name.rsplit_once('.') {
        Some((_, extension)) if !extension.contains('/') => extension.to_ascii_lowercase(),
        _ => String::new(),
    };
    match extension.as_str() {
        "pgssub" | "pgs" | "sup" => "sup".into(),
        "vobsub" | "dvdsub" | "mks" => "mks".into(),
        "srt" | "ass" | "ssa" | "vtt" => extension,
        _ => "srt".into(),
    }
}
