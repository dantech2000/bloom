// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the pages of the app show, and how it loads: the data of the home
//! page, a library and a detail page, the query of a library view, and the
//! requests of each page, which run at the same time where they do not
//! depend on each other. No window here: the app calls a loader on a
//! background thread and puts the answer on the page.

use anyhow::Result;

use crate::jellyfin::{Client, Item, ItemKind, ItemQuery, ItemsResult, Person, SeerrItem};

pub const PAGE_SIZE: usize = 100;
/// Slides in the home hero.
pub const HERO_ITEMS: usize = 20;
/// Id of the synthetic library that lists the user's favourites.
pub const FAVORITES_ID: &str = "favorites";
/// Id prefix of the synthetic library that lists a person's titles.
pub const PERSON_VIEW: &str = "person";
/// Id of the synthetic library that lists the playlists.
pub const PLAYLISTS_ID: &str = "playlists";

#[derive(Default)]
pub struct HomeData {
    pub loading: bool,
    /// Section kinds in display order (`homesection0..`).
    pub order: Vec<String>,
    /// Items of the hero slideshow at the top of the page.
    pub hero: Vec<Item>,
    pub resume: Vec<Item>,
    pub next_up: Vec<Item>,
    pub latest: Vec<(Item, Vec<Item>)>,
}

pub struct LibraryData {
    pub view: Item,
    pub title: String,
    pub loading: bool,
    /// The items of the current page.
    pub items: Vec<Item>,
    pub total: usize,
    /// Index of the first item of the current page.
    pub start: usize,
    pub sort: &'static str,
    pub descending: bool,
    /// One of the server's item filters, such as "IsUnplayed".
    pub filter: Option<&'static str>,
    /// Letter picked at the right edge; '#' stands for names before "A".
    pub letter: Option<char>,
    /// What of the library shows; picked in the menu of the title.
    pub show: LibraryShow,
}

/// The views of a library, as in the title menu of the web client.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LibraryShow {
    /// The movies or the shows of the library.
    #[default]
    All,
    Favorites,
    /// The collections of the server (in a movie library).
    Collections,
    /// All episodes (in a show library).
    Episodes,
}

impl LibraryShow {
    pub fn label(self, library: &str) -> String {
        match self {
            LibraryShow::All => library.to_string(),
            LibraryShow::Favorites => "Favorites".to_string(),
            LibraryShow::Collections => "Collections".to_string(),
            LibraryShow::Episodes => "Episodes".to_string(),
        }
    }

    /// The views a library of this collection type has.
    pub fn of(collection_type: Option<&str>) -> &'static [LibraryShow] {
        match collection_type {
            Some("movies") => &[
                LibraryShow::All,
                LibraryShow::Favorites,
                LibraryShow::Collections,
            ],
            Some("tvshows") => &[
                LibraryShow::All,
                LibraryShow::Favorites,
                LibraryShow::Episodes,
            ],
            _ => &[],
        }
    }
}

impl LibraryData {
    pub fn new(view: Item) -> Self {
        Self {
            title: view.name.clone(),
            view,
            loading: true,
            items: Vec::new(),
            total: 0,
            start: 0,
            sort: "SortName",
            descending: false,
            filter: None,
            letter: None,
            show: LibraryShow::All,
        }
    }
}

/// Sort keys of the library toolbar: label and the server's `sortBy` value.
pub const LIBRARY_SORTS: [(&str, &str); 8] = [
    ("Name", "SortName"),
    ("Random", "Random"),
    ("Community Rating", "CommunityRating,SortName"),
    ("Critic Rating", "CriticRating,SortName"),
    ("Date Added", "DateCreated,SortName"),
    ("Date Played", "DatePlayed,SortName"),
    ("Release Date", "ProductionYear,PremiereDate,SortName"),
    ("Runtime", "Runtime,SortName"),
];

/// Filters of the library toolbar: label and the server's `filters` value.
pub const LIBRARY_FILTERS: [(&str, &str); 4] = [
    ("Unplayed", "IsUnplayed"),
    ("Played", "IsPlayed"),
    ("Favorites", "IsFavorite"),
    ("Resumable", "IsResumable"),
];

