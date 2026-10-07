// SPDX-License-Identifier: AGPL-3.0-or-later
//! The pages of the settings, one for each section.

use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, ObjectFit, ParentElement as _,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled, div, px, rgba,
};
use serde_json::Value;

use super::{Choice, checkbox, field, group, icon_button, raised, select};
use crate::{
    app::Bloom,
    config::SubtitleLook,
    ui::{theme::UiTheme, tip::tip},
};

/// Languages a select offers: code as the server stores it, and name. The
/// server knows some 500; these are the ones in common use for audio and
/// subtitles. A value outside the list still shows, by its code.
const LANGUAGES: [(&str, &str); 40] = [
    ("ara", "Arabic"),
    ("bul", "Bulgarian"),
    ("cat", "Catalan"),
    ("chi", "Chinese"),
    ("hrv", "Croatian"),
    ("cze", "Czech"),
    ("dan", "Danish"),
    ("dut", "Dutch"),
    ("eng", "English"),
    ("est", "Estonian"),
    ("fil", "Filipino"),
    ("fin", "Finnish"),
    ("fre", "French"),
    ("ger", "German"),
    ("gre", "Greek"),
    ("heb", "Hebrew"),
    ("hin", "Hindi"),
    ("hun", "Hungarian"),
    ("ice", "Icelandic"),
    ("ind", "Indonesian"),
    ("ita", "Italian"),
    ("jpn", "Japanese"),
    ("kor", "Korean"),
    ("lav", "Latvian"),
    ("lit", "Lithuanian"),
    ("may", "Malay"),
    ("nor", "Norwegian"),
    ("per", "Persian"),
    ("pol", "Polish"),
    ("por", "Portuguese"),
    ("rum", "Romanian"),
    ("rus", "Russian"),
    ("srp", "Serbian"),
    ("slo", "Slovak"),
    ("spa", "Spanish"),
    ("swe", "Swedish"),
    ("tha", "Thai"),
    ("tur", "Turkish"),
    ("ukr", "Ukrainian"),
    ("vie", "Vietnamese"),
];

/// Subtitle modes of the server, with the web client's texts.
const SUBTITLE_MODES: [(&str, &str, &str); 5] = [
    (
        "Default",
        "Default",
        "Follows the default and forced marks of the file. Your language counts when the file has more than one choice.",
    ),
    (
        "Smart",
        "Smart",
        "Shows subtitles in your language when the audio is in another language.",
    ),
    (
        "OnlyForced",
        "Only forced",
        "Shows only the subtitles the file marks as forced.",
    ),
    (
        "Always",
        "Always",
        "Shows subtitles in your language, whatever the audio language is.",
    ),
    (
        "None",
        "None",
        "Shows no subtitles at the start. You can turn them on in the player.",
    ),
];

/// Kinds of home sections, as the web client names them. The last three
/// have no row in this app; a slot that holds one stays as it is.
const HOME_SECTIONS: [(&str, &str); 10] = [
    ("none", "None"),
    ("latestmedia", "Recently Added"),
    ("smalllibrarytiles", "My Media"),
    ("librarybuttons", "My Media (small)"),
    ("resume", "Continue Watching"),
    ("nextup", "Next Up"),
    ("resumeaudio", "Continue Listening (web only)"),
    ("resumebook", "Continue Reading (web only)"),
    ("livetv", "Live TV (web only)"),
    ("activerecordings", "Active Recordings (web only)"),
];

/// Lengths of the skip buttons of the player, in seconds.
const SKIP_SECS: [u32; 6] = [5, 10, 15, 20, 25, 30];

fn language_name(code: &str) -> String {
    if code.is_empty() {
        return "Any language".to_string();
    }
    LANGUAGES
        .iter()
        .find(|(known, _)| *known == code)
        .map_or_else(|| code.to_string(), |(_, name)| name.to_string())
}

