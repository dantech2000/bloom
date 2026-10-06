// SPDX-License-Identifier: AGPL-3.0-or-later
//! Server dashboard for administrators: a sidebar of sections and one page
//! for each. A section lives in its own file and gives three things:
//! `Data` (what the page shows), `load` (reads it from the server on a
//! background thread) and `render` (builds the page). Actions that change the
//! server go through [`Bloom::admin_action`], and the risky ones first ask
//! with [`Bloom::ask_confirm`].

pub mod access;
pub mod activity;
pub mod api_keys;
pub mod branding;
pub mod config;
pub mod dashboard;
pub mod devices;
pub mod dialogs;
pub mod lazy;
pub mod libraries;
pub mod logs;
pub mod plugin_config;
pub mod plugins;
pub mod rows;
pub mod tasks;
pub mod triggers;
pub mod text_editor;
pub mod users;

use std::{
    collections::HashMap,
    rc::Rc,
    time::{Duration, Instant},
};

use anyhow::Result;
pub use dialogs::{Field, Prompt, Secret};
use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled, div,
    prelude::FluentBuilder as _, px, rgb, rgba,
};

use crate::{
    app::{Bloom, Page},
    jellyfin::Client,
    ui::{glass::glass, scroll_area::ScrollArea, theme::UiTheme},
    views::cards::icon,
};

/// Width of the section list at the left.
pub(crate) const SIDEBAR_W: f32 = 236.;
/// Time between two loads of the live part of the dashboard.
const LIVE_INTERVAL: Duration = Duration::from_secs(5);
/// Time between two redraws of a page whose data does not change.
const CLOCK_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    Dashboard,
    General,
    Branding,
    Users,
    Libraries,
    LibraryDisplay,
    LibraryMetadata,
    Nfo,
    Transcoding,
    Resume,
    Streaming,
    Trickplay,
    Devices,
    Activity,
    Plugins,
    Networking,
    Tasks,
    Logs,
    ApiKeys,
}

impl Section {
    /// Sections in sidebar order, with the heading each group starts with.
    pub const GROUPS: [(&'static str, &'static [Section]); 6] = [
        (
            "Server",
            &[Section::Dashboard, Section::General, Section::Branding, Section::Users],
        ),
        (
            "Libraries",
            &[
                Section::Libraries,
                Section::LibraryDisplay,
                Section::LibraryMetadata,
                Section::Nfo,
            ],
        ),
        (
            "Playback",
            &[Section::Transcoding, Section::Resume, Section::Streaming, Section::Trickplay],
        ),
        ("Devices", &[Section::Devices, Section::Activity]),
        ("Plugins", &[Section::Plugins]),
        (
            "Advanced",
            &[Section::Networking, Section::Tasks, Section::Logs, Section::ApiKeys],
        ),
    ];

