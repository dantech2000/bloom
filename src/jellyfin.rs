// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Minimal blocking Jellyfin REST client. Call it from a background thread.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::config::{APP_NAME, APP_VERSION};

pub const TICKS_PER_SECOND: i64 = 10_000_000;

#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    pub base: Arc<str>,
    pub device_id: Arc<str>,
    pub token: Option<Arc<str>>,
    pub user_id: Option<Arc<str>>,
    /// The id the server gives itself; with the user id, the identity a
    /// playback report belongs to. Set for the client of a session.
    pub server_id: Option<Arc<str>>,
}

impl Client {
    pub fn new(base: &str, device_id: &str) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
            base: base.trim_end_matches('/').into(),
            device_id: device_id.into(),
            token: None,
            user_id: None,
            server_id: None,
        }
    }

    pub fn with_session(mut self, token: &str, user_id: &str) -> Self {
        self.token = Some(token.into());
        self.user_id = Some(user_id.into());
        // The image loader sends this header to this server only.
        register_image_auth(&self.base, self.auth_header());
        self
    }

    pub fn with_server(mut self, server_id: &str) -> Self {
        self.server_id = Some(server_id.into());
        self
    }

    /// The HTTP agent, for a request this client has no method for.
    pub(crate) fn agent(&self) -> &ureq::Agent {
        &self.agent
    }

    pub(crate) fn user(&self) -> Result<&str> {
        self.user_id
            .as_deref()
            .ok_or_else(|| anyhow!("not signed in"))
    }

    pub(crate) fn auth_header(&self) -> String {
        let device = hostname();
        let mut value = format!(
            "MediaBrowser Client=\"{APP_NAME}\", Device=\"{device}\", DeviceId=\"{}\", Version=\"{APP_VERSION}\"",
            self.device_id
        );
        if let Some(token) = &self.token {
            value.push_str(&format!(", Token=\"{token}\""));
        }
        value
    }

    /// Header for mpv so it can fetch the stream with this session. mpv splits
    /// header lists on commas, so only the comma-free token form is used.
    pub fn mpv_auth_header(&self) -> String {
        match &self.token {
            Some(token) => format!("Authorization: MediaBrowser Token=\"{token}\""),
            None => String::new(),
        }
    }

    pub(crate) fn url(&self, path: &str, query: &[(&str, String)]) -> String {
        let mut url = format!("{}{}", self.base, path);
        let mut first = true;
        for (k, v) in query {
            if v.is_empty() {
                continue;
            }
            url.push(if first { '?' } else { '&' });
            first = false;
            url.push_str(k);
            url.push('=');
            url.push_str(&urlencode(v));
        }
        url
    }

    pub(crate) fn get<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
        let url = self.url(path, query);
        let mut response = self
            .agent
            .get(&url)
            .header("Authorization", self.auth_header())
            .header("Accept", "application/json")
            .call()
            .map_err(|e| map_err(e, path))?;
        response
            .body_mut()
            .read_json::<T>()
            .map_err(|e| map_body_err(e, "decode", path))
    }

    pub(crate) fn post<B: Serialize>(&self, path: &str, body: &B) -> Result<ureq::Body> {
        let url = self.url(path, &[]);
        let response = self
            .agent
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("Accept", "application/json")
            .send_json(body)
            .map_err(|e| map_err(e, path))?;
        Ok(response.into_body())
    }

    /// Body-less request (POST/DELETE) that decodes a JSON response.
    pub(crate) fn send<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
        let url = self.url(path, query);
        let request = match method {
            "DELETE" => self.agent.delete(&url).force_send_body(),
            _ => self.agent.post(&url),
        };
        let mut response = request
            .header("Authorization", self.auth_header())
            .header("Accept", "application/json")
            .send_empty()
            .map_err(|e| map_err(e, path))?;
        response
            .body_mut()
            .read_json::<T>()
            .map_err(|e| map_body_err(e, "decode", path))
    }

    /// POST or DELETE with no body, for calls whose answer is empty.
    pub(crate) fn call(&self, method: &str, path: &str, query: &[(&str, String)]) -> Result<()> {
        let url = self.url(path, query);
        let request = match method {
            "DELETE" => self.agent.delete(&url).force_send_body(),
            _ => self.agent.post(&url),
        };
        request
            .header("Authorization", self.auth_header())
            .send_empty()
            .map_err(|e| map_err(e, path))?;
        Ok(())
    }

    /// GET that returns the body as text (log files).
    pub(crate) fn get_text(&self, path: &str, query: &[(&str, String)]) -> Result<String> {
        let url = self.url(path, query);
        let mut response = self
            .agent
            .get(&url)
            .header("Authorization", self.auth_header())
            .call()
            .map_err(|e| map_err(e, path))?;
        response
            .body_mut()
            .read_to_string()
            .map_err(|e| map_body_err(e, "read", path))
    }

    /// True when the signed-in user is a server administrator.
    /// What the server knows of the signed-in user: whether the user is an
    /// administrator, the audio language the user prefers ("eng"), and what
    /// the user may do with SyncPlay.
    pub fn user_settings(&self) -> Result<(bool, Option<String>, String)> {
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Me {
            #[serde(default)]
            policy: Policy,
            #[serde(default)]
            configuration: Configuration,
        }
        #[derive(Default, Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Policy {
            #[serde(default)]
            is_administrator: bool,
            /// "CreateAndJoinGroups", "JoinGroups" or "None".
            #[serde(default)]
            sync_play_access: String,
        }
        #[derive(Default, Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Configuration {
            #[serde(default)]
            audio_language_preference: Option<String>,
        }
        let me: Me = self.get("/Users/Me", &[])?;
        let language = me
            .configuration
            .audio_language_preference
            .filter(|language| !language.is_empty());
        Ok((me.policy.is_administrator, language, me.policy.sync_play_access))
    }

    /// Sets the picture of the signed-in user. The server takes the image
    /// as base64 text, with the type of the image as the content type.
    pub fn upload_user_image(&self, bytes: &[u8], mime: &str) -> Result<()> {
        use base64::Engine as _;
        let user = self.user()?.to_string();
        let body = base64::engine::general_purpose::STANDARD.encode(bytes);
        let send = |path: &str, query: &[(&str, String)]| {
            self.agent
                .post(&self.url(path, query))
                .header("Authorization", self.auth_header())
                .header("Content-Type", mime)
                .send(body.as_str())
        };
        // The address since server 10.9, then the one before it.
        match send("/UserImage", &[("userId", user.clone())]) {
            Ok(_) => Ok(()),
            Err(ureq::Error::StatusCode(404 | 405)) => send(&format!("/Users/{user}/Images/Primary"), &[])
                .map(drop)
                .map_err(|e| map_err(e, "/Users/Images/Primary")),
            Err(err) => Err(map_err(err, "/UserImage")),
        }
    }

    /// Posts an image as base64 text with its type as the content type, as
    /// the server expects for the splash screen.
    pub fn post_image(&self, path: &str, bytes: &[u8], mime: &str) -> Result<()> {
        use base64::Engine as _;
        let body = base64::engine::general_purpose::STANDARD.encode(bytes);
        self.agent
            .post(&self.url(path, &[]))
            .header("Authorization", self.auth_header())
            .header("Content-Type", mime)
            .send(body.as_str())
            .map(drop)
            .map_err(|e| map_err(e, path))
    }

    /// Removes the picture of the signed-in user.
    pub fn delete_user_image(&self) -> Result<()> {
        let user = self.user()?.to_string();
        match self.call("DELETE", "/UserImage", &[("userId", user.clone())]) {
            Ok(()) => Ok(()),
            Err(_) => self.call("DELETE", &format!("/Users/{user}/Images/Primary"), &[]),
        }
    }

    /// The tag of the picture of the signed-in user; none without a picture.
    pub fn user_image_tag(&self) -> Result<Option<String>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Me {
            #[serde(default)]
            primary_image_tag: Option<String>,
        }
        Ok(self.get::<Me>("/Users/Me", &[])?.primary_image_tag)
    }

    // ----- Discovery & auth -------------------------------------------------

    pub fn public_info(&self) -> Result<PublicSystemInfo> {
        self.get("/System/Info/Public", &[])
    }

    pub fn public_users(&self) -> Result<Vec<User>> {
        self.get("/Users/Public", &[])
    }

    pub fn authenticate(&self, username: &str, password: &str) -> Result<AuthResult> {
        #[derive(Serialize)]
        #[serde(rename_all = "PascalCase")]
        struct Body<'a> {
            username: &'a str,
            pw: &'a str,
        }
        let mut body = self.post(
            "/Users/AuthenticateByName",
            &Body {
                username,
                pw: password,
            },
        )?;
        body.read_json().context("decode auth response")
    }

    // ----- Browsing ---------------------------------------------------------

    pub fn views(&self) -> Result<Vec<Item>> {
        let user = self.user()?.to_string();
        let result: ItemsResult = self.get("/UserViews", &[("userId", user)])?;
        Ok(result.items)
    }

    /// The user's home screen sections in order, as set in the web client.
    pub fn home_sections(&self) -> Result<Vec<String>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Prefs {
            #[serde(default)]
            custom_prefs: std::collections::HashMap<String, Option<String>>,
        }
        let user = self.user()?.to_string();
        let prefs: Prefs = self.get(
            "/DisplayPreferences/usersettings",
            &[("userId", user), ("client", "emby".to_string())],
        )?;
        Ok(DEFAULT_HOME_SECTIONS
            .iter()
            .enumerate()
            .map(|(i, default)| {
                prefs
                    .custom_prefs
                    .get(&format!("homesection{i}"))
                    .cloned()
                    .flatten()
                    .unwrap_or_else(|| default.to_string())
            })
            .collect())
    }

    /// Random unwatched movies and series with a backdrop, for the home hero.
    /// The query mirrors the Media Bar plugin of the web client.
    pub fn hero_items(&self, limit: usize) -> Result<Vec<Item>> {
        let user = self.user()?.to_string();
        let result: ItemsResult = self.get(
            "/Items",
            &[
                ("userId", user),
                ("includeItemTypes", "Movie,Series".to_string()),
                ("recursive", "true".to_string()),
                ("hasOverview", "true".to_string()),
                ("imageTypes", "Logo,Backdrop".to_string()),
                ("sortBy", "Random".to_string()),
                ("isPlayed", "false".to_string()),
                ("enableUserData", "true".to_string()),
                ("limit", limit.to_string()),
                (
                    "fields",
                    format!(
                        "{ITEM_FIELDS},CommunityRating,CriticRating,OfficialRating,ChildCount,\
                         RemoteTrailers"
                    ),
                ),
                ("enableImageTypes", "Primary,Backdrop,Logo".to_string()),
            ],
        )?;
        Ok(result.items)
    }

    pub fn item(&self, id: &str) -> Result<Item> {
        let user = self.user()?.to_string();
        self.get(&format!("/Items/{id}"), &[("userId", user)])
    }

    pub fn items(&self, q: &ItemQuery) -> Result<ItemsResult> {
        let user = self.user()?.to_string();
        let mut params: Vec<(&str, String)> = vec![
            ("userId", user),
            ("parentId", q.parent_id.clone().unwrap_or_default()),
            ("includeItemTypes", q.include_types.join(",")),
            (
                "recursive",
                q.recursive.map(|b| b.to_string()).unwrap_or_default(),
            ),
            ("sortBy", q.sort_by.clone().unwrap_or_default()),
            ("sortOrder", q.sort_order.clone().unwrap_or_default()),
            ("searchTerm", q.search.clone().unwrap_or_default()),
            ("limit", q.limit.map(|n| n.to_string()).unwrap_or_default()),
            (
                "startIndex",
                q.start.map(|n| n.to_string()).unwrap_or_default(),
            ),
            ("fields", item_fields()),
            ("enableImageTypes", "Primary,Backdrop,Thumb".to_string()),
            ("imageTypeLimit", "1".to_string()),
        ];
        if let Some(filters) = &q.filters {
            params.push(("filters", filters.clone()));
        }
        if let Some(prefix) = &q.name_starts_with {
            params.push(("nameStartsWith", prefix.clone()));
        }
        if let Some(limit) = &q.name_less_than {
            params.push(("nameLessThan", limit.clone()));
        }
        if let Some(person) = &q.person_id {
            params.push(("personIds", person.clone()));
        }
        self.get("/Items", &params)
    }

    pub fn resume(&self, limit: usize) -> Result<Vec<Item>> {
        let user = self.user()?.to_string();
        let result: ItemsResult = self.get(
            "/UserItems/Resume",
            &[
                ("userId", user),
                ("limit", limit.to_string()),
                ("fields", item_fields()),
                ("mediaTypes", "Video".to_string()),
                ("enableImageTypes", "Primary,Backdrop,Thumb".to_string()),
            ],
        )?;
        Ok(result.items)
    }

    pub fn next_up(&self, limit: usize) -> Result<Vec<Item>> {
        let user = self.user()?.to_string();
        let result: ItemsResult = self.get(
            "/Shows/NextUp",
            &[
                ("userId", user),
                ("limit", limit.to_string()),
                ("fields", item_fields()),
                ("enableImageTypes", "Primary,Backdrop,Thumb".to_string()),
            ],
        )?;
        Ok(result.items)
    }

    pub fn latest(&self, parent_id: &str, limit: usize) -> Result<Vec<Item>> {
        let user = self.user()?.to_string();
        self.get(
            "/Items/Latest",
            &[
                ("userId", user),
                ("parentId", parent_id.to_string()),
                ("limit", limit.to_string()),
                ("fields", item_fields()),
                ("enableImageTypes", "Primary,Backdrop,Thumb".to_string()),
            ],
        )
    }

    pub fn seasons(&self, series_id: &str) -> Result<Vec<Item>> {
        let user = self.user()?.to_string();
        let result: ItemsResult = self.get(
            &format!("/Shows/{series_id}/Seasons"),
            &[("userId", user), ("fields", item_fields())],
        )?;
        Ok(result.items)
    }

    pub fn episodes(&self, series_id: &str, season_id: &str) -> Result<Vec<Item>> {
        let user = self.user()?.to_string();
        let result: ItemsResult = self.get(
            &format!("/Shows/{series_id}/Episodes"),
            &[
                ("userId", user),
                ("seasonId", season_id.to_string()),
                ("fields", item_fields()),
            ],
        )?;
        Ok(result.items)
    }

    /// The next unwatched episode of a series, if any.
    pub fn series_next_up(&self, series_id: &str) -> Result<Option<Item>> {
        let user = self.user()?.to_string();
        let result: ItemsResult = self.get(
            "/Shows/NextUp",
            &[
                ("userId", user),
                ("seriesId", series_id.to_string()),
                ("limit", "1".to_string()),
                ("fields", item_fields()),
            ],
        )?;
        Ok(result.items.into_iter().next())
    }

    /// Titles the server ranks as close to an item ("More Like This").
    pub fn similar(&self, item_id: &str, limit: usize) -> Result<Vec<Item>> {
        let user = self.user()?.to_string();
        let result: ItemsResult = self.get(
            &format!("/Items/{item_id}/Similar"),
            &[
                ("userId", user),
                ("limit", limit.to_string()),
                ("fields", item_fields()),
            ],
        )?;
        Ok(result.items)
    }

    /// Hides an item from "Continue Watching" or "Next Up" without a change
    /// to its played state. This needs the Jellyfin Enhanced server plugin.
    pub fn hide_from_row(&self, next_up: bool, item_id: &str) -> Result<()> {
        let row = if next_up { "next-up" } else { "continue-watching" };
        let path = format!("/JellyfinEnhanced/{row}/hide/{item_id}");
        self.agent
            .post(&self.url(&path, &[]))
            .header("Authorization", self.auth_header())
            .send_empty()
            .map_err(|e| map_err(e, &path))?;
        Ok(())
    }

    /// Intro, credits and similar ranges of an item. The Intro Skipper plugin
    /// fills these on the server.
    pub fn media_segments(&self, item_id: &str) -> Result<Vec<MediaSegment>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Page {
            #[serde(default)]
            items: Vec<MediaSegment>,
        }
        let page: Page = self.get(&format!("/MediaSegments/{item_id}"), &[])?;
        Ok(page.items)
    }

    /// The quality tag settings of the user in the Jellyfin Enhanced plugin:
    /// tags on, resolution tag, dynamic range tag. `None` without the plugin.
    pub fn quality_tag_settings(&self) -> Option<(bool, bool, bool)> {
        self.quality_prefs()
    }

    // ----- Seerr, through the Jellyfin Enhanced plugin -----------------------

    /// True when the user has a linked Seerr account for requests.
    pub fn seerr_active(&self) -> bool {
        #[derive(Deserialize)]
        struct Status {
            #[serde(default)]
            active: bool,
            #[serde(rename = "userFound", default)]
            user_found: bool,
        }
        self.get::<Status>("/JellyfinEnhanced/jellyseerr/user-status", &[])
            .map(|s| s.active && s.user_found)
            .unwrap_or(false)
    }

    /// Titles Seerr knows for a search text, in the library or not.
    pub fn seerr_search(&self, query: &str) -> Result<Vec<SeerrItem>> {
        #[derive(Deserialize)]
        struct Page {
            #[serde(default)]
            results: Vec<SeerrItem>,
        }
        let page: Page = self.get(
            "/JellyfinEnhanced/jellyseerr/search",
            &[
                ("query", query.to_string()),
                ("page", "1".to_string()),
                ("language", "en".to_string()),
            ],
        )?;
        Ok(page
            .results
            .into_iter()
            .filter(|r| r.media_type == "movie" || r.media_type == "tv")
            .collect())
    }

    /// Asks Seerr for a movie, or for all seasons of a series.
    pub fn seerr_request(&self, item: &SeerrItem) -> Result<()> {
        let body = if item.media_type == "tv" {
            serde_json::json!({ "mediaType": "tv", "mediaId": item.id, "seasons": "all" })
        } else {
            serde_json::json!({ "mediaType": "movie", "mediaId": item.id })
        };
        self.post("/JellyfinEnhanced/jellyseerr/request", &body)?;
        Ok(())
    }

    // ----- User data --------------------------------------------------------

    pub fn set_played(&self, item_id: &str, played: bool) -> Result<UserData> {
        let user = self.user()?.to_string();
        let method = if played { "POST" } else { "DELETE" };
        self.send(
            method,
            &format!("/UserPlayedItems/{item_id}"),
            &[("userId", user)],
        )
    }

    pub fn set_favorite(&self, item_id: &str, favorite: bool) -> Result<UserData> {
        let user = self.user()?.to_string();
        let method = if favorite { "POST" } else { "DELETE" };
        self.send(
            method,
            &format!("/UserFavoriteItems/{item_id}"),
            &[("userId", user)],
        )
    }

    // ----- Media URLs -------------------------------------------------------

    /// Image endpoints are anonymous on Jellyfin, so the URL can be handed to
    /// gpui's image loader directly.
    pub fn image_url(
        &self,
        item_id: &str,
        kind: &str,
        tag: Option<&str>,
        max_width: u32,
    ) -> String {
        // A few fixed widths, so the image a card asks for is the same file
        // at every window size, and the cache on disk can serve it.
        let max_width = [160, 240, 320, 400, 480, 640, 800, 1000, 1280, 1600, 1920]
            .into_iter()
            .find(|step| *step >= max_width)
            .unwrap_or(max_width);
        let mut url = format!(
            "{}/Items/{item_id}/Images/{kind}?maxWidth={max_width}&quality=90",
            self.base
        );
        if let Some(tag) = tag {
            url.push_str("&tag=");
            url.push_str(tag);
        }
        url
    }

    /// Address of an item's page in the web client.
    pub fn web_url(&self, item_id: &str) -> String {
        format!("{}/web/#/details?id={item_id}", self.base)
    }

    /// Address of a plugin's page in the web dashboard; its settings are there.
    pub fn web_plugin_url(&self, plugin_id: &str) -> String {
        format!("{}/web/#/dashboard/plugins/{plugin_id}", self.base)
    }

    /// The file of an item as it is, for the media source the server named
    /// for it (the id of the item itself for the one version of a file).
    pub fn stream_url(&self, item_id: &str, media_source_id: &str) -> String {
        format!(
            "{}/Videos/{item_id}/stream?static=true&mediaSourceId={media_source_id}",
            self.base
        )
    }

    pub fn user_image_url(&self, user_id: &str, tag: &str) -> String {
        format!(
            "{}/Users/{user_id}/Images/Primary?maxWidth=96&tag={tag}",
            self.base
        )
    }

    // ----- Playback reporting ----------------------------------------------

    pub fn report_start(&self, p: &Progress) -> Result<()> {
        if no_report() {
            log_unsent("start", p);
            return Ok(());
        }
        self.post("/Sessions/Playing", &p.body(true)).map(drop)
    }

    pub fn report_progress(&self, p: &Progress) -> Result<()> {
        if no_report() {
            log_unsent("progress", p);
            return Ok(());
        }
        self.post("/Sessions/Playing/Progress", &p.body(true))
            .map(drop)
    }

    pub fn report_stopped(&self, p: &Progress) -> Result<()> {
        if no_report() {
            log_unsent("stopped", p);
            return Ok(());
        }
        self.post("/Sessions/Playing/Stopped", &p.body(false))
            .map(drop)
    }

    // ----- Sign-in page, Quick Connect, sign out ------------------------------

    /// What the server shows on its sign-in page. Needs no sign-in.
    pub fn branding(&self) -> Result<Branding> {
        self.get("/Branding/Configuration", &[])
    }

    /// Image the server makes from the library art, for the sign-in page.
    pub fn splashscreen_url(&self) -> String {
        format!("{}/Branding/Splashscreen?format=jpg&quality=80", self.base)
    }

    pub fn quick_connect_enabled(&self) -> Result<bool> {
        self.get("/QuickConnect/Enabled", &[])
    }

    /// Starts a Quick Connect sign-in: the server gives a code to show and a
    /// secret to ask with.
    pub fn quick_connect_initiate(&self) -> Result<QuickConnect> {
        self.send("POST", "/QuickConnect/Initiate", &[])
    }

    /// State of a sign-in that `quick_connect_initiate` started.
    pub fn quick_connect_state(&self, secret: &str) -> Result<QuickConnect> {
        self.get("/QuickConnect/Connect", &[("secret", secret.to_string())])
    }

    /// Changes an approved Quick Connect secret into a session.
    pub fn authenticate_quick_connect(&self, secret: &str) -> Result<AuthResult> {
        #[derive(Serialize)]
        #[serde(rename_all = "PascalCase")]
        struct Body<'a> {
            secret: &'a str,
        }
        let mut body = self.post("/Users/AuthenticateWithQuickConnect", &Body { secret })?;
        body.read_json().context("decode auth response")
    }

    /// Lets the device that shows `code` sign in as this user.
    pub fn quick_connect_authorize(&self, code: &str) -> Result<()> {
        self.call(
            "POST",
            "/QuickConnect/Authorize",
            &[("code", code.to_string()), ("userId", self.user()?.to_string())],
        )
    }

    /// Ends this session on the server; its token stops working.
    pub fn logout(&self) -> Result<()> {
        self.call("POST", "/Sessions/Logout", &[])
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Branding {
    #[serde(default)]
    pub login_disclaimer: Option<String>,
    #[serde(default)]
    pub splashscreen_enabled: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct QuickConnect {
    #[serde(default)]
    pub authenticated: bool,
    #[serde(default)]
    pub secret: String,
    #[serde(default)]
    pub code: String,
}

/// The web client's default home section order.
pub const DEFAULT_HOME_SECTIONS: [&str; 7] = [
    "smalllibrarytiles",
    "resume",
    "resumeaudio",
    "resumebook",
    "livetv",
    "nextup",
    "latestmedia",
];

const ITEM_FIELDS: &str =
    "Overview,PrimaryImageAspectRatio,ProductionYear,Genres,ParentId,DateCreated,Status,EndDate";

/// Quality tags on poster cards, as bits: on, resolution tag, range tag.
static QUALITY_TAGS: AtomicU8 = AtomicU8::new(0);
const TAGS_ON: u8 = 1;
const TAGS_RESOLUTION: u8 = 2;
const TAGS_RANGE: u8 = 4;

/// True when poster cards show quality tags.
pub fn quality_tags() -> bool {
    QUALITY_TAGS.load(Ordering::Relaxed) & TAGS_ON != 0
}

/// Sets if poster cards show quality tags, and which of the tags.
pub fn set_quality_tags(on: bool, resolution: bool, range: bool) {
    let bits = if on { TAGS_ON } else { 0 }
        | if resolution { TAGS_RESOLUTION } else { 0 }
        | if range { TAGS_RANGE } else { 0 };
    QUALITY_TAGS.store(bits, Ordering::Relaxed);
}

/// The item fields of a list query. The streams are large, so a query asks
/// for them only while the quality tags are on.
fn item_fields() -> String {
    if quality_tags() {
        format!("{ITEM_FIELDS},MediaStreams")
    } else {
        ITEM_FIELDS.to_string()
    }
}

/// A failed request. Its text is as before ("HTTP 530 at /Items"); the
/// verdict says, without the text, whether the server is there
/// (`connection::classify`).
#[derive(Debug)]
pub struct RequestError {
    pub verdict: crate::connection::Verdict,
    text: String,
    /// The error of the HTTP client, kept as the source.
    source: Option<ureq::Error>,
}

impl RequestError {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(verdict: crate::connection::Verdict, text: String) -> Self {
        Self { verdict, text, source: None }
    }

    fn from_ureq(verdict: crate::connection::Verdict, text: String, source: ureq::Error) -> Self {
        Self { verdict, text, source: Some(source) }
    }
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl std::error::Error for RequestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|e| e as &(dyn std::error::Error + 'static))
    }
}

fn map_err(err: ureq::Error, path: &str) -> anyhow::Error {
    let verdict = crate::connection::classify_ureq(&err);
    let text = match &err {
        ureq::Error::StatusCode(401) => format!("unauthorized (401) at {path}"),
        ureq::Error::StatusCode(code) => format!("HTTP {code} at {path}"),
        other => format!("{other} ({path})"),
    };
    RequestError::from_ureq(verdict, text, err).into()
}

/// An error while the body was read: a connection that broke or timed out
/// is the network, a body that is not JSON is an answer of the server. The
/// text keeps the request ("decode /Items: ...") as before.
fn map_body_err(err: ureq::Error, what: &str, path: &str) -> anyhow::Error {
    let verdict = crate::connection::classify_ureq(&err);
    RequestError::from_ureq(verdict, format!("{what} {path}: {err}"), err).into()
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b',' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Name of this machine for the server's list of devices. It is asked for
/// once: every request sends it, and the answer takes a process to get.
/// Characters that are not plain ASCII are replaced, because a header value
/// with them is refused.
/// Scheme, host and port of a URL, lower case, with the default port filled
/// in. `None` for anything that is not an `http(s)` URL with a host.
fn origin_of(url: &str) -> Option<(String, String, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default = match scheme.as_str() {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    // A name before `@` is login data, not the host.
    let authority = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !port.contains(']') => (host, port.parse().ok()?),
        _ => (authority, default),
    };
    (!host.is_empty()).then(|| (scheme, host.to_ascii_lowercase(), port))
}

type ImageAuth = Vec<((String, String, u16), String)>;

static IMAGE_AUTH: std::sync::Mutex<ImageAuth> = std::sync::Mutex::new(Vec::new());
/// Counts the sign-ins; the image loader retries failed images after one.
static SESSIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn register_image_auth(base: &str, header: String) {
    let Some(origin) = origin_of(base) else { return };
    let mut all = IMAGE_AUTH.lock().unwrap();
    all.retain(|(known, _)| *known != origin);
    all.push((origin, header));
    SESSIONS.fetch_add(1, Ordering::Relaxed);
}

/// Number of sessions opened so far. A change means the app connected again.
pub fn session_count() -> u64 {
    SESSIONS.load(Ordering::Relaxed)
}

/// The server answers again after an outage: images that failed may be
/// tried again at once.
pub fn reconnected() {
    SESSIONS.fetch_add(1, Ordering::Relaxed);
}

/// True when two URLs have the same scheme, host and port; false when one
/// of them is no `http(s)` URL. The session header goes with a request only
/// when its URL has the origin of the server.
pub(crate) fn same_origin(a: &str, b: &str) -> bool {
    match (origin_of(a), origin_of(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

fn header_for<'a>(all: &'a ImageAuth, url: &str) -> Option<&'a str> {
    let origin = origin_of(url)?;
    all.iter().find(|(known, _)| *known == origin).map(|(_, header)| header.as_str())
}

/// The `Authorization` header for an image URL of the signed-in server, and
/// `None` for any other origin, so the token never goes to another host.
pub fn image_auth_header(url: &str) -> Option<String> {
    header_for(&IMAGE_AUTH.lock().unwrap(), url).map(str::to_string)
}

fn hostname() -> &'static str {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        std::env::var("HOSTNAME")
            .ok()
            .or_else(|| {
                std::process::Command::new("hostname")
                    .output()
                    .ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            })
            .map(|name| {
                name.chars()
                    .map(|c| if c.is_ascii_graphic() && c != '"' && c != ',' { c } else { '-' })
                    .collect::<String>()
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Mac".to_string())
    })
}

// ----- Types -----------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct ItemQuery {
    pub parent_id: Option<String>,
    pub include_types: Vec<String>,
    pub recursive: Option<bool>,
    pub sort_by: Option<String>,
    pub sort_order: Option<String>,
    pub search: Option<String>,
    pub filters: Option<String>,
    pub name_starts_with: Option<String>,
    pub name_less_than: Option<String>,
    pub person_id: Option<String>,
    pub limit: Option<usize>,
    pub start: Option<usize>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PublicSystemInfo {
    pub id: String,
    pub server_name: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct User {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub primary_image_tag: Option<String>,
    #[serde(default)]
    pub has_password: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct AuthResult {
    pub user: User,
    pub access_token: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ItemsResult {
    #[serde(default)]
    pub items: Vec<Item>,
    #[serde(default)]
    pub total_record_count: usize,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct Item {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "Type", default)]
    pub kind: String,
    #[serde(default)]
    pub collection_type: Option<String>,
    #[serde(default)]
    pub overview: Option<String>,
    #[serde(default)]
    pub production_year: Option<i32>,
    #[serde(default)]
    pub run_time_ticks: Option<i64>,
    #[serde(default)]
    pub community_rating: Option<f32>,
    #[serde(default)]
    pub critic_rating: Option<f32>,
    /// Streams of the file; a list query has them only while the quality
    /// tags are on.
    #[serde(default)]
    pub media_streams: Vec<MediaStream>,
    #[serde(default)]
    pub child_count: Option<i32>,
    #[serde(default)]
    pub official_rating: Option<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub original_title: Option<String>,
    #[serde(default)]
    pub series_name: Option<String>,
    #[serde(default)]
    pub series_id: Option<String>,
    #[serde(default)]
    pub season_id: Option<String>,
    #[serde(default)]
    pub index_number: Option<i32>,
    #[serde(default)]
    pub parent_index_number: Option<i32>,
    #[serde(default)]
    pub image_tags: ImageTags,
    #[serde(default)]
    pub backdrop_image_tags: Vec<String>,
    #[serde(default)]
    pub parent_backdrop_item_id: Option<String>,
    #[serde(default)]
    pub parent_backdrop_image_tags: Vec<String>,
    #[serde(default)]
    pub series_primary_image_tag: Option<String>,
    #[serde(default)]
    pub series_thumb_image_tag: Option<String>,
    #[serde(default)]
    pub parent_thumb_item_id: Option<String>,
    #[serde(default)]
    pub parent_thumb_image_tag: Option<String>,
    #[serde(default)]
    pub season_name: Option<String>,
    #[serde(default)]
    pub taglines: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub people: Vec<Person>,
    #[serde(default)]
    pub studios: Vec<NamedRef>,
    #[serde(default)]
    pub external_urls: Vec<NamedRef>,
    #[serde(default)]
    pub media_sources: Vec<MediaSource>,
    #[serde(default)]
    pub remote_trailers: Vec<NamedRef>,
    /// Ids of the item at other sites: "Tmdb", "Tvdb", "Imdb".
    #[serde(default)]
    pub provider_ids: std::collections::HashMap<String, Option<String>>,
    /// "Continuing" or "Ended" for a series.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub end_date: Option<String>,
    #[serde(default)]
    pub user_data: UserData,
}

/// A marked range of an item, such as its intro.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct MediaSegment {
    /// "Intro", "Outro", "Recap", "Preview" or "Commercial".
    #[serde(rename = "Type", default)]
    pub kind: String,
    #[serde(default)]
    start_ticks: i64,
    #[serde(default)]
    end_ticks: i64,
}

impl MediaSegment {
    pub fn start_secs(&self) -> f64 {
        self.start_ticks as f64 / TICKS_PER_SECOND as f64
    }

    pub fn end_secs(&self) -> f64 {
        self.end_ticks as f64 / TICKS_PER_SECOND as f64
    }
}

/// A search result from Seerr (TMDB data).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeerrItem {
    pub id: i64,
    /// "movie" or "tv".
    #[serde(default)]
    pub media_type: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    pub poster_path: Option<String>,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    first_air_date: Option<String>,
    #[serde(default)]
    pub vote_average: Option<f32>,
    #[serde(default)]
    pub media_info: Option<SeerrMediaInfo>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeerrMediaInfo {
    /// 2 pending, 3 processing, 4 partly available, 5 available.
    #[serde(default)]
    pub status: Option<i32>,
    #[serde(default)]
    pub jellyfin_media_id: Option<String>,
}

impl SeerrItem {
    pub fn display_name(&self) -> String {
        self.title
            .clone()
            .or_else(|| self.name.clone())
            .unwrap_or_default()
    }

    pub fn year(&self) -> Option<String> {
        self.release_date
            .as_deref()
            .or(self.first_air_date.as_deref())
            .and_then(|d| d.get(..4))
            .map(str::to_string)
    }

    pub fn poster_url(&self) -> Option<String> {
        self.poster_path
            .as_deref()
            .map(|path| format!("https://image.tmdb.org/t/p/w400{path}"))
    }

    pub fn status(&self) -> Option<i32> {
        self.media_info.as_ref().and_then(|m| m.status)
    }

    /// Id of the matching library item, when the title is in the library.
    pub fn library_id(&self) -> Option<&str> {
        self.media_info
            .as_ref()
            .and_then(|m| m.jellyfin_media_id.as_deref())
            .filter(|id| !id.is_empty())
    }
}

/// A cast or crew member of an item.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct Person {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub role: Option<String>,
    /// "Actor", "Director", "Writer", "GuestStar", ...
    #[serde(rename = "Type", default)]
    pub kind: String,
    #[serde(default)]
    pub primary_image_tag: Option<String>,
}

/// A name with an optional link or id (studios, external links, trailers).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct NamedRef {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct MediaSource {
    #[serde(default)]
    pub media_streams: Vec<MediaStream>,
    /// File name and path; the quality tags read stub, 3D and IMAX hints here.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct MediaStream {
    /// "Video", "Audio" or "Subtitle".
    #[serde(rename = "Type", default)]
    pub kind: String,
    /// Place of the stream in the media source; a remote control names a
    /// track by it.
    #[serde(default)]
    pub index: i64,
    #[serde(default)]
    pub display_title: Option<String>,
    #[serde(default)]
    pub is_default: bool,
    /// Three-letter language code, such as "eng".
    #[serde(default)]
    pub language: Option<String>,
    /// "SDR", "HDR10", "HDR10Plus", "HLG", "DOVI" and its variants.
    #[serde(default)]
    pub video_range_type: Option<String>,
    // The fields below are what the quality tags of the plugin read.
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub codec_tag: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub channels: Option<i32>,
    #[serde(default)]
    pub channel_layout: Option<String>,
    #[serde(default)]
    pub width: Option<i32>,
    #[serde(default)]
    pub height: Option<i32>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct ImageTags {
    #[serde(default)]
    pub primary: Option<String>,
    #[serde(default)]
    pub thumb: Option<String>,
    #[serde(default)]
    pub logo: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub struct UserData {
    #[serde(default)]
    pub playback_position_ticks: i64,
    #[serde(default)]
    pub played: bool,
    #[serde(default)]
    pub is_favorite: bool,
    #[serde(default)]
    pub unplayed_item_count: Option<i32>,
    #[serde(default)]
    pub played_percentage: Option<f32>,
}

impl Item {
    pub fn is_playable(&self) -> bool {
        matches!(
            self.kind.as_str(),
            "Movie" | "Episode" | "Video" | "MusicVideo"
        )
    }

    pub fn is_series(&self) -> bool {
        self.kind == "Series"
    }

    pub fn runtime_secs(&self) -> Option<i64> {
        self.run_time_ticks.map(|t| t / TICKS_PER_SECOND)
    }

    pub fn resume_secs(&self) -> i64 {
        self.user_data.playback_position_ticks / TICKS_PER_SECOND
    }

    /// Progress in 0..=1 when the item has been partially watched.
    pub fn progress(&self) -> Option<f32> {
        let pos = self.user_data.playback_position_ticks;
        let total = self.run_time_ticks?;
        (pos > 0 && total > 0).then(|| (pos as f32 / total as f32).clamp(0., 1.))
    }

    /// Poster-style image: own primary, else the series poster for episodes.
    pub fn poster_url(&self, client: &Client, width: u32) -> Option<String> {
        if let Some(tag) = &self.image_tags.primary {
            return Some(client.image_url(&self.id, "Primary", Some(tag), width));
        }
        match (&self.series_id, &self.series_primary_image_tag) {
            (Some(series), Some(tag)) => {
                Some(client.image_url(series, "Primary", Some(tag), width))
            }
            _ => None,
        }
    }

    /// Wide image in the order the web client uses for home rows: the item's
    /// thumb, the series thumb, a backdrop, then the episode still.
    pub fn wide_url(&self, client: &Client, width: u32) -> Option<String> {
        if let Some(tag) = &self.image_tags.thumb {
            return Some(client.image_url(&self.id, "Thumb", Some(tag), width));
        }
        if let (Some(series), Some(tag)) = (&self.series_id, &self.series_thumb_image_tag) {
            return Some(client.image_url(series, "Thumb", Some(tag), width));
        }
        if let (Some(parent), Some(tag)) = (&self.parent_thumb_item_id, &self.parent_thumb_image_tag)
        {
            return Some(client.image_url(parent, "Thumb", Some(tag), width));
        }
        if let Some(tag) = self.backdrop_image_tags.first() {
            return Some(client.image_url(&self.id, "Backdrop", Some(tag), width));
        }
        if let (Some(parent), Some(tag)) = (
            &self.parent_backdrop_item_id,
            self.parent_backdrop_image_tags.first(),
        ) {
            return Some(client.image_url(parent, "Backdrop", Some(tag), width));
        }
        if self.kind == "Episode"
            && let Some(tag) = &self.image_tags.primary
        {
            return Some(client.image_url(&self.id, "Primary", Some(tag), width));
        }
        None
    }

    /// The year line of a card: "2014", or "2011 - Present" for a series.
    pub fn year_label(&self) -> Option<String> {
        let start = self.production_year?;
        if !self.is_series() {
            return Some(start.to_string());
        }
        if self.status.as_deref() == Some("Continuing") {
            return Some(format!("{start} - Present"));
        }
        let end = self
            .end_date
            .as_deref()
            .and_then(|d| d.get(..4))
            .and_then(|y| y.parse::<i32>().ok());
        match end {
            Some(end) if end != start => Some(format!("{start} - {end}")),
            _ => Some(start.to_string()),
        }
    }

    /// Quality tags of the file, as Jellyfin Enhanced shows them on a poster
    /// (resolution, codec, range, audio, ...); the rules are in `quality`.
    pub fn quality_labels(&self) -> Vec<String> {
        crate::quality::labels(self)
    }

    /// The streams of one kind ("Video", "Audio", "Subtitle") in file order.
    pub fn streams(&self, kind: &str) -> Vec<&MediaStream> {
        self.media_sources
            .first()
            .map_or(&self.media_streams, |source| &source.media_streams)
            .iter()
            .filter(|s| s.kind == kind)
            .collect()
    }

    /// Display title of the first stream of a kind; the default one wins.
    pub fn stream_title(&self, kind: &str) -> Option<String> {
        let streams = &self.media_sources.first()?.media_streams;
        streams
            .iter()
            .filter(|s| s.kind == kind)
            .max_by_key(|s| s.is_default)
            .and_then(|s| s.display_title.clone())
    }

    /// Names of the people of one kind, in credit order, without repeats.
    pub fn people_of(&self, kind: &str) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for person in self.people.iter().filter(|p| p.kind == kind) {
            if !names.contains(&person.name) {
                names.push(person.name.clone());
            }
        }
        names
    }

    pub fn logo_url(&self, client: &Client, width: u32) -> Option<String> {
        let tag = self.image_tags.logo.as_deref()?;
        Some(client.image_url(&self.id, "Logo", Some(tag), width))
    }

    pub fn backdrop_url(&self, client: &Client, width: u32) -> Option<String> {
        if let Some(tag) = self.backdrop_image_tags.first() {
            return Some(client.image_url(&self.id, "Backdrop", Some(tag), width));
        }
        match (
            &self.parent_backdrop_item_id,
            self.parent_backdrop_image_tags.first(),
        ) {
            (Some(parent), Some(tag)) => {
                Some(client.image_url(parent, "Backdrop", Some(tag), width))
            }
            _ => None,
        }
    }

    /// "S2:E5" style label for episodes.
    pub fn episode_code(&self) -> Option<String> {
        match (self.parent_index_number, self.index_number) {
            (Some(s), Some(e)) => Some(format!("S{s}:E{e}")),
            (None, Some(e)) => Some(format!("E{e}")),
            _ => None,
        }
    }

    /// Title used for the player window.
    pub fn display_title(&self) -> String {
        match (&self.series_name, self.episode_code()) {
            (Some(series), Some(code)) => format!("{series} · {code} · {}", self.name),
            (Some(series), None) => format!("{series} · {}", self.name),
            _ => match self.production_year {
                Some(year) if self.kind == "Movie" => format!("{} ({year})", self.name),
                _ => self.name.clone(),
            },
        }
    }
}

/// Playback state reported back to the server.
#[derive(Clone, Debug)]
pub struct Progress {
    pub item_id: String,
    pub play_session_id: String,
    pub position_ticks: i64,
    pub paused: bool,
    /// Volume from 0 to 100 and mute, for a device that controls this one.
    pub volume: i64,
    pub muted: bool,
    /// "DirectPlay", "DirectStream" or "Transcode".
    pub play_method: String,
    pub media_source_id: String,
}

impl Progress {
    fn body(&self, with_state: bool) -> serde_json::Value {
        let mut body = serde_json::json!({
            "ItemId": self.item_id,
            "MediaSourceId": if self.media_source_id.is_empty() { &self.item_id } else { &self.media_source_id },
            "PlaySessionId": self.play_session_id,
            "PositionTicks": self.position_ticks,
            "PlayMethod": if self.play_method.is_empty() { "DirectPlay" } else { self.play_method.as_str() },
            "CanSeek": true,
        });
        if with_state {
            body["IsPaused"] = serde_json::Value::Bool(self.paused);
            body["VolumeLevel"] = serde_json::Value::from(self.volume);
            body["IsMuted"] = serde_json::Value::Bool(self.muted);
        }
        body
    }
}

pub fn format_duration(secs: i64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

pub fn format_runtime(secs: i64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server that answers every request with `head` and `body`, then
    /// closes the connection. `body` may be shorter than the head says. It
    /// ends with the test.
    struct Sender {
        port: u16,
        stop: Arc<std::sync::atomic::AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Sender {
        fn start(head: &'static str, body: &'static str) -> Self {
            use std::io::{Read, Write};
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stopping = stop.clone();
            let thread = std::thread::spawn(move || {
                for mut stream in listener.incoming().flatten() {
                    if stopping.load(Ordering::SeqCst) {
                        return;
                    }
                    let mut buffer = [0u8; 4096];
                    let _ = stream.read(&mut buffer);
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(body.as_bytes());
                    let _ = stream.flush();
                    // Closed with the rest of the body never sent.
                }
            });
            Self { port, stop, thread: Some(thread) }
        }

        fn client(&self) -> Client {
            Client::new(&format!("http://127.0.0.1:{}", self.port), "d")
        }
    }

    impl Drop for Sender {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// Finding 9 of the review: a connection that breaks while the body
    /// comes is a lost connection, not an answer of the server.
    #[test]
    fn a_body_cut_short_is_a_lost_connection_not_an_answer() {
        use crate::connection::{Verdict, Why, classify};
        let server = Sender::start(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n",
            "{\"Items\": [{\"Id\": \"a\"",
        );
        let err = server.client().get::<serde_json::Value>("/Items", &[]).unwrap_err();
        assert_eq!(classify(&err), Verdict::Unreachable(Why::Connect), "{err:#}");
        // The text still names the request, and the error of the HTTP
        // client is the source.
        assert!(format!("{err:#}").starts_with("decode /Items: "), "{err:#}");
        let request = err.downcast_ref::<RequestError>().expect("a typed error");
        assert!(std::error::Error::source(request).is_some_and(|s| s.downcast_ref::<ureq::Error>().is_some()));
        // The same for a text body.
        let err = server.client().get_text("/System/Logs/Log", &[]).unwrap_err();
        assert_eq!(classify(&err), Verdict::Unreachable(Why::Connect), "{err:#}");
    }

    /// A whole body that is not JSON is an answer: the server is there.
    #[test]
    fn a_whole_body_that_is_not_json_is_an_answer() {
        use crate::connection::{Verdict, classify};
        let server = Sender::start(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 6\r\nConnection: close\r\n\r\n",
            "<html>",
        );
        let err = server.client().get::<serde_json::Value>("/Items", &[]).unwrap_err();
        assert_eq!(classify(&err), Verdict::Answered, "{err:#}");
        let err = server.client().send::<serde_json::Value>("POST", "/Items", &[]).unwrap_err();
        assert_eq!(classify(&err), Verdict::Answered, "{err:#}");
    }

    #[test]
    fn same_origin_compares_scheme_host_and_port() {
        assert!(same_origin("https://jf.example.com/a", "https://jf.example.com:443"));
        assert!(same_origin("HTTP://JF.example.com:80/x", "http://jf.example.com"));
        assert!(!same_origin("http://jf.example.com/a", "https://jf.example.com"));
        assert!(!same_origin("https://jf.example.com:8920/a", "https://jf.example.com"));
        assert!(!same_origin("https://cdn.example.com/a", "https://jf.example.com"));
        assert!(!same_origin("file:///a", "https://jf.example.com"));
        assert!(!same_origin("/a", "https://jf.example.com"));
    }

    #[test]
    fn image_header_goes_to_the_servers_origin_only() {
        let client = Client::new("https://media.example.com/", "dev-1").with_session("tok", "u");
        let all: ImageAuth = vec![(origin_of(&client.base).unwrap(), client.auth_header())];
        let sent = header_for(&all, "https://MEDIA.example.com:443/Items/1/Images/Primary?tag=x");
        assert_eq!(sent, Some(client.auth_header().as_str()));
        assert!(sent.unwrap().contains("Token=\"tok\""));
        for other in [
            "https://evil.example.com/a.jpg",
            "http://media.example.com/a.jpg",
            "https://media.example.com:8443/a.jpg",
            "https://media.example.com@evil.example.com/a.jpg",
            "https://media.example.com.evil.net/a.jpg",
            "file:///etc/passwd",
            "https://image.tmdb.org/t/p/w500/a.jpg",
        ] {
            assert_eq!(header_for(&all, other), None, "{other}");
        }
        // The real registry holds the same entry.
        assert!(image_auth_header("https://media.example.com/x").is_some());
        assert!(image_auth_header("https://other.example.com/x").is_none());
    }

    #[test]
    fn trickplay_url_has_no_token() {
        let client = Client::new("https://media.example.com", "dev-1").with_session("secret-tok", "u");
        let url = client.trickplay_url("abc", 320, 2);
        assert_eq!(url, "https://media.example.com/Videos/abc/Trickplay/320/2.jpg");
        assert!(!url.contains("secret-tok") && !url.contains("api_key"));
    }

    #[test]
    fn builds_urls_and_headers() {
        let client =
            Client::new("https://media.example.com/", "dev-1").with_session("tok", "user-1");
        assert_eq!(&*client.base, "https://media.example.com");
        assert_eq!(
            client.url(
                "/Items",
                &[
                    ("userId", "u 1".into()),
                    ("limit", String::new()),
                    ("sortBy", "SortName,Name".into())
                ]
            ),
            "https://media.example.com/Items?userId=u%201&sortBy=SortName,Name"
        );
        assert_eq!(
            client.mpv_auth_header(),
            "Authorization: MediaBrowser Token=\"tok\""
        );
        let full = client.auth_header();
        assert!(full.starts_with(&format!("MediaBrowser Client=\"{APP_NAME}\"")));
        assert!(full.contains("DeviceId=\"dev-1\"") && full.ends_with("Token=\"tok\""));
        assert_eq!(
            client.stream_url("abc", "abc"),
            "https://media.example.com/Videos/abc/stream?static=true&mediaSourceId=abc"
        );
        assert_eq!(
            client.stream_url("abc", "def"),
            "https://media.example.com/Videos/abc/stream?static=true&mediaSourceId=def"
        );
    }

    #[test]
    fn item_helpers() {
        let json = r#"{"Id":"e1","Name":"Pilot","Type":"Episode","SeriesName":"Show","SeriesId":"s1",
            "IndexNumber":1,"ParentIndexNumber":2,"RunTimeTicks":36000000000,
            "UserData":{"PlaybackPositionTicks":18000000000,"Played":false},
            "ImageTags":{},"SeriesPrimaryImageTag":"pt"}"#;
        let item: Item = serde_json::from_str(json).unwrap();
        let client = Client::new("http://h", "d");
        assert_eq!(item.episode_code().as_deref(), Some("S2:E1"));
        assert_eq!(item.display_title(), "Show · S2:E1 · Pilot");
        assert_eq!(item.runtime_secs(), Some(3600));
        assert_eq!(item.resume_secs(), 1800);
        assert_eq!(item.progress(), Some(0.5));
        // The width goes up to the next fixed step.
        assert_eq!(
            item.poster_url(&client, 300).unwrap(),
            "http://h/Items/s1/Images/Primary?maxWidth=320&quality=90&tag=pt"
        );
        assert_eq!(format_duration(3725), "1:02:05");
        assert_eq!(format_runtime(5400), "1h 30m");
    }
}

// ----- Player timeline: chapters, preview images, what plays next ------------

/// A named point in a video.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Chapter {
    #[serde(default)]
    pub start_position_ticks: i64,
    #[serde(default)]
    pub name: String,
}

impl Chapter {
    pub fn start_secs(&self) -> f64 {
        self.start_position_ticks as f64 / TICKS_PER_SECOND as f64
    }
}

/// The preview images of a video ("trickplay"): sheets of small frames, one
/// frame for each `interval` of the video.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Trickplay {
    /// Size of one frame.
    pub width: u32,
    pub height: u32,
    /// Frames in one row and in one column of a sheet.
    pub tile_width: u32,
    pub tile_height: u32,
    #[serde(default)]
    pub thumbnail_count: u32,
    /// Milliseconds between two frames.
    pub interval: u32,
}

/// What the player timeline shows for one item.
#[derive(Clone, Debug, Default)]
pub struct TimelineInfo {
    pub chapters: Vec<Chapter>,
    pub trickplay: Option<Trickplay>,
    /// The audio track for the user, as its place among the audio tracks of
    /// the file, from 1 (the way mpv counts them): a track in the audio
    /// language of the user, else the track the server names.
    pub default_audio: Option<i64>,
    /// The subtitle the server chose for the user, counted the same way:
    /// `Some(None)` is no subtitle, `None` is unknown.
    pub default_subtitle: Option<Option<i64>>,
}

impl Client {
    /// Chapters and preview images of an item. The lists of items leave
    /// these out, because they are large.
    pub fn timeline_info(&self, item_id: &str) -> Result<TimelineInfo> {
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Full {
            #[serde(default)]
            chapters: Vec<Chapter>,
            /// Media source id, then frame width, then the sheet layout.
            #[serde(default)]
            trickplay: std::collections::HashMap<
                String,
                std::collections::HashMap<String, serde_json::Value>,
            >,
            #[serde(default)]
            media_sources: Vec<Source>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Source {
            #[serde(default)]
            default_audio_stream_index: Option<i64>,
            #[serde(default)]
            default_subtitle_stream_index: Option<i64>,
            #[serde(default)]
            media_streams: Vec<Stream>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Stream {
            #[serde(rename = "Type", default)]
            kind: String,
            #[serde(default)]
            index: i64,
            #[serde(default)]
            is_external: bool,
            #[serde(default)]
            language: Option<String>,
        }
        #[derive(Default, Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Me {
            #[serde(default)]
            configuration: Audio,
        }
        #[derive(Default, Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Audio {
            #[serde(default)]
            audio_language_preference: Option<String>,
            #[serde(default)]
            play_default_audio_track: bool,
        }
        let user = self.user()?.to_string();
        let full: Full = self.get(&format!("/Items/{item_id}"), &[("userId", user)])?;
        let widths = full
            .trickplay
            .get(item_id)
            .or_else(|| full.trickplay.values().next());
        // The smallest frames are enough for a preview over the timeline.
        let trickplay = widths.and_then(|widths| {
            widths
                .values()
                .filter_map(|v| serde_json::from_value::<Trickplay>(v.clone()).ok())
                .filter(|t| t.width > 0 && t.height > 0 && t.interval > 0)
                .filter(|t| t.tile_width > 0 && t.tile_height > 0)
                .min_by_key(|t| t.width)
        });
        // The audio settings of the user, read now so a change made a
        // moment ago in another client counts.
        let audio = self
            .get::<Me>("/Users/Me", &[])
            .map(|me| me.configuration)
            .unwrap_or_default();
        let language = audio
            .audio_language_preference
            .filter(|language| !language.is_empty() && !audio.play_default_audio_track);
        let default_audio = full.media_sources.first().and_then(|source| {
            let tracks: Vec<&Stream> = source
                .media_streams
                .iter()
                .filter(|stream| stream.kind == "Audio" && !stream.is_external)
                .collect();
            // A track in the language of the user comes first. The server
            // can name another one: it remembers the track of an earlier
            // playback, also when that was the default track of the file.
            let preferred = language
                .as_deref()
                .and_then(|language| tracks.iter().position(|t| t.language.as_deref() == Some(language)));
            let chosen = source
                .default_audio_stream_index
                .and_then(|wanted| tracks.iter().position(|t| t.index == wanted));
            preferred.or(chosen).map(|place| place as i64 + 1)
        });
        let default_subtitle = full.media_sources.first().and_then(|source| {
            let Some(wanted) = source.default_subtitle_stream_index.filter(|i| *i >= 0) else {
                return Some(None);
            };
            // A subtitle in a file of its own is not in the stream mpv plays.
            source
                .media_streams
                .iter()
                .filter(|stream| stream.kind == "Subtitle" && !stream.is_external)
                .position(|stream| stream.index == wanted)
                .map(|place| Some(place as i64 + 1))
        });
        Ok(TimelineInfo {
            chapters: full.chapters,
            trickplay,
            default_audio,
            default_subtitle,
        })
    }

    /// Address of one sheet of preview images. The server wants the session
    /// for these; the image loader sends the header (see [`image_auth_header`]),
    /// so the token is not in the URL.
    pub fn trickplay_url(&self, item_id: &str, width: u32, sheet: u32) -> String {
        self.url(&format!("/Videos/{item_id}/Trickplay/{width}/{sheet}.jpg"), &[])
    }

    /// The episodes that follow an episode in its series, in order.
    pub fn episodes_after(&self, series_id: &str, episode_id: &str) -> Result<Vec<Item>> {
        let user = self.user()?.to_string();
        let result: ItemsResult = self.get(
            &format!("/Shows/{series_id}/Episodes"),
            &[
                ("userId", user),
                ("startItemId", episode_id.to_string()),
                ("limit", "50".to_string()),
                ("fields", ITEM_FIELDS.to_string()),
            ],
        )?;
        Ok(result
            .items
            .into_iter()
            .filter(|item| item.id != episode_id)
            .collect())
    }

    /// The episodes a series stands for in a play queue: the next unwatched
    /// one and those after it, or the first ones when all are watched.
    pub fn series_queue(&self, series_id: &str) -> Result<Vec<Item>> {
        if let Some(next) = self.series_next_up(series_id)? {
            let mut episodes = self.episodes_after(series_id, &next.id)?;
            episodes.insert(0, next);
            return Ok(episodes);
        }
        let user = self.user()?.to_string();
        let result: ItemsResult = self.get(
            &format!("/Shows/{series_id}/Episodes"),
            &[
                ("userId", user),
                ("limit", "50".to_string()),
                ("fields", ITEM_FIELDS.to_string()),
            ],
        )?;
        Ok(result.items)
    }
}

/// `BLOOM_NO_REPORT` keeps playback from the server's records, so a test
/// does not move a resume point or mark an item as played.
pub fn no_report() -> bool {
    std::env::var_os("BLOOM_NO_REPORT").is_some()
}

/// With the reports off, the log still shows what would go to the server,
/// so a test can check their order.
fn log_unsent(kind: &str, p: &Progress) {
    log::debug!(
        "report {kind} (not sent): item {} session {} at {} s",
        p.item_id,
        &p.play_session_id[..p.play_session_id.len().min(8)],
        p.position_ticks / TICKS_PER_SECOND
    );
}
