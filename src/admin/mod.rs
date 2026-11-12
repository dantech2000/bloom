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
pub mod libraries;
pub mod logs;
pub mod plugin_config;
pub mod plugins;
pub mod tasks;
pub mod triggers;
pub mod text_editor;
pub mod users;

use crate::ui::tone::{self, State};
use std::{
    collections::HashMap,
    rc::Rc,
    time::{Duration, Instant},
};

use anyhow::Result;
pub use dialogs::{Field, Prompt, Secret};
use gpui_icons::LucideIcon;
use gpui_kit::{
    AnyElement, ClickEvent, Context, Div, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled, div,
    prelude::FluentBuilder as _, px, rgba,
};

use crate::{
    app::{Bloom, Page},
    jellyfin::Client,
    ui::{
        motion::{Enter, enter},
        scroll_area::ScrollArea,
        theme::UiTheme,
    },

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

/// The loads of the live part of the dashboard. One timer serves the app,
/// whichever dashboard page is open, so this is the app's and not a page's.
#[derive(Default)]
pub struct Live {
    /// A load is on its way: the timer starts no second one behind it, so a
    /// slow server does not collect requests.
    pub(crate) busy: bool,
    /// Names the load on its way: only its own answer ends the wait, not a
    /// late one of a session or a page before.
    pub(crate) load: u64,
    /// Timer that loads the live part again.
    pub(crate) tick: Option<gpui_kit::Task<()>>,
}

/// State of the admin page. The data of a section stays when the user goes to
/// another section, so a visit back shows it at once while it loads again.
pub struct AdminData {
    pub section: Section,
    /// A phone shows the list of sections as a page of its own, and the
    /// section when a row is tapped; the back arrow returns to the list.
    pub list: bool,
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
    /// The user whose page is open on the Users section (the id).
    pub user_open: Option<String>,
    /// When the live reload last asked for a redraw.
    redrawn: Instant,
}

impl AdminData {
    pub fn new(section: Section) -> Self {
        Self {
            section,
            list: false,
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
            user_open: None,
            redrawn: Instant::now(),
        }
    }

    /// The name of the user whose page is open, for the top bar of a phone.
    pub fn open_user_name(&self) -> Option<String> {
        let id = self.user_open.as_ref()?;
        self.users
            .as_ref()?
            .users
            .iter()
            .find(|user| &user.id == id)
            .map(|user| user.name.clone())
    }

    /// The title of the page inside a section on a phone: the name of the
    /// open user, or of the log file the user chose.
    pub fn subpage_title(&self) -> Option<String> {
        if let Some(name) = self.open_user_name() {
            return Some(name);
        }
        if self.section == Section::Logs {
            let open = self.log_open.as_ref()?;
            if self.logs.as_ref()?.open.as_ref() == Some(open) {
                return Some(open.clone());
            }
        }
        None
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

/// The form of one value (`Bloom::ask_value`).
#[derive(Default)]
pub struct Ask {
    pub title: String,
    pub message: String,
    /// The label of the field; the title when empty.
    pub label: String,
    pub placeholder: String,
    /// The text the field starts with.
    pub value: String,
    pub masked: bool,
    /// The form does not submit while the field is empty.
    pub required: bool,
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

    pub fn admin_data_mut(&mut self) -> Option<&mut AdminData> {
        match &mut self.page {
            Page::Admin(data) => Some(data),
            _ => None,
        }
    }

    /// Every dashboard page the app holds: the one on show, and those Back
    /// and Forward return to. The answer for an editor looks for it here:
    /// an editor on a page the user left kept "Saving…" for good when its
    /// answer looked at the page on show alone.
    pub(crate) fn admin_pages_mut(&mut self) -> impl Iterator<Item = &mut AdminData> {
        std::iter::once(&mut self.page)
            .chain(self.history.iter_mut())
            .chain(self.forward.iter_mut())
            .filter_map(|page| match page {
                Page::Admin(data) => Some(data),
                _ => None,
            })
    }

    pub fn open_admin(&mut self, section: Section, cx: &mut Context<Self>) {
        if !self.is_admin() {
            return;
        }
        match &mut self.page {
            // Inside the dashboard a section change keeps the loaded data.
            Page::Admin(data) => {
                data.section = section;
                data.list = false;
                data.error = None;
                // A question of the section before goes with it.
                self.dialogs.forget();
                // A click in the list of sections shows the list of the
                // section; an editor with edits that are not saved stays.
                data.triggers = None;
                data.user_open = None;
                if data.access.as_ref().is_some_and(|editor| !editor.dirty()) {
                    data.access = None;
                }
                if data.plugin_editor.as_ref().is_some_and(|editor| !editor.dirty()) {
                    data.plugin_editor = None;
                    data.plugin_open = None;
                }
                self.page_scroll
                    .set_offset(gpui_kit::point(px(0.), px(0.)));
                self.load_page(cx);
                cx.notify();
            }
            _ => {
                let mut data = AdminData::new(section);
                // A phone starts at the list of sections, as Settings does.
                data.list = self.compact_w();
                self.navigate(Page::Admin(data), cx)
            }
        }
    }

    /// On a phone, with a section open: back to the list of sections.
    /// False when the back arrow has to leave the dashboard instead.
    pub fn admin_back_to_list(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.compact_w() {
            return false;
        }
        let Page::Admin(data) = &mut self.page else {
            return false;
        };
        if data.list {
            return false;
        }
        // The pages inside a section close first: the access editor (when
        // it has nothing to save), then the page of a user.
        if data.access.as_ref().is_some_and(|editor| !editor.dirty()) {
            data.access = None;
            self.page_scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
            cx.notify();
            return true;
        }
        if data.user_open.is_some() {
            data.user_open = None;
            self.page_scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
            cx.notify();
            return true;
        }
        if data.section == Section::Logs && data.log_open.is_some() {
            data.log_open = None;
            self.page_scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
            cx.notify();
            return true;
        }
        data.list = true;
        self.page_scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
        cx.notify();
        true
    }

    /// Opens the "…" menu of a row or a card of the dashboard with these
    /// entries, at the point of the click. One menu serves every row: the
    /// entries are built by the row that was clicked.
    pub fn open_admin_menu(
        &mut self,
        items: Vec<crate::ui::menu::MenuItem>,
        position: gpui_kit::Point<gpui_kit::Pixels>,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        self.admin_menu.update(cx, |menu, cx| {
            menu.set_items(items, cx);
            menu.open_at(Some(position), window, cx);
        });
    }

    /// Opens the page of a user on the Users section.
    pub fn admin_open_user(&mut self, id: String, cx: &mut Context<Self>) {
        if let Page::Admin(data) = &mut self.page {
            data.user_open = Some(id);
            self.page_scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
            cx.notify();
        }
    }

    /// Back from the page of a user to the list.
    pub fn admin_close_user(&mut self, cx: &mut Context<Self>) {
        if let Page::Admin(data) = &mut self.page {
            data.user_open = None;
            self.page_scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
            cx.notify();
        }
    }

    /// Width of the content of a section: the window less the sidebar and
    /// the padding; on a phone the window less the side margins.
    pub(crate) fn admin_content_w(&self) -> f32 {
        if self.compact_w() {
            self.viewport_w - 32.
        } else {
            (self.viewport_w - SIDEBAR_W - 56.).max(320.)
        }
    }

    /// The edges of a bar at the foot of a section (the save bar of an
    /// editor): left, right and bottom. On a phone it sits over the tab bar.
    pub(crate) fn admin_bar_edges(&self) -> (f32, f32, f32) {
        if self.compact_w() {
            (16., 16., crate::views::shell::bottom_inset() + crate::views::shell::TABBAR_H + 12.)
        } else {
            (SIDEBAR_W + 28., 28., 20.)
        }
    }

    /// Loads the data of the open section. `load_page` calls this.
    pub fn load_admin(&mut self, cx: &mut Context<Self>) {
        let Page::Admin(data) = &mut self.page else {
            return;
        };
        data.loading = true;
        let section = data.section;
        let log_open = data.log_open.clone();
        self.start_admin_tick(cx);
        self.fetch_for_page(
            cx,
            move |client| -> Result<Loaded> {
                let client = &client;
                Ok(match section {
                    Section::Dashboard => Loaded::Dashboard(dashboard::load(client)?),
                    Section::Users => Loaded::Users(users::load(client)?),
                    Section::Libraries => Loaded::Libraries(libraries::load(client, &[])?),
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
                        Err(err) => data.error = Some(explain_load(&err)),
                    }
                }
                cx.notify();
            },
        );
    }

    /// Keeps the dashboard current while it is open. The loop ends by itself
    /// when the user leaves the admin page.
    pub fn start_admin_tick(&mut self, cx: &mut Context<Self>) {
        if self.admin_live.tick.is_some() {
            return;
        }
        self.admin_live.tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(LIVE_INTERVAL).await;
                let open = this.update(cx, |this, cx| {
                    let open = matches!(this.page, Page::Admin(_));
                    if open && this.playing.is_none() {
                        this.refresh_admin(cx);
                    } else if !open {
                        this.admin_live.tick = None;
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
        if data.loading || crate::connection::is_offline() || self.admin_live.busy || self.session.is_none() {
            return;
        }
        // The sections with a live part.
        match data.section {
            Section::Dashboard if data.dashboard.is_some() => {
                self.live_load(cx, |client| dashboard::refresh(&client), |data, live| {
                    let Some(dashboard) = &mut data.dashboard else {
                        return false;
                    };
                    let changed = dashboard.live != live;
                    dashboard.live = live;
                    changed
                })
            }
            // The progress of a running task or library scan.
            Section::Tasks => self.live_load(cx, |client| tasks::load(&client), |data, tasks| {
                let changed = data.tasks.as_ref() != Some(&tasks);
                data.tasks = Some(tasks);
                changed
            }),
            Section::Libraries => {
                let previous = data
                    .libraries
                    .as_ref()
                    .map(|d| d.libraries.clone())
                    .unwrap_or_default();
                self.live_load(
                    cx,
                    move |client| libraries::load(&client, &previous),
                    |data, libraries| {
                        let changed = data.libraries.as_ref() != Some(&libraries);
                        data.libraries = Some(libraries);
                        changed
                    },
                )
            }
            _ => {}
        }
    }

    /// One load of the live part: `work` reads it, and `take` puts it in
    /// the page and says whether it changed. Its answer ends the wait.
    fn live_load<T: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        work: impl FnOnce(Client) -> Result<T> + Send + 'static,
        take: impl FnOnce(&mut AdminData, T) -> bool + 'static,
    ) {
        self.admin_live.busy = true;
        self.admin_live.load += 1;
        let (load, generation) = (self.admin_live.load, self.generation);
        self.fetch(cx, work, move |this, result, cx| {
            if this.admin_live.load == load {
                this.admin_live.busy = false;
            }
            // A load that brings the same data back causes no redraw; see
            // `AdminData::redraw_due`.
            if let (Page::Admin(data), Ok(value)) = (&mut this.page, result)
                && this.generation == generation
            {
                let changed = take(data, value);
                if data.redraw_due(changed) {
                    cx.notify();
                }
            }
        });
    }

    /// Shows a question; the action runs only when the user agrees.
    pub fn ask_confirm(&mut self, confirm: Confirm, cx: &mut Context<Self>) {
        // One popup at a time: a panel does not stay beside the question.
        self.close_panels();
        self.dialogs.confirm = Some(confirm);
        cx.notify();
    }

    /// Shows a form with one field; `run` gets its text when the user
    /// submits. The editors ask for a value this way.
    pub fn ask_value(
        &mut self,
        ask: Ask,
        cx: &mut Context<Self>,
        run: impl Fn(&mut Bloom, &str, &mut Context<Bloom>) + 'static,
    ) {
        self.ask_prompt(
            Prompt {
                message: ask.message,
                action: "Set".to_string(),
                fields: vec![Field {
                    label: if ask.label.is_empty() { ask.title.clone() } else { ask.label },
                    placeholder: ask.placeholder,
                    value: ask.value,
                    masked: ask.masked,
                    required: ask.required,
                }],
                title: ask.title,
                run: Rc::new(move |this, values, cx| {
                    run(this, values.first().map(String::as_str).unwrap_or_default(), cx)
                }),
            },
            cx,
        );
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
        let compact = self.compact_w();
        // A phone: the rows have the size of a touch target, and the list
        // fills the page.
        let row_h = if compact { 44. } else { 0. };

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
                // The row of the Settings page, a little smaller.
                sidebar = sidebar.child(
                    crate::ui::form::nav_row(
                        SharedString::from(format!("admin.nav.{section:?}")),
                        section.glyph(),
                        section.label(),
                        section == data.section,
                        (34., 17., 16.),
                        cx,
                    )
                    .mx(px(12.))
                    .py(px(7.))
                    .min_h(px(row_h))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_admin(section, cx))),
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

        // A phone shows one of the two: the list, or the section with the
        // top bar's title (the back arrow returns to the list).
        let (show_list, show_page) = if compact { (data.list, !data.list) } else { (true, true) };
        // How the page comes in: pushed from the right on a phone (the
        // section from the list, a user or a log from the section), a fade
        // on the Mac where the sidebar stays. The key names the page, so a
        // change of section or of subpage runs it again.
        let page_key = SharedString::from(format!(
            "admin.page.{:?}.{}.{}.{}{}{}",
            data.section,
            data.user_open.as_deref().unwrap_or(""),
            data.log_open.as_deref().unwrap_or(""),
            data.access.is_some() as u8,
            data.plugin_editor.is_some() as u8,
            data.triggers.is_some() as u8,
        ));
        let page = enter(page, page_key, if compact { Enter::Push } else { Enter::Fade }, cx);
        div()
            .relative()
            .size_full()
            .flex()
            // The list is longer than a small window; it scrolls by itself.
            .when(show_list, |el| {
                el.child(
                    div()
                        .when(!compact, |el| el.w(px(SIDEBAR_W)))
                        .when(compact, |el| el.flex_1().min_w_0())
                        .h_full()
                        .flex_shrink_0()
                        .child(
                            ScrollArea::new("admin.nav.scroll")
                                .size_full()
                                .child(sidebar.when(compact, |el| el.px(px(4.)).pb(px(72.)))),
                        )
                        // Back from a section on a phone: the list fades in.
                        .map(|el| if compact { enter(el, "admin.list", Enter::Fade, cx) } else { el.into_any_element() }),
                )
            })
            .when(show_page, |el| {
                el.child(
                    div().flex_1().min_w_0().h_full().child(
                        ScrollArea::new("admin.scroll")
                            .track(&self.page_scroll)
                            .size_full()
                            .child(
                                div()
                                    .px(px(if compact { 16. } else { 28. }))
                                    .pt(px(8.))
                                    .pb(px(72.))
                                    .flex()
                                    .flex_col()
                                    .gap(px(20.))
                                    .when(!compact, |el| {
                                        el.child(
                                            div()
                                                .text_size(px(26.))
                                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                                .text_color(t.colors.foreground)
                                                .child(data.section.label()),
                                        )
                                    })
                                    .child(page),
                            ),
                    ),
                )
            })
            .children(self.render_config_bar(cx))
            .children(self.render_plugin_bar(cx))
            .children(self.render_access_bar(cx))
            .children(self.render_prompt(cx))
            .children(self.render_secret(cx))
            .children(self.render_text_editor(cx))
    }

    /// The question dialog over the page.
    pub(crate) fn render_confirm(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let confirm = self.dialogs.confirm.clone()?;
        let run = confirm.run.clone();
        let card = crate::ui::dialog::titled("admin.confirm.card", &confirm.title, &confirm.message, cx)
                        .child(
                            div()
                                .mt(px(8.))
                                .flex()
                                .justify_end()
                                .gap(px(10.))
                                .child(
                                    button("admin.confirm.cancel", "Cancel", ButtonKind::Plain, cx)
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.dialogs.confirm = None;
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
                                            this.dialogs.confirm = None;
                                            run(this, cx);
                                            cx.notify();
                                        },
                                    )),
                                ),
                        );
        // A click beside the dialog closes it.
        let layer = crate::ui::dialog::layer("admin.confirm", card, cx).on_click(cx.listener(
            |this, _: &ClickEvent, _, cx| {
                this.dialogs.confirm = None;
                cx.notify();
            },
        ));
        Some(crate::ui::dialog::shown(layer, "admin.confirm", cx))
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
    match (loaded, &data.error) {
        (Some(loaded), _) => render(loaded, cx),
        // The failure in plain words, and a way to try again without
        // leaving the page (the raw error and no button, before 2026-10-23).
        (None, Some(error)) => card(cx)
            .max_w(px(560.))
            .gap(px(14.))
            .child(
                div()
                    .text_size(px(17.))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(t.colors.foreground)
                    .child(format!("{} could not load", data.section.label())),
            )
            .child(
                div()
                    .text_size(px(15.))
                    .line_height(px(22.))
                    .text_color(t.colors.foreground.opacity(0.7))
                    .child(error.clone()),
            )
            .child(
                div().flex().child(
                    button("admin.retry", "Try again", ButtonKind::Primary, cx)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            if let Page::Admin(data) = &mut this.page {
                                data.error = None;
                            }
                            this.load_page(cx);
                            cx.notify();
                        })),
                ),
            ),
        (None, None) => skeleton(data.section),
    }
}

