//! Nova menus composed from Kit buttons and popup positioning.
use super::{
    motion::{Enter, enter},
    theme::{UiTheme, apply_style},
};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::base::{Align, Button, ElementExt as _, Placement, Positioner};
use gpui_kit::{
    AnyElement, App, Bounds, ClickEvent, Context, ElementId, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Point, Render,
    RenderOnce, Role, SharedString, StatefulInteractiveElement as _, StyleRefinement, Styled,
    Window, div, prelude::FluentBuilder as _,
};
use std::{rc::Rc, time::Duration};

type Handler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
#[derive(Clone)]
/// An action, check item, radio item, separator or submenu entry.
pub struct MenuItem {
    id: ElementId,
    label: SharedString,
    disabled: bool,
    checked: Option<bool>,
    radio: bool,
    handler: Option<Handler>,
    /// Shared, so the copy of an entry a frame makes does not copy the
    /// tree under it.
    children: Rc<Vec<MenuItem>>,
    separator: bool,
    /// A heading: small, quiet text over a group of entries.
    heading: bool,
    icon: Option<LucideIcon>,
    /// A second, quiet line under the label.
    detail: Option<SharedString>,
}
impl MenuItem {
    /// Creates an action with a caller-owned ID, unique within this menu tree.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            disabled: false,
            checked: None,
            radio: false,
            handler: None,
            children: Rc::new(Vec::new()),
            separator: false,
            heading: false,
            icon: None,
            detail: None,
        }
    }
    /// Shows an icon before the label.
    pub fn icon(mut self, icon: LucideIcon) -> Self {
        self.icon = Some(icon);
        self
    }
    /// Shows a second, quiet line under the label.
    pub fn detail(mut self, text: impl Into<SharedString>) -> Self {
        self.detail = Some(text.into());
        self
    }
    /// Disables interaction and applies the disabled appearance.
    pub fn disabled(mut self, value: bool) -> Self {
        self.disabled = value;
        self
    }
    /// Makes this a checkbox item with the supplied initial state.
    pub fn checked(mut self, value: bool) -> Self {
        self.checked = Some(value);
        self
    }
    /// Makes this a radio item; activating it clears radio siblings at this menu level.
    pub fn radio(mut self, value: bool) -> Self {
        self.checked = Some(value);
        self.radio = true;
        self
    }
    /// Handles activation by pointer, keyboard or an accessibility action.
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.handler = Some(Rc::new(handler));
        self
    }
    /// Creates an item containing another level of menu entries.
    pub fn submenu(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        items: impl IntoIterator<Item = MenuItem>,
    ) -> Self {
        let mut item = Self::new(id, label);
        item.children = Rc::new(items.into_iter().collect());
        item
    }
    /// The text of an entry, for the debug channel; none for a separator.
    pub fn debug_label(&self) -> Option<String> {
        (!self.separator).then(|| self.label.to_string())
    }

    /// Creates a non-interactive separator between menu entries.
    pub fn separator() -> Self {
        let mut item = Self::new("separator", "");
        item.separator = true;
        item
    }
    /// Creates a non-interactive menu heading.
    pub fn label(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        let mut item = Self::new(id, label).disabled(true);
        item.heading = true;
        item
    }
    fn enabled(&self) -> bool {
        !self.disabled && !self.separator
    }
}

/// Retained popup visibility, item state and keyboard navigation.
pub struct MenuState {
    items: Vec<MenuItem>,
    open: bool,
    focus: FocusHandle,
    trigger: FocusHandle,
    return_focus: Option<FocusHandle>,
    anchor: Bounds<Pixels>,
    pointer: bool,
    /// On a touch window that is no sheet (a phone in landscape), the
    /// window y the popup's bottom edge stays above: the top of the bar
    /// its trigger sits in, so the popup clears the bar.
    pub touch_bottom: Option<f32>,
    path: Vec<usize>,
    highlighted: Vec<Option<usize>>,
    row_bounds: Vec<Vec<Bounds<Pixels>>>,
    popup_bounds: Vec<Bounds<Pixels>>,
    query: String,
    typed_at: Option<web_time::Instant>,
}
impl MenuState {
    /// The labels of the entries, for a test of what a menu holds.
    #[cfg(test)]
    pub fn labels(&self) -> Vec<String> {
        self.items.iter().filter_map(MenuItem::debug_label).collect()
    }