/// A select that opens its list at the pointer. `choices` makes the list
/// when the select is clicked.
fn select_field(
    id: &'static str,
    value: String,
    longest: &str,
    choices: impl Fn(&Bloom) -> Vec<Choice> + 'static,
    cx: &mut Context<Bloom>,
) -> Stateful<Div> {
    select(id, value, longest, cx).on_click(cx.listener(
        move |this, event: &ClickEvent, window, cx| {
            let choices = choices(this);
            this.open_choices(choices, event.position(), window, cx);
        },
    ))
}

/// A checkbox for a yes-or-no field of the user's configuration on the server.
fn server_check(key: &'static str, default: bool, app: &Bloom, cx: &mut Context<Bloom>) -> Stateful<Div> {
    let on = app.prefs.flag(key, default);
    checkbox(SharedString::from(format!("settings.check.{key}")), on, cx).on_click(cx.listener(
        move |this, _: &ClickEvent, _, cx| this.set_user_config(key, Value::Bool(!on), cx),
    ))
}

/// A select of a language field of the user's configuration.
fn language_select(id: &'static str, key: &'static str, app: &Bloom, cx: &mut Context<Bloom>) -> Stateful<Div> {
    let current = app.prefs.text(key);
    select_field(
        id,
        language_name(&current),
        "Any language",
        move |app| {
            let current = app.prefs.text(key);
            let mut choices = vec![Choice::new("Any language", current.is_empty(), move |this, cx| {
                this.set_user_config(key, Value::String(String::new()), cx)
            })];
            choices.extend(LANGUAGES.iter().map(|(code, name)| {
                Choice::new(*name, current == *code, move |this, cx| {
                    this.set_user_config(key, Value::String(code.to_string()), cx)
                })
            }));
            choices
        },
        cx,
    )
}

