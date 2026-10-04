//! The guided walkthrough, as the WalkthroughScope says: a series of steps,
//! each lighting one part of the window with a spotlight, the rest dimmed,
//! and explaining it in a callout beside it, from creating a project to the
//! prompt modes one by one.
//!
//! The parts it points at say where they are as they are laid out, through
//! [`mark`]; the window showing the walkthrough keeps a [`Tour`] and draws
//! it over everything with [`overlay`], selecting what each step points at
//! before it shows.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::*;

use crate::chat_input::{self, SendMode};

actions!(suspense, [OpenWalkthrough]);

/// How much room the cut-out leaves around the part it lights.
const ROOM: Pixels = px(4.);
/// How far the cut-out's corners are rounded.
const CUT_OUT_RADIUS: Pixels = px(6.);
/// How long the cut-out takes to move from one part to the next.
const MOVE: Duration = Duration::from_millis(200);
/// The callout's widest.
const CALLOUT_WIDTH: Pixels = px(320.);
/// How far the callout stands from the cut-out.
const GAP: Pixels = px(12.);
/// How far the callout keeps from the window's edges.
const MARGIN: Pixels = px(16.);
/// The pointer's width along the card's edge, and how far it reaches out.
const POINTER_WIDTH: Pixels = px(12.);
const POINTER_DEPTH: Pixels = px(6.);
/// How near the pointer comes to the card's corners.
const POINTER_INSET: Pixels = px(12.);

/// A part of the window a step points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// The ribbon's Project tab.
    ProjectTab,
    /// The Project tab's New Project button.
    NewProject,
    /// The New Project form's panel.
    NewProjectForm,
    /// The chat input's tabs.
    ChatTabs,
    /// A tab of the chat input.
    ChatTab(SendMode),
    /// The chat input's text box.
    TextBox,
}

/// Where each part was laid out, and in which frame.
#[derive(Default)]
struct Targets {
    bounds: HashMap<Target, (Bounds<Pixels>, u64)>,
    /// Counts the frames, so only where a part is laid out in this frame is
    /// taken.
    frame: u64,
}

impl Global for Targets {}

