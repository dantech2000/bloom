//! The window's plumbing (toasts, the config save, the guarded fetches)
//! and the sessions: sign-in, a switch, sign-out.

use super::*;

impl Bloom {
    // ----- infrastructure ---------------------------------------------------

    pub fn toast(&self, title: impl Into<String>, description: impl Into<String>, cx: &mut App) {
        let title = title.into();
        let description = description.into();
        let id = format!("toast-{}", uuid::Uuid::new_v4());
        self.toasts.update(cx, |toasts, cx| {
            toasts.push(id, title, description, Some(Duration::from_secs(6)), cx);
        });
    }

    /// A notice that replaces the one before it of the same `key`, where
    /// several in a row say one thing (three downloads queued one after the
    /// other): one card with the latest word, not a stack.
    pub fn toast_as(&self, key: &str, title: impl Into<String>, description: impl Into<String>, cx: &mut App) {
        let (title, description) = (title.into(), description.into());
        let id = format!("toast-{key}");
        self.toasts.update(cx, |toasts, cx| {
            toasts.push(id, title, description, Some(Duration::from_secs(6)), cx);
        });
    }

    pub fn save_config(&self, cx: &mut App) {
        self.config_saves.set(self.config_saves.get() + 1);
        if sandboxed() {
            return;
        }
        if let Err(err) = self.config.save() {
            log::error!("saving config failed: {err:#}");
            self.toast("Could not save settings", format!("{err:#}"), cx);
        }
    }

