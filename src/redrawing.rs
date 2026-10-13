//! The window's views, each drawn again only when its own state changes, as
//! the Application's Redrawing says: a [`Part`] draws a piece of its owner as
//! a cached view of its own, so a spinner turning or a time ticking in one
//! redraws that one, and every other is reused as it was last drawn.

use std::cell::Cell;
#[cfg(test)]
use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::base::ElementExt as _;
use gpui_kit::*;

/// What a part draws, from its owner's state.
type Draw<T> = Rc<dyn Fn(&mut T, &mut Window, &mut Context<T>) -> AnyElement>;

/// A piece of `T`'s view drawn as a view of its own: drawn again whenever
/// `T` changes, when it is notified itself, or when a view within it is, and
/// otherwise reused as it was, as long as its place is the same size.
pub struct Part<T: 'static> {
    owner: WeakEntity<T>,
    draw: Draw<T>,
    /// For a part as tall as what it draws, rather than as its place says:
    /// its height as last laid out.
    fitted: Option<Rc<Cell<Pixels>>>,
    #[cfg(test)]
    draws: usize,
    /// The width it was laid out at each time it was drawn.
    #[cfg(test)]
    widths: Rc<RefCell<Vec<Pixels>>>,
    _following: Vec<Subscription>,
}

impl<T: 'static> Part<T> {
    /// A part of `cx`'s entity, filling its place, drawn by `draw`.
    pub fn new(
        cx: &mut Context<T>,
        draw: impl Fn(&mut T, &mut Window, &mut Context<T>) -> AnyElement + 'static,
    ) -> Entity<Self> {
        Self::make(cx, None, Rc::new(draw))
    }

    /// A part of `cx`'s entity as tall as what `draw` draws, across its
    /// place's width.
    pub fn fitted(
        cx: &mut Context<T>,
        draw: impl Fn(&mut T, &mut Window, &mut Context<T>) -> AnyElement + 'static,
    ) -> Entity<Self> {
        Self::make(cx, Some(Rc::default()), Rc::new(draw))
    }

    fn make(cx: &mut Context<T>, fitted: Option<Rc<Cell<Pixels>>>, draw: Draw<T>) -> Entity<Self> {
        let owner = cx.entity();
        cx.new(|cx| Part {
            owner: owner.downgrade(),
            draw,
            fitted,
            #[cfg(test)]
            draws: 0,
            #[cfg(test)]
            widths: Rc::default(),
            _following: vec![cx.observe(&owner, |_, _, cx| cx.notify())],
        })
    }

    /// Drawn again also whenever `entity` changes while `when` holds of its
    /// owner, as for what it shows of another entity.
    pub fn follow<E: 'static>(
        &mut self,
        entity: &Entity<E>,
        when: impl Fn(&T, &App) -> bool + 'static,
        cx: &mut Context<Self>,
    ) {
        let owner = self.owner.clone();
        self._following.push(cx.observe(entity, move |_, _, cx| {
            if owner.upgrade().is_some_and(|owner| when(owner.read(cx), cx)) {
                cx.notify();
            }
        }));
    }

    /// How many times it has been drawn.
    #[cfg(test)]
    pub fn draws(&self) -> usize {
        self.draws
    }

    /// The widths it was laid out at, each time it was drawn.
    #[cfg(test)]
    pub fn widths(&self) -> Vec<Pixels> {
        self.widths.borrow().clone()
    }
}

/// `part` in its place, reused as it was last drawn until it changes.
pub fn cached<T: 'static>(part: &Entity<Part<T>>, cx: &App) -> AnyElement {
    let style = match &part.read(cx).fitted {
        Some(height) => StyleRefinement::default()
            .w_full()
            .flex_none()
            .h(height.get()),
        None => StyleRefinement::default().size_full(),
    };
    part.clone().cached(style).into_any_element()
}

/// Draws `part` again in the next frame, whatever else does.
pub fn redraw<T: 'static>(part: &Entity<Part<T>>, cx: &mut App) {
    part.update(cx, |_, cx| cx.notify());
}

impl<T: 'static> Render for Part<T> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.draws += 1;
        }
        let draw = self.draw.clone();
        let drawn = self
            .owner
            .update(cx, |owner, cx| draw(owner, window, cx))
            .unwrap_or_else(|_| Empty.into_any_element());
        #[cfg(test)]
        let widths = self.widths.clone();
        let Some(height) = self.fitted.clone() else {
            return div()
                .size_full()
                .child(drawn)
                .on_prepaint(move |_bounds, _, _| {
                    #[cfg(test)]
                    widths.borrow_mut().push(_bounds.size.width);
                })
                .into_any_element();
        };
        // Its place takes the height it was last laid out at; one that
        // differs is drawn again at once in its new height.
        let part = cx.entity().downgrade();
        div()
            .w_full()
            .flex_none()
            .child(drawn)
            .on_prepaint(move |bounds, _, cx| {
                #[cfg(test)]
                widths.borrow_mut().push(bounds.size.width);
                if bounds.size.height != height.get() {
                    height.set(bounds.size.height);
                    let part = part.clone();
                    cx.defer(move |cx| {
                        part.update(cx, |_, cx| cx.notify()).ok();
                    });
                }
            })
            .into_any_element()
    }
}