/// Marks its parent as `target`: a child covering the parent edge to edge,
/// padding and all, that notes where the parent is laid out, in the
/// window's coordinates, in this frame. It takes no room and no mouse.
pub fn mark(target: Target) -> impl IntoElement {
    canvas(
        move |bounds, _, cx| {
            let targets = cx.default_global::<Targets>();
            let frame = targets.frame;
            targets.bounds.insert(target, (bounds, frame));
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .bottom_0()
    .right_0()
}

/// Starts a frame: painted first in the window, before any part it points
/// at, so a part not laid out in this frame isn't pointed at where it was.
pub fn frame_start() -> impl IntoElement {
    canvas(
        |_, _, cx| {
            let targets = cx.default_global::<Targets>();
            targets.frame += 1;
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_0()
}

/// Where `target` is laid out in this frame, in the window's coordinates.
fn bounds_of(target: Target, cx: &App) -> Option<Bounds<Pixels>> {
    let targets = cx.try_global::<Targets>()?;
    let (bounds, frame) = targets.bounds.get(&target)?;
    (*frame == targets.frame).then_some(*bounds)
}

/// A step, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Projects,
    Create,
    FillIn,
    Modes,
    Code,
    Spec,
    Chain,
    Ask,
    Ready,
}

impl Step {
    pub const ALL: [Step; 9] = [
        Step::Projects,
        Step::Create,
        Step::FillIn,
        Step::Modes,
        Step::Code,
        Step::Spec,
        Step::Chain,
        Step::Ask,
        Step::Ready,
    ];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|step| *step == self).unwrap_or(0)
    }

    pub fn title(self) -> &'static str {
        match self {
            Step::Projects => "Projects live here",
            Step::Create => "Create a project",
            Step::FillIn => "Fill it in",
            Step::Modes => "Every prompt has a mode",
            Step::Code => "Code",
            Step::Spec => "Spec",
            Step::Chain => "Code and Spec",
            Step::Ask => "Ask",
            Step::Ready => "You're ready",
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            Step::Projects => {
                "A project is a folder holding its spec, its code, and a piton.config.pi that says where each is. Everything Suspense does, it does in the project open."
            }
            Step::Create => "Create a project to work in: click New Project.",
            Step::FillIn => {
                "Name the project, choose where it goes, the template it starts from, and the agents it is written for. The defaults are a fine start."
            }
            Step::Modes => {
                "What you type below is sent in the mode of the tab selected, and each mode does a different kind of work."
            }
            Step::Code => {
                "Changes the code to do what the prompt asks, reading the spec to know how, and leaves the spec as it is. Code tasks run on this machine, in the project."
            }
            Step::Spec => {
                "Changes the spec, the description of what the application should be, and leaves the code as it is, so a change can be agreed on before anything is built. Spec tasks run in a container that holds only the spec, and the spec is built after each."
            }
            Step::Chain => {
                "Changes the spec first, then hands it to a code step that changes the code to match, one prompt doing both. It is where the chat input starts, and the usual way to ask for a change."
            }
            Step::Ask => {
                "Asks a question about the code and the spec, changing neither, and answers beside the tasks. Questions run straight away, never waiting behind tasks."
            }
            Step::Ready => {
                "Type a prompt, pick its mode, and send it. The walkthrough can be taken again from Walkthrough, in the Application tab."
            }
        }
    }

    /// The parts it lights, together.
    pub fn targets(self) -> Vec<Target> {
        match self {
            Step::Projects => vec![Target::ProjectTab],
            Step::Create => vec![Target::NewProject],
            Step::FillIn => vec![Target::NewProjectForm],
            Step::Modes => vec![Target::ChatTabs],
            Step::Code => vec![Target::ChatTab(SendMode::Code)],
            Step::Spec => vec![Target::ChatTab(SendMode::Spec)],
            // Code and Spec together, beneath the chain.
            Step::Chain => vec![
                Target::ChatTab(SendMode::Code),
                Target::ChatTab(SendMode::Both),
                Target::ChatTab(SendMode::Spec),
            ],
            Step::Ask => vec![Target::ChatTab(SendMode::Ask)],
            Step::Ready => vec![Target::TextBox],
        }
    }

    /// The mode whose colour it reads in, for a mode's step.
    pub fn mode(self) -> Option<SendMode> {
        match self {
            Step::Code => Some(SendMode::Code),
            Step::Spec => Some(SendMode::Spec),
            Step::Chain => Some(SendMode::Both),
            Step::Ask => Some(SendMode::Ask),
            _ => None,
        }
    }

    /// Whether Next goes on from it: not from the steps that go on by
    /// themselves, once the user has done what they ask.
    pub fn can_go_on(self) -> bool {
        !matches!(self, Step::Create | Step::FillIn)
    }

    /// Whether it is one of the steps that create the project.
    fn creates(self) -> bool {
        matches!(self, Step::Create | Step::FillIn)
    }
}

/// A walkthrough under way.
pub struct Tour {
    step: Step,
    /// The project has been created, or the open one taken: the steps that
    /// create it are passed over.
    created: bool,
    /// The cut-out as it moves, kept as it is drawn.
    motion: Rc<RefCell<Motion>>,
}

/// The cut-out as it moves: where from, where to, and since when.
struct Motion {
    from: Option<Bounds<Pixels>>,
    to: Option<Bounds<Pixels>>,
    moved_at: Instant,
}

impl Default for Tour {
    fn default() -> Self {
        Self::new()
    }
}

impl Tour {
    /// A walkthrough from its first step.
    pub fn new() -> Self {
        Self {
            step: Step::Projects,
            created: false,
            motion: Rc::new(RefCell::new(Motion {
                from: None,
                to: None,
                moved_at: Instant::now(),
            })),
        }
    }

    pub fn step(&self) -> Step {
        self.step
    }

    /// Goes to `step`, as a step that goes on by itself does.
    pub fn go_to(&mut self, step: Step) {
        self.step = step;
    }

    /// The project was created, or the open one taken: on to the modes.
    pub fn project_ready(&mut self) {
        self.created = true;
        self.step = Step::Modes;
    }

