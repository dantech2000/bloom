// SPDX-License-Identifier: AGPL-3.0-or-later
//! The macOS menu bar and its standard shortcuts.

use gpui_kit::{
    App, KeyBinding, Menu, MenuItem, OsAction, actions,
    base::input::{Copy, Cut, Paste, Redo, SelectAll, Undo},
};

actions!(
    bloom,
    [
        About,
        CheckForUpdates,
        Settings,
        Hide,
        HideOthers,
        ShowAll,
        Quit,
        CloseWindow,
        Minimize,
        Zoom,
        BringAllToFront,
        ToggleFullScreen,
        Back,
        Forward,
        Home,
        Search,
        Reload,
        Random,
        PlayPause,
        PictureInPicture,
        OpenReadme,
        OpenReleaseNotes,
        ReportProblem,
    ]
);

/// The pages of the project the Help menu opens in the browser.
pub const README_URL: &str = "https://github.com/dantech2000/bloom#readme";
pub const RELEASES_URL: &str = "https://github.com/dantech2000/bloom/releases";
pub const ISSUES_URL: &str = "https://github.com/dantech2000/bloom/issues";

/// Registers the shortcuts, the app-wide actions and the menus. The window
/// actions (back, search, fullscreen and so on) are handled by the main
/// view. The edit actions are those of the text fields (`input::Undo` and
/// so on, bound to cmd-z and the rest inside a field), so the menu runs
/// what the keys run, and nothing when no field has the focus.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-,", Settings, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
        KeyBinding::new("cmd-[", Back, None),
        KeyBinding::new("cmd-]", Forward, None),
        KeyBinding::new("shift-cmd-h", Home, None),
        KeyBinding::new("cmd-f", Search, None),
        KeyBinding::new("cmd-r", Reload, None),
        KeyBinding::new("shift-cmd-p", PictureInPicture, None),
    ]);
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &BringAllToFront, _| crate::macos::arrange_in_front());
    cx.on_action(|_: &OpenReadme, cx| crate::macos::open_web_url(cx, README_URL));
    cx.on_action(|_: &OpenReleaseNotes, cx| crate::macos::open_web_url(cx, RELEASES_URL));
    cx.on_action(|_: &ReportProblem, cx| crate::macos::open_web_url(cx, ISSUES_URL));
    set_menus(cx, false);
}

