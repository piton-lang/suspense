//! A virtualized list that knows how tall all of its rows are without laying
//! them all out at once, so its scrollbar holds still.
//!
//! gpui's list counts a row it hasn't laid out as no height at all, so how far
//! it scrolls, and a scrollbar's thumb with it, would jump as rows came into
//! view. Laying out every row up front fixes that, but costs every row again
//! each time the width changes or rows are added. Instead, each row's height
//! is kept here: the rows the list lays out in view are noted as it does, and
//! the rest are measured a few at a time, a frame's small share at a time,
//! until every row is known. Until a row is measured at the current width, it
//! counts as tall as it last was, or as tall as the rows measured so far on
//! average. The scrollbar reads these heights, and the list is only ever made
//! to lay out the rows in view.

use std::cell::{Ref, RefCell};
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::*;

use crate::scrollbar::Scroll;

/// Draws row `ix` of a list.
pub type RenderRow = Rc<dyn Fn(usize, &mut Window, &mut App) -> AnyElement>;

/// How long a frame may spend measuring rows out of view.
const MEASURE_BUDGET: Duration = Duration::from_millis(4);

/// How tall a row counts before any row has been measured.
const FIRST_GUESS: Pixels = px(40.);

/// A virtualized list's state, and the height of every one of its rows.
#[derive(Clone)]
pub struct MeasuredList {
    state: ListState,
    heights: Rc<RefCell<Heights>>,
}

#[derive(Default)]
struct Heights {
    /// The width rows were last measured at, once the list has been laid out.
    width: Option<Pixels>,
    rows: Vec<RowHeight>,
    /// Where each row starts, and after the last, where the list ends; built
    /// again once a height changes.
    tops: Vec<Pixels>,
    tops_stale: bool,
    /// No row before this one is waiting to be measured.
    next: usize,
    /// The list's own guesses at its rows' heights were thrown away, so its
    /// scrolling by the wheel needs them given again.
    hint: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct RowHeight {
    height: Pixels,
    /// Measured at the current width, rather than guessed.
    exact: bool,
}

impl Heights {
    /// How tall a row not yet measured counts: as tall as the rows measured so
    /// far, on average.
    fn guess(&self) -> Pixels {
        let (sum, count) = self
            .rows
            .iter()
            .filter(|row| row.exact)
            .fold((px(0.), 0usize), |(sum, count), row| {
                (sum + row.height, count + 1)
            });
        if count == 0 {
            FIRST_GUESS
        } else {
            sum / count as f32
        }
    }

    fn record(&mut self, ix: usize, height: Pixels) {
        let Some(row) = self.rows.get_mut(ix) else {
            return;
        };
        if row.height != height {
            self.tops_stale = true;
        }
        *row = RowHeight {
            height,
            exact: true,
        };
    }

    fn tops(&mut self) -> &[Pixels] {
        if self.tops_stale || self.tops.len() != self.rows.len() + 1 {
            self.tops.clear();
            let mut top = px(0.);
            self.tops.push(top);
            for row in &self.rows {
                top += row.height;
                self.tops.push(top);
            }
            self.tops_stale = false;
        }
        &self.tops
    }
}

impl MeasuredList {
    /// An empty list, laying out `overdraw` beyond what's in view so rows
    /// don't pop in as it scrolls.
    pub fn new(overdraw: Pixels) -> Self {
        Self {
            state: ListState::new(0, ListAlignment::Top, overdraw),
            heights: Rc::default(),
        }
    }

    pub fn state(&self) -> &ListState {
        &self.state
    }

    pub fn count(&self) -> usize {
        self.heights.borrow().rows.len()
    }

    /// Makes it a list of `count` rows, none of them known, scrolled to the
    /// top.
    pub fn reset(&self, count: usize) {
        self.state.reset(count);
        let mut heights = self.heights.borrow_mut();
        let guess = heights.guess();
        heights.rows = vec![
            RowHeight {
                height: guess,
                exact: false,
            };
            count
        ];
        heights.tops_stale = true;
        heights.next = 0;
        heights.hint = true;
    }

    /// Replaces the rows in `old` with `count` new ones.
    pub fn splice(&self, old: Range<usize>, count: usize) {
        self.state.splice(old.clone(), count);
        let mut heights = self.heights.borrow_mut();
        let guess = heights.guess();
        let new = RowHeight {
            height: guess,
            exact: false,
        };
        let old = old.start.min(heights.rows.len())..old.end.min(heights.rows.len());
        // Rows added at the end lie below whatever the list scrolls through,
        // so its own guesses above them still stand.
        if old.end < heights.rows.len() {
            heights.hint = true;
        }
        heights.next = heights.next.min(old.start);
        heights.rows.splice(old, std::iter::repeat_n(new, count));
        heights.tops_stale = true;
    }

    /// Has the rows in `range`, whose content changed, measured again. Until
    /// they are, they count as tall as they were.
    pub fn remeasure(&self, range: Range<usize>) {
        let mut heights = self.heights.borrow_mut();
        let range = range.start.min(heights.rows.len())..range.end.min(heights.rows.len());
        if range.is_empty() {
            return;
        }
        self.state.remeasure_items(range.clone());
        for row in &mut heights.rows[range.clone()] {
            row.exact = false;
        }
        heights.next = heights.next.min(range.start);
    }