#[derive(Default)]
pub struct DetailData {
    pub item: Item,
    pub loading: bool,
    /// Seasons of a series.
    pub seasons: Vec<Item>,
    /// Episodes of a season.
    pub episodes: Vec<Item>,
    /// Next episode to watch of a series.
    pub next_up: Option<Item>,
    /// "More Like This" for a movie or a series.
    pub similar: Vec<Item>,
    /// Audio track picked on the page: its place among the audio streams.
    pub audio: Option<usize>,
    /// Subtitle picked on the page: `Some(None)` is "Off".
    pub subtitle: Option<Option<usize>>,
    /// The cast and crew of the item and its guest stars, each person once
    /// with the roles joined ([`merged_people`]); made when the item loads.
    pub cast: Vec<(Person, String)>,
    pub guests: Vec<(Person, String)>,
}

impl DetailData {
    /// Puts the item of the page in place, with its cast and guests.
    pub fn set_item(&mut self, item: Item) {
        self.cast = merged_people(&item.people, false);
        self.guests = merged_people(&item.people, true);
        self.item = item;
    }
}

/// The people of an item in credit order, the guest stars or the others.
/// The crew roles of one person join into one entry.
pub fn merged_people(people: &[Person], guests: bool) -> Vec<(Person, String)> {
    let mut out: Vec<(Person, String)> = Vec::new();
    for person in people {
        if (person.kind == "GuestStar") != guests {
            continue;
        }
        let role = match person.kind.as_str() {
            "Actor" | "GuestStar" => person
                .role
                .clone()
                .filter(|r| !r.is_empty())
                .map(|r| format!("as {r}"))
                .unwrap_or_default(),
            other => other.to_string(),
        };
        match out.iter_mut().find(|(p, _)| p.id == person.id) {
            Some((_, roles)) if !role.is_empty() && !roles.contains(&role) => {
                roles.push_str(" / ");
                roles.push_str(&role);
            }
            Some(_) => {}
            None => out.push((person.clone(), role)),
        }
    }
    out
}

/// The query of the page of a library that is on show.
pub fn library_query(data: &LibraryData) -> ItemQuery {
    let view = &data.view;
    let favorites = view.collection_type.as_deref() == Some(FAVORITES_ID);
    let person = view.collection_type.as_deref() == Some(PERSON_VIEW);
    let include_types = match view.collection_type.as_deref() {
        Some("movies") => vec!["Movie".to_string()],
        Some("tvshows") => vec!["Series".to_string()],
        Some(FAVORITES_ID) | Some(PERSON_VIEW) => {
            vec!["Movie".to_string(), "Series".to_string()]
        }
        Some(PLAYLISTS_ID) => vec!["Playlist".to_string()],
        _ => Vec::new(),
    };
    let include_types = match data.show {
        LibraryShow::Collections => vec!["BoxSet".to_string()],
        LibraryShow::Episodes => vec!["Episode".to_string()],
        _ => include_types,
    };
    // The collections are not inside the movie library.
    let collections = data.show == LibraryShow::Collections;
    let playlists = view.collection_type.as_deref() == Some(PLAYLISTS_ID);
    let recursive = !include_types.is_empty();
    let filters: Vec<&str> = (favorites || data.show == LibraryShow::Favorites)
        .then_some("IsFavorite")
        .into_iter()
        .chain(data.filter.filter(|f| *f != "IsFavorite" || data.show != LibraryShow::Favorites))
        .collect();
    ItemQuery {
        parent_id: (!favorites && !person && !collections && !playlists).then(|| view.id.clone()),
        person_id: person.then(|| view.id.clone()),
        filters: (!filters.is_empty()).then(|| filters.join(",")),
        include_types,
        recursive: Some(recursive),
        sort_by: Some(data.sort.to_string()),
        sort_order: Some(
            if data.descending {
                "Descending"
            } else {
                "Ascending"
            }
            .to_string(),
        ),
        name_starts_with: data.letter.filter(|c| *c != '#').map(|c| c.to_string()),
        name_less_than: (data.letter == Some('#')).then(|| "A".to_string()),
        limit: Some(PAGE_SIZE),
        start: Some(data.start),
        ..Default::default()
    }
}

/// The user's views that the app shows as libraries.
pub fn library_views(views: Vec<Item>) -> Vec<Item> {
    views
        .into_iter()
        .filter(|v| {
            !matches!(
                v.collection_type.as_deref(),
                Some("playlists") | Some("livetv") | Some("books") | Some("music")
            )
        })
        .collect()
}

