//! Same-frame scroll correction when older transcript rows are prepended.
//!
//! gpui positions a scroller's children from the offset during that scroller's
//! prepaint. The content height is already known by then, because layout has
//! finished. This pass-through reads that height and shifts the offset before
//! the scroller prepaints, so the frame never shows the jumped position.
//!
//! The offset grows more negative as the user scrolls down, so keeping the
//! same lines on screen means subtracting the growth at the top.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, ParentElement, Pixels, ScrollHandle, Window, point, px,
};

/// Scroll position captured before a theme change relayouts the transcript.
pub(super) struct AppearanceHold {
    /// The transcript was sitting on its tail, so the next layout stays there.
    pub(super) pinned: bool,
    /// Offset to restore when the transcript was not pinned.
    pub(super) offset_y: Pixels,
}

pub(super) struct ScrollAnchor {
    handle: ScrollHandle,
    previous_offset_y: Pixels,
    previous_height: Pixels,
    active: bool,
    appearance: Option<AppearanceHold>,
    slot: Rc<Cell<Option<LayoutId>>>,
    child: Option<AnyElement>,
}

impl ScrollAnchor {
    pub(super) fn new(
        handle: ScrollHandle,
        anchor: Option<(Pixels, Pixels)>,
        appearance: Option<AppearanceHold>,
    ) -> Self {
        let (previous_offset_y, previous_height, active) = match anchor {
            Some((offset_y, height)) => (offset_y, height, true),
            None => (px(0.), px(0.), false),
        };
        Self {
            handle,
            previous_offset_y,
            previous_height,
            active,
            appearance,
            slot: Rc::new(Cell::new(None)),
            child: None,
        }
    }

    /// Pass-through placed around the scroll content so its layout id is the
    /// content box the anchor measures.
    pub(super) fn probe(&self) -> ContentProbe {
        ContentProbe {
            slot: Rc::clone(&self.slot),
            child: None,
        }
    }
}

pub(super) struct ContentProbe {
    slot: Rc<Cell<Option<LayoutId>>>,
    child: Option<AnyElement>,
}

impl Element for ScrollAnchor {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut child = self.child.take().expect("scroll anchor child");
        let layout_id = child.request_layout(window, cx);
        (layout_id, child)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        child: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(appearance) = self.appearance.take() {
            let x = self.handle.offset().x;
            if appearance.pinned {
                // The scroller applies this during its own prepaint, using the
                // content height from this frame. That is `-max`, which
                // [`theme_scroll_y`] returns for a pinned tail.
                self.handle.scroll_to_bottom();
            } else {
                // A positive offset is not a position in this scroller. The
                // child clamps a too-negative offset to the new bottom, which
                // is the same rule as [`theme_scroll_y`] once `max` is known.
                let y = theme_scroll_y(false, appearance.offset_y, px(f32::MAX));
                self.handle.set_offset(point(x, y));
            }
        }
        if self.active
            && let Some(layout_id) = self.slot.get()
        {
            let height = window.layout_bounds(layout_id).size.height;
            let growth = height - self.previous_height;
            if growth.abs() > px(0.5) {
                let x = self.handle.offset().x;
                self.handle
                    .set_offset(point(x, self.previous_offset_y - growth));
            }
        }
        child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        child: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        child.paint(window, cx);
    }
}

impl IntoElement for ScrollAnchor {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl ParentElement for ScrollAnchor {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.child = elements.into_iter().next();
    }
}

impl Element for ContentProbe {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut child = self.child.take().expect("content probe child");
        let layout_id = child.request_layout(window, cx);
        self.slot.set(Some(layout_id));
        (layout_id, child)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        child: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        child: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        child.paint(window, cx);
    }
}

impl IntoElement for ContentProbe {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl ParentElement for ContentProbe {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.child = elements.into_iter().next();
    }
}

/// Offset after a theme relayout.
///
/// A pinned tail moves to the new bottom. Any other offset is kept, then
/// clamped when the content became shorter than that offset.
pub(super) fn theme_scroll_y(pinned: bool, previous_offset_y: Pixels, max_after: Pixels) -> Pixels {
    if pinned {
        return -max_after;
    }
    if previous_offset_y < -max_after {
        -max_after
    } else if previous_offset_y > px(0.) {
        px(0.)
    } else {
        previous_offset_y
    }
}

#[cfg(test)]
mod tests {
    use super::theme_scroll_y;
    use gpui_kit::px;

    #[test]
    fn theme_switch_pins_the_tail_and_keeps_a_mid_scroll() {
        assert_eq!(theme_scroll_y(true, px(-800.), px(1600.)), px(-1600.));
        assert_eq!(theme_scroll_y(false, px(-400.), px(1600.)), px(-400.));
        assert_eq!(theme_scroll_y(false, px(-800.), px(400.)), px(-400.));
        assert_eq!(theme_scroll_y(false, px(20.), px(400.)), px(0.));
    }
}
