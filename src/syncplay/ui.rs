// SPDX-License-Identifier: AGPL-3.0-or-later
//! SyncPlay in the app: the threads that feed the session, what the app
//! does with its answers, and the panel of the groups.

use std::{collections::HashMap, time::Duration};

use gpui_icons::LucideIcon;
use gpui_kit::{
    ClickEvent, Context, Div, InteractiveElement as _, MouseButton, MouseDownEvent,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled, Task, Window, div,
    prelude::FluentBuilder as _, px, rgba,
};

use super::{
    core::{Intent, Notice},
    protocol::{GroupInfo, GroupState},
    session::{Session, Ui},
};
use crate::{
    app::{Bloom, Page},
    jellyfin::Item,
    realtime::{self, Realtime},
    ui::{glass::glass, scroll_area::ScrollArea, theme::UiTheme, tip::tip},
    views::cards::icon,
};

/// Width of the panel.
const PANEL_W: f32 = 420.;

/// SyncPlay as the app holds it.
#[derive(Default)]
pub struct SyncState {
    pub session: Option<Session>,
    realtime: Option<Realtime>,
    tasks: Vec<Task<()>>,
    /// What the server lets this user do: "CreateAndJoinGroups",
    /// "JoinGroups" or "None".
    pub access: String,
    pub panel_open: bool,
    /// The groups of the server, for the panel.
    pub groups: Vec<GroupInfo>,
    pub groups_loading: bool,
    /// Titles of the items of the queue, by item id.
    pub titles: HashMap<String, String>,
    /// When a click outside the panel closed it.
    pub panel_closed: Option<std::time::Instant>,
    /// The player must get the keyboard focus at the next frame.
    pub focus_player: bool,
    /// Waits for the news of the server to go quiet before a page loads
    /// again; a scan of the library sends a message for every few items.
    news: Option<Task<()>>,
    /// The news that waits is about the library, not only about the user.
    news_library: bool,
    /// News waits for its quiet moment.
    news_pending: bool,
    /// When the first message that no reload has handled arrived.
    news_first: Option<std::time::Instant>,
    /// A reload came due while the player was open; it runs when the
    /// player closes.
    news_owed: bool,
    /// What the news did, for the debug command `events`.
    pub news_stats: crate::news::NewsStats,
    /// The load of the group's item whose answer is wanted, see
    /// `sync_now_playing`.
    now_playing_load: crate::app::Revision,
}

impl SyncState {
    pub fn in_group(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.core.in_group())
    }

    pub fn following(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.core.following())
    }

    /// The group has the player: its end and its next item are the group's
    /// to decide, and the app must not close it.
    pub fn holds_player(&self) -> bool {
        self.session.as_ref().is_some_and(|s| {
            s.core.following() && (s.core.loaded().is_some() || s.core.phase_name() == "loading")
        })
    }

    pub fn allowed(&self) -> bool {
        matches!(self.access.as_str(), "CreateAndJoinGroups" | "JoinGroups")
    }

    /// Sends a message over the socket of the server, such as a
    /// subscription. False without a socket.
    pub fn socket_send(&self, kind: &str, data: Option<serde_json::Value>) -> bool {
        match &self.realtime {
            Some(realtime) => {
                realtime.send(kind, data);
                true
            }
            None => false,
        }
    }
}

