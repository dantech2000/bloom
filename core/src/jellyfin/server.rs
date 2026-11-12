// SPDX-License-Identifier: AGPL-3.0-or-later
//! The requests of the server that are not about a page of the library:
//! its dashboard (system, tasks, users, libraries, plugins, keys, devices,
//! logs), the sessions a client may control, the metadata of an item, and
//! SyncPlay. Each is one endpoint under its name, so a caller names what it
//! wants and no path is written twice.
//!
//! A list comes back as the type the caller names (`T`): each page reads
//! the fields it shows. An object an editor sends back whole is a
//! `serde_json::Value`, so no field the app does not know is lost.

use anyhow::Result;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

use super::{Client, Method};

impl Client {
    // ----- The system -------------------------------------------------------

    pub fn system_info<T: DeserializeOwned>(&self) -> Result<T> {
        self.get("/System/Info", &[])
    }

    /// How many movies, series and episodes the libraries hold.
    pub fn item_counts<T: DeserializeOwned>(&self) -> Result<T> {
        self.get("/Items/Counts", &[])
    }

    /// The folders of the server and the room on their disks.
    pub fn storage_info<T: DeserializeOwned>(&self) -> Result<T> {
        self.get("/System/Info/Storage", &[])
    }

    pub fn restart_server(&self) -> Result<()> {
        self.call(Method::Post, "/System/Restart", &[])
    }

    pub fn shutdown_server(&self) -> Result<()> {
        self.call(Method::Post, "/System/Shutdown", &[])
    }

    /// The times of the server around this request, for the clock of SyncPlay.
    pub fn utc_time<T: DeserializeOwned>(&self) -> Result<T> {
        self.get("/GetUtcTime", &[])
    }

    /// A configuration object of the server: the main one for an empty
    /// name, else the named one ("branding", "encoding", "metadata").
    pub fn configuration(&self, name: &str) -> Result<Value> {
        self.get(&configuration_path(name), &[])
    }

    /// Sends a configuration object back whole.
    pub fn save_configuration(&self, name: &str, body: &Value) -> Result<()> {
        self.post(&configuration_path(name), body).map(drop)
    }

    /// A list of the server for a choice in a form: "Options" (languages of
    /// the interface), "Cultures", "Countries".
    pub fn localization<T: DeserializeOwned>(&self, list: &str) -> Result<T> {
        self.get(&format!("/Localization/{list}"), &[])
    }

    /// The newest entries of the activity log: `Limit`, `HasUserId`.
    pub fn activity_log<T: DeserializeOwned>(&self, query: &[(&str, String)]) -> Result<T> {
        self.get("/System/ActivityLog/Entries", query)
    }

    pub fn log_files<T: DeserializeOwned>(&self) -> Result<T> {
        self.get("/System/Logs", &[])
    }

    /// The end of one log file: at most its last `max` bytes, from the
    /// start of a line, and whether the file is longer. A log of a busy
    /// day is tens of megabytes, and a viewer shows its end.
    pub fn log_tail(&self, name: &str, max: usize) -> Result<(String, bool)> {
        self.get_text_tail("/System/Logs/Log", &[("name", name.to_string())], max)
    }

    // ----- Scheduled tasks ---------------------------------------------------

    /// The tasks a dashboard shows (the hidden ones are left out).
    pub fn scheduled_tasks<T: DeserializeOwned>(&self) -> Result<T> {
        self.get("/ScheduledTasks", &[("IsHidden", "false".to_string())])
    }

    pub fn scheduled_task(&self, id: &str) -> Result<Value> {
        self.get(&format!("/ScheduledTasks/{id}"), &[])
    }

    /// Replaces the triggers of a task with this list.
    pub fn set_task_triggers<B: Serialize>(&self, id: &str, triggers: &B) -> Result<()> {
        self.post(&format!("/ScheduledTasks/{id}/Triggers"), triggers).map(drop)
    }

    pub fn start_task(&self, id: &str) -> Result<()> {
        self.call(Method::Post, &format!("/ScheduledTasks/Running/{id}"), &[])
    }

    pub fn stop_task(&self, id: &str) -> Result<()> {
        self.call(Method::Delete, &format!("/ScheduledTasks/Running/{id}"), &[])
    }

    // ----- Users --------------------------------------------------------------

    pub fn users<T: DeserializeOwned>(&self) -> Result<T> {
        self.get("/Users", &[])
    }

    /// A user as the server has it, policy and configuration with it.
    pub fn user_json(&self, id: &str) -> Result<Value> {
        self.get(&format!("/Users/{id}"), &[])
    }

    /// Sends the policy of a user back whole.
    pub fn set_user_policy(&self, id: &str, policy: &Value) -> Result<()> {
        self.post(&format!("/Users/{id}/Policy"), policy).map(drop)
    }