    /// The step after this one; none after the last.
    pub fn next(&self) -> Option<Step> {
        let next = *Step::ALL.get(self.step.index() + 1)?;
        Some(if self.created && next.creates() {
            Step::Modes
        } else {
            next
        })
    }

    /// The step before this one, never back into the steps that create the
    /// project once it has been created; none before the first.
    pub fn back(&self) -> Option<Step> {
        let back = *Step::ALL.get(self.step.index().checked_sub(1)?)?;
        Some(if self.created && back.creates() {
            Step::Projects
        } else {
            back
        })
    }

}

impl Motion {
    /// Where the cut-out is now, moving toward `target`; whether it is
    /// still moving.
    fn cut_out(&mut self, target: Option<Bounds<Pixels>>) -> (Option<Bounds<Pixels>>, bool) {
        let now = Instant::now();
        if target != self.to {
            self.from = self.shown(now);
            self.to = target;
            self.moved_at = now;
        }
        let moving = self.from.is_some() && now.duration_since(self.moved_at) < MOVE;
        (self.shown(now), moving)
    }

    fn shown(&self, now: Instant) -> Option<Bounds<Pixels>> {
        let to = self.to?;
        let Some(from) = self.from else {
            return Some(to);
        };
        let t = (now.duration_since(self.moved_at).as_secs_f32() / MOVE.as_secs_f32()).min(1.);
        // Eased out.
        let t = 1. - (1. - t) * (1. - t);
        let lerp = |a: Pixels, b: Pixels| a + (b - a) * t;
        Some(Bounds {
            origin: point(lerp(from.origin.x, to.origin.x), lerp(from.origin.y, to.origin.y)),
            size: size(
                lerp(from.size.width, to.size.width),
                lerp(from.size.height, to.size.height),
            ),
        })
    }
}

/// What the callout's buttons do.
pub struct Actions {
    pub back: Box<dyn Fn(&mut Window, &mut App)>,
    pub next: Box<dyn Fn(&mut Window, &mut App)>,
    pub skip: Box<dyn Fn(&mut Window, &mut App)>,
    /// Offered at Create a project while a project is open.
    pub use_open_project: Option<Box<dyn Fn(&mut Window, &mut App)>>,
}

/// The walkthrough over the window: dimmed but for the cut-out around the
/// step's part, which alone takes the mouse with the callout beside it.
pub fn overlay(tour: &mut Tour, actions: Actions, window: &mut Window, cx: &App) -> AnyElement {
    let step = tour.step;
    let theme = cx.theme();
    let accent = step
        .mode()
        .map_or(theme.ring, |mode| chat_input::mode_color(mode, cx));
    let width = CALLOUT_WIDTH.min(window.viewport_size().width - MARGIN * 2.);
    let card = callout(step, tour.back().is_some(), actions, width, accent, cx);
    let spotlight = Spotlight {
        targets: step.targets(),
        motion: tour.motion.clone(),
        accent,
        dim: crate::theme::dimming(cx),
        surface: theme.popover,
        border: theme.border,
        callout: Some(card),
    };
    // Lets UI tests find the walkthrough; inert in normal builds.
    gpui_kit::TestSupportExt::test_support(div().id("walkthrough").absolute().inset_0().child(spotlight))
        .into_any_element()
}

/// Which side of its cut-out the callout sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Below,
    Above,
    Right,
    Left,
    /// No side has room, as for a panel filling the window: over it.
    Over,
}

