// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a download asks of the server beside the media file: the item
//! for `meta.json`, the artwork and the external subtitles.

use super::*;

/// The item, its segments and the names of its files, for `meta.json`.
pub(super) fn fetch_meta(client: &Client, item_id: &str, server_id: &str) -> Result<Value> {
    let user = client.user()?.to_string();
    let item: Value = client.get(&format!("/Items/{item_id}"), &[("userId", user)])?;
    // Intro and credits ranges; optional.
    let segments = client
        .get::<Value>(&format!("/MediaSegments/{item_id}"), &[])
        .ok()
        .and_then(|page| page.get("Items").cloned())
        .unwrap_or(Value::Array(Vec::new()));
    let source = item["MediaSources"].get(0).cloned().unwrap_or(Value::Null);
    Ok(serde_json::json!({
        "ServerId": server_id,
        "ItemId": item_id,
        "File": media_file_name(&source),
        "Item": item,
        "Segments": segments,
    }))
}

/// "video.mkv": the container of the file, else the extension of its path.
pub(super) fn media_file_name(source: &Value) -> String {
    let container = source["Container"]
        .as_str()
        .and_then(|c| c.split(',').next())
        .filter(|c| !c.is_empty() && c.chars().all(|ch| ch.is_ascii_alphanumeric()))
        .map(str::to_lowercase);
    let extension = container.or_else(|| {
        source["Path"]
            .as_str()
            .and_then(|p| p.rsplit_once('.'))
            .map(|(_, ext)| ext.to_lowercase())
            .filter(|ext| !ext.is_empty() && ext.len() <= 5 && ext.chars().all(|c| c.is_ascii_alphanumeric()))
    });
    format!("video.{}", extension.unwrap_or_else(|| "mkv".to_string()))
}

/// Poster and backdrop of the item, to its folder and to the image cache of
/// the app, so the pages show them without the server. `live` is asked
/// before each write: a job whose entry is gone writes nothing.
pub(super) fn fetch_images(
    agent: &ureq::Agent,
    client: &Client,
    item: &Value,
    folder: &Path,
    live: &dyn Fn() -> bool,
    artwork: Artwork,
) {
    let id = item["Id"].as_str().unwrap_or_default();
    let text = |value: &Value| value.as_str().map(str::to_string);
    let poster = text(&item["ImageTags"]["Primary"])
        .map(|tag| (id.to_string(), "Primary", tag))
        .or_else(|| {
            Some((
                text(&item["SeriesId"])?,
                "Primary",
                text(&item["SeriesPrimaryImageTag"])?,
            ))
        });
    let backdrop = item["BackdropImageTags"]
        .get(0)
        .and_then(text)
        .map(|tag| (id.to_string(), "Backdrop", tag))
        .or_else(|| {
            Some((
                text(&item["ParentBackdropItemId"])?,
                "Backdrop",
                item["ParentBackdropImageTags"].get(0).and_then(text)?,
            ))
        });
    let series = text(&item["SeriesId"])
        .zip(text(&item["SeriesPrimaryImageTag"]))
        .map(|(series, tag)| (series, "Primary", tag));
    let wanted = [
        (poster, POSTER_FILE, 640, &[240, 320, 400, 480, 640, 800][..]),
        (backdrop, BACKDROP_FILE, 1280, &[1000, 1280, 1600, 1920][..]),
        (series, SERIES_FILE, 640, &[240, 320, 400, 480, 640, 800][..]),
    ];
    for (image, name, width, seed_widths) in wanted {
        let Some((owner, kind, tag)) = image else { continue };
        let path = folder.join(name);
        if !path.is_file() {
            let url = client.image_url(&owner, kind, Some(&tag), width);
            // An answer that is no image (the page of a proxy) is not kept.
            let Ok(bytes) = get_bytes(agent, &url, None) else { continue };
            if !(artwork.is_image)(&bytes) || !live() {
                continue;
            }
            if fsutil::write_atomic(&path, &bytes).is_err() {
                continue;
            }
        }
        // The cache of the app gets the same image under each width the
        // pages ask for: one file on the disk, cloned (APFS), not copied.
        for seed_width in seed_widths {
            let url = client.image_url(&owner, kind, Some(&tag), *seed_width);
            (artwork.seed)(&url, &path);
        }
    }
}

/// The format asked of the server and the extension of the file kept
/// for an external subtitle stream; none for a stream the engine leaves
/// out. A PGS sidecar comes as it is under the server's name for the
/// format (a server before 12.2 refuses "sup"), and mpv takes the `.sup`
/// on the side. The server gives no file for the other image sidecars: a
/// VobSub pair (`.idx/.sub`) goes through its text converter and fails.
pub(super) fn sidecar_format(stream: &Value) -> Option<(&'static str, &'static str)> {
    if stream["Type"].as_str() != Some("Subtitle") || stream["IsExternal"].as_bool() != Some(true) {
        return None;
    }
    match stream["Codec"].as_str().unwrap_or_default().to_lowercase().as_str() {
        "subrip" | "srt" => Some(("srt", "srt")),
        "ass" => Some(("ass", "ass")),
        "ssa" => Some(("ssa", "ssa")),
        "webvtt" | "vtt" => Some(("vtt", "vtt")),
        "pgssub" | "pgs" | "hdmv_pgs_subtitle" => Some(("pgssub", "sup")),
        _ => None,
    }
}

