//! Pages and the way between them: Back and Forward, the loads of a
//! page, the popups and panels, the title search.

use super::*;

impl Bloom {
    // ----- navigation -------------------------------------------------------

    /// True when poster cards show quality tags: the choice in this app, or
    /// else the user's Jellyfin Enhanced setting.
    pub fn quality_tags(&self) -> bool {
        self.config.quality_tags.unwrap_or(self.je_quality_tags.0)
    }

    /// Makes the cards follow the quality tag settings. A change loads the
    /// page again, because only a query with tags on brings the streams.
    pub fn apply_quality_tags(&mut self, cx: &mut Context<Self>) {
        let on = self.quality_tags();
        let changed = on != crate::jellyfin::quality_tags();
        crate::jellyfin::set_quality_tags(on, self.je_quality_tags.1, self.je_quality_tags.2);
        self.rebuild_menu(cx);
        if changed {
            self.load_page(cx);
        }
        cx.notify();
    }

    pub fn toggle_quality_tags(&mut self, cx: &mut Context<Self>) {
        self.config.quality_tags = Some(!self.quality_tags());
        self.save_config(cx);
        self.apply_quality_tags(cx);
    }

    pub fn navigate(&mut self, page: Page, cx: &mut Context<Self>) {
        self.forget_admin_dialogs();
        self.leave_page();
        // The rows of a detail page have one name on every detail page:
        // the next one starts at the left, not where the last one was.
        self.nav.rows.borrow_mut().retain(|id, _| !id.starts_with("detail."));
        // Keep the place in the page, so Back returns to it.
        let place = self.nav.scroll.offset();
        self.nav.scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
        let previous = std::mem::replace(&mut self.nav.page, page);
        self.nav.history.push((previous, place));
        self.nav.back = false;
        if self.nav.history.len() > 40 {
            self.nav.history.remove(0);
        }
        self.nav.forward.clear();
        self.load_page(cx);
        self.events_page(cx);
        cx.notify();
    }

    /// What goes with the page that leaves: the card under the pointer is
    /// a card of that page, and the pointer may never leave it (the page
    /// changed under it), so its hover layer would show on the way back.
    pub(super) fn leave_page(&mut self) {
        self.nav.hovered_card = None;
    }

    pub fn back(&mut self, cx: &mut Context<Self>) {
        if let Some((page, offset)) = self.nav.history.pop() {
            self.nav.back = true;
            let left = self.swap_page(page, offset, cx);
            self.nav.forward.push(left);
        }
    }

    /// Returns to the page Back left, as cmd-] and the forward button do.
    pub fn forward(&mut self, cx: &mut Context<Self>) {
        if let Some((page, offset)) = self.nav.forward.pop() {
            self.nav.back = false;
            let left = self.swap_page(page, offset, cx);
            self.nav.history.push(left);
        }
    }

    /// Shows `page` at `offset` and gives back the page it replaced with
    /// its place.
    pub(super) fn swap_page(
        &mut self,
        page: Page,
        offset: gpui_kit::Point<gpui_kit::Pixels>,
        cx: &mut Context<Self>,
    ) -> (Page, gpui_kit::Point<gpui_kit::Pixels>) {
        self.forget_admin_dialogs();
        self.leave_page();
        let left_offset = self.nav.scroll.offset();
        self.nav.scroll.set_offset(offset);
        let left = std::mem::replace(&mut self.nav.page, page);
        if matches!(self.nav.page, Page::Library(_)) {
            self.rebuild_library_menus(cx);
        }
        if matches!(self.nav.page, Page::Admin(_)) {
            self.start_admin_tick(cx);
        }
        // The menus of the detail page are those of the item shown last;
        // a page that comes back with its data is not loaded again.
        if matches!(self.nav.page, Page::Detail(_)) {
            self.rebuild_detail_menus(cx);
        }
        if self.page_loading() {
            // The user left before the answer came, and the answer was
            // thrown away (`generation`); the page asks again.
            self.load_page(cx);
        } else {
            self.next_generation();
        }
        self.events_page(cx);
        cx.notify();
        (left, left_offset)
    }

    /// True while the current page waits for its data.
    pub(super) fn page_loading(&self) -> bool {
        match &self.nav.page {
            Page::Home(d) => d.loading,
            Page::Library(d) => d.loading,
            Page::Detail(d) => d.loading,
            Page::Search(d) => d.loading,
            Page::Admin(d) => d.loading,
            Page::Playlist(d) => d.loading,
            Page::Settings(_) | Page::Downloads => false,
        }
    }