    /// A new user: `Name` and `Password`.
    pub fn create_user(&self, body: &Value) -> Result<()> {
        self.post("/Users/New", body).map(drop)
    }

    /// The password of a user: `NewPw`, or `ResetPassword`.
    pub fn set_user_password(&self, id: &str, body: &Value) -> Result<()> {
        // The id is a hex number, so it needs no escape.
        self.post(&format!("/Users/Password?userId={id}"), body).map(drop)
    }

    pub fn delete_user(&self, id: &str) -> Result<()> {
        self.call(Method::Delete, &format!("/Users/{id}"), &[])
    }

    // ----- Libraries ----------------------------------------------------------

    /// The libraries with their folders and options.
    pub fn virtual_folders<T: DeserializeOwned>(&self) -> Result<T> {
        self.get("/Library/VirtualFolders", &[])
    }

    /// The folders a policy can give a user (`Items` with `Id` and `Name`).
    pub fn media_folders(&self) -> Result<Value> {
        self.get("/Library/MediaFolders", &[])
    }

    /// Starts a scan of every library.
    pub fn scan_libraries(&self) -> Result<()> {
        self.call(Method::Post, "/Library/Refresh", &[])
    }

    /// Asks the server to read an item again: a scan of a library, or the
    /// metadata of a title. The query names the modes (`Recursive`,
    /// `MetadataRefreshMode`, `ImageRefreshMode`, `ReplaceAllMetadata`...).
    pub fn refresh_item(&self, id: &str, query: &[(&str, String)]) -> Result<()> {
        self.call(Method::Post, &format!("/Items/{id}/Refresh"), query)
    }

    // ----- Plugins ------------------------------------------------------------

    pub fn plugins<T: DeserializeOwned>(&self) -> Result<T> {
        self.get("/Plugins", &[])
    }

    pub fn plugin_configuration(&self, id: &str) -> Result<Value> {
        self.get(&plugin_configuration_path(id), &[])
    }

    /// The configuration as the server wrote it: the order of its keys is
    /// the order of the plugin's own page.
    pub fn plugin_configuration_text(&self, id: &str) -> Result<String> {
        self.get_text(&plugin_configuration_path(id), &[])
    }

    /// Sends the configuration of a plugin back whole.
    pub fn save_plugin_configuration(&self, id: &str, body: &Value) -> Result<()> {
        self.post(&plugin_configuration_path(id), body).map(drop)
    }

    /// Turns one version of a plugin on or off.
    pub fn set_plugin_enabled(&self, id: &str, version: &str, enabled: bool) -> Result<()> {
        let verb = if enabled { "Enable" } else { "Disable" };
        self.call(Method::Post, &format!("/Plugins/{id}/{version}/{verb}"), &[])
    }

    pub fn uninstall_plugin(&self, id: &str, version: &str) -> Result<()> {
        self.call(Method::Delete, &format!("/Plugins/{id}/{version}"), &[])
    }

    // ----- API keys -----------------------------------------------------------

    pub fn api_keys<T: DeserializeOwned>(&self) -> Result<T> {
        self.get("/Auth/Keys", &[])
    }

    pub fn create_api_key(&self, app: &str) -> Result<()> {
        self.call(Method::Post, "/Auth/Keys", &[("app", app.to_string())])
    }

    /// Revokes a key. The error names no path: the path holds the key.
    pub fn revoke_api_key(&self, key: &str) -> Result<()> {
        self.call(Method::Delete, &format!("/Auth/Keys/{key}"), &[])
            .map_err(|_| anyhow::anyhow!("the key was not revoked"))
    }

    // ----- Devices and sessions -------------------------------------------------

    pub fn devices<T: DeserializeOwned>(&self) -> Result<T> {
        self.get("/Devices", &[])
    }

    pub fn delete_device(&self, id: &str) -> Result<()> {
        self.call(Method::Delete, "/Devices", &[("id", id.to_string())])
    }

    /// The sessions of the server, narrowed by the query: `deviceId`,
    /// `controllableByUserId`, `ActiveWithinSeconds`.
    pub fn sessions<T: DeserializeOwned>(&self, query: &[(&str, String)]) -> Result<T> {
        self.get("/Sessions", query)
    }

    /// Tells the server what this client can play and be told to do.
    pub fn post_capabilities(&self, body: &Value) -> Result<()> {
        self.post("/Sessions/Capabilities/Full", body).map(drop)
    }

    /// Tells another session to play items (`itemIds`, `playCommand`,
    /// `startPositionTicks`).
    pub fn session_play(&self, session_id: &str, query: &[(&str, String)]) -> Result<()> {
        self.call(Method::Post, &format!("/Sessions/{session_id}/Playing"), query)
    }

    /// A playstate command for another session: "Pause", "Unpause",
    /// "Stop", "Seek" (with `seekPositionTicks`)...
    pub fn session_playstate(&self, session_id: &str, command: &str, query: &[(&str, String)]) -> Result<()> {
        self.call(Method::Post, &format!("/Sessions/{session_id}/Playing/{command}"), query)
    }