/// What a failed load says, in plain words, by what the error was: the
/// server away, the session gone, the page missing on this version, the
/// server in trouble, or an answer the app did not understand.
fn explain_load(err: &anyhow::Error) -> String {
    use crate::connection::{Verdict, Why, classify, plain_words};
    let text = format!("{err:#}");
    match classify(err) {
        Verdict::Unreachable(Why::Gateway(code)) => format!(
            "The server is offline behind its proxy ({code}): the proxy answers, but the server \
             behind it does not."
        ),
        Verdict::Unreachable(why) => {
            format!("The server does not answer. {} Check your network connection.", plain_words(why))
        }
        Verdict::Unauthorized => {
            "The server refused the request (401): the session is no longer valid. Sign in again."
                .to_string()
        }
        Verdict::Answered if text.contains("HTTP 403") => {
            "The server refused the request (403): the account may not be an administrator.".to_string()
        }
        Verdict::Answered if text.contains("HTTP 404") => {
            "This page is not available on this version of the server.".to_string()
        }
        Verdict::Answered if text.contains("HTTP 5") => {
            "The server has a problem at the moment. Try again in a little while.".to_string()
        }
        Verdict::Answered if text.contains("decode") => {
            "The server answered, but not in a way Bloom understands.".to_string()
        }
        Verdict::Answered => text,
    }
}

