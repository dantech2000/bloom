//! Browsing beyond the plain pages: a library's grid with genre, decade
//! and rating filters, the filter choices of a library, people in the
//! search, a person's page with the role per title, the whole of a home
//! row ("See all"), and the picks of a new user's Home.

use bloom_core::jellyfin::{Client, Item, ItemsResult};
use bloom_core::pages::{LibraryData, LibraryShow};
use serde_json::Value;

use crate::{BloomError, CollectionType, LibraryFilter, LibrarySort, MediaItem, Page, Server, media, medias, prefs::Language, run};

/// The fields of a card, as core asks for them (`item_fields`), with the
/// streams while the quality tags are on.
fn card_fields() -> String {
    let fields = "Overview,PrimaryImageAspectRatio,ProductionYear,Genres,ParentId,DateCreated,Status,EndDate";
    if bloom_core::jellyfin::quality_tags() { format!("{fields},MediaStreams") } else { fields.to_string() }
}

/// A person as a search result shows it: a round photo and the name.
#[derive(uniffi::Record)]
pub struct PersonCard {
    pub id: String,
    pub name: String,
    pub image: Option<String>,
}

/// A title of a person, with what the person did on it ("as Jo March",
/// "Director"); none when the server does not say.
#[derive(uniffi::Record)]
pub struct Credit {
    pub item: MediaItem,
    pub role: Option<String>,
}

/// A person's page: the photo, the biography, the dates and place of
/// birth, and the titles of the libraries the person is in.
#[derive(uniffi::Record)]
pub struct PersonPage {
    pub id: String,
    pub name: String,
    pub image: Option<String>,
    pub overview: Option<String>,
    /// "1975-03-14", as the server keeps it (the date part alone).
    pub born: Option<String>,
    pub died: Option<String>,
    pub birthplace: Option<String>,
    pub credits: Vec<Credit>,
}

/// What a library can be narrowed to: its genres and years.
#[derive(uniffi::Record)]
pub struct FilterChoices {
    pub genres: Vec<String>,
    pub years: Vec<i32>,
}

/// A view of a film or show library besides its titles: the lists that
/// hold its titles and its studios (Jellyfin 12.0 filters collections and
/// playlists by library).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibraryView {
    Collections,
    Playlists,
    Studios,
}

impl LibraryView {
    fn show(self) -> LibraryShow {
        match self {
            Self::Collections => LibraryShow::Collections,
            Self::Playlists => LibraryShow::Playlists,
            Self::Studios => LibraryShow::Studios,
        }
    }
}

/// The languages of a library's audio and subtitle tracks, to narrow its
/// grid by (Jellyfin 12.0, `/Items/Filters2`); empty from an older server.
#[derive(uniffi::Record)]
pub struct LanguageChoices {
    pub audio: Vec<Language>,
    pub subtitles: Vec<Language>,
}

/// A home row a "See all" opens in full.
#[derive(uniffi::Enum, Clone, Copy)]
pub enum HomeRow {
    ContinueWatching,
    NextUp,
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty()).map(str::to_string)
}

/// The date part of a server date ("1975-03-14T00:00:00.0000000Z").
fn day(value: &Value, key: &str) -> Option<String> {
    text(value, key).map(|date| date.chars().take(10).collect())
}

fn photo(client: &Client, id: &str, tag: Option<&str>) -> Option<String> {
    tag.map(|tag| client.image_url(id, "Primary", Some(tag), 360))
}

/// What the person did on an item, from its people: an actor's character,
/// else the kind of credit; several credits of one person joined.
fn role_of(item: &Item, person_id: &str) -> Option<String> {
    let roles: Vec<String> = item
        .people
        .iter()
        .filter(|person| person.id == person_id)
        .filter_map(|person| {
            let role = person.role.as_deref().map(str::trim).filter(|role| !role.is_empty());
            match (person.kind.name(), role) {
                ("Actor" | "GuestStar", Some(role)) => Some(format!("as {role}")),
                ("Actor" | "GuestStar", None) => None,
                ("", role) => role.map(str::to_string),
                (kind, _) => Some(spaced(kind)),
            }
        })
        .collect();
    let mut unique: Vec<String> = Vec::new();
    for role in roles {
        if !unique.contains(&role) {
            unique.push(role);
        }
    }
    (!unique.is_empty()).then(|| unique.join(", "))
}