    pub fn open_home(&mut self, cx: &mut Context<Self>) {
        self.open_root(Page::Home(HomeData::default()), cx);
    }

    /// Shows a page of the top bar: the page starts at its top, and Back
    /// and Forward have nothing to return to.
    pub(super) fn open_root(&mut self, page: Page, cx: &mut Context<Self>) {
        self.nav.scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
        self.forget_admin_dialogs();
        self.clear_history();
        self.nav.page = page;
        self.events_page(cx);
        self.rebuild_library_menus(cx);
        self.load_page(cx);
        cx.notify();
    }

    /// Forgets the pages Back and Forward would show.
    pub(super) fn clear_history(&mut self) {
        self.nav.history.clear();
        self.nav.forward.clear();
    }

    pub fn open_library(&mut self, view: Item, cx: &mut Context<Self>) {
        self.open_root(Page::Library(LibraryData::new(view)), cx);
    }

    pub fn open_favorites(&mut self, cx: &mut Context<Self>) {
        self.open_library(
            Item {
                id: FAVORITES_ID.into(),
                name: "Favorites".into(),
                collection_type: Some(FAVORITES_ID.into()),
                ..Default::default()
            },
            cx,
        );
    }

    /// Opens a random movie or series.
    pub fn open_random(&mut self, cx: &mut Context<Self>) {
        let query = ItemQuery {
            include_types: vec!["Movie".to_string(), "Series".to_string()],
            recursive: Some(true),
            sort_by: Some("Random".to_string()),
            limit: Some(1),
            ..Default::default()
        };
        self.fetch_in_session(
            cx,
            move |client| client.items(&query),
            |this, result, cx| match result {
                Ok(page) => {
                    if let Some(item) = page.items.into_iter().next() {
                        this.open_item(item, cx);
                    }
                }
                Err(err) => this.toast("Could not pick a random item", format!("{err:#}"), cx),
            },
        );
    }

    /// Opens the library item of an id (used by Seerr cards in the library).
    pub fn open_item_id(&mut self, id: &str, cx: &mut Context<Self>) {
        let id = id.to_string();
        self.fetch_in_session(
            cx,
            move |client| client.item(&id),
            |this, result, cx| match result {
                Ok(item) => this.open_item(item, cx),
                Err(err) => this.toast("Could not open the item", format!("{err:#}"), cx),
            },
        );
    }

    /// Lists the movies and series a person takes part in.
    pub fn open_person(&mut self, id: &str, name: &str, cx: &mut Context<Self>) {
        self.open_folder(
            Item {
                id: id.to_string(),
                name: name.to_string(),
                collection_type: Some(PERSON_VIEW.into()),
                ..Default::default()
            },
            cx,
        );
    }

    pub fn open_folder(&mut self, folder: Item, cx: &mut Context<Self>) {
        self.navigate(Page::Library(LibraryData::new(folder)), cx);
        self.rebuild_library_menus(cx);
    }

    pub fn open_item(&mut self, item: Item, cx: &mut Context<Self>) {
        match item.kind {
            ItemKind::Playlist => self.open_playlist(item, cx),
            ItemKind::CollectionFolder | ItemKind::UserView | ItemKind::Folder | ItemKind::BoxSet => {
                self.open_folder(item, cx)
            }
            _ => self.navigate(
                Page::Detail(Box::new(DetailData {
                    item,
                    loading: true,
                    ..Default::default()
                })),
                cx,
            ),
        }
    }

    pub fn open_search(&mut self, query: String, cx: &mut Context<Self>) {
        let query = query.trim().to_string();
        if let Page::Search(data) = &mut self.nav.page {
            data.typed = query.clone();
            data.query = query;
        } else {
            self.navigate(
                Page::Search(SearchData {
                    typed: query.clone(),
                    query,
                    ..Default::default()
                }),
                cx,
            );
            return;
        }
        self.load_page(cx);
        cx.notify();
    }

