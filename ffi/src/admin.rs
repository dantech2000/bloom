//! The server's dashboard for the iOS app (the Mac's src/admin, the parts a
//! phone uses): the server and its activity, storage and paths, users and
//! their access, libraries, tasks and their triggers, devices, the
//! activity log, logs, API keys, plugins and branding. The other settings
//! pages that edit the server's JSON objects open in Jellyfin Web instead.
//! Every call needs an administrator; the server refuses it otherwise.
//!
//! An editor of a server object (a policy, the branding) gets the object as
//! the server gave it (`original`, JSON) next to the typed values the page
//! shows. A save reads the object again and sends it back whole with only
//! the values the user changed (`draft::with_changes`), so the fields the
//! app does not know stay as the server has them.

use bloom_core::{draft::with_changes, jellyfin::Client};
use serde_json::{Value, json};

use crate::{BloomError, CollectionType, LogSeverity, PluginStatus, Server, SyncPlayAccess, TaskResult, TaskState, flag, parse_json, run};

fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn opt(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).filter(|t| !t.is_empty()).map(str::to_string)
}

fn refused(detail: impl Into<String>) -> BloomError {
    BloomError::Answered { detail: detail.into() }
}

/// Writes into `object` the values of `edited` that differ from `original`
/// (both as `(key, value)` lists of one typed editor, in the same order).
/// A value the user left alone is not written, so a key the server does
/// not send stays absent.
fn put_changes(object: &mut Value, original: Vec<(&str, Value)>, edited: Vec<(&str, Value)>) {
    for ((key, before), (_, after)) in original.into_iter().zip(edited) {
        if before != after {
            object[key] = after;
        }
    }
}

/// A server date ("2026-11-14T10:20:30.1234567Z") as seconds since 1970.
fn epoch_seconds(iso: &str) -> Option<i64> {
    let (date, time) = iso.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.trim_end_matches('Z').split(':');
    let (h, min) = (t.next()?.parse::<i64>().ok()?, t.next()?.parse::<i64>().ok()?);
    let sec = t.next().and_then(|s| s.split(['.', '+', '-']).next()?.parse::<i64>().ok()).unwrap_or(0);
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + min * 60 + sec)
}

