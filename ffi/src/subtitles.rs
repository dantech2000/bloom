//! "Search for subtitles…" (the Mac's src/subtitles.rs, its requests): the
//! server's subtitle providers, and a download into the library. The paths
//! and the reading of the answers are core's (`bloom_core::subtitles`).

use std::time::Duration;

use bloom_core::{
    jellyfin::{Client, Method, MediaStream, StreamKind},
    subtitles::{language_choices, language_name, new_stream, parse_candidates, remote_search_path},
};
use serde_json::Value;

use crate::{BloomError, Server, run, run_apart};

#[derive(uniffi::Record)]
pub struct SubtitleCandidate {
    pub id: String,
    pub name: String,
    /// Provider, format, downloads, rating, author.
    pub detail: String,
    /// "Hash match", "Forced", "Hearing impaired"...
    pub badges: Vec<String>,
}

#[derive(uniffi::Record)]
pub struct SubtitleLanguage {
    pub code: String,
    pub name: String,
}

fn subtitle_streams(client: &Client, item_id: &str) -> anyhow::Result<Vec<MediaStream>> {
    let item = client.item(item_id)?;
    Ok(item
        .media_sources
        .first()
        .map(|s| s.media_streams.iter().filter(|m| m.kind == StreamKind::Subtitle).cloned().collect())
        .unwrap_or_default())
}

#[uniffi::export]
impl Server {
    /// The languages to search in, the user's preference first.
    pub fn subtitle_languages(&self, preference: Option<String>) -> Vec<SubtitleLanguage> {
        language_choices(preference.as_deref())
            .into_iter()
            .map(|code| SubtitleLanguage { name: language_name(&code), code })
            .collect()
    }

    pub async fn search_subtitles(&self, item_id: String, language: String) -> Result<Vec<SubtitleCandidate>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let body: Value = client.get(&remote_search_path(&item_id, &language), &[("isPerfectMatch", "false".to_string())])?;
            Ok(parse_candidates(&body)
                .iter()
                .map(|c| SubtitleCandidate {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    detail: c.detail(),
                    badges: c.badges().into_iter().map(str::to_string).collect(),
                })
                .collect())
        })
        .await
    }

    /// Downloads a candidate into the library and waits for its stream
    /// (the server lists it after it reads the item again): its index, or
    /// none when it did not show in time. The wait (up to 24 s) runs on a
    /// thread of its own, not on the pool.
    pub async fn download_subtitle(&self, item_id: String, subtitle_id: String) -> Result<Option<i64>, BloomError> {
        let client = self.signed_in()?;
        run_apart("subtitle-download", move || {
            let before = subtitle_streams(&client, &item_id)?;
            client.call(Method::Post, &remote_search_path(&item_id, &subtitle_id), &[])?;
            for _ in 0..16 {
                std::thread::sleep(Duration::from_millis(1500));
                let after = subtitle_streams(&client, &item_id)?;
                if let Some(found) = new_stream(&before, &after) {
                    return Ok(Some(found.index));
                }
            }
            Ok(None)
        })
        .await
    }
}
