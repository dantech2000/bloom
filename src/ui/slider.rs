//! Single-value slider using Kit state and pointer primitives.
use super::theme::{UiTheme, apply_style};
pub use gpui_kit::base::slider::{SliderEvent, SliderState};
use gpui_kit::base::{SliderIndicator, SliderThumb, SliderTrack};
use gpui_kit::{
    AccessibleAction, App, Axis, Bounds, DispatchPhase, ElementId, Entity, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Pixels, RenderOnce, Role, SharedString,
    StatefulInteractiveElement as _, StyleRefinement, Styled, TouchDragEvent, TouchPhase, Window,
    canvas, div, prelude::FluentBuilder as _, relative,
};

#[derive(IntoElement)]
/// A Nova presentation of retained Kit slider state.
pub struct Slider {
    state: Entity<SliderState>,
    label: Option<SharedString>,
    style: StyleRefinement,
}
impl Slider {
    /// Creates a Nova slider using the caller's retained Kit state.
    pub fn new(state: &Entity<SliderState>) -> Self {
        Self {
            state: state.clone(),
            label: None,
            style: Default::default(),
        }
    }
    /// Sets the accessible name of the control.
    pub fn aria_label(mut self, value: impl Into<SharedString>) -> Self {
        self.label = Some(value.into());
        self
    }
}
impl Styled for Slider {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}
impl RenderOnce for Slider {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = self.state.read(cx);
        let (min, max, step, value) = (
            state.min_value(),
            state.max_value(),
            state.step_value(),
            state.value().end(),
        );
        // A range that is none (no length yet, a value that is no number)
        // shows as a slider that takes no input. An assert here ended the
        // app from inside a frame.
        let valid = state.value().is_single()
            && min.is_finite()
            && max.is_finite()
            && min < max
            && step.is_finite()
            && step > 0.
            && value.is_finite();
        let disabled = !valid;
        let (min, max, step, value) = if valid { (min, max, step, value) } else { (0., 1., 1., 0.) };
        let percent = if valid { state.percentage().end } else { 0. };
        let focus = window
            .use_keyed_state(
                ElementId::from(("slider-focus", self.state.entity_id())),
                cx,
                |_, cx| cx.focus_handle(),
            )
            .read(cx)
            .clone()
            .tab_stop(!disabled);
        let focused = focus.is_focused(window);
        // A finger on a touch screen: the drag is claimed at its start, or
        // GPUI takes it for a scroll and the thumb stays. The flag lives
        // across frames, since the listener is registered at each paint.
        let claimed = window.use_keyed_state(
            ElementId::from(("slider-touch", self.state.entity_id())),
            cx,
            |_, _| false,
        );
        let theme = UiTheme::read(cx);
        let colors = theme.colors;
        let click_focus = focus.clone();
        let thumb_focus = focus.clone();
        let input_state = self.state.clone();
        let increment = self.state.clone();
        let decrement = self.state.clone();
        let set_value = self.state.clone();
        let release = self.state.clone();
        let release_out = self.state.clone();
        let touch_state = self.state.clone();
        let touch = canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, _, window, _| {
                window.on_mouse_event(move |event: &TouchDragEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    match event.phase {
                        TouchPhase::Started => {
                            if !bounds.contains(&event.start_position) {
                                return;
                            }
                            claimed.update(cx, |on, _| *on = true);
                            window.prevent_default();
                            set_from_x(&touch_state, event.position.x, false, window, cx);
                        }
                        TouchPhase::Moved => {
                            if *claimed.read(cx) {
                                set_from_x(&touch_state, event.position.x, false, window, cx);
                            }
                        }
                        TouchPhase::Ended | TouchPhase::Cancelled => {
                            if *claimed.read(cx) {
                                claimed.update(cx, |on, _| *on = false);
                                set_from_x(&touch_state, event.position.x, true, window, cx);
                            }
                        }
                    }
                });
            },
        )
        .absolute()
        .inset_0();
        let thumb = SliderThumb::new(&self.state)
            .disabled(disabled)
            .absolute()
            .left(relative(percent))
            .ml(-theme.space(1.5))
            .top(theme.space(1.))
            .size(theme.space(3.))
            .rounded_full()
            .bg(colors.primary)
            .when(focused && !disabled, |t| {
                t.border_1().border_color(colors.ring)
            })
            .when(!disabled, |t| {
                t.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    thumb_focus.focus(window, cx)
                })
            });
        let root = div()
            .id(("slider", self.state.entity_id()))
            .role(Role::Slider)
            .aria_numeric_value(value as f64)
            .aria_min_numeric_value(min as f64)
            .aria_max_numeric_value(max as f64)
            .aria_numeric_value_step(step as f64)
            .aria_orientation(gpui_kit::Orientation::Horizontal)
            .when_some(self.label, |root, label| root.aria_label(label))
            .relative()
            .w_full()
            .h(theme.space(5.))
            .flex()
            .items_center()
            .when(disabled, |root| root.opacity(0.5).cursor_not_allowed())
            .when(!disabled, |root| {
                root.track_focus(&focus)
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        click_focus.focus(window, cx)
                    })
                    .on_key_down(move |event, window, cx| {
                        if event.keystroke.modifiers.modified() {
                            return;
                        }
                        let state = input_state.read(cx);
                        let value = match event.keystroke.key.as_str() {
                            "right" | "up" => state.value().end() + state.step_value(),
                            "left" | "down" => state.value().end() - state.step_value(),
                            "pageup" => state.value().end() + state.step_value() * 10.,
                            "pagedown" => state.value().end() - state.step_value() * 10.,
                            "home" => state.min_value(),
                            "end" => state.max_value(),
                            _ => return,
                        };
                        update_value(&input_state, value, window, cx);
                        cx.stop_propagation();
                    })
                    .on_a11y_action(AccessibleAction::Increment, move |_, window, cx| {
                        let state = increment.read(cx);
                        let value = state.value().end() + state.step_value();
                        update_value(&increment, value, window, cx);
                    })
                    .on_a11y_action(AccessibleAction::Decrement, move |_, window, cx| {
                        let state = decrement.read(cx);
                        let value = state.value().end() - state.step_value();
                        update_value(&decrement, value, window, cx);
                    })
                    .on_a11y_action(AccessibleAction::SetValue, move |data, window, cx| {
                        if let Some(gpui_kit::accesskit::ActionData::NumericValue(value)) = data {
                            update_value(&set_value, *value as f32, window, cx);
                        }
                    })
                    .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                        release.update(cx, |state, cx| state.handle_release(cx))
                    })
                    .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                        release_out.update(cx, |state, cx| state.handle_release(cx))
                    })
            })
            .child(
                // The track takes clicks over the full height of the slider;
                // the line it shows is thin. A click a few pixels off the line
                // would miss a track of the line's own height.
                SliderTrack::new(&self.state)
                    .disabled(disabled)
                    .relative()
                    .w_full()
                    .h(theme.space(5.))
                    .flex()
                    .items_center()
                    .child(
                        SliderIndicator::new(&self.state)
                            .relative()
                            .w_full()
                            .h(theme.space(1.))
                            .rounded_full()
                            .bg(colors.foreground.opacity(0.25))
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(percent))
                                    .rounded_full()
                                    .bg(colors.primary),
                            ),
                    )
                    .child(thumb),
            )
            .when(!disabled, |root| root.child(touch));
        apply_style(root, &self.style)
    }
}

