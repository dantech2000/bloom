// SPDX-License-Identifier: AGPL-3.0-or-later
//! The backups of the server (Jellyfin 10.11 and later): `GET /Backup`
//! lists them, `POST /Backup/Create` makes one, `GET /Backup/Manifest`
//! reads one by its file name, and `POST /Backup/Restore` schedules a
//! restore and restarts the server. The list has no size of an archive:
//! the server does not send one.

use serde::{Deserialize, Serialize};

/// What an archive holds. The database is always in a backup the app makes:
/// an archive without it restores nothing of the users or the library.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase", default)]
pub struct Contents {
    pub database: bool,
    pub metadata: bool,
    pub subtitles: bool,
    pub trickplay: bool,
}

impl Contents {
    /// The choice a new backup starts with: the database alone, as
    /// Jellyfin Web offers it.
    pub fn new_backup() -> Self {
        Self { database: true, ..Self::default() }
    }

    /// The parts, in words: "Database, Metadata".
    pub fn parts(&self) -> Vec<&'static str> {
        [
            (self.database, "Database"),
            (self.metadata, "Metadata"),
            (self.subtitles, "Subtitles"),
            (self.trickplay, "Trickplay"),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect()
    }

    pub fn summary(&self) -> String {
        let parts = self.parts();
        if parts.is_empty() { "Settings only".to_string() } else { parts.join(", ") }
    }
}

/// One backup as the server lists it.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase", default)]
pub struct Backup {
    pub server_version: String,
    pub backup_engine_version: String,
    pub date_created: String,
    /// The archive's path on the server.
    pub path: String,
    pub options: Contents,
}

impl Backup {
    /// The archive's file name, which the manifest and the restore take
    /// (the server looks for it in its backup folder). A Windows server
    /// writes back slashes.
    pub fn file_name(&self) -> &str {
        self.path.rsplit(['/', '\\']).next().unwrap_or(&self.path)
    }

    /// "12.2.0" for the "12.2.0.0" the server writes.
    pub fn version(&self) -> &str {
        self.server_version.strip_suffix(".0").filter(|v| v.matches('.').count() >= 2).unwrap_or(&self.server_version)
    }

    /// Seconds since 1970 of the date the backup was made.
    pub fn created(&self) -> Option<i64> {
        self.date_created
            .parse::<jiff::Timestamp>()
            .or_else(|_| format!("{}Z", self.date_created).parse())
            .ok()
            .map(|t| t.as_second())
    }
}

/// The body of a new backup.
pub fn create_body(contents: &Contents) -> Contents {
    Contents { database: true, ..*contents }
}

/// The body of a restore.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Restore<'a> {
    pub archive_file_name: &'a str,
}

/// The backups, the newest first.
pub fn newest_first(mut backups: Vec<Backup>) -> Vec<Backup> {
    backups.sort_by(|a, b| b.created().cmp(&a.created()).then_with(|| b.path.cmp(&a.path)));
    backups
}

/// Whether the library scan runs, by the scheduled tasks of the server:
/// Jellyfin Web warns before a backup in that case, since the archive then
/// holds a library half scanned.
pub fn library_scan_running(tasks: &serde_json::Value) -> bool {
    tasks.as_array().is_some_and(|tasks| {
        tasks.iter().any(|task| {
            task.get("Key").and_then(|k| k.as_str()) == Some("RefreshLibrary")
                && task.get("State").and_then(|s| s.as_str()) == Some("Running")
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The answer of a 12.2 server, as the container gave it.
    const LISTED: &str = r#"[{"ServerVersion":"12.2.0.0","BackupEngineVersion":"0.2.0",
        "DateCreated":"2026-11-21T09:12:33.2417447+00:00",
        "Path":"/config/data/backups/jellyfin-backup-20261121091233.zip",
        "Options":{"Metadata":false,"Trickplay":false,"Subtitles":false,"Database":true}},
        {"ServerVersion":"10.11.2","BackupEngineVersion":"0.2.0","DateCreated":"2026-11-25T01:00:00Z",
        "Path":"C:\\ProgramData\\Jellyfin\\backups\\jellyfin-backup-20261125010000.zip",
        "Options":{"Metadata":true,"Trickplay":true,"Subtitles":true,"Database":true}}]"#;

    #[test]
    fn a_list_reads_and_sorts_newest_first() {
        let backups: Vec<Backup> = serde_json::from_str(LISTED).unwrap();
        let backups = newest_first(backups);
        assert_eq!(backups[0].file_name(), "jellyfin-backup-20261125010000.zip");
        assert_eq!(backups[1].file_name(), "jellyfin-backup-20261121091233.zip");
        assert_eq!(backups[1].version(), "12.2.0");
        assert_eq!(backups[0].version(), "10.11.2");
        assert_eq!(backups[1].options.summary(), "Database");
        assert_eq!(backups[0].options.summary(), "Database, Metadata, Subtitles, Trickplay");
        assert_eq!(backups[1].created(), Some(1_795_252_353));
    }

    #[test]
    fn a_new_backup_always_holds_the_database() {
        assert_eq!(Contents::new_backup().parts(), ["Database"]);
        let asked = Contents { database: false, metadata: true, ..Contents::default() };
        let body = serde_json::to_value(create_body(&asked)).unwrap();
        assert_eq!(body, json!({"Database": true, "Metadata": true, "Subtitles": false, "Trickplay": false}));
        assert_eq!(Contents::default().summary(), "Settings only");
        let restore = serde_json::to_value(Restore { archive_file_name: "a.zip" }).unwrap();
        assert_eq!(restore, json!({"ArchiveFileName": "a.zip"}));
    }

    #[test]
    fn a_running_scan_is_seen_in_the_tasks() {
        let idle = json!([{"Key": "RefreshLibrary", "State": "Idle"}, {"Key": "CleanLogFiles", "State": "Running"}]);
        assert!(!library_scan_running(&idle));
        let running = json!([{"Key": "RefreshLibrary", "State": "Running"}]);
        assert!(library_scan_running(&running));
        assert!(!library_scan_running(&json!({})));
    }
}