    pub fn label(self) -> &'static str {
        match self {
            Section::Dashboard => "Dashboard",
            Section::General => "General",
            Section::Branding => "Branding",
            Section::Users => "Users",
            Section::Libraries => "Libraries",
            Section::LibraryDisplay => "Display",
            Section::LibraryMetadata => "Metadata",
            Section::Nfo => "NFO Settings",
            Section::Transcoding => "Transcoding",
            Section::Resume => "Resume",
            Section::Streaming => "Streaming",
            Section::Trickplay => "Trickplay",
            Section::Networking => "Networking",
            Section::Devices => "Devices",
            Section::Activity => "Activity",
            Section::Plugins => "Plugins",
            Section::Tasks => "Scheduled Tasks",
            Section::Logs => "Logs",
            Section::ApiKeys => "API Keys",
        }
    }

    fn glyph(self) -> LucideIcon {
        match self {
            Section::Dashboard => LucideIcon::LayoutGrid,
            Section::General => LucideIcon::Settings2,
            Section::Branding => LucideIcon::Palette,
            Section::Users => LucideIcon::Users,
            Section::Libraries => LucideIcon::Library,
            Section::LibraryDisplay => LucideIcon::Tv,
            Section::LibraryMetadata => LucideIcon::Tags,
            Section::Nfo => LucideIcon::FileText,
            Section::Transcoding => LucideIcon::Cpu,
            Section::Resume => LucideIcon::RotateCcw,
            Section::Streaming => LucideIcon::Gauge,
            Section::Trickplay => LucideIcon::Images,
            Section::Networking => LucideIcon::Network,
            Section::Devices => LucideIcon::Monitor,
            Section::Activity => LucideIcon::Bell,
            Section::Plugins => LucideIcon::Boxes,
            Section::Tasks => LucideIcon::Clock,
            Section::Logs => LucideIcon::Server,
            Section::ApiKeys => LucideIcon::Settings,
        }
    }

    /// Section of a name such as "users" (for the debug channel).
    pub fn from_name(name: &str) -> Option<Self> {
        let wanted = name.to_lowercase().replace([' ', '-', '_'], "");
        Section::GROUPS
            .iter()
            .flat_map(|(_, sections)| sections.iter().copied())
            .find(|s| s.label().to_lowercase().replace(' ', "") == wanted)
            .or(match wanted.as_str() {
                "tasks" => Some(Section::Tasks),
                "keys" => Some(Section::ApiKeys),
                "nfo" => Some(Section::Nfo),
                "network" => Some(Section::Networking),
                _ => None,
            })
    }
}

/// What one load brings back.
enum Loaded {
    Dashboard(dashboard::Data),
    Users(users::Data),
    Libraries(libraries::Data),
    Devices(devices::Data),
    Activity(activity::Data),
    Plugins(plugins::Data),
    Tasks(tasks::Data),
    Logs(logs::Data),
    ApiKeys(api_keys::Data),
    Config(Section, config::Data),
}

/// State of the admin page. The data of a section stays when the user goes to
/// another section, so a visit back shows it at once while it loads again.
pub struct AdminData {
    pub section: Section,
    pub loading: bool,
    pub error: Option<String>,
    pub dashboard: Option<dashboard::Data>,
    pub users: Option<users::Data>,
    pub libraries: Option<libraries::Data>,
    pub devices: Option<devices::Data>,
    pub activity: Option<activity::Data>,
    pub plugins: Option<plugins::Data>,
    pub tasks: Option<tasks::Data>,
    pub logs: Option<logs::Data>,
    pub api_keys: Option<api_keys::Data>,
    /// The access editor of one user, inside the Users section.
    pub access: Option<access::Editor>,
    /// The trigger editor of one task, inside the Tasks section.
    pub triggers: Option<triggers::Editor>,
    /// The configuration pages that were opened, each with its edits.
    pub config: HashMap<Section, config::Data>,
    /// Log file the user chose on the Logs page.
    pub log_open: Option<String>,
    /// The large text dialog, while it is open.
    pub text_editor: Option<text_editor::TextEditor>,
    /// The settings of one plugin, while the Plugins page shows them.
    pub plugin_editor: Option<plugin_config::Editor>,
    /// The open of a plugin editor that is wanted: the one asked for last,
    /// while the user did not close it. An answer for another open is
    /// dropped.
    pub plugin_open: Option<crate::app::Revision>,
    /// When the live reload last asked for a redraw.
    redrawn: Instant,
}

impl AdminData {
    pub fn new(section: Section) -> Self {
        Self {
            section,
            loading: true,
            error: None,
            dashboard: None,
            users: None,
            libraries: None,
            devices: None,
            activity: None,
            plugins: None,
            tasks: None,
            logs: None,
            api_keys: None,
            access: None,
            triggers: None,
            config: HashMap::new(),
            log_open: None,
            text_editor: None,
            plugin_editor: None,
            plugin_open: None,
            redrawn: Instant::now(),
        }
    }

    /// Whether a live reload needs a redraw: when the data changed, and once
    /// a minute anyway so that the "5 minutes ago" texts move on.
    fn redraw_due(&mut self, changed: bool) -> bool {
        if !changed && self.redrawn.elapsed() < CLOCK_INTERVAL {
            return false;
        }
        self.redrawn = Instant::now();
        true
    }
}