fn list(v: Value) -> Vec<Value> {
    match v {
        Value::Array(items) => items,
        Value::Object(mut map) => match map.remove("Items") {
            Some(Value::Array(items)) => items,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

#[derive(uniffi::Record)]
pub struct ServerOverview {
    pub name: String,
    pub version: String,
    pub os: String,
    pub pending_restart: bool,
    pub update_available: bool,
    pub movies: u32,
    pub series: u32,
    pub episodes: u32,
    pub running_tasks: Vec<AdminTask>,
    /// Tasks whose last run failed.
    pub failed_tasks: Vec<AdminTask>,
    pub now_playing: Vec<ActiveSession>,
    /// Devices the server heard from in the last 16 minutes: those with a
    /// signed-in session and those that only ask for data (the Mac's count).
    pub active_devices: u32,
    /// What the server did by itself (activity with no user), newest first.
    pub alerts: Vec<ActivityEntry>,
    /// Address of the dashboard in Jellyfin Web.
    pub web_dashboard: String,
}

#[derive(uniffi::Record)]
pub struct ActiveSession {
    pub user: String,
    pub device: String,
    pub client: String,
    pub title: Option<String>,
    pub paused: bool,
    pub method: Option<String>,
}

#[derive(uniffi::Record)]
pub struct AdminUser {
    pub id: String,
    pub name: String,
    pub admin: bool,
    pub disabled: bool,
    pub last_active: Option<String>,
}

#[derive(uniffi::Record)]
pub struct AdminLibrary {
    pub id: String,
    pub name: String,
    pub collection_type: Option<CollectionType>,
    pub locations: Vec<String>,
    /// 0 to 100 while a scan runs.
    pub scan_progress: Option<f64>,
}

#[derive(uniffi::Record)]
pub struct AdminTask {
    pub id: String,
    pub name: String,
    pub category: String,
    pub description: String,
    pub state: TaskState,
    pub progress: Option<f64>,
    pub last_result: Option<TaskResult>,
    pub last_end: Option<String>,
    /// The triggers in words: "Every day at 03:00", "Manual only".
    pub schedule: String,
}

/// One disk of the server, with its folders that live on it.
#[derive(uniffi::Record)]
pub struct StorageDisk {
    /// The libraries and folders on it: "Movies, Shows, Server data".
    pub labels: Vec<String>,
    pub used: u64,
    pub free: u64,
    pub network: bool,
}

#[derive(uniffi::Record)]
pub struct ServerPath {
    pub label: String,
    pub path: String,
}

/// The disks and the folders of the server (the Mac loads them once).
#[derive(uniffi::Record)]
pub struct ServerStorage {
    /// Empty when the server does not report disk space (before 10.11).
    pub disks: Vec<StorageDisk>,
    pub paths: Vec<ServerPath>,
}

/// The values of a user's policy that the access editor shows (the Mac's
/// src/admin/access.rs). The rest of the policy stays in `original`.
#[derive(uniffi::Record, Clone)]
pub struct AccessPolicy {
    pub is_administrator: bool,
    pub is_disabled: bool,
    pub is_hidden: bool,
    pub enable_media_playback: bool,
    pub enable_video_playback_transcoding: bool,
    pub enable_audio_playback_transcoding: bool,
    pub enable_playback_remuxing: bool,
    pub enable_content_downloading: bool,
    pub enable_content_deletion: bool,
    pub enable_collection_management: bool,
    pub enable_subtitle_management: bool,
    pub enable_remote_control_of_other_users: bool,
    pub enable_shared_device_control: bool,
    pub sync_play_access: SyncPlayAccess,
    /// Bits per second; 0 is no limit.
    pub remote_client_bitrate_limit: i64,
    /// 0 is no limit.
    pub max_active_sessions: i64,
    /// -1 never locks; 0 locks after 3.
    pub login_attempts_before_lockout: i64,
    pub enable_all_folders: bool,
    /// Library ids as the server spells them.
    pub enabled_folders: Vec<String>,
}

#[derive(uniffi::Record)]
pub struct AccessLibrary {
    pub id: String,
    pub name: String,
}

#[derive(uniffi::Record)]
pub struct UserAccess {
    /// The policy as the server gave it (JSON), for the save.
    pub original: String,
    pub policy: AccessPolicy,
    pub libraries: Vec<AccessLibrary>,
    /// The signed-in account: its administrator and disabled values stay.
    pub me: bool,
}

#[derive(uniffi::Record)]
pub struct TaskTrigger {
    /// In words: "On Sundays at 03:00, stops after 2 hours".
    pub text: String,
    /// The trigger as the server has it (JSON); a removal names it so.
    pub raw: String,
}

#[derive(uniffi::Enum, Clone, Copy)]
pub enum TriggerKind {
    Daily,
    Weekly,
    Interval,
    Startup,
}

/// The branding configuration's text values; the rest stays in `original`.
#[derive(uniffi::Record)]
pub struct Branding {
    pub original: String,
    pub login_disclaimer: String,
    pub custom_css: String,
    pub splashscreen_enabled: bool,
    /// The server keeps an image of its own (`SplashscreenLocation`).
    pub custom_splash: bool,
    /// The splash image as clients load it (no key needed): the custom
    /// image, or else the server's default.
    pub splash_url: String,
}

#[derive(uniffi::Record)]
pub struct AdminDevice {
    pub id: String,
    pub name: String,
    pub app: String,
    pub user: String,
    pub last_active: Option<String>,
}

#[derive(uniffi::Record)]
pub struct ActivityEntry {
    pub name: String,
    pub overview: Option<String>,
    pub date: String,
    pub severity: LogSeverity,
}

#[derive(uniffi::Record)]
pub struct LogFile {
    pub name: String,
    pub modified: String,
    pub size: u64,
}

#[derive(uniffi::Record)]
pub struct ApiKey {
    pub token: String,
    pub app: String,
    pub created: String,
}

#[derive(uniffi::Record)]
pub struct AdminPlugin {
    pub id: String,
    pub name: String,
    pub version: String,
    pub status: PluginStatus,
    pub description: String,
    pub can_uninstall: bool,
    /// Its settings page in Jellyfin Web.
    pub web_url: String,
}

fn task(t: &Value) -> AdminTask {
    let last = t.get("LastExecutionResult");
    AdminTask {
        id: s(t, "Id"),
        name: s(t, "Name"),
        category: s(t, "Category"),
        description: s(t, "Description"),
        state: TaskState::parse(&s(t, "State")),
        progress: t.get("CurrentProgressPercentage").and_then(Value::as_f64),
        last_result: last.and_then(|l| opt(l, "Status")).map(|status| TaskResult::parse(&status)),
        last_end: last.and_then(|l| opt(l, "EndTimeUtc")),
        schedule: schedule(t.get("Triggers").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default()),
    }
}

// ----- triggers (the Mac's src/admin/triggers.rs) ----------------------------

/// Server ticks in one second, and in one minute.
const TICKS_PER_SECOND: i64 = 10_000_000;
const TICKS_PER_MINUTE: i64 = 60 * TICKS_PER_SECOND;
const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

/// A time of day in ticks as "02:00" (24 hours, local time of the server).
fn time_text(ticks: i64) -> String {
    let minutes = ticks / TICKS_PER_MINUTE;
    format!("{:02}:{:02}", minutes / 60 % 24, minutes % 60)
}

/// A length of time in words: "2 hours", "1 hour 30 minutes", "45 minutes".
fn span(minutes: i64) -> String {
    let unit = |n: i64, name: &str| if n == 1 { format!("{n} {name}") } else { format!("{n} {name}s") };
    match (minutes / 60, minutes % 60) {
        (0, m) => unit(m, "minute"),
        (h, 0) => unit(h, "hour"),
        (h, m) => format!("{} {}", unit(h, "hour"), unit(m, "minute")),
    }
}

/// How often an interval trigger fires: "Every 12 hours", "Every 90 minutes".
fn every(ticks: i64) -> String {
    match (ticks / TICKS_PER_SECOND).max(1) {
        1 => "Every second".to_string(),
        s if s < 60 || s % 60 != 0 => format!("Every {s} seconds"),
        3600 => "Every hour".to_string(),
        s if s % 3600 == 0 => format!("Every {} hours", s / 3600),
        s => format!("Every {} minutes", s / 60),
    }
}

/// One trigger in plain words, a time limit after it.
fn describe(trigger: &Value) -> String {
    let at = time_text(trigger.get("TimeOfDayTicks").and_then(Value::as_i64).unwrap_or(0));
    let day = match trigger.get("DayOfWeek") {
        Some(Value::String(name)) => DAYS.iter().copied().find(|d| d.eq_ignore_ascii_case(name)),
        Some(Value::Number(n)) => n.as_u64().and_then(|n| DAYS.get(n as usize).copied()),
        _ => None,
    };
    let mut text = match trigger.get("Type").and_then(Value::as_str).unwrap_or("") {
        "DailyTrigger" => format!("Every day at {at}"),
        "WeeklyTrigger" => match day {
            Some(day) => format!("On {day}s at {at}"),
            None => format!("Every week at {at}"),
        },
        "IntervalTrigger" => every(trigger.get("IntervalTicks").and_then(Value::as_i64).unwrap_or(0)),
        "StartupTrigger" => "When the server starts".to_string(),
        other => other.trim_end_matches("Trigger").to_string(),
    };
    if let Some(ticks) = trigger.get("MaxRuntimeTicks").and_then(Value::as_i64) {
        text.push_str(&format!(", stops after {}", span((ticks / TICKS_PER_MINUTE).max(1))));
    }
    text
}

fn schedule(triggers: &[Value]) -> String {
    match triggers {
        [] => "Manual only".to_string(),
        triggers => triggers.iter().map(describe).collect::<Vec<_>>().join(", "),
    }
}

fn read_triggers(client: &Client, id: &str) -> Result<Vec<Value>, BloomError> {
    let task = client.scheduled_task(id)?;
    Ok(task.get("Triggers").and_then(Value::as_array).cloned().unwrap_or_default())
}

fn trigger_rows(triggers: &[Value]) -> Vec<TaskTrigger> {
    triggers.iter().map(|t| TaskTrigger { text: describe(t), raw: t.to_string() }).collect()
}

/// Reads the triggers again, changes that list, sends it back whole, and
/// returns what the server holds then.
fn change_triggers(client: &Client, id: &str, change: impl FnOnce(&mut Vec<Value>) -> Result<(), BloomError>) -> Result<Vec<TaskTrigger>, BloomError> {
    let mut list = read_triggers(client, id)?;
    change(&mut list)?;
    client.set_task_triggers(id, &list)?;
    Ok(trigger_rows(&read_triggers(client, id)?))
}

// ----- access (the Mac's src/admin/access.rs) ---------------------------------

/// The server refuses a policy without these two.
const PROVIDER_KEYS: [&str; 2] = ["AuthenticationProviderId", "PasswordResetProviderId"];

impl AccessPolicy {
    fn from_json(p: &Value) -> Self {
        let number = |key: &str, default: i64| p.get(key).and_then(Value::as_i64).unwrap_or(default);
        Self {
            is_administrator: flag(p, "IsAdministrator"),
            is_disabled: flag(p, "IsDisabled"),
            is_hidden: flag(p, "IsHidden"),
            enable_media_playback: flag(p, "EnableMediaPlayback"),
            enable_video_playback_transcoding: flag(p, "EnableVideoPlaybackTranscoding"),
            enable_audio_playback_transcoding: flag(p, "EnableAudioPlaybackTranscoding"),
            enable_playback_remuxing: flag(p, "EnablePlaybackRemuxing"),
            enable_content_downloading: flag(p, "EnableContentDownloading"),
            enable_content_deletion: flag(p, "EnableContentDeletion"),
            enable_collection_management: flag(p, "EnableCollectionManagement"),
            enable_subtitle_management: flag(p, "EnableSubtitleManagement"),
            enable_remote_control_of_other_users: flag(p, "EnableRemoteControlOfOtherUsers"),
            enable_shared_device_control: flag(p, "EnableSharedDeviceControl"),
            sync_play_access: SyncPlayAccess::parse(&opt(p, "SyncPlayAccess").unwrap_or_else(|| "CreateAndJoinGroups".into())),
            remote_client_bitrate_limit: number("RemoteClientBitrateLimit", 0),
            max_active_sessions: number("MaxActiveSessions", 0),
            login_attempts_before_lockout: number("LoginAttemptsBeforeLockout", -1),
            enable_all_folders: flag(p, "EnableAllFolders"),
            enabled_folders: p.get("EnabledFolders").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect(),
        }
    }

    /// The values as policy keys, in one order for `put_changes`.
    fn pairs(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("IsAdministrator", json!(self.is_administrator)),
            ("IsDisabled", json!(self.is_disabled)),
            ("IsHidden", json!(self.is_hidden)),
            ("EnableMediaPlayback", json!(self.enable_media_playback)),
            ("EnableVideoPlaybackTranscoding", json!(self.enable_video_playback_transcoding)),
            ("EnableAudioPlaybackTranscoding", json!(self.enable_audio_playback_transcoding)),
            ("EnablePlaybackRemuxing", json!(self.enable_playback_remuxing)),
            ("EnableContentDownloading", json!(self.enable_content_downloading)),
            ("EnableContentDeletion", json!(self.enable_content_deletion)),
            ("EnableCollectionManagement", json!(self.enable_collection_management)),
            ("EnableSubtitleManagement", json!(self.enable_subtitle_management)),
            ("EnableRemoteControlOfOtherUsers", json!(self.enable_remote_control_of_other_users)),
            ("EnableSharedDeviceControl", json!(self.enable_shared_device_control)),
            ("SyncPlayAccess", json!(self.sync_play_access.text())),
            ("RemoteClientBitrateLimit", json!(self.remote_client_bitrate_limit)),
            ("MaxActiveSessions", json!(self.max_active_sessions)),
            ("LoginAttemptsBeforeLockout", json!(self.login_attempts_before_lockout)),
            ("EnableAllFolders", json!(self.enable_all_folders)),
            ("EnabledFolders", json!(self.enabled_folders)),
        ]
    }
}

/// The policy to send: the server's policy now, with the values the user
/// changed from `original` (and only those). For the signed-in account the
/// administrator and disabled values never change.
fn policy_to_send(fresh: &Value, original: &Value, edited: &AccessPolicy, me: bool) -> Value {
    let mut edited = edited.clone();
    let before = AccessPolicy::from_json(original);
    if me {
        edited.is_administrator = before.is_administrator;
        edited.is_disabled = before.is_disabled;
    }
    let mut changed = original.clone();
    put_changes(&mut changed, before.pairs(), edited.pairs());
    with_changes(fresh, original, &changed)
}

/// The user's policy as the server holds it now, when a save may build on
/// it: an object with both provider keys (the server refuses a policy
/// without them, and a write into a value that is not an object panics).
fn fresh_policy(client: &Client, user_id: &str) -> Result<Value, BloomError> {
    let fresh = client.user_json(user_id)?.get("Policy").cloned().unwrap_or(Value::Null);
    checked_policy(fresh)
}

fn checked_policy(fresh: Value) -> Result<Value, BloomError> {
    if !fresh.is_object() {
        return Err(refused("the server sent no policy for this user"));
    }
    for key in PROVIDER_KEYS {
        if fresh.get(key).and_then(Value::as_str).is_none_or(str::is_empty) {
            return Err(refused(format!("the policy of the server has no {key}")));
        }
    }
    Ok(fresh)
}

/// The policy that enables or disables a user: the server's policy with
/// that one value changed. The signed-in account never disables itself.
fn disabled_policy(fresh: &Value, disabled: bool, me: bool) -> Result<Value, BloomError> {
    if me && disabled {
        return Err(refused("an administrator cannot disable the account they are signed in with"));
    }
    let mut edited = AccessPolicy::from_json(fresh);
    edited.is_disabled = disabled;
    Ok(policy_to_send(fresh, fresh, &edited, me))
}

fn read_access(client: &Client, user_id: &str) -> Result<UserAccess, BloomError> {
    let user = client.user_json(user_id)?;
    let policy = user.get("Policy").cloned().unwrap_or(Value::Null);
    if !policy.is_object() {
        return Err(refused("the server sent no policy for this user"));
    }
    let folders = client.media_folders()?;
    Ok(UserAccess {
        original: policy.to_string(),
        policy: AccessPolicy::from_json(&policy),
        libraries: list(folders).iter().map(|f| AccessLibrary { id: s(f, "Id"), name: s(f, "Name") }).filter(|l| !l.id.is_empty()).collect(),
        me: client.user_id.as_deref() == Some(user_id),
    })
}

// ----- storage (the Mac's src/admin/dashboard.rs) ------------------------------

fn disks(storage: &Value) -> Vec<StorageDisk> {
    let mut disks: Vec<StorageDisk> = Vec::new();
    // Folders with the same free and used space are on the same disk.
    let mut add = |label: &str, folder: &Value| {
        let (free, used) = (folder["FreeSpace"].as_i64().unwrap_or(0), folder["UsedSpace"].as_i64().unwrap_or(0));
        if free + used <= 0 {
            return;
        }
        let (free, used) = (free.max(0) as u64, used.max(0) as u64);
        match disks.iter_mut().find(|d| d.free == free && d.used == used) {
            Some(disk) => {
                if !disk.labels.iter().any(|l| l == label) {
                    disk.labels.push(label.to_string());
                }
            }
            None => disks.push(StorageDisk { labels: vec![label.to_string()], used, free, network: s(folder, "StorageType") == "Network" }),
        }
    };
    let data = s(&storage["ProgramDataFolder"], "Path");
    // Media first: it is the disk that fills up. The collections library is
    // a folder of the server's own data (by whole folder names).
    for library in storage["Libraries"].as_array().into_iter().flatten() {
        for folder in library["Folders"].as_array().into_iter().flatten() {
            let inside_data = !data.is_empty() && std::path::Path::new(&s(folder, "Path")).starts_with(&data);
            if !inside_data {
                add(&s(library, "Name"), folder);
            }
        }
    }
    for (label, key) in [
        ("Server data", "ProgramDataFolder"),
        ("Metadata", "InternalMetadataFolder"),
        ("Cache", "CacheFolder"),
        ("Transcodes", "TranscodingTempFolder"),
        ("Logs", "LogFolder"),
    ] {
        if let Some(folder) = storage.get(key).filter(|f| f.is_object()) {
            add(label, folder);
        }
    }
    disks
}

/// A device counts as active when the server heard from it in this time.
const ACTIVE_SECONDS: i64 = 960;

/// The devices with a signed-in session, and those that only ask the
/// server for data (a TV on its home screen), as the Mac counts them.
fn active_devices(sessions: &[Value], devices: &[Value]) -> u32 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let mut ids: Vec<String> = sessions.iter().filter(|x| opt(x, "UserId").is_some()).map(|x| s(x, "DeviceId")).collect();
    for device in devices {
        if epoch_seconds(&s(device, "DateLastActivity")).is_some_and(|at| now - at <= ACTIVE_SECONDS) {
            ids.push(s(device, "Id"));
        }
    }
    ids.sort();
    ids.dedup();
    ids.len() as u32
}

fn activity_entry(e: &Value) -> ActivityEntry {
    ActivityEntry { name: s(e, "Name"), overview: opt(e, "ShortOverview"), date: s(e, "Date"), severity: LogSeverity::parse(&s(e, "Severity")) }
}

// ----- branding ------------------------------------------------------------------

impl Branding {
    fn from_json(b: &Value) -> Self {
        Self {
            original: b.to_string(),
            login_disclaimer: s(b, "LoginDisclaimer"),
            custom_css: s(b, "CustomCss"),
            splashscreen_enabled: flag(b, "SplashscreenEnabled"),
            custom_splash: opt(b, "SplashscreenLocation").is_some(),
            splash_url: String::new(),
        }
    }

    /// The branding the server holds now, with the address of its image.
    fn fetch(client: &Client) -> Result<Self, BloomError> {
        Ok(Self { splash_url: client.splashscreen_file_url(), ..Self::from_json(&client.configuration("branding")?) })
    }

    fn pairs(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("LoginDisclaimer", json!(self.login_disclaimer)),
            ("CustomCss", json!(self.custom_css)),
            ("SplashscreenEnabled", json!(self.splashscreen_enabled)),
        ]
    }
}

