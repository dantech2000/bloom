//! The shadcn Nova Button visual port.
//!
//! Visual source: shadcn/ui 4.19.0 `button.tsx` and `style-nova.css` at
//! `1773ecfeeb4a04366978d353e69b5c7ded78dcb2`. Interaction comes from the
//! GPUI Kit headless button.

use std::rc::Rc;

use gpui_kit::base::Button as BaseButton;
use gpui_kit::{
    AnyElement, App, ClickEvent, ElementId, FontWeight, IntoElement, ParentElement, RenderOnce,
    SharedString, Styled, Window, px,
};

use super::theme::{ThemeMode, UiRadius, UiTheme};

type ButtonClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// The pinned shadcn Button visual variant.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ButtonVariant {
    /// Primary action.
    #[default]
    Default,
    /// Low-emphasis action.
    Ghost,
}

/// The pinned shadcn Button size.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ButtonSize {
    /// 32px-high default button.
    #[default]
    Default,
    /// 28px square icon button.
    IconSm,
    /// 32px square icon button.
    Icon,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ButtonMetrics {
    height: f32,
    horizontal_padding: f32,
    gap: f32,
    radius: f32,
    text_size: f32,
    icon_only: bool,
}

impl ButtonSize {
    fn metrics(self, radii: UiRadius) -> ButtonMetrics {
        let radius = f32::from(radii.lg);
        let radius_md = f32::from(radii.md);
        match self {
            Self::Default => ButtonMetrics::new(32.0, 10.0, 6.0, radius, 14.0, false),
            Self::IconSm => ButtonMetrics::new(28.0, 0.0, 0.0, radius_md.min(12.0), 14.0, true),
            Self::Icon => ButtonMetrics::new(32.0, 0.0, 0.0, radius, 14.0, true),
        }
    }
}

impl ButtonMetrics {
    const fn new(
        height: f32,
        horizontal_padding: f32,
        gap: f32,
        radius: f32,
        text_size: f32,
        icon_only: bool,
    ) -> Self {
        Self {
            height,
            horizontal_padding,
            gap,
            radius,
            text_size,
            icon_only,
        }
    }
}

/// A styled Button that keeps GPUI Kit's pointer and keyboard behavior.
#[derive(IntoElement)]
pub struct Button {
    style: gpui_kit::StyleRefinement,
    id: ElementId,
    variant: ButtonVariant,
    size: ButtonSize,
    aria_label: Option<SharedString>,
    on_click: Option<ButtonClickHandler>,
    tooltip: Option<Rc<dyn Fn(&mut Window, &mut App) -> gpui_kit::AnyView>>,
    children: Vec<AnyElement>,
}

impl Button {
    /// Creates a Button with a caller-owned stable ID.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            style: gpui_kit::StyleRefinement::default(),
            id: id.into(),
            variant: ButtonVariant::default(),
            size: ButtonSize::default(),
            aria_label: None,
            on_click: None,
            tooltip: None,
            children: Vec::new(),
        }
    }

    /// Sets the visual variant.
    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    /// Sets the visual size.
    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    /// Sets the accessible name for icon or custom content.
    pub fn aria_label(mut self, label: impl Into<SharedString>) -> Self {
        self.aria_label = Some(label.into());
        self
    }

    /// Runs for pointer, Enter, or Space activation.
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }

    /// Shows a view when the pointer rests on the Button; see `ui::tip::tip`.
    pub fn tooltip(
        mut self,
        build: impl Fn(&mut Window, &mut App) -> gpui_kit::AnyView + 'static,
    ) -> Self {
        self.tooltip = Some(Rc::new(build));
        self
    }

}

impl ParentElement for Button {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let root = style_button(BaseButton::new(self.id), self.variant, self.size, UiTheme::read(cx));
        let mut root = super::theme::apply_style(root, &self.style).children(self.children);

        if let Some(label) = self.aria_label {
            root = root.accessibility_label(label);
        }
        if let Some(handler) = self.on_click {
            root = root.on_click(move |event, window, cx| handler(event, window, cx));
        }
        if let Some(build) = self.tooltip {
            root = gpui_kit::StatefulInteractiveElement::tooltip(root, move |window, cx| {
                build(window, cx)
            });
        }

        root
    }
}

fn style_button<
    T: Styled + gpui_kit::InteractiveElement + gpui_kit::prelude::FluentBuilder,
>(
    base: T,
    variant: ButtonVariant,
    size: ButtonSize,
    theme: &UiTheme,
) -> T {
    let colors = theme.colors;
    let metrics = size.metrics(theme.radius);
    let focus_border = colors.ring;

    let base = base
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .whitespace_nowrap()
        .h(theme.space(metrics.height / 4.))
        .gap(theme.space(metrics.gap / 4.))
        .rounded(px(metrics.radius))
        .border_1()
        .border_color(colors.background.opacity(0.0))
        .font_family(theme.fonts.body.clone())
        .font_weight(FontWeight::MEDIUM)
        .text_size(theme.text(metrics.text_size))
        .focus_visible(move |style| style.border_color(focus_border))
        .when(metrics.icon_only, |base| {
            base.w(theme.space(metrics.height / 4.)).p_0()
        })
        .when(!metrics.icon_only, |base| {
            base.px(theme.space(metrics.horizontal_padding / 4.))
        });

    let base = match variant {
        ButtonVariant::Default => base
            .bg(colors.primary)
            .text_color(colors.primary_foreground)
            .hover(move |style| style.bg(colors.primary.opacity(0.80))),
        ButtonVariant::Ghost => {
            let hover = match theme.mode {
                ThemeMode::Light => colors.muted,
                ThemeMode::Dark => colors.muted.opacity(0.50),
            };
            base.bg(colors.background.opacity(0.0))
                .text_color(colors.foreground)
                .hover(move |style| style.bg(hover).text_color(colors.foreground))
        }
    };

    base.cursor_pointer()
}

impl gpui_kit::Styled for Button {
    fn style(&mut self) -> &mut gpui_kit::StyleRefinement {
        &mut self.style
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_variants_show_the_clickable_cursor() {
        for theme in [UiTheme::neutral_light(), UiTheme::neutral_dark()] {
            for variant in [ButtonVariant::Default, ButtonVariant::Ghost] {
                let mut button = style_button(gpui_kit::div(), variant, ButtonSize::Default, &theme);
                assert_eq!(button.style().mouse_cursor, Some(gpui_kit::CursorStyle::PointingHand));
            }
        }
    }

    #[test]
    fn nova_sizes_are_exact() {
        let sizes = [
            (ButtonSize::Default, 32.0, 10.0, 6.0, 10.0),
            (ButtonSize::IconSm, 28.0, 0.0, 0.0, 8.0),
            (ButtonSize::Icon, 32.0, 0.0, 0.0, 10.0),
        ];

        for (size, height, padding, gap, radius) in sizes {
            let metrics = size.metrics(UiRadius::new(px(10.0)));
            assert_eq!(metrics.height, height);
            assert_eq!(metrics.horizontal_padding, padding);
            assert_eq!(metrics.gap, gap);
            assert_eq!(metrics.radius, radius);
        }
    }
}
