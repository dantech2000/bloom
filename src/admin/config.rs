// SPDX-License-Identifier: AGPL-3.0-or-later
//! The configuration pages of the server: General, Branding, the library
//! and playback options, Networking. They are one page with different
//! contents. A page is a list of fields; a field names a place in a
//! configuration object of the server and how to edit it.
//!
//! The server gives a configuration as one object and takes the whole
//! object back. The page keeps the object as it came and a copy the user
//! edits. A save posts the copy, so every key the page does not show goes
//! back as it was.

use std::rc::Rc;

use anyhow::Result;
use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, ParentElement as _, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled, Window, div, px, rgba,
};
use gpui_kit::IntoElement as _;
use serde_json::{Value, json};

use super::{
    ButtonKind, Confirm, Section, button,
    dialogs::{Field as PromptField, Prompt},
};
use crate::{
    app::{Bloom, Page},
    jellyfin::Client,
    settings::{Choice, checkbox, field, group, select},
    ui::{glass::glass, theme::UiTheme},
    views::cards::icon,
};

/// How a field is shown and edited.
#[derive(Clone)]
pub enum Kind {
    Bool,
    /// A checkbox that puts a value into the list of the field, or out.
    InList(&'static str),
    /// A number. The page shows the stored number divided by `scale`
    /// (bits a second as megabits), with the unit after it.
    Number { min: f64, max: f64, unit: &'static str, scale: f64, whole: bool },
    Text,
    /// A folder or a file on the server.
    Path,
    /// One of a set of values, each with its label.
    Choice(Vec<(Value, String)>),
    /// Lines of text; in the form, separated by commas.
    List,
    /// Text of many lines, edited in a large dialog; `mono` is for code.
    Multiline { mono: bool },
}

#[derive(Clone)]
pub struct Field {
    /// Which configuration object of the page the field is in.
    source: usize,
    /// The keys from the object down to the value.
    path: Vec<&'static str>,
    label: &'static str,
    help: &'static str,
    kind: Kind,
}

/// The contents of one page.
pub struct Spec {
    groups: Vec<(&'static str, Vec<Field>)>,
    /// A warning the user must confirm before a save, on a page where a
    /// wrong value breaks access or playback.
    warning: Option<&'static str>,
}

/// Lists the fields choose from; loaded with the page.
#[derive(Clone, Default)]
pub struct Options {
    /// Value and name of each display language.
    ui_languages: Vec<(String, String)>,
    metadata_languages: Vec<(String, String)>,
    countries: Vec<(String, String)>,
    users: Vec<(String, String)>,
}

/// What a save did: the objects the server took (source and body), and
/// why it refused the others.
#[derive(Default)]
struct Sent {
    taken: Vec<(usize, Value)>,
    refused: Vec<anyhow::Error>,
}

pub struct Data {
    /// The objects as the server gave them, in the order of the sources.
    values: Vec<Value>,
    /// The copies the user edits.
    edited: Vec<Value>,
    options: Options,
    /// The address of the splash screen image, for the Branding page.
    splash_preview: String,
}

/// The configuration objects of a page: "" is the main one, any other is a
/// named one (`/System/Configuration/<name>`). A field names one by its
/// place in this list.
fn sources(section: Section) -> Vec<&'static str> {
    match section {
        Section::General
        | Section::LibraryMetadata
        | Section::Resume
        | Section::Streaming
        | Section::Trickplay => vec![""],
        Section::Branding => vec!["branding"],
        Section::LibraryDisplay => vec!["", "metadata"],
        Section::Nfo => vec!["xbmcmetadata"],
        Section::Transcoding => vec!["encoding"],
        Section::Networking => vec!["network"],
        _ => Vec::new(),
    }
}

fn endpoint(source: &str) -> String {
    match source {
        "" => "/System/Configuration".to_string(),
        name => format!("/System/Configuration/{name}"),
    }
}

fn at<'a>(value: &'a Value, path: &[&str]) -> &'a Value {
    path.iter().fold(value, |value, key| &value[*key])
}

fn put(value: &mut Value, path: &[&str], new: Value) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut place = value;
    for key in parents {
        if !place[*key].is_object() {
            place[*key] = json!({});
        }
        place = &mut place[*key];
    }
    place[*last] = new;
}

fn text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn list(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| items.iter().map(text).collect())
        .unwrap_or_default()
}

pub fn load(client: &Client, section: Section) -> Result<Data> {
    let values = sources(section)
        .into_iter()
        .map(|source| client.get::<Value>(&endpoint(source), &[]))
        .collect::<Result<Vec<_>>>()?;
    // The lists are a help, not a need: a field shows its raw value
    // without them.
    let pairs = |path: &str, value: &str, name: &str| -> Vec<(String, String)> {
        client
            .get::<Vec<Value>>(path, &[])
            .unwrap_or_default()
            .iter()
            .map(|entry| (text(&entry[value]), text(&entry[name])))
            .filter(|(value, _)| !value.is_empty())
            .collect()
    };
    let mut options = Options::default();
    match section {
        Section::General => options.ui_languages = pairs("/Localization/Options", "Value", "Name"),
        Section::LibraryMetadata => {
            options.metadata_languages =
                pairs("/Localization/Cultures", "TwoLetterISOLanguageName", "DisplayName");
            options.countries =
                pairs("/Localization/Countries", "TwoLetterISORegionName", "DisplayName");
        }
        Section::Nfo => options.users = pairs("/Users", "Id", "Name"),
        _ => {}
    }
    let splash_preview = match section {
        Section::Branding => client.url("/Branding/Splashscreen", &[]),
        _ => String::new(),
    };
    Ok(Data { edited: values.clone(), values, options, splash_preview })
}

// ----- the pages --------------------------------------------------------------

