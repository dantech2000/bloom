//! Playlists and collections (the Mac's src/lists.rs, its requests): the
//! lists of the user, a playlist's entries, and the edits. A delete goes
//! only to a playlist or a collection, as the server names the item: a
//! `DELETE /Items/{id}` also deletes media.

use bloom_core::jellyfin::{Client, ItemKind, ItemsResult, Method};
use serde::Deserialize;
use serde_json::json;

use crate::{BloomError, ListKind, MediaItem, Server, media, medias, run};

/// An entry of a playlist: the item, and the id of its place (an item can
/// be in a playlist twice).
#[derive(uniffi::Record)]
pub struct ListEntry {
    pub entry_id: String,
    pub item: MediaItem,
}

#[derive(Deserialize)]
struct Created {
    #[serde(rename = "Id", default)]
    id: String,
}

fn list_query(client: &Client, kind: ListKind) -> anyhow::Result<Vec<bloom_core::jellyfin::Item>> {
    let user = client.user()?.to_string();
    let result: ItemsResult = client.get(
        "/Items",
        &[
            ("userId", user),
            ("includeItemTypes", kind.item_type().to_string()),
            ("recursive", "true".to_string()),
            ("sortBy", "SortName".to_string()),
            ("fields", "ChildCount,PrimaryImageAspectRatio".to_string()),
            ("enableImageTypes", "Primary".to_string()),
        ],
    )?;
    Ok(result.items)
}

#[uniffi::export]
impl Server {
    /// The user's playlists or the server's collections.
    pub async fn lists(&self, kind: ListKind) -> Result<Vec<MediaItem>, BloomError> {
        let client = self.signed_in()?;
        run(move || Ok(medias(&client, &list_query(&client, kind)?))).await
    }

    pub async fn playlist_entries(&self, list_id: String) -> Result<Vec<ListEntry>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            #[derive(Deserialize)]
            struct Entry {
                #[serde(rename = "PlaylistItemId", default)]
                entry_id: String,
                #[serde(flatten)]
                item: bloom_core::jellyfin::Item,
            }
            #[derive(Deserialize)]
            struct Entries {
                #[serde(rename = "Items", default)]
                items: Vec<Entry>,
            }
            let user = client.user()?.to_string();
            let entries: Entries = client.get(
                &format!("/Playlists/{list_id}/Items"),
                &[
                    ("userId", user),
                    ("fields", "PrimaryImageAspectRatio,ChildCount,Overview".to_string()),
                    ("enableImageTypes", "Primary,Backdrop,Thumb".to_string()),
                ],
            )?;
            Ok(entries.items.iter().map(|e| ListEntry { entry_id: e.entry_id.clone(), item: media(&client, &e.item) }).collect())
        })
        .await
    }

    /// A new playlist or collection with these items; its id.
    pub async fn create_list(&self, kind: ListKind, name: String, item_ids: Vec<String>) -> Result<String, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let created: Created = if kind == ListKind::Collection {
                client.send(Method::Post, "/Collections", &[("name", name), ("ids", item_ids.join(","))])?
            } else {
                let user = client.user()?.to_string();
                let body = json!({ "Name": name, "Ids": item_ids, "UserId": user, "MediaType": "Video" });
                client.post("/Playlists", &body)?.read_json().map_err(anyhow::Error::from)?
            };
            if created.id.is_empty() {
                return Err(BloomError::Answered { detail: "the server sent no id for the new list".into() });
            }
            Ok(created.id)
        })
        .await
    }

    pub async fn add_to_list(&self, kind: ListKind, list_id: String, item_ids: Vec<String>) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let ids = item_ids.join(",");
            if kind == ListKind::Collection {
                client.call(Method::Post, &format!("/Collections/{list_id}/Items"), &[("ids", ids)])?;
            } else {
                let user = client.user()?.to_string();
                client.call(Method::Post, &format!("/Playlists/{list_id}/Items"), &[("ids", ids), ("userId", user)])?;
            }
            Ok(())
        })
        .await
    }

    /// Takes items out: a playlist by its entry ids, a collection by item ids.
    pub async fn remove_from_list(&self, kind: ListKind, list_id: String, ids: Vec<String>) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let ids = ids.join(",");
            if kind == ListKind::Collection {
                client.call(Method::Delete, &format!("/Collections/{list_id}/Items"), &[("ids", ids)])?;
            } else {
                client.call(Method::Delete, &format!("/Playlists/{list_id}/Items"), &[("entryIds", ids)])?;
            }
            Ok(())
        })
        .await
    }

    pub async fn move_in_playlist(&self, list_id: String, entry_id: String, new_index: u32) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || Ok(client.call(Method::Post, &format!("/Playlists/{list_id}/Items/{entry_id}/Move/{new_index}"), &[])?))
            .await
    }

    pub async fn rename_playlist(&self, list_id: String, name: String) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || {
            client.post(&format!("/Playlists/{list_id}"), &json!({ "Name": name }))?;
            Ok(())
        })
        .await
    }

    /// Deletes a playlist or a collection, and nothing else: the server
    /// says what the item is; the caller's word for it is not trusted.
    pub async fn delete_list(&self, list_id: String) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let item = client.item(&list_id)?;
            if !matches!(item.kind, ItemKind::Playlist | ItemKind::BoxSet) {
                return Err(BloomError::Internal {
                    message: format!("refused to delete {:?}: it is a {}, not a playlist or a collection", item.name, item.kind),
                });
            }
            Ok(client.call(Method::Delete, &format!("/Items/{list_id}"), &[])?)
        })
        .await
    }
}