    /// Creates a closed menu with the supplied entries and new focus handles.
    pub fn new(items: impl IntoIterator<Item = MenuItem>, cx: &mut Context<Self>) -> Self {
        Self {
            items: items.into_iter().collect(),
            open: false,
            focus: cx.focus_handle().tab_stop(false),
            trigger: cx.focus_handle().tab_stop(true),
            return_focus: None,
            anchor: Default::default(),
            pointer: false,
            touch_bottom: None,
            path: Vec::new(),
            highlighted: Vec::new(),
            row_bounds: Vec::new(),
            popup_bounds: Vec::new(),
            query: String::new(),
            typed_at: None,
        }
    }
    /// Returns whether the popup is open.
    pub fn is_open(&self) -> bool {
        self.open
    }
    /// Whether the menu has no entries (such a menu does not open).
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    /// Replaces the menu entries and resets submenu navigation.
    pub fn set_items(&mut self, items: impl IntoIterator<Item = MenuItem>, cx: &mut Context<Self>) {
        self.items = items.into_iter().collect();
        self.path.clear();
        self.highlighted = vec![self.items.iter().position(MenuItem::enabled)];
        cx.notify();
    }
    /// Opens the menu at its trigger and transfers keyboard focus into it.
    pub fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_at(None, window, cx);
    }
    /// Opens at a pointer position, or at the trigger when the position is absent.
    pub fn open_at(
        &mut self,
        point: Option<Point<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A menu without entries has nothing to show. Opened anyway, it
        // drew an empty card under its trigger (the player's settings
        // button, before the entries of the menu were built).
        if self.items.is_empty() {
            return;
        }
        self.pointer = point.is_some();
        self.return_focus = if self.pointer {
            window.focused(cx)
        } else {
            None
        };
        if let Some(point) = point {
            self.anchor = Bounds::new(point, gpui_kit::size(gpui_kit::px(0.), gpui_kit::px(0.)));
        }
        self.open = true;
        self.path.clear();
        self.query.clear();
        self.highlighted = vec![self.items.iter().position(MenuItem::enabled)];
        self.focus.focus(window, cx);
        cx.notify();
    }
    /// Opens the menu at its trigger with the submenu of the entry with
    /// this label shown, as a hover over the entry does.
    pub fn open_submenu(&mut self, label: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open(window, cx);
        if let Some(index) = self
            .items
            .iter()
            .position(|item| item.label == label && !item.children.is_empty())
        {
            self.path = vec![index];
            self.highlighted = vec![
                Some(index),
                self.items[index].children.iter().position(MenuItem::enabled),
            ];
            cx.notify();
        }
    }
    /// Closes and optionally restores the prior context focus or menu trigger.
    pub fn close(&mut self, restore: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.open = false;
        self.path.clear();
        if restore {
            self.return_focus
                .take()
                .unwrap_or_else(|| self.trigger.clone())
                .focus(window, cx);
        }
        cx.notify();
    }
    fn level(&self, depth: usize) -> &[MenuItem] {
        let mut items = self.items.as_slice();
        for &index in self.path.iter().take(depth) {
            items = items.get(index).map_or(&[], |item| item.children.as_slice());
        }
        items
    }
    fn highlight(&mut self, depth: usize, index: usize, cx: &mut Context<Self>) {
        // The rows on screen can be those of the entries before
        // `set_items`: a place that is gone since is no entry.
        if !self.level(depth).get(index).is_some_and(MenuItem::enabled) {
            return;
        }
        if self.highlighted.get(depth).copied().flatten() == Some(index) {
            return;
        }
        self.path.truncate(depth);
        self.highlighted.resize(depth + 1, None);
        self.highlighted[depth] = Some(index);
        cx.notify();
    }
    fn activate(
        &mut self,
        depth: usize,
        index: usize,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = self.level(depth).get(index).cloned() else {
            return;
        };
        if !item.enabled() {
            return;
        }
        if !item.children.is_empty() {
            self.path.truncate(depth);
            self.path.push(index);
            self.highlighted.truncate(depth + 1);
            self.highlighted
                .push(item.children.iter().position(MenuItem::enabled));
            cx.notify();
        } else {
            let checked = item.checked.map(|value| item.radio || !value);
            if let Some(checked) = checked {
                let mut items = &mut self.items;
                for &parent in &self.path[..depth] {
                    items = Rc::make_mut(&mut items[parent].children);
                }
                if item.radio {
                    // The group of a radio entry: the radio entries next
                    // to it, up to a separator or a heading. A menu can
                    // hold two groups at one level (the sort key and the
                    // direction of a library), and a choice in one cleared
                    // the mark of the other.
                    let group = |item: &MenuItem| item.radio && !item.separator && !item.heading;
                    let first = items[..index].iter().rposition(|item| !group(item)).map_or(0, |n| n + 1);
                    let end = items[index..].iter().position(|item| !group(item)).map_or(items.len(), |n| index + n);
                    for sibling in &mut items[first..end] {
                        sibling.checked = Some(false);
                    }
                }
                items[index].checked = Some(checked);
            }
            self.close(true, window, cx);
            if let Some(handler) = item.handler {
                let event = event.clone();
                window.defer(cx, move |window, cx| {
                    handler(&event, window, cx);
                });
            }
        }
    }
    fn key(&mut self, event: &gpui_kit::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open {
            return;
        }
        let depth = self.path.len();
        let key = event.keystroke.key.as_str();
        if key == "escape" {
            self.close(true, window, cx);
        } else if key == "tab" {
            self.close(true, window, cx);
            return;
        } else if event.keystroke.modifiers.modified() {
            return;
        } else if key == "left" && depth > 0 {
            self.path.pop();
            self.highlighted.pop();
            cx.notify();
        } else if matches!(key, "enter" | "space" | "right") {
            if let Some(index) = self.highlighted[depth]
                && (key != "right" || !self.level(depth)[index].children.is_empty())
            {
                self.activate(
                    depth,
                    index,
                    &ClickEvent::Keyboard(Default::default()),
                    window,
                    cx,
                );
            }
        } else {
            let items = self.level(depth);
            let enabled: Vec<_> = items
                .iter()
                .enumerate()
                .filter_map(|(i, item)| item.enabled().then_some(i))
                .collect();
            if enabled.is_empty() {
                return;
            }
            let current = enabled
                .iter()
                .position(|i| Some(*i) == self.highlighted[depth])
                .unwrap_or(0);
            let next = match key {
                "up" => Some(enabled[(current + enabled.len() - 1) % enabled.len()]),
                "down" => Some(enabled[(current + 1) % enabled.len()]),
                "home" => enabled.first().copied(),
                "end" => enabled.last().copied(),
                _ if key.chars().count() == 1 => {
                    let now = web_time::Instant::now();
                    if self
                        .typed_at
                        .is_none_or(|time| now.duration_since(time) > Duration::from_millis(700))
                    {
                        self.query.clear();
                    }
                    self.typed_at = Some(now);
                    self.query.push_str(key);
                    let query = self.query.to_lowercase();
                    enabled.into_iter().find(|&i| {
                        self.level(depth)[i]
                            .label
                            .to_lowercase()
                            .starts_with(&query)
                    })
                }
                _ => return,
            };
            if let Some(index) = next {
                self.highlight(depth, index, cx);
            }
        }
        cx.stop_propagation();
    }
}
impl Focusable for MenuState {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl Render for MenuState {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Liquid glass prototype: in the light theme the ink inside the
        // panel is the dark ink of the theme (`ui::glass::liquid_light`);
        // otherwise the light ink, with the exact values of before (a
        // share of 255 is not the same byte as a rounded fraction).
        let (edge, faint, quiet, hover) = if super::glass::liquid_light(cx) {
            let ink = gpui_kit::rgb(0x121212);
            (ink.opacity(0.2), ink.opacity(0.12), ink.opacity(0.7), ink.opacity(0.12))
        } else {
            (
                gpui_kit::rgba(0xf5f5f733),
                gpui_kit::rgba(0xf5f5f71f),
                gpui_kit::rgba(0xf5f5f7b3),
                gpui_kit::rgba(0xffffff1f),
            )
        };
        // A menu is as tall as its entries, up to the room it has: under
        // its trigger or over it, whichever is more, so it does not cover
        // the trigger. A fixed limit cut the last entry of a long menu off.
        let window_h = f32::from(window.viewport_size().height);
        let window_w = f32::from(window.viewport_size().width);
        let (top, bottom) = (f32::from(self.anchor.top()), f32::from(self.anchor.bottom()));
        let beside_trigger = (window_h - bottom).max(top) - 24.;
        // A phone: the menu is a sheet at the bottom of the window, the
        // width of it, with rows a finger can hit; a submenu takes the
        // sheet over, with a row at the top to go back.
        let compact = crate::views::shell::compact(window);
        let sheet_bottom = window_h - crate::views::shell::bottom_inset() - 12.;
        // A phone in landscape is wide: the menu stays a popup there, with
        // rows a finger can hit.
        let touch = compact || window_h.min(window_w) < crate::views::shell::PHONE_SHORT_SIDE;
        let mut root = div()
            .id(("menu", cx.entity_id()))
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::key));
        if !self.open {
            return root;
        }
        let t = UiTheme::read(cx).clone();
        self.row_bounds.resize_with(self.path.len() + 1, Vec::new);
        self.popup_bounds
            .resize(self.path.len() + 1, Default::default());
        for depth in 0..=self.path.len() {
            if compact && depth != self.path.len() {
                continue;
            }
            let items = self.level(depth).to_vec();
            self.row_bounds[depth].resize(items.len(), Default::default());
            let entity = cx.entity();
            let popup_id = if depth == 0 {
                ElementId::from(("menu-popup", cx.entity_id()))
            } else {
                (
                    self.level(depth - 1)[self.path[depth - 1]].id.clone(),
                    "popup",
                )
                    .into()
            };
            // The entries scroll inside the padding of the card, so a long
            // list ends at the same distance from the edge on every side.
            let mut list = div()
                .id((popup_id.clone(), "entries"))
                .max_h(gpui_kit::px(
                    if compact {
                        // The sheet's header takes the rest; a longer list
                        // scrolls inside.
                        window_h * 0.72
                    } else if depth == 0 && !self.pointer {
                        beside_trigger.max(200.)
                    } else {
                        (window_h - 48.).max(200.)
                    } - 24.,
                ))
                .overflow_y_scroll();
            if compact && depth > 0 {
                let parent = self.level(depth - 1)[self.path[depth - 1]].label.clone();
                list = list.child(
                    Button::new(("menu-sheet-back", depth))
                        .focusable(false)
                        .w_full()
                        .min_h(gpui_kit::px(44.))
                        .justify_start()
                        .gap(gpui_kit::px(8.))
                        .px(gpui_kit::px(8.))
                        .rounded(gpui_kit::px(12.))
                        .text_color(quiet)
                        .child(
                            lucide(LucideIcon::ChevronLeft)
                                .size(t.space(4.))
                                .text_color(quiet),
                        )
                        .child(parent)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.path.pop();
                            this.highlighted.pop();
                            cx.notify();
                        })),
                );
            }
            let popup = div()
                .id(popup_id)
                .relative()
                .role(Role::Menu)
                // A menu under a trigger is at least as wide as the trigger.
                .min_w(if compact {
                    gpui_kit::px(window_w - 24.)
                } else if depth == 0 && !self.pointer {
                    self.anchor.size.width.max(t.space(40.))
                } else {
                    t.space(40.)
                })
                // The look of the popups of the app: the frosted glass of
                // the web theme (what is behind shows through, blurred), on
                // a large rounded card with a light edge, and rows with room
                // around them.
                // The same room on every side as the SyncPlay panel: 12 px
                // to a row, and 12 px more to its text.
                .p(gpui_kit::px(12.))
                .rounded(gpui_kit::px(20.))
                .border_1()
                .border_color(edge)
                .child(super::glass::glass(gpui_kit::px(20.), super::glass::POPUP_TINT))
                .text_color(t.colors.popover_foreground)
                .font_family(t.fonts.body.clone())
                .text_size(gpui_kit::px(15.))
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .shadow(t.shadows.md.clone())
                .on_prepaint(move |bounds, _, cx| {
                    entity.update(cx, |this, _| this.popup_bounds[depth] = bounds)
                })
                .on_mouse_down_out(cx.listener(
                    |this, event: &gpui_kit::MouseDownEvent, window, cx| {
                        if !this.anchor.contains(&event.position)
                            && !this
                                .popup_bounds
                                .iter()
                                .any(|b| b.contains(&event.position))
                        {
                            this.close(false, window, cx);
                        }
                    },
                ));
            // Rows line up when some entries of the menu have an icon.
            let icons = items.iter().any(|item| item.icon.is_some());
            for (index, item) in items.into_iter().enumerate() {
                if item.separator {
                    list = list.child(
                        div()
                            .h(gpui_kit::px(1.))
                            .mx(gpui_kit::px(12.))
                            .my(gpui_kit::px(8.))
                            .bg(faint),
                    );
                    continue;
                }
                if item.heading {
                    list = list.child(
                        div()
                            .px(gpui_kit::px(12.))
                            .pt(gpui_kit::px(4.))
                            .pb(gpui_kit::px(6.))
                            .text_size(gpui_kit::px(12.))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(quiet)
                            .child(item.label),
                    );
                    continue;
                }
                let entity = cx.entity();
                let selected = self.highlighted[depth] == Some(index);
                let submenu = !item.children.is_empty();
                let role = if submenu {
                    Role::MenuItem
                } else if item.radio {
                    Role::MenuItemRadio
                } else if item.checked.is_some() {
                    Role::MenuItemCheckBox
                } else {
                    Role::MenuItem
                };
                let row = Button::new(item.id.clone())
                    .role(role)
                    .focusable(false)
                    .disabled(item.disabled)
                    .accessibility_label(item.label.clone())
                    .relative()
                    .w_full()
                    .min_h(gpui_kit::px(match (touch, item.detail.is_some()) {
                        (true, true) => 58.,
                        (true, false) => 44.,
                        (false, true) => 52.,
                        (false, false) => 38.,
                    }))
                    .justify_start()
                    .gap(gpui_kit::px(12.))
                    .px(gpui_kit::px(12.))
                    .rounded(gpui_kit::px(12.))
                    .when(selected, |b| {
                        b.bg(hover).aria_active_descendant()
                    })
                    .when(item.disabled, |b| b.opacity(0.5))
                    .when_some(item.checked, |b, checked| {
                        b.aria_toggled(if checked {
                            gpui_kit::Toggled::True
                        } else {
                            gpui_kit::Toggled::False
                        })
                    })
                    .when(submenu, |b| {
                        b.aria_expanded(self.path.get(depth) == Some(&index))
                    })
                    .on_prepaint(move |bounds, window, cx| {
                        entity.update(cx, |this, _| {
                            if this.row_bounds[depth][index] != bounds {
                                this.row_bounds[depth][index] = bounds;
                                window.request_animation_frame();
                            }
                        })
                    })
                    .on_hover(cx.listener(move |this, hovered, _, cx| {
                        if *hovered {
                            this.highlight(depth, index, cx);
                        }
                    }))
                    .on_click(cx.listener(move |this, event, window, cx| {
                        this.activate(depth, index, event, window, cx)
                    }))
                    .when(icons, |b| {
                        b.child(div().w(gpui_kit::px(18.)).flex_shrink_0().children(
                            item.icon.map(|icon| {
                                lucide(icon)
                                    .size(gpui_kit::px(18.))
                                    .text_color(t.colors.popover_foreground)
                            }),
                        ))
                    })
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .items_start()
                            .gap(gpui_kit::px(2.))
                            .line_height(gpui_kit::px(19.))
                            .child(item.label)
                            .children(item.detail.map(|detail| {
                                div()
                                    .line_height(gpui_kit::px(15.))
                                    .text_size(gpui_kit::px(12.))
                                    .font_weight(gpui_kit::FontWeight::NORMAL)
                                    .text_color(quiet)
                                    .child(detail)
                            })),
                    )
                    .when(item.checked == Some(true), |b| {
                        b.child(
                            lucide(LucideIcon::Check)
                                .size(t.space(4.))
                                .text_color(t.colors.popover_foreground),
                        )
                    })
                    .when(submenu, |b| {
                        b.child(
                            lucide(LucideIcon::ChevronRight)
                                .size(t.space(4.))
                                .text_color(t.colors.popover_foreground),
                        )
                    });
                list = list.child(row);
            }
            // The sheet of a phone rises from the bottom; a popup settles
            // into place. The element leaves the tree with the menu, so
            // the entrance runs at each open.
            let popup = enter(
                popup.child(list),
                SharedString::from(format!("menu.enter.{depth}")),
                if compact { Enter::Rise } else { Enter::Settle },
                cx,
            );
            let anchor = if compact {
                // A line along the bottom of the window: the sheet sits on it.
                Bounds::new(
                    gpui_kit::point(gpui_kit::px(12.), gpui_kit::px(sheet_bottom)),
                    gpui_kit::size(gpui_kit::px(window_w - 24.), gpui_kit::px(0.)),
                )
            } else if depth == 0 {
                self.anchor
            } else {
                self.row_bounds[depth - 1][self.path[depth - 1]]
            };
            root = root.child(
                gpui_kit::deferred(
                    Positioner::side(anchor)
                        .align(Align::Start)
                        // A sheet opens up from the bottom; so does a popup
                        // from a button of the player's bar on a phone in
                        // landscape (it lay over the bar, 2026-10-23).
                        .placement(if compact || (touch && depth == 0 && !self.pointer) {
                            Placement::Top
                        } else if depth > 0 || self.pointer {
                            Placement::Right
                        } else {
                            Placement::Bottom
                        })
                        .offset(gpui_kit::px(if compact {
                            0.
                        } else if touch && depth == 0 && !self.pointer {
                            // Above the bar the trigger sits in, not the
                            // trigger alone.
                            self.touch_bottom
                                .map(|y| (f32::from(self.anchor.top()) - y).max(8.))
                                .unwrap_or(8.)
                        } else {
                            8.
                        }))
                        .occlude()
                        .child(popup),
                )
                .with_priority(gpui_kit::base::POPUP_PRIORITY + depth),
            );
        }
        root
    }
}