fn item(source: usize, path: &[&'static str], label: &'static str, help: &'static str, kind: Kind) -> Field {
    Field { source, path: path.to_vec(), label, help, kind }
}

fn whole(min: f64, max: f64, unit: &'static str) -> Kind {
    Kind::Number { min, max, unit, scale: 1., whole: true }
}

fn choice(options: &[(&str, &str)]) -> Kind {
    Kind::Choice(options.iter().map(|(value, label)| (json!(value), label.to_string())).collect())
}

fn named(options: &[(String, String)], first: Option<(&str, &str)>) -> Kind {
    Kind::Choice(
        first
            .into_iter()
            .map(|(value, label)| (json!(value), label.to_string()))
            .chain(options.iter().map(|(value, label)| (json!(value), label.clone())))
            .collect(),
    )
}

/// The fields of a page. The labels and the option values follow the web
/// dashboard.
pub fn spec(section: Section, options: &Options) -> Spec {
    use Kind::{Bool, InList, List, Path, Text};
    let mut warning = None;
    let groups = match section {
        Section::General => vec![
            (
                "Server",
                vec![
                    item(0, &["ServerName"], "Server name", "The name clients show for this server.", Text),
                    item(
                        0,
                        &["UICulture"],
                        "Preferred display language",
                        "The language of the dashboard and of messages of the server.",
                        named(&options.ui_languages, None),
                    ),
                ],
            ),
            (
                "Paths",
                vec![
                    item(0, &["CachePath"], "Cache path", "A folder for cache files such as images. Empty uses the default of the server.", Path),
                    item(0, &["MetadataPath"], "Metadata path", "A folder for downloaded artwork and metadata. Empty uses the default.", Path),
                ],
            ),
            (
                "Quick Connect",
                vec![item(0, &["QuickConnectAvailable"], "Enable Quick Connect on this server", "", Bool)],
            ),
            (
                "Performance",
                vec![
                    item(0, &["LibraryScanFanoutConcurrency"], "Parallel library scan tasks limit", "0 lets the server choose from its core count. A high number can cause trouble with network file systems.", whole(0., 256., "")),
                    item(0, &["ParallelImageEncodingLimit"], "Parallel image encoding limit", "0 lets the server choose from its core count.", whole(0., 256., "")),
                ],
            ),
        ],
        Section::Branding => vec![
            (
                "Sign-in page",
                vec![item(0, &["LoginDisclaimer"], "Login disclaimer", "A message at the bottom of the sign-in page. It can have several lines.", Kind::Multiline { mono: false })],
            ),
            (
                "Splash screen",
                vec![item(0, &["SplashscreenEnabled"], "Enable the splash screen", "The image clients show while they load.", Bool)],
            ),
            (
                "Custom CSS",
                vec![item(0, &["CustomCss"], "Custom CSS", "Styles the web client of the server loads. A mistake here changes how the web client looks.", Kind::Multiline { mono: true })],
            ),
        ],
        Section::LibraryDisplay => vec![(
            "Display",
            vec![
                item(1, &["UseFileCreationTimeForDateAdded"], "Date added is the creation date of the file", "Off uses the date of the scan into the library.", Bool),
                item(0, &["EnableFolderView"], "Display a folder view to show plain media folders", "", Bool),
                item(0, &["DisplaySpecialsWithinSeasons"], "Display specials within seasons they aired in", "", Bool),
                item(0, &["EnableGroupingMoviesIntoCollections"], "Group movies into collections", "A movie in a collection shows as one entry in the movie lists.", Bool),
                item(0, &["EnableGroupingShowsIntoCollections"], "Group shows into collections", "", Bool),
                item(0, &["EnableExternalContentInSuggestions"], "Enable external content in suggestions", "Internet trailers and live TV programs in suggested content.", Bool),
            ],
        )],
        Section::LibraryMetadata => vec![
            (
                "Preferred metadata language",
                vec![
                    item(0, &["PreferredMetadataLanguage"], "Language", "The default for new libraries.", named(&options.metadata_languages, Some(("", "None")))),
                    item(0, &["MetadataCountryCode"], "Country/Region", "", named(&options.countries, Some(("", "None")))),
                ],
            ),
            (
                "Chapter images",
                vec![
                    item(0, &["DummyChapterDuration"], "Interval of dummy chapters", "Seconds between chapters made for media without any. 0 makes none.", whole(0., 86_400., "s")),
                    item(
                        0,
                        &["ChapterImageResolution"],
                        "Resolution",
                        "The resolution of the extracted chapter images.",
                        choice(&[
                            ("MatchSource", "Match source"),
                            ("P2160", "2160p"),
                            ("P1440", "1440p"),
                            ("P1080", "1080p"),
                            ("P720", "720p"),
                            ("P480", "480p"),
                            ("P360", "360p"),
                            ("P240", "240p"),
                            ("P144", "144p"),
                        ]),
                    ),
                ],
            ),
        ],
        Section::Nfo => vec![(
            "NFO files",
            vec![
                item(0, &["UserId"], "Save user watch data to NFO files for", "Other programs can then read the watched state.", named(&options.users, Some(("", "None")))),
                item(0, &["ReleaseDateFormat"], "Release date format", "Every date in an NFO file is read and written in this format.", choice(&[("yyyy-MM-dd", "yyyy-MM-dd")])),
                item(0, &["SaveImagePathsInNfo"], "Save image paths within NFO files", "For image files whose names do not follow the rules of Kodi.", Bool),
                item(0, &["EnablePathSubstitution"], "Enable path substitution", "Image paths use the path substitution settings of the server.", Bool),
                item(0, &["EnableExtraThumbsDuplication"], "Copy extrafanart to the extrathumbs field", "Downloaded images go to both fields, for Kodi skins.", Bool),
            ],
        )],
        Section::Resume => vec![(
            "Resume",
            vec![
                item(0, &["MinResumePct"], "Minimum resume percentage", "A title counts as not played when stopped before this point.", whole(0., 100., "%")),
                item(0, &["MaxResumePct"], "Maximum resume percentage", "A title counts as played when stopped after this point.", whole(1., 100., "%")),
                item(0, &["MinAudiobookResume"], "Minimum audiobook resume", "Minutes. A title counts as not played when stopped before this time.", whole(0., 100., "min")),
                item(0, &["MaxAudiobookResume"], "Audiobook remaining minutes to resume", "A title counts as played when less than this is left.", whole(1., 100., "min")),
                item(0, &["MinResumeDurationSeconds"], "Minimum resume duration", "The shortest video that keeps its place and can be resumed.", whole(0., 86_400., "s")),
            ],
        )],
        Section::Streaming => vec![(
            "Streaming",
            vec![item(
                0,
                &["RemoteClientBitrateLimit"],
                "Internet streaming bitrate limit",
                "For every device outside the network. 0 is no limit. A limit below the bitrate of a video makes the server transcode it.",
                Kind::Number { min: 0., max: 10_000., unit: "Mbps", scale: 1_000_000., whole: false },
            )],
        )],
        Section::Trickplay => {
            let o = |key: &'static str| -> Vec<&'static str> { vec!["TrickplayOptions", key] };
            let field = |key: &'static str, label, help, kind| Field {
                source: 0,
                path: o(key),
                label,
                help,
                kind,
            };
            vec![
                (
                    "Trickplay",
                    vec![
                        field("EnableHwAcceleration", "Enable hardware decoding", "", Bool),
                        field("EnableHwEncoding", "Enable hardware accelerated MJPEG encoding", "For QSV, VA-API, VideoToolbox and RKMPP.", Bool),
                        field("EnableKeyFrameOnlyExtraction", "Only generate images from key frames", "Much faster, with less exact times.", Bool),
                        field("ScanBehavior", "Scan behavior", "Blocking makes the images before a scan of the library ends.", choice(&[("NonBlocking", "Non blocking: queue generation, then return"), ("Blocking", "Blocking: generate, then return")])),
                        field("ProcessPriority", "Process priority", "A lower priority keeps the server responsive.", choice(&[("High", "High"), ("AboveNormal", "Above normal"), ("Normal", "Normal"), ("BelowNormal", "Below normal"), ("Idle", "Idle")])),
                    ],
                ),
                (
                    "Images",
                    vec![
                        field("Interval", "Image interval", "Milliseconds between two images.", whole(1., 3_600_000., "ms")),
                        field("WidthResolutions", "Width resolutions", "The widths, in pixels, the images are made in.", List),
                        field("TileWidth", "Tile width", "Images in one row of a sheet.", whole(1., 100., "")),
                        field("TileHeight", "Tile height", "Rows of images in a sheet.", whole(1., 100., "")),
                        field("JpegQuality", "JPEG quality", "", whole(0., 100., "")),
                        field("Qscale", "Qscale", "The quality scale of ffmpeg: 2 is the best, 31 the worst.", whole(2., 31., "")),
                        field("ProcessThreads", "FFmpeg threads", "0 lets ffmpeg choose.", whole(0., 256., "")),
                    ],
                ),
            ]
        }
        Section::Transcoding => {
            warning = Some("A wrong value here can make videos fail to play on devices that need the server to convert them. Hardware acceleration must fit the hardware of the server.");
            let number = |min: f64, max: f64, unit| Kind::Number { min, max, unit, scale: 1., whole: false };
            let threads: Vec<(Value, String)> = std::iter::once((json!(-1), "Auto".to_string()))
                .chain((1..=16).map(|n| (json!(n), n.to_string())))
                .chain(std::iter::once((json!(0), "Max".to_string())))
                .collect();
            let codec = |value: &'static str, label: &'static str| item(0, &["HardwareDecodingCodecs"], label, "", InList(value));
            vec![
                (
                    "Hardware acceleration",
                    vec![
                        item(0, &["HardwareAccelerationType"], "Hardware acceleration", "It needs more setup on the server; see the Jellyfin documentation.", choice(&[
                            ("none", "None"),
                            ("amf", "AMD AMF"),
                            ("nvenc", "Nvidia NVENC"),
                            ("qsv", "Intel QuickSync (QSV)"),
                            ("vaapi", "Video Acceleration API (VAAPI)"),
                            ("rkmpp", "Rockchip MPP (RKMPP)"),
                            ("videotoolbox", "Apple VideoToolBox"),
                            ("v4l2m2m", "Video4Linux2 (V4L2)"),
                        ])),
                        item(0, &["VaapiDevice"], "VA-API device", "The render node for VAAPI.", Path),
                        item(0, &["QsvDevice"], "QSV device", "The device for Intel QSV on a system with more than one GPU.", Path),
                    ],
                ),
                (
                    "Enable hardware decoding for",
                    vec![
                        codec("h264", "H264"),
                        codec("hevc", "HEVC"),
                        codec("mpeg1video", "MPEG1"),
                        codec("mpeg2video", "MPEG2"),
                        codec("mpeg4", "MPEG4"),
                        codec("vc1", "VC1"),
                        codec("vp8", "VP8"),
                        codec("vp9", "VP9"),
                        codec("av1", "AV1"),
                        item(0, &["EnableDecodingColorDepth10Hevc"], "HEVC 10bit", "", Bool),
                        item(0, &["EnableDecodingColorDepth10Vp9"], "VP9 10bit", "", Bool),
                        item(0, &["EnableDecodingColorDepth10HevcRext"], "HEVC RExt 8/10bit", "", Bool),
                        item(0, &["EnableDecodingColorDepth12HevcRext"], "HEVC RExt 12bit", "", Bool),
                        item(0, &["EnableEnhancedNvdecDecoder"], "Enable enhanced NVDEC decoder", "", Bool),
                        item(0, &["PreferSystemNativeHwDecoder"], "Prefer OS native DXVA or VA-API hardware decoders", "", Bool),
                    ],
                ),
                (
                    "Hardware encoding",
                    vec![
                        item(0, &["EnableHardwareEncoding"], "Enable hardware encoding", "", Bool),
                        item(0, &["EnableIntelLowPowerH264HwEncoder"], "Enable Intel Low-Power H.264 hardware encoder", "", Bool),
                        item(0, &["EnableIntelLowPowerHevcHwEncoder"], "Enable Intel Low-Power HEVC hardware encoder", "", Bool),
                    ],
                ),
                (
                    "Encoding format",
                    vec![
                        item(0, &["AllowHevcEncoding"], "Allow encoding in HEVC format", "", Bool),
                        item(0, &["AllowAv1Encoding"], "Allow encoding in AV1 format", "", Bool),
                    ],
                ),
                (
                    "Tone mapping",
                    vec![
                        item(0, &["EnableVppTonemapping"], "Enable VPP tone mapping", "Done by the Intel driver; works with certain hardware and HDR10 only.", Bool),
                        item(0, &["VppTonemappingBrightness"], "VPP tone mapping brightness gain", "0 to 100. The recommended value is 16.", number(0., 100., "")),
                        item(0, &["VppTonemappingContrast"], "VPP tone mapping contrast gain", "1 to 2. The recommended value is 1.", number(1., 2., "")),
                        item(0, &["EnableVideoToolboxTonemapping"], "Enable VideoToolbox tone mapping", "", Bool),
                        item(0, &["EnableTonemapping"], "Enable tone mapping", "Turns HDR video into SDR and keeps detail and colour.", Bool),
                        item(0, &["TonemappingAlgorithm"], "Tone mapping algorithm", "", choice(&[("none", "None"), ("clip", "Clip"), ("linear", "Linear"), ("gamma", "Gamma"), ("reinhard", "Reinhard"), ("hable", "Hable"), ("mobius", "Mobius"), ("bt2390", "BT.2390")])),
                        item(0, &["TonemappingMode"], "Tone mapping mode", "", choice(&[("auto", "Auto"), ("max", "MAX"), ("rgb", "RGB"), ("lum", "LUM"), ("itp", "ITP")])),
                        item(0, &["TonemappingRange"], "Tone mapping range", "", choice(&[("auto", "Auto"), ("tv", "TV"), ("pc", "PC")])),
                        item(0, &["TonemappingDesat"], "Tone mapping desaturation", "0 is the recommended and the default value.", number(0., 1_000_000., "")),
                        item(0, &["TonemappingPeak"], "Tone mapping peak", "Overrides the peak of the signal. The default is 100 (1000 nit).", number(0., 1_000_000., "")),
                        item(0, &["TonemappingParam"], "Tone mapping param", "Tunes the algorithm. Leave it at 0 in most cases.", number(0., 1_000_000., "")),
                    ],
                ),
                (
                    "Paths and threads",
                    vec![
                        item(0, &["EncodingThreadCount"], "Transcoding thread count", "The most threads a transcode uses.", Kind::Choice(threads)),
                        item(0, &["TranscodingTempPath"], "Transcode path", "A folder for the files of running transcodes.", Path),
                        item(0, &["FallbackFontPath"], "Fallback font folder path", "Fonts for ASS/SSA subtitles when the system has none for a script.", Path),
                        item(0, &["EnableFallbackFont"], "Enable fallback fonts", "", Bool),
                    ],
                ),
                (
                    "Audio",
                    vec![
                        item(0, &["EnableAudioVbr"], "Enable VBR audio encoding", "Better quality for the bitrate; in rare cases it causes buffering.", Bool),
                        item(0, &["DownMixAudioBoost"], "Audio boost when downmixing", "1 keeps the volume of the original.", number(0.5, 3., "")),
                        item(0, &["DownMixStereoAlgorithm"], "Stereo downmix algorithm", "", choice(&[("None", "None"), ("Dave750", "Dave750"), ("NightmodeDialogue", "NightmodeDialogue"), ("Rfc7845", "RFC7845"), ("Ac4", "AC-4")])),
                        item(0, &["HlsAudioSeekStrategy"], "HLS audio seek strategy", "", choice(&[("TrimCopiedAudio", "Trim copied audio"), ("TranscodeAudio", "Transcode audio")])),
                        item(0, &["MaxMuxingQueueSize"], "Max muxing queue size", "Packets buffered while the streams start. The recommended value is 2048.", whole(128., 2_147_483_647., "")),
                    ],
                ),
                (
                    "Encoding quality",
                    vec![
                        item(0, &["EncoderPreset"], "Encoding preset", "A slower preset gives better quality or a smaller file.", choice(&[("auto", "Auto"), ("veryslow", "veryslow"), ("slower", "slower"), ("slow", "slow"), ("medium", "medium"), ("fast", "fast"), ("faster", "faster"), ("veryfast", "veryfast"), ("superfast", "superfast"), ("ultrafast", "ultrafast")])),
                        item(0, &["H265Crf"], "H.265 encoding CRF", "0 to 51. Lower is better quality and a higher bitrate.", whole(0., 51., "")),
                        item(0, &["H264Crf"], "H.264 encoding CRF", "0 to 51. Sane values are 18 to 28.", whole(0., 51., "")),
                        item(0, &["DeinterlaceMethod"], "Deinterlacing method", "", choice(&[("yadif", "YADIF"), ("bwdif", "BWDIF")])),
                        item(0, &["DeinterlaceDoubleRate"], "Double the frame rate when deinterlacing", "", Bool),
                    ],
                ),
                (
                    "Subtitles and segments",
                    vec![
                        item(0, &["EnableSubtitleExtraction"], "Allow subtitle extraction on the fly", "Embedded subtitles go to the client as text, which avoids a transcode of the video.", Bool),
                        item(0, &["EnableThrottling"], "Throttle transcodes", "A transcode stops once it is far enough ahead of the playback.", Bool),
                        item(0, &["ThrottleDelaySeconds"], "Throttle after", "Seconds a transcode must be ahead before it stops.", whole(10., 3_600., "s")),
                        item(0, &["EnableSegmentDeletion"], "Delete segments", "Old segments go once the client has them.", Bool),
                        item(0, &["SegmentKeepSeconds"], "Time to keep segments", "Seconds a segment stays before it is deleted.", whole(10., 3_600., "s")),
                    ],
                ),
            ]
        }
        Section::Networking => {
            warning = Some("A wrong value here can make the server unreachable, also for this app. Most of these take effect after a restart of the server.");
            let port = || whole(1., 65_535., "");
            vec![
                (
                    "Server address",
                    vec![
                        item(0, &["InternalHttpPort"], "Local HTTP port number", "", port()),
                        item(0, &["EnableHttps"], "Enable HTTPS", "The server listens on the HTTPS port; it needs a certificate.", Bool),
                        item(0, &["InternalHttpsPort"], "Local HTTPS port number", "", port()),
                        item(0, &["BaseUrl"], "Base URL", "A subdirectory for the address of the server, such as /jellyfin.", Text),
                        item(0, &["LocalNetworkAddresses"], "Bind to local network address", "Empty binds to every address.", List),
                        item(0, &["LocalNetworkSubnets"], "LAN networks", "Addresses or subnets that count as the local network.", List),
                        item(0, &["KnownProxies"], "Known proxies", "Reverse proxies whose forwarded address the server trusts.", List),
                    ],
                ),
                (
                    "HTTPS",
                    vec![
                        item(0, &["RequireHttps"], "Require HTTPS", "Every HTTP request goes to HTTPS.", Bool),
                        item(0, &["CertificatePath"], "Custom SSL certificate path", "A PKCS #12 file with a certificate and a private key.", Path),
                    ],
                ),
                (
                    "Remote access",
                    vec![
                        item(0, &["EnableRemoteAccess"], "Allow remote connections to this server", "Off blocks every connection from outside the local network.", Bool),
                        item(0, &["RemoteIPFilter"], "Remote IP address filter", "Addresses or subnets for the filter below.", List),
                        item(0, &["IsRemoteIPFilterBlacklist"], "Remote IP address filter mode", "", Kind::Choice(vec![(json!(false), "Whitelist".to_string()), (json!(true), "Blacklist".to_string())])),
                        item(0, &["PublicHttpPort"], "Public HTTP port number", "", port()),
                        item(0, &["PublicHttpsPort"], "Public HTTPS port number", "", port()),
                    ],
                ),
                (
                    "IP protocols",
                    vec![
                        item(0, &["EnableIPv4"], "Enable IPv4", "", Bool),
                        item(0, &["EnableIPv6"], "Enable IPv6", "", Bool),
                    ],
                ),
                (
                    "Discovery",
                    vec![
                        item(0, &["AutoDiscovery"], "Enable auto discovery", "Apps find the server on the local network by UDP port 7359.", Bool),
                        item(0, &["PublishedServerUriBySubnet"], "Published server URIs", "The address the server names to clients, by subnet: subnet=address.", List),
                    ],
                ),
            ]
        }
        _ => Vec::new(),
    };
    Spec { groups, warning }
}

