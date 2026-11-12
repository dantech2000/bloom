// SPDX-License-Identifier: AGPL-3.0-or-later
//! A column of rows of different heights that builds only the rows in view.
//! [`super::rows`] does this for rows of one height. A form row has a
//! description that wraps, so its height is not known before layout.
//!
//! The first frame of a list builds every row and measures it. The heights
//! stay with the element (GPUI keeps the state of an element by its id
//! while it is drawn). Each frame after that takes the total height from
//! them and builds and lays out only the rows inside the scroll viewport.
//! A visible row that is taller or shorter than measured corrects the
//! heights and asks for one more frame. A change of width measures again,
//! and so does a list that was out of the window for a frame.

use std::{ops::Range, rc::Rc};

use gpui_kit::{
    AnyElement, App, AvailableSpace, Bounds, Context, Display, Element, ElementId, FlexDirection,
    GlobalElementId, InspectorElementId, IntoElement, LayoutId, ParentElement as _, Pixels, Refineable as _,
    SharedString, Style, StyleRefinement, Styled, WeakEntity, Window, div, point, px, size,
};

use crate::app::Bloom;

/// Rows this far outside the viewport are built too, so a scroll step does
/// not show an empty band before the next frame.
const MARGIN: f32 = 200.;

type Build = Rc<dyn Fn(&Bloom, Range<usize>, &mut Context<Bloom>) -> Vec<AnyElement>>;

/// What the first frame measured.
struct Measured {
    width: f32,
    /// Where each row starts, and the end of the last one: one more entry
    /// than there are rows.
    offsets: Vec<f32>,
}

impl Measured {
    fn of(width: f32, heights: impl Iterator<Item = f32>) -> Self {
        let mut offsets = vec![0.];
        let mut y = 0.;
        for height in heights {
            y += height;
            offsets.push(y);
        }
        Self { width, offsets }
    }

    fn rows(&self) -> usize {
        self.offsets.len() - 1
    }

    fn total(&self) -> f32 {
        self.offsets[self.rows()]
    }

    fn height(&self, row: usize) -> f32 {
        self.offsets[row + 1] - self.offsets[row]
    }

    /// Gives a row another height; the rows after it move.
    fn set_height(&mut self, row: usize, height: f32) {
        let change = height - self.height(row);
        for offset in &mut self.offsets[row + 1..] {
            *offset += change;
        }
    }

    /// The rows that touch the band from `top` to `bottom`.
    fn span(&self, top: f32, bottom: f32) -> Range<usize> {
        let starts = &self.offsets[..self.rows()];
        let first = starts.partition_point(|y| *y <= top).saturating_sub(1);
        let last = starts.partition_point(|y| *y < bottom);
        first..last
    }
}

pub struct Lazy {
    app: WeakEntity<Bloom>,
    id: SharedString,
    count: usize,
    build: Build,
    style: StyleRefinement,
}

/// `count` rows. `id` names the list, and must change when the rows or
/// their order change. `build` makes the rows of a range; it runs while the
/// frame is drawn, so it reads the data from the app again.
pub fn lazy(
    cx: &Context<Bloom>,
    id: impl Into<SharedString>,
    count: usize,
    build: impl Fn(&Bloom, Range<usize>, &mut Context<Bloom>) -> Vec<AnyElement> + 'static,
) -> Lazy {
    Lazy {
        app: cx.weak_entity(),
        id: id.into(),
        count,
        build: Rc::new(build),
        style: StyleRefinement::default(),
    }
}

impl Lazy {
    /// Takes the heights out of the element's state, when they are of this
    /// many rows. `keep` puts them back.
    fn take(&self, id: &GlobalElementId, window: &mut Window) -> Option<Measured> {
        window
            .with_element_state::<Option<Measured>, _>(id, |state, _| (state.flatten(), None))
            .filter(|measured| measured.rows() == self.count)
    }

    fn keep(id: &GlobalElementId, measured: Option<Measured>, window: &mut Window) {
        window.with_element_state::<Option<Measured>, _>(id, |_, _| ((), measured));
    }