/// External subtitle files of the item. Returns what `meta.json` keeps of
/// them: the stream index, language, title and file name. `live` is asked
/// before each write.
pub(super) fn fetch_subtitles(
    agent: &ureq::Agent,
    client: &Client,
    item_id: &str,
    source: &Value,
    folder: &Path,
    live: &dyn Fn() -> bool,
) -> Vec<Value> {
    let source_id = source["Id"].as_str().unwrap_or(item_id);
    let mut out = Vec::new();
    for stream in source["MediaStreams"].as_array().into_iter().flatten() {
        let Some((format, extension)) = sidecar_format(stream) else { continue };
        let Some(index) = stream["Index"].as_i64() else { continue };
        let path = stream["DeliveryUrl"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| format!("/Videos/{item_id}/{source_id}/Subtitles/{index}/0/Stream.{format}"));
        let Some(url) = resolve_url(&client.base, &path) else {
            log::info!("subtitle {index} of {item_id}: the address {path:?} is not one this app fetches");
            continue;
        };
        // The session goes to the own server only, whatever address the
        // server gave for the file.
        let auth = crate::jellyfin::same_origin(&url, &client.base).then_some(client);
        let name = format!("sub-{index}.{extension}");
        let file = folder.join(&name);
        if !file.exists() {
            if !live() {
                continue;
            }
            if let Err(err) = get_file(agent, &url, auth, &file) {
                log::info!("subtitle {index} of {item_id} not fetched: {err:#}");
                continue;
            }
        }
        out.push(serde_json::json!({
            "Index": index,
            "Language": stream["Language"],
            "Title": stream["DisplayTitle"],
            "File": name,
        }));
    }
    out
}

/// The address of a file the server named, as a URL: a path is on the
/// server, `//host/path` takes the server's scheme, an `http(s)` URL stands
/// as it is (also on another host, or plain `http` from an `https` server:
/// the session header does not go there, see `same_origin`). `None` for a
/// scheme this app does not fetch (`file:`, `ftp:`, `javascript:`).
pub(super) fn resolve_url(base: &str, given: &str) -> Option<String> {
    let base = base.trim_end_matches('/');
    if let Some((scheme, _)) = given.split_once("://") {
        return match scheme.to_ascii_lowercase().as_str() {
            "http" | "https" => Some(given.to_string()),
            _ => None,
        };
    }
    if let Some(rest) = given.strip_prefix("//") {
        let scheme = base.split_once("://")?.0;
        return Some(format!("{scheme}://{rest}"));
    }
    if given.starts_with('/') {
        return Some(format!("{base}{given}"));
    }
    // "mailto:x", "javascript:..." and the like: a scheme, not a path.
    let head = given.split(['/', '?', '#']).next().unwrap_or(given);
    if head.contains(':') {
        return None;
    }
    Some(format!("{base}/{given}"))
}

/// A subtitle file larger than this is not one (a PGS track of a 4K film
/// is about 100 MB).
pub(super) const MOST_SUBTITLE: u64 = 256 * 1024 * 1024;

/// GET of a subtitle file straight to `path` (whole or not at all, see
/// `fsutil`), so the memory of the app need not hold a PGS track. `auth`
/// sends the session header with it. A route that answers with the web
/// page gave no subtitle.
pub(super) fn get_file(agent: &ureq::Agent, url: &str, auth: Option<&Client>, path: &Path) -> Result<()> {
    let mut request = agent.get(url);
    if let Some(client) = auth {
        request = request.header("Authorization", client.auth_header());
    }
    let mut response = request.call().context("get")?;
    let mut reader = response.body_mut().as_reader().take(MOST_SUBTITLE + 1);
    let mut head = Vec::with_capacity(512);
    (&mut reader).take(512).read_to_end(&mut head).context("read")?;
    if head.starts_with(b"<!DOCTYPE") || head.starts_with(b"<html") {
        return Err(anyhow!("not a subtitle file"));
    }
    fsutil::write_atomic_with(path, None, |file| {
        file.write_all(&head)?;
        let size = head.len() as u64 + std::io::copy(&mut reader, file)?;
        match size > MOST_SUBTITLE {
            true => Err(std::io::Error::other(format!("the answer is over {} MB", MOST_SUBTITLE / (1024 * 1024)))),
            false => Ok(()),
        }
    })
    .context("write")
}

/// GET of a small file. `auth` sends the session header with it.
pub(super) fn get_bytes(agent: &ureq::Agent, url: &str, auth: Option<&Client>) -> Result<Vec<u8>> {
    let mut request = agent.get(url);
    if let Some(client) = auth {
        request = request.header("Authorization", client.auth_header());
    }
    let mut response = request.call().context("get")?;
    // An image or a subtitle: small. The address of a subtitle can name
    // any host, and its answer must not fill the memory.
    const MOST: u64 = 64 * 1024 * 1024;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MOST + 1)
        .read_to_end(&mut bytes)
        .context("read")?;
    if bytes.len() as u64 > MOST {
        return Err(anyhow!("the answer is over {} MB", MOST / (1024 * 1024)));
    }
    Ok(bytes)
}