/// The libraries with a "Recently Added" row on the home page.
pub fn home_libraries(views: &[Item]) -> Vec<Item> {
    views
        .iter()
        .filter(|v| {
            matches!(
                v.collection_type.as_deref(),
                Some("movies") | Some("tvshows") | Some("homevideos") | None
            )
        })
        .cloned()
        .collect()
}

/// A hero item is usable only with a backdrop to show.
pub fn hero_usable(item: &Item) -> bool {
    !item.backdrop_image_tags.is_empty()
}

/// Loads the rows of the home page. `libraries` are the libraries the app
/// knows; with none (at sign-in) they load in the same pass as the page,
/// so the page does not wait for them first, and come back as the first
/// value (empty when they were known). The hero slides load with
/// `need_hero`.
pub fn load_home(client: &Client, libraries: Vec<Item>, need_views: bool, need_hero: bool) -> Result<(Vec<Item>, HomeData)> {
    // The requests do not depend on each other, so they run at the same
    // time; the page waits for the slowest one and not for their sum.
    std::thread::scope(|scope| {
        let views = scope.spawn(|| {
            if need_views {
                client.views().map(library_views)
            } else {
                Ok(Vec::new())
            }
        });
        let order = scope.spawn(|| client.home_sections());
        let resume = scope.spawn(|| client.resume(12));
        let next_up = scope.spawn(|| client.next_up(24));
        let hero = scope.spawn(|| {
            if !need_hero {
                return Vec::new();
            }
            // The hero is optional; the page loads without it.
            let mut items = client.hero_items(HERO_ITEMS * 2).unwrap_or_default();
            items.retain(hero_usable);
            items.truncate(HERO_ITEMS);
            items
        });
        let join = |what: &str| anyhow::anyhow!("{what} request stopped");
        // The latest rows need the libraries; they start as soon as those
        // are in, while the rest runs.
        let views = views.join().map_err(|_| join("libraries"))??;
        let libraries = if need_views {
            home_libraries(&views)
        } else {
            libraries
        };
        let latest = std::thread::scope(|scope| {
            let latest: Vec<_> = libraries
                .iter()
                .map(|library| scope.spawn(|| client.latest(&library.id, 16)))
                .collect();
            latest
                .into_iter()
                .map(|handle| handle.join().map_err(|_| join("latest"))?)
                .collect::<Result<Vec<_>>>()
        })?;
        let latest: Vec<(Item, Vec<Item>)> = libraries
            .into_iter()
            .zip(latest)
            .filter(|(_, items)| !items.is_empty())
            .collect();
        Ok((
            views,
            HomeData {
                loading: false,
                // A failed settings read falls back to the web defaults.
                order: order.join().map_err(|_| join("settings"))?.unwrap_or_else(|_| {
                    crate::jellyfin::DEFAULT_HOME_SECTIONS
                        .iter()
                        .map(|s| s.to_string())
                        .collect()
                }),
                resume: resume.join().map_err(|_| join("resume"))??,
                next_up: next_up.join().map_err(|_| join("next up"))??,
                latest,
                hero: hero.join().map_err(|_| join("hero"))?,
            },
        ))
    })
}