/// Builds the menu bar. The full-screen entry reads by the state of the
/// window; the main view calls this again when that state changes.
pub fn set_menus(cx: &mut App, fullscreen: bool) {
    let menu = |name: &'static str, items: Vec<MenuItem>| Menu {
        name: name.into(),
        items,
        disabled: false,
    };
    cx.set_menus(vec![
        // macOS shows the first menu under the app's name.
        menu(
            crate::brand::NAME,
            vec![
                MenuItem::action(format!("About {}", crate::brand::NAME), About),
                MenuItem::action("Check for Updates…", CheckForUpdates),
                MenuItem::separator(),
                MenuItem::action("Settings…", Settings),
                MenuItem::separator(),
                MenuItem::action(format!("Hide {}", crate::brand::NAME), Hide),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::action("Show All", ShowAll),
                MenuItem::separator(),
                MenuItem::action(format!("Quit {}", crate::brand::NAME), Quit),
            ],
        ),
        // Close Window lives under File, where Music, TV and System
        // Settings have it; the app has no documents, so it is the only entry.
        menu("File", vec![MenuItem::action("Close Window", CloseWindow)]),
        menu(
            "Edit",
            vec![
                MenuItem::os_action("Undo", Undo, OsAction::Undo),
                MenuItem::os_action("Redo", Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action("Cut", Cut, OsAction::Cut),
                MenuItem::os_action("Copy", Copy, OsAction::Copy),
                MenuItem::os_action("Paste", Paste, OsAction::Paste),
                MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
            ],
        ),
        menu(
            "View",
            vec![
                MenuItem::action("Home", Home),
                MenuItem::action("Back", Back),
                MenuItem::action("Forward", Forward),
                MenuItem::action("Search", Search),
                MenuItem::action("Random Item", Random),
                MenuItem::separator(),
                MenuItem::action("Reload", Reload),
                MenuItem::separator(),
                MenuItem::action(
                    if fullscreen { "Exit Full Screen" } else { "Enter Full Screen" },
                    ToggleFullScreen,
                ),
            ],
        ),
        menu(
            "Playback",
            vec![
                MenuItem::action("Play or Pause", PlayPause),
                MenuItem::action("Picture in Picture", PictureInPicture),
            ],
        ),
        // gpui registers the menu named "Window" with AppKit, which adds
        // the list of windows below these entries.
        menu(
            "Window",
            vec![
                MenuItem::action("Minimize", Minimize),
                MenuItem::action("Zoom", Zoom),
                MenuItem::separator(),
                MenuItem::action("Bring All to Front", BringAllToFront),
            ],
        ),
        // AppKit adds the search field to the menu named "Help".
        menu(
            "Help",
            vec![
                MenuItem::action(format!("{} README", crate::brand::NAME), OpenReadme),
                MenuItem::action("Release Notes", OpenReleaseNotes),
                MenuItem::separator(),
                MenuItem::action("Report a Problem", ReportProblem),
            ],
        ),
    ]);
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use gpui_kit::{
        AppContext as _, ClipboardItem, Context, Entity, InteractiveElement as _, IntoElement,
        ParentElement as _, Render, Styled as _, TestAppContext, VisualTestContext, Window, div,
        base::input::{InputState, Textarea, TextareaState},
    };

    use super::*;

    /// A window with one text field of the app and a Back listener above
    /// it, as the main view has.
    enum Field {
        Line(Entity<InputState>),
        Area(Entity<TextareaState>),
    }

    struct Form {
        field: Field,
        /// The root, which holds the focus when no field does, as the
        /// main screen does (`app_focus`).
        focus: gpui_kit::FocusHandle,
        /// Back and Forward that reached the form; the main view counts
        /// the same way, and passes the keys on in the text editor.
        backs: Rc<Cell<usize>>,
        forwards: Rc<Cell<usize>>,
    }

    impl Render for Form {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let editor = matches!(self.field, Field::Area(_));
            div()
                .size_full()
                .track_focus(&self.focus)
                .on_action(cx.listener(move |this, _: &Back, _, cx| {
                    if editor {
                        cx.propagate();
                    } else {
                        this.backs.set(this.backs.get() + 1)
                    }
                }))
                .on_action(cx.listener(move |this, _: &Forward, _, cx| {
                    if editor {
                        cx.propagate();
                    } else {
                        this.forwards.set(this.forwards.get() + 1)
                    }
                }))
                .child(match &self.field {
                    Field::Line(state) => crate::ui::input::Input::new(state).into_any_element(),
                    Field::Area(state) => Textarea::new(state).into_any_element(),
                })
        }
    }

    fn form(
        cx: &mut TestAppContext,
        make: impl FnOnce(&mut Window, &mut Context<Form>) -> Field,
    ) -> (Entity<Form>, &mut VisualTestContext) {
        cx.update(|cx| {
            crate::ui::theme::init(cx);
            init(cx);
        });
        let (form, cx) = cx.add_window_view(|window, cx| Form {
            field: make(window, cx),
            focus: cx.focus_handle(),
            backs: Rc::default(),
            forwards: Rc::default(),
        });
        cx.update(|window, cx| {
            form.update(cx, |form, cx| match &form.field {
                Field::Line(state) => state.update(cx, |state, cx| state.focus(window, cx)),
                Field::Area(state) => state.update(cx, |state, cx| state.focus(window, cx)),
            })
        });
        (form, cx)
    }

    fn value(form: &Entity<Form>, cx: &VisualTestContext) -> String {
        form.read_with(cx, |form, cx| match &form.field {
            Field::Line(state) => state.read(cx).value().to_string(),
            Field::Area(state) => state.read(cx).value().to_string(),
        })
    }

    fn clipboard(cx: &VisualTestContext) -> String {
        cx.read_from_clipboard().and_then(|item| item.text()).unwrap_or_default()
    }

    /// The Edit menu runs the actions of the field (`input::SelectAll` and
    /// so on), and the keys run the same actions: each step checks the
    /// text and the clipboard. `copies` says whether the field gives its
    /// text to the clipboard (a password field does not).
    fn edit_menu_works(form: &Entity<Form>, cx: &mut VisualTestContext, copies: bool) {
        cx.simulate_input("hello world");
        assert_eq!(value(form, cx), "hello world");
        cx.dispatch_action(SelectAll);
        cx.dispatch_action(Copy);
        assert_eq!(clipboard(cx), if copies { "hello world" } else { "" }, "Copy");
        cx.write_to_clipboard(ClipboardItem::new_string(String::new()));
        cx.dispatch_action(Cut);
        assert_eq!(value(form, cx), if copies { "" } else { "hello world" }, "Cut takes the text");
        assert_eq!(clipboard(cx), if copies { "hello world" } else { "" }, "Cut fills the clipboard");
        if !copies {
            return;
        }
        cx.dispatch_action(Paste);
        assert_eq!(value(form, cx), "hello world", "Paste");
        cx.dispatch_action(Undo);
        assert_eq!(value(form, cx), "", "Undo takes the paste back");
        cx.dispatch_action(Redo);
        assert_eq!(value(form, cx), "hello world", "Redo");
        // The keys, once each: cmd-a cmd-c fills the clipboard with the
        // text once, cmd-v puts the clipboard in place of the selection.
        cx.write_to_clipboard(ClipboardItem::new_string(String::new()));
        cx.simulate_keystrokes("cmd-a cmd-c");
        assert_eq!(clipboard(cx), "hello world", "cmd-c");
        cx.write_to_clipboard(ClipboardItem::new_string("x".into()));
        cx.simulate_keystrokes("cmd-a cmd-v");
        assert_eq!(value(form, cx), "x", "cmd-v");
        cx.simulate_keystrokes("cmd-z");
        assert_eq!(value(form, cx), "hello world", "cmd-z");
        cx.simulate_keystrokes("shift-cmd-z");
        assert_eq!(value(form, cx), "x", "shift-cmd-z");
        cx.simulate_keystrokes("cmd-a cmd-x");
        assert_eq!((value(form, cx), clipboard(cx)), (String::new(), "x".into()), "cmd-x");
    }

    #[gpui_kit::test]
    fn the_edit_menu_works_in_a_text_field(cx: &mut TestAppContext) {
        let (form, cx) = form(cx, |window, cx| Field::Line(cx.new(|cx| InputState::new(window, cx))));
        edit_menu_works(&form, cx, true);
    }

    #[gpui_kit::test]
    fn the_edit_menu_works_in_a_password_field(cx: &mut TestAppContext) {
        let (form, cx) = form(cx, |window, cx| {
            Field::Line(cx.new(|cx| InputState::new(window, cx).masked(true)))
        });
        edit_menu_works(&form, cx, false);
    }

    #[gpui_kit::test]
    fn the_edit_menu_works_in_the_text_editor(cx: &mut TestAppContext) {
        let (form, cx) = form(cx, |window, cx| {
            Field::Area(cx.new(|cx| TextareaState::new(window, cx).rows(4)))
        });
        edit_menu_works(&form, cx, true);
    }

    /// The field binds cmd-[ and cmd-] too (outdent, indent); the menu's
    /// bindings came later and win, as in Safari, where cmd-[ goes back
    /// from a focused field. In a one-line field that is right.
    #[gpui_kit::test]
    fn back_by_key_wins_in_a_one_line_field_and_with_no_field(cx: &mut TestAppContext) {
        let (form, cx) = form(cx, |window, cx| Field::Line(cx.new(|cx| InputState::new(window, cx))));
        cx.simulate_input("abc");
        cx.simulate_keystrokes("cmd-[");
        assert_eq!(form.read_with(cx, |form, _| form.backs.get()), 1, "Back in a one-line field");
        assert_eq!(value(&form, cx), "abc");
        cx.simulate_keystrokes("cmd-]");
        assert_eq!(form.read_with(cx, |form, _| form.forwards.get()), 1, "Forward in a one-line field");
        // With the screen itself focused the key is Back as well.
        cx.update(|window, cx| {
            let focus = form.read(cx).focus.clone();
            window.focus(&focus, cx);
        });
        cx.simulate_keystrokes("cmd-[");
        assert_eq!(form.read_with(cx, |form, _| form.backs.get()), 2, "Back did not fire with no field focused");
        // The edit actions do nothing with no field focused.
        cx.write_to_clipboard(ClipboardItem::new_string("kept".into()));
        cx.dispatch_action(SelectAll);
        cx.dispatch_action(Copy);
        cx.dispatch_action(Paste);
        assert_eq!(clipboard(cx), "kept");
        assert_eq!(value(&form, cx), "abc");
    }

    /// In the text editor the main view passes Back and Forward on, and
    /// gpui runs the field's own binding next: cmd-] indents the line.
    #[gpui_kit::test]
    fn the_text_editor_keeps_indent_and_outdent(cx: &mut TestAppContext) {
        let (form, cx) = form(cx, |window, cx| {
            Field::Area(cx.new(|cx| TextareaState::new(window, cx).rows(4)))
        });
        cx.simulate_input("abc");
        cx.simulate_keystrokes("cmd-]");
        assert_eq!(form.read_with(cx, |form, _| form.forwards.get()), 0, "Forward fired in the editor");
        let indented = value(&form, cx);
        assert_ne!(indented, "abc", "cmd-] did not indent");
        assert!(indented.ends_with("abc"), "{indented:?}");
        cx.simulate_keystrokes("cmd-[");
        assert_eq!(form.read_with(cx, |form, _| form.backs.get()), 0, "Back fired in the editor");
        assert_eq!(value(&form, cx), "abc", "cmd-[ did not outdent");
    }
}