// ----- editing ----------------------------------------------------------------

impl Data {
    /// Fields whose value is not the one of the server.
    fn changes(&self, spec: &Spec) -> usize {
        let mut places: Vec<(usize, &[&str])> = Vec::new();
        for field in spec.groups.iter().flat_map(|(_, fields)| fields) {
            let place = (field.source, field.path.as_slice());
            if !places.contains(&place)
                && at(&self.values[field.source], &field.path) != at(&self.edited[field.source], &field.path)
            {
                places.push(place);
            }
        }
        places.len()
    }

    pub fn dirty(&self) -> bool {
        self.values != self.edited
    }
}

impl Bloom {
    fn config(&self) -> Option<(Section, &Data)> {
        let Page::Admin(admin) = &self.page else {
            return None;
        };
        admin.config.get(&admin.section).map(|data| (admin.section, data))
    }

    /// Changes one value of the copy the user edits. Nothing goes to the
    /// server before a save.
    pub fn config_put(&mut self, source: usize, path: Vec<&'static str>, value: Value, cx: &mut Context<Self>) {
        if let Page::Admin(admin) = &mut self.page
            && let Some(data) = admin.config.get_mut(&admin.section)
            && let Some(object) = data.edited.get_mut(source)
        {
            put(object, &path, value);
            cx.notify();
        }
    }

