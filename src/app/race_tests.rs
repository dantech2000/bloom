// SPDX-License-Identifier: AGPL-3.0-or-later
//! Races of the navigation, the title index and the search (review of
//! 2026-10-05, UI findings 1, 3 and 6). See `race_harness` for the setup.

use std::sync::atomic::{AtomicUsize, Ordering};

use gpui_kit::{Focusable as _, TestAppContext};

use super::race_harness::{MockServer, add_server, app, item, items_json, plain, session, tick_until};
use super::{Page, Screen};

// ----- finding 1 ----------------------------------------------------------

#[gpui_kit::test]
fn back_restores_a_library_page_whose_load_was_thrown_away(cx: &mut TestAppContext) {
    let server = MockServer::start(|method, path, _| match (method, path) {
        ("GET", p) if p.starts_with("/Items?") => (200, items_json(&["m1", "m2", "m3"], "Movie")),
        _ => plain(method, path),
    });
    let (bloom, cx) = app(cx);
    bloom.update(cx, |this, cx| {
        this.session = Some(session(&server.url, "u1"));
        this.screen = Screen::Main;
        this.open_library(item("lib1", "Movies", "CollectionFolder"), cx);
        // The user opens a title before the library answers.
        this.open_item(item("m9", "Nine", "Movie"), cx);
    });
    cx.run_until_parked();
    assert_eq!(server.count("GET", "/Items?"), 1, "the library asked once");
    bloom.update(cx, |this, cx| this.back(cx));
    cx.run_until_parked();
    bloom.read_with(cx, |this, _| match &this.page {
        Page::Library(data) => {
            assert!(
                !data.loading || server.count("GET", "/Items?") > 1,
                "Back shows the library as loading, and no request is on its way (items={})",
                data.items.len()
            );
            assert_eq!(data.items.len(), 3);
        }
        _ => panic!("Back did not return to the library"),
    });
}

// ----- finding 3 ----------------------------------------------------------

/// What marks the request of the title index among the item queries.
const INDEX: &str = "includeItemTypes=Movie,Series&recursive=true&sortBy=SortName";

/// A server whose title index grows by one title at each request: the
/// answer to an older request is told apart from a newer one.
fn growing_index(prefix: &'static str) -> MockServer {
    let asked = AtomicUsize::new(0);
    MockServer::start(move |method, path, _| match (method, path) {
        ("GET", p) if p.contains(INDEX) => {
            let n = asked.fetch_add(1, Ordering::Relaxed) + 1;
            let ids: Vec<String> = (1..=n).map(|i| format!("{prefix}{i}")).collect();
            let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
            (200, items_json(&ids, "Movie"))
        }
        _ => plain(method, path),
    })
}

fn two_servers(this: &mut super::Bloom, one: &MockServer, two: &MockServer) {
    add_server(this, "one", &one.url, "u1");
    add_server(this, "two", &two.url, "u2");
}

fn catalog_ids(this: &super::Bloom) -> Vec<String> {
    this.catalog.iter().map(|i| i.id.clone()).collect()
}

#[gpui_kit::test]
fn the_title_index_of_the_last_session_lands_in_the_new_one(cx: &mut TestAppContext) {
    let one = MockServer::start(|method, path, _| match (method, path) {
        ("GET", p) if p.starts_with("/Items?") => (200, items_json(&["a1", "a2", "a3"], "Movie")),
        _ => plain(method, path),
    });
    let two = MockServer::start(|method, path, _| match (method, path) {
        ("GET", p) if p.starts_with("/Items?") => (200, items_json(&["b1"], "Movie")),
        _ => plain(method, path),
    });
    let (bloom, cx) = app(cx);
    bloom.update(cx, |this, cx| {
        two_servers(this, &one, &two);
        this.session = Some(session(&one.url, "u1"));
        this.screen = Screen::Main;
        // The title index of server one is on its way...
        this.refresh_catalog(cx);
        // ...when the user picks the profile on server two.
        assert!(this.open_session("two", "u2", cx));
        assert!(this.catalog.is_empty());
    });
    cx.run_until_parked();
    bloom.read_with(cx, |this, _| {
        let ids = catalog_ids(this);
        assert!(
            ids.iter().all(|id| id.starts_with('b')),
            "the title index holds the titles of server one while server two is open: {ids:?}"
        );
    });
    assert_eq!(
        two.count("GET", "/Items/a1/Images"),
        0,
        "server two was asked for the poster of a title of server one"
    );
}