impl Bloom {
    /// Opens the socket of the server and starts the session of SyncPlay,
    /// for the user who signed in.
    pub fn start_sync(&mut self, cx: &mut Context<Self>) {
        self.stop_sync();
        let Some(client) = self.session.as_ref().map(|s| s.client.clone()) else {
            return;
        };
        // A test instance on the config of the user would be the same
        // session on the server and take its socket messages away.
        if std::env::var_os("BLOOM_NO_SOCKET").is_some() {
            return;
        }
        let (realtime, socket) = realtime::start(client.clone());
        let (mut session, exchanges) = Session::new(client, self.player.clone());
        session.core.correct = self.config.sync_correction.unwrap_or(true);
        session.clock.extra_offset = -self.config.sync_offset_ms;
        let events = self.player.events();
        self.sync.realtime = Some(realtime);
        self.sync.session = Some(session);
        self.sync.tasks = vec![
            cx.spawn(async move |this, cx| {
                while let Ok(event) = socket.recv().await {
                    let fed = this.update(cx, |this, cx| this.on_socket_event(event, cx));
                    if fed.is_err() {
                        break;
                    }
                }
            }),
            cx.spawn(async move |this, cx| {
                while let Ok(exchange) = exchanges.recv().await {
                    let fed = this.update(cx, |this, cx| {
                        let ui = this.sync.session.as_mut().map(|s| s.exchange(exchange));
                        this.sync_apply(ui.unwrap_or_default(), cx);
                    });
                    if fed.is_err() {
                        break;
                    }
                }
            }),
            cx.spawn(async move |this, cx| {
                while let Ok(event) = events.recv().await {
                    let fed = this.update(cx, |this, cx| {
                        let ui = this.sync.session.as_mut().map(|s| s.player_event(event));
                        this.sync_apply(ui.unwrap_or_default(), cx);
                    });
                    if fed.is_err() {
                        break;
                    }
                }
            }),
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_millis(250)).await;
                    let fed = this.update(cx, |this, cx| {
                        let ui = this.sync.session.as_mut().map(|s| s.tick());
                        this.sync_apply(ui.unwrap_or_default(), cx);
                        // The numbers of the open panel move.
                        if this.sync.panel_open && this.sync.in_group() {
                            cx.notify();
                        }
                    });
                    if fed.is_err() {
                        break;
                    }
                }
            }),
        ];
    }

    /// A message of the socket: a real one, or one the debug command
    /// `socket-inject` makes. Both go the same way.
    pub fn on_socket_event(&mut self, event: realtime::SocketEvent, cx: &mut Context<Self>) {
        if let realtime::SocketEvent::Message { kind, .. } = &event {
            self.sync.news_stats.message(kind);
        }
        // Remote control reads the socket as well.
        self.cast_socket(&event, cx);
        // A socket that opens again may mean another network.
        self.adaptive_socket(&event);
        // A lost socket asks whether the server is there; one that opens
        // again ends the offline state.
        self.connection_socket(&event, cx);
        let ui = match self.sync.session.as_mut() {
            Some(session) => session.socket(event),
            // No session (the socket is off in a test instance): the news
            // still reaches the pages.
            None => match event {
                realtime::SocketEvent::Message { kind, data } => vec![Ui::Server { kind, data }],
                _ => Vec::new(),
            },
        };
        self.sync_apply(ui, cx);
    }

    /// Ends the session: on sign-out, and before a new one starts.
    pub fn stop_sync(&mut self) {
        if let Some(session) = &mut self.sync.session
            && session.core.in_group()
        {
            session.user(Intent::Leave);
        }
        self.sync.tasks.clear();
        self.sync.session = None;
        self.sync.realtime = None;
        self.sync.groups.clear();
        self.sync.panel_open = false;
    }

    /// Gives an action of the user to the session and does what comes back.
    pub fn sync_user(&mut self, intent: Intent, cx: &mut Context<Self>) {
        let ui = self.sync.session.as_mut().map(|s| s.user(intent));
        self.sync_apply(ui.unwrap_or_default(), cx);
    }

    pub(crate) fn sync_apply(&mut self, ui: Vec<Ui>, cx: &mut Context<Self>) {
        if ui.is_empty() {
            return;
        }
        for item in ui {
            match item {
                Ui::Notice(notice) => self.sync_notice(notice, cx),
                Ui::NowPlaying { item_id } => self.sync_now_playing(item_id, cx),
                Ui::ClosePlayer => {
                    // The poll of the player sees the end and closes the view.
                    self.start_player_poll(cx);
                }
                Ui::Server { kind, .. } => self.server_news(&kind, cx),
            }
        }
        cx.notify();
    }

    fn sync_notice(&mut self, notice: Notice, cx: &mut Context<Self>) {
        match notice {
            Notice::Joined(name) => {
                // The group plays at one speed.
                self.player.set_speed(1.);
                self.toast("SyncPlay", format!("You are in the group \"{name}\"."), cx);
                self.load_groups(cx);
            }
            Notice::Left => {
                self.player.set_speed(self.speed as f64);
                self.toast("SyncPlay", "You left the group.", cx);
                self.load_groups(cx);
            }
            Notice::UserJoined(name) => self.toast("SyncPlay", format!("{name} joined the group."), cx),
            Notice::UserLeft(name) => self.toast("SyncPlay", format!("{name} left the group."), cx),
            Notice::State(..) => {}
            Notice::Queue => self.load_queue_titles(cx),
            Notice::Denied(what) => {
                let text = match what.as_str() {
                    "GroupDoesNotExist" => "That group does not exist any more.",
                    "LibraryAccessDenied" => "You do not have access to what the group plays.",
                    "CreateGroupDenied" => "You are not allowed to make a group.",
                    "JoinGroupDenied" => "You are not allowed to join a group.",
                    "SyncPlayIsDisabled" => "SyncPlay is switched off on the server.",
                    _ => "The server refused the request.",
                };
                self.toast("SyncPlay", text, cx);
                self.load_groups(cx);
            }
            Notice::SteppedAside(why) => self.toast(
                "SyncPlay",
                format!("{why} The group goes on without this player."),
                cx,
            ),
        }
    }

    /// The group loads an item into the player: show the player, and get
    /// what the screen needs of the item.
    fn sync_now_playing(&mut self, item_id: String, cx: &mut Context<Self>) {
        self.player.set_volume(self.volume as f64);
        self.player.set_muted(self.muted);
        self.player.set_speed(1.);
        self.apply_subtitle_style();
        // The audio track stays as the player chose it: a change after the
        // start would move the position.
        self.queue.explicit_audio = true;
        self.segments.clear();
        self.episode_picker.open = false;
        // The item of the player is not known until the server answers; a
        // local play meanwhile sets its own (`begin`).
        self.playing = None;
        self.player_status = self.player.status();
        self.player_open = true;
        self.sync.focus_player = true;
        self.show_controls();
        self.start_player_poll(cx);
        // The answer is for this load: the group may load another item, or
        // the player may close, before it comes.
        let load = self.next_revision();
        self.sync.now_playing_load = load;
        let wanted = item_id.clone();
        self.fetch(
            cx,
            move |client| {
                let item = client.item(&item_id)?;
                let segments = client.media_segments(&item_id).unwrap_or_default();
                Ok((item, segments))
            },
            move |this, result, cx| {
                if let Ok((item, segments)) = result {
                    this.sync.titles.insert(wanted.clone(), item.display_title());
                    if this.sync.now_playing_load != load || !this.player_open || this.playing.is_some() {
                        return;
                    }
                    this.playing = Some(item);
                    this.segments = segments;
                    this.load_timeline(wanted, cx);
                    this.tracks_version = 0;
                    this.rebuild_track_menus(cx);
                    cx.notify();
                }
            },
        );
    }

    /// News of the server that is not SyncPlay's: the pages that show what
    /// changed load again, once the news has been quiet for a moment.
    fn server_news(&mut self, kind: &str, cx: &mut Context<Self>) {
        let Some(library) = crate::news::kind_of(kind) else {
            return;
        };
        log::debug!("server news: {kind}");
        self.sync.news_library |= library;
        self.sync.news_pending = true;
        let first = *self.sync.news_first.get_or_insert_with(std::time::Instant::now);
        let wait = crate::news::delay(first.elapsed());
        // A new message replaces the task; `delay` caps the total wait.
        self.sync.news = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            this.update(cx, |this, cx| this.settle_news(cx)).ok();
        }));
    }

    /// Runs the reload that the news asks for. While the player is open the
    /// reload stays owed until the player closes.
    fn settle_news(&mut self, cx: &mut Context<Self>) {
        self.sync.news_first = None;
        if self.player_open {
            self.sync.news_owed = true;
            return;
        }
        self.sync.news_owed = false;
        let library = std::mem::take(&mut self.sync.news_library);
        self.sync.news_pending = false;
        let shows = match self.page {
            Page::Home(_) => crate::news::Shows::Home,
            Page::Library(_) => crate::news::Shows::Library,
            _ => crate::news::Shows::Other,
        };
        let outcome = crate::news::outcome(library, shows, false);
        log::info!("server news settled: {outcome:?}");
        self.sync.news_stats.reloaded(&outcome);
        if outcome.catalog {
            self.reload_catalog(cx);
        }
        if outcome.page {
            self.load_page(cx);
        }
    }

    /// The player view closed: runs the reload that came due under it.
    pub fn news_player_closed(&mut self, cx: &mut Context<Self>) {
        if self.sync.news_owed {
            self.settle_news(cx);
        }
    }

    /// The counters of `events`.
    pub fn news_describe(&self) -> String {
        self.sync.news_stats.describe(self.sync.news_pending)
    }

    /// Plays items in the group when this player is in one. False when it
    /// is not; the caller then plays them alone.
    pub fn sync_play(&mut self, items: &[Item], resume: bool, cx: &mut Context<Self>) -> bool {
        if !self.sync.in_group() {
            return false;
        }
        let Some(first) = items.first().cloned() else {
            return true;
        };
        let start = if resume { first.resume_secs() as f64 * 1000. } else { 0. };
        let mut item_ids: Vec<String> = items
            .iter()
            .filter(|item| !item.is_series())
            .map(|item| item.id.clone())
            .collect();
        // One episode stands for itself and the episodes after it, as the
        // queue of this app does outside a group.
        let follow = (items.len() == 1 && first.kind == "Episode")
            .then(|| first.series_id.clone())
            .flatten();
        match follow {
            Some(series_id) => {
                let episode_id = first.id.clone();
                self.fetch(
                    cx,
                    move |client| Ok(client.episodes_after(&series_id, &episode_id).unwrap_or_default()),
                    move |this, result, cx| {
                        item_ids.extend(result.unwrap_or_default().into_iter().map(|item| item.id));
                        this.sync_user(Intent::Play { item_ids, index: 0, start }, cx);
                    },
                );
            }
            None => self.sync_user(Intent::Play { item_ids, index: 0, start }, cx),
        }
        true
    }

    /// Switches the correction of the position on or off.
    pub fn toggle_sync_correction(&mut self, cx: &mut Context<Self>) {
        let on = !self.config.sync_correction.unwrap_or(true);
        self.config.sync_correction = Some(on);
        self.save_config(cx);
        if let Some(session) = &mut self.sync.session {
            session.core.correct = on;
        }
        cx.notify();
    }

    /// Sets how much later than the group this player plays.
    pub fn set_sync_offset(&mut self, ms: f64, cx: &mut Context<Self>) {
        self.config.sync_offset_ms = ms;
        self.save_config(cx);
        // A later player sees the clock of the server behind by that much.
        if let Some(session) = &mut self.sync.session {
            session.clock.extra_offset = -ms;
        }
        cx.notify();
    }

    pub fn toggle_sync_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // One popup at a time.
        let open = !self.sync.panel_open;
        self.close_popups(None, window, cx);
        // A click on the button while the panel is open closed it a moment
        // ago, as a click outside of it; that click must not open it again.
        if open && self.sync.panel_closed.is_some_and(|at| at.elapsed() < Duration::from_millis(250)) {
            return;
        }
        self.sync.panel_open = open;
        if self.sync.panel_open {
            self.load_groups(cx);
        }
        cx.notify();
    }

    fn load_groups(&mut self, cx: &mut Context<Self>) {
        self.sync.groups_loading = true;
        self.fetch(
            cx,
            |client| client.get::<Vec<GroupInfo>>("/SyncPlay/List", &[]),
            |this, result, cx| {
                this.sync.groups_loading = false;
                match result {
                    Ok(groups) => this.sync.groups = groups,
                    Err(err) => log::warn!("syncplay: list of groups: {err:#}"),
                }
                cx.notify();
            },
        );
    }

    /// Gets the titles of the queue items the panel does not know yet.
    fn load_queue_titles(&mut self, cx: &mut Context<Self>) {
        let Some(queue) = self.sync.session.as_ref().and_then(|s| s.core.queue()) else {
            return;
        };
        let missing: Vec<String> = queue
            .playlist
            .iter()
            .map(|item| item.item_id.clone())
            .filter(|id| !self.sync.titles.contains_key(id))
            .collect();
        if missing.is_empty() {
            return;
        }
        self.fetch(
            cx,
            move |client| {
                Ok(missing
                    .into_iter()
                    .filter_map(|id| client.item(&id).ok().map(|item| (id, item.display_title())))
                    .collect::<Vec<_>>())
            },
            |this, result, cx| {
                this.sync.titles.extend(result.unwrap_or_default());
                cx.notify();
            },
        );
    }

    /// A small label in the header of the player while it plays with a
    /// group: the name of the group and what the group waits for.
    pub fn sync_player_chip(&self, cx: &mut Context<Self>) -> Option<Div> {
        let session = self.sync.session.as_ref()?;
        let group = session.core.group()?;
        let t = UiTheme::read(cx).clone();
        let state = if !session.core.following() {
            "not following"
        } else {
            match (session.core.phase_name(), group.state) {
                ("loading", _) => "loading",
                ("catching-up" | "seeking", _) => "catching up",
                (_, GroupState::Waiting) => "waiting for a member",
                (_, GroupState::Paused) => "paused",
                (_, GroupState::Playing) => "in sync",
                (_, GroupState::Idle) => "idle",
            }
        };
        Some(
            div()
                .ml(px(12.))
                .h(px(24.))
                .px(px(10.))
                .rounded_full()
                .bg(rgba(0xffffff26))
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(12.))
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(t.colors.foreground)
                .child(crate::icons::filled(crate::icons::Filled::Groups, 16., t.colors.foreground))
                .child(format!("{} · {state}", group.group_name)),
        )
    }

    /// Text of the sync for the debug channel.
    pub fn sync_describe(&self) -> String {
        match &self.sync.session {
            Some(session) => format!("access={} {}", self.sync.access, session.describe()),
            None => format!("access={} no session", self.sync.access),
        }
    }

    /// The panel of the groups. `top` places it under the top bar; without
    /// it the panel sits over the controls of the player.
    pub fn render_sync_panel(&self, top: Option<f32>, cx: &mut Context<Self>) -> Option<Div> {
        if !self.sync.panel_open {
            return None;
        }
        let t = UiTheme::read(cx).clone();
        let soft = rgba(0xf5f5f7b3);
        let session = self.sync.session.as_ref()?;

        fn swallow(_: &mut Bloom, _: &MouseDownEvent, _: &mut Window, cx: &mut Context<Bloom>) {
            cx.stop_propagation();
        }
        let row = |id: SharedString| {
            div()
                .id(id)
                .min_h(px(44.))
                .px(px(12.))
                .py(px(6.))
                .rounded(px(12.))
                .flex()
                .items_center()
                .gap(px(12.))
                .cursor_pointer()
                .hover(|s| s.bg(rgba(0xffffff1f)))
        };
        let label = |title: String, detail: String| {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .truncate()
                        .text_size(px(15.))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(t.colors.foreground)
                        .child(title),
                )
                .when(!detail.is_empty(), |el| {
                    el.child(div().truncate().text_size(px(12.)).text_color(soft).child(detail))
                })
        };
        let heading = |text: String| {
            div()
                .px(px(12.))
                .pt(px(4.))
                .text_size(px(12.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(soft)
                .child(text)
        };

        let mut body = div().flex().flex_col().gap(px(2.));
        let title = match session.core.group() {
            None => {
                if self.sync.groups.is_empty() {
                    body = body.child(
                        div().px(px(12.)).py(px(10.)).text_size(px(14.)).text_color(soft).child(
                            if self.sync.groups_loading {
                                "Loading…"
                            } else {
                                "No group is open. Make one, and others can join it."
                            },
                        ),
                    );
                }
                for group in &self.sync.groups {
                    let group_id = group.group_id.clone();
                    body = body.child(
                        row(SharedString::from(format!("sync.join.{}", group.group_id)))
                            .child(crate::icons::filled(crate::icons::Filled::Groups, 22., t.colors.foreground))
                            .child(label(group.group_name.clone(), group.participants.join(", ")))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.sync_user(Intent::Join { group_id: group_id.clone() }, cx)
                            })),
                    );
                }
                if self.sync.access == "CreateAndJoinGroups" {
                    let name = self
                        .session
                        .as_ref()
                        .map_or_else(|| "My group".to_string(), |s| format!("{}'s group", s.user_name));
                    body = body.child(
                        row("sync.new".into())
                            .child(icon(LucideIcon::Plus, 18., t.colors.foreground))
                            .child(label("New group".into(), "Others can join it from their player.".into()))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.sync_user(Intent::Create { name: name.clone() }, cx)
                            })),
                    );
                }
                "SyncPlay".to_string()
            }
            Some(group) => {
                let state = match group.state {
                    GroupState::Idle => "Nothing plays",
                    GroupState::Waiting => "Waits for a member",
                    GroupState::Paused => "Paused",
                    GroupState::Playing => "Plays",
                };
                body = body.child(
                    div()
                        .px(px(12.))
                        .pb(px(4.))
                        .text_size(px(13.))
                        .text_color(soft)
                        .child(format!("{state} · {}", group.participants.join(", "))),
                );
                // The queue of the group.
                if let Some(queue) = session.core.queue().filter(|q| !q.playlist.is_empty()) {
                    body = body.child(heading("Queue".into()));
                    let current = queue.current().map(|item| item.playlist_item_id.clone());
                    let mut list = div().flex().flex_col().gap(px(2.));
                    for (n, item) in queue.playlist.iter().enumerate() {
                        let playing = current.as_deref() == Some(item.playlist_item_id.as_str());
                        let title = self
                            .sync
                            .titles
                            .get(&item.item_id)
                            .cloned()
                            .unwrap_or_else(|| "…".to_string());
                        let (jump, remove) = (item.playlist_item_id.clone(), item.playlist_item_id.clone());
                        list = list.child(
                            row(SharedString::from(format!("sync.queue.{}", item.playlist_item_id)))
                                .when(playing, |el| el.bg(rgba(0xffffff1f)))
                                .child(
                                    div()
                                        .w(px(18.))
                                        .text_size(px(12.))
                                        .text_color(soft)
                                        .child(if playing { "▶".to_string() } else { (n + 1).to_string() }),
                                )
                                .child(label(title, String::new()))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.sync_user(Intent::Jump { playlist_item_id: jump.clone() }, cx)
                                }))
                                .children((n > 0).then(|| {
                                    let moved = item.playlist_item_id.clone();
                                    div()
                                        .id(SharedString::from(format!("sync.up.{moved}")))
                                        .size(px(28.))
                                        .rounded(px(8.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .hover(|s| s.bg(rgba(0xffffff29)))
                                        .tooltip(tip("Move up"))
                                        .child(icon(LucideIcon::ChevronUp, 15., soft))
                                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                            cx.stop_propagation();
                                            this.sync_user(
                                                Intent::Move {
                                                    playlist_item_id: moved.clone(),
                                                    new_index: n - 1,
                                                },
                                                cx,
                                            )
                                        }))
                                }))
                                .children((n + 1 < queue.playlist.len()).then(|| {
                                    let moved = item.playlist_item_id.clone();
                                    div()
                                        .id(SharedString::from(format!("sync.down.{moved}")))
                                        .size(px(28.))
                                        .rounded(px(8.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .hover(|s| s.bg(rgba(0xffffff29)))
                                        .tooltip(tip("Move down"))
                                        .child(icon(LucideIcon::ChevronDown, 15., soft))
                                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                            cx.stop_propagation();
                                            this.sync_user(
                                                Intent::Move {
                                                    playlist_item_id: moved.clone(),
                                                    new_index: n + 1,
                                                },
                                                cx,
                                            )
                                        }))
                                }))
                                .when(!playing, |el| {
                                    el.child(
                                        div()
                                            .id(SharedString::from(format!("sync.remove.{remove}")))
                                            .size(px(28.))
                                            .rounded(px(8.))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .hover(|s| s.bg(rgba(0xffffff29)))
                                            .tooltip(tip("Remove from the queue"))
                                            .child(icon(LucideIcon::X, 15., soft))
                                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                                cx.stop_propagation();
                                                this.sync_user(
                                                    Intent::Remove { playlist_item_ids: vec![remove.clone()] },
                                                    cx,
                                                )
                                            })),
                                    )
                                }),
                        );
                    }
                    let height = (queue.playlist.len() as f32 * 46.).min(230.);
                    body = body.child(
                        div().h(px(height)).child(
                            ScrollArea::new("sync.queue.scroll").size_full().child(list),
                        ),
                    );
                    let (shuffle, repeat) = (queue.shuffle_mode.clone(), queue.repeat_mode.clone());
                    let next_shuffle = if shuffle == "Shuffle" { "Sorted" } else { "Shuffle" };
                    let next_repeat = match repeat.as_str() {
                        "RepeatNone" => "RepeatAll",
                        "RepeatAll" => "RepeatOne",
                        _ => "RepeatNone",
                    };
                    body = body
                        .child(
                            row("sync.shuffle".into())
                                .child(icon(LucideIcon::Shuffle, 18., t.colors.foreground))
                                .child(label(
                                    "Shuffle".into(),
                                    if shuffle == "Shuffle" { "On".into() } else { "Off".into() },
                                ))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.sync_user(Intent::Shuffle(next_shuffle.to_string()), cx)
                                })),
                        )
                        .child(
                            row("sync.repeat".into())
                                .child(icon(LucideIcon::Repeat, 18., t.colors.foreground))
                                .child(label(
                                    "Repeat".into(),
                                    match repeat.as_str() {
                                        "RepeatAll" => "The queue".into(),
                                        "RepeatOne" => "This item".into(),
                                        _ => "Off".into(),
                                    },
                                ))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.sync_user(Intent::Repeat(next_repeat.to_string()), cx)
                                })),
                        );
                }
                body = body.child(heading("This player".into()));
                let following = session.core.following();
                body = body.child(
                    row("sync.follow".into())
                        .child(icon(
                            if following { LucideIcon::CircleStop } else { LucideIcon::CirclePlay },
                            18.,
                            t.colors.foreground,
                        ))
                        .child(label(
                            if following { "Stop local playback" } else { "Resume local playback" }.into(),
                            if following {
                                "Stay in the group; it does not wait for this player.".into()
                            } else {
                                "Play with the group again.".into()
                            },
                        ))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            let intent = if following { Intent::StopFollowing } else { Intent::Follow };
                            this.sync_user(intent, cx)
                        })),
                );
                body = body.child(
                    row("sync.leave".into())
                        .child(icon(LucideIcon::LogOut, 18., t.colors.foreground))
                        .child(label("Leave the group".into(), String::new()))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.sync_user(Intent::Leave, cx)
                        })),
                );
                // How well this player is in step.
                body = body.child(
                    div()
                        .px(px(12.))
                        .pt(px(6.))
                        .text_size(px(12.))
                        .text_color(soft)
                        .child(format!(
                            "Server clock {:+.0} ms · ping {:.0} ms · drift {}",
                            session.clock.offset(),
                            session.clock.ping(),
                            session.core.drift_ms.map_or("–".to_string(), |d| format!("{d:+.0} ms")),
                        )),
                );
                group.group_name.clone()
            }
        };

        let panel = div()
            .absolute()
            .right(px(24.))
            .w(px(PANEL_W.min(self.viewport_w - 48.)))
            .rounded(px(24.))
            .border_1()
            .border_color(rgba(0xf5f5f733))
            .on_mouse_down(MouseButton::Left, cx.listener(swallow))
            // A click anywhere else closes the panel, as it closes a menu.
            .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _, cx| {
                if std::mem::take(&mut this.sync.panel_open) {
                    this.sync.panel_closed = Some(std::time::Instant::now());
                    cx.notify();
                }
            }))
            .child(glass(px(24.), crate::ui::glass::POPUP_TINT))
            .p(px(12.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                div()
                    .px(px(12.))
                    .pt(px(4.))
                    .truncate()
                    .text_size(px(17.))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(t.colors.foreground)
                    .child(title),
            )
            .child(body);
        Some(match top {
            Some(top) => panel.top(px(top)),
            None => panel.bottom(px(134.)),
        })
    }
}