    /// Changes the value of the field with a key, from text; for the debug
    /// channel. It edits the copy only.
    pub fn config_edit(&mut self, key: &str, text: &str, cx: &mut Context<Self>) -> String {
        let Some((section, data)) = self.config() else {
            return "error: no configuration page is open".into();
        };
        let spec = spec(section, &data.options);
        let found = spec
            .groups
            .iter()
            .flat_map(|(_, fields)| fields)
            .find(|field| field.path.last().is_some_and(|last| last.eq_ignore_ascii_case(key)));
        let Some(field) = found.cloned() else {
            return format!("error: this page has no field {key:?}");
        };
        let value = serde_json::from_str(text).unwrap_or_else(|_| json!(text));
        self.config_put(field.source, field.path, value, cx);
        self.config_state()
    }

    /// Opens the form of the field with a key, as a click on it does.
    pub fn config_ask_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some((section, data)) = self.config() else {
            return false;
        };
        let spec = spec(section, &data.options);
        let found = spec
            .groups
            .iter()
            .flat_map(|(_, fields)| fields)
            .find(|field| field.path.last().is_some_and(|last| last.eq_ignore_ascii_case(key)))
            .cloned();
        let Some(field) = found else {
            return false;
        };
        let value = at(&data.edited[field.source], &field.path);
        let current = match &field.kind {
            Kind::List => list(value).join(", "),
            Kind::Number { scale, .. } => (value.as_f64().unwrap_or(0.) / scale).to_string(),
            _ => text(value),
        };
        self.config_ask(field, current, cx);
        true
    }

    /// Opens the large text dialog for a field of the kind `Multiline`; for
    /// a click on the field and for the debug channel. The text goes into
    /// the edited copy of the page, not to the server.
    fn open_multiline(&mut self, field: Field, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, data)) = self.config() else {
            return;
        };
        let Kind::Multiline { mono } = field.kind else {
            return;
        };
        let value = text(at(&data.edited[field.source], &field.path));
        let (source, path) = (field.source, field.path.clone());
        self.open_text_editor(
            super::text_editor::TextEdit {
                title: field.label.to_string(),
                message: field.help.to_string(),
                action: "Use this text".to_string(),
                mono,
                value,
                apply: Rc::new(move |this, value, cx| {
                    this.config_put(source, path.clone(), json!(value), cx)
                }),
            },
            window,
            cx,
        );
    }

    /// The same for the field with a key.
    pub fn config_open_text(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some((section, data)) = self.config() else {
            return false;
        };
        let spec = spec(section, &data.options);
        let found = spec
            .groups
            .iter()
            .flat_map(|(_, fields)| fields)
            .find(|field| {
                matches!(field.kind, Kind::Multiline { .. })
                    && field.path.last().is_some_and(|last| last.eq_ignore_ascii_case(key))
            })
            .cloned();
        match found {
            Some(field) => {
                self.open_multiline(field, window, cx);
                true
            }
            None => false,
        }
    }

    /// Takes one key of an object from the server, in the loaded and in the
    /// edited copy, so an edit of another key stays. A key the server does
    /// not send is removed.
    pub fn config_take_key(&mut self, section: Section, source: usize, key: &str, server: &Value, cx: &mut Context<Self>) {
        if let Page::Admin(admin) = &mut self.page
            && let Some(data) = admin.config.get_mut(&section)
        {
            for copy in [&mut data.values, &mut data.edited] {
                if let Some(object) = copy.get_mut(source).and_then(Value::as_object_mut) {
                    match server.get(key) {
                        Some(value) => object.insert(key.to_string(), value.clone()),
                        None => object.remove(key),
                    };
                }
            }
            cx.notify();
        }
    }

    /// The open configuration page in a line, for the debug channel.
    pub fn config_state(&self) -> String {
        match self.config() {
            Some((section, data)) => {
                let spec = spec(section, &data.options);
                let fields: usize = spec.groups.iter().map(|(_, fields)| fields.len()).sum();
                format!("{section:?}: {fields} fields, {} changed", data.changes(&spec))
            }
            None => "no configuration page".into(),
        }
    }

    pub fn config_discard(&mut self, cx: &mut Context<Self>) {
        if let Page::Admin(admin) = &mut self.page
            && let Some(data) = admin.config.get_mut(&admin.section)
        {
            data.edited = data.values.clone();
            cx.notify();
        }
    }

    /// Sends the edited objects to the server; on a page with a warning,
    /// after the user confirms it.
    pub fn config_save(&mut self, cx: &mut Context<Self>) {
        let Some((section, data)) = self.config() else {
            return;
        };
        let spec = spec(section, &data.options);
        match spec.warning {
            Some(warning) => self.ask_confirm(
                Confirm {
                    title: format!("Save the {} settings?", section.label().to_lowercase()),
                    message: warning.to_string(),
                    action: "Save".to_string(),
                    danger: true,
                    run: Rc::new(|this, cx| this.config_send(cx)),
                },
                cx,
            ),
            None => self.config_send(cx),
        }
    }

    fn config_send(&mut self, cx: &mut Context<Self>) {
        let Page::Admin(admin) = &mut self.page else {
            return;
        };
        let Some(data) = admin.config.get_mut(&admin.section) else {
            return;
        };
        // Each object that changed goes back whole: the object the server
        // gave, with the edited values in it.
        let posts: Vec<(usize, String, Value)> = sources(admin.section)
            .into_iter()
            .enumerate()
            .filter(|(n, _)| data.values[*n] != data.edited[*n])
            .map(|(n, source)| (n, endpoint(source), data.edited[n].clone()))
            .collect();
        if posts.is_empty() {
            return;
        }
        // An object counts as the server's once the server took it; a
        // refused one keeps its edits on show, to change or send again. The
        // answer is for the session that sent the objects.
        let section = admin.section;
        let epoch = self.session_epoch;
        self.fetch(
            cx,
            move |client| -> Result<Sent> {
                let mut sent = Sent::default();
                for (n, path, body) in posts {
                    match client.post(&path, &body) {
                        Ok(_) => sent.taken.push((n, body)),
                        Err(err) => sent.refused.push(err),
                    }
                }
                Ok(sent)
            },
            move |this, result, cx| {
                if this.session_epoch != epoch {
                    return;
                }
                let sent = result.unwrap_or_else(|err| Sent { taken: Vec::new(), refused: vec![err] });
                match sent.refused.first() {
                    None => this.toast("Settings saved", "", cx),
                    Some(err) => this.toast("The server refused the change", format!("{err:#}"), cx),
                }
                if let Page::Admin(admin) = &mut this.page
                    && let Some(data) = admin.config.get_mut(&section)
                {
                    for (n, body) in sent.taken {
                        if let Some(value) = data.values.get_mut(n) {
                            *value = body;
                        }
                    }
                }
                // The load after the save brings what the server has now; a
                // page with edits left keeps them (`load_admin`).
                if matches!(this.page, Page::Admin(_)) {
                    this.load_page(cx);
                }
                cx.notify();
            },
        );
    }

    /// A form for the value of a text, path, number or list field.
    fn config_ask(&mut self, field: Field, current: String, cx: &mut Context<Self>) {
        let (label, kind) = (field.label, field.kind.clone());
        let placeholder = match &kind {
            Kind::List => "Values, with a comma between them",
            Kind::Path => "A path on the server",
            Kind::Number { .. } => "A number",
            _ => "",
        };
        let message = match (&kind, field.help) {
            (Kind::List, "") => "Put a comma between two values.".to_string(),
            (Kind::List, help) => format!("{help} Put a comma between two values."),
            (_, help) => help.to_string(),
        };
        self.ask_prompt(
            Prompt {
                title: label.to_string(),
                message,
                action: "Set".to_string(),
                fields: vec![PromptField {
                    label: label.to_string(),
                    placeholder: placeholder.to_string(),
                    value: current,
                    masked: false,
                    required: false,
                }],
                run: Rc::new(move |this, values, cx| {
                    let typed = values.first().map(|v| v.trim()).unwrap_or_default();
                    let value = match &kind {
                        Kind::Number { min, max, scale, whole, .. } => {
                            let Ok(number) = typed.parse::<f64>() else {
                                this.toast(label, "That is not a number.", cx);
                                return;
                            };
                            let stored = number.clamp(*min, *max) * scale;
                            if *whole || *scale != 1. {
                                json!(stored.round() as i64)
                            } else {
                                json!(stored)
                            }
                        }
                        Kind::List => {
                            // A list of numbers stays a list of numbers.
                            let numbers = list_is_numbers(this, &field);
                            Value::Array(
                                typed
                                    .split([',', '\n'])
                                    .map(str::trim)
                                    .filter(|entry| !entry.is_empty())
                                    .map(|entry| match entry.parse::<i64>() {
                                        Ok(number) if numbers => json!(number),
                                        _ => json!(entry),
                                    })
                                    .collect(),
                            )
                        }
                        _ => json!(typed),
                    };
                    this.config_put(field.source, field.path.clone(), value, cx);
                }),
            },
            cx,
        );
    }

    /// The bar with Save and Discard, while the page has changes.
    pub fn render_config_bar(&self, cx: &mut Context<Self>) -> Option<Div> {
        let (section, data) = self.config()?;
        if !data.dirty() {
            return None;
        }
        let changes = data.changes(&spec(section, &data.options));
        Some(bar_shell(
            changes,
            button("config.discard", "Discard", ButtonKind::Plain, cx)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.config_discard(cx))),
            button("config.save", "Save", ButtonKind::Primary, cx)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.config_save(cx))),
            cx,
        ))
    }
}

