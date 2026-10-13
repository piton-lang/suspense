//! Nothing above the view moves what is in it: in a list scrolled away from
//! its top, an item above the view that is added, removed, or changes height
//! never moves the items in view, the list's scroll shifting by the same
//! amount instead, at once and with no motion. A list at its top keeps its
//! top.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::*;

use super::slide::Slides;

#[derive(Default)]
struct SteadyState {
    /// The items laid out this frame, each by its key.
    items: Vec<(SharedString, LayoutId)>,
    /// Where each item was last laid out, down from the list's top, however
    /// it had scrolled.
    last: HashMap<SharedString, Pixels>,
    /// The first item in view as the list was last laid out, while it was
    /// scrolled away from its top.
    anchor: Option<SharedString>,
}

/// A list's items, each by its key, kept steady across frames.
#[derive(Clone, Default)]
pub struct Steady(Rc<RefCell<SteadyState>>);

impl Steady {
    /// `child`, one of the list's items, by `key`.
    pub fn track(&self, key: impl Into<SharedString>, child: impl IntoElement) -> Tracked {
        Tracked {
            key: key.into(),
            steady: self.clone(),
            child: child.into_any_element(),
        }
    }

    /// The item first in view as the list was last laid out, while it was
    /// scrolled away from its top, and where it lay then, down from the
    /// list's top: what the list holds still.
    pub fn anchor(&self) -> Option<(SharedString, Pixels)> {
        let state = self.0.borrow();
        let anchor = state.anchor.clone()?;
        let top = *state.last.get(&anchor)?;
        Some((anchor, top))
    }

    /// `child`, the list scrolled by `handle`, holding what is in view
    /// still. Items that `slides` would slide to their places are told the
    /// list shifted, so its shift never counts as their move.
    pub fn hold(&self, handle: ScrollHandle, slides: Slides, child: impl IntoElement) -> Held {
        Held {
            steady: self.clone(),
            handle,
            slides,
            child: child.into_any_element(),
        }
    }
}

pub struct Tracked {
    key: SharedString,
    steady: Steady,
    child: AnyElement,
}

impl IntoElement for Tracked {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Tracked {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let id = self.child.request_layout(window, cx);
        self.steady.0.borrow_mut().items.push((self.key.clone(), id));
        (id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

pub struct Held {
    steady: Steady,
    handle: ScrollHandle,
    slides: Slides,
    child: AnyElement,
}

impl IntoElement for Held {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Held {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        self.steady.0.borrow_mut().items.clear();
        (self.child.request_layout(window, cx), ())
    }

    /// Once laid out, before anything in it is placed: where the item first
    /// in view last frame has moved to, down the list, the scroll follows,
    /// so it, and all in view with it, stay where they were.
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let items: Vec<(SharedString, Bounds<Pixels>)> = {
            let state = self.steady.0.borrow();
            state.items.clone()
        }
        .into_iter()
        .map(|(key, id)| {
            let mut at = window.layout_bounds(id);
            at.origin.y -= bounds.top();
            (key, at)
        })
        .collect();
        {
            let state = self.steady.0.borrow();
            let offset = self.handle.offset();
            let moved = state.anchor.as_ref().and_then(|anchor| {
                let was = state.last.get(anchor)?;
                let (_, now) = items.iter().find(|(key, _)| key == anchor)?;
                Some(now.top() - *was)
            });
            if offset.y < px(0.)
                && let Some(by) = moved.filter(|by| *by != px(0.))
            {
                self.handle.set_offset(point(offset.x, offset.y - by));
                // What moved with it hasn't moved at all where it is seen.
                for (key, now) in &items {
                    if state
                        .last
                        .get(key)
                        .is_some_and(|was| f32::from((now.top() - *was - by).abs()) < 0.5)
                    {
                        self.slides.shift(key, point(px(0.), by));
                    }
                }
            }
        }
        self.child.prepaint(window, cx);
        let top = -self.handle.offset().y;
        let mut state = self.steady.0.borrow_mut();
        state.anchor = (top > px(0.))
            .then(|| items.iter().find(|(_, at)| at.bottom() > top))
            .flatten()
            .map(|(key, _)| key.clone());
        state.last = items.into_iter().map(|(key, at)| (key, at.top())).collect();
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}
