//! The closed sets of the server's text, as enums for Swift (CLAUDE.md:
//! "Compare with the enum, never with the server's text"). Core's enum is
//! the source where it has one; a set a newer server may grow has an
//! `Other` variant that keeps the text, as core's enums do.

use bloom_core::{
    jellyfin::{ItemKind, SegmentKind as CoreSegment},
    syncplay::{
        core::PhaseKind,
        protocol::{GroupState, RepeatMode},
    },
};

/// An enum of the server's names with a variant for a name this build
/// does not know: `parse` and `text` in Rust, `name()` for Swift.
macro_rules! named {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident = $text:literal),* $(,)? }) => {
        $(#[$meta])*
        #[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[$vmeta])* $variant,)*
            /// A name this build does not know, as the server sent it.
            Other { name: String },
        }

        impl $name {
            #[allow(dead_code, reason = "not every set is read from text")]
            pub(crate) fn parse(text: &str) -> Self {
                match text {
                    $($text => Self::$variant,)*
                    other => Self::Other { name: other.to_string() },
                }
            }

            pub(crate) fn text(&self) -> &str {
                match self {
                    $(Self::$variant => $text,)*
                    Self::Other { name } => name,
                }
            }
        }

        #[uniffi::export]
        impl $name {
            /// The server's name.
            pub fn name(&self) -> String {
                self.text().to_string()
            }
        }
    };
}

// ----- Items ----------------------------------------------------------------

named! {
    /// The kind of an item (core's `ItemKind`); `Other` with an empty name
    /// is an item the server has not described yet.
    MediaKind {
        Movie = "Movie",
        Series = "Series",
        Season = "Season",
        Episode = "Episode",
        BoxSet = "BoxSet",
        Playlist = "Playlist",
        Folder = "Folder",
        CollectionFolder = "CollectionFolder",
        UserView = "UserView",
        Video = "Video",
        MusicVideo = "MusicVideo",
        Audio = "Audio",
        MusicAlbum = "MusicAlbum",
        MusicArtist = "MusicArtist",
        Person = "Person",
        Trailer = "Trailer",
    }
}

impl From<&ItemKind> for MediaKind {
    fn from(kind: &ItemKind) -> Self {
        match kind {
            ItemKind::Movie => Self::Movie,
            ItemKind::Series => Self::Series,
            ItemKind::Season => Self::Season,
            ItemKind::Episode => Self::Episode,
            ItemKind::BoxSet => Self::BoxSet,
            ItemKind::Playlist => Self::Playlist,
            ItemKind::Folder => Self::Folder,
            ItemKind::CollectionFolder => Self::CollectionFolder,
            ItemKind::UserView => Self::UserView,
            ItemKind::Video => Self::Video,
            ItemKind::MusicVideo => Self::MusicVideo,
            ItemKind::Audio => Self::Audio,
            ItemKind::MusicAlbum => Self::MusicAlbum,
            ItemKind::MusicArtist => Self::MusicArtist,
            ItemKind::Person => Self::Person,
            ItemKind::Trailer => Self::Trailer,
            ItemKind::Other(name) => Self::Other { name: name.clone() },
        }
    }
}

/// The kind of the server's name ("Movie"), for an item the app kept as
/// text (an index of downloads written by an older build).
#[uniffi::export]
pub fn media_kind(name: String) -> MediaKind {
    MediaKind::from(&ItemKind::from(name.as_str()))
}

named! {
    /// What a library holds (the server's `CollectionType`).
    CollectionType {
        Movies = "movies",
        TvShows = "tvshows",
        Music = "music",
        MusicVideos = "musicvideos",
        HomeVideos = "homevideos",
        BoxSets = "boxsets",
        Books = "books",
        Photos = "photos",
        LiveTv = "livetv",
        Playlists = "playlists",
        Folders = "folders",
        Trailers = "trailers",
    }
}

/// The type of the server's name ("tvshows"), for an item kept as text.
#[uniffi::export]
pub fn collection_type(name: String) -> CollectionType {
    CollectionType::parse(&name)
}

