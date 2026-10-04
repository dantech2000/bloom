// SPDX-License-Identifier: AGPL-3.0-or-later
//! A column of rows of different heights that builds only the rows in view.
//! [`super::rows`] does this for rows of one height. A form row has a
//! description that wraps, so its height is not known before layout.
//!
//! The first frame of a list builds every row and measures it. The heights
//! go to a cache. Each frame after that takes the total height from the
//! cache and builds and lays out only the rows inside the scroll viewport.
//! A visible row that is taller or shorter than the cache says corrects the
//! cache and asks for one more frame. A change of width drops the cache.

use std::{cell::RefCell, collections::HashMap, ops::Range, rc::Rc};

use gpui_kit::{
    AnyElement, App, AvailableSpace, Bounds, Context, Display, Element, ElementId, FlexDirection,
    GlobalElementId, InspectorElementId, IntoElement, LayoutId, ParentElement as _, Pixels, Refineable as _,
    Style, StyleRefinement, Styled, WeakEntity, Window, div, point, px, size,
};

use crate::app::Bloom;

/// Rows this far outside the viewport are built too, so a scroll step does
/// not show an empty band before the next frame.
const MARGIN: f32 = 200.;

type Build = Rc<dyn Fn(&Bloom, Range<usize>, &mut Context<Bloom>) -> Vec<AnyElement>>;

/// What the first frame measured.
struct Measured {
    width: f32,
    heights: Vec<f32>,
}

thread_local! {
    /// By list id. The UI thread is the only one that draws.
    static CACHE: RefCell<HashMap<String, Measured>> = RefCell::new(HashMap::new());
}

/// Forgets the heights of the lists whose id starts with `prefix`.
pub fn forget(prefix: &str) {
    CACHE.with(|cache| cache.borrow_mut().retain(|id, _| !id.starts_with(prefix)));
}

pub struct Lazy {
    app: WeakEntity<Bloom>,
    id: String,
    count: usize,
    build: Build,
    style: StyleRefinement,
}

/// `count` rows. `id` names the list, and must change when the rows or
/// their order change. `build` makes the rows of a range; it runs while the
/// frame is drawn, so it reads the data from the app again.
pub fn lazy(
    cx: &Context<Bloom>,
    id: impl Into<String>,
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
    /// The cache has the heights; rows are built in the prepaint pass.
    Known,
    /// Every row is built and laid out once, to learn its height.
    Measuring(Vec<AnyElement>, Vec<LayoutId>),
}

pub enum Prepaint {
    Windowed(Vec<AnyElement>),
    Measured(Vec<AnyElement>),
}

impl Element for Lazy {
    type RequestLayoutState = Layout;
    type PrepaintState = Prepaint;

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
    ) -> (LayoutId, Layout) {
        let total = CACHE.with(|cache| {
            cache
                .borrow()
                .get(&self.id)
                .filter(|m| m.heights.len() == self.count)
                .map(|m| m.heights.iter().sum::<f32>())
        });
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
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Layout,
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let width = f32::from(bounds.size.width);
        match layout {
            Layout::Measuring(items, ids) => {
                let heights: Vec<f32> = ids
                    .iter()
                    .map(|id| f32::from(window.layout_bounds(*id).size.height))
                    .collect();
                CACHE.with(|cache| {
                    cache.borrow_mut().insert(self.id.clone(), Measured { width, heights })
                });
                for item in items.iter_mut() {
                    item.prepaint(window, cx);
                }
                // One more frame, which builds only the rows in view.
                window.refresh();
                Prepaint::Measured(std::mem::take(items))
            }
            Layout::Known => {
                let Some(app) = self.app.upgrade() else {
                    return Prepaint::Windowed(Vec::new());
                };
                let (cached_width, heights) = CACHE.with(|cache| {
                    cache
                        .borrow()
                        .get(&self.id)
                        .map(|m| (m.width, m.heights.clone()))
                        .unwrap_or_default()
                });
                if heights.len() != self.count {
                    return Prepaint::Windowed(Vec::new());
                }
                if (cached_width - width).abs() > 0.5 {
                    // The rows wrap in another place now; measure again.
                    CACHE.with(|cache| cache.borrow_mut().remove(&self.id));
                    window.refresh();
                    return Prepaint::Windowed(Vec::new());
                }
                // The rows that lie inside the scroll viewport, plus the margin.
                let visible = window.content_mask().bounds;
                let top = f32::from(visible.origin.y - bounds.origin.y) - MARGIN;
                let bottom = f32::from(visible.bottom_right().y - bounds.origin.y) + MARGIN;
                let mut offsets = Vec::with_capacity(heights.len());
                let mut y = 0.;
                for h in &heights {
                    offsets.push(y);
                    y += h;
                }
                let first = offsets.iter().rposition(|y| *y <= top).unwrap_or(0);
                let last = offsets.iter().position(|y| *y >= bottom).unwrap_or(self.count);
                if first >= last {
                    return Prepaint::Windowed(Vec::new());
                }
                let built = app.update(cx, |this, cx| (self.build)(this, first..last, cx));
                let space = size(
                    AvailableSpace::Definite(bounds.size.width),
                    AvailableSpace::MinContent,
                );
                let mut items = Vec::with_capacity(built.len());
                let mut corrected = false;
                for (i, row) in built.into_iter().enumerate() {
                    let mut item = div().w(bounds.size.width).child(row).into_any_element();
                    let height = f32::from(item.layout_as_root(space, window, cx).height);
                    if (height - heights[first + i]).abs() > 0.5 {
                        CACHE.with(|cache| {
                            if let Some(m) = cache.borrow_mut().get_mut(&self.id) {
                                m.heights[first + i] = height;
                            }
                        });
                        corrected = true;
                    }
                    item.prepaint_at(
                        point(bounds.origin.x, bounds.origin.y + px(offsets[first + i])),
                        window,
                        cx,
                    );
                    items.push(item);
                }
                if corrected {
                    window.refresh();
                }
                Prepaint::Windowed(items)
            }
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut Layout,
        prepaint: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (Prepaint::Windowed(items) | Prepaint::Measured(items)) = prepaint;
        for item in items {
            item.paint(window, cx);
        }
    }
}