    pub fn scroll_to_end(&self) {
        self.state.scroll_to_end();
    }

    pub fn scroll_to_top(&self) {
        self.state.scroll_to(ListOffset {
            item_ix: 0,
            offset_in_item: px(0.),
        });
    }

    /// What its scrollbar scrolls.
    pub fn scroll(&self) -> Scroll {
        Scroll::Measured(self.clone())
    }

    /// Whether every row's height at the current width is known.
    #[cfg(test)]
    pub fn is_settled(&self) -> bool {
        let heights = self.heights.borrow();
        heights.width.is_some() && heights.rows.iter().all(|row| row.exact)
    }

    /// Where each row starts, and where the list ends.
    fn tops(&self) -> Ref<'_, [Pixels]> {
        self.heights.borrow_mut().tops();
        Ref::map(self.heights.borrow(), |heights| heights.tops.as_slice())
    }

    /// How far it is scrolled, negative going down.
    pub fn offset(&self) -> Point<Pixels> {
        let top = self.state.logical_scroll_top();
        let max = self.max_offset().y;
        let tops = self.tops();
        let count = tops.len() - 1;
        let y = if top.item_ix < count {
            tops[top.item_ix] + top.offset_in_item
        } else {
            tops[count]
        };
        point(px(0.), -y.min(max).max(px(0.)))
    }

    /// How far it can scroll.
    pub fn max_offset(&self) -> Point<Pixels> {
        let total = *self.tops().last().unwrap_or(&px(0.));
        let viewport = self.state.viewport_bounds().size.height;
        point(px(0.), (total - viewport).max(px(0.)))
    }

    pub fn set_offset(&self, offset: Point<Pixels>) {
        let y = (-offset.y).max(px(0.)).min(self.max_offset().y);
        let tops = self.tops();
        let count = tops.len() - 1;
        // The row the new top falls in.
        let ix = tops[1..].partition_point(|end| *end <= y).min(count);
        let offset_in_item = if ix < count { y - tops[ix] } else { px(0.) };
        drop(tops);
        self.state.scroll_to(ListOffset {
            item_ix: ix,
            offset_in_item,
        });
    }

    /// How tall all of its rows are, as far as they're known.
    pub fn total_height(&self) -> Pixels {
        *self.tops().last().unwrap_or(&px(0.))
    }

    /// The part of it in view.
    pub fn viewport(&self) -> Bounds<Pixels> {
        self.state.viewport_bounds()
    }

    /// The list, drawing its rows with `render`. Only the rows in view are
    /// laid out each frame, along with a few out of view until every row has
    /// been measured.
    pub fn element(&self, render: RenderRow) -> AnyElement {
        let rows = list(self.state.clone(), {
            let render = render.clone();
            move |ix, window, cx| render(ix, window, cx)
        })
        .size_full();
        let this = self.clone();
        // Prepainted after the list, once it has laid out this frame's rows.
        let measure = canvas(
            move |_, window, cx| this.after_layout(&render, window, cx),
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_0();
        div()
            .relative()
            .size_full()
            .child(rows)
            .child(measure)
            .into_any_element()
    }

    fn after_layout(&self, render: &RenderRow, window: &mut Window, cx: &mut App) {
        let viewport = self.state.viewport_bounds();
        let width = viewport.size.width;
        if width <= px(0.) {
            return;
        }
        let count = self.count();
        let mut heights = self.heights.borrow_mut();
        // A new width wraps the rows anew: every height is only a guess until
        // measured again. The list threw its own guesses away too.
        if heights.width != Some(width) {
            heights.width = Some(width);
            for row in &mut heights.rows {
                row.exact = false;
            }
            heights.next = 0;
            heights.hint = true;
        }

        // The rows the list laid out, from the top of the view down.
        let first = self.state.logical_scroll_top().item_ix;
        for ix in first..count {
            let Some(bounds) = self.state.bounds_for_item(ix) else {
                break;
            };
            heights.record(ix, bounds.size.height);
            if bounds.top() > viewport.bottom() {
                break;
            }
        }

        // Scrolling by the wheel goes by the list's own idea of its rows'
        // heights, so rows it hasn't laid out need a guess there too.
        if std::mem::take(&mut heights.hint) {
            let guess = heights.guess();
            drop(heights);
            self.state.clone().with_uniform_item_height(guess);
            heights = self.heights.borrow_mut();
        }

        // Then some of the rest, measured as the list would lay them out.
        let space = size(AvailableSpace::Definite(width), AvailableSpace::MinContent);
        let deadline = Instant::now() + MEASURE_BUDGET;
        loop {
            let next = heights.next;
            let Some(ix) = heights.rows[next.min(count)..]
                .iter()
                .position(|row| !row.exact)
                .map(|at| next + at)
            else {
                heights.next = count;
                break;
            };
            heights.next = ix;
            drop(heights);
            let mut element = render(ix, window, cx);
            let height = element.layout_as_root(space, window, cx).height;
            heights = self.heights.borrow_mut();
            heights.record(ix, height);
            if Instant::now() >= deadline {
                break;
            }
        }
        if heights.rows.iter().skip(heights.next).any(|row| !row.exact) {
            window.request_animation_frame();
        }
    }
}