/// The shape of a section while it loads: a line of counts and two
/// cards, as most sections are, so the page does not jump when the data
/// comes. Quiet glass, no text.
fn skeleton(section: Section) -> Div {
    let block = |w: f32, h: f32| surface(div()).w(px(w)).h(px(h)).opacity(0.45);
    let compact = crate::views::shell::compact_now();
    let wide = !compact && !matches!(section, Section::Logs);
    div()
        .flex()
        .flex_col()
        .gap(px(18.))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(12.))
                .child(block(128., 56.))
                .child(block(160., 56.))
                .when(wide, |el| el.child(block(140., 56.)).child(block(176., 56.))),
        )
        .child(surface(div()).w_full().h(px(if compact { 220. } else { 180. })).opacity(0.45))
        .child(surface(div()).w_full().h(px(if compact { 320. } else { 300. })).opacity(0.3))
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

/// The surface of every card of the dashboard: 24 px corners, the light
/// edge, and the glass of the theme. With liquid glass it is a glass panel,
/// as a group of the Settings page; with the frosted look it is the flat
/// tint of before.
pub fn surface<E: Styled + gpui_kit::ParentElement + gpui_kit::prelude::FluentBuilder>(el: E) -> E {
    let liquid = crate::ui::glass::liquid();
    el.rounded(px(24.))
        .border_1()
        .border_color(tone::EDGE)
        .when(!liquid, |el| el.bg(tone::CARD))
        .when(liquid, |el| el.relative().child(crate::ui::glass::card_glass(px(24.), crate::ui::glass::POPUP_TINT)))
}

