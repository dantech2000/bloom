// SPDX-License-Identifier: AGPL-3.0-or-later
//! The macOS menu bar and its standard shortcuts.

use gpui_kit::{App, KeyBinding, Menu, MenuItem, actions};

actions!(
    bloom,
    [
        About,
        Hide,
        HideOthers,
        ShowAll,
        Quit,
        Minimize,
        Zoom,
        ToggleFullScreen,
        Back,
        Home,
        Search,
        Reload,
        Random,
        PlayPause,
        PictureInPicture,
    ]
);

/// Registers the shortcuts, the app-wide actions and the menus. The window
/// actions (back, search, fullscreen and so on) are handled by the main view.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
        KeyBinding::new("cmd-[", Back, None),
        KeyBinding::new("shift-cmd-h", Home, None),
        KeyBinding::new("cmd-f", Search, None),
        KeyBinding::new("cmd-r", Reload, None),
        KeyBinding::new("shift-cmd-p", PictureInPicture, None),
    ]);
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());

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
                MenuItem::separator(),
                MenuItem::action(format!("Hide {}", crate::brand::NAME), Hide),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::action("Show All", ShowAll),
                MenuItem::separator(),
                MenuItem::action(format!("Quit {}", crate::brand::NAME), Quit),
            ],
        ),
        menu(
            "View",
            vec![
                MenuItem::action("Home", Home),
                MenuItem::action("Back", Back),
                MenuItem::action("Search", Search),
                MenuItem::action("Random Item", Random),
                MenuItem::separator(),
                MenuItem::action("Reload", Reload),
                MenuItem::separator(),
                MenuItem::action("Enter Full Screen", ToggleFullScreen),
            ],
        ),
        menu(
            "Playback",
            vec![
                MenuItem::action("Play or Pause", PlayPause),
                MenuItem::action("Picture in Picture", PictureInPicture),
            ],
        ),
        menu(
            "Window",
            vec![
                MenuItem::action("Minimize", Minimize),
                MenuItem::action("Zoom", Zoom),
            ],
        ),
    ]);
}
