//! The guided walkthrough, as the WalkthroughScope says: a series of steps,
//! each lighting one part of the window with a spotlight, the rest dimmed,
//! and explaining it in a callout beside it, from creating a project to the
//! prompt modes one by one.
//!
//! The parts it points at say where they are as they are laid out, through
//! [`note`]; the window showing the walkthrough keeps a [`Tour`] and draws
//! it over everything with [`overlay`], selecting what each step points at
//! before it shows.

use std::collections::HashMap;
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
/// About how tall a callout is, to place it where it fits.
const CALLOUT_HEIGHT: Pixels = px(200.);
/// How far the callout stands from the part it points at.
const GAP: Pixels = px(12.);
/// How far the callout keeps from the window's edges.
const MARGIN: Pixels = px(8.);
/// The pointer's size.
const POINTER: Pixels = px(10.);

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

/// Where each part was last laid out, and whether a walkthrough shows.
#[derive(Default)]
struct Targets {
    bounds: HashMap<Target, Bounds<Pixels>>,
    showing: bool,
}

impl Global for Targets {}

/// Notes where `target` is as it is laid out, for an element's
/// `on_prepaint`; while a walkthrough shows, a part that moved draws it again.
pub fn note(target: Target) -> impl Fn(Bounds<Pixels>, &mut Window, &mut App) + 'static {
    move |bounds, window, cx| {
        let targets = cx.default_global::<Targets>();
        if targets.bounds.get(&target) != Some(&bounds) {
            targets.bounds.insert(target, bounds);
            if targets.showing {
                window.refresh();
            }
        }
    }
}

/// Where `target` was last laid out.
fn bounds_of(target: Target, cx: &App) -> Option<Bounds<Pixels>> {
    cx.try_global::<Targets>()?.bounds.get(&target).copied()
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
    /// The cut-out as it moves: where from, where to, and since when.
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
            from: None,
            to: None,
            moved_at: Instant::now(),
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

/// Marks a walkthrough as showing, or not, so the parts it points at draw
/// it again as they move.
pub fn set_showing(showing: bool, cx: &mut App) {
    cx.default_global::<Targets>().showing = showing;
}

/// The walkthrough over the window: dimmed but for the cut-out around the
/// step's part, which alone takes the mouse with the callout beside it.
pub fn overlay(tour: &mut Tour, actions: Actions, window: &mut Window, cx: &App) -> AnyElement {
    let step = tour.step;
    let target = step
        .targets()
        .into_iter()
        .filter_map(|target| bounds_of(target, cx))
        .reduce(|a, b| a.union(&b))
        .map(|bounds| bounds.dilate(ROOM));
    let (cut, moving) = tour.cut_out(target);
    if moving || cut.is_none() {
        window.request_animation_frame();
    }
    let viewport = window.viewport_size();
    let theme = cx.theme();
    let dim = crate::theme::dimming(cx);
    let accent = step
        .mode()
        .map_or(theme.ring, |mode| chat_input::mode_color(mode, cx));
    // The dimmed window, in four pieces around the cut-out, each taking the
    // mouse and doing nothing with it.
    let shade = |left: Pixels, top: Pixels, width: Pixels, height: Pixels| {
        div()
            .absolute()
            .left(left)
            .top(top)
            .w(width.max(px(0.)))
            .h(height.max(px(0.)))
            .bg(dim)
            .occlude()
    };
    let mut layer = div()
        .id("walkthrough")
        .absolute()
        .top_0()
        .left_0()
        .size_full();
    match cut {
        Some(cut) => {
            let (l, t, r, b) = (cut.left(), cut.top(), cut.right(), cut.bottom());
            layer = layer
                .child(shade(px(0.), px(0.), viewport.width, t))
                .child(shade(px(0.), b, viewport.width, viewport.height - b))
                .child(shade(px(0.), t, l, b - t))
                .child(shade(r, t, viewport.width - r, b - t))
                .child(
                    div()
                        .absolute()
                        .left(l)
                        .top(t)
                        .w(cut.size.width)
                        .h(cut.size.height)
                        .rounded(CUT_OUT_RADIUS)
                        .border_2()
                        .border_color(accent),
                );
        }
        None => layer = layer.child(shade(px(0.), px(0.), viewport.width, viewport.height)),
    }
    let anchor = cut.unwrap_or(Bounds {
        origin: point(viewport.width / 2., viewport.height / 2.),
        size: Size::default(),
    });
    let (position, side) = place(anchor, viewport);
    let layer = layer.child(callout(
        step,
        tour.back().is_some(),
        actions,
        position,
        side,
        anchor,
        accent,
        cx,
    ));
    // Lets UI tests find the walkthrough; inert in normal builds.
    gpui_kit::TestSupportExt::test_support(layer).into_any_element()
}

/// Which side of its part the callout sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Below,
    Above,
    Right,
    Left,
    Over,
}