/// The field that finds an entry of a long list (users, tasks, log files):
/// the whole width, a plain keyboard, the sizes of the platform.
pub fn find_field(
    state: &gpui_kit::Entity<crate::ui::input::InputState>,
    label: &'static str,
    compact: bool,
) -> crate::ui::input::Input {
    crate::ui::input::Input::new(state)
        .keyboard(crate::ui::input::Keyboard::plain())
        .aria_label(label)
        .w_full()
        .h(px(if compact { 50. } else { 44. }))
        .when(compact, |el| el.text_size(px(17.)).line_height(px(22.)))
}

/// Entries this many or more get a field to find one.
pub const FIND_FROM: usize = 10;

/// A line that says a list has nothing for the query.
pub fn nothing_found(what: &str, query: &str, cx: &Context<Bloom>) -> Div {
    let t = UiTheme::read(cx);
    div()
        .h(px(60.))
        .px(px(16.))
        .flex()
        .items_center()
        .text_size(px(15.))
        .text_color(t.colors.foreground.opacity(0.6))
        .child(format!("No {what} named \u{201c}{query}\u{201d}."))
}

/// The "…" button of a row or a card: the actions that are not the main
/// one go into its menu (`Bloom::open_admin_menu`). Add `.on_click(...)`
/// that builds the entries and opens the menu at `event.position()`.
pub fn more_button(id: impl Into<SharedString>, cx: &Context<Bloom>) -> Stateful<Div> {
    let t = UiTheme::read(cx);
    div()
        .id(id.into())
        .role(gpui_kit::Role::Button)
        .aria_label("More")
        .size(px(38.))
        .flex_shrink_0()
        .rounded(px(12.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .bg(tone::RAISED)
        .hover(|s| s.bg(rgba(0xffffff2e)))
        .tooltip(crate::ui::tip::tip("More"))
        .child(crate::icons::icon(LucideIcon::Ellipsis, 16., t.colors.foreground))
}

/// A plain card surface: the glass group of the theme, as the Settings
/// page draws it.
pub fn card(_cx: &Context<Bloom>) -> Div {
    surface(div()).p(px(20.))
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

/// A small label, such as a status ("Active") or a role ("Admin"). It is a
/// quiet pill; a state shows as a dot of its colour before the text. The
/// dashboard has one ink: a kind of thing has no colour.
pub fn badge(text: impl Into<SharedString>, state: Option<State>) -> Div {
    div()
        .px(px(9.))
        .py(px(2.))
        .rounded_full()
        .bg(tone::HOVER)
        .flex()
        .items_center()
        .gap(px(6.))
        .text_size(px(12.))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(tone::INK)
        .children(state.map(|state| div().size(px(6.)).rounded_full().bg(state.color())))
        .child(text.into())
}

pub use bloom_core::draft::{Draft, with_changes};
pub use crate::ui::button::{ButtonKind, button};

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

/// A value as the text of its field: a text as it is, a list with commas
/// between its entries, nothing for null.
pub fn text_of(value: &serde_json::Value) -> String {
    use serde_json::Value;
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        Value::Array(items) => items.iter().map(text_of).collect::<Vec<_>>().join(", "),
        other => other.to_string(),
    }
}

/// True or false from a word the user typed.
pub fn parse_flag(text: &str) -> Option<bool> {
    match text.trim().to_lowercase().as_str() {
        "true" | "on" | "yes" | "1" => Some(true),
        "false" | "off" | "no" | "0" => Some(false),
        _ => None,
    }
}

/// A number the user typed. "nan" and "inf" parse as numbers; neither is
/// one for a setting.
pub fn parse_number(text: &str) -> Option<f64> {
    text.trim().parse::<f64>().ok().filter(|number| number.is_finite())
}

/// The entries of a list the user typed, with commas or new lines between
/// them.
pub fn list_entries(text: &str) -> impl Iterator<Item = &str> {
    text.split([',', '\n']).map(str::trim).filter(|entry| !entry.is_empty())
}

/// A box that shows one of a set of values by its label; a click opens the
/// menu of the options, and `put` gets the value the user chose.
pub fn choice_box(
    id: impl Into<SharedString>,
    options: Vec<(serde_json::Value, SharedString)>,
    current: serde_json::Value,
    cx: &mut Context<Bloom>,
    put: impl Fn(&mut Bloom, &serde_json::Value, &mut Context<Bloom>) + 'static,
) -> gpui_kit::base::Button {
    use crate::{settings::Choice, ui::form::select};
    let shown = options
        .iter()
        .find(|(option, _)| *option == current)
        .map_or_else(|| SharedString::from(text_of(&current)), |(_, label)| label.clone());
    let longest = options
        .iter()
        .map(|(_, label)| label.as_ref())
        .max_by_key(|label| label.chars().count())
        .unwrap_or_default()
        .to_string();
    let put = Rc::new(put);
    select(id, shown, &longest, cx).on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
        let choices = options
            .iter()
            .map(|(option, label)| {
                let (option, put) = (option.clone(), put.clone());
                Choice::new(label.to_string(), option == current, move |this, cx| put(this, &option, cx))
            })
            .collect();
        this.open_choices(choices, event.position(), window, cx);
    }))
}