/// A marked part of an item (core's `SegmentKind`).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SegmentKind {
    Intro,
    Outro,
    Recap,
    Preview,
    Commercial,
    /// A kind this build does not know.
    Unknown,
}

impl From<CoreSegment> for SegmentKind {
    fn from(kind: CoreSegment) -> Self {
        match kind {
            CoreSegment::Intro => Self::Intro,
            CoreSegment::Outro => Self::Outro,
            CoreSegment::Recap => Self::Recap,
            CoreSegment::Preview => Self::Preview,
            CoreSegment::Commercial => Self::Commercial,
            CoreSegment::Unknown => Self::Unknown,
        }
    }
}

/// The kind of the server's name ("Intro"), for a segment kept as text.
#[uniffi::export]
pub fn segment_kind(name: String) -> SegmentKind {
    serde_json::from_value::<CoreSegment>(serde_json::Value::String(name)).unwrap_or_default().into()
}

/// The kind of a track the user can choose before playing.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrackKind {
    Audio,
    Subtitle,
}

/// How the server sends an item (the name of the playback reports).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlayMethod {
    /// The file as it is.
    DirectPlay,
    /// A stream the server makes with the video of the file as it is.
    DirectStream,
    Transcode,
}

impl PlayMethod {
    pub(crate) fn text(self) -> &'static str {
        match self {
            Self::DirectPlay => "DirectPlay",
            Self::DirectStream => "DirectStream",
            Self::Transcode => "Transcode",
        }
    }
}

#[uniffi::export]
impl PlayMethod {
    /// The server's name, as the reports send it.
    pub fn name(&self) -> String {
        self.text().to_string()
    }
}

/// A list the user keeps.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ListKind {
    Playlist,
    /// A collection: the server's `BoxSet`.
    Collection,
}

impl ListKind {
    /// The server's item type.
    pub(crate) fn item_type(self) -> &'static str {
        match self {
            Self::Playlist => "Playlist",
            Self::Collection => "BoxSet",
        }
    }
}

/// The order of a library's page (the server's `sortBy`).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LibrarySort {
    Name,
    DateAdded,
    ReleaseDate,
    CommunityRating,
    DatePlayed,
    Random,
}

impl LibrarySort {
    pub(crate) fn field(self) -> &'static str {
        match self {
            Self::Name => "SortName",
            Self::DateAdded => "DateCreated",
            Self::ReleaseDate => "PremiereDate",
            Self::CommunityRating => "CommunityRating",
            Self::DatePlayed => "DatePlayed",
            Self::Random => "Random",
        }
    }
}

/// A filter of a library's page (the server's `filters`).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LibraryFilter {
    Unplayed,
    Played,
    Favorite,
    Resumable,
}

impl LibraryFilter {
    pub(crate) fn field(self) -> &'static str {
        match self {
            Self::Unplayed => "IsUnplayed",
            Self::Played => "IsPlayed",
            Self::Favorite => "IsFavorite",
            Self::Resumable => "IsResumable",
        }
    }
}

named! {
    /// A row of the home page, in the user's order (`homesection0..`).
    HomeSection {
        SmallLibraryTiles = "smalllibrarytiles",
        LibraryButtons = "librarybuttons",
        Resume = "resume",
        NextUp = "nextup",
        LatestMedia = "latestmedia",
    }
}

/// The subtitle the user's settings pick for an item.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SubtitleChoice {
    /// The server named no choice: mpv keeps its own.
    Unknown,
    /// No subtitle.
    Off,
    /// The track by mpv's number.
    Track { number: i64 },
}

impl From<Option<Option<i64>>> for SubtitleChoice {
    fn from(choice: Option<Option<i64>>) -> Self {
        match choice {
            None => Self::Unknown,
            Some(None) => Self::Off,
            Some(Some(number)) => Self::Track { number },
        }
    }
}

// ----- Other devices ---------------------------------------------------------

/// Where items go on another session (the server's `PlayCommand`).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlayCommand {
    PlayNow,
    PlayNext,
    PlayLast,
}