/// A -> B -> A: the first request of A answers late, in the second session
/// of A. The server and the user are the same, so only the epoch tells the
/// answer apart from the one of the new request.
#[gpui_kit::test(iterations = 20)]
fn an_old_answer_of_the_same_profile_does_not_replace_the_new_index(cx: &mut TestAppContext) {
    let one = growing_index("a");
    let two = MockServer::start(|method, path, _| plain(method, path));
    let (bloom, cx) = app(cx);
    bloom.update(cx, |this, cx| {
        two_servers(this, &one, &two);
        assert!(this.open_session("one", "u1", cx));
    });
    cx.run_until_parked();
    // The index asks again (the server said the library changed); the
    // request is served, and its answer waits for its turn...
    let before = one.count("GET", INDEX);
    let posters_before = one.count("GET", "/Items/a1/Images");
    bloom.update(cx, |this, cx| this.reload_catalog(cx));
    tick_until(cx, || one.count("GET", INDEX) > before);
    // ...while the user goes to server two and back, and the new session
    // asks for the index once more.
    bloom.update(cx, |this, cx| {
        assert!(this.open_session("two", "u2", cx));
        assert!(this.open_session("one", "u1", cx));
        this.refresh_catalog(cx);
    });
    cx.run_until_parked();
    let served = one.count("GET", INDEX);
    assert_eq!(served, before + 2, "two requests of the index after the first load");
    let newest: Vec<String> = (1..=served).map(|i| format!("a{i}")).collect();
    bloom.read_with(cx, |this, _| {
        assert_eq!(
            catalog_ids(this),
            newest,
            "the index shows the answer of the old session's request"
        );
    });
    assert_eq!(
        one.count("GET", "/Items/a1/Images"),
        posters_before + 1,
        "the posters were fetched for the old answer as well"
    );
}

/// Two refreshes of one session overlap: the older answer must not replace
/// the newer one, and the posters are fetched once.
#[gpui_kit::test(iterations = 20)]
fn an_older_refresh_does_not_replace_a_newer_one(cx: &mut TestAppContext) {
    let one = growing_index("a");
    let (bloom, cx) = app(cx);
    bloom.update(cx, |this, cx| {
        this.session = Some(session(&one.url, "u1"));
        this.screen = Screen::Main;
        this.refresh_catalog(cx);
    });
    tick_until(cx, || one.count("GET", INDEX) == 1);
    bloom.update(cx, |this, cx| this.reload_catalog(cx));
    cx.run_until_parked();
    assert_eq!(one.count("GET", INDEX), 2);
    bloom.read_with(cx, |this, _| {
        assert_eq!(catalog_ids(this), ["a1", "a2"], "the index shows the older answer");
    });
    assert_eq!(one.count("GET", "/Items/a1/Images"), 1, "the posters were fetched twice");
}

// ----- finding 6 ----------------------------------------------------------

fn search_server(status: u16) -> MockServer {
    MockServer::start(move |method, path, _| match (method, path) {
        ("GET", p) if p.starts_with("/Items?") && p.contains("searchTerm=pilot") => {
            (status, items_json(&["e1", "e2"], "Episode"))
        }
        _ => plain(method, path),
    })
}

fn open_pilot(bloom: &gpui_kit::Entity<super::Bloom>, cx: &mut gpui_kit::VisualTestContext, server: &MockServer) {
    bloom.update(cx, |this, cx| {
        this.session = Some(session(&server.url, "u1"));
        this.screen = Screen::Main;
        // The title index knows movies and series, not episodes.
        this.catalog = vec![item("m1", "Mayday", "Movie")];
        this.catalog_loaded = Some(std::time::Instant::now());
        this.open_search("pilot".into(), cx);
    });
}

/// The page after a letter more and the letter deleted again before the
/// typing rests: what two keystrokes do (`search_titles`). The server
/// search that follows the rest skips a text equal to the last one it ran
/// (`query.trim() != shown` in the input subscription), so the page must
/// not wait for it.
fn back_to_pilot(bloom: &gpui_kit::Entity<super::Bloom>, cx: &mut gpui_kit::VisualTestContext, server: &MockServer) {
    let asked = server.count("GET", "searchTerm=pilot");
    bloom.update(cx, |this, cx| {
        this.search_titles("pilots".into(), cx);
        this.search_titles("pilot".into(), cx);
    });
    cx.run_until_parked();
    bloom.read_with(cx, |this, _| match &this.page {
        Page::Search(data) => {
            assert_eq!(data.query, data.typed.as_str(), "the debounce would skip this text");
            assert!(
                !data.loading || server.count("GET", "searchTerm=pilot") > asked,
                "the page says loading, shows {} results, and no request is on its way",
                data.results.len()
            );
        }
        _ => panic!("not the search page"),
    });
}

#[gpui_kit::test]
fn typing_the_last_search_text_again_leaves_the_page_loading(cx: &mut TestAppContext) {
    let server = search_server(200);
    let (bloom, cx) = app(cx);
    open_pilot(&bloom, cx, &server);
    cx.run_until_parked();
    bloom.read_with(cx, |this, _| match &this.page {
        Page::Search(data) => {
            assert!(!data.loading);
            assert_eq!(data.results.len(), 2);
        }
        _ => panic!("not the search page"),
    });
    back_to_pilot(&bloom, cx, &server);
    bloom.read_with(cx, |this, _| match &this.page {
        Page::Search(data) => assert_eq!(data.results.len(), 2, "the answer of the server is gone"),
        _ => panic!("not the search page"),
    });
}

