//! More of the dashboard for the iOS app (the Mac's src/admin/backups.rs,
//! the follow of src/admin/logs.rs and the filters of
//! src/admin/activity.rs): the server's backups, the lines a log file got
//! since the last read, and a page of the activity log that the server
//! filters and sorts (Jellyfin 12.0). The logic is in bloom-core
//! (`backups`, `log_follow`, `activity_query`).

use bloom_core::activity_query::{Kind, Level, Period, Query, Who};
use bloom_core::backups::{Backup, Contents, library_scan_running};
use serde_json::Value;

use crate::{BloomError, LogSeverity, Server, run};

#[derive(uniffi::Record)]
pub struct BackupInfo {
    /// What the manifest and the restore take.
    pub file_name: String,
    pub date: String,
    /// "12.2.0".
    pub server_version: String,
    /// "Database, Metadata".
    pub contents: String,
    pub metadata: bool,
    pub subtitles: bool,
    pub trickplay: bool,
}

#[derive(uniffi::Record)]
pub struct BackupList {
    /// The newest first.
    pub backups: Vec<BackupInfo>,
    /// The library scan runs: a backup now holds a library half scanned.
    pub scan_running: bool,
    /// The server's folder of the backups, when it has one.
    pub folder: Option<String>,
}

fn info(backup: &Backup) -> BackupInfo {
    BackupInfo {
        file_name: backup.file_name().to_string(),
        date: backup.date_created.clone(),
        server_version: backup.version().to_string(),
        contents: backup.options.summary(),
        metadata: backup.options.metadata,
        subtitles: backup.options.subtitles,
        trickplay: backup.options.trickplay,
    }
}

/// The lines of a log after a known end (`bloom_core::log_follow`).
#[derive(uniffi::Record)]
pub struct LogChunk {
    pub text: String,
    /// Where the next read starts.
    pub end: u64,
    /// The file started again: `text` replaces what the viewer has.
    pub restarted: bool,
    /// More was new than was read: `text` is the end of it.
    pub cut: bool,
}

#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq)]
pub enum ActivityKind {
    All,
    Playback,
    SignIn,
    Session,
    User,
    Plugin,
    Task,
    Subtitle,
}

#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq)]
pub enum ActivityLevel {
    All,
    Information,
    Warning,
    Error,
}

#[derive(uniffi::Enum, Clone, Copy, PartialEq, Eq)]
pub enum ActivityPeriod {
    Any,
    Day,
    Week,
    Month,
}

/// What a page of the activity log keeps and in what order.
#[derive(uniffi::Record)]
pub struct ActivityFilter {
    pub kind: ActivityKind,
    pub level: ActivityLevel,
    /// The entries with no user: what the server did by itself.
    pub server_only: bool,
    /// The entries of a user, by name; ignored with `server_only`.
    pub user: Option<String>,
    pub period: ActivityPeriod,
    pub oldest_first: bool,
    pub start: u32,
    pub limit: u32,
}

#[derive(uniffi::Record)]
pub struct ActivityLine {
    pub id: i64,
    pub name: String,
    pub overview: Option<String>,
    pub date: String,
    pub severity: LogSeverity,
    pub kind: String,
    pub user: Option<String>,
}

#[derive(uniffi::Record)]
pub struct ActivityPage {
    pub entries: Vec<ActivityLine>,
    /// The entries that match, on every page.
    pub total: u32,
    /// The users' names, for the user filter.
    pub users: Vec<String>,
}

fn query(filter: &ActivityFilter) -> Query {
    Query {
        kind: match filter.kind {
            ActivityKind::All => Kind::All,
            ActivityKind::Playback => Kind::Playback,
            ActivityKind::SignIn => Kind::SignIn,
            ActivityKind::Session => Kind::Session,
            ActivityKind::User => Kind::User,
            ActivityKind::Plugin => Kind::Plugin,
            ActivityKind::Task => Kind::Task,
            ActivityKind::Subtitle => Kind::Subtitle,
        },
        level: match filter.level {
            ActivityLevel::All => Level::All,
            ActivityLevel::Information => Level::Information,
            ActivityLevel::Warning => Level::Warning,
            ActivityLevel::Error => Level::Error,
        },
        who: match (&filter.user, filter.server_only) {
            (_, true) => Who::Server,
            (Some(name), false) if !name.is_empty() => Who::User(name.clone()),
            _ => Who::Anyone,
        },
        period: match filter.period {
            ActivityPeriod::Any => Period::Any,
            ActivityPeriod::Day => Period::Day,
            ActivityPeriod::Week => Period::Week,
            ActivityPeriod::Month => Period::Month,
        },
        oldest_first: filter.oldest_first,
        start: filter.start as usize,
        limit: filter.limit.max(1) as usize,
    }
}

/// Seconds since 1970.
fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).filter(|t| !t.is_empty()).map(str::to_string)
}