    /// Shows the titles that fit the text at once, from the title index.
    /// The server search for the same text replaces them when it answers.
    pub(crate) fn search_titles(&mut self, typed: String, cx: &mut Context<Self>) {
        let typed = typed.trim().to_string();
        if self.session.is_none() {
            return;
        }
        self.refresh_catalog(cx);
        let results = self.search.catalog.search(&typed);
        match &mut self.nav.page {
            Page::Search(data) => {
                if data.typed == typed {
                    return;
                }
                // Back at the text of the server search before the typing
                // rested: the debounce skips a text equal to the last one.
                // The answer shows again when it came, is waited for when
                // it is on its way, and is asked for when neither (it
                // failed, or it was thrown away while the text differed).
                let back_at_query = typed == data.query;
                let on_its_way = data.loading && data.pending.as_deref() == Some(typed.as_str());
                let answered = match &data.answered {
                    Some((text, items, seerr)) if back_at_query && *text == typed => Some((items.clone(), seerr.clone())),
                    _ => None,
                };
                data.typed = typed.clone();
                self.nav.scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
                if let Some((items, seerr)) = answered {
                    data.results = items;
                    data.seerr = seerr;
                    data.loading = false;
                    cx.notify();
                    return;
                }
                data.loading = !typed.is_empty();
                // Without an index the old results stay until the server answers.
                if !self.search.catalog.is_empty() || typed.is_empty() {
                    data.results = results;
                    data.seerr.clear();
                }
                if back_at_query && !on_its_way {
                    self.load_page(cx);
                }
            }
            _ if typed.is_empty() => return,
            _ => self.navigate(
                Page::Search(SearchData {
                    typed,
                    loading: true,
                    results,
                    ..Default::default()
                }),
                cx,
            ),
        }
        cx.notify();
    }

    /// Every menu of the app. A new menu goes into this list, and nowhere
    /// else: the observers of `Bloom::new` and `close_popups` read it.
    pub(super) fn menus(&self) -> [&Entity<MenuState>; 12] {
        [
            &self.popups.admin_menu,
            &self.popups.profile_menu,
            &self.playback.audio_menu,
            &self.playback.settings_menu,
            &self.popups.sort_menu,
            &self.popups.show_menu,
            &self.popups.filter_menu,
            &self.popups.detail_audio_menu,
            &self.popups.detail_subtitle_menu,
            &self.popups.more_menu,
            &self.popups.card_menu,
            &self.playback.subtitle_menu,
        ]
    }

    /// Closes every popup but `keep`: the menus and the open panel. The app
    /// shows one popup at a time; whatever opens one calls this first.
    pub fn close_popups(
        &mut self,
        keep: Option<&Entity<MenuState>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open: Vec<Entity<MenuState>> = self
            .menus()
            .into_iter()
            .filter(|menu| Some(*menu) != keep && menu.read(cx).is_open())
            .cloned()
            .collect();
        for menu in open {
            menu.update(cx, |menu, cx| menu.close(false, window, cx));
        }
        if self.close_panels() {
            cx.notify();
        }
    }

    /// True while a panel that is not a menu is open.
    pub fn panel_open(&self) -> bool {
        self.popups.panel.is_some()
    }

    /// True while this panel is the open one.
    pub fn panel_is(&self, panel: Panel) -> bool {
        self.popups.panel == Some(panel)
    }

    /// Opens a panel in the place of whatever popup is open.
    pub fn open_panel(&mut self, panel: Panel, window: &mut Window, cx: &mut Context<Self>) {
        self.close_popups(None, window, cx);
        self.popups.panel = Some(panel);
        cx.notify();
    }

