// SPDX-License-Identifier: AGPL-3.0-or-later
//! The buttons of the detail page and the Playback menu of the menu bar
//! (2026-11-18): the watched and favourite states show at once, and come
//! back when the server refuses; the menu's actions reach the player.

use std::time::Duration;

use gpui_kit::TestAppContext;

use super::race_harness::{MockServer, app, item, plain, session, tick_until};
use super::{DetailData, Page, Screen};

fn detail_server(status: u16) -> MockServer {
    MockServer::start(move |method, path, _| match (method, path) {
        ("POST", p) if p.contains("/UserFavoriteItems/") => {
            // Slow enough that the state before the answer shows.
            std::thread::sleep(Duration::from_millis(300));
            (status, r#"{"IsFavorite":true,"Played":false}"#.to_string())
        }
        _ => plain(method, path),
    })
}

fn favorite(bloom: &gpui_kit::Entity<super::Bloom>, cx: &mut gpui_kit::VisualTestContext) -> bool {
    bloom.read_with(cx, |this, _| match &this.nav.page {
        Page::Detail(data) => data.item.user_data.is_favorite,
        _ => panic!("not on the detail page"),
    })
}

fn open_film(bloom: &gpui_kit::Entity<super::Bloom>, cx: &mut gpui_kit::VisualTestContext, url: &str) {
    bloom.update(cx, |this, _| {
        this.session = Some(session(url, "u1"));
        this.screen = Screen::Main;
        this.nav.page = Page::Detail(Box::new(DetailData {
            item: item("m1", "Film", "Movie"),
            ..Default::default()
        }));
    });
}

#[gpui_kit::test]
fn the_heart_fills_before_the_server_answers(cx: &mut TestAppContext) {
    let server = detail_server(200);
    let (bloom, cx) = app(cx);
    open_film(&bloom, cx, &server.url);
    bloom.update(cx, |this, cx| this.toggle_favorite(cx));
    assert!(favorite(&bloom, cx), "the heart waited for the server");
    tick_until(cx, || server.count("POST", "/UserFavoriteItems/") == 1);
    cx.run_until_parked();
    assert!(favorite(&bloom, cx), "the answer of the server keeps it");
}

#[gpui_kit::test]
fn a_refused_favourite_goes_back(cx: &mut TestAppContext) {
    let server = detail_server(500);
    let (bloom, cx) = app(cx);
    open_film(&bloom, cx, &server.url);
    bloom.update(cx, |this, cx| this.toggle_favorite(cx));
    assert!(favorite(&bloom, cx));
    tick_until(cx, || server.count("POST", "/UserFavoriteItems/") == 1);
    // The answer, a task of its own, lands; the state of before comes back.
    for _ in 0..50 {
        cx.run_until_parked();
        if !favorite(&bloom, cx) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!favorite(&bloom, cx), "a refusal puts the state of before back");
}

#[gpui_kit::test]
fn the_playback_menu_reaches_the_player(cx: &mut TestAppContext) {
    let server = MockServer::start(|method, path, _| plain(method, path));
    let (bloom, cx) = app(cx);
    bloom.update(cx, |this, _| {
        this.session = Some(session(&server.url, "u1"));
        this.screen = Screen::Main;
        this.playback.open = true;
    });
    cx.run_until_parked();
    let speed = |cx: &mut gpui_kit::VisualTestContext| bloom.read_with(cx, |this, _| this.playback.speed);
    cx.dispatch_action(crate::menus::Faster);
    cx.run_until_parked();
    assert_eq!(speed(cx), 1.25);
    cx.dispatch_action(crate::menus::Faster);
    cx.dispatch_action(crate::menus::Faster);
    cx.run_until_parked();
    assert_eq!(speed(cx), 1.75, "1.75x is a step of its own");
    cx.dispatch_action(crate::menus::NormalSpeed);
    cx.run_until_parked();
    assert_eq!(speed(cx), 1.);
    let muted = bloom.read_with(cx, |this, _| this.playback.muted);
    cx.dispatch_action(crate::menus::ToggleMute);
    cx.run_until_parked();
    assert_eq!(bloom.read_with(cx, |this, _| this.playback.muted), !muted);
    // On the pages the menu does nothing.
    bloom.update(cx, |this, _| this.playback.open = false);
    cx.run_until_parked();
    cx.dispatch_action(crate::menus::Faster);
    cx.run_until_parked();
    assert_eq!(speed(cx), 1.);
}
