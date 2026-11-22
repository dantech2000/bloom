//! The user's settings on the server, as the Mac keeps them (src/settings):
//! two documents, the user's `Configuration` (languages, subtitle mode,
//! remember tracks) and the display preferences `usersettings` (home
//! sections and the web client's keys in `CustomPrefs`). Swift edits the
//! JSON; a save sends the server's document now with the user's changes
//! in it (`draft::with_changes`), so what another client set stays.

use bloom_core::draft;
use serde_json::Value;

use crate::{BloomError, Server, SyncPlayAccess, flag, parse_json as parse, run};

#[derive(uniffi::Enum, Clone, Copy)]
pub enum PrefsDoc {
    Configuration,
    Display,
}

/// The two documents as JSON, and facts of the user. (No `HasPassword`:
/// Jellyfin 12 sends it as true for every user.)
#[derive(uniffi::Record)]
pub struct UserPrefs {
    pub configuration_json: String,
    pub display_json: String,
    /// The user may run the server's dashboard.
    pub is_admin: bool,
    /// The user may make and edit collections.
    pub can_collect: bool,
    /// The user may download items to this device.
    pub can_download: bool,
    pub syncplay_access: SyncPlayAccess,
    /// The user's picture, when they have one.
    pub picture: Option<String>,
    /// The user may search for and download subtitles.
    pub can_manage_subtitles: bool,
}

/// A language the server knows, for the language choices.
#[derive(uniffi::Record)]
pub struct Language {
    pub name: String,
    /// ISO 639-2 (three letters), as the server stores a preference.
    pub code: String,
}

#[uniffi::export]
impl Server {
    pub async fn prefs(&self) -> Result<UserPrefs, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let user = client.user()?.to_string();
            let me = client.get::<Value>("/Users/Me", &[])?;
            // The app works without the display document; the pages then
            // show the defaults.
            let display = client
                .get::<Value>("/DisplayPreferences/usersettings", &[("userId", user.clone()), ("client", "emby".into())])
                .unwrap_or_else(|_| serde_json::json!({ "Id": "usersettings", "CustomPrefs": {}, "Client": "emby" }));
            Ok(UserPrefs {
                configuration_json: me.get("Configuration").cloned().unwrap_or_default().to_string(),
                display_json: display.to_string(),
                is_admin: flag(&me["Policy"], "IsAdministrator"),
                can_collect: flag(&me["Policy"], "EnableCollectionManagement")
                    || flag(&me["Policy"], "IsAdministrator"),
                can_download: me["Policy"]["EnableContentDownloading"].as_bool().unwrap_or(true),
                syncplay_access: SyncPlayAccess::parse(me["Policy"]["SyncPlayAccess"].as_str().unwrap_or("CreateAndJoinGroups")),
                can_manage_subtitles: flag(&me["Policy"], "EnableSubtitleManagement")
                    || flag(&me["Policy"], "IsAdministrator"),
                picture: me["PrimaryImageTag"].as_str().map(|tag| {
                    format!("{}/Users/{user}/Images/Primary?maxWidth=240&tag={tag}", client.base.trim_end_matches('/'))
                }),
            })
        })
        .await
    }

    /// Saves `edited`, read as `original`: the server's document now, with
    /// the places that differ taken from `edited`. Answers what the server
    /// has after the save.
    pub async fn save_prefs(&self, doc: PrefsDoc, original_json: String, edited_json: String) -> Result<String, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let (original, edited) = (parse(&original_json)?, parse(&edited_json)?);
            let user = client.user()?.to_string();
            let fresh = match doc {
                PrefsDoc::Configuration => client
                    .get::<Value>("/Users/Me", &[])?
                    .get("Configuration")
                    .cloned()
                    .ok_or_else(|| BloomError::Answered { detail: "the user has no configuration".into() })?,
                PrefsDoc::Display => client
                    .get::<Value>("/DisplayPreferences/usersettings", &[("userId", user.clone()), ("client", "emby".into())])?,
            };
            let merged = draft::with_changes(&fresh, &original, &edited);
            match doc {
                PrefsDoc::Configuration => client.post(&format!("/Users/Configuration?userId={user}"), &merged),
                PrefsDoc::Display => {
                    client.post(&format!("/DisplayPreferences/usersettings?userId={user}&client=emby"), &merged)
                }
            }?;
            Ok(merged.to_string())
        })
        .await
    }

    /// The error never holds the passwords: `post`'s text names the path.
    pub async fn change_password(&self, current: String, new: String) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let user = client.user()?.to_string();
            client
                .post(&format!("/Users/Password?userId={user}"), &serde_json::json!({ "CurrentPw": current, "NewPw": new }))
                .map(drop)
                .map_err(Into::into)
        })
        .await
    }

    /// The languages of the server's list, by name.
    pub async fn languages(&self) -> Result<Vec<Language>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let list: Vec<Value> = client.localization("Cultures")?;
            let mut languages: Vec<Language> = list
                .iter()
                .filter_map(|culture| {
                    let code = culture["ThreeLetterISOLanguageName"].as_str()?.to_string();
                    let name = culture["DisplayName"].as_str()?.to_string();
                    (!code.is_empty()).then_some(Language { name, code })
                })
                .collect();
            languages.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(first_of_each_code(languages))
        })
        .await
    }
}

/// One language per code, the first of the list (by name) for each: a code
/// the server lists twice under two names that sort apart is not kept twice.
fn first_of_each_code(languages: Vec<Language>) -> Vec<Language> {
    let mut seen = std::collections::HashSet::new();
    languages.into_iter().filter(|language| seen.insert(language.code.clone())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_listed_twice_stays_once() {
        let language = |name: &str, code: &str| Language { name: name.into(), code: code.into() };
        // "German" and "Swiss German" share "ger" and sort apart.
        let list = vec![language("English", "eng"), language("German", "ger"), language("Spanish", "spa"), language("Swiss German", "ger")];
        let codes: Vec<_> = first_of_each_code(list).into_iter().map(|l| (l.name, l.code)).collect();
        assert_eq!(codes, [("English".into(), "eng".into()), ("German".into(), "ger".into()), ("Spanish".into(), "spa".into())]);
    }
}
