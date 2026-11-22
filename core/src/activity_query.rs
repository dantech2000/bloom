// SPDX-License-Identifier: AGPL-3.0-or-later
//! A page of the server's activity log, filtered and sorted by the server
//! (Jellyfin 12.0, "Sorting and filtering on the Activity page"). The text
//! filters of `GET /System/ActivityLog/Entries` (`type`, `username`) match
//! a part of the value (SQL `LIKE '%x%'`), `severity` is exact, `minDate`
//! is the earliest date. A server before 12.0 knows `minDate`, `hasUserId`,
//! `startIndex` and `limit` only and ignores the rest.

/// Entries on one page.
pub const PAGE: usize = 100;

/// What kind of event a filter keeps: the part of the server's type
/// names the kind shares ("Session" for SessionStarted and SessionEnded).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Kind {
    #[default]
    All,
    Playback,
    SignIn,
    Session,
    User,
    Plugin,
    Task,
    Subtitle,
}

impl Kind {
    pub const ALL: [Kind; 8] =
        [Kind::All, Kind::Playback, Kind::SignIn, Kind::Session, Kind::User, Kind::Plugin, Kind::Task, Kind::Subtitle];

    pub fn label(self) -> &'static str {
        match self {
            Kind::All => "Every kind",
            Kind::Playback => "Playback",
            Kind::SignIn => "Sign-ins",
            Kind::Session => "Sessions",
            Kind::User => "Users",
            Kind::Plugin => "Plugins",
            Kind::Task => "Tasks",
            Kind::Subtitle => "Subtitles",
        }
    }

    /// The text of the `type` filter.
    fn part(self) -> Option<&'static str> {
        Some(match self {
            Kind::All => return None,
            Kind::Playback => "Playback",
            Kind::SignIn => "Authentication",
            Kind::Session => "Session",
            Kind::User => "User",
            Kind::Plugin => "Plugin",
            Kind::Task => "Task",
            Kind::Subtitle => "Subtitle",
        })
    }
}

/// The level a filter keeps (exact, as the server compares it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Level {
    #[default]
    All,
    Information,
    Warning,
    Error,
}

impl Level {
    pub const ALL: [Level; 4] = [Level::All, Level::Information, Level::Warning, Level::Error];

    pub fn label(self) -> &'static str {
        match self {
            Level::All => "Every level",
            Level::Information => "Information",
            Level::Warning => "Warnings",
            Level::Error => "Errors",
        }
    }

    fn name(self) -> Option<&'static str> {
        match self {
            Level::All => None,
            Level::Information => Some("Information"),
            Level::Warning => Some("Warning"),
            Level::Error => Some("Error"),
        }
    }
}

/// Whose entries a filter keeps.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum Who {
    #[default]
    Anyone,
    /// The entries with no user: what the server did by itself.
    Server,
    /// The entries of a user, by name (the server matches a part of it).
    User(String),
}

impl Who {
    pub fn label(&self) -> String {
        match self {
            Who::Anyone => "Every user".to_string(),
            Who::Server => "The server".to_string(),
            Who::User(name) => name.clone(),
        }
    }
}

/// How far back a filter looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Period {
    #[default]
    Any,
    Day,
    Week,
    Month,
}

impl Period {
    pub const ALL: [Period; 4] = [Period::Any, Period::Day, Period::Week, Period::Month];

    pub fn label(self) -> &'static str {
        match self {
            Period::Any => "Any time",
            Period::Day => "Last 24 hours",
            Period::Week => "Last 7 days",
            Period::Month => "Last 30 days",
        }
    }

    fn seconds(self) -> Option<i64> {
        match self {
            Period::Any => None,
            Period::Day => Some(86_400),
            Period::Week => Some(7 * 86_400),
            Period::Month => Some(30 * 86_400),
        }
    }
}

/// One page of the activity log as the user asked for it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Query {
    pub kind: Kind,
    pub level: Level,
    pub who: Who,
    pub period: Period,
    pub oldest_first: bool,
    /// The first entry of the page.
    pub start: usize,
    pub limit: usize,
}

impl Default for Query {
    fn default() -> Self {
        Self {
            kind: Kind::All,
            level: Level::All,
            who: Who::Anyone,
            period: Period::Any,
            oldest_first: false,
            start: 0,
            limit: PAGE,
        }
    }
}