impl Bloom {
    pub(super) fn render_settings_profile(&self, cx: &mut Context<Self>) -> Div {
        let t = UiTheme::read(cx).clone();
        let Some(session) = &self.session else {
            return div();
        };
        let initial = session
            .user_name
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_default();
        let avatar = div()
            .relative()
            .size(px(96.))
            .flex_shrink_0()
            .rounded_full()
            .overflow_hidden()
            .bg(rgba(0xcccccf69))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(40.))
            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
            .text_color(t.colors.foreground)
            .child(initial)
            .children(session.user_image.clone().map(|url| {
                div()
                    .absolute()
                    .inset_0()
                    .child(crate::images::remote_with(url, px(48.), ObjectFit::Cover).size_full())
            }));
        let mut facts: Vec<String> = vec![session.server_name.clone()];
        if session.is_admin {
            facts.push("Administrator".to_string());
        }
        if let Some(login) = self.prefs.last_login.as_deref().map(crate::admin::ago)
            && !login.is_empty()
        {
            facts.push(format!("Signed in {login}"));
        }
        // The profile header of the theme: a glass card with the picture
        // and the name.
        let header = div()
            .rounded(px(32.))
            .border_1()
            .border_color(rgba(0xf5f5f733))
            .bg(rgba(0x2a2a2ab0))
            .p(px(22.))
            .flex()
            .items_center()
            .gap(px(22.))
            .child(avatar)
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(28.))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(t.colors.foreground)
                            .child(session.user_name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(14.))
                            .text_color(t.colors.foreground.opacity(0.6))
                            .child(facts.join(" · ")),
                    ),
            );

        let has_picture = session.user_image.is_some();
        let picture = div()
            .flex()
            .gap(px(8.))
            .child(
                raised(
                    "settings.picture",
                    if has_picture { "Change picture" } else { "Add picture" },
                    cx,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.pick_profile_picture(cx))),
            )
            .children(has_picture.then(|| {
                raised("settings.picture-remove", "Remove", cx).on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| this.remove_profile_picture(cx),
                ))
            }));
        let password = raised("settings.password", "Change password", cx).on_click(cx.listener(
            |this, _: &ClickEvent, _, cx| this.ask_new_password(cx),
        ));
        let quick = raised("settings.quickconnect", "Enter code", cx).on_click(cx.listener(
            |this, _: &ClickEvent, window, cx| this.open_authorize(window, cx),
        ));
        let sign_out = raised("settings.signout", "Sign out", cx)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.sign_out(cx)));
        let account = group("Account", cx)
            .child(field(
                "Profile picture",
                "A PNG, JPEG, WebP or GIF image, up to 10 MB. It shows on every device.",
                picture,
                cx,
            ))
            .child(field(
                "Password",
                if self.prefs.has_password {
                    "This account has a password."
                } else {
                    "This account has no password."
                },
                password,
                cx,
            ))
            .child(field(
                "Quick Connect",
                "Signs in another device with the six-digit code it shows.",
                quick,
                cx,
            ))
            .child(field(
                "Sign out",
                "Ends this session on the server and forgets it on this Mac.",
                sign_out,
                cx,
            ));
        div().flex().flex_col().gap(px(18.)).child(header).child(account)
    }

    pub(super) fn render_settings_playback(&self, cx: &mut Context<Self>) -> Div {
        let audio = group("Audio", cx)
            .child(field(
                "Preferred audio language",
                "A track in this language plays when the file has one.",
                language_select("settings.audio-language", "AudioLanguagePreference", self, cx),
                cx,
            ))
            .child(field(
                "Play default audio track regardless of language",
                "The track the file marks as its default plays, also when it is not in your language.",
                server_check("PlayDefaultAudioTrack", false, self, cx),
                cx,
            ))
            .child(field(
                "Remember audio selections",
                "The server keeps the track you chose for an item and names it again.",
                server_check("RememberAudioSelections", true, self, cx),
                cx,
            ));

        let up_next = self.prefs.up_next_card();
        let next = group("Next episode", cx)
            .child(field(
                "Play next episode automatically",
                "When an episode ends, the one after it starts.",
                server_check("EnableNextEpisodeAutoPlay", true, self, cx),
                cx,
            ))
            .child(field(
                "Show the Up next card",
                "Near the end of a video, a card names what plays next.",
                checkbox("settings.check.upnext", up_next, cx).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        this.set_custom_prefs(
                            vec![("enableNextVideoInfoOverlay".to_string(), (!up_next).to_string())],
                            cx,
                        )
                    },
                )),
                cx,
            ));

        let skip = |id: &'static str, key: &'static str, secs: f64, cx: &mut Context<Self>| {
            select_field(
                id,
                format!("{} seconds", secs as u32),
                "30 seconds",
                move |app| {
                    let current = match key {
                        "skipBackLength" => app.prefs.skip_back_secs(),
                        _ => app.prefs.skip_forward_secs(),
                    } as u32;
                    SKIP_SECS
                        .iter()
                        .map(|secs| {
                            let secs = *secs;
                            Choice::new(format!("{secs} seconds"), secs == current, move |this, cx| {
                                this.set_custom_prefs(
                                    vec![(key.to_string(), (secs * 1000).to_string())],
                                    cx,
                                )
                            })
                        })
                        .collect()
                },
                cx,
            )
        };
        let buttons = group("Skip buttons", cx)
            .child(field(
                "Skip back length",
                "How far the back button of the player goes.",
                skip("settings.skip-back", "skipBackLength", self.prefs.skip_back_secs(), cx),
                cx,
            ))
            .child(field(
                "Skip forward length",
                "How far the forward button of the player goes.",
                skip(
                    "settings.skip-forward",
                    "skipForwardLength",
                    self.prefs.skip_forward_secs(),
                    cx,
                ),
                cx,
            ));
        let cap = self.config.max_bitrate;
        let cap_text = |bitrate: Option<u64>| match bitrate {
            Some(bitrate) => crate::stream::bitrate_label(bitrate),
            None => "Auto".to_string(),
        };
        let quality = group("Quality", cx).child(field(
            "Maximum streaming bitrate",
            "Above this the server transcodes the video. Auto plays the file as it is. The Quality entry of the player menu changes it too.",
            select_field(
                "settings.max-bitrate",
                cap_text(cap),
                "120 Mbps",
                move |app| {
                    let current = app.config.max_bitrate;
                    std::iter::once(None)
                        .chain(crate::stream::LADDER.iter().map(|rung| Some(rung.bitrate)))
                        .map(|bitrate| {
                            Choice::new(cap_text(bitrate), bitrate == current, move |this, cx| {
                                this.set_max_bitrate(bitrate, cx)
                            })
                        })
                        .collect()
                },
                cx,
            ),
            cx,
        ))
        .child(field(
            "Lower the quality on a slow connection",
            "Off: Auto always plays the file as it is. On: Auto measures the connection and picks a lower bitrate when the file would not keep up.",
            checkbox(
                "settings.check.adaptive-quality",
                self.config.adaptive_quality.unwrap_or(crate::adaptive::DEFAULT_ON),
                cx,
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_adaptive_quality(cx))),
            cx,
        ));
        let offset = self.config.sync_offset_ms;
        let offset_text = |ms: f64| match ms {
            ms if ms == 0. => "None".to_string(),
            ms if ms > 0. => format!("{ms:.0} ms later"),
            ms => format!("{:.0} ms earlier", -ms),
        };
        let sync = group("SyncPlay", cx)
            .child(field(
                "Keep in step with the group",
                "When this player drifts from the group, it plays a little faster or slower for a moment.",
                checkbox(
                    "settings.check.sync-correction",
                    self.config.sync_correction.unwrap_or(true),
                    cx,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_sync_correction(cx))),
                cx,
            ))
            .child(field(
                "Delay of this player",
                "For a sound output with a delay of its own, such as Bluetooth: play earlier by that delay.",
                select_field(
                    "settings.sync-offset",
                    offset_text(offset),
                    "250 ms earlier",
                    move |app| {
                        let current = app.config.sync_offset_ms;
                        [-250., -200., -150., -100., -50., 0., 50., 100., 150., 200., 250.]
                            .into_iter()
                            .map(|ms: f64| {
                                let text = match ms {
                                    ms if ms == 0. => "None".to_string(),
                                    ms if ms > 0. => format!("{ms:.0} ms later"),
                                    ms => format!("{:.0} ms earlier", -ms),
                                };
                                Choice::new(text, ms == current, move |this, cx| {
                                    this.set_sync_offset(ms, cx)
                                })
                            })
                            .collect()
                    },
                    cx,
                ),
                cx,
            ));
        let remote = group("Remote control", cx).child(field(
            "Allow other devices to control this app",
            "Another Jellyfin client signed in with this account can send items here and pause, seek or change the volume (\"Play On\").",
            checkbox(
                "settings.check.remote-control",
                self.remote_control_allowed(),
                cx,
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_remote_control(cx))),
            cx,
        ));
        div()
            .flex()
            .flex_col()
            .gap(px(18.))
            .child(audio)
            .child(quality)
            .child(next)
            .child(buttons)
            .child(sync)
            .child(remote)
    }

    pub(super) fn render_settings_subtitles(&self, cx: &mut Context<Self>) -> Div {
        let mode = self.prefs.text("SubtitleMode");
        let (_, mode_name, mode_text) = SUBTITLE_MODES
            .iter()
            .find(|(key, _, _)| *key == mode)
            .copied()
            .unwrap_or(SUBTITLE_MODES[0]);
        let language = group("Language", cx)
            .child(field(
                "Preferred subtitle language",
                "",
                language_select("settings.sub-language", "SubtitleLanguagePreference", self, cx),
                cx,
            ))
            .child(field(
                "Subtitle mode",
                mode_text,
                select_field(
                    "settings.sub-mode",
                    mode_name.to_string(),
                    "Only forced",
                    |app| {
                        let current = app.prefs.text("SubtitleMode");
                        SUBTITLE_MODES
                            .iter()
                            .map(|(key, name, _)| {
                                Choice::new(*name, current == *key, move |this, cx| {
                                    this.set_user_config(
                                        "SubtitleMode",
                                        Value::String(key.to_string()),
                                        cx,
                                    )
                                })
                            })
                            .collect()
                    },
                    cx,
                ),
                cx,
            ))
            .child(field(
                "Remember subtitle selections",
                "The server keeps the subtitle you chose for an item and names it again.",
                server_check("RememberSubtitleSelections", true, self, cx),
                cx,
            ));

        // The look is a setting of this app; the player applies it.
        let look = self.config.subtitle_look.clone();
        fn pick<T: Copy + PartialEq + 'static>(
            id: &'static str,
            options: &'static [(&'static str, Option<T>)],
            current: Option<T>,
            set: fn(&mut SubtitleLook, Option<T>),
            cx: &mut Context<Bloom>,
        ) -> Stateful<Div> {
            let name = options
                .iter()
                .find(|(_, value)| *value == current)
                .map_or("Custom", |(name, _)| name);
            let longest = options.iter().map(|(name, _)| *name).max_by_key(|n| n.len()).unwrap_or("");
            select_field(
                id,
                name.to_string(),
                longest,
                move |_| {
                    options
                        .iter()
                        .map(|(name, value)| {
                            let value = *value;
                            Choice::new(*name, value == current, move |this, cx| {
                                this.set_subtitle_look(|look| set(look, value), cx)
                            })
                        })
                        .collect()
                },
                cx,
            )
        }
        const FOLLOW: &str = "From Jellyfin Enhanced";
        const SIZES: [(&str, Option<f64>); 5] = [
            (FOLLOW, None),
            ("Small", Some(0.8)),
            ("Normal", Some(1.)),
            ("Large", Some(1.3)),
            ("Extra large", Some(1.6)),
        ];
        const COLORS: [(&str, Option<&str>); 5] = [
            (FOLLOW, None),
            ("White", Some("#FFFFFFFF")),
            ("Yellow", Some("#FFFFE000")),
            ("Green", Some("#FF7CFC00")),
            ("Cyan", Some("#FF00E5FF")),
        ];
        const BOXES: [(&str, Option<bool>); 3] =
            [(FOLLOW, None), ("Dark box", Some(true)), ("No box", Some(false))];
        const PLACES: [(&str, Option<i64>); 4] = [
            (FOLLOW, None),
            ("Bottom", Some(100)),
            ("Raised", Some(90)),
            ("High", Some(80)),
        ];
        let appearance = group("Appearance in this app", cx)
            .child(field(
                "Text size",
                "",
                pick("settings.sub-size", &SIZES, look.scale, |look, v| look.scale = v, cx),
                cx,
            ))
            .child(field(
                "Text colour",
                "",
                pick(
                    "settings.sub-color",
                    &COLORS,
                    // A colour outside the table shows as the first entry.
                    COLORS
                        .iter()
                        .find(|(_, value)| value.is_some() && *value == look.color.as_deref())
                        .and_then(|(_, value)| *value),
                    |look, v| look.color = v.map(str::to_string),
                    cx,
                ),
                cx,
            ))
            .child(field(
                "Background",
                "A dark box behind the text helps on bright pictures.",
                pick("settings.sub-box", &BOXES, look.background, |look, v| look.background = v, cx),
                cx,
            ))
            .child(field(
                "Position",
                "",
                pick("settings.sub-place", &PLACES, look.position, |look, v| look.position = v, cx),
                cx,
            ));
        div().flex().flex_col().gap(px(18.)).child(language).child(appearance)
    }

    pub(super) fn render_settings_home(&self, cx: &mut Context<Self>) -> Div {
        let t = UiTheme::read(cx).clone();
        let longest = HOME_SECTIONS
            .iter()
            .map(|(_, name)| *name)
            .max_by_key(|name| name.len())
            .unwrap_or("");
        let name_of = |kind: &str| {
            HOME_SECTIONS
                .iter()
                .find(|(known, _)| *known == kind)
                .map_or_else(|| kind.to_string(), |(_, name)| name.to_string())
        };
        let mut sections = group("Sections of the home page", cx).child(
            div()
                .pb(px(6.))
                .text_size(px(13.))
                .text_color(t.colors.foreground.opacity(0.6))
                .child("The home page shows them from top to bottom. The web client uses the same list."),
        );
        const SLOTS: usize = 10;
        for slot in 0..SLOTS {
            let kind = self.prefs.home_section(slot);
            // A move changes places with the neighbour, in one save.
            let swap = |other: usize| {
                let (here, there) = (kind.clone(), self.prefs.home_section(other));
                move |this: &mut Bloom, _: &ClickEvent, _: &mut gpui_kit::Window, cx: &mut Context<Bloom>| {
                    this.set_custom_prefs(
                        vec![
                            (format!("homesection{slot}"), there.clone()),
                            (format!("homesection{other}"), here.clone()),
                        ],
                        cx,
                    )
                }
            };
            let (first, last) = (slot == 0, slot + 1 == SLOTS);
            let mut up = icon_button(
                SharedString::from(format!("settings.home.up.{slot}")),
                LucideIcon::ChevronUp,
                !first,
                cx,
            )
            .tooltip(tip("Move up"));
            if !first {
                up = up.on_click(cx.listener(swap(slot - 1)));
            }
            let mut down = icon_button(
                SharedString::from(format!("settings.home.down.{slot}")),
                LucideIcon::ChevronDown,
                !last,
                cx,
            )
            .tooltip(tip("Move down"));
            if !last {
                down = down.on_click(cx.listener(swap(slot + 1)));
            }
            let pick = select(
                SharedString::from(format!("settings.home.{slot}")),
                name_of(&kind),
                longest,
                cx,
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                let current = this.prefs.home_section(slot);
                let choices = HOME_SECTIONS
                    .iter()
                    .map(|(kind, name)| {
                        Choice::new(*name, current == *kind, move |this, cx| {
                            this.set_custom_prefs(
                                vec![(format!("homesection{slot}"), kind.to_string())],
                                cx,
                            )
                        })
                    })
                    .collect();
                this.open_choices(choices, event.position(), window, cx);
            }));
            sections = sections.child(field(
                format!("Section {}", slot + 1),
                "",
                div().flex().items_center().gap(px(6.)).child(pick).child(up).child(down),
                cx,
            ));
        }
        div().flex().flex_col().gap(px(18.)).child(sections)
    }

    pub(super) fn render_settings_display(&self, cx: &mut Context<Self>) -> Div {
        let trailers = self.config.hero_video.unwrap_or(true);
        let tags = self.quality_tags();
        let home = group("Home", cx).child(field(
            "Trailers in the hero",
            "A trailer plays behind the title at the top of the home page.",
            checkbox("settings.check.trailers", trailers, cx)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_hero_video(cx))),
            cx,
        ));
        let glass = group("Glass", cx).child(field(
            "Liquid glass",
            "Panels and controls bend and take the colour of the picture behind them. Off gives the frosted glass of before.",
            checkbox("settings.check.glass", crate::ui::glass::liquid(), cx)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_liquid_glass(cx))),
            cx,
        ));
        let cards = group("Cards", cx).child(field(
            "Quality tags",
            "The tags the plugin sets on the posters, such as 4K, Dolby Vision, HEVC and Atmos.",
            checkbox("settings.check.tags", tags, cx)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_quality_tags(cx))),
            cx,
        ));
        let mut enhanced = group("Jellyfin Enhanced", cx);
        for (key, label) in crate::enhanced::TOGGLES {
            if !self.enhanced_toggle_shown(key) {
                continue;
            }
            enhanced = enhanced.child(field(
                label,
                "",
                checkbox(SharedString::from(format!("settings.check.je.{key}")), self.je(key), cx)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.toggle_enhanced(key, cx)
                    })),
                cx,
            ));
        }
        div()
            .flex()
            .flex_col()
            .gap(px(18.))
            .child(home)
            .child(glass)
            .child(cards)
            .child(enhanced)
    }
}