/// Where a callout `card` big goes beside `cut`, in a window `viewport`
/// big: beneath, above, right, or left, the first side it fits on whole,
/// 12 pixels from the cut-out, centred on it along that side, slid to stay
/// 16 pixels inside the window.
pub fn place(cut: Bounds<Pixels>, card: Size<Pixels>, viewport: Size<Pixels>) -> (Point<Pixels>, Side) {
    let slide_x = |x: Pixels| x.min(viewport.width - MARGIN - card.width).max(MARGIN);
    let slide_y = |y: Pixels| y.min(viewport.height - MARGIN - card.height).max(MARGIN);
    let centred_x = slide_x(cut.center().x - card.width / 2.);
    let centred_y = slide_y(cut.center().y - card.height / 2.);
    let fits_tall = card.height + MARGIN * 2. <= viewport.height;
    let fits_wide = card.width + MARGIN * 2. <= viewport.width;
    if fits_wide && cut.bottom() + GAP + card.height <= viewport.height - MARGIN {
        (point(centred_x, cut.bottom() + GAP), Side::Below)
    } else if fits_wide && cut.top() - GAP - card.height >= MARGIN {
        (point(centred_x, cut.top() - GAP - card.height), Side::Above)
    } else if fits_tall && cut.right() + GAP + card.width <= viewport.width - MARGIN {
        (point(cut.right() + GAP, centred_y), Side::Right)
    } else if fits_tall && cut.left() - GAP - card.width >= MARGIN {
        (point(cut.left() - GAP - card.width, centred_y), Side::Left)
    } else {
        // Over it, near its foot, inside the window.
        (
            point(
                slide_x(cut.right() - card.width - MARGIN * 2.),
                slide_y(cut.bottom() - card.height - MARGIN * 2.),
            ),
            Side::Over,
        )
    }
}

/// The pointer's three corners, on the edge of a callout at `card` facing
/// the cut-out on `side`, aimed at `cut`'s centre, kept 12 pixels from the
/// card's corners: its base's two ends, then its tip.
pub fn pointer(card: Bounds<Pixels>, cut: Bounds<Pixels>, side: Side) -> Option<[Point<Pixels>; 3]> {
    let half = POINTER_WIDTH / 2.;
    let along_x = cut
        .center()
        .x
        .max(card.left() + POINTER_INSET + half)
        .min(card.right() - POINTER_INSET - half);
    let along_y = cut
        .center()
        .y
        .max(card.top() + POINTER_INSET + half)
        .min(card.bottom() - POINTER_INSET - half);
    Some(match side {
        Side::Below => [
            point(along_x - half, card.top()),
            point(along_x + half, card.top()),
            point(along_x, card.top() - POINTER_DEPTH),
        ],
        Side::Above => [
            point(along_x - half, card.bottom()),
            point(along_x + half, card.bottom()),
            point(along_x, card.bottom() + POINTER_DEPTH),
        ],
        Side::Right => [
            point(card.left(), along_y - half),
            point(card.left(), along_y + half),
            point(card.left() - POINTER_DEPTH, along_y),
        ],
        Side::Left => [
            point(card.right(), along_y - half),
            point(card.right(), along_y + half),
            point(card.right() + POINTER_DEPTH, along_y),
        ],
        Side::Over => return None,
    })
}

/// The dimming, the cut-out, and the callout, placed as the window is laid
/// out in this frame: the cut-out from where its part is laid out, in the
/// window's own coordinates, and the callout beside it at its measured size.
struct Spotlight {
    targets: Vec<Target>,
    motion: Rc<RefCell<Motion>>,
    accent: Hsla,
    dim: Hsla,
    surface: Hsla,
    border: Hsla,
    callout: Option<AnyElement>,
}

/// What a spotlight placed, to paint it.
struct Placed {
    cut: Option<Bounds<Pixels>>,
    card: Bounds<Pixels>,
    side: Side,
    shades: Vec<Bounds<Pixels>>,
}