/// Loads the item of a detail page and the rows under it. `known` is the
/// item as the card that opened the page had it: it says what kind of item
/// this is, so the requests for its rows start with the item request.
pub fn load_detail(client: &Client, known: &Item) -> Result<DetailData> {
    let item_id = known.id.as_str();
    let is = |wanted: ItemKind| known.kind == wanted;
    let (item, seasons, next_up, episodes, similar) = std::thread::scope(|scope| {
        let item = scope.spawn(|| client.item(item_id));
        let seasons = scope.spawn(|| match is(ItemKind::Series) {
            true => client.seasons(item_id),
            false => Ok(Vec::new()),
        });
        let next_up = scope.spawn(|| match is(ItemKind::Series) {
            true => client.series_next_up(item_id).unwrap_or(None),
            false => None,
        });
        // A season shows its episodes; an episode shows the others of its
        // season.
        let episodes = scope.spawn(|| match (&known.series_id, &known.season_id) {
            (Some(series), _) if is(ItemKind::Season) => client.episodes(series, item_id),
            (Some(series), Some(season)) if is(ItemKind::Episode) => {
                Ok(client.episodes(series, season).unwrap_or_default())
            }
            _ => Ok(Vec::new()),
        });
        // The row is optional; the page loads without it.
        let similar = scope.spawn(|| {
            if is(ItemKind::Movie) || is(ItemKind::Series) {
                client.similar(item_id, 12).unwrap_or_default()
            } else {
                Vec::new()
            }
        });
        let stopped = || anyhow::anyhow!("detail request stopped");
        anyhow::Ok((
            item.join().map_err(|_| stopped())??,
            seasons.join().map_err(|_| stopped())??,
            next_up.join().map_err(|_| stopped())?,
            episodes.join().map_err(|_| stopped())??,
            similar.join().map_err(|_| stopped())?,
        ))
    })?;
    let mut fresh = DetailData {
        seasons,
        next_up,
        episodes,
        similar,
        ..Default::default()
    };
    // An item opened by id alone has no kind until now.
    if known.kind.is_unknown() {
        match item.kind {
            ItemKind::Series => {
                fresh.seasons = client.seasons(item_id)?;
                fresh.next_up = client.series_next_up(item_id).unwrap_or(None);
            }
            ItemKind::Season => {
                if let Some(series) = &item.series_id {
                    fresh.episodes = client.episodes(series, item_id)?;
                }
            }
            _ => {}
        }
        if matches!(item.kind, ItemKind::Movie | ItemKind::Series) {
            fresh.similar = client.similar(item_id, 12).unwrap_or_default();
        }
    }
    // The card that opened an episode page does not always carry the
    // season id; the item does.
    if item.kind == ItemKind::Episode
        && fresh.episodes.is_empty()
        && let (Some(series), Some(season)) = (&item.series_id, &item.season_id)
    {
        fresh.episodes = client.episodes(series, season).unwrap_or_default();
    }
    fresh.set_item(item);
    Ok(fresh)
}

/// Runs a search on the server: the titles and episodes of the library,
/// and the titles of Seerr when the user has it. The two requests do not
/// depend on each other, so they run at the same time.
pub fn load_search(client: &Client, text: &str) -> Result<(Vec<Item>, Vec<SeerrItem>)> {
    let query = ItemQuery {
        search: Some(text.to_string()),
        include_types: vec!["Movie".into(), "Series".into(), "Episode".into()],
        recursive: Some(true),
        limit: Some(100),
        ..Default::default()
    };
    let (page, seerr): (ItemsResult, _) = std::thread::scope(|scope| {
        let page = scope.spawn(|| client.items(&query));
        // Seerr is optional; the library results still show.
        let seerr = scope.spawn(|| {
            if client.seerr_active() {
                client.seerr_search(text).unwrap_or_default()
            } else {
                Vec::new()
            }
        });
        let stopped = || anyhow::anyhow!("search request stopped");
        anyhow::Ok((
            page.join().map_err(|_| stopped())??,
            seerr.join().map_err(|_| stopped())?,
        ))
    })?;
    Ok((page.items, seerr))
}