/// The item of the group that answers late (review of 2026-10-05, devices
/// finding 2). See `app::race_harness`.
#[cfg(test)]
mod race_tests {
    use gpui_kit::TestAppContext;
    use serde_json::json;

    use crate::app::{Screen, race_harness::{MockServer, app, plain, session, tick_until}};

    fn server() -> MockServer {
        MockServer::start(|method, path, _| match (method, path) {
            ("GET", p) if p.starts_with("/Items/") => {
                let id = p.trim_start_matches("/Items/").split('?').next().unwrap_or_default();
                (200, json!({"Id": id, "Name": format!("Title {id}"), "Type": "Movie"}).to_string())
            }
            _ => plain(method, path),
        })
    }

    #[gpui_kit::test]
    fn the_item_of_a_closed_player_does_not_come_back(cx: &mut TestAppContext) {
        let server = server();
        let (bloom, cx) = app(cx);
        bloom.update(cx, |this, cx| {
            this.session = Some(session(&server.url, "u1"));
            this.screen = Screen::Main;
            this.sync_now_playing("x".into(), cx);
        });
        // The item is asked for; its answer waits while the user closes the player.
        tick_until(cx, || server.count("GET", "/Items/x") == 1);
        bloom.update(cx, |this, cx| this.close_player_view(cx));
        cx.run_until_parked();
        bloom.read_with(cx, |this, _| {
            assert!(!this.player_open);
            assert!(this.playing.is_none(), "the closed player got the item of the group");
            assert_eq!(this.sync.titles.get("x").map(String::as_str), Some("Title x"), "the title is not cached");
        });
    }

    /// The group loads another item before the first one answers.
    #[gpui_kit::test(iterations = 20)]
    fn the_player_shows_the_item_loaded_last(cx: &mut TestAppContext) {
        let server = server();
        let (bloom, cx) = app(cx);
        bloom.update(cx, |this, cx| {
            this.session = Some(session(&server.url, "u1"));
            this.screen = Screen::Main;
            this.sync_now_playing("x".into(), cx);
            this.sync_now_playing("y".into(), cx);
        });
        cx.run_until_parked();
        bloom.read_with(cx, |this, _| {
            assert!(this.player_open);
            assert_eq!(
                this.playing.as_ref().map(|i| i.id.as_str()),
                Some("y"),
                "the player shows the item the group loaded first"
            );
        });
    }
}