impl From<bloom_core::cast::protocol::PlayCommand> for PlayCommand {
    fn from(command: bloom_core::cast::protocol::PlayCommand) -> Self {
        use bloom_core::cast::protocol::PlayCommand as Core;
        match command {
            Core::PlayNow => Self::PlayNow,
            Core::PlayNext => Self::PlayNext,
            Core::PlayLast => Self::PlayLast,
        }
    }
}

impl From<PlayCommand> for bloom_core::cast::protocol::PlayCommand {
    fn from(command: PlayCommand) -> Self {
        match command {
            PlayCommand::PlayNow => Self::PlayNow,
            PlayCommand::PlayNext => Self::PlayNext,
            PlayCommand::PlayLast => Self::PlayLast,
        }
    }
}

/// A playstate command for another session.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq)]
pub enum PlaystateCommand {
    Pause,
    Unpause,
    PlayPause,
    Stop,
    NextTrack,
    PreviousTrack,
    Seek { secs: f64 },
}

impl PlaystateCommand {
    /// The server's name, and the position of a seek.
    pub(crate) fn request(self) -> (&'static str, Option<f64>) {
        match self {
            Self::Pause => ("Pause", None),
            Self::Unpause => ("Unpause", None),
            Self::PlayPause => ("PlayPause", None),
            Self::Stop => ("Stop", None),
            Self::NextTrack => ("NextTrack", None),
            Self::PreviousTrack => ("PreviousTrack", None),
            Self::Seek { secs } => ("Seek", Some(secs)),
        }
    }
}

named! {
    /// Where Jellyfin Enhanced hides a title (its `HideScope`). The library
    /// and search are the plugin's surfaces: a scope of a surface's name
    /// hides the title there.
    HideScope {
        Global = "global",
        HomeSections = "homesections",
        ContinueWatching = "continuewatching",
        NextUp = "nextup",
        Library = "library",
        Search = "search",
    }
}

impl HideScope {
    /// The plugin's text of a file (any case; empty is global).
    pub(crate) fn of_file(text: &str) -> Self {
        match text.to_lowercase() {
            text if text.is_empty() => Self::Global,
            text => Self::parse(&text),
        }
    }
}

named! {
    /// What a Seerr title is.
    SeerrMediaType {
        Movie = "movie",
        Tv = "tv",
    }
}

// ----- SyncPlay ---------------------------------------------------------------

named! {
    /// A user's SyncPlay access (the server's `SyncPlayAccess`).
    SyncPlayAccess {
        CreateAndJoinGroups = "CreateAndJoinGroups",
        JoinGroups = "JoinGroups",
        /// No SyncPlay: the server's "None".
        Off = "None",
    }
}

/// The state of a SyncPlay group (core's `GroupState`).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SyncGroupState {
    Idle,
    Waiting,
    Paused,
    Playing,
}

impl From<GroupState> for SyncGroupState {
    fn from(state: GroupState) -> Self {
        match state {
            GroupState::Idle => Self::Idle,
            GroupState::Waiting => Self::Waiting,
            GroupState::Paused => Self::Paused,
            GroupState::Playing => Self::Playing,
        }
    }
}

named! {
    /// Why a group changed its state (the server's `PlaybackRequestType`).
    SyncReason {
        Play = "Play",
        SetPlaylistItem = "SetPlaylistItem",
        RemoveFromPlaylist = "RemoveFromPlaylist",
        MovePlaylistItem = "MovePlaylistItem",
        Queue = "Queue",
        Unpause = "Unpause",
        Pause = "Pause",
        Stop = "Stop",
        Seek = "Seek",
        Buffer = "Buffer",
        Ready = "Ready",
        NextItem = "NextItem",
        PreviousItem = "PreviousItem",
        SetRepeatMode = "SetRepeatMode",
        SetShuffleMode = "SetShuffleMode",
        Ping = "Ping",
        IgnoreWait = "IgnoreWait",
    }
}

