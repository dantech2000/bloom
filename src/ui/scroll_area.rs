//! Native vertical scrolling with Nova-styled GPUI Kit scrollbars.

use super::theme::UiTheme;
use gpui_kit::base::{Scrollbar, ScrollbarHandle, ScrollbarMode};
use gpui_kit::{
    AnyElement, App, ElementId, InteractiveElement as _, IntoElement, ParentElement, RenderOnce,
    Role, ScrollHandle, StatefulInteractiveElement as _, Styled, Window, div, px,
};

/// Overlay on a relative container holding a list or scroll viewport.
pub fn scroll_area_scrollbar_for<H: ScrollbarHandle + Clone>(
    id: impl Into<ElementId>,
    target: &H,
    cx: &App,
) -> Scrollbar {
    let t = UiTheme::read(cx);
    Scrollbar::vertical(target)
        .id(id)
        .mode(ScrollbarMode::Scrolling)
        .styles(|styles| {
            styles
                .track(|style| style.width(t.space(2.5)))
                .thumb(|style| {
                    style
                        .bg(t.colors.border)
                        .width(t.space(2.))
                        .inset(px(1.))
                        .radius(t.space(1.))
                })
                .thumb_hover(|style| style.bg(t.colors.muted_foreground.opacity(0.72)))
        })
}

#[derive(IntoElement)]
/// A native scroll viewport with a Nova-styled Kit scrollbar.
pub struct ScrollArea {
    id: ElementId,
    style: gpui_kit::StyleRefinement,
    handle: Option<ScrollHandle>,
    children: Vec<AnyElement>,
}
impl ScrollArea {
    /// Creates a `ScrollArea` with a stable caller-owned ID.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            style: Default::default(),
            handle: None,
            children: vec![],
        }
    }
    /// Uses a caller-owned scroll handle in place of the internal one.
    pub fn track(mut self, handle: &ScrollHandle) -> Self {
        self.handle = Some(handle.clone());
        self
    }
}
impl ParentElement for ScrollArea {
    fn extend(&mut self, children: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(children);
    }
}
impl Styled for ScrollArea {
    fn style(&mut self) -> &mut gpui_kit::StyleRefinement {
        &mut self.style
    }
}
impl RenderOnce for ScrollArea {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let handle = self.handle.clone().unwrap_or_else(|| {
            window
                .use_keyed_state((self.id.clone(), "scroll"), cx, |_, _| ScrollHandle::new())
                .read(cx)
                .clone()
        });
        let keyboard = handle.clone();
        let viewport = div()
            .id("viewport")
            .role(Role::ScrollView)
            .focusable()
            .size_full()
            // A gesture along the other axis does not move this area.
            .restrict_scroll_to_axis()
            .track_scroll(&handle)
            .overflow_y_scroll()
            .on_key_down(move |event, window, cx| {
                if event.keystroke.modifiers.modified() {
                    return;
                }
                let mut offset = keyboard.offset();
                let max = keyboard.max_offset().y;
                let page = keyboard.bounds().size.height;
                let value = &mut offset.y;
                *value = match event.keystroke.key.as_str() {
                    "up" => *value + px(32.),
                    "down" => *value - px(32.),
                    "pageup" => *value + page,
                    "pagedown" => *value - page,
                    "home" => px(0.),
                    "end" => -max,
                    _ => return,
                };
                *value = (*value).clamp(-max.max(px(0.)), px(0.));
                keyboard.set_offset(offset);
                window.refresh();
                cx.stop_propagation();
            })
            .child(div().flex().flex_col().min_w_full().children(self.children));
        let scrollbar = scroll_area_scrollbar_for("scrollbar", &handle, cx);
        let t = UiTheme::read(cx);
        let root = div()
            .id(self.id)
            .relative()
            .min_h_0()
            .min_w_0()
            .font_family(t.fonts.body.clone())
            .text_color(t.colors.foreground)
            .child(viewport)
            .child(scrollbar);
        super::theme::apply_style(root, &self.style)
    }
}