#[derive(IntoElement)]
/// A named menu trigger backed by retained menu state. The trigger has no
/// look of its own: its content is the whole button.
pub struct Menu {
    state: Entity<MenuState>,
    label: SharedString,
    trigger: Option<AnyElement>,
    style: StyleRefinement,
}
impl Menu {
    /// Creates a named trigger for the caller's retained menu state.
    pub fn new(state: &Entity<MenuState>, label: impl Into<SharedString>) -> Self {
        Self {
            state: state.clone(),
            label: label.into(),
            trigger: None,
            style: Default::default(),
        }
    }
    /// Supplies custom content for the named trigger button.
    pub fn trigger(mut self, child: impl IntoElement) -> Self {
        self.trigger = Some(child.into_any_element());
        self
    }
}
impl Styled for Menu {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}
impl RenderOnce for Menu {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = self.state.clone();
        let keyboard = state.clone();
        let measured = state.clone();
        let focus = state.read(cx).trigger.clone();
        let open = state.read(cx).open;
        let base = div()
            .id(("menu-trigger", state.entity_id()))
            .relative()
            .on_prepaint(move |bounds, _, cx| {
                measured.update(cx, |this, _| {
                    if !this.pointer || !this.open {
                        this.anchor = bounds;
                    }
                })
            })
            .capture_key_down(move |event, window, cx| {
                let key = event.keystroke.key.as_str();
                if keyboard.read(cx).trigger.is_focused(window) && (key == "down" || key == "up") {
                    keyboard.update(cx, |this, cx| this.open(window, cx));
                    cx.stop_propagation();
                }
            });
        let base = base.child(
            Button::new(("menu-button", state.entity_id()))
                .track_focus(&focus)
                .tab_stop(focus.tab_stop)
                .accessibility_label(self.label.clone())
                .aria_expanded(open)
                .on_click(move |_, window, cx| {
                    state.update(cx, |this, cx| {
                        if this.open {
                            this.close(true, window, cx);
                        } else {
                            this.open(window, cx);
                        }
                    })
                })
                .child(
                    self.trigger
                        .unwrap_or_else(|| self.label.into_any_element()),
                ),
        );
        apply_style(base.child(self.state), &self.style)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::AppContext as _;