/// Where the callout goes beside `part`, in a window `viewport` big: on
/// whichever side has room, below first, never off the window.
pub fn place(part: Bounds<Pixels>, viewport: Size<Pixels>) -> (Point<Pixels>, Side) {
    let fits_x = |x: Pixels| x.max(MARGIN).min(viewport.width - CALLOUT_WIDTH - MARGIN);
    let fits_y = |y: Pixels| y.max(MARGIN).min(viewport.height - CALLOUT_HEIGHT - MARGIN);
    let centre_x = part.center().x - CALLOUT_WIDTH / 2.;
    let centre_y = part.center().y - CALLOUT_HEIGHT / 2.;
    let (at, side) = if viewport.height - part.bottom() >= CALLOUT_HEIGHT + GAP + MARGIN {
        (point(centre_x, part.bottom() + GAP), Side::Below)
    } else if part.top() >= CALLOUT_HEIGHT + GAP + MARGIN {
        (point(centre_x, part.top() - GAP - CALLOUT_HEIGHT), Side::Above)
    } else if viewport.width - part.right() >= CALLOUT_WIDTH + GAP + MARGIN {
        (point(part.right() + GAP, centre_y), Side::Right)
    } else if part.left() >= CALLOUT_WIDTH + GAP + MARGIN {
        (point(part.left() - GAP - CALLOUT_WIDTH, centre_y), Side::Left)
    } else {
        // A part as big as the window, as a panel: over it, near its foot.
        (
            point(
                part.right() - CALLOUT_WIDTH - px(24.),
                part.bottom() - CALLOUT_HEIGHT - px(24.),
            ),
            Side::Over,
        )
    };
    (point(fits_x(at.x), fits_y(at.y)), side)
}

#[allow(clippy::too_many_arguments)]
fn callout(
    step: Step,
    has_back: bool,
    actions: Actions,
    position: Point<Pixels>,
    side: Side,
    part: Bounds<Pixels>,
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
    let title_color = step.mode().map_or(theme.foreground, |_| accent);
    let surface = theme.popover;
    // A small square, half beneath the card's edge, toward the part.
    let pointer = {
        let along_x = (part.center().x - position.x - POINTER / 2.)
            .max(px(12.))
            .min(CALLOUT_WIDTH - px(12.) - POINTER);
        let along_y = (part.center().y - position.y - POINTER / 2.).max(px(12.)).min(px(60.));
        let nub = div()
            .absolute()
            .size(POINTER)
            .bg(surface)
            .border_color(theme.border);
        match side {
            Side::Below => Some(nub.top(-POINTER / 2.).left(along_x).border_t_1().border_l_1()),
            Side::Above => Some(nub.bottom(-POINTER / 2.).left(along_x).border_b_1().border_r_1()),
            Side::Right => Some(nub.left(-POINTER / 2.).top(along_y).border_l_1().border_b_1()),
            Side::Left => Some(nub.right(-POINTER / 2.).top(along_y).border_r_1().border_t_1()),
            Side::Over => None,
        }
    };
    let card = v_flex()
        .id("walkthrough-callout")
        .absolute()
        .left(position.x)
        .top(position.y)
        .max_w(CALLOUT_WIDTH)
        .w(CALLOUT_WIDTH)
        .p_4()
        .gap_3()
        .rounded(px(8.))
        .border_1()
        .border_color(theme.border)
        .bg(surface)
        .text_color(theme.popover_foreground)
        .shadow_lg()
        .occlude()
        .children(pointer)
        .child(
            div()
                .font_semibold()
                .text_color(title_color)
                .child(step.title()),
        )
        .child(div().text_sm().child(step.text()))
        .children(use_open_project.map(|use_open| {
            Button::new("walkthrough-use-open-project")
                .small()
                .label("Use the open project")
                .on_click(move |_, window, cx| use_open(window, cx))
        }))
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!("Step {} of {count}", step.index() + 1)),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("walkthrough-skip")
                        .link()
                        .small()
                        .label("Skip tour")
                        .on_click(move |_, window, cx| skip(window, cx)),
                )
                .child(div().flex_1())
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

    use super::{Side, Step, Tour, place};

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
        let tab = Bounds::new(point(px(100.), px(10.)), size(px(60.), px(24.)));
        let (at, side) = place(tab, window);
        assert_eq!(side, Side::Below);
        assert!(at.y > px(34.));
        let bottom = Bounds::new(point(px(100.), px(700.)), size(px(400.), px(60.)));
        assert_eq!(place(bottom, window).1, Side::Above);
        // Never off the window.
        let corner = Bounds::new(point(px(1190.), px(10.)), size(px(10.), px(10.)));
        let (at, _) = place(corner, window);
        assert!(at.x + px(320.) <= px(1200.));
        let panel = Bounds::new(point(px(32.), px(32.)), size(px(1136.), px(736.)));
        assert_eq!(place(panel, window).1, Side::Over);
    }
}