    /// A general command for another session, with its arguments.
    pub fn session_command(&self, session_id: &str, body: &Value) -> Result<()> {
        self.post(&format!("/Sessions/{session_id}/Command"), body).map(drop)
    }

    // ----- Branding -------------------------------------------------------------

    /// The address of the splash image as the server keeps it.
    pub fn splashscreen_file_url(&self) -> String {
        self.url("/Branding/Splashscreen", &[])
    }

    pub fn upload_splashscreen(&self, bytes: &[u8], mime: &str) -> Result<()> {
        self.post_image("/Branding/Splashscreen", bytes, mime)
    }

    pub fn delete_splashscreen(&self) -> Result<()> {
        self.call(Method::Delete, "/Branding/Splashscreen", &[])
    }

    // ----- The metadata of an item ----------------------------------------------

    /// An item as the server has it for the signed-in user, every field:
    /// what a metadata editor sends back, and what a download keeps.
    pub fn item_json(&self, id: &str) -> Result<Value> {
        let user = self.user()?.to_string();
        self.get(&format!("/Items/{id}"), &[("userId", user)])
    }

    /// Sends an item back whole.
    pub fn save_item(&self, id: &str, item: &Value) -> Result<()> {
        self.post(&format!("/Items/{id}"), item).map(drop)
    }

    /// The images an item has.
    pub fn item_images<T: DeserializeOwned>(&self, id: &str) -> Result<T> {
        self.get(&format!("/Items/{id}/Images"), &[])
    }

    /// The images the providers offer for an item: `type`, `startIndex`,
    /// `limit`, `IncludeAllLanguages`.
    pub fn remote_images<T: DeserializeOwned>(&self, id: &str, query: &[(&str, String)]) -> Result<T> {
        self.get(&format!("/Items/{id}/RemoteImages"), query)
    }

    /// Makes an image of a provider the item's image of a kind.
    pub fn download_remote_image(&self, id: &str, kind: &str, url: &str, provider: &str) -> Result<()> {
        self.call(
            Method::Post,
            &format!("/Items/{id}/RemoteImages/Download"),
            &[("Type", kind.to_string()), ("ImageUrl", url.to_string()), ("ProviderName", provider.to_string())],
        )
    }

    pub fn delete_item_image(&self, id: &str, kind: &str, index: u32) -> Result<()> {
        self.call(Method::Delete, &format!("/Items/{id}/Images/{kind}/{index}"), &[])
    }

    /// The titles the providers know for the search of a kind of item
    /// ("Movie", "Series"...).
    pub fn remote_search(&self, kind: &str, body: &Value) -> Result<Vec<Value>> {
        self.post_json(&format!("/Items/RemoteSearch/{kind}"), body)
    }

    /// The marked ranges of an item as the server sends them.
    pub fn media_segments_json(&self, item_id: &str) -> Result<Value> {
        self.get(&format!("/MediaSegments/{item_id}"), &[])
    }

    /// What a user did with an item: the position, the last play.
    pub fn user_data_json(&self, item_id: &str, user_id: &str) -> Result<Value> {
        self.get(&format!("/UserItems/{item_id}/UserData"), &[("userId", user_id.to_string())])
    }

    /// Changes what the server holds of a user for an item (`PlaybackPositionTicks`...).
    pub fn save_user_data(&self, item_id: &str, user_id: &str, body: &Value) -> Result<()> {
        self.post(&format!("/UserItems/{item_id}/UserData?userId={user_id}"), body).map(drop)
    }

    // ----- SyncPlay -------------------------------------------------------------

    pub fn syncplay_groups<T: DeserializeOwned>(&self) -> Result<T> {
        self.get("/SyncPlay/List", &[])
    }

    /// A SyncPlay request: its path (`syncplay::protocol::Request::path`)
    /// and its body, when it has one. The result comes over the socket; a
    /// 204 here does not mean the server did it.
    pub fn syncplay_request(&self, path: &str, body: Option<&Value>) -> Result<()> {
        match body {
            Some(body) => self.post(path, body).map(drop),
            None => self.call(Method::Post, path, &[]),
        }
    }
}

fn configuration_path(name: &str) -> String {
    match name {
        "" => "/System/Configuration".to_string(),
        name => format!("/System/Configuration/{name}"),
    }
}

fn plugin_configuration_path(id: &str) -> String {
    format!("/Plugins/{id}/Configuration")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_paths_of_the_configuration() {
        assert_eq!(configuration_path(""), "/System/Configuration");
        assert_eq!(configuration_path("branding"), "/System/Configuration/branding");
        assert_eq!(plugin_configuration_path("abc"), "/Plugins/abc/Configuration");
    }
}