named! {
    /// Why the server refused a SyncPlay request.
    SyncDenial {
        GroupDoesNotExist = "GroupDoesNotExist",
        LibraryAccessDenied = "LibraryAccessDenied",
        CreateGroupDenied = "CreateGroupDenied",
        JoinGroupDenied = "JoinGroupDenied",
        SyncPlayIsDisabled = "SyncPlayIsDisabled",
    }
}

/// The repeat mode of a group's queue (core's `RepeatMode`).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SyncRepeat {
    Off,
    One,
    All,
}

impl From<RepeatMode> for SyncRepeat {
    fn from(mode: RepeatMode) -> Self {
        match mode {
            RepeatMode::RepeatNone => Self::Off,
            RepeatMode::RepeatOne => Self::One,
            RepeatMode::RepeatAll => Self::All,
        }
    }
}

/// What this player is in the middle of (core's `PhaseKind`).
#[derive(uniffi::Enum, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SyncPhase {
    #[default]
    Idle,
    Loading,
    Parked,
    Seeking,
    CatchingUp,
    Starting,
    Playing,
    Advancing,
}

impl From<PhaseKind> for SyncPhase {
    fn from(phase: PhaseKind) -> Self {
        match phase {
            PhaseKind::Idle => Self::Idle,
            PhaseKind::Loading => Self::Loading,
            PhaseKind::Parked => Self::Parked,
            PhaseKind::Seeking => Self::Seeking,
            PhaseKind::CatchingUp => Self::CatchingUp,
            PhaseKind::Starting => Self::Starting,
            PhaseKind::Playing => Self::Playing,
            PhaseKind::Advancing => Self::Advancing,
        }
    }
}

impl SyncPhase {
    fn core(self) -> PhaseKind {
        match self {
            Self::Idle => PhaseKind::Idle,
            Self::Loading => PhaseKind::Loading,
            Self::Parked => PhaseKind::Parked,
            Self::Seeking => PhaseKind::Seeking,
            Self::CatchingUp => PhaseKind::CatchingUp,
            Self::Starting => PhaseKind::Starting,
            Self::Playing => PhaseKind::Playing,
            Self::Advancing => PhaseKind::Advancing,
        }
    }
}

#[uniffi::export]
impl SyncPhase {
    /// Core's short text ("catching-up"), for the statistics line.
    pub fn name(&self) -> String {
        self.core().name().to_string()
    }
}

// ----- The dashboard ------------------------------------------------------------

named! {
    /// What a scheduled task does now (the server's `TaskState`).
    TaskState {
        Idle = "Idle",
        Running = "Running",
        Cancelling = "Cancelling",
    }
}

named! {
    /// How a task's last run ended (the server's `TaskCompletionStatus`).
    TaskResult {
        Completed = "Completed",
        Failed = "Failed",
        Cancelled = "Cancelled",
        Aborted = "Aborted",
    }
}

named! {
    /// The state of a plugin (the server's `PluginStatus`).
    PluginStatus {
        Active = "Active",
        Restart = "Restart",
        Deleted = "Deleted",
        Superceded = "Superceded",
        Malfunctioned = "Malfunctioned",
        NotSupported = "NotSupported",
        Disabled = "Disabled",
    }
}

