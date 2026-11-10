//! Nova-styled Avatar with native image loading and error fallback.

use gpui_kit::{
    App, ElementId, ImageSource, InteractiveElement as _, IntoElement, ObjectFit, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled, StyledImage, Window, div,
    img,
};

use super::theme::UiTheme;

/// A compact, circular Avatar with an optional image and fallback content.
#[derive(IntoElement)]
pub struct Avatar {
    style: gpui_kit::StyleRefinement,
    id: ElementId,
    image: Option<ImageSource>,
    fallback: SharedString,
    aria_label: Option<SharedString>,
}

impl Avatar {
    /// Creates a `Avatar` with a stable caller-owned ID.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            style: gpui_kit::StyleRefinement::default(),
            id: id.into(),
            image: None,
            fallback: SharedString::default(),
            aria_label: None,
        }
    }

    /// Sets the text shown while the image is unavailable.
    pub fn fallback(mut self, text: impl Into<SharedString>) -> Self {
        self.fallback = text.into();
        self
    }
    /// Sets the avatar image source.
    pub fn image(mut self, source: impl Into<ImageSource>) -> Self {
        self.image = Some(source.into());
        self
    }
    /// Sets the accessible name of the control.
    pub fn aria_label(mut self, label: impl Into<SharedString>) -> Self {
        self.aria_label = Some(label.into());
        self
    }
}

impl RenderOnce for Avatar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = UiTheme::read(cx);
        let image_size = theme.space(8.);
        let (muted, ink) = (theme.colors.muted, theme.colors.muted_foreground);
        let text_size = theme.text(14.);
        let fallback = move || {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(muted)
                .text_color(ink)
                .text_size(text_size)
                .child(self.fallback.clone())
                .into_any_element()
        };
        let content = match self.image {
            Some(source) => {
                let loading = fallback.clone();
                img(source)
                    .size_full()
                    .rounded_full()
                    .object_fit(ObjectFit::Cover)
                    .with_loading(loading)
                    .with_fallback(fallback)
                    .into_any_element()
            }
            None => fallback(),
        };
        let root = div()
            .id(self.id)
            .size(image_size)
            .flex_shrink_0()
            .rounded_full()
            .overflow_hidden()
            .child(content);
        let root = if let Some(label) = self.aria_label {
            root.aria_label(label)
        } else {
            root
        };
        super::theme::apply_style(root, &self.style)
    }
}

impl gpui_kit::Styled for Avatar {
    fn style(&mut self) -> &mut gpui_kit::StyleRefinement {
        &mut self.style
    }
}