/// The query of the title index of the search: every movie and series,
/// with the fields of a card and the original title, which the search
/// reads too.
pub fn catalog_query() -> ItemQuery {
    ItemQuery {
        include_types: vec!["Movie".into(), "Series".into()],
        recursive: Some(true),
        sort_by: Some("SortName".into()),
        fields: Some(crate::jellyfin::catalog_fields()),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_net::Routes;

    fn items(ids: &[&str], kind: &str, extra: &str) -> String {
        let items: Vec<String> = ids
            .iter()
            .map(|id| format!(r#"{{"Id":"{id}","Name":"Title {id}","Type":"{kind}"{extra}}}"#))
            .collect();
        format!(r#"{{"Items":[{}],"TotalRecordCount":{}}}"#, items.join(","), items.len())
    }

    fn client(server: &Routes) -> Client {
        Client::new(&server.url, "pages-test").with_session("token", "u1")
    }

    fn library(collection_type: &str) -> LibraryData {
        let view = Item { id: "lib".into(), name: "Library".into(), collection_type: Some(collection_type.into()), ..Default::default() };
        LibraryData::new(view)
    }

    #[test]
    fn a_library_asks_for_its_own_kind_inside_itself() {
        let query = library_query(&library("movies"));
        assert_eq!(query.parent_id.as_deref(), Some("lib"));
        assert_eq!(query.include_types, ["Movie"]);
        assert_eq!(query.recursive, Some(true));
        assert_eq!((query.limit, query.start), (Some(PAGE_SIZE), Some(0)));
        assert_eq!(query.filters, None);
        // A folder with no kind lists what is in it, one level.
        let folder = library_query(&LibraryData::new(Item { id: "f".into(), ..Default::default() }));
        assert!(folder.include_types.is_empty());
        assert_eq!(folder.recursive, Some(false));
    }

    #[test]
    fn the_views_without_a_parent_ask_the_whole_server() {
        for kind in [FAVORITES_ID, PERSON_VIEW, PLAYLISTS_ID] {
            assert_eq!(library_query(&library(kind)).parent_id, None, "{kind}");
        }
        assert_eq!(library_query(&library(PERSON_VIEW)).person_id.as_deref(), Some("lib"));
        assert_eq!(library_query(&library(FAVORITES_ID)).filters.as_deref(), Some("IsFavorite"));
        assert_eq!(library_query(&library(PLAYLISTS_ID)).include_types, ["Playlist"]);
        // The collections are not inside the movie library.
        let mut movies = library("movies");
        movies.show = LibraryShow::Collections;
        let query = library_query(&movies);
        assert_eq!((query.parent_id, query.include_types), (None, vec!["BoxSet".to_string()]));
    }

    #[test]
    fn the_favourite_filter_is_named_once() {
        let mut shows = library("tvshows");
        shows.show = LibraryShow::Favorites;
        shows.filter = Some("IsFavorite");
        assert_eq!(library_query(&shows).filters.as_deref(), Some("IsFavorite"));
        shows.filter = Some("IsUnplayed");
        assert_eq!(library_query(&shows).filters.as_deref(), Some("IsFavorite,IsUnplayed"));
    }

    #[test]
    fn a_letter_narrows_by_the_start_of_the_name() {
        let mut movies = library("movies");
        movies.letter = Some('K');
        movies.descending = true;
        let query = library_query(&movies);
        assert_eq!((query.name_starts_with.as_deref(), query.name_less_than), (Some("K"), None));
        assert_eq!(query.sort_order.as_deref(), Some("Descending"));
        movies.letter = Some('#');
        let query = library_query(&movies);
        assert_eq!((query.name_starts_with, query.name_less_than.as_deref()), (None, Some("A")));
    }

    #[test]
    fn the_libraries_of_the_app_and_of_the_home_rows() {
        let view = |kind: Option<&str>| Item { collection_type: kind.map(str::to_string), ..Default::default() };
        let all = vec![view(Some("movies")), view(Some("music")), view(Some("livetv")), view(Some("boxsets")), view(None)];
        let shown = library_views(all);
        let kinds: Vec<_> = shown.iter().map(|v| v.collection_type.as_deref()).collect();
        assert_eq!(kinds, [Some("movies"), Some("boxsets"), None]);
        let home: Vec<_> = home_libraries(&shown).into_iter().map(|v| v.collection_type).collect();
        assert_eq!(home, [Some("movies".to_string()), None]);
    }

    #[test]
    fn the_home_page_loads_its_libraries_with_its_rows() {
        let server = Routes::start(|_, path| match path {
            p if p.starts_with("/UserViews") => (200, items(&["lib1"], "CollectionFolder", r#","CollectionType":"movies""#)),
            p if p.contains("/Latest") => (200, r#"[{"Id":"new1","Name":"New","Type":"Movie"}]"#.into()),
            p if p.contains("/Resume") => (200, items(&["r1", "r2"], "Movie", "")),
            _ => (200, items(&[], "Movie", "")),
        });
        let (views, home) = load_home(&client(&server), Vec::new(), true, false).unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(home.resume.len(), 2);
        assert_eq!(home.latest.len(), 1, "the row of the library that loaded in the same pass");
        assert_eq!(home.latest[0].1[0].id, "new1");
        assert!(!home.order.is_empty(), "the sections fall back to the defaults of the web client");
        assert!(home.hero.is_empty() && !home.loading);
        // With the libraries known they are not asked for again.
        let before = server.count("/UserViews");
        let (again, _) = load_home(&client(&server), views, false, false).unwrap();
        assert!(again.is_empty());
        assert_eq!(server.count("/UserViews"), before);
    }

    #[test]
    fn a_home_page_whose_rows_fail_is_an_error() {
        let server = Routes::start(|_, path| match path {
            p if p.contains("/Resume") => (500, String::new()),
            _ => (200, items(&[], "Movie", "")),
        });
        assert!(load_home(&client(&server), Vec::new(), true, false).is_err());
    }

    #[test]
    fn an_item_opened_by_id_alone_gets_the_rows_of_its_kind() {
        let server = Routes::start(|_, path| match path {
            p if p.starts_with("/Shows/s1/Seasons") => (200, items(&["season1", "season2"], "Season", "")),
            p if p.starts_with("/Items/s1/Similar") => (200, items(&["other"], "Series", "")),
            p if p.starts_with("/Items/s1") => (200, r#"{"Id":"s1","Name":"Show","Type":"Series"}"#.into()),
            _ => (200, items(&[], "Episode", "")),
        });
        // The card says it is a series: its rows start with the item.
        let known = Item { id: "s1".into(), kind: ItemKind::Series, ..Default::default() };
        let detail = load_detail(&client(&server), &known).unwrap();
        assert_eq!((detail.item.name.as_str(), detail.seasons.len(), detail.similar.len()), ("Show", 2, 1));
        assert_eq!(server.count("/Shows/s1/Seasons"), 1);
        // An id alone: the rows follow once the item says its kind.
        let known = Item { id: "s1".into(), ..Default::default() };
        let detail = load_detail(&client(&server), &known).unwrap();
        assert_eq!((detail.seasons.len(), detail.similar.len()), (2, 1));
        assert_eq!(server.count("/Shows/s1/Seasons"), 2);
    }

    #[test]
    fn a_detail_page_without_its_item_is_an_error_and_without_similar_is_not() {
        let server = Routes::start(|_, path| match path {
            p if p.contains("/Similar") => (500, String::new()),
            p if p.starts_with("/Items/m1") => (200, r#"{"Id":"m1","Name":"Film","Type":"Movie"}"#.into()),
            _ => (404, String::new()),
        });
        let movie = |id: &str| Item { id: id.into(), kind: ItemKind::Movie, ..Default::default() };
        let detail = load_detail(&client(&server), &movie("m1")).unwrap();
        assert!(detail.similar.is_empty());
        assert!(load_detail(&client(&server), &movie("gone")).is_err());
    }

    #[test]
    fn a_person_shows_once_with_the_roles_joined() {
        let person = |id: &str, kind: &str, role: Option<&str>| Person {
            id: id.into(),
            name: id.to_uppercase(),
            role: role.map(str::to_string),
            kind: kind.into(),
            primary_image_tag: None,
        };
        let people = [
            person("a", "Actor", Some("Hero")),
            person("b", "Director", None),
            person("b", "Writer", None),
            person("b", "Writer", None),
            person("c", "GuestStar", Some("")),
            person("a", "Actor", Some("Twin")),
        ];
        let cast: Vec<(String, String)> = merged_people(&people, false).into_iter().map(|(p, roles)| (p.id, roles)).collect();
        assert_eq!(cast, [("a".into(), "as Hero / as Twin".into()), ("b".into(), "Director / Writer".into())]);
        let guests = merged_people(&people, true);
        assert_eq!((guests.len(), guests[0].0.id.as_str(), guests[0].1.as_str()), (1, "c", ""));
    }

    #[test]
    fn a_search_asks_for_titles_and_episodes() {
        let server = Routes::start(|_, path| match path {
            p if p.starts_with("/Items?") && p.contains("searchTerm=pilot") => (200, items(&["e1", "e2"], "Episode", "")),
            _ => (404, String::new()),
        });
        let (found, seerr) = load_search(&client(&server), "pilot").unwrap();
        assert_eq!(found.len(), 2);
        assert!(seerr.is_empty());
        assert_eq!(server.count("includeItemTypes=Movie,Series,Episode"), 1);
    }

    #[test]
    fn the_title_index_asks_for_the_original_titles() {
        let fields = catalog_query().fields.unwrap();
        assert!(fields.contains("OriginalTitle") && fields.contains("Overview"), "{fields}");
    }
}