/// The answer came while a letter more was typed, and was not shown.
#[gpui_kit::test]
fn an_answer_that_came_while_typing_shows_on_the_return_to_its_text(cx: &mut TestAppContext) {
    let server = search_server(200);
    let (bloom, cx) = app(cx);
    open_pilot(&bloom, cx, &server);
    bloom.update(cx, |this, cx| this.search_titles("pilots".into(), cx));
    cx.run_until_parked();
    assert_eq!(server.count("GET", "searchTerm=pilot"), 1);
    bloom.read_with(cx, |this, _| match &this.page {
        Page::Search(data) => assert!(data.results.is_empty(), "the answer for 'pilot' shows for 'pilots'"),
        _ => panic!("not the search page"),
    });
    bloom.update(cx, |this, cx| this.search_titles("pilot".into(), cx));
    cx.run_until_parked();
    bloom.read_with(cx, |this, _| match &this.page {
        Page::Search(data) => {
            assert!(!data.loading, "the page waits for an answer that already came");
            assert_eq!(data.results.len(), 2);
        }
        _ => panic!("not the search page"),
    });
}

/// The search failed; a return to its text asks again.
#[gpui_kit::test]
fn a_return_to_a_text_whose_search_failed_asks_again(cx: &mut TestAppContext) {
    let server = search_server(500);
    let (bloom, cx) = app(cx);
    open_pilot(&bloom, cx, &server);
    cx.run_until_parked();
    assert_eq!(server.count("GET", "searchTerm=pilot"), 1);
    bloom.read_with(cx, |this, _| match &this.page {
        Page::Search(data) => assert!(!data.loading),
        _ => panic!("not the search page"),
    });
    back_to_pilot(&bloom, cx, &server);
    assert_eq!(server.count("GET", "searchTerm=pilot"), 2, "the search was not asked again");
    bloom.read_with(cx, |this, _| match &this.page {
        Page::Search(data) => assert!(!data.loading, "the page waits after the second failure"),
        _ => panic!("not the search page"),
    });
}

#[gpui_kit::test]
fn forward_returns_to_the_page_back_left_until_a_new_page_opens(cx: &mut TestAppContext) {
    let (bloom, cx) = app(cx);
    bloom.update(cx, |this, cx| {
        this.screen = Screen::Main;
        this.navigate(Page::Downloads, cx);
        this.navigate(Page::Settings(crate::settings::Section::About), cx);
        this.back(cx);
        assert!(matches!(this.page, Page::Downloads));
        assert_eq!((this.history.len(), this.forward.len()), (1, 1));
        this.forward(cx);
        assert!(matches!(this.page, Page::Settings(crate::settings::Section::About)));
        assert_eq!((this.history.len(), this.forward.len()), (2, 0));
        this.forward(cx);
        assert!(matches!(this.page, Page::Settings(_)), "Forward with nothing ahead stays");
        this.back(cx);
        this.back(cx);
        assert!(matches!(this.page, Page::Home(_)));
        assert_eq!((this.history.len(), this.forward.len()), (0, 2));
        this.forward(cx);
        assert!(matches!(this.page, Page::Downloads));
        // A new page forgets what was ahead.
        this.navigate(Page::Settings(crate::settings::Section::Profile), cx);
        assert_eq!(this.forward.len(), 0);
        this.forward(cx);
        assert!(matches!(this.page, Page::Settings(crate::settings::Section::Profile)));
        // Home starts over.
        this.open_home(cx);
        assert_eq!((this.history.len(), this.forward.len()), (0, 0));
    });
}

#[gpui_kit::test]
fn the_screen_takes_the_focus_back_when_the_search_field_leaves_the_page(cx: &mut TestAppContext) {
    let server = MockServer::start(|method, path, _| plain(method, path));
    let (bloom, cx) = app(cx);
    bloom.update(cx, |this, cx| {
        this.session = Some(session(&server.url, "u1"));
        this.screen = Screen::Main;
        this.open_search("tro".into(), cx);
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        bloom.update(cx, |this, cx| {
            let field = this.search_input.read(cx).focus_handle(cx);
            window.focus(&field, cx);
        });
    });
    // A frame with the field in it, then one without: the test window
    // draws only when asked.
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        assert!(bloom.read(cx).search_input.read(cx).focus_handle(cx).is_focused(window), "the field has the focus");
    });
    // Typing lands in the field: it is in the frame.
    cx.simulate_input("x");
    assert_eq!(bloom.read_with(cx, |this, cx| this.search_input.read(cx).value().to_string()), "x");
    // The user opens a result: the search page, with its field, is gone.
    bloom.update(cx, |this, cx| this.open_item(item("m9", "Nine", "Movie"), cx));
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.update(|window, cx| {
        let this = bloom.read(cx);
        assert!(!this.search_input.read(cx).focus_handle(cx).is_focused(window), "the field kept the focus");
        assert!(this.app_focus.is_focused(window), "the screen did not take the focus back");
    });
    // So Back by its key reaches the screen.
    cx.simulate_keystrokes("cmd-[");
    bloom.read_with(cx, |this, _| assert!(matches!(this.page, Page::Search(_)), "cmd-[ did not go back"));
}