fn web(client: &Client, path: &str) -> String {
    format!("{}/web/#/{path}", client.base.trim_end_matches('/'))
}

#[uniffi::export]
impl Server {
    pub async fn admin_overview(&self) -> Result<ServerOverview, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let info: Value = client.system_info()?;
            let counts: Value = client.item_counts().unwrap_or_default();
            let tasks = list(client.scheduled_tasks().unwrap_or_default());
            let sessions = list(client.sessions(&[("ActiveWithinSeconds", ACTIVE_SECONDS.to_string())]).unwrap_or_default());
            let devices = list(client.devices().unwrap_or_default());
            // What the server did by itself: no user is named.
            let alerts = list(client.activity_log(&[("Limit", "7".to_string()), ("HasUserId", "false".to_string())]).unwrap_or_default());
            let count = |key: &str| counts.get(key).and_then(Value::as_u64).unwrap_or_default() as u32;
            Ok(ServerOverview {
                name: s(&info, "ServerName"),
                version: s(&info, "Version"),
                os: opt(&info, "OperatingSystemDisplayName").unwrap_or_else(|| s(&info, "OperatingSystem")),
                pending_restart: flag(&info, "HasPendingRestart"),
                update_available: flag(&info, "HasUpdateAvailable"),
                movies: count("MovieCount"),
                series: count("SeriesCount"),
                episodes: count("EpisodeCount"),
                running_tasks: tasks.iter().map(task).filter(|t| t.state != TaskState::Idle).collect(),
                failed_tasks: tasks.iter().map(task).filter(|t| t.state == TaskState::Idle && t.last_result == Some(TaskResult::Failed)).collect(),
                active_devices: active_devices(&sessions, &devices),
                alerts: alerts.iter().map(activity_entry).collect(),
                now_playing: sessions
                    .iter()
                    .filter(|x| x.get("NowPlayingItem").is_some())
                    .map(|x| {
                        let item = &x["NowPlayingItem"];
                        let title = match (opt(item, "SeriesName"), opt(item, "Name")) {
                            (Some(series), Some(name)) => Some(format!("{series} · {name}")),
                            (_, name) => name,
                        };
                        ActiveSession {
                            user: s(x, "UserName"),
                            device: s(x, "DeviceName"),
                            client: s(x, "Client"),
                            title,
                            paused: flag(&x["PlayState"], "IsPaused"),
                            method: opt(&x["PlayState"], "PlayMethod"),
                        }
                    })
                    .collect(),
                web_dashboard: web(&client, "dashboard"),
            })
        })
        .await
    }

    pub async fn admin_scan_libraries(&self) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || Ok(client.scan_libraries()?)).await
    }

    /// The disks and the folders of the server.
    pub async fn admin_storage(&self) -> Result<ServerStorage, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let info: Value = client.system_info()?;
            // Servers before 10.11 have no storage endpoint.
            let storage: Value = client.storage_info().unwrap_or_default();
            let paths = [
                ("Server data", "ProgramDataPath"),
                ("Metadata", "InternalMetadataPath"),
                ("Cache", "CachePath"),
                ("Transcodes", "TranscodingTempPath"),
                ("Logs", "LogPath"),
                ("Web client", "WebPath"),
            ];
            Ok(ServerStorage {
                disks: disks(&storage),
                paths: paths.iter().filter_map(|(label, key)| Some(ServerPath { label: label.to_string(), path: opt(&info, key)? })).collect(),
            })
        })
        .await
    }

    /// Restarts or shuts down the server (`shutdown`). The app asks first;
    /// no test sends it (CLAUDE.md).
    pub async fn admin_power(&self, shutdown: bool) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || {
            if shutdown { client.shutdown_server()? } else { client.restart_server()? }
            Ok(())
        })
        .await
    }

    pub async fn admin_users(&self) -> Result<Vec<AdminUser>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let users: Value = client.users()?;
            Ok(list(users)
                .iter()
                .map(|u| AdminUser {
                    id: s(u, "Id"),
                    name: s(u, "Name"),
                    admin: flag(&u["Policy"], "IsAdministrator"),
                    disabled: flag(&u["Policy"], "IsDisabled"),
                    last_active: opt(u, "LastActivityDate"),
                })
                .collect())
        })
        .await
    }

    /// Enables or disables a user: the policy goes back whole, with the one
    /// value changed (a typed policy would drop what the app does not know).
    pub async fn admin_set_user_disabled(&self, user_id: String, disabled: bool) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let fresh = fresh_policy(&client, &user_id)?;
            let me = client.user_id.as_deref() == Some(user_id.as_str());
            Ok(client.set_user_policy(&user_id, &disabled_policy(&fresh, disabled, me)?)?)
        })
        .await
    }

    /// The access editor of a user: the policy and the libraries.
    pub async fn admin_user_access(&self, user_id: String) -> Result<UserAccess, BloomError> {
        let client = self.signed_in()?;
        run(move || read_access(&client, &user_id)).await
    }

    /// Saves the access editor. The policy is read again first, so a change
    /// somebody else made to a value the user did not edit stays; returns
    /// the editor as the server holds it then.
    pub async fn admin_save_user_access(&self, user_id: String, original: String, edited: AccessPolicy) -> Result<UserAccess, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let original = parse_json(&original)?;
            if !original.is_object() {
                return Err(BloomError::Internal { message: "the app's original policy is not an object".into() });
            }
            let fresh = fresh_policy(&client, &user_id)?;
            let me = client.user_id.as_deref() == Some(user_id.as_str());
            client.set_user_policy(&user_id, &policy_to_send(&fresh, &original, &edited, me))?;
            read_access(&client, &user_id)
        })
        .await
    }

    pub async fn admin_set_user_password(&self, user_id: String, password: String) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || Ok(client.set_user_password(&user_id, &json!({ "NewPw": password }))?)).await
    }

    pub async fn admin_create_user(&self, name: String, password: String) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || Ok(client.create_user(&json!({ "Name": name, "Password": password }))?)).await
    }

    pub async fn admin_delete_user(&self, user_id: String) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || Ok(client.delete_user(&user_id)?)).await
    }

    pub async fn admin_libraries(&self) -> Result<Vec<AdminLibrary>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let folders: Value = client.virtual_folders()?;
            Ok(list(folders)
                .iter()
                .map(|f| AdminLibrary {
                    id: s(f, "ItemId"),
                    name: s(f, "Name"),
                    collection_type: opt(f, "CollectionType").map(|kind| CollectionType::parse(&kind)),
                    locations: f.get("Locations").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect(),
                    scan_progress: (s(f, "RefreshStatus") == "Active").then(|| f.get("RefreshProgress").and_then(Value::as_f64).unwrap_or(0.)),
                })
                .collect())
        })
        .await
    }

    /// Scans one library for new and changed files.
    pub async fn admin_scan_library(&self, library_id: String) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let query = [
                ("Recursive", "true".to_string()),
                ("ImageRefreshMode", "Default".to_string()),
                ("MetadataRefreshMode", "Default".to_string()),
                ("ReplaceAllImages", "false".to_string()),
                ("ReplaceAllMetadata", "false".to_string()),
            ];
            Ok(client.refresh_item(&library_id, &query)?)
        })
        .await
    }

    pub async fn admin_tasks(&self) -> Result<Vec<AdminTask>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let tasks: Value = client.scheduled_tasks()?;
            let mut all: Vec<AdminTask> = list(tasks).iter().filter(|t| !flag(t, "IsHidden")).map(task).collect();
            all.sort_by(|a, b| a.category.cmp(&b.category).then(a.name.cmp(&b.name)));
            Ok(all)
        })
        .await
    }

    pub async fn admin_run_task(&self, task_id: String, stop: bool) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || {
            if stop { client.stop_task(&task_id)? } else { client.start_task(&task_id)? }
            Ok(())
        })
        .await
    }

    pub async fn admin_task_triggers(&self, task_id: String) -> Result<Vec<TaskTrigger>, BloomError> {
        let client = self.signed_in()?;
        run(move || Ok(trigger_rows(&read_triggers(&client, &task_id)?))).await
    }

    /// Adds a trigger: `minutes` after midnight (daily, weekly), `day` 0 for
    /// Sunday (weekly), `interval_minutes` (interval), and a time limit.
    pub async fn admin_add_task_trigger(
        &self,
        task_id: String,
        kind: TriggerKind,
        minutes: i64,
        day: u8,
        interval_minutes: i64,
        limit_minutes: Option<i64>,
    ) -> Result<Vec<TaskTrigger>, BloomError> {
        let client = self.signed_in()?;
        // A time of day: minutes after midnight, within the day.
        let minutes = minutes.clamp(0, 1439);
        run(move || {
            let mut trigger = match kind {
                TriggerKind::Daily => json!({ "Type": "DailyTrigger", "TimeOfDayTicks": minutes * TICKS_PER_MINUTE }),
                TriggerKind::Weekly => json!({
                    "Type": "WeeklyTrigger",
                    "DayOfWeek": DAYS[day as usize % 7],
                    "TimeOfDayTicks": minutes * TICKS_PER_MINUTE,
                }),
                // Up to a year: a larger number does not fit the server's time span.
                TriggerKind::Interval => json!({ "Type": "IntervalTrigger", "IntervalTicks": interval_minutes.clamp(1, 525_600) * TICKS_PER_MINUTE }),
                TriggerKind::Startup => json!({ "Type": "StartupTrigger" }),
            };
            if let Some(limit) = limit_minutes {
                trigger["MaxRuntimeTicks"] = json!(limit.clamp(1, 525_600) * TICKS_PER_MINUTE);
            }
            change_triggers(&client, &task_id, |list| {
                list.push(trigger);
                Ok(())
            })
        })
        .await
    }

    /// Removes the trigger `raw` (as `admin_task_triggers` gave it).
    pub async fn admin_remove_task_trigger(&self, task_id: String, raw: String) -> Result<Vec<TaskTrigger>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let trigger = parse_json(&raw)?;
            change_triggers(&client, &task_id, |list| match list.iter().position(|t| *t == trigger) {
                Some(at) => {
                    list.remove(at);
                    Ok(())
                }
                None => Err(refused("the trigger is not on the server any more")),
            })
        })
        .await
    }

    pub async fn admin_devices(&self) -> Result<Vec<AdminDevice>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let devices: Value = client.devices()?;
            Ok(list(devices)
                .iter()
                .map(|d| AdminDevice {
                    id: s(d, "Id"),
                    name: s(d, "Name"),
                    app: [s(d, "AppName"), s(d, "AppVersion")].join(" ").trim().to_string(),
                    user: s(d, "LastUserName"),
                    last_active: opt(d, "DateLastActivity"),
                })
                .collect())
        })
        .await
    }

    pub async fn admin_delete_device(&self, device_id: String) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || Ok(client.delete_device(&device_id)?)).await
    }

    pub async fn admin_activity(&self, limit: u32) -> Result<Vec<ActivityEntry>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let log: Value = client.activity_log(&[("Limit", limit.to_string())])?;
            Ok(list(log)
                .iter()
                .map(activity_entry)
                .collect())
        })
        .await
    }

    pub async fn admin_logs(&self) -> Result<Vec<LogFile>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let files: Value = client.log_files()?;
            let mut all: Vec<LogFile> = list(files)
                .iter()
                .map(|f| LogFile { name: s(f, "Name"), modified: s(f, "DateModified"), size: f.get("Size").and_then(Value::as_u64).unwrap_or_default() })
                .collect();
            all.sort_by(|a, b| b.modified.cmp(&a.modified));
            Ok(all)
        })
        .await
    }

    /// The end of a log file, at most `max` bytes.
    pub async fn admin_log_tail(&self, name: String, max: u32) -> Result<String, BloomError> {
        let client = self.signed_in()?;
        run(move || Ok(client.log_tail(&name, max as usize)?.0)).await
    }

    pub async fn admin_api_keys(&self) -> Result<Vec<ApiKey>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let keys: Value = client.api_keys()?;
            Ok(list(keys).iter().map(|k| ApiKey { token: s(k, "AccessToken"), app: s(k, "AppName"), created: s(k, "DateCreated") }).collect())
        })
        .await
    }

    pub async fn admin_create_api_key(&self, app: String) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || Ok(client.create_api_key(&app)?)).await
    }

    pub async fn admin_revoke_api_key(&self, token: String) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || Ok(client.revoke_api_key(&token)?)).await
    }

    pub async fn admin_plugins(&self) -> Result<Vec<AdminPlugin>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let plugins: Value = client.plugins()?;
            Ok(list(plugins)
                .iter()
                .map(|p| AdminPlugin {
                    id: s(p, "Id"),
                    name: s(p, "Name"),
                    version: s(p, "Version"),
                    status: PluginStatus::parse(&s(p, "Status")),
                    description: s(p, "Description"),
                    can_uninstall: flag(p, "CanUninstall"),
                    web_url: web(&client, &format!("dashboard/plugins/{}", s(p, "Id"))),
                })
                .collect())
        })
        .await
    }

    pub async fn admin_set_plugin_enabled(&self, plugin_id: String, version: String, enabled: bool) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || Ok(client.set_plugin_enabled(&plugin_id, &version, enabled)?)).await
    }

    pub async fn admin_uninstall_plugin(&self, plugin_id: String, version: String) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || Ok(client.uninstall_plugin(&plugin_id, &version)?)).await
    }

    pub async fn admin_branding(&self) -> Result<Branding, BloomError> {
        let client = self.signed_in()?;
        run(move || Branding::fetch(&client)).await
    }

    /// Makes `image` (PNG, JPEG, WebP or GIF, of type `mime`) the splash
    /// screen. The server writes the image's place into the branding, so
    /// the answer is the branding it holds after.
    pub async fn admin_upload_splash(&self, image: Vec<u8>, mime: String) -> Result<Branding, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            client.upload_splashscreen(&image, &mime)?;
            Branding::fetch(&client)
        })
        .await
    }

    /// Deletes the custom splash image; clients show the default again.
    pub async fn admin_remove_splash(&self) -> Result<Branding, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            client.delete_splashscreen()?;
            Branding::fetch(&client)
        })
        .await
    }

    /// Saves the branding: the configuration of the server now, with the
    /// values that differ from `original`. Returns what the server holds.
    pub async fn admin_save_branding(&self, original: String, login_disclaimer: String, custom_css: String, splashscreen_enabled: bool) -> Result<Branding, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let original = parse_json(&original)?;
            let before = Branding::from_json(&original);
            let after = Branding { login_disclaimer, custom_css, splashscreen_enabled, ..Branding::from_json(&Value::Null) };
            let mut changed = original.clone();
            put_changes(&mut changed, before.pairs(), after.pairs());
            let fresh = client.configuration("branding")?;
            client.save_configuration("branding", &with_changes(&fresh, &original, &changed))?;
            Branding::fetch(&client)
        })
        .await
    }

    /// A page of the dashboard in Jellyfin Web ("dashboard/general",
    /// "dashboard/playback/transcoding", "dashboard/networking"...).
    pub fn admin_web_page(&self, path: String) -> Result<String, BloomError> {
        Ok(web(&self.signed_in()?, &path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_branding_knows_a_custom_splash_image() {
        let set = json!({ "SplashscreenEnabled": true, "SplashscreenLocation": "/config/data/splashscreen.png" });
        assert!(Branding::from_json(&set).custom_splash);
        assert!(!Branding::from_json(&json!({ "SplashscreenLocation": "" })).custom_splash);
        assert!(!Branding::from_json(&json!({})).custom_splash);
    }

    fn policy() -> Value {
        json!({
            "IsAdministrator": false,
            "IsDisabled": false,
            "EnableAllFolders": true,
            "EnabledFolders": [],
            "EnableContentDownloading": true,
            "RemoteClientBitrateLimit": 0,
            "AuthenticationProviderId": "Default",
            "SomethingNew": { "Kept": [1, 2] },
        })
    }

    #[test]
    fn a_save_changes_only_the_edited_values() {
        let original = policy();
        let mut edited = AccessPolicy::from_json(&original);
        edited.enable_content_downloading = false;
        edited.remote_client_bitrate_limit = 8_000_000;
        // Somebody else changed these meanwhile.
        let mut fresh = original.clone();
        fresh["EnableAllFolders"] = json!(false);
        fresh["SomethingNew"] = json!({ "Kept": [3] });
        let sent = policy_to_send(&fresh, &original, &edited, false);
        assert_eq!(sent["EnableContentDownloading"], false);
        assert_eq!(sent["RemoteClientBitrateLimit"], 8_000_000);
        assert_eq!(sent["EnableAllFolders"], false);
        assert_eq!(sent["SomethingNew"], json!({ "Kept": [3] }));
        assert_eq!(sent["AuthenticationProviderId"], "Default");
        // A key the server does not send stays absent when left alone.
        assert!(sent.get("SyncPlayAccess").is_none());
    }

    #[test]
    fn the_own_account_keeps_its_administrator_and_disabled_values() {
        let original = policy();
        let mut edited = AccessPolicy::from_json(&original);
        edited.is_administrator = true;
        edited.is_disabled = true;
        edited.is_hidden = true;
        let sent = policy_to_send(&original, &original, &edited, true);
        assert_eq!(sent["IsAdministrator"], false);
        assert_eq!(sent["IsDisabled"], false);
        assert_eq!(sent["IsHidden"], true);
    }

    #[test]
    fn a_user_is_disabled_through_the_checked_policy() {
        let fresh = checked_policy(json!({ "IsDisabled": false, "AuthenticationProviderId": "Default", "PasswordResetProviderId": "Default", "SomethingNew": 1 }))
            .expect("a whole policy");
        let sent = disabled_policy(&fresh, true, false).expect("another user");
        assert_eq!(sent["IsDisabled"], true);
        assert_eq!(sent["SomethingNew"], 1, "the fields the app does not know stay");
        assert_eq!(sent["AuthenticationProviderId"], "Default");
        // Only the one value changes: no key the server did not send.
        assert!(sent.get("IsAdministrator").is_none());
        assert!(disabled_policy(&fresh, true, true).is_err(), "the own account disabled itself");
        assert_eq!(disabled_policy(&fresh, false, true).expect("enable self")["IsDisabled"], false);
        // A policy that is not an object, or without the providers, is refused.
        assert!(checked_policy(json!(["IsDisabled"])).is_err());
        assert!(checked_policy(Value::Null).is_err());
        assert!(checked_policy(json!({ "AuthenticationProviderId": "Default" })).is_err());
    }

    #[test]
    fn triggers_read_in_words() {
        assert_eq!(describe(&json!({ "Type": "DailyTrigger", "TimeOfDayTicks": 3 * 60 * TICKS_PER_MINUTE })), "Every day at 03:00");
        assert_eq!(describe(&json!({ "Type": "WeeklyTrigger", "DayOfWeek": "Sunday", "TimeOfDayTicks": 0 })), "On Sundays at 00:00");
        assert_eq!(describe(&json!({ "Type": "IntervalTrigger", "IntervalTicks": 864_000_000_000_i64 })), "Every 24 hours");
        assert_eq!(
            describe(&json!({ "Type": "StartupTrigger", "MaxRuntimeTicks": 90 * TICKS_PER_MINUTE })),
            "When the server starts, stops after 1 hour 30 minutes"
        );
        assert_eq!(schedule(&[]), "Manual only");
    }

    #[test]
    fn server_dates_become_seconds() {
        assert_eq!(epoch_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch_seconds("2026-11-14T10:20:30.1234567Z"), Some(1_794_651_630));
        assert_eq!(epoch_seconds("2000-03-01T00:00:00.000Z"), Some(951_868_800));
        assert_eq!(epoch_seconds("garbage"), None);
    }

    #[test]
    fn folders_on_one_disk_share_a_line() {
        let storage = json!({
            "ProgramDataFolder": { "Path": "/config", "FreeSpace": 10, "UsedSpace": 90 },
            "CacheFolder": { "Path": "/cache", "FreeSpace": 10, "UsedSpace": 90 },
            "Libraries": [
                { "Name": "Movies", "Folders": [{ "Path": "/media/movies", "FreeSpace": 500, "UsedSpace": 500, "StorageType": "Network" }] },
                { "Name": "Collections", "Folders": [{ "Path": "/config/data/collections", "FreeSpace": 10, "UsedSpace": 90 }] },
            ],
        });
        let disks = disks(&storage);
        assert_eq!(disks.len(), 2);
        assert_eq!(disks[0].labels, ["Movies"]);
        assert!(disks[0].network);
        assert_eq!(disks[1].labels, ["Server data", "Cache"]);
    }
}