impl IntoElement for Spotlight {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Spotlight {
    type RequestLayoutState = ();
    type PrepaintState = Placed;

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
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style {
            position: Position::Absolute,
            ..Style::default()
        };
        style.inset = Edges::all(px(0.).into());
        style.size = size(relative(1.).into(), relative(1.).into());
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let viewport = window.viewport_size();
        // Where the parts are laid out in this frame, before this.
        let target = self
            .targets
            .iter()
            .filter_map(|target| bounds_of(*target, cx))
            .reduce(|a, b| a.union(&b))
            .map(|bounds| bounds.dilate(ROOM));
        let (cut, moving) = self.motion.borrow_mut().cut_out(target);
        if moving {
            window.request_animation_frame();
        }
        let whole = Bounds::new(point(px(0.), px(0.)), viewport);
        let shades = match cut {
            Some(cut) => vec![
                Bounds::from_corners(point(px(0.), px(0.)), point(viewport.width, cut.top())),
                Bounds::from_corners(point(px(0.), cut.bottom()), point(viewport.width, viewport.height)),
                Bounds::from_corners(point(px(0.), cut.top()), point(cut.left(), cut.bottom())),
                Bounds::from_corners(point(cut.right(), cut.top()), point(viewport.width, cut.bottom())),
            ],
            None => vec![whole],
        };
        // The dimmed window takes the mouse, and does nothing with it.
        for shade in &shades {
            window.insert_hitbox(*shade, HitboxBehavior::BlockMouse);
        }
        let anchor = cut.unwrap_or(Bounds::new(
            point(viewport.width / 2., viewport.height / 2.),
            Size::default(),
        ));
        let Some(callout) = self.callout.as_mut() else {
            return Placed {
                cut,
                card: Bounds::default(),
                side: Side::Over,
                shades,
            };
        };
        let measured = callout.layout_as_root(
            size(AvailableSpace::MinContent, AvailableSpace::MinContent),
            window,
            cx,
        );
        let (origin, side) = place(anchor, measured, viewport);
        callout.prepaint_at(origin, window, cx);
        Placed {
            cut,
            card: Bounds::new(origin, measured),
            side,
            shades,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        placed: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        for shade in &placed.shades {
            window.paint_quad(fill(*shade, self.dim));
        }
        if let Some(cut) = placed.cut {
            window.paint_quad(quad(
                cut,
                CUT_OUT_RADIUS,
                transparent_black(),
                px(2.),
                self.accent,
                BorderStyle::default(),
            ));
        }
        if let Some(callout) = self.callout.as_mut() {
            callout.paint(window, cx);
        }
        // The pointer over the card's edge, in its colour, with its border
        // along the two sides that stand out.
        if let Some(cut) = placed.cut
            && let Some([a, b, tip]) = pointer(placed.card, cut, placed.side)
        {
            let mut fill = PathBuilder::fill();
            fill.move_to(a);
            fill.line_to(tip);
            fill.line_to(b);
            fill.close();
            if let Ok(path) = fill.build() {
                window.paint_path(path, self.surface);
            }
            let mut line = PathBuilder::stroke(px(1.));
            line.move_to(a);
            line.line_to(tip);
            line.line_to(b);
            if let Ok(path) = line.build() {
                window.paint_path(path, self.border);
            }
        }
    }
}

/// The callout's card, `width` wide: with 16 pixels of padding, left
/// aligned, the title, its text 8 pixels beneath, and 12 pixels beneath
/// that the footer, Skip tour at its left, and at its right, 8 pixels
/// apart, where the step is, Back, and Next.
fn callout(
    step: Step,
    has_back: bool,
    actions: Actions,
    width: Pixels,
    accent: Hsla,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let Actions {
        back,
        next,
        skip,
        use_open_project,
    } = actions;
    let last = step == Step::Ready;
    let count = Step::ALL.len();
    // A mode's step reads in its mode's colour.
    let title_color = step.mode().map_or(theme.foreground, |_| accent);
    let card = v_flex()
        .id("walkthrough-callout")
        .w(width)
        .items_start()
        .p_4()
        .rounded(px(8.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .shadow_lg()
        .occlude()
        .child(
            div()
                .font_semibold()
                .text_color(title_color)
                .child(step.title()),
        )
        .child(div().mt_2().w_full().text_sm().child(step.text()))
        .children(use_open_project.map(|use_open| {
            div().mt_3().child(
                Button::new("walkthrough-use-open-project")
                    .small()
                    .label("Use the open project")
                    .on_click(move |_, window, cx| use_open(window, cx)),
            )
        }))
        .child(
            h_flex()
                .mt_3()
                .w_full()
                .items_center()
                .child(
                    Button::new("walkthrough-skip")
                        .link()
                        .small()
                        .text_color(theme.muted_foreground)
                        .label("Skip tour")
                        .on_click(move |_, window, cx| skip(window, cx)),
                )
                .child(div().flex_1())
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("Step {} of {count}", step.index() + 1)),
                        )
                        .child(
                            Button::new("walkthrough-back")
                                .small()
                                .label("Back")
                                .disabled(!has_back)
                                .on_click(move |_, window, cx| back(window, cx)),
                        )
                        .child(
                            Button::new("walkthrough-next")
                                .small()
                                .primary()
                                .label(if last { "Done" } else { "Next" })
                                .disabled(!step.can_go_on())
                                .on_click(move |_, window, cx| next(window, cx)),
                        ),
                ),
        );
    gpui_kit::TestSupportExt::test_support(card).into_any_element()
}