/// A question the user must answer before an action runs.
#[derive(Clone)]
pub struct Confirm {
    pub title: String,
    pub message: String,
    /// Text of the button that runs the action, such as "Restart".
    pub action: String,
    /// Paints the action button red.
    pub danger: bool,
    pub run: Rc<dyn Fn(&mut Bloom, &mut Context<Bloom>)>,
}

impl Bloom {
    /// True when the signed-in user may open the dashboard.
    pub fn is_admin(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.is_admin)
    }

    /// The state of the admin page while it is open.
    pub fn admin_data(&self) -> Option<&AdminData> {
        match &self.page {
            Page::Admin(data) => Some(data),
            _ => None,
        }
    }

    pub fn open_admin(&mut self, section: Section, cx: &mut Context<Self>) {
        if !self.is_admin() {
            return;
        }
        match &mut self.page {
            // Inside the dashboard a section change keeps the loaded data.
            Page::Admin(data) => {
                data.section = section;
                data.error = None;
                // A click in the list of sections shows the list of the
                // section; an editor with edits that are not saved stays.
                data.triggers = None;
                if data.access.as_ref().is_some_and(|editor| !editor.dirty()) {
                    data.access = None;
                }
                self.page_scroll
                    .set_offset(gpui_kit::point(px(0.), px(0.)));
                self.load_page(cx);
                cx.notify();
            }
            _ => self.navigate(Page::Admin(AdminData::new(section)), cx),
        }
    }

    /// Loads the data of the open section. `load_page` calls this.
    pub fn load_admin(&mut self, generation: u64, cx: &mut Context<Self>) {
        let Page::Admin(data) = &mut self.page else {
            return;
        };
        data.loading = true;
        let section = data.section;
        let log_open = data.log_open.clone();
        self.start_admin_tick(cx);
        self.fetch(
            cx,
            move |client| -> Result<Loaded> {
                let client = &client;
                Ok(match section {
                    Section::Dashboard => Loaded::Dashboard(dashboard::load(client)?),
                    Section::Users => Loaded::Users(users::load(client)?),
                    Section::Libraries => Loaded::Libraries(libraries::load(client)?),
                    Section::Devices => Loaded::Devices(devices::load(client)?),
                    Section::Activity => Loaded::Activity(activity::load(client)?),
                    Section::Plugins => Loaded::Plugins(plugins::load(client)?),
                    Section::Tasks => Loaded::Tasks(tasks::load(client)?),
                    Section::Logs => Loaded::Logs(logs::load(client, log_open.as_deref())?),
                    Section::ApiKeys => Loaded::ApiKeys(api_keys::load(client)?),
                    section => Loaded::Config(section, config::load(client, section)?),
                })
            },
            move |this, result, cx| {
                if this.generation != generation {
                    return;
                }
                if let Page::Admin(data) = &mut this.page {
                    data.loading = false;
                    match result {
                        Ok(loaded) => {
                            data.error = None;
                            match loaded {
                                Loaded::Dashboard(d) => data.dashboard = Some(d),
                                Loaded::Users(d) => data.users = Some(d),
                                Loaded::Libraries(d) => data.libraries = Some(d),
                                Loaded::Devices(d) => data.devices = Some(d),
                                Loaded::Activity(d) => data.activity = Some(d),
                                Loaded::Plugins(d) => data.plugins = Some(d),
                                Loaded::Tasks(d) => data.tasks = Some(d),
                                Loaded::Logs(d) => data.logs = Some(d),
                                Loaded::ApiKeys(d) => data.api_keys = Some(d),
                                // A page with edits keeps them; a visit
                                // back must not throw them away.
                                Loaded::Config(section, d) => {
                                    if !data.config.get(&section).is_some_and(|c| c.dirty()) {
                                        data.config.insert(section, d);
                                    }
                                }
                            }
                        }
                        Err(err) => data.error = Some(format!("{err:#}")),
                    }
                }
                cx.notify();
            },
        );
    }

    /// Keeps the dashboard current while it is open. The loop ends by itself
    /// when the user leaves the admin page.
    pub fn start_admin_tick(&mut self, cx: &mut Context<Self>) {
        if self.admin_tick.is_some() {
            return;
        }
        self.admin_tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(LIVE_INTERVAL).await;
                let open = this.update(cx, |this, cx| {
                    let open = matches!(this.page, Page::Admin(_));
                    if open && this.playing.is_none() {
                        this.refresh_admin(cx);
                    } else if !open {
                        this.admin_tick = None;
                    }
                    open
                });
                if !matches!(open, Ok(true)) {
                    break;
                }
            }
        }));
    }

    /// Loads the live part of the dashboard again. The page does not show
    /// that it loads, and an error keeps the data of the last load.
    fn refresh_admin(&mut self, cx: &mut Context<Self>) {
        let Page::Admin(data) = &self.page else {
            return;
        };
        // The poll waits while the server is offline; recovery loads the page.
        if data.loading || crate::connection::is_offline() {
            return;
        }
        let generation = self.generation;
        // A load that brings the same data back causes no redraw; see
        // `AdminData::redraw_due`.
        match data.section {
            Section::Dashboard if data.dashboard.is_some() => {}
            // The progress of a running task or library scan.
            Section::Tasks => {
                return self.fetch(
                    cx,
                    |client| tasks::load(&client),
                    move |this, result, cx| {
                        if let (Page::Admin(data), Ok(tasks)) = (&mut this.page, result)
                            && this.generation == generation
                        {
                            let changed = data.tasks.as_ref() != Some(&tasks);
                            data.tasks = Some(tasks);
                            if data.redraw_due(changed) {
                                cx.notify();
                            }
                        }
                    },
                );
            }
            Section::Libraries => {
                let previous = data
                    .libraries
                    .as_ref()
                    .map(|d| d.libraries.clone())
                    .unwrap_or_default();
                return self.fetch(
                    cx,
                    move |client| libraries::refresh(&client, &previous),
                    move |this, result, cx| {
                        if let (Page::Admin(data), Ok(libraries)) = (&mut this.page, result)
                            && this.generation == generation
                        {
                            let changed = data.libraries.as_ref() != Some(&libraries);
                            data.libraries = Some(libraries);
                            if data.redraw_due(changed) {
                                cx.notify();
                            }
                        }
                    },
                );
            }
            _ => return,
        }
        self.fetch(
            cx,
            |client| dashboard::refresh(&client),
            move |this, result, cx| {
                if this.generation != generation {
                    return;
                }
                if let (Page::Admin(data), Ok(live)) = (&mut this.page, result)
                    && let Some(dashboard) = &mut data.dashboard
                {
                    let changed = dashboard.live != live;
                    dashboard.live = live;
                    if data.redraw_due(changed) {
                        cx.notify();
                    }
                }
            },
        );
    }

    /// Shows a question; the action runs only when the user agrees.
    pub fn ask_confirm(&mut self, confirm: Confirm, cx: &mut Context<Self>) {
        self.admin_confirm = Some(confirm);
        cx.notify();
    }

    /// Runs a change on the server, tells the result in a toast, and loads
    /// the open section again.
    pub fn admin_action(
        &mut self,
        done: &'static str,
        cx: &mut Context<Self>,
        work: impl FnOnce(Client) -> Result<()> + Send + 'static,
    ) {
        self.fetch(cx, work, move |this, result, cx| {
            match result {
                Ok(()) => this.toast(done, "", cx),
                Err(err) => this.toast("The server refused the change", format!("{err:#}"), cx),
            }
            if matches!(this.page, Page::Admin(_)) {
                this.load_page(cx);
            }
            cx.notify();
        });
    }

    pub fn render_admin(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Page::Admin(data) = &self.page else {
            unreachable!()
        };
        let t = UiTheme::read(cx).clone();

        let mut sidebar = div().py(px(8.)).flex().flex_col();
        for (heading, sections) in Section::GROUPS {
            sidebar = sidebar.child(
                div()
                    .px(px(24.))
                    .pt(px(14.))
                    .pb(px(6.))
                    .text_size(px(13.))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(t.colors.foreground.opacity(0.5))
                    .child(heading),
            );
            for section in sections.iter().copied() {
                let selected = section == data.section;
                let color = if selected {
                    t.colors.primary_foreground
                } else {
                    t.colors.foreground
                };
                // The theme's `.navMenuOption`, as on the Settings page: a
                // rounded row with a round icon disc, the accent fill and
                // dark text when selected.
                sidebar = sidebar.child(
                    div()
                        .id(SharedString::from(format!("admin.nav.{section:?}")))
                        .mx(px(12.))
                        .mb(px(2.))
                        .px(px(14.))
                        .py(px(7.))
                        .rounded(px(12.))
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .cursor_pointer()
                        .text_size(px(16.))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(color)
                        .when(selected, |el| el.bg(t.colors.primary))
                        .when(!selected, |el| el.hover(|s| s.bg(rgba(0xf5f5f71f))))
                        .child(
                            div()
                                .size(px(34.))
                                .flex_shrink_0()
                                .rounded_full()
                                .bg(if selected { rgba(0x12121226) } else { rgba(0xcccccf69) })
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(icon(section.glyph(), 17., color)),
                        )
                        .child(section.label())
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.open_admin(section, cx)
                        })),
                );
            }
        }

        let page: Div = match data.section {
            Section::Dashboard => section_page(data, &data.dashboard, cx, |d, cx| {
                dashboard::render(self, d, cx)
            }),
            Section::Users => section_page(data, &data.users, cx, |d, cx| users::render(self, d, cx)),
            Section::Libraries => section_page(data, &data.libraries, cx, |d, cx| {
                libraries::render(self, d, cx)
            }),
            Section::Devices => {
                section_page(data, &data.devices, cx, |d, cx| devices::render(self, d, cx))
            }
            Section::Activity => {
                section_page(data, &data.activity, cx, |d, cx| activity::render(self, d, cx))
            }
            Section::Plugins => match &data.plugin_editor {
                Some(editor) => plugin_config::render(self, editor, cx),
                None => section_page(data, &data.plugins, cx, |d, cx| plugins::render(self, d, cx)),
            },
            Section::Tasks => section_page(data, &data.tasks, cx, |d, cx| tasks::render(self, d, cx)),
            Section::Logs => section_page(data, &data.logs, cx, |d, cx| logs::render(self, d, cx)),
            Section::ApiKeys => {
                section_page(data, &data.api_keys, cx, |d, cx| api_keys::render(self, d, cx))
            }
            section => {
                let loaded = data.config.get(&section);
                section_page(data, &loaded, cx, |d, cx| config::render(section, d, cx))
            }
        };

        div()
            .relative()
            .size_full()
            .flex()
            // The list is longer than a small window; it scrolls by itself.
            .child(
                div().w(px(SIDEBAR_W)).h_full().flex_shrink_0().child(
                    ScrollArea::new("admin.nav.scroll").size_full().child(sidebar),
                ),
            )
            .child(
                div().flex_1().min_w_0().h_full().child(
                    ScrollArea::new("admin.scroll")
                        .track(&self.page_scroll)
                        .size_full()
                        .child(
                            div()
                                .px(px(28.))
                                .pt(px(8.))
                                .pb(px(72.))
                                .flex()
                                .flex_col()
                                .gap(px(20.))
                                .child(
                                    div()
                                        .text_size(px(26.))
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(t.colors.foreground)
                                        .child(data.section.label()),
                                )
                                .child(page),
                        ),
                ),
            )
            .children(self.render_config_bar(cx))
            .children(self.render_plugin_bar(cx))
            .children(self.render_access_bar(cx))
            .children(self.render_confirm(cx))
            .children(self.render_prompt(cx))
            .children(self.render_secret(cx))
            .children(self.render_text_editor(cx))
    }

    /// The question dialog over the page.
    pub(crate) fn render_confirm(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let confirm = self.admin_confirm.clone()?;
        let t = UiTheme::read(cx).clone();
        let run = confirm.run.clone();
        Some(
            div()
                .id("admin.confirm")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x00000099))
                // A click beside the dialog closes it.
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.admin_confirm = None;
                    cx.notify();
                }))
                .child(
                    div()
                        .id("admin.confirm.card")
                        .relative()
                        .w(px(440.))
                        .rounded(px(24.))
                        .border_1()
                        .border_color(rgba(0xf5f5f733))
                        .child(glass(px(24.), crate::ui::glass::POPUP_TINT))
                        .p(px(24.))
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        .on_click(|_: &ClickEvent, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .text_size(px(20.))
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(t.colors.foreground)
                                .child(confirm.title.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(15.))
                                .text_color(t.colors.foreground.opacity(0.8))
                                .child(confirm.message.clone()),
                        )
                        .child(
                            div()
                                .mt(px(8.))
                                .flex()
                                .justify_end()
                                .gap(px(10.))
                                .child(
                                    button("admin.confirm.cancel", "Cancel", ButtonKind::Plain, cx)
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.admin_confirm = None;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    button(
                                        "admin.confirm.run",
                                        confirm.action.clone(),
                                        if confirm.danger {
                                            ButtonKind::Danger
                                        } else {
                                            ButtonKind::Primary
                                        },
                                        cx,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, _, cx| {
                                            this.admin_confirm = None;
                                            run(this, cx);
                                            cx.notify();
                                        },
                                    )),
                                ),
                        ),
                ),
        )
    }
}