    /// Shows a panel in the place of the open one, for an opener with no
    /// window at hand. The menu whose entry opened it has closed itself.
    pub(crate) fn replace_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        self.popups.panel = Some(panel);
        cx.notify();
    }

    /// The button of a panel: closes the panel when it is open and opens it
    /// in the place of every other popup when it is not. True when the
    /// panel is open afterwards.
    pub fn toggle_panel(&mut self, panel: Panel, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let was_open = self.panel_is(panel);
        self.close_popups(None, window, cx);
        // A click on the button while the panel is open closed it a moment
        // ago, as a click outside of it; that click must not open it again.
        let dismissed = self
            .popups.panel_dismissed
            .take()
            .is_some_and(|(which, at)| which == panel && at.elapsed() < Duration::from_millis(250));
        if was_open || dismissed {
            cx.notify();
            return false;
        }
        self.open_panel(panel, window, cx);
        true
    }

    /// A click outside of a panel: closes it when it is the open one.
    pub fn dismiss_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        if self.close_panel(panel) {
            self.popups.panel_dismissed = Some((panel, std::time::Instant::now()));
            cx.notify();
        }
    }

    /// Closes the open panel, whichever it is; true when one was open.
    pub fn close_panels(&mut self) -> bool {
        self.popups.panel.take().is_some()
    }

    /// Closes this panel; true when it was the open one.
    pub fn close_panel(&mut self, panel: Panel) -> bool {
        self.panel_is(panel) && self.close_panels()
    }

    /// Loads the title index again now: the server said the library changed.
    pub fn reload_catalog(&mut self, cx: &mut Context<Self>) {
        self.search.loaded = None;
        self.refresh_catalog(cx);
    }

    /// Loads the title index, when there is none or it is older than ten
    /// minutes, and fetches the posters of its titles to the disk.
    pub fn refresh_catalog(&mut self, cx: &mut Context<Self>) {
        const MAX_AGE: Duration = Duration::from_secs(600);
        if self.search.loaded.is_some_and(|at| at.elapsed() < MAX_AGE) {
            return;
        }
        if self.session.is_none() {
            return;
        }
        self.search.loaded = Some(std::time::Instant::now());
        // The answer is for the session that asked and for the newest
        // request: another profile or server may be open by then with an
        // index of its own, or the library changed and a newer request is
        // on its way. An older answer is dropped, posters and all.
        let epoch = self.session_epoch;
        let request = self.next_revision();
        self.search.request = request;
        let query = crate::pages::catalog_query();
        self.fetch(
            cx,
            move |client| {
                let started = std::time::Instant::now();
                let page = client.items(&query)?;
                log::info!(
                    "title index: {} titles in {} ms",
                    page.items.len(),
                    started.elapsed().as_millis()
                );
                Ok(page.items)
            },
            move |this, result, cx| {
                if this.session_epoch != epoch || this.search.request != request {
                    return;
                }
                match result {
                    Ok(items) => {
                        if let Some(session) = &this.session {
                            let width = (this.metrics().portrait_w * 2.) as u32;
                            let posters = items
                                .iter()
                                .filter_map(|item| item.poster_url(&session.client, width))
                                .collect();
                            crate::images::prewarm(posters, cx);
                        }
                        // The titles are folded once here, not at each key.
                        this.search.catalog = crate::search_index::Index::new(items);
                    }
                    // The next search tries again.
                    Err(err) => {
                        log::warn!("title index failed: {err:#}");
                        this.search.loaded = None;
                    }
                }
            },
        );
    }

    /// (Re)loads the data for the current page.
    pub fn load_page(&mut self, cx: &mut Context<Self>) {
        self.next_generation();
        // Without the server, the pages show what is on this Mac.
        if self.offline_load_page(cx) {
            return;
        }
        match &self.nav.page {
            Page::Home(_) => self.load_home(cx),
            Page::Library(_) => self.load_library(cx),
            Page::Detail(_) => self.load_detail(cx),
            Page::Admin(_) => self.load_admin(cx),
            Page::Playlist(_) => self.load_playlist(cx),
            // The downloads page reads the index of this Mac; nothing loads.
            Page::Downloads => {}
            // The settings load with the session and stay in the app.
            Page::Settings(_) => {
                if !self.prefs.loaded {
                    self.load_prefs(cx);
                }
            }
            Page::Search(_) => self.load_search(cx),
        }
    }

    /// Loads the rows of the home page and, at sign-in, the libraries. `load_page` calls this.
    pub(super) fn load_home(&mut self, cx: &mut Context<Self>) {
        let Page::Home(data) = &mut self.nav.page else {
            return;
        };
        data.loading = true;
        let libraries = home_libraries(&self.views);
        let need_hero = self.hero.slides.is_empty();
        // At sign-in the libraries are not known yet. They load in
        // the same pass as the page, so the page does not wait for
        // them first.
        let need_views = self.views.is_empty();
        self.fetch_for_page(
            cx,
            move |client| {
                let started = std::time::Instant::now();
                let loaded = crate::pages::load_home(&client, libraries, need_views, need_hero)?;
                log::info!(
                    "home data loaded in {} ms ({} ms after start)",
                    started.elapsed().as_millis(),
                    crate::perf::since_start_ms()
                );
                Ok(loaded)
            },
            move |this, result, cx| {
                if let Page::Home(data) = &mut this.nav.page {
                    match result {
                        Ok((views, mut fresh)) => {
                            if !views.is_empty() {
                                this.views = views;
                            }
                            let hero = std::mem::take(&mut fresh.hero);
                            *data = fresh;
                            if this.hero.slides.is_empty() && !hero.is_empty() {
                                this.hero.slides = hero;
                                this.show_hero_slide(0);
                            }
                        }
                        Err(err) => {
                            data.loading = false;
                            // No server at all: the offline state takes over.
                            if !this.request_failed(&err, cx) {
                                this.toast("Could not load home", format!("{err:#}"), cx);
                            }
                        }
                    }
                }
                cx.notify();
            },
        );
    }

    /// Loads the first page of the library on show. `load_page` calls this.
    pub(super) fn load_library(&mut self, cx: &mut Context<Self>) {
        let Page::Library(data) = &mut self.nav.page else {
            return;
        };
        data.loading = true;
        let query = library_query(data);
        self.fetch_for_page(
            cx,
            move |client| {
                let started = std::time::Instant::now();
                let page = client.items(&query)?;
                log::info!(
                    "library data loaded in {} ms ({} items)",
                    started.elapsed().as_millis(),
                    page.items.len()
                );
                Ok(page)
            },
            move |this, result, cx| {
                if let Page::Library(data) = &mut this.nav.page {
                    data.loading = false;
                    match result {
                        Ok(page) => {
                            data.total = page.total_record_count;
                            data.items = page.items;
                        }
                        Err(err) => {
                            if !this.request_failed(&err, cx) {
                                this.toast("Could not load library", format!("{err:#}"), cx)
                            }
                        }
                    }
                }
                cx.notify();
            },
        );
    }

    /// Loads the item of the detail page and the rows under it. `load_page` calls this.
    pub(super) fn load_detail(&mut self, cx: &mut Context<Self>) {
        let Page::Detail(data) = &mut self.nav.page else {
            return;
        };
        data.loading = true;
        // The card already says what kind of item this is, so the
        // requests for its rows start with the item request.
        let known = Item {
            id: data.item.id.clone(),
            kind: data.item.kind.clone(),
            series_id: data.item.series_id.clone(),
            season_id: data.item.season_id.clone(),
            ..Default::default()
        };
        self.fetch_for_page(
            cx,
            move |client| {
                let started = std::time::Instant::now();
                let fresh = crate::pages::load_detail(&client, &known)?;
                log::info!("detail data loaded in {} ms", started.elapsed().as_millis());
                Ok(fresh)
            },
            move |this, result, cx| {
                if let Page::Detail(data) = &mut this.nav.page {
                    match result {
                        Ok(mut fresh) => {
                            // A reload of the same item keeps the
                            // tracks the user picked on the page.
                            if fresh.item.id == data.item.id {
                                (fresh.audio, fresh.subtitle) = (data.audio, data.subtitle);
                            }
                            **data = fresh;
                            // The row of the season starts at the
                            // episode of the page; the ones before
                            // it are a scroll to the left away.
                            if data.item.kind == ItemKind::Episode {
                                let at = data
                                    .episodes
                                    .iter()
                                    .position(|ep| ep.id == data.item.id)
                                    .unwrap_or(0);
                                let offset = at as f32 * this.detail_episode_step();
                                this.row_scroll(&"detail.season-episodes".into())
                                    .set_offset(gpui_kit::point(px(-offset), px(0.)));
                            }
                            this.rebuild_detail_menus(cx);
                            this.load_enhanced_detail(cx);
                        }
                        Err(err) => {
                            data.loading = false;
                            if !this.request_failed(&err, cx) {
                                this.toast("Could not load details", format!("{err:#}"), cx)
                            }
                        }
                    }
                }
                cx.notify();
            },
        );
    }

    /// Runs the search of the page on the server. `load_page` calls this.
    pub(super) fn load_search(&mut self, cx: &mut Context<Self>) {
        let Page::Search(data) = &mut self.nav.page else {
            return;
        };
        if data.query.is_empty() {
            // The title index may already show results for the text
            // in the field; the server search starts when typing rests.
            if data.typed.is_empty() {
                data.results.clear();
                data.loading = false;
            }
            return;
        }
        data.loading = true;
        data.pending = Some(data.query.clone());
        let text = data.query.clone();
        let asked = text.clone();
        self.fetch_for_page(
            cx,
            move |client| {
                let started = std::time::Instant::now();
                let found = crate::pages::load_search(&client, &text)?;
                log::info!("search data loaded in {} ms", started.elapsed().as_millis());
                Ok(found)
            },
            move |this, result, cx| {
                if let Page::Search(data) = &mut this.nav.page {
                    if data.pending.as_deref() == Some(asked.as_str()) {
                        data.pending = None;
                    }
                    // The user typed on; a search for the new text
                    // follows. The answer is kept for a return to
                    // this text.
                    let shown = data.typed == data.query && data.query == asked;
                    match result {
                        Ok((items, seerr)) => {
                            if shown {
                                data.results = items.clone();
                                data.seerr = seerr.clone();
                                data.loading = false;
                            }
                            data.answered = Some((asked, items, seerr));
                        }
                        Err(err) => {
                            if !shown {
                                return;
                            }
                            data.loading = false;
                            if !this.request_failed(&err, cx) {
                                this.toast("Search failed", format!("{err:#}"), cx)
                            }
                        }
                    }
                }
                cx.notify();
            },
        );
    }
}