    #[gpui_kit::test]
    fn check_actions_toggle_and_radio_actions_select_one(cx: &mut gpui_kit::TestAppContext) {
        cx.update(super::super::theme::init);
        let window = cx.add_window(|_, cx| {
            MenuState::new(
                [
                    MenuItem::new("notifications", "Notifications").checked(true),
                    MenuItem::new("compact", "Compact").radio(true),
                    MenuItem::new("comfortable", "Comfortable").radio(false),
                    MenuItem::new("locked", "Locked")
                        .checked(true)
                        .disabled(true),
                ],
                cx,
            )
        });
        window
            .update(cx, |menu, window, cx| {
                menu.activate(0, 0, &ClickEvent::default(), window, cx);
                assert_eq!(menu.items[0].checked, Some(false));
                menu.activate(0, 2, &ClickEvent::default(), window, cx);
                assert_eq!(menu.items[1].checked, Some(false));
                assert_eq!(menu.items[2].checked, Some(true));
                menu.activate(0, 3, &ClickEvent::default(), window, cx);
                assert_eq!(menu.items[3].checked, Some(true));
                // Two groups at one level: a choice in the second leaves
                // the mark of the first.
                menu.set_items(
                    [
                        MenuItem::new("name", "Name").radio(true),
                        MenuItem::new("date", "Date").radio(false),
                        MenuItem::separator(),
                        MenuItem::new("up", "Ascending").radio(true),
                        MenuItem::new("down", "Descending").radio(false),
                    ],
                    cx,
                );
                menu.activate(0, 4, &ClickEvent::default(), window, cx);
                let marks: Vec<Option<bool>> = menu.items.iter().map(|item| item.checked).collect();
                assert_eq!(marks, [Some(true), Some(false), None, Some(false), Some(true)]);
            })
            .unwrap();
    }

    #[gpui_kit::test]
    fn handlers_can_update_menu_after_activation(cx: &mut gpui_kit::TestAppContext) {
        cx.update(super::super::theme::init);
        let window = cx.add_window(|_, cx| MenuState::new([], cx));
        window
            .update(cx, |menu, window, cx| {
                let menu_handle = cx.weak_entity();
                menu.set_items(
                    [MenuItem::new("reset", "Reset").on_click(move |_, _, cx| {
                        menu_handle
                            .update(cx, |menu, cx| menu.set_items([], cx))
                            .ok();
                    })],
                    cx,
                );
                menu.activate(0, 0, &ClickEvent::default(), window, cx);
            })
            .unwrap();

        cx.run_until_parked();

        assert!(
            cx.read_window(&window, |menu, cx| menu.read(cx).items.is_empty())
                .unwrap()
        );
    }
}