/// "GuestStar" reads "Guest star".
fn spaced(kind: &str) -> String {
    let mut out = String::new();
    for (index, c) in kind.chars().enumerate() {
        if index > 0 && c.is_uppercase() {
            out.push(' ');
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

#[uniffi::export]
impl Server {
    /// One page of a grid, as `library_page`, narrowed by genres, years,
    /// a lowest community rating and the languages of the audio and
    /// subtitle tracks (codes of [`Self::language_choices`], any of them
    /// keeps a title; Jellyfin 12.0). Besides the ids `library_page`
    /// takes, "genre:<name>" lists the films and shows of a genre in every
    /// library.
    #[allow(clippy::too_many_arguments, reason = "an export: Swift calls it with labels, a struct adds a type for one call")]
    pub async fn browse_page(
        &self,
        library_id: String,
        collection_type: Option<CollectionType>,
        start: u32,
        limit: u32,
        sort_by: Option<LibrarySort>,
        descending: bool,
        filter: Option<LibraryFilter>,
        genres: Vec<String>,
        years: Vec<i32>,
        min_rating: Option<f32>,
        audio_languages: Vec<String>,
        subtitle_languages: Vec<String>,
    ) -> Result<Page, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let person = library_id.strip_prefix("person:").map(str::to_string);
            let genre = library_id.strip_prefix("genre:").map(str::to_string);
            let studio = library_id.strip_prefix("studio:").map(str::to_string);
            let favorites = library_id == "favorites";
            let across = favorites || person.is_some() || genre.is_some() || studio.is_some();
            let types = match collection_type {
                Some(CollectionType::Movies) => "Movie",
                Some(CollectionType::TvShows) => "Series",
                _ if across => "Movie,Series",
                _ if library_id == "playlists" => "Playlist",
                _ => "",
            };
            let no_parent = across || library_id == "playlists";
            let mut params: Vec<(&str, String)> = vec![
                ("userId", client.user()?.to_string()),
                ("recursive", "true".into()),
                ("sortBy", format!("{},SortName", sort_by.unwrap_or(LibrarySort::Name).field())),
                ("sortOrder", if descending { "Descending" } else { "Ascending" }.into()),
                ("startIndex", start.to_string()),
                ("limit", limit.to_string()),
                ("fields", card_fields()),
                ("enableImageTypes", "Primary,Backdrop,Thumb".into()),
                ("imageTypeLimit", "1".into()),
            ];
            if !no_parent {
                params.push(("parentId", library_id.clone()));
            }
            if !types.is_empty() {
                params.push(("includeItemTypes", types.into()));
            }
            if let Some(person) = person {
                params.push(("personIds", person));
            }
            if let Some(studio) = studio {
                params.push(("studioIds", studio));
            }
            let mut wanted = genres;
            if let Some(genre) = genre {
                wanted.push(genre);
            }
            if !wanted.is_empty() {
                params.push(("genres", wanted.join("|")));
            }
            if !years.is_empty() {
                params.push(("years", years.iter().map(i32::to_string).collect::<Vec<_>>().join(",")));
            }
            if let Some(rating) = min_rating {
                params.push(("minCommunityRating", rating.to_string()));
            }
            params.extend(bloom_core::jellyfin::language_params(&audio_languages, &subtitle_languages));
            let mut filters: Vec<&str> = Vec::new();
            if favorites {
                filters.push("IsFavorite");
            }
            if let Some(filter) = filter {
                filters.push(filter.field());
            }
            if !filters.is_empty() {
                params.push(("filters", filters.join(",")));
            }
            let page: ItemsResult = client.get("/Items", &params)?;
            Ok(Page { items: medias(&client, &page.items), total: page.total_record_count as u32 })
        })
        .await
    }

    /// One page of a library's collections, playlists or studios, by name,
    /// as the Mac's library asks for them (core's `library_query`). A
    /// studio's card opens "studio:<id>" with `browse_page`.
    pub async fn library_view_page(&self, library_id: String, collection_type: Option<CollectionType>, view: LibraryView, start: u32, limit: u32) -> Result<Page, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let mut data = LibraryData::new(Item {
                id: library_id,
                collection_type: collection_type.map(|kind| kind.text().to_string()),
                ..Default::default()
            });
            data.show = view.show();
            let mut query = bloom_core::pages::library_query(&data);
            (query.start, query.limit) = (Some(start as usize), Some(limit as usize));
            query.fields = Some(card_fields());
            let page = bloom_core::pages::library_items(&client, &query, data.show)?;
            Ok(Page { items: medias(&client, &page.items), total: page.total_record_count as u32 })
        })
        .await
    }

    /// The genres and years of a library's films and shows; no library
    /// for every library of the user.
    pub async fn filter_choices(&self, library_id: Option<String>, collection_type: Option<CollectionType>) -> Result<FilterChoices, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let types = match collection_type {
                Some(CollectionType::Movies) => "Movie",
                Some(CollectionType::TvShows) => "Series",
                _ => "Movie,Series",
            };
            let mut params: Vec<(&str, String)> = vec![
                ("userId", client.user()?.to_string()),
                ("includeItemTypes", types.into()),
                ("recursive", "true".into()),
                ("sortBy", "SortName".into()),
                ("enableImages", "false".into()),
            ];
            if let Some(library) = library_id.filter(|id| !id.contains(':') && id != "favorites" && id != "playlists") {
                params.push(("parentId", library));
            }
            // The names of a list of genres or years (`/Items/Filters`
            // answers empty without a library on 12.2).
            let names = |path: &str| -> Result<Vec<String>, BloomError> {
                let answer: Value = client.get(path, &params)?;
                Ok(answer
                    .get("Items")
                    .and_then(Value::as_array)
                    .map(|list| list.iter().filter_map(|item| text(item, "Name")).collect())
                    .unwrap_or_default())
            };
            let mut genres = names("/Genres")?;
            genres.sort_by_key(|name| name.to_lowercase());
            genres.dedup();
            // The years are optional: the genres still help without them.
            let mut years: Vec<i32> = names("/Years").unwrap_or_default().iter().filter_map(|year| year.parse().ok()).collect();
            years.sort_unstable();
            years.dedup();
            Ok(FilterChoices { genres, years })
        })
        .await
    }

    /// The languages of the audio and subtitle tracks of a library's films
    /// and shows (of their episodes for shows), as `filter_choices` takes
    /// its library; no library for every library of the user.
    pub async fn language_choices(&self, library_id: Option<String>, collection_type: Option<CollectionType>) -> Result<LanguageChoices, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let types: &[&str] = match collection_type {
                Some(CollectionType::Movies) => &["Movie"],
                Some(CollectionType::TvShows) => &["Series"],
                _ => &["Movie", "Series"],
            };
            let library = library_id.filter(|id| !id.contains(':') && id != "favorites" && id != "playlists");
            let choices = client.language_filters(library.as_deref(), types)?;
            let languages = |list: Vec<bloom_core::jellyfin::LanguageChoice>| {
                list.into_iter().map(|choice| Language { name: choice.name, code: choice.code }).collect()
            };
            Ok(LanguageChoices { audio: languages(choices.audio), subtitles: languages(choices.subtitles) })
        })
        .await
    }

    /// The people whose name holds the text (cast and crew of the
    /// libraries), the best known first as the server ranks them.
    pub async fn search_people(&self, query: String) -> Result<Vec<PersonCard>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let page: Value = client.get(
                "/Persons",
                &[
                    ("userId", client.user()?.to_string()),
                    ("searchTerm", query),
                    ("limit", "20".into()),
                    ("enableImageTypes", "Primary".into()),
                    ("imageTypeLimit", "1".into()),
                ],
            )?;
            let people = page.get("Items").and_then(Value::as_array).cloned().unwrap_or_default();
            Ok(people
                .iter()
                .filter_map(|person| {
                    let id = text(person, "Id")?;
                    let tag = person.pointer("/ImageTags/Primary").and_then(Value::as_str);
                    Some(PersonCard { image: photo(&client, &id, tag), name: text(person, "Name").unwrap_or_default(), id })
                })
                .collect())
        })
        .await
    }

    /// A person's page: the person, then the films and shows the person is
    /// in, the newest first, each with the person's role.
    pub async fn person_page(&self, person_id: String) -> Result<PersonPage, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let user = client.user()?.to_string();
            let (person, titles) = std::thread::scope(|scope| {
                let person = scope.spawn(|| client.get::<Value>(&format!("/Items/{person_id}"), &[("userId", user.clone())]));
                let titles = scope.spawn(|| {
                    client.get::<ItemsResult>(
                        "/Items",
                        &[
                            ("userId", user.clone()),
                            ("personIds", person_id.clone()),
                            ("recursive", "true".into()),
                            ("includeItemTypes", "Movie,Series".into()),
                            ("sortBy", "PremiereDate,ProductionYear,SortName".into()),
                            ("sortOrder", "Descending".into()),
                            ("limit", "300".into()),
                            ("fields", format!("{},People", card_fields())),
                            ("enableImageTypes", "Primary,Backdrop,Thumb".into()),
                            ("imageTypeLimit", "1".into()),
                        ],
                    )
                });
                let stopped = || anyhow::anyhow!("person request stopped");
                anyhow::Ok((person.join().map_err(|_| stopped())??, titles.join().map_err(|_| stopped())??))
            })?;
            let tag = person.pointer("/ImageTags/Primary").and_then(Value::as_str);
            let birthplace = person
                .get("ProductionLocations")
                .and_then(Value::as_array)
                .and_then(|places| places.first())
                .and_then(Value::as_str)
                .map(str::to_string);
            Ok(PersonPage {
                id: person_id.clone(),
                name: text(&person, "Name").unwrap_or_default(),
                image: photo(&client, &person_id, tag),
                overview: text(&person, "Overview"),
                born: day(&person, "PremiereDate"),
                died: day(&person, "EndDate"),
                birthplace,
                credits: titles.items.iter().map(|item| Credit { item: media(&client, item), role: role_of(item, &person_id) }).collect(),
            })
        })
        .await
    }

    /// A home row in full, for its "See all" (Home loads 12 and 24).
    pub async fn home_row(&self, row: HomeRow) -> Result<Vec<MediaItem>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let items = match row {
                HomeRow::ContinueWatching => client.resume(200)?,
                HomeRow::NextUp => client.next_up(200)?,
            };
            Ok(medias(&client, &items))
        })
        .await
    }

    /// Films and shows the user has not seen, in a random order: the
    /// "Pick something" row of a Home with no personal rows.
    pub async fn picks(&self, limit: u32) -> Result<Vec<MediaItem>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let page: ItemsResult = client.get(
                "/Items",
                &[
                    ("userId", client.user()?.to_string()),
                    ("recursive", "true".into()),
                    ("includeItemTypes", "Movie,Series".into()),
                    ("filters", "IsUnplayed".into()),
                    ("sortBy", "Random".into()),
                    ("limit", limit.to_string()),
                    ("fields", card_fields()),
                    ("enableImageTypes", "Primary,Backdrop,Thumb".into()),
                    ("imageTypeLimit", "1".into()),
                ],
            )?;
            Ok(medias(&client, &page.items))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kind_of_credit_reads_as_words() {
        assert_eq!(spaced("GuestStar"), "Guest star");
        assert_eq!(spaced("Director"), "Director");
    }
}