    /// Saves the config once the changes stop, for a value that changes
    /// many times in a row (a slider that is dragged): each save is a write
    /// with a full sync of the disk, some milliseconds on the UI thread.
    /// The window place waits the same way; one write takes both. The quit
    /// of the app writes what still waits.
    pub fn save_config_soon(&mut self, cx: &mut Context<Self>) {
        self.window_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(400)).await;
            this.update(cx, |this, cx| {
                this.window_save = None;
                this.save_config(cx);
            })
            .ok();
        }));
    }

    /// Writes a save of the config that still waits, at once: every way
    /// out of the window calls this (the red button, Close Window, the
    /// quit), as the task that waits ends with the window. True when a
    /// save waited.
    pub(crate) fn flush_config_save(&mut self, cx: &mut Context<Self>) -> bool {
        let waited = self.window_save.take().is_some();
        if waited {
            self.save_config(cx);
        }
        waited
    }

    /// Runs blocking work with a client on a background thread and hands the
    /// result back to this view.
    pub fn fetch_with<T, W, D>(&self, client: Client, cx: &mut Context<Self>, work: W, done: D)
    where
        T: Send + 'static,
        W: FnOnce(Client) -> Result<T> + Send + 'static,
        D: FnOnce(&mut Self, Result<T>, &mut Context<Self>) + 'static,
    {
        // The work goes to a background thread now, not when the UI thread
        // next gets to its tasks: at startup that is after the first frame.
        let work = cx.background_executor().spawn(async move { work(client) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            this.update(cx, |this, cx| done(this, result, cx)).ok();
        })
        .detach();
    }

    pub fn fetch<T, W, D>(&self, cx: &mut Context<Self>, work: W, done: D)
    where
        T: Send + 'static,
        W: FnOnce(Client) -> Result<T> + Send + 'static,
        D: FnOnce(&mut Self, Result<T>, &mut Context<Self>) + 'static,
    {
        let Some(client) = self.session.as_ref().map(|s| s.client.clone()) else {
            return;
        };
        self.fetch_with(client, cx, work, done);
    }

    /// As [`Bloom::fetch`], for an answer that only the session that asked
    /// may take: it is dropped when another profile or server is open by
    /// the time it comes (a picture, a password, a page to open).
    pub fn fetch_in_session<T, W, D>(&self, cx: &mut Context<Self>, work: W, done: D)
    where
        T: Send + 'static,
        W: FnOnce(Client) -> Result<T> + Send + 'static,
        D: FnOnce(&mut Self, Result<T>, &mut Context<Self>) + 'static,
    {
        let epoch = self.session_epoch;
        self.fetch(cx, work, move |this, result, cx| {
            if this.session_epoch == epoch {
                done(this, result, cx);
            }
        });
    }

    /// As [`Bloom::fetch`], for an answer that belongs to the page on show:
    /// it is dropped when the user has gone to another page, or the page
    /// has asked again, by the time it comes.
    pub fn fetch_for_page<T, W, D>(&self, cx: &mut Context<Self>, work: W, done: D)
    where
        T: Send + 'static,
        W: FnOnce(Client) -> Result<T> + Send + 'static,
        D: FnOnce(&mut Self, Result<T>, &mut Context<Self>) + 'static,
    {
        let generation = self.nav.generation;
        self.fetch(cx, work, move |this, result, cx| {
            if this.nav.generation == generation {
                done(this, result, cx);
            }
        });
    }

    pub(super) fn next_generation(&mut self) -> u64 {
        self.nav.generation += 1;
        self.nav.generation
    }

    /// A new revision, larger than every one before it.
    pub(crate) fn next_revision(&mut self) -> Revision {
        self.revisions.0 += 1;
        self.revisions
    }

    /// The session changes: every answer still on its way is for the old one.
    pub(super) fn next_session_epoch(&mut self) {
        self.session_epoch.0 += 1;
        // The page of the session before is gone too: an answer for it
        // must not land in the page of the same kind that opens next.
        self.next_generation();
        // The posters of the session before are of no use to the next one.
        crate::images::cancel_prewarm();
        // Nor are its hero slides, and a question it left open must not
        // run against the next one.
        self.clear_hero();
        self.forget_admin_dialogs();
        // A live load of the dashboard of the session before has no answer
        // to wait for here.
        self.admin_live.busy = false;
        self.admin_live.load += 1;
        // The devices of the server before, and an editor open on one of
        // its items, are not for the next one.
        self.cast.sessions.clear();
        self.metadata.open = None;
    }

    /// Ends the session in the app (the server and the config are the
    /// caller's business), so that what the old session asked for is
    /// dropped when it answers.
    pub fn drop_session(&mut self, cx: &mut Context<Self>) {
        // What plays on another device for this session stops, while the
        // session can still say so.
        self.cast_off(cx);
        self.session = None;
        self.next_session_epoch();
    }

    // ----- sessions ---------------------------------------------------------

    pub fn open_session(&mut self, server_id: &str, user_id: &str, cx: &mut Context<Self>) -> bool {
        let Some(server) = self.config.server(server_id) else {
            return false;
        };
        let Some(profile) = server.profiles.iter().find(|p| p.user_id == user_id) else {
            return false;
        };
        let url = crate::options::options().server_url.as_deref().unwrap_or(&server.url);
        let client = Client::new(url, &self.config.device_id)
            .with_server(&server.id)
            .with_session(&profile.token, &profile.user_id)
            .opened();
        let (id, server_name, profile) = (server.id.clone(), server.name.clone(), profile.clone());
        // What plays on another device for the session before stops, while
        // that session can still say so.
        self.cast_off(cx);
        self.next_session_epoch();
        let user_image = profile
            .image_tag
            .as_ref()
            .map(|tag| client.user_image_url(&profile.user_id, tag));
        self.session = Some(Session {
            server_id: id,
            server_name,
            user_id: profile.user_id.clone(),
            user_name: profile.name.clone(),
            user_image,
            client,
            is_admin: false,
            audio_language: None,
        });
        if !sandboxed() {
            self.start_downloads(cx);
        }
        self.prefs = Default::default();
        self.load_prefs(cx);
        // The title index of the search loads a moment after the home page,
        // so it does not slow the first content.
        self.search.catalog = Default::default();
        self.search.loaded = None;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            this.update(cx, |this, cx| this.refresh_catalog(cx)).ok();
        })
        .detach();
        // The socket of the server, for SyncPlay and for live news.
        if !sandboxed() {
            self.start_sync(cx);
        }
        // The speed of the connection, for the "Auto" quality.
        self.adaptive_signed_in(cx);
        self.lists.playlists.clear();
        self.load_lists(cx);
        // The dashboard entry of the menu shows once the server confirms
        // that this user is an administrator.
        self.fetch_in_session(
            cx,
            |client| client.user_settings(),
            |this, result, cx| {
                if let (Ok((is_admin, audio_language, sync_access)), Some(session)) =
                    (result, this.session.as_mut())
                {
                    session.audio_language = audio_language;
                    this.sync.access = sync_access;
                    cx.notify();
                    if is_admin {
                        session.is_admin = true;
                        this.rebuild_menu(cx);
                        cx.notify();
                    }
                }
            },
        );
        // Quality tags follow the user's Jellyfin Enhanced setting, unless
        // the profile menu of this app set them.
        crate::jellyfin::set_quality_tags(self.config.quality_tags.unwrap_or(false), true, true);
        self.fetch_in_session(
            cx,
            |client| Ok(client.quality_prefs()),
            |this, result, cx| {
                if let Ok(Some((enabled, prefs))) = result {
                    use crate::quality_tags::Category;
                    crate::quality_tags::set_prefs(prefs);
                    this.je_quality_tags =
                        (enabled, prefs.shows(Category::Resolution), prefs.shows(Category::DynamicRange));
                    this.apply_quality_tags(cx);
                }
            },
        );
        self.load_enhanced(cx);
        // The file is written when the profile changed, not at each start:
        // a save is a write with a full sync of the disk on the UI thread.
        let active = Some((server_id.to_string(), user_id.to_string()));
        if self.config.active != active {
            self.config.active = active;
            self.save_config(cx);
        }
        self.screen = Screen::Main;
        self.clear_history();
        self.views.clear();
        self.nav.page = Page::Home(HomeData::default());
        // The home page loads the libraries with its own data.
        self.load_page(cx);
        self.events_page(cx);
        self.rebuild_menu(cx);
        cx.notify();
        true
    }

    /// Ends the session on the server, removes its saved profile and shows
    /// the sign-in page of that server.
    pub fn sign_out(&mut self, cx: &mut Context<Self>) {
        self.stop_sync();
        self.config.active = None;
        match self.session.as_ref().map(|s| (s.server_id.clone(), s.user_id.clone())) {
            // `forget_profile` drops the session, and with it what it asked
            // for, and saves the config.
            Some((server_id, user_id)) => {
                self.forget_profile(&server_id, &user_id, cx);
                self.select_server(server_id, cx);
            }
            None => self.save_config(cx),
        }
        // The pages of the user who left must not show again.
        self.clear_history();
        self.views.clear();
        self.nav.page = Page::Home(HomeData::default());
        self.events_page(cx);
        self.screen = Screen::Connect;
        self.rebuild_menu(cx);
        cx.notify();
    }

    pub fn show_connect(&mut self, cx: &mut Context<Self>) {
        if let Some(server_id) = self.session.as_ref().map(|s| s.server_id.clone()) {
            self.select_server(server_id, cx);
        }
        self.connect.error = None;
        self.screen = Screen::Connect;
        cx.notify();
    }

    pub fn add_profile(&mut self, server_id: &str, profile: Profile, cx: &mut Context<Self>) {
        let active = self.config.active.clone();
        self.config.upsert_profile(server_id, profile.clone());
        // `open_session` saves when the active profile changes; a sign-in
        // to the one that is active already is saved here.
        let opened = self.open_session(server_id, &profile.user_id, cx);
        if !opened || self.config.active == active {
            self.save_config(cx);
        }
    }

    pub fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        let dark = !self.config.dark.unwrap_or(true);
        self.config.dark = Some(dark);
        self.save_config(cx);
        UiTheme::set(cx, theme(dark));
        // The parts AppKit draws follow the theme (see `main.rs`).
        crate::macos::set_app_appearance(Some(dark));
        self.rebuild_menu(cx);
        cx.notify();
    }

    /// The View menu says "Exit Full Screen" while the window is full
    /// screen. The menu bar is built again only when the state changes.
    pub(super) fn follow_fullscreen_menu(&mut self, window: &Window, cx: &mut App) {
        let fullscreen = window.is_fullscreen();
        if fullscreen != self.menus_fullscreen {
            self.menus_fullscreen = fullscreen;
            crate::menus::set_menus(cx, fullscreen);
        }
    }

    /// Whether the window is full screen, as the bounds observer last saw
    /// it; for the events, which have no window at hand.
    pub(crate) fn events_fullscreen(&self) -> bool {
        self.menus_fullscreen
    }

    /// Card and padding sizes for the current window width.
    pub fn metrics(&self) -> Metrics {
        Metrics::new(self.viewport_w)
    }

    /// Scroll state of every row, for the debug channel.
    pub fn debug_rows(&self) -> String {
        self.nav.rows
            .borrow()
            .iter()
            .map(|(id, handle)| {
                format!(
                    "{id}: offset={} max={} width={}",
                    f32::from(handle.offset().x),
                    f32::from(handle.max_offset().x),
                    f32::from(handle.bounds().size.width),
                )
            })
            .collect::<Vec<_>>()
            .join(" | ")
    }

    /// The scroll handle of a horizontal row, created on first use.
    pub fn row_scroll(&self, id: &SharedString) -> ScrollHandle {
        self.nav.rows
            .borrow_mut()
            .entry(id.clone())
            .or_default()
            .clone()
    }

    /// Sets the watched or the favourite state of an item on the server and
    /// hands the state the server answers to `done`. With `for_page` the
    /// answer is for the page on show, and is dropped when the user has
    /// left it by then.
    pub(super) fn set_user_data(
        &mut self,
        item_id: &str,
        mark: Mark,
        on: bool,
        for_page: bool,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, crate::jellyfin::UserData, &mut Context<Self>) + 'static,
    ) {
        let (item_id, generation) = (item_id.to_string(), self.nav.generation);
        self.fetch(
            cx,
            move |client| match mark {
                Mark::Played => client.set_played(&item_id, on),
                Mark::Favorite => client.set_favorite(&item_id, on),
            },
            move |this, result, cx| {
                if for_page && this.nav.generation != generation {
                    return;
                }
                match result {
                    Ok(user_data) => done(this, user_data, cx),
                    Err(err) => {
                        let title = match mark {
                            Mark::Played => "Could not update watched state",
                            Mark::Favorite => "Could not update favorites",
                        };
                        this.toast(title, format!("{err:#}"), cx)
                    }
                }
                cx.notify();
            },
        );
    }

    /// Marks an item watched or unwatched, then reloads the page data.
    pub fn set_item_played(&mut self, item: &Item, played: bool, cx: &mut Context<Self>) {
        self.set_user_data(&item.id, Mark::Played, played, false, cx, |this, _, cx| this.load_page(cx));
    }

    /// Adds an item to the favourites or removes it, then reloads the page data.
    pub fn set_item_favorite(&mut self, item: &Item, favorite: bool, cx: &mut Context<Self>) {
        self.set_user_data(&item.id, Mark::Favorite, favorite, false, cx, |this, _, cx| this.load_page(cx));
    }
}