/// The top of a page inside a section (the access of a user, the triggers
/// of a task): the way back and the title beside it. Add the cards.
pub fn sub_page(back: Stateful<Div>, title: impl IntoElement) -> Div {
    div()
        .max_w(px(920.))
        .flex()
        .flex_col()
        .gap(px(18.))
        .child(div().flex().items_center().gap(px(14.)).child(back).child(title))
}

/// What such a page shows in place of its cards while it loads, or when
/// the load failed (`failed` says what could not be read).
pub fn load_note(loading: bool, error: Option<&str>, failed: &str, cx: &Context<Bloom>) -> Option<Div> {
    let text = match error {
        Some(error) => format!("{failed}: {error}"),
        None if loading => "Loading…".to_string(),
        None => return None,
    };
    let t = UiTheme::read(cx);
    Some(div().py(px(30.)).text_size(px(15.)).text_color(t.colors.muted_foreground).child(text))
}

#[cfg(test)]
mod draft_tests {
    use super::{Draft, list_entries, parse_flag, parse_number, text_of};
    use serde_json::json;

    /// The answer of a save is the new baseline, and an edit made while
    /// the save was on its way stays an edit.
    #[test]
    fn an_edit_after_the_send_stays_when_the_answer_comes() {
        let mut draft = Draft::new(json!({ "A": 1, "B": 2 }));
        draft.edited["A"] = json!(5);
        assert!(draft.dirty());
        let sent = draft.edited.clone();
        let body = draft.body(&json!({ "A": 1, "B": 2, "New": true }));
        assert_eq!(body, json!({ "A": 5, "B": 2, "New": true }));
        // The user goes on while the save is on its way.
        draft.edited["B"] = json!(9);
        draft.saving = true;
        draft.saved(body.clone(), &sent);
        assert_eq!(draft.original, body);
        assert_eq!(draft.edited, json!({ "A": 5, "B": 9, "New": true }));
        assert!(draft.dirty() && !draft.saving);
        draft.discard();
        assert!(!draft.dirty());
    }

