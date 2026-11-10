// SPDX-License-Identifier: AGPL-3.0-or-later
//! The scheduler and the workers: who downloads what, and one download
//! from its first request to the rename of its part file.

use super::*;

/// Starts workers for queued entries while there is room. Each worker gets
/// its job here, under the lock, so what it works on is fixed at the start.
pub(super) fn schedule(weak: Weak<Inner>) {
    let mut recovered = false;
    loop {
        let Some(inner) = weak.upgrade() else { break };
        let mut s = inner.lock();
        // The folder was held by another instance at the start: once it
        // is free, this one takes it, with the index as that instance
        // left it.
        if s.owner.is_none()
            && let Some(lock) = lock_folder(&s.dir)
        {
            log::info!("the download folder is free now: downloads are on");
            s.owner = Some(lock);
            let mut generations = s.generations;
            s.entries = load_entries(&s.dir, &mut generations);
            s.generations = generations;
            inner.bump();
        }
        // Once, on this thread and off the lock: the look at the disk
        // takes its time, and the app opens the engine on its UI thread.
        if s.owner.is_some() && !recovered {
            recovered = true;
            drop(s);
            recover(&inner);
            s = inner.lock();
        }
        while s.owner.is_some() && s.running < s.options.parallel.max(1) {
            let Some(client) = s.client.clone() else { break };
            let server_id = s.server_id.clone();
            let next = s.entries.iter().position(|e| {
                e.state == EntryState::Queued
                    && e.server_id == server_id
                    && !s.tombstoned(&e.server_id, &e.item_id)
            });
            let Some(at) = next else { break };
            let generation = s.next_generation();
            let folder = s.dir.join(&server_id).join(&s.entries[at].item_id);
            let options = s.options.clone();
            let entry = &mut s.entries[at];
            let job = Job {
                server_id,
                item_id: entry.item_id.clone(),
                generation,
                client,
                folder,
                cancel: Arc::new(AtomicBool::new(false)),
                options,
            };
            entry.state = EntryState::Downloading;
            entry.error = None;
            entry.generation = generation;
            entry.cancel = job.cancel.clone();
            entry.admitted = false;
            s.running += 1;
            inner.bump();
            let worker = inner.clone();
            let spawned = thread::Builder::new()
                .name(format!("download-{}", &job.item_id[..job.item_id.len().min(8)]))
                .spawn(move || work(worker, job));
            // No thread: the entry waits again. Left as it was, it counted
            // as a running download for good and held the others back.
            if spawned.is_err() {
                s.running -= 1;
                if let Some(entry) = s.entries.iter_mut().find(|e| e.generation == generation) {
                    entry.state = EntryState::Queued;
                }
                // To the wait below, with the lock given up: the next try
                // comes at the next round, not at once and for ever.
                break;
            }
        }
        let _ = inner
            .wake
            .wait_timeout(s, Duration::from_millis(500))
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}

/// Why a download stopped before its end.
pub(super) enum Stop {
    Cancelled,
    /// The test switch stopped it.
    Limit,
    /// A try failed; the next try may go.
    Failed(String),
    /// The storage refused it: no retry, the word stays on the entry.
    Refused(String),
}

pub(super) fn work(inner: Arc<Inner>, job: Job) {
    #[cfg(test)]
    if let Some(gate) = &job.options.start_gate {
        gate.wait();
    }
    let outcome = download_run(&inner, &job, false);
    let mut s = inner.lock();
    s.running = s.running.saturating_sub(1);
    match s.owned_mut(&job) {
        Some(entry) => {
            entry.speed = 0.;
            entry.admitted = false;
            match outcome {
                Ok(()) => {
                    entry.state = EntryState::Done;
                    entry.error = None;
                    log::info!("download done: {}", entry.title());
                }
                Err(Stop::Cancelled) => entry.state = EntryState::Paused,
                Err(Stop::Limit) => {
                    entry.state = EntryState::Paused;
                    entry.error = Some("Stopped by the byte limit of the test switch".into());
                }
                Err(Stop::Failed(why) | Stop::Refused(why)) => {
                    log::warn!("download failed: {}: {why}", entry.title());
                    entry.state = EntryState::Failed;
                    entry.error = Some(why);
                }
            }
        }
        // Removed while it ran. The worker is out of the folder now: the
        // files go, off the lock, and the tombstone with them. An entry
        // added again meanwhile starts after that, in a clean folder.
        None => {
            if s.tombstones.iter().any(|t| t.generation == job.generation) {
                drop(s);
                delete_folder(&inner, &job.folder, job.generation);
                s = inner.lock();
            }
        }
    }
    inner.commit(s, true);
}

/// Agent for the files: no limit on the time a body may take (a large
/// file over a slow line takes hours, and a line that went dead is the
/// stall limit's to find), a limit on the time without a byte (`stall`),
/// and the name of this app as the user agent. A cancel ends a wait for
/// bytes within a second.
pub(super) fn agent(stall: Duration, cancel: Arc<AtomicBool>) -> ureq::Agent {
    crate::downloads::stall::agent(
        ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(20)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .timeout_recv_body(None)
            .user_agent(format!("{}/{}", crate::brand::FOLDER, crate::config::APP_VERSION))
            .build(),
        stall,
        cancel,
    )
}

/// One run of a download. `again` says the item was read from the server
/// again in this download already (see the size check below).
pub(super) fn download_run(inner: &Arc<Inner>, job: &Job, again: bool) -> Result<(), Stop> {
    let Job { client, folder, cancel, options, server_id, item_id, .. } = job;
    let failed = |err: anyhow::Error| Stop::Failed(format!("{err:#}"));
    // A job whose entry is gone writes nothing, not even its folder.
    job.check(inner)?;
    fs::create_dir_all(folder).map_err(|e| failed(e.into()))?;
    let agent = agent(options.stall, cancel.clone());
    let live = || job.live(inner);

    // The item as the server has it. A folder from a run before keeps it.
    let meta_path = folder.join(META_FILE);
    let mut meta: Value = fs::read(&meta_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null);
    if meta.get("Item").is_none() {
        meta = fetch_meta(client, item_id, server_id).map_err(failed)?;
        job.check(inner)?;
        write_json(&meta_path, &meta).map_err(failed)?;
    }
    let item = meta["Item"].clone();
    let source = item["MediaSources"].get(0).cloned().unwrap_or(Value::Null);
    let total = source["Size"].as_u64().filter(|n| *n > 0);
    let file = meta["File"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| media_file_name(&source));
    check_file_name(&file).map_err(failed)?;
    {
        let mut s = inner.lock();
        let Some(entry) = s.owned_mut(job) else {
            return Err(Stop::Cancelled);
        };
        entry.fill_from(&item);
        entry.total = total;
        entry.file = Some(file.clone());
        // A restart finds the part file by its name in the index.
        inner.save(s);
        inner.bump();
    }
    job.check(inner)?;

    // Artwork and subtitles are small and optional: the item plays
    // without them.
    fetch_images(&agent, client, &item, folder, &live, options.artwork);
    // Once, unless one of them did not come: then the next run of the
    // download asks again (a file that is there is not fetched twice).
    if meta.get("Subtitles").is_none() || meta["SubtitlesMissing"].as_bool() == Some(true) {
        let wanted = source["MediaStreams"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|s| sidecar_format(s).is_some())
            .count();
        let subtitles = fetch_subtitles(&agent, client, item_id, &source, folder, &live);
        meta["SubtitlesMissing"] = Value::Bool(subtitles.len() < wanted);
        meta["Subtitles"] = Value::Array(subtitles);
        if live() {
            let _ = write_json(&meta_path, &meta);
        }
    }
    job.check(inner)?;

    // The media file.
    let final_path = folder.join(&file);
    let part = folder.join(format!("{file}.part"));
    if let (Ok(meta), Some(total)) = (fs::metadata(&final_path), total)
        && meta.len() == total
    {
        set_done(inner, job, total, 0.);
        return Ok(());
    }
    let part_len = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    check_space(inner, job, total, part_len)?;

    let url = client.url(&format!("/Items/{item_id}/Download"), &[]);
    let mut attempt = 0;
    let mut wait = options.retry_wait;
    // The longest the part file has been in this download.
    let mut farthest = part_len;
    // Every failed try, also those that brought new bytes.
    let mut tries = 0;
    loop {
        job.check(inner)?;
        let start = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        set_done(inner, job, start, 0.);
        // The part file is the whole file already (a cancel or an error
        // came between the last byte and the rename): nothing to ask for.
        // A request for the bytes after the end is a 416, which would
        // throw the file away.
        // The length alone does not say it is the file the server has
        // now, so the item is read again first (once in a download).
        if start > 0 && total == Some(start) {
            if !again {
                let fresh = fetch_meta(client, item_id, server_id).map_err(failed)?;
                if size_of(&fresh) != total {
                    job.check(inner)?;
                    start_over(folder, &part, false);
                    write_json(&meta_path, &fresh).map_err(failed)?;
                    return download_run(inner, job, true);
                }
            }
            return finish(inner, job, &part, &final_path, start);
        }
        match fetch_media(&agent, job, &url, &part, start, inner, total) {
            Ok(got) => {
                let want = total.unwrap_or(got);
                // More than the size: `fetch_media` stopped at the size
                // and wrote no further, so the part file is the start of
                // a longer file. It goes: with the length of the size it
                // would pass for the whole file at the next try.
                if got > want {
                    let _ = fs::remove_file(&part);
                    let _ = fs::remove_file(tag_file(&part));
                }
                // Not the size `meta.json` says: the file on the server
                // may be another one by now (a release that replaced the
                // first). The item is read again, once; with the size it
                // has now the download starts over. A part file that came
                // whole in this request is the new file and stays.
                if got != want && !again {
                    let fresh = fetch_meta(client, item_id, server_id).map_err(failed)?;
                    let size = size_of(&fresh);
                    if size != total {
                        job.check(inner)?;
                        log::info!("download of {item_id}: the file on the server changed ({want} to {size:?} bytes)");
                        let whole = start == 0 && size == Some(got) && fresh["File"].as_str() == Some(file.as_str());
                        start_over(folder, &part, whole);
                        write_json(&meta_path, &fresh).map_err(failed)?;
                        return download_run(inner, job, true);
                    }
                }
                if got != want {
                    // The file is not what the server said; a retry gets
                    // the rest, or finds that there is none.
                    attempt += 1;
                    if attempt >= ATTEMPTS {
                        return Err(Stop::Failed(format!(
                            "The file is {} of {} bytes",
                            format_bytes(got),
                            format_bytes(want)
                        )));
                    }
                } else {
                    return finish(inner, job, &part, &final_path, got);
                }
            }
            Err(Stop::Failed(why)) => {
                // A try that brought bytes is no failed try: a large file
                // over a line that cuts now and then still gets there.
                // Farther than any try before, that is: a part file that
                // is thrown away and fetched again to the same place (a
                // server that sends less than it says) does not count.
                // And not for ever: a server that sends a little more at
                // each try and then cuts could hold a download for good.
                tries += 1;
                if tries >= TRIES_AT_MOST {
                    return Err(Stop::Failed(why));
                }
                let reached = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
                if reached > farthest {
                    farthest = reached;
                    attempt = 0;
                    wait = options.retry_wait;
                }
                attempt += 1;
                log::info!("download of {item_id}: try {attempt} failed: {why}");
                if attempt >= ATTEMPTS {
                    return Err(Stop::Failed(why));
                }
            }
            Err(stop) => return Err(stop),
        }
        // Back off, and go at once when the user cancels meanwhile.
        let until = Instant::now() + wait;
        while Instant::now() < until {
            if cancel.load(Ordering::Acquire) {
                return Err(Stop::Cancelled);
            }
            thread::sleep(Duration::from_millis(20));
        }
        wait = (wait * 2).min(Duration::from_secs(30));
    }
}

/// The part file is the whole file: it takes its name, and the entry
/// its size.
pub(super) fn finish(inner: &Arc<Inner>, job: &Job, part: &Path, media: &Path, size: u64) -> Result<(), Stop> {
    job.check(inner)?;
    fs::rename(part, media).map_err(|e| Stop::Failed(e.to_string()))?;
    let _ = fs::remove_file(tag_file(part));
    set_done(inner, job, size, 0.);
    Ok(())
}

/// The size of the media file in a `meta.json`; none when the server did
/// not say.
pub(super) fn size_of(meta: &Value) -> Option<u64> {
    meta["Item"]["MediaSources"][0]["Size"].as_u64().filter(|n| *n > 0)
}

/// Where the validator of a part file is kept (see `fetch_media`).
pub(super) fn tag_file(part: &Path) -> PathBuf {
    let mut name = part.as_os_str().to_os_string();
    name.push(".tag");
    PathBuf::from(name)
}

/// The file on the server is another one than the folder was made for:
/// what was fetched for the old one goes. The part file stays when it is
/// the new file, whole (`keep_part`). The subtitle files go in every case:
/// they are fetched by their place in the item, and a file of the old
/// release would be kept under the name of the new one.
pub(super) fn start_over(folder: &Path, part: &Path, keep_part: bool) {
    if !keep_part {
        let _ = fs::remove_file(part);
        let _ = fs::remove_file(tag_file(part));
    }
    for entry in fs::read_dir(folder).into_iter().flatten().flatten() {
        if entry.file_name().to_string_lossy().starts_with("sub-") {
            let _ = fs::remove_file(entry.path());
        }
    }
}

pub(super) fn set_done(inner: &Arc<Inner>, job: &Job, done: u64, speed: f64) {
    let mut s = inner.lock();
    if let Some(entry) = s.owned_mut(job) {
        entry.done = done;
        entry.speed = speed;
    }
    inner.bump();
}

/// Gets the media file from `start` on, into the part file. Returns the
/// size of the part file at the end of the body. `known_size` says the
/// download was admitted on its size; without one, the budget is checked
/// as the bytes come.
pub(super) fn fetch_media(
    agent: &ureq::Agent,
    job: &Job,
    url: &str,
    part: &Path,
    start: u64,
    inner: &Arc<Inner>,
    total: Option<u64>,
) -> Result<u64, Stop> {
    let known_size = total.is_some();
    let Job { client, cancel, options, item_id, .. } = job;
    let failed = |err: String| Stop::Failed(err);
    let mut request = agent.get(url).header("Authorization", client.auth_header());
    // A part file goes on only as the file it was started from: the
    // validator of that answer (its `ETag`, or else its date) goes with
    // the range, and a server that has another file by now sends the whole
    // file (a 200), which starts the part file over. Without it, the bytes
    // of two files would end up in one. A part file with no validator
    // (the server gave none) cannot be tied to a file, so it does not go
    // on: the download starts over.
    let tag_path = tag_file(part);
    let mut start = start;
    if start > 0 {
        match fs::read_to_string(&tag_path) {
            Ok(tag) => {
                request = request
                    .header("Range", format!("bytes={start}-"))
                    .header("If-Range", tag.trim());
            }
            Err(_) => {
                log::info!("download of {item_id}: the part file has no validator; starting over");
                let _ = fs::remove_file(part);
                start = 0;
            }
        }
    }
    let mut response = match request.call() {
        Ok(response) => response,
        // The part is longer than the file: it is not the file any more.
        Err(ureq::Error::StatusCode(416)) => {
            let _ = fs::remove_file(part);
            let _ = fs::remove_file(&tag_path);
            return Err(failed("The server has less than the part file (416)".into()));
        }
        Err(ureq::Error::StatusCode(code)) => return Err(failed(format!("HTTP {code}"))),
        Err(err) => return Err(failed(err.to_string())),
    };
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    log::info!(
        "download of {item_id}: from byte {start}: HTTP {}{}",
        response.status().as_u16(),
        header("content-range").map(|r| format!(" ({r})")).unwrap_or_default()
    );
    let mut file = match response.status().as_u16() {
        206 => {
            // "bytes <start>-<end>/<total>"
            let range = header("content-range").unwrap_or_default();
            let from: Option<u64> = range
                .trim_start_matches("bytes ")
                .split('-')
                .next()
                .and_then(|s| s.trim().parse().ok());
            if from != Some(start) {
                return Err(failed(format!("The server sent a wrong range: {range:?}")));
            }
            fs::File::options().append(true).create(true).open(part)
        }
        200 => {
            // The server ignored the range: the whole file comes again.
            if start > 0 {
                log::info!("download of {item_id}: the server ignored the range; starting over");
            }
            start = 0;
            // The validator of the file this part file is now of. A weak
            // tag cannot go with a range.
            let tag = header("etag")
                .filter(|tag| !tag.starts_with("W/"))
                .or_else(|| header("last-modified"));
            match tag {
                // A tag that cannot be kept must not leave the one of
                // the file before in its place.
                Some(tag) => {
                    if fs::write(&tag_path, tag).is_err() {
                        let _ = fs::remove_file(&tag_path);
                    }
                }
                None => {
                    let _ = fs::remove_file(&tag_path);
                }
            }
            fs::File::create(part)
        }
        code => return Err(failed(format!("HTTP {code}"))),
    }
    .map_err(|e| failed(e.to_string()))?;

    let mut reader = response.body_mut().as_reader();
    let mut buffer = vec![0u8; CHUNK];
    let mut done = start;
    let mut this_run = 0u64;
    let (mut mark_at, mut mark_done) = (Instant::now(), done);
    let mut saved_at = Instant::now();
    let mut speed = 0.;
    loop {
        // With the test switch on, one read never goes past its limit.
        let room = options
            .byte_limit
            .map_or(buffer.len(), |limit| (limit.saturating_sub(this_run) as usize).clamp(1, buffer.len()));
        let n = match reader.read(&mut buffer[..room]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) => {
                let _ = file.flush();
                // A cancel ends the wait with an error as well.
                if cancel.load(Ordering::Acquire) {
                    return Err(Stop::Cancelled);
                }
                return Err(failed(format!("Read failed: {err}")));
            }
        };
        // A download of unknown size was not admitted on its size: each
        // read is checked before it is written.
        if !known_size
            && let Err(stop) = check_budget(inner, job, done, n as u64)
        {
            let _ = file.flush();
            return Err(stop);
        }
        // Not past the size the download was admitted on: the space was
        // checked for that size, and a server that sends more is sending
        // another file.
        // The length that comes back says so (it is over the size), and
        // the caller reads the item again: the file may have been
        // replaced by a larger one.
        if total.is_some_and(|total| done + n as u64 > total) {
            let _ = file.flush();
            return Ok(done + n as u64);
        }
        file.write_all(&buffer[..n]).map_err(|e| write_failed(&e))?;
        done += n as u64;
        this_run += n as u64;
        // The speed over the last half second.
        if mark_at.elapsed() >= Duration::from_millis(500) {
            speed = (done - mark_done) as f64 / mark_at.elapsed().as_secs_f64();
            mark_at = Instant::now();
            mark_done = done;
        }
        set_done(inner, job, done, speed);
        if saved_at.elapsed() >= Duration::from_secs(2) {
            saved_at = Instant::now();
            inner.save(inner.lock());
        }
        if cancel.load(Ordering::Acquire) {
            let _ = file.flush();
            return Err(Stop::Cancelled);
        }
        if options.byte_limit.is_some_and(|limit| this_run >= limit) {
            let _ = file.flush();
            return Err(Stop::Limit);
        }
        // A test switch: slow the download to this many bytes a second, so
        // the progress can be watched on a fast link.
        if let Some(bps) = options.throttle_bps.filter(|b| *b > 0) {
            thread::sleep(Duration::from_secs_f64(n as f64 / bps as f64));
        }
    }
    file.flush().map_err(|e| failed(e.to_string()))?;
    Ok(done)
}

/// A write to the part file failed. A full disk is no failed try: the
/// next try would ask the server for the same bytes and stop at the same
/// place, so the download ends with the word for it.
pub(super) fn write_failed(err: &std::io::Error) -> Stop {
    use std::io::ErrorKind;
    match err.kind() {
        ErrorKind::StorageFull | ErrorKind::QuotaExceeded => Stop::Refused("The disk is full".into()),
        _ => Stop::Failed(format!("Write failed: {err}")),
    }
}