/// Sets the value from a position along the track, as the track does for
/// the mouse (`update_value_by_position` marks the drag, so the release
/// emits its event and the player seeks).
fn set_from_x(state: &Entity<SliderState>, x: Pixels, release: bool, window: &mut Window, cx: &mut App) {
    state.update(cx, |state, cx| {
        let y = state.bounds().center().y;
        state.update_value_by_position(Axis::Horizontal, gpui_kit::point(x, y), false, window, cx);
        if release {
            state.handle_release(cx);
        }
    });
}

/// Sets a slider from 0 to `max` in whole steps at `value`, with no window
/// and no event: for a slider that follows something (the seek slider and
/// the position). The state is changed in place: a fresh state has no
/// bounds, and a click before the next paint would divide by zero.
pub fn set_range_and_value(state: &mut SliderState, max: f32, value: f32) {
    *state = std::mem::replace(state, SliderState::new())
        .min(0.)
        .max(max)
        .step(1.)
        .default_value(value.clamp(0., max));
}

fn update_value(state: &Entity<SliderState>, value: f32, window: &mut Window, cx: &mut App) {
    if !value.is_finite() {
        return;
    }
    state.update(cx, |state, cx| {
        let min = state.min_value();
        let value = (min + ((value - min) / state.step_value()).round() * state.step_value())
            .clamp(min, state.max_value());
        state.set_value(value, window, cx);
        state.handle_release(cx);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{AppContext as _, Context, Render, TestAppContext, VisualTestContext};

    struct View {
        state: Entity<SliderState>,
    }
    impl Render for View {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(gpui_kit::px(200.)).child(Slider::new(&self.state))
        }
    }

    /// A slider with no range (the seek slider of an item whose length
    /// is not known yet) draws; its assert ended the app.
    #[gpui_kit::test]
    fn a_slider_with_no_range_draws(cx: &mut TestAppContext) {
        cx.update(super::super::theme::init);
        let window = cx.add_window(|_, cx| View { state: cx.new(|_| SliderState::new().min(0.).max(0.)) });
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| window.draw(cx).clear(cx));
    }
}