/// The bar at the bottom of a page with unsaved changes: the count, and the
/// two buttons the caller made.
pub(super) fn bar_shell(
    changes: usize,
    discard: Stateful<Div>,
    save: Stateful<Div>,
    cx: &Context<Bloom>,
) -> Div {
    let t = UiTheme::read(cx).clone();
    div()
        .absolute()
        .left(px(super::SIDEBAR_W + 28.))
        .right(px(28.))
        .bottom(px(20.))
        .h(px(64.))
        .rounded(px(24.))
        .border_1()
        .border_color(rgba(0xf5f5f733))
        .child(glass(px(24.), crate::ui::glass::POPUP_TINT))
        .px(px(20.))
        .flex()
        .items_center()
        .gap(px(10.))
        .child(
            div()
                .flex_1()
                .text_size(px(15.))
                .text_color(t.colors.foreground)
                .child(match changes {
                    1 => "1 change is not saved.".to_string(),
                    n => format!("{n} changes are not saved."),
                }),
        )
        .child(discard)
        .child(save)
}

/// The object of the Branding page as the server gave it.
pub fn branding_of(data: &Data) -> Option<&Value> {
    data.values.first()
}

fn list_is_numbers(app: &Bloom, field: &Field) -> bool {
    app.config()
        .and_then(|(_, data)| at(&data.values[field.source], &field.path).as_array().cloned())
        .is_some_and(|items| !items.is_empty() && items.iter().all(Value::is_number))
}