    /// Builds, lays out and places the rows in view. None when the width
    /// changed: the rows wrap in another place now, and the list measures
    /// again.
    fn windowed(
        &self,
        measured: &mut Measured,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Vec<AnyElement>> {
        if (measured.width - f32::from(bounds.size.width)).abs() > 0.5 {
            return None;
        }
        let Some(app) = self.app.upgrade() else {
            return Some(Vec::new());
        };
        // The rows that lie inside the scroll viewport, plus the margin.
        let visible = window.content_mask().bounds;
        let top = f32::from(visible.origin.y - bounds.origin.y) - MARGIN;
        let bottom = f32::from(visible.bottom_right().y - bounds.origin.y) + MARGIN;
        let span = measured.span(top, bottom);
        if span.is_empty() {
            return Some(Vec::new());
        }
        let first = span.start;
        let built = app.update(cx, |this, cx| (self.build)(this, span, cx));
        let space = size(AvailableSpace::Definite(bounds.size.width), AvailableSpace::MinContent);
        let mut items = Vec::with_capacity(built.len());
        let mut corrected = false;
        for (row, built) in (first..measured.rows()).zip(built) {
            let mut item = div().w(bounds.size.width).child(built).into_any_element();
            let height = f32::from(item.layout_as_root(space, window, cx).height);
            item.prepaint_at(
                point(bounds.origin.x, bounds.origin.y + px(measured.offsets[row])),
                window,
                cx,
            );
            if (height - measured.height(row)).abs() > 0.5 {
                measured.set_height(row, height);
                corrected = true;
            }
            items.push(item);
        }
        if corrected {
            window.refresh();
        }
        Some(items)
    }
}

impl Styled for Lazy {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl IntoElement for Lazy {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

pub enum Layout {
    /// The heights are known; rows are built in the prepaint pass.
    Known,
    /// Every row is built and laid out once, to learn its height.
    Measuring(Vec<AnyElement>, Vec<LayoutId>),
}

impl Element for Lazy {
    type RequestLayoutState = Layout;
    type PrepaintState = Vec<AnyElement>;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone().into())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Layout) {
        let id = id.expect("a lazy list has an id");
        let measured = self.take(id, window);
        let total = measured.as_ref().map(Measured::total);
        Self::keep(id, measured, window);
        let mut style = Style::default();
        style.size.width = gpui_kit::relative(1.).into();
        style.flex_shrink = 0.;
        if let Some(total) = total {
            style.size.height = px(total).into();
            style.refine(&self.style);
            return (window.request_layout(style, [], cx), Layout::Known);
        }
        style.display = Display::Flex;
        style.flex_direction = FlexDirection::Column;
        style.refine(&self.style);
        let built = match self.app.upgrade() {
            Some(app) => app.update(cx, |this, cx| (self.build)(this, 0..self.count, cx)),
            None => Vec::new(),
        };
        let mut items = Vec::with_capacity(built.len());
        let mut ids = Vec::with_capacity(built.len());
        for row in built {
            let mut item = div().w_full().child(row).into_any_element();
            ids.push(item.request_layout(window, cx));
            items.push(item);
        }
        (window.request_layout(style, ids.clone(), cx), Layout::Measuring(items, ids))
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Layout,
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<AnyElement> {
        let id = id.expect("a lazy list has an id");
        match layout {
            Layout::Measuring(items, ids) => {
                let heights = ids.iter().map(|id| f32::from(window.layout_bounds(*id).size.height));
                let measured = Measured::of(f32::from(bounds.size.width), heights);
                // A build that gave fewer rows than asked (no data yet)
                // measures again when the rows are there.
                Self::keep(id, Some(measured), window);
                for item in items.iter_mut() {
                    item.prepaint(window, cx);
                }
                // One more frame, which builds only the rows in view.
                window.refresh();
                std::mem::take(items)
            }
            Layout::Known => {
                let Some(mut measured) = self.take(id, window) else {
                    return Vec::new();
                };
                let items = self.windowed(&mut measured, bounds, window, cx);
                if items.is_none() {
                    window.refresh();
                }
                Self::keep(id, items.is_some().then_some(measured), window);
                items.unwrap_or_default()
            }
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut Layout,
        items: &mut Vec<AnyElement>,
        window: &mut Window,
        cx: &mut App,
    ) {
        for item in items {
            item.paint(window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Measured;

    #[test]
    fn the_span_is_the_rows_that_touch_the_band() {
        let mut measured = Measured::of(300., [10., 20., 30., 40.].into_iter());
        assert_eq!(measured.rows(), 4);
        assert_eq!(measured.total(), 100.);
        // Rows start at 0, 10, 30 and 60.
        assert_eq!(measured.span(0., 100.), 0..4);
        assert_eq!(measured.span(-50., 5.), 0..1);
        assert_eq!(measured.span(12., 30.), 1..2);
        assert_eq!(measured.span(12., 31.), 1..3);
        assert_eq!(measured.span(200., 300.), 3..4);
        assert!(measured.span(-50., -10.).is_empty());
        // A row that grows moves the rows after it.
        measured.set_height(1, 50.);
        assert_eq!(measured.total(), 130.);
        assert_eq!(measured.height(1), 50.);
        assert_eq!(measured.span(55., 61.), 1..3);
    }
}
