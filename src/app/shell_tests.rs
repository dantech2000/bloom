// SPDX-License-Identifier: AGPL-3.0-or-later
//! The shell of the app with no request on its way: Back and Forward, the
//! focus, one popup at a time, the debug channel's own question, and the
//! saves of the config. See `race_harness` for the setup.

use gpui_kit::{Focusable as _, TestAppContext};

use super::race_harness::{MockServer, add_server, app, item, plain, profile, session};
use super::{Page, Panel, Screen};

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

/// A save of the config is a write with a sync of the disk on the UI
/// thread: a session that opens with the profile that is active already
/// (every start) writes nothing, and a sign-in or a sign-out writes once.
#[gpui_kit::test]
fn the_config_is_saved_once_for_a_change_of_the_profile(cx: &mut TestAppContext) {
    let server = MockServer::start(|method, path, _| plain(method, path));
    let (bloom, cx) = app(cx);
    bloom.update(cx, |this, cx| {
        add_server(this, "one", &server.url, "u1");
        let saves = |this: &super::Bloom| this.config_saves.get();
        let before = saves(this);
        assert!(this.open_session("one", "u1", cx));
        assert_eq!(saves(this), before + 1, "a new active profile is saved once");
        assert!(this.open_session("one", "u1", cx));
        assert_eq!(saves(this), before + 1, "the profile that is active already was saved again");
        // A sign-in to the active profile (a new token), and to another one.
        this.add_profile("one", profile("u1"), cx);
        assert_eq!(saves(this), before + 2, "a sign-in to the active profile");
        this.add_profile("one", profile("u2"), cx);
        assert_eq!(saves(this), before + 3, "a sign-in to another profile");
        this.sign_out(cx);
        assert_eq!(saves(this), before + 4, "a sign-out");
        assert!(this.config.active.is_none());
    });
    cx.run_until_parked();
}

/// The place of the window is saved once the changes stop, and the save
/// does not count as one that still waits: the way out of the window
/// wrote the config again each time after a move.
#[gpui_kit::test]
fn a_saved_window_place_does_not_wait_to_be_saved_again(cx: &mut TestAppContext) {
    let (bloom, cx) = app(cx);
    cx.run_until_parked();
    let before = bloom.read_with(cx, |this, _| this.config_saves.get());
    cx.simulate_resize(gpui_kit::size(gpui_kit::px(1000.), gpui_kit::px(700.)));
    cx.run_until_parked();
    assert!(bloom.read_with(cx, |this, _| this.config.window.is_some()), "the resize did not reach the app");
    cx.executor().advance_clock(std::time::Duration::from_millis(700));
    cx.run_until_parked();
    assert_eq!(bloom.read_with(cx, |this, _| this.config_saves.get()), before + 1, "the place was not saved once");
    bloom.update(cx, |this, cx| {
        assert!(!this.flush_config_save(cx), "a save that went out still counts as waiting");
    });
}