// ----- the page -----------------------------------------------------------------

/// A field that shows its value and opens a form on a click.
pub(super) fn value_button(id: SharedString, value: String, cx: &Context<Bloom>) -> Stateful<Div> {
    let t = UiTheme::read(cx);
    let empty = value.is_empty();
    div()
        .id(id)
        .min_w(px(120.))
        .max_w(px(340.))
        .h(px(38.))
        .pl(px(12.))
        .pr(px(10.))
        .rounded(px(8.))
        .border_1()
        .border_color(rgba(0x282828cc))
        .bg(rgba(0x00000059))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(10.))
        .cursor_pointer()
        .text_size(px(15.))
        .text_color(if empty { t.colors.foreground.opacity(0.5) } else { t.colors.foreground })
        .hover(|s| s.border_color(rgba(0xf5f5f766)))
        .child(div().min_w_0().truncate().child(if empty { "Not set".to_string() } else { value }))
        .child(icon(LucideIcon::Pencil, 15., t.colors.foreground.opacity(0.7)))
}

/// A group with this many fields or more builds only the fields in view.
const LAZY_MIN: usize = 4;

/// One row of the page: the label and help of a field with its control.
fn field_row(
    section: Section,
    g: usize,
    n: usize,
    entry: &Field,
    data: &Data,
    cx: &mut Context<Bloom>,
) -> Div {
    let id = SharedString::from(format!("config.{section:?}.{g}.{n}"));
    let value = at(&data.edited[entry.source], &entry.path).clone();
    let (source, path) = (entry.source, entry.path.clone());
    let control: Stateful<Div> = match &entry.kind {
        Kind::Bool => {
            let on = value.as_bool().unwrap_or(false);
            checkbox(id, on, cx).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.config_put(source, path.clone(), json!(!on), cx)
            }))
        }
        Kind::InList(member) => {
            let member = *member;
            let items = list(&value);
            let on = items.iter().any(|entry| entry == member);
            checkbox(id, on, cx).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                let mut items = items.clone();
                if on {
                    items.retain(|entry| entry != member);
                } else {
                    items.push(member.to_string());
                }
                this.config_put(source, path.clone(), json!(items), cx)
            }))
        }
        Kind::Choice(options) => {
            // A key the server left out shows as the empty choice.
            let value = if value.is_null() { json!("") } else { value };
            let shown = options
                .iter()
                .find(|(option, _)| *option == value)
                .map_or_else(|| text(&value), |(_, label)| label.clone());
            let longest = options
                .iter()
                .map(|(_, label)| label.as_str())
                .max_by_key(|label| label.chars().count())
                .unwrap_or_default()
                .to_string();
            let options = options.clone();
            select(id, shown, &longest, cx).on_click(cx.listener(
                move |this, event: &ClickEvent, window, cx| {
                    let choices = options
                        .iter()
                        .map(|(option, label)| {
                            let (path, option) = (path.clone(), option.clone());
                            Choice::new(label.clone(), option == value, move |this, cx| {
                                this.config_put(source, path.clone(), option.clone(), cx)
                            })
                        })
                        .collect();
                    this.open_choices(choices, event.position(), window, cx);
                },
            ))
        }
        Kind::Number { unit, scale, .. } => {
            let number = value.as_f64().unwrap_or(0.) / scale;
            let typed = if number.fract() == 0. { format!("{number:.0}") } else { number.to_string() };
            let shown = if unit.is_empty() { typed.clone() } else { format!("{typed} {unit}") };
            let entry = entry.clone();
            value_button(id, shown, cx).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.config_ask(entry.clone(), typed.clone(), cx)
            }))
        }
        Kind::Multiline { .. } => {
            let typed = text(&value);
            let lines = typed.split('\n').count();
            let shown = match (typed.is_empty(), lines) {
                (true, _) => String::new(),
                (false, 1) => typed.clone(),
                (false, n) => format!("{n} lines"),
            };
            let entry = entry.clone();
            value_button(id, shown, cx).on_click(cx.listener(
                move |this, _: &ClickEvent, window, cx| this.open_multiline(entry.clone(), window, cx),
            ))
        }
        Kind::Text | Kind::Path | Kind::List => {
            let typed = match &entry.kind {
                Kind::List => list(&value).join(", "),
                _ => text(&value),
            };
            let entry = entry.clone();
            value_button(id, typed.clone(), cx).on_click(cx.listener(
                move |this, _: &ClickEvent, _, cx| this.config_ask(entry.clone(), typed.clone(), cx),
            ))
        }
    };
    field(entry.label, entry.help, control, cx)
}