/// The page of a section: its content, or a loading or error note.
fn section_page<D>(
    data: &AdminData,
    loaded: &Option<D>,
    cx: &mut Context<Bloom>,
    render: impl FnOnce(&D, &mut Context<Bloom>) -> Div,
) -> Div {
    let t = UiTheme::read(cx).clone();
    let note = |text: String| {
        div()
            .py(px(40.))
            .text_size(px(15.))
            .text_color(t.colors.muted_foreground)
            .child(text)
    };
    match (loaded, &data.error) {
        (Some(loaded), _) => render(loaded, cx),
        (None, Some(error)) => note(format!("Could not load this page: {error}")),
        (None, None) => note("Loading…".to_string()),
    }
}

// ----- shared widgets ---------------------------------------------------------

/// A titled card. Add the content with `.child(...)`.
pub fn panel(title: impl Into<SharedString>, cx: &Context<Bloom>) -> Div {
    let t = UiTheme::read(cx);
    card(cx).child(
        div()
            .mb(px(12.))
            .text_size(px(20.))
            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
            .text_color(t.colors.foreground)
            .child(title.into()),
    )
}

/// A plain card surface: the glass group of the theme, as the Settings
/// page draws it.
pub fn card(_cx: &Context<Bloom>) -> Div {
    div()
        .rounded(px(24.))
        .border_1()
        .border_color(rgba(0xf5f5f733))
        .bg(rgba(0x2a2a2ab0))
        .p(px(20.))
        .flex()
        .flex_col()
}