impl Query {
    /// The parameters of the request; `now` is seconds since 1970, for the
    /// earliest date of a period.
    pub fn pairs(&self, now: i64) -> Vec<(&'static str, String)> {
        let mut pairs = vec![("startIndex", self.start.to_string()), ("limit", self.limit.to_string())];
        if let Some(part) = self.kind.part() {
            pairs.push(("type", part.to_string()));
        }
        if let Some(level) = self.level.name() {
            pairs.push(("severity", level.to_string()));
        }
        match &self.who {
            Who::Anyone => {}
            Who::Server => pairs.push(("hasUserId", "false".to_string())),
            Who::User(name) => pairs.push(("username", name.clone())),
        }
        if let Some(back) = self.period.seconds()
            && let Ok(since) = jiff::Timestamp::from_second(now - back)
        {
            pairs.push(("minDate", since.to_string()));
        }
        pairs.push(("sortBy", "DateCreated".to_string()));
        pairs.push(("sortOrder", if self.oldest_first { "Ascending" } else { "Descending" }.to_string()));
        pairs
    }

    /// Whether any filter is on (the sort and the page are not filters).
    pub fn filtered(&self) -> bool {
        self.kind != Kind::All || self.level != Level::All || self.who != Who::Anyone || self.period != Period::Any
    }

    /// The same filters from the first page: a change of a filter or of the
    /// sort starts there.
    pub fn first_page(mut self) -> Self {
        self.start = 0;
        self
    }

    /// The start of the page before, if there is one.
    pub fn previous(&self) -> Option<usize> {
        (self.start > 0).then(|| self.start.saturating_sub(self.limit))
    }

    /// The start of the page after, if the server has more.
    pub fn next(&self, total: usize) -> Option<usize> {
        let next = self.start + self.limit;
        (next < total).then_some(next)
    }

    /// "1–100 of 394", for the pager; "None" when nothing matched.
    pub fn range_label(&self, shown: usize, total: usize) -> String {
        if shown == 0 {
            return format!("0 of {total}");
        }
        format!("{}\u{2013}{} of {total}", self.start + 1, self.start + shown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value<'a>(pairs: &'a [(&str, String)], key: &str) -> Option<&'a str> {
        pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str())
    }

    #[test]
    fn the_default_is_the_newest_page() {
        let pairs = Query::default().pairs(0);
        assert_eq!(
            pairs,
            vec![
                ("startIndex", "0".to_string()),
                ("limit", "100".to_string()),
                ("sortBy", "DateCreated".to_string()),
                ("sortOrder", "Descending".to_string()),
            ]
        );
        assert!(!Query::default().filtered());
    }

    #[test]
    fn each_filter_has_its_parameter() {
        let query = Query {
            kind: Kind::Session,
            level: Level::Error,
            who: Who::User("alex".into()),
            period: Period::Day,
            oldest_first: true,
            start: 200,
            limit: 100,
        };
        // 2026-11-21T09:00:00Z
        let pairs = query.pairs(1_795_251_600);
        assert_eq!(value(&pairs, "type"), Some("Session"));
        assert_eq!(value(&pairs, "severity"), Some("Error"));
        assert_eq!(value(&pairs, "username"), Some("alex"));
        assert_eq!(value(&pairs, "minDate"), Some("2026-11-20T09:00:00Z"));
        assert_eq!(value(&pairs, "sortOrder"), Some("Ascending"));
        assert_eq!(value(&pairs, "startIndex"), Some("200"));
        assert_eq!(value(&pairs, "hasUserId"), None);
        assert!(query.filtered());
        let server = Query { who: Who::Server, ..Query::default() }.pairs(0);
        assert_eq!(value(&server, "hasUserId"), Some("false"));
        assert_eq!(value(&server, "username"), None);
        assert_eq!(value(&Query { kind: Kind::SignIn, ..Query::default() }.pairs(0), "type"), Some("Authentication"));
    }

    #[test]
    fn the_pager_steps_by_a_page() {
        let first = Query::default();
        assert_eq!(first.previous(), None);
        assert_eq!(first.next(394), Some(100));
        assert_eq!(first.range_label(100, 394), "1\u{2013}100 of 394");
        let last = Query { start: 300, ..Query::default() };
        assert_eq!(last.next(394), None);
        assert_eq!(last.previous(), Some(200));
        assert_eq!(last.range_label(94, 394), "301\u{2013}394 of 394");
        assert_eq!(Query { start: 50, ..Query::default() }.previous(), Some(0));
        assert_eq!(last.clone().first_page().start, 0);
        assert_eq!(first.range_label(0, 0), "0 of 0");
    }
}