    #[test]
    fn typed_words_make_values() {
        assert_eq!(parse_flag(" Yes "), Some(true));
        assert_eq!(parse_flag("0"), Some(false));
        assert_eq!(parse_flag("maybe"), None);
        assert_eq!(parse_number(" 1.5 "), Some(1.5));
        assert_eq!(parse_number("nan"), None);
        assert_eq!(parse_number("inf"), None);
        assert_eq!(list_entries("a,\n b ,").collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(text_of(&json!(["a", 2])), "a, 2");
        assert_eq!(text_of(&json!(null)), "");
        assert_eq!(text_of(&json!(1.5)), "1.5");
    }
}

#[cfg(test)]
mod live_tests {
    use gpui_kit::TestAppContext;

    use super::{AdminData, Section};
    use crate::app::{Page, race_harness::{MockServer, app, plain, session}};

    /// The timer of the live part starts no load behind one that is still
    /// on its way: a slow server got a new request at each tick.
    #[gpui_kit::test]
    fn one_live_load_at_a_time(cx: &mut TestAppContext) {
        let server = MockServer::start(|method, path, _| match (method, path) {
            ("GET", p) if p.starts_with("/ScheduledTasks") => (200, "[]".to_string()),
            _ => plain(method, path),
        });
        let (bloom, cx) = app(cx);
        bloom.update(cx, |this, cx| {
            this.session = Some(session(&server.url, "u1"));
            let mut admin = AdminData::new(Section::Tasks);
            admin.loading = false;
            this.page = Page::Admin(admin);
            // Three ticks before the first answer.
            this.refresh_admin(cx);
            this.refresh_admin(cx);
            this.refresh_admin(cx);
            assert!(this.admin_live.busy);
        });
        cx.run_until_parked();
        assert_eq!(server.count("GET", "/ScheduledTasks"), 1);
        // The answer is in: the next tick loads again.
        bloom.update(cx, |this, cx| {
            assert!(!this.admin_live.busy);
            this.refresh_admin(cx);
        });
        cx.run_until_parked();
        assert_eq!(server.count("GET", "/ScheduledTasks"), 2);
    }
}