named! {
    /// The weight of an activity entry (the server's `LogLevel`).
    LogSeverity {
        Information = "Information",
        Warning = "Warning",
        Error = "Error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_item_kind_keeps_its_name_both_ways() {
        for name in ["Movie", "Series", "Season", "Episode", "BoxSet", "Playlist", "Folder", "CollectionFolder", "UserView", "Video", "MusicVideo", "Audio", "MusicAlbum", "MusicArtist", "Person", "Trailer"] {
            let kind = media_kind(name.into());
            assert!(!matches!(kind, MediaKind::Other { .. }), "{name} has no variant");
            assert_eq!(kind.name(), name);
            assert_eq!(kind, MediaKind::from(&ItemKind::from(name)), "core and the text disagree on {name}");
        }
        assert_eq!(media_kind("AudioBook".into()), MediaKind::Other { name: "AudioBook".into() });
        assert_eq!(media_kind(String::new()), MediaKind::Other { name: String::new() });
    }

    #[test]
    fn names_a_newer_server_sends_are_kept() {
        assert_eq!(collection_type("tvshows".into()), CollectionType::TvShows);
        assert_eq!(collection_type("mixed".into()).name(), "mixed");
        assert_eq!(TaskState::parse("Idle"), TaskState::Idle);
        assert_eq!(TaskResult::parse("Failed"), TaskResult::Failed);
        assert_eq!(PluginStatus::parse("Malfunctioned"), PluginStatus::Malfunctioned);
        assert_eq!(PluginStatus::parse("Broken").name(), "Broken");
        assert_eq!(LogSeverity::parse("Critical"), LogSeverity::Other { name: "Critical".into() });
        assert_eq!(SyncPlayAccess::parse("None"), SyncPlayAccess::Off);
        assert_eq!(SyncPlayAccess::Off.name(), "None");
        assert_eq!(SyncDenial::parse("JoinGroupDenied"), SyncDenial::JoinGroupDenied);
        assert_eq!(SyncReason::parse("Buffer"), SyncReason::Buffer);
        assert_eq!(SeerrMediaType::parse("tv"), SeerrMediaType::Tv);
        assert_eq!(HomeSection::parse("latestmedia"), HomeSection::LatestMedia);
        assert_eq!(HomeSection::parse("resumeaudio").name(), "resumeaudio");
    }

    #[test]
    fn a_hide_scope_reads_as_the_plugin_writes_it() {
        assert_eq!(HideScope::of_file(""), HideScope::Global);
        assert_eq!(HideScope::of_file("NextUp"), HideScope::NextUp);
        assert_eq!(HideScope::of_file("homesections"), HideScope::HomeSections);
        assert_eq!(HideScope::of_file("someday").name(), "someday");
    }

    #[test]
    fn a_segment_kind_comes_from_core() {
        assert_eq!(segment_kind("Intro".into()), SegmentKind::Intro);
        assert_eq!(segment_kind("Outro".into()), SegmentKind::Outro);
        assert_eq!(segment_kind("Commercial".into()), SegmentKind::Commercial);
        assert_eq!(segment_kind("Recaps".into()), SegmentKind::Unknown);
        assert_eq!(SegmentKind::from(CoreSegment::Preview), SegmentKind::Preview);
    }

    #[test]
    fn a_subtitle_choice_has_three_cases() {
        assert_eq!(SubtitleChoice::from(None), SubtitleChoice::Unknown);
        assert_eq!(SubtitleChoice::from(Some(None)), SubtitleChoice::Off);
        assert_eq!(SubtitleChoice::from(Some(Some(3))), SubtitleChoice::Track { number: 3 });
    }

    #[test]
    fn commands_keep_the_server_names() {
        assert_eq!(PlaystateCommand::Seek { secs: 12. }.request(), ("Seek", Some(12.)));
        assert_eq!(PlaystateCommand::PlayPause.request(), ("PlayPause", None));
        let core: bloom_core::cast::protocol::PlayCommand = PlayCommand::PlayLast.into();
        assert_eq!(core.name(), "PlayLast");
        assert_eq!(PlayCommand::from(core), PlayCommand::PlayLast);
        assert_eq!(ListKind::Collection.item_type(), "BoxSet");
        assert_eq!(LibrarySort::ReleaseDate.field(), "PremiereDate");
        assert_eq!(LibraryFilter::Favorite.field(), "IsFavorite");
        assert_eq!(PlayMethod::DirectStream.text(), "DirectStream");
    }

    #[test]
    fn sync_enums_follow_core() {
        assert_eq!(SyncGroupState::from(GroupState::Waiting), SyncGroupState::Waiting);
        assert_eq!(SyncRepeat::from(RepeatMode::RepeatOne), SyncRepeat::One);
        assert_eq!(SyncPhase::from(PhaseKind::CatchingUp).name(), "catching-up");
        assert_eq!(SyncPhase::default().name(), "idle");
    }
}