pub fn render(section: Section, data: &Data, cx: &mut Context<Bloom>) -> Div {
    let spec = spec(section, &data.options);
    let mut page = div().max_w(px(920.)).flex().flex_col().gap(px(18.));
    for (g, (title, fields)) in spec.groups.iter().enumerate() {
        let mut card = group(*title, cx);
        if fields.len() >= LAZY_MIN {
            // A long group: the rows of the fields in view only. They read
            // the edits from the app each time they are built.
            let fields = fields.clone();
            card = card.child(super::lazy::lazy(
                cx,
                format!("config.{section:?}.{g}"),
                fields.len(),
                move |app, range, cx| {
                    let Some((_, data)) = app.config() else {
                        return Vec::new();
                    };
                    range
                        .map(|n| field_row(section, g, n, &fields[n], data, cx).into_any_element())
                        .collect()
                },
            ));
        } else {
            for (n, entry) in fields.iter().enumerate() {
                card = card.child(field_row(section, g, n, entry, data, cx));
            }
        }
        // The splash screen has an image and buttons besides its toggle.
        if section == Section::Branding && *title == "Splash screen" {
            card = card.child(super::branding::splash_block(&data.values[0], &data.splash_preview, cx));
        }
        page = page.child(card);
    }
    // Room for the bar with Save, so it does not cover the last field.
    page.child(div().h(px(if data.dirty() { 72. } else { 0. })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_touches_only_its_key() {
        let server = json!({
            "ServerName": "home",
            "Unknown": { "Kept": [1, 2] },
            "TrickplayOptions": { "Interval": 10000, "Other": true },
        });
        let mut edited = server.clone();
        put(&mut edited, &["TrickplayOptions", "Interval"], json!(5000));
        put(&mut edited, &["ServerName"], json!("den"));
        assert_eq!(edited["TrickplayOptions"]["Interval"], 5000);
        assert_eq!(edited["TrickplayOptions"]["Other"], true);
        assert_eq!(edited["Unknown"], server["Unknown"]);
        assert_eq!(at(&edited, &["ServerName"]), "den");
        // A key the server did not send is made, not lost.
        put(&mut edited, &["CachePath"], json!("/cache"));
        assert_eq!(edited["CachePath"], "/cache");
    }

    #[test]
    fn every_page_has_fields_with_distinct_places() {
        for section in [
            Section::General,
            Section::Branding,
            Section::LibraryDisplay,
            Section::LibraryMetadata,
            Section::Nfo,
            Section::Transcoding,
            Section::Resume,
            Section::Streaming,
            Section::Trickplay,
            Section::Networking,
        ] {
            let spec = spec(section, &Options::default());
            let fields: Vec<&Field> = spec.groups.iter().flat_map(|(_, fields)| fields).collect();
            assert!(!fields.is_empty(), "{section:?}");
            for field in &fields {
                assert!(field.source < sources(section).len(), "{section:?} {}", field.label);
                // Two fields share a place only as members of one list.
                let same = fields
                    .iter()
                    .filter(|other| other.source == field.source && other.path == field.path)
                    .count();
                assert!(same == 1 || matches!(field.kind, Kind::InList(_)), "{section:?} {}", field.label);
            }
        }
        assert!(sources(Section::Users).is_empty());
    }

    #[test]
    fn counts_changed_fields() {
        let options = Options::default();
        let spec = spec(Section::Resume, &options);
        let values = vec![json!({ "MinResumePct": 5, "MaxResumePct": 90 })];
        let mut data = Data { edited: values.clone(), values, options, splash_preview: String::new() };
        assert_eq!(data.changes(&spec), 0);
        assert!(!data.dirty());
        put(&mut data.edited[0], &["MinResumePct"], json!(10));
        assert_eq!(data.changes(&spec), 1);
        assert!(data.dirty());
    }
}

/// A refused save (review of 2026-10-05, UI finding 5). See
/// `app::race_harness`.
#[cfg(test)]
mod race_tests {
    use gpui_kit::TestAppContext;
    use serde_json::json;

    use super::{Page, Section};
    use crate::app::{Screen, race_harness::{MockServer, app, plain, session}};

    #[gpui_kit::test]
    fn a_refused_save_of_the_server_settings_loses_the_edits(cx: &mut TestAppContext) {
        let server = MockServer::start(|method, path, _| match (method, path) {
            ("GET", p) if p.starts_with("/System/Configuration") => {
                (200, r#"{"ServerName":"home","EnableMetrics":false}"#.into())
            }
            ("POST", p) if p.starts_with("/System/Configuration") => (500, "refused".into()),
            _ => plain(method, path),
        });
        let (bloom, cx) = app(cx);
        bloom.update(cx, |this, cx| {
            this.session = Some(session(&server.url, "u1"));
            this.screen = Screen::Main;
            this.open_admin(Section::General, cx);
        });
        cx.run_until_parked();
        bloom.update(cx, |this, cx| {
            let answer = this.config_edit("ServerName", "den", cx);
            assert!(!answer.starts_with("error"), "{answer}");
            assert!(this.config_state().contains("1 changed"), "{}", this.config_state());
            this.config_save(cx);
        });
        cx.run_until_parked();
        assert_eq!(server.count("POST", "/System/Configuration"), 1);
        bloom.read_with(cx, |this, _| {
            assert!(
                this.config_state().contains("1 changed"),
                "the server refused the save, and the edit is gone: {}",
                this.config_state()
            );
        });
        // The edit is still there to send again.
        bloom.update(cx, |this, cx| this.config_save(cx));
        cx.run_until_parked();
        assert_eq!(server.count("POST", "/System/Configuration"), 2, "the save was not sent again");
        assert_eq!(server.bodies("POST", "/System/Configuration")[1]["ServerName"], json!("den"));
    }

    /// Two objects on one page: the server takes the first and refuses the
    /// second. The first is clean, the second keeps its edit.
    #[gpui_kit::test]
    fn a_partly_refused_save_keeps_the_refused_edits_only(cx: &mut TestAppContext) {
        let server = MockServer::start(|method, path, _| match (method, path) {
            ("GET", "/System/Configuration/metadata") => (200, r#"{"UseFileCreationTimeForDateAdded":false}"#.into()),
            ("GET", p) if p.starts_with("/System/Configuration") => (200, r#"{"EnableFolderView":false}"#.into()),
            ("POST", "/System/Configuration/metadata") => (500, "refused".into()),
            ("POST", p) if p.starts_with("/System/Configuration") => (204, String::new()),
            _ => plain(method, path),
        });
        let (bloom, cx) = app(cx);
        bloom.update(cx, |this, cx| {
            this.session = Some(session(&server.url, "u1"));
            this.screen = Screen::Main;
            this.open_admin(Section::LibraryDisplay, cx);
        });
        cx.run_until_parked();
        bloom.update(cx, |this, cx| {
            for key in ["EnableFolderView", "UseFileCreationTimeForDateAdded"] {
                let answer = this.config_edit(key, "true", cx);
                assert!(!answer.starts_with("error"), "{answer}");
            }
            assert!(this.config_state().contains("2 changed"), "{}", this.config_state());
            this.config_save(cx);
        });
        cx.run_until_parked();
        assert_eq!(server.count("POST", "/System/Configuration"), 2, "both objects go to the server");
        bloom.read_with(cx, |this, _| {
            assert!(this.config_state().contains("1 changed"), "{}", this.config_state());
            let Page::Admin(admin) = &this.page else { panic!("not the dashboard") };
            let data = &admin.config[&Section::LibraryDisplay];
            assert_eq!(data.values[0], data.edited[0], "the object the server took is still dirty");
            assert_eq!(data.values[0]["EnableFolderView"], json!(true), "the taken object is not the baseline");
            assert_ne!(data.values[1], data.edited[1], "the refused edit is gone");
            assert_eq!(data.edited[1]["UseFileCreationTimeForDateAdded"], json!(true));
        });
    }
}