#[uniffi::export]
impl Server {
    /// The backups (Jellyfin 10.11 and later), and whether the library scan
    /// runs.
    pub async fn admin_backups(&self) -> Result<BackupList, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let backups = client.backups()?;
            let tasks: Option<Value> = client.scheduled_tasks().ok();
            Ok(BackupList {
                folder: backups.first().and_then(|b| b.path.rsplit_once(['/', '\\']).map(|(dir, _)| dir.to_string())),
                backups: backups.iter().map(info).collect(),
                scan_running: tasks.is_some_and(|tasks| library_scan_running(&tasks)),
            })
        })
        .await
    }

    /// Makes a backup: the database, and the parts asked for.
    pub async fn admin_create_backup(&self, metadata: bool, subtitles: bool, trickplay: bool) -> Result<BackupInfo, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let contents = Contents { database: true, metadata, subtitles, trickplay };
            Ok(info(&client.create_backup(&contents)?))
        })
        .await
    }

    /// The manifest of one archive, read again before a restore (it may be
    /// gone: the server answers 404).
    pub async fn admin_backup_manifest(&self, file_name: String) -> Result<BackupInfo, BloomError> {
        let client = self.signed_in()?;
        run(move || Ok(info(&client.backup_manifest(&file_name)?))).await
    }

    /// Schedules the restore of an archive; the server restarts.
    pub async fn admin_restore_backup(&self, file_name: String) -> Result<(), BloomError> {
        let client = self.signed_in()?;
        run(move || Ok(client.restore_backup(&file_name)?)).await
    }

    /// The whole lines of a log file after byte `from` (0: the end of the
    /// file), at most `max` bytes of them.
    pub async fn admin_log_since(&self, name: String, from: u64, max: u32) -> Result<LogChunk, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            let growth = client.log_since(&name, from, max as usize)?;
            Ok(LogChunk { text: growth.text, end: growth.end, restarted: growth.restarted, cut: growth.cut })
        })
        .await
    }

    /// The lines a log file got after `end`; None when its size did not
    /// move (one small request for a quiet log).
    pub async fn admin_log_follow(&self, name: String, end: u64, max: u32) -> Result<Option<LogChunk>, BloomError> {
        let client = self.signed_in()?;
        run(move || {
            Ok(client
                .log_follow(&name, end, max as usize)?
                .map(|growth| LogChunk { text: growth.text, end: growth.end, restarted: growth.restarted, cut: growth.cut }))
        })
        .await
    }

    /// A page of the activity log, filtered and sorted by the server.
    pub async fn admin_activity_page(&self, filter: ActivityFilter) -> Result<ActivityPage, BloomError> {
        let client = self.signed_in()?;
        let query = query(&filter);
        run(move || {
            let (page, users) = std::thread::scope(|scope| {
                let users = scope.spawn(|| client.users::<Value>());
                let page: Result<Value, _> = client.activity_log(&query.pairs(now()));
                (page, users.join().ok().and_then(Result::ok))
            });
            let page = page?;
            let names: Vec<(String, String)> = users
                .and_then(|u| u.as_array().cloned())
                .unwrap_or_default()
                .iter()
                .filter_map(|u| Some((text(u, "Id")?, text(u, "Name")?)))
                .collect();
            let entries = page
                .get("Items")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .map(|e| {
                    let user = text(e, "UserId").and_then(|id| names.iter().find(|(i, _)| *i == id).map(|(_, n)| n.clone()));
                    ActivityLine {
                        id: e.get("Id").and_then(Value::as_i64).unwrap_or_default(),
                        name: text(e, "Name").unwrap_or_default(),
                        overview: text(e, "ShortOverview"),
                        date: text(e, "Date").unwrap_or_default(),
                        severity: LogSeverity::parse(&text(e, "Severity").unwrap_or_default()),
                        kind: text(e, "Type").unwrap_or_default(),
                        user,
                    }
                })
                .collect();
            let mut users: Vec<String> = names.into_iter().map(|(_, n)| n).collect();
            users.sort_by_key(|n| n.to_lowercase());
            Ok(ActivityPage {
                entries,
                total: page.get("TotalRecordCount").and_then(Value::as_u64).unwrap_or_default() as u32,
                users,
            })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The phone's filter makes the same query as the Mac's.
    #[test]
    fn a_filter_is_the_core_query() {
        let filter = ActivityFilter {
            kind: ActivityKind::SignIn,
            level: ActivityLevel::Error,
            server_only: true,
            user: Some("alex".into()),
            period: ActivityPeriod::Week,
            oldest_first: true,
            start: 100,
            limit: 50,
        };
        let q = query(&filter);
        assert_eq!(q.kind, Kind::SignIn);
        assert_eq!(q.who, Who::Server);
        assert_eq!((q.start, q.limit, q.oldest_first), (100, 50, true));
        let anyone = query(&ActivityFilter { server_only: false, user: Some(String::new()), ..filter });
        assert_eq!(anyone.who, Who::Anyone);
    }
}
