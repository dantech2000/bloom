// SPDX-License-Identifier: AGPL-3.0-or-later
//! Quality tags of the Jellyfin Enhanced plugin, the parts that need the
//! server or the window: the request for the choices of the user, and the
//! look of one tag. The rules that derive and order the tags are in
//! `bloom_core::quality_tags`.

use gpui_kit::{Div, ParentElement as _, Styled, div, px, rgb, rgba};

pub use bloom_core::quality_tags::{
    Category, Input, Prefs, arrange, detect, force_categories, labels, master_switch, prefs, set_prefs,
};

use crate::jellyfin::Client;

#[easy_ext::ext(QualityApi)]
pub(crate) impl Client {
    /// The master switch of the tags and the choices of the user for the
    /// six categories. `None` without the plugin. The caller puts the
    /// choices in force (`set_prefs`), on the UI thread.
    fn quality_prefs(&self) -> Option<(bool, Prefs)> {
        use serde_json::Value;
        let user = self.user().ok()?.to_string();
        let own: Value = self
            .get(&format!("/JellyfinEnhanced/user-settings/{user}/settings.json"), &[])
            .ok()?;
        let public: Value = self
            .get("/JellyfinEnhanced/public-config", &[])
            .unwrap_or(Value::Null);
        // A null of the user is no choice: the server default counts
        // then, as for the categories (`Prefs::from_settings`).
        Some((master_switch(&own, &public), Prefs::from_settings(&own, &public)))
    }
}

/// One tag, in the look of the tags on the posters.
pub fn pill(label: String) -> Div {
    div()
        .px(px(6.))
        .py(px(1.))
        .rounded(px(6.))
        .bg(rgba(0x000000b3))
        .border_1()
        .border_color(rgba(0xffffff33))
        .text_size(px(11.))
        .font_weight(gpui_kit::FontWeight::BOLD)
        .text_color(rgb(0xffffff))
        .child(label)
}