/// Whether the walkthrough has been taken or skipped, remembered for the
/// user, as the UserPreferencesScope says; not until it has. Tests neither
/// read nor write it.
pub mod preference {
    #[cfg(not(test))]
    fn file() -> Option<std::path::PathBuf> {
        Some(dirs::config_dir()?.join("suspense").join("walkthrough-taken"))
    }

    pub fn taken() -> bool {
        #[cfg(not(test))]
        if let Some(file) = file() {
            return file.exists();
        }
        false
    }

    /// Remembers it as taken; it is only a convenience, so failing to is
    /// ignored.
    pub fn set_taken() {
        #[cfg(not(test))]
        if let Some(file) = file() {
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir).ok();
            }
            std::fs::write(file, "taken").ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::{Bounds, point, px, size};

    use super::{Side, Step, Tour, place, pointer};

    #[test]
    fn steps_go_on_and_back_past_the_project_once_it_exists() {
        let mut tour = Tour::new();
        assert_eq!(tour.back(), None);
        assert_eq!(tour.next(), Some(Step::Create));
        tour.go_to(Step::Create);
        assert!(!tour.step().can_go_on());
        tour.go_to(Step::FillIn);
        tour.project_ready();
        assert_eq!(tour.step(), Step::Modes);
        // Never back into creating it.
        assert_eq!(tour.back(), Some(Step::Projects));
        tour.go_to(Step::Projects);
        assert_eq!(tour.next(), Some(Step::Modes));
        tour.go_to(Step::Ready);
        assert_eq!(tour.next(), None);
        assert_eq!(Step::ALL.len(), 9);
    }

    #[test]
    fn the_callout_goes_where_it_fits() {
        let window = size(px(1200.), px(800.));
        let card = size(px(320.), px(180.));
        let tab = Bounds::new(point(px(400.), px(10.)), size(px(60.), px(24.)));
        let (at, side) = place(tab, card, window);
        assert_eq!(side, Side::Below);
        // 12 pixels beneath, centred on it.
        assert_eq!(at.y, px(46.));
        assert_eq!(at.x + px(160.), tab.center().x);
        let bottom = Bounds::new(point(px(100.), px(700.)), size(px(400.), px(60.)));
        let (at, side) = place(bottom, card, window);
        assert_eq!(side, Side::Above);
        assert_eq!(at.y + card.height, px(688.));
        // Slid to stay 16 pixels in, the pointer still at the part's centre.
        let corner = Bounds::new(point(px(1180.), px(10.)), size(px(10.), px(10.)));
        let (at, side) = place(corner, card, window);
        assert_eq!(at.x + px(320.), px(1184.));
        let tip = pointer(Bounds::new(at, card), corner, side).unwrap()[2];
        assert_eq!(tip.y, at.y - px(6.));
        assert!(tip.x <= px(1184.) - px(18.));
        let centred = Bounds::new(point(px(400.), px(10.)), size(px(60.), px(24.)));
        let (at, side) = place(centred, card, window);
        let [a, b, tip] = pointer(Bounds::new(at, card), centred, side).unwrap();
        assert_eq!(tip.x, centred.center().x);
        assert_eq!(b.x - a.x, px(12.));
        // A panel filling the window: over it.
        let panel = Bounds::new(point(px(28.), px(28.)), size(px(1144.), px(744.)));
        assert_eq!(place(panel, card, window).1, Side::Over);
    }
}
