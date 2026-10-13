// SPDX-License-Identifier: AGPL-3.0-or-later
//! Races of the navigation, the title index and the search (review of
//! 2026-10-05, UI findings 1, 3 and 6). See `race_harness` for the setup.

use std::sync::atomic::{AtomicUsize, Ordering};

use gpui_kit::{Focusable as _, TestAppContext};

use super::race_harness::{MockServer, add_server, app, item, items_json, plain, session, tick_until};
use super::{Page, Panel, Screen};

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

/// The answer of a page that the user has left does not land in the page
/// of the same kind that is open now (`Bloom::fetch_for_page`).
#[gpui_kit::test(iterations = 20)]
fn the_answer_of_a_library_the_user_left_stays_out_of_the_next_one(cx: &mut TestAppContext) {
    let server = MockServer::start(|method, path, _| match (method, path) {
        ("GET", p) if p.starts_with("/Items?") && p.contains("lib1") => (200, items_json(&["a1", "a2"], "Movie")),
        ("GET", p) if p.starts_with("/Items?") && p.contains("lib2") => (200, items_json(&["b1"], "Movie")),
        _ => plain(method, path),
    });
    let (bloom, cx) = app(cx);
    bloom.update(cx, |this, cx| {
        this.session = Some(session(&server.url, "u1"));
        this.screen = Screen::Main;
        this.open_library(item("lib1", "One", "CollectionFolder"), cx);
        // The user opens another library before the first one answers.
        this.open_library(item("lib2", "Two", "CollectionFolder"), cx);
    });
    cx.run_until_parked();
    bloom.read_with(cx, |this, _| match &this.page {
        Page::Library(data) => {
            let ids: Vec<&str> = data.items.iter().map(|i| i.id.as_str()).collect();
            assert_eq!(ids, ["b1"], "library two shows the titles of library one");
        }
        _ => panic!("the second library is not open"),
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

#[gpui_kit::test]
fn escape_closes_an_open_panel_before_it_goes_back(cx: &mut TestAppContext) {
    let server = MockServer::start(|method, path, _| plain(method, path));
    let (bloom, cx) = app(cx);
    bloom.update(cx, |this, cx| {
        this.session = Some(session(&server.url, "u1"));
        this.screen = Screen::Main;
        this.open_search("tro".into(), cx);
        this.open_item(item("m9", "Nine", "Movie"), cx);
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        let focus = bloom.read(cx).app_focus.clone();
        window.focus(&focus, cx);
    });
    for cast in [false, true] {
        bloom.update(cx, |this, cx| {
            this.panel = Some(if cast { Panel::Cast } else { Panel::SyncPlay });
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_keystrokes("escape");
        bloom.read_with(cx, |this, _| {
            assert!(this.panel.is_none(), "Escape left the panel open");
            assert!(matches!(this.page, Page::Detail(_)), "Escape went back with a panel open");
        });
    }
    // With no panel open Escape goes back, as before.
    cx.simulate_keystrokes("escape");
    bloom.read_with(cx, |this, _| assert!(matches!(this.page, Page::Search(_)), "Escape did not go back"));
}

// ----- review of 2026-10-07 --------------------------------------------------

/// The item of a detail page with one audio track of the name, as the
/// server sends it.
fn json_with_audio(id: &str, track: &str) -> serde_json::Value {
    serde_json::json!({
        "Id": id, "Name": id, "Type": "Movie",
        "MediaSources": [{ "MediaStreams": [{ "Type": "Audio", "Index": 1, "DisplayTitle": track }] }],
    })
}

fn item_with_audio(id: &str, track: &str) -> crate::jellyfin::Item {
    serde_json::from_value(json_with_audio(id, track)).unwrap()
}

/// The menus of the detail page are one set for the app. A page that Back
/// brings up again has its data and loads nothing, so its menus must be
/// built again for it: they held the entries of the item opened after it,
/// and "Edit metadata" there opened that other item.
#[gpui_kit::test]
fn back_to_a_detail_page_brings_its_own_menus(cx: &mut TestAppContext) {
    let server = MockServer::start(|method, path, _| match (method, path) {
        ("GET", p) if p.starts_with("/Items/b") && !p.contains("/Similar") => (200, json_with_audio("b", "Track of B").to_string()),
        _ => plain(method, path),
    });
    let (bloom, cx) = app(cx);
    let detail = |item| Page::Detail(super::DetailData { item, ..Default::default() });
    bloom.update(cx, |this, cx| {
        this.session = Some(session(&server.url, "u1"));
        this.screen = Screen::Main;
        this.page = detail(item_with_audio("a", "Track of A"));
        this.rebuild_detail_menus(cx);
        assert_eq!(this.detail_audio_menu.read(cx).labels(), ["Track of A"]);
        this.navigate(detail(item_with_audio("b", "")), cx);
    });
    cx.run_until_parked();
    bloom.update(cx, |this, cx| {
        assert_eq!(this.detail_audio_menu.read(cx).labels(), ["Track of B"]);
        this.back(cx);
    });
    cx.run_until_parked();
    bloom.read_with(cx, |this, cx| {
        assert_eq!(this.detail_audio_menu.read(cx).labels(), ["Track of A"], "the menu of the page is the one of the item after it");
    });
}

/// A question of the dashboard holds an action for the page and the server
/// it was asked on. It goes when the page or the session changes: left
/// open, it showed again on the next page with dialogs, and its action ran
/// against the server opened since.
#[gpui_kit::test]
fn a_question_does_not_outlive_its_page_or_its_session(cx: &mut TestAppContext) {
    let server = MockServer::start(|method, path, _| plain(method, path));
    let (bloom, cx) = app(cx);
    let ask = |this: &mut super::Bloom| {
        this.dialogs.confirm = Some(crate::admin::Confirm {
            title: "Shut down the server?".into(),
            message: String::new(),
            action: "Shut down".into(),
            danger: true,
            run: std::rc::Rc::new(|_, _| {}),
        });
    };
    bloom.update(cx, |this, cx| {
        add_server(this, "one", &server.url, "u1");
        add_server(this, "two", &server.url, "u2");
        assert!(this.open_session("one", "u1", cx));
        ask(this);
        this.open_item(item("m1", "One", "Movie"), cx);
        assert!(this.dialogs.confirm.is_none(), "the question is open on the next page");
        ask(this);
        this.back(cx);
        assert!(this.dialogs.confirm.is_none(), "the question is open after Back");
        ask(this);
        assert!(this.open_session("two", "u2", cx));
        assert!(this.dialogs.confirm.is_none(), "the question is open for the next session");
    });
    cx.run_until_parked();
}

/// The hero slides are those of one user: the next session loads its own.
#[gpui_kit::test]
fn the_hero_slides_of_one_profile_do_not_show_for_the_next(cx: &mut TestAppContext) {
    let server = MockServer::start(|method, path, _| plain(method, path));
    let (bloom, cx) = app(cx);
    bloom.update(cx, |this, cx| {
        add_server(this, "one", &server.url, "u1");
        add_server(this, "two", &server.url, "u2");
        assert!(this.open_session("one", "u1", cx));
        this.hero.slides = vec![item("m1", "Of user one", "Movie")];
        this.hero.index = 0;
        assert!(this.open_session("two", "u2", cx));
        assert!(this.hero.slides.is_empty(), "the slides of the profile before are still there");
    });
    cx.run_until_parked();
}

/// The debug channel answers a question only when a debug command opened
/// it: not one a person opened, also when that one took the place in
/// memory of a question the channel opened before.
#[gpui_kit::test]
fn the_debug_channel_knows_its_own_question(cx: &mut TestAppContext) {
    let (bloom, cx) = app(cx);
    let question = || crate::admin::Confirm {
        title: "Shut down the server?".into(),
        message: String::new(),
        action: "Shut down".into(),
        danger: true,
        run: std::rc::Rc::new(|_, _| {}),
    };
    bloom.update(cx, |this, _| {
        // A person opened it.
        this.dialogs.confirm = Some(question());
        assert!(!this.debug_opened_confirm());
        // A debug command opened this one, and the channel noted it.
        let own = question();
        this.debug_dialogs.0 = Some(std::rc::Rc::downgrade(&own.run));
        this.dialogs.confirm = Some(own);
        assert!(this.debug_opened_confirm());
        // It is closed, and a person opens another: not the channel's.
        this.dialogs.confirm = None;
        this.dialogs.confirm = Some(question());
        assert!(!this.debug_opened_confirm(), "a question of a person counts as the channel's");
    });
}

/// A session that opens asks for its user four times within a moment (the
/// admin flag, the download right, the list rights, the subtitle rights):
/// one request answers them all. Another session has its own answer.
#[gpui_kit::test]
fn the_user_is_asked_for_once_for_those_who_ask_together(_cx: &mut TestAppContext) {
    let server = MockServer::start(|method, path, _| match (method, path) {
        ("GET", p) if p.starts_with("/Users/Me") => (200, r#"{"Name":"u","Policy":{"IsAdministrator":true}}"#.into()),
        _ => plain(method, path),
    });
    let client = session(&server.url, "u1").client;
    for _ in 0..4 {
        assert_eq!(client.me().unwrap()["Policy"]["IsAdministrator"], true);
    }
    assert_eq!(server.count("GET", "/Users/Me"), 1);
    let other = crate::jellyfin::Client::new(&server.url, "device").with_token("another-token", "u2");
    other.me().unwrap();
    assert_eq!(server.count("GET", "/Users/Me"), 2, "another session got the user of the first");
}

/// A reload of the detail page keeps the tracks the user picked on it.
#[gpui_kit::test]
fn a_reload_of_the_detail_page_keeps_the_picked_tracks(cx: &mut TestAppContext) {
    let server = MockServer::start(|method, path, _| match (method, path) {
        ("GET", p) if p.starts_with("/Items/a") && !p.contains("/Similar") => {
            (200, json_with_audio("a", "Track of A").to_string())
        }
        _ => plain(method, path),
    });
    let (bloom, cx) = app(cx);
    bloom.update(cx, |this, cx| {
        this.session = Some(session(&server.url, "u1"));
        this.screen = Screen::Main;
        this.page = Page::Detail(super::DetailData {
            item: item_with_audio("a", "Track of A"),
            audio: Some(0),
            subtitle: Some(None),
            ..Default::default()
        });
        this.load_page(cx);
    });
    cx.run_until_parked();
    bloom.read_with(cx, |this, _| match &this.page {
        Page::Detail(data) => {
            assert!(!data.loading);
            assert_eq!((data.audio, data.subtitle), (Some(0), Some(None)), "the reload dropped the picked tracks");
        }
        _ => panic!("not the detail page"),
    });
}

/// "Open a random title" answers after the user switched the profile: the
/// title of the session before does not open in the new one.
#[gpui_kit::test]
fn an_answer_for_the_session_before_opens_no_page(cx: &mut TestAppContext) {
    let server = MockServer::start(|method, path, _| match (method, path) {
        ("GET", p) if p.starts_with("/Items?") && p.contains("Random") => (200, items_json(&["m7"], "Movie")),
        _ => plain(method, path),
    });
    let (bloom, cx) = app(cx);
    bloom.update(cx, |this, cx| {
        add_server(this, "one", &server.url, "u1");
        add_server(this, "two", &server.url, "u2");
        assert!(this.open_session("one", "u1", cx));
        this.open_random(cx);
        assert!(this.open_session("two", "u2", cx));
    });
    cx.run_until_parked();
    bloom.read_with(cx, |this, _| {
        assert!(matches!(this.page, Page::Home(_)), "the random title of the session before opened");
    });
}

/// The keys list is a popup like the panels: it closes with them, and a
/// question of the dashboard does not open beside a panel.
#[gpui_kit::test]
fn the_keys_list_and_the_panels_close_together(cx: &mut TestAppContext) {
    let (bloom, cx) = app(cx);
    bloom.update(cx, |this, cx| {
        this.panel = Some(Panel::Help);
        assert!(this.panel_open());
        assert!(this.close_panels());
        assert!(!this.panel_open());
        assert!(!this.close_panels());
        this.panel = Some(Panel::Cast);
        this.ask_confirm(
            crate::admin::Confirm {
                title: String::new(),
                message: String::new(),
                action: String::new(),
                danger: false,
                run: std::rc::Rc::new(|_, _| {}),
            },
            cx,
        );
        assert!(this.panel.is_none(), "a panel stays open beside the question");
    });
}

/// One popup at a time: a panel takes the place of an open menu and of
/// another panel, and a menu takes the place of a panel.
#[gpui_kit::test]
fn a_panel_and_a_menu_are_never_open_together(cx: &mut TestAppContext) {
    let (bloom, cx) = app(cx);
    let menu = bloom.read_with(cx, |this, _| this.profile_menu.clone());
    cx.update(|window, cx| {
        menu.update(cx, |menu, cx| {
            menu.set_items([crate::ui::menu::MenuItem::new("one", "One")], cx);
            menu.open(window, cx);
        });
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        bloom.update(cx, |this, cx| {
            assert!(this.toggle_panel(Panel::SyncPlay, window, cx));
            assert!(!this.profile_menu.read(cx).is_open(), "the menu stays open beside the panel");
            this.open_panel(Panel::Cast, window, cx);
            assert!(this.panel_is(Panel::Cast) && !this.panel_is(Panel::SyncPlay));
        });
        menu.update(cx, |menu, cx| menu.open(window, cx));
    });
    cx.run_until_parked();
    bloom.read_with(cx, |this, cx| {
        assert!(this.profile_menu.read(cx).is_open());
        assert!(!this.panel_open(), "the panel stays open beside the menu");
    });
}

/// The click that closed a panel from outside does not open it again when
/// it was a click on the panel's own button.
#[gpui_kit::test]
fn the_button_of_a_panel_closed_from_outside_does_not_open_it_again(cx: &mut TestAppContext) {
    let (bloom, cx) = app(cx);
    cx.update(|window, cx| {
        bloom.update(cx, |this, cx| {
            assert!(this.toggle_panel(Panel::SyncPlay, window, cx));
            this.dismiss_panel(Panel::SyncPlay, cx);
            assert!(!this.toggle_panel(Panel::SyncPlay, window, cx), "the same click opened the panel again");
            // The next click is a new one, and another panel never waits.
            assert!(this.toggle_panel(Panel::SyncPlay, window, cx));
            this.dismiss_panel(Panel::SyncPlay, cx);
            assert!(this.toggle_panel(Panel::Cast, window, cx));
            assert!(!this.toggle_panel(Panel::Cast, window, cx), "a second press left the panel open");
            assert!(!this.panel_open());
        });
    });
}

/// A password typed for one server is cleared when another server is
/// picked: Enter must not send it there.
#[gpui_kit::test]
fn a_change_of_the_server_clears_the_typed_password(cx: &mut TestAppContext) {
    let server = MockServer::start(|method, path, _| plain(method, path));
    let (bloom, cx) = app(cx);
    bloom.update(cx, |this, cx| {
        add_server(this, "one", &server.url, "u1");
        add_server(this, "two", &server.url, "u2");
        this.select_server("one".into(), cx);
        this.connect.clear_password = false;
        this.select_server("one".into(), cx);
        assert!(!this.connect.clear_password, "the same server again clears what the user types");
        this.select_server("two".into(), cx);
        assert!(this.connect.clear_password);
    });
    cx.run_until_parked();
}

/// The render thread looks at the picture for the events only while a
/// program can get the colours: the setting for the colours off, or the
/// stream off, ends the looks (the tick that would end them does not run
/// with the stream off). And colours of a look before a time off are not
/// those of the picture when the looks begin again.
#[gpui_kit::test]
fn the_looks_at_the_picture_end_with_their_setting(cx: &mut TestAppContext) {
    let (bloom, cx) = app(cx);
    cx.run_until_parked();
    bloom.update(cx, |this, cx| {
        this.player.watch_colours(true);
        this.player.put_colours(vec![[1, 2, 3]]);
        assert_eq!(this.player.colours().1, [[1, 2, 3]]);
        this.set_events_colours(false, cx);
        assert!(!this.player.watches_colours(), "the colours setting is off and the render thread still looks");
        this.player.watch_colours(true);
        assert!(this.player.colours().1.is_empty(), "the colours of an older look are still there");
        // On while on keeps what the render thread found.
        this.player.put_colours(vec![[4, 5, 6]]);
        this.player.watch_colours(true);
        assert_eq!(this.player.colours().1, [[4, 5, 6]]);
        this.set_events(false, cx);
        assert!(!this.player.watches_colours(), "the stream is off and the render thread still looks");
    });
    cx.run_until_parked();
}

/// A setting that changes many times in a row is saved once, when the
/// changes stop; and a save that still waits is written at once on the way
/// out of the window, as its task ends with the window.
#[gpui_kit::test]
fn a_save_that_waits_is_written_once_and_on_the_way_out(cx: &mut TestAppContext) {
    let (bloom, cx) = app(cx);
    let saves = |bloom: &gpui_kit::Entity<super::Bloom>, cx: &mut gpui_kit::VisualTestContext| {
        bloom.read_with(cx, |this, _| this.config_saves.get())
    };
    cx.run_until_parked();
    let before = saves(&bloom, cx);
    bloom.update(cx, |this, cx| {
        // A slider that is dragged.
        for _ in 0..20 {
            this.save_config_soon(cx);
        }
    });
    assert_eq!(saves(&bloom, cx), before, "a save went out at each step");
    cx.executor().advance_clock(std::time::Duration::from_millis(500));
    cx.run_until_parked();
    assert_eq!(saves(&bloom, cx), before + 1, "the changes were not saved once they stopped");
    // A change, and the window goes before the wait is over.
    bloom.update(cx, |this, cx| {
        this.save_config_soon(cx);
        assert!(this.flush_config_save(cx), "no save waited");
        assert!(!this.flush_config_save(cx), "the save went out twice");
    });
    assert_eq!(saves(&bloom, cx), before + 2);
    cx.executor().advance_clock(std::time::Duration::from_millis(500));
    cx.run_until_parked();
    assert_eq!(saves(&bloom, cx), before + 2, "the task that waited saved again");
}

/// Four callers ask for the user at the same time, as at the start of a
/// session: one request answers them all. A server that is slow holds up
/// no caller of another server.
#[test]
fn the_user_is_asked_for_once_by_callers_at_the_same_time() {
    use std::{thread, time::{Duration, Instant}};
    let slow = MockServer::start(|method, path, _| match (method, path) {
        ("GET", p) if p.starts_with("/Users/Me") => {
            thread::sleep(Duration::from_millis(400));
            (200, r#"{"Name":"slow"}"#.into())
        }
        _ => plain(method, path),
    });
    let fast = MockServer::start(|method, path, _| match (method, path) {
        ("GET", p) if p.starts_with("/Users/Me") => (200, r#"{"Name":"fast"}"#.into()),
        _ => plain(method, path),
    });
    let client = crate::jellyfin::Client::new(&slow.url, "device").with_token("token-of-the-slow-one", "u1");
    let callers: Vec<_> = (0..4)
        .map(|_| {
            let client = client.clone();
            thread::spawn(move || client.me().unwrap()["Name"].as_str().map(str::to_string))
        })
        .collect();
    // While the slow server still works on its answer.
    thread::sleep(Duration::from_millis(100));
    let other = crate::jellyfin::Client::new(&fast.url, "device").with_token("token-of-the-fast-one", "u2");
    let started = Instant::now();
    assert_eq!(other.me().unwrap()["Name"], "fast");
    assert!(started.elapsed() < Duration::from_millis(250), "the slow server held up the other: {:?}", started.elapsed());
    for caller in callers {
        assert_eq!(caller.join().unwrap().as_deref(), Some("slow"));
    }
    assert_eq!(slow.count("GET", "/Users/Me"), 1, "each caller sent a request of its own");
}