/// "Label    value" line for facts such as the server version.
pub fn fact(label: impl Into<SharedString>, value: impl Into<SharedString>, cx: &Context<Bloom>) -> Div {
    let t = UiTheme::read(cx);
    div()
        .py(px(5.))
        .flex()
        .items_start()
        .gap(px(16.))
        .text_size(px(15.))
        .child(
            div()
                .w(px(160.))
                .flex_shrink_0()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(t.colors.foreground)
                .child(label.into()),
        )
        .child(
            div()
                .min_w_0()
                .text_color(t.colors.foreground.opacity(0.8))
                .child(value.into()),
        )
}

/// The colour of a state, when a colour stands for one: green for "works"
/// or "live", amber for "needs a look", red for "failed" or "dangerous".
/// Any other colour gives `None`. The dashboard has one ink; a colour that
/// only tells one kind of thing from another is not used.
pub fn state_color(color: gpui_kit::Rgba) -> Option<gpui_kit::Rgba> {
    let (r, g, b) = (color.r, color.g, color.b);
    if g > r * 1.25 && g > b * 1.1 {
        Some(rgb(0x7ee787))
    } else if r > b * 2. && g > b * 1.4 && r >= g {
        Some(rgb(0xe3b341))
    } else if r > g * 1.7 && r > b * 1.7 {
        Some(rgb(0xff7b72))
    } else {
        None
    }
}

