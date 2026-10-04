// SPDX-License-Identifier: AGPL-3.0-or-later
//! A column of rows of one height that builds only the rows in view. The
//! long lists of the dashboard (log lines, devices, the activity timeline)
//! laid out every row on every frame, and Taffy's layout was the whole cost
//! of a scroll frame. This element takes the height of all its rows in the
//! layout pass, and in the prepaint pass, when it knows where it lies in the
//! window, builds and lays out the rows inside the scroll viewport only.

use std::ops::Range;

use gpui_kit::{
    AnyElement, App, AvailableSpace, Bounds, Context, Element, ElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, ParentElement as _, Pixels, Refineable as _,
    Style, StyleRefinement, Styled, WeakEntity, Window, div, point, px, size,
};

use crate::app::Bloom;

/// Rows this far outside the viewport are built too, so a scroll step does
/// not show an empty band before the next frame.
const MARGIN: f32 = 160.;

type Build = Box<dyn Fn(&Bloom, Range<usize>, &mut Context<Bloom>) -> Vec<AnyElement>>;

pub struct Rows {
    app: WeakEntity<Bloom>,
    count: usize,
    row_h: f32,
    build: Build,
    style: StyleRefinement,
}

/// `count` rows of `row_h` points each. `build` makes the rows of a range;
/// it runs in the prepaint pass, so it reads the data from the app again
/// rather than from the render that made the element.
pub fn rows(
    cx: &Context<Bloom>,
    count: usize,
    row_h: f32,
    build: impl Fn(&Bloom, Range<usize>, &mut Context<Bloom>) -> Vec<AnyElement> + 'static,
) -> Rows {
    Rows {
        app: cx.weak_entity(),
        count,
        row_h,
        build: Box::new(build),
        style: StyleRefinement::default(),
    }
}

impl Styled for Rows {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl IntoElement for Rows {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Rows {
    type RequestLayoutState = ();
    type PrepaintState = Vec<AnyElement>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = gpui_kit::relative(1.).into();
        style.size.height = px(self.row_h * self.count as f32).into();
        style.flex_shrink = 0.;
        style.refine(&self.style);
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<AnyElement> {
        if self.count == 0 || self.row_h <= 0. {
            return Vec::new();
        }
        // The rows that lie inside the scroll viewport, plus the margin.
        let visible = window.content_mask().bounds;
        let top = f32::from(visible.origin.y - bounds.origin.y) - MARGIN;
        let bottom = f32::from(visible.bottom_right().y - bounds.origin.y) + MARGIN;
        let first = (top / self.row_h).floor().max(0.) as usize;
        let last = ((bottom / self.row_h).ceil().max(0.) as usize).min(self.count);
        if first >= last {
            return Vec::new();
        }
        let Some(app) = self.app.upgrade() else {
            return Vec::new();
        };
        let built = app.update(cx, |this, cx| (self.build)(this, first..last, cx));
        let width = bounds.size.width;
        let row_h = px(self.row_h);
        let space = size(AvailableSpace::Definite(width), AvailableSpace::Definite(row_h));
        let mut items = Vec::with_capacity(built.len());
        for (i, row) in built.into_iter().enumerate() {
            // A row that wants more than its height is cut, not laid over
            // the next one.
            let mut item = div()
                .w(width)
                .h(row_h)
                .overflow_hidden()
                .child(row)
                .into_any_element();
            item.layout_as_root(space, window, cx);
            let origin = point(
                bounds.origin.x,
                bounds.origin.y + row_h * (first + i) as f32,
            );
            item.prepaint_at(origin, window, cx);
            items.push(item);
        }
        items
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut (),
        items: &mut Vec<AnyElement>,
        window: &mut Window,
        cx: &mut App,
    ) {
        for item in items {
            item.paint(window, cx);
        }
    }
}