/// The quiet round disc icons and letters sit on, as in the navigation.
pub const DISC: u32 = 0xcccccf40;

/// A small label, such as a status ("Active") or a role ("Admin"). It is a
/// quiet pill; a colour that stands for a state shows as a dot before the
/// text.
pub fn badge(text: impl Into<SharedString>, color: gpui_kit::Rgba) -> Div {
    div()
        .px(px(9.))
        .py(px(2.))
        .rounded_full()
        .bg(rgba(0xffffff1f))
        .flex()
        .items_center()
        .gap(px(6.))
        .text_size(px(12.))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(rgb(0xf5f5f7))
        .children(state_color(color).map(|dot| div().size(px(6.)).rounded_full().bg(dot)))
        .child(text.into())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// Accent fill, for the main action of a page.
    Primary,
    /// Quiet outline.
    Plain,
    /// Red fill, for actions that stop or delete something.
    Danger,
}

/// A text button. Add `.on_click(...)`.
pub fn button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    cx: &Context<Bloom>,
) -> Stateful<Div> {
    let t = UiTheme::read(cx);
    // The theme's `.raised` button: dark, and the accent with dark text
    // under the pointer. The main action of a page has the accent already.
    let (bg, fg) = match kind {
        ButtonKind::Primary => (t.colors.primary, t.colors.primary_foreground),
        ButtonKind::Plain => (rgba(0x282828cc), t.colors.foreground),
        ButtonKind::Danger => (rgb(0xc62828), rgb(0xffffff)),
    };
    let (accent, ink) = (t.colors.primary, t.colors.primary_foreground);
    div()
        .id(id.into())
        .h(px(38.))
        .px(px(16.))
        .rounded(px(12.))
        .flex()
        .items_center()
        .gap(px(8.))
        .cursor_pointer()
        .bg(bg)
        .text_color(fg)
        .text_size(px(15.))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .hover(move |s| match kind {
            ButtonKind::Plain => s.bg(accent).text_color(ink),
            _ => s.opacity(0.88),
        })
        .child(label.into())
}

/// A server date ("2026-10-03T09:56:25.8420000Z") as a timestamp.
pub fn parse_date(date: &str) -> Option<jiff::Timestamp> {
    date.parse()
        .or_else(|_| format!("{date}Z").parse())
        .ok()
}

/// How long ago a server date was: "just now", "5 minutes ago", "3 days ago".
pub fn ago(date: &str) -> String {
    let Some(then) = parse_date(date) else {
        return String::new();
    };
    ago_seconds(jiff::Timestamp::now().as_second() - then.as_second())
}

/// "5 minutes ago" for a time that many seconds back.
pub fn ago_seconds(seconds: i64) -> String {
    let unit = |n: i64, name: &str| format!("{n} {name}{} ago", if n == 1 { "" } else { "s" });
    match seconds {
        i64::MIN..60 => "just now".to_string(),
        60..3600 => unit(seconds / 60, "minute"),
        3600..86_400 => unit(seconds / 3600, "hour"),
        86_400..2_592_000 => unit(seconds / 86_400, "day"),
        2_592_000..31_536_000 => unit(seconds / 2_592_000, "month"),
        _ => unit(seconds / 31_536_000, "year"),
    }
}
