//! The ribbon along the top of the main window: tabs, with the selected tab's
//! commands beneath them in labelled groups. Each tab is a module of its own,
//! which says which commands it holds, and renders and runs them.
//!
//! Following the usual ribbon conventions: main commands are icons above
//! their labels, tooltips explain rather than repeat the label and give
//! any shortcut, a command that can't run is disabled rather than hidden,
//! groups are set apart by dividers named for them, and the ribbon collapses by
//! double-clicking a tab or Ctrl/Cmd+F1, which is remembered
//! across launches. Collapsed, the tabs go too, and the
//! commands marked primary (for now, all of them) sit small in a single row.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonRounded, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, Sizable as _, Size, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::activity::{Job, RevealJob};
use crate::project_directory::ProjectDirectory;
use crate::project_indicator::ProjectIndicator;

mod application_tab;
mod code_tab;
mod project_tab;
mod spec_tab;

pub use application_tab::set_dark_mode;

actions!(
    suspense,
    [
        ToggleRibbon,
        ShowTab1,
        ShowTab2,
        ShowTab3,
        ShowTab4,
        RunPrimary,
        RunRelease
    ]
);

/// The height of the row of tabs, which the collapsed ribbon's row keeps.
const TAB_ROW_HEIGHT: Pixels = px(32.);

/// The room either side of a tab's label, which is all a tab is.
const TAB_PADDING: Pixels = px(8.);

/// The padding around the commands, on every side.
const BODY_PADDING: Pixels = px(8.);

/// The gap between one command and the next, across and down, and between a
/// group's buttons and the divider beside them.
const GAP: Pixels = px(4.);

/// How wide the divider between two groups is.
const DIVIDER_WIDTH: Pixels = px(4.);

/// A slim button's height.
const SLIM_HEIGHT: Pixels = px(27.);

/// A small button's height, in the collapsed ribbon's row.
const SMALL_HEIGHT: Pixels = px(22.);

/// Slim buttons stacked in one column, at most.
const SLIM_STACK: usize = 2;

/// How tall a column of `slim` slim buttons is, with the gaps between them.
fn stacked(slim: usize) -> Pixels {
    SLIM_HEIGHT * slim as f32 + GAP * (slim as f32 - 1.)
}

/// A full button is exactly two slim buttons tall, however much its icon and
/// label would take.
fn full_height() -> Pixels {
    stacked(SLIM_STACK)
}

/// Ctrl+F1 (Cmd+F1 on macOS) collapses or expands the ribbon, as in Office.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-f1", ToggleRibbon, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-f1", ToggleRibbon, None),
        // Ctrl+F5 (Cmd+F5 on macOS) runs the project's primary run target.
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-f5", RunPrimary, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-f5", RunPrimary, None),
        // With Shift, the release build.
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-shift-f5", RunRelease, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-f5", RunRelease, None),
        // Alt+1 to Alt+4 open the tabs in the order they are shown.
        KeyBinding::new("alt-1", ShowTab1, None),
        KeyBinding::new("alt-2", ShowTab2, None),
        KeyBinding::new("alt-3", ShowTab3, None),
        KeyBinding::new("alt-4", ShowTab4, None),
    ]);
}

/// A tab of the ribbon.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RibbonTab {
    Project,
    Code,
    Spec,
    Application,
}

impl RibbonTab {
    /// Every tab, in order.
    pub const ALL: [RibbonTab; 4] = [
        RibbonTab::Project,
        RibbonTab::Code,
        RibbonTab::Spec,
        RibbonTab::Application,
    ];

    fn label(self) -> &'static str {
        match self {
            RibbonTab::Project => "Project",
            RibbonTab::Code => "Code",
            RibbonTab::Spec => "Spec",
            RibbonTab::Application => "Application",
        }
    }

    /// The chat input mode the tab works on, whose colour it takes while
    /// open: Code and Spec have one; the others don't.
    fn mode(self) -> Option<crate::chat_input::SendMode> {
        match self {
            RibbonTab::Code => Some(crate::chat_input::SendMode::Code),
            RibbonTab::Spec => Some(crate::chat_input::SendMode::Spec),
            RibbonTab::Project | RibbonTab::Application => None,
        }
    }

    /// The tab's commands, in the order of its groups.
    fn commands(self, cx: &App) -> Vec<CommandPlace> {
        match self {
            RibbonTab::Project => project_tab::COMMANDS.to_vec(),
            RibbonTab::Code => code_tab::commands(cx),
            RibbonTab::Spec => spec_tab::COMMANDS.to_vec(),
            RibbonTab::Application => application_tab::COMMANDS.to_vec(),
        }
    }
}

/// A command in the ribbon, from whichever tab holds it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Command {
    NewProject,
    OpenProject,
    BuildSpec,
    NewScope,
    NewConcept,
    NewShape,
    NewInstruction,
    AnalyzeDivergence,
    ViewDivergenceReports,
    GenerateSkills,
    Rescope,
    FindHowToRun,
    RunTarget(usize),
    FindRunAgain,
    Brightness,
    ResetBrightness,
    Theme,
    Settings,
    Welcome,
    Walkthrough,
}

/// Where a command sits in its tab, and whether it stays in the collapsed
/// ribbon.
#[derive(Clone, Copy)]
struct CommandPlace {
    command: Command,
    group: &'static str,
    /// Full or slim.
    size: CommandSize,
    /// Shown, small, in the collapsed ribbon.
    primary: bool,
}

/// How a command's button is laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommandSize {
    /// Two slim buttons stacked tall, its small icon above its label.
    Full,
    /// Near enough half of that, its small icon left of its label; slim
    /// buttons side by side stack down, two to a column.
    Slim,
    /// Shorter than slim and only as wide as its icon and label, for the
    /// collapsed ribbon's row.
    Small,
}

pub struct Ribbon {
    /// The open tabs, in the order of the tabs; never empty. A click opens one
    /// alone, and Ctrl/Cmd+click opens or closes one alongside the others.
    open_tabs: Vec<RibbonTab>,
    /// Collapsed to a row of its primary commands; the choice is kept across
    /// launches.
    collapsed: bool,
    /// The projects a spec build runs in, on its own.
    building: Vec<std::path::PathBuf>,
    /// Everything running, shown as a spinner beside the project's name.
    jobs: Vec<Job>,
    /// Whether the list of what's running is open.
    jobs_open: bool,
    /// Which project is open, and the recent projects when clicked.
    project_indicator: Entity<ProjectIndicator>,
    /// The Application tab's Brightness slider, made for the mode showing.
    brightness: Option<application_tab::BrightnessSlider>,
}

impl EventEmitter<RevealJob> for Ribbon {}
impl EventEmitter<RunCommand> for Ribbon {}

/// Emitted by the Code tab's run commands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RunCommand {
    /// Find how the project is run.
    Find,
    /// Run the project's target at this index.
    Target(usize),
}

impl Ribbon {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.observe_global::<ProjectDirectory>(|_, cx| cx.notify())
            .detach();
        cx.observe_global::<crate::run_targets::ProjectTargets>(|_, cx| cx.notify())
            .detach();
        Self {
            // Always Project at launch: which tab was last open isn't kept.
            open_tabs: vec![RibbonTab::Project],
            collapsed: collapsed_preference::load(),
            building: Vec::new(),
            jobs: Vec::new(),
            jobs_open: false,
            project_indicator: cx.new(ProjectIndicator::new),
            brightness: None,
        }
    }

    /// Everything running, as the spinner and its list show it. The list
    /// closes once nothing is.
    pub fn set_jobs(&mut self, jobs: Vec<Job>, cx: &mut Context<Self>) {
        if self.jobs == jobs {
            return;
        }
        self.jobs = jobs;
        if self.jobs.is_empty() {
            self.jobs_open = false;
        }
        cx.notify();
    }

    #[cfg(test)]
    pub fn jobs(&self) -> &[Job] {
        &self.jobs
    }

    /// The project indicator.
    pub fn project_indicator(&self) -> &Entity<ProjectIndicator> {
        &self.project_indicator
    }

    pub fn jobs_open(&self) -> bool {
        self.jobs_open
    }

    pub fn close_jobs(&mut self, cx: &mut Context<Self>) {
        if self.jobs_open {
            self.jobs_open = false;
            cx.notify();
        }
    }

    /// The project indicator, a solid block the full height of the row, with
    /// no line between it and what follows. Lets UI tests find it; inert in
    /// normal builds.
    fn render_indicator(&self) -> impl IntoElement {
        gpui_kit::TestSupportExt::test_support(
            div()
                .id("ribbon-prefix")
                .flex()
                .flex_none()
                .h_full()
                .child(self.project_indicator.clone()),
        )
    }

    /// The left container: the project indicator, leading the bar whether
    /// expanded or collapsed.
    fn render_left(&self) -> Option<AnyElement> {
        container(
            "ribbon-left",
            vec![self.render_indicator().into_any_element()],
        )
    }

    /// The right container, ending the bar: the activity spinner while
    /// anything is running, and nothing otherwise.
    fn render_right(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        container(
            "ribbon-right",
            self.render_activity(cx).into_iter().collect(),
        )
    }

    /// The spinner beside the project's name while anything is running, with
    /// how many things are when more than one is; clicking it opens the list
    /// of them, and clicking one reveals it.
    fn render_activity(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.jobs.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let count = self.jobs.len();
        let tooltip: SharedString = if count == 1 {
            let job = &self.jobs[0];
            match &job.detail {
                Some(detail) => format!("{}: {detail}", job.title).into(),
                None => job.title.clone(),
            }
        } else {
            format!("{count} things running").into()
        };
        let list = self.jobs_open.then(|| {
            let rows = self.jobs.iter().enumerate().map(|(ix, job)| {
                let (kind, project) = (job.kind, job.project.clone());
                let row = h_flex()
                    .id(("ribbon-job", ix))
                    .gap_2()
                    .px_3()
                    .py_1p5()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .hover(|row| row.bg(theme.list_hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.jobs_open = false;
                        cx.emit(RevealJob(kind, project.clone()));
                        cx.notify();
                    }))
                    .child(gpui_kit::component::spinner::Spinner::new().small())
                    .child(div().flex_none().font_medium().child(job.title.clone()))
                    .children(job.detail.clone().map(|detail| {
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .child(detail)
                    }));
                // Lets UI tests find the row; inert in normal builds.
                gpui_kit::TestSupportExt::test_support(row)
            });
            let list = v_flex()
                .id("ribbon-jobs")
                .w(px(360.))
                .mt(px(28.))
                .p_1()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .shadow_md()
                .occlude()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_jobs(cx)))
                .children(rows);
            // Lets UI tests find the list; inert in normal builds.
            deferred(
                anchored()
                    .snap_to_window()
                    .child(gpui_kit::TestSupportExt::test_support(list)),
            )
            .with_priority(2)
        });
        let spinner = h_flex()
            .id("ribbon-activity")
            .flex_none()
            .gap_1()
            .px_1()
            .py_0p5()
            .rounded(theme.radius)
            .cursor_pointer()
            .hover(|spinner| spinner.bg(theme.list_hover))
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.jobs_open = !this.jobs_open;
                cx.notify();
            }))
            .child(gpui_kit::component::spinner::Spinner::new().small())
            .when(count > 1, |spinner| {
                spinner.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(count.to_string()),
                )
            })
            .children(list);
        // Lets UI tests find the spinner; inert in normal builds.
        Some(gpui_kit::TestSupportExt::test_support(spinner).into_any_element())
    }

    /// Whether the ribbon is collapsed to its primary commands.
    #[cfg(test)]
    pub fn is_collapsed(&self) -> bool {
        self.collapsed
    }

    /// Collapses the ribbon to its primary commands, or expands it again,
    /// remembering the choice.
    pub fn toggle_collapsed(&mut self, cx: &mut Context<Self>) {
        self.collapsed = !self.collapsed;
        collapsed_preference::save(self.collapsed);
        cx.notify();
    }

    /// A tab was clicked: it opens alone, and a double-click collapses the
    /// ribbon; with `add`, from Ctrl/Cmd+click, it opens alongside the others,
    /// or closes if it was open, unless it is the only one. (Collapsed, there
    /// are no tabs to click.)
    pub(crate) fn tab_clicked(
        &mut self,
        tab: RibbonTab,
        clicks: usize,
        add: bool,
        cx: &mut Context<Self>,
    ) {
        if add {
            self.open_tabs = toggle_open(&self.open_tabs, tab);
        } else {
            self.open_tabs = vec![tab];
            if clicks >= 2 && !self.collapsed {
                self.toggle_collapsed(cx);
            }
        }
        cx.notify();
    }

    /// Opens the tab at `ix`, counting from 0 in the order the tabs are shown,
    /// alone, as a click does, expanding the ribbon if it is collapsed. An
    /// index past the last tab does nothing.
    pub fn show_tab_at(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(&tab) = RibbonTab::ALL.get(ix) else {
            return;
        };
        if self.collapsed {
            self.toggle_collapsed(cx);
        }
        self.select_tab(tab, cx);
    }

    /// Opens `tab` alone, expanding the ribbon if it is collapsed, as the
    /// walkthrough shows a tab.
    pub fn show_tab(&mut self, tab: RibbonTab, cx: &mut Context<Self>) {
        if self.collapsed {
            self.toggle_collapsed(cx);
        }
        self.select_tab(tab, cx);
    }

    /// Opens `tab` alone.
    pub fn select_tab(&mut self, tab: RibbonTab, cx: &mut Context<Self>) {
        self.open_tabs = vec![tab];
        cx.notify();
    }

    #[cfg(test)]
    pub fn open_tabs(&self) -> &[RibbonTab] {
        &self.open_tabs
    }

    /// Where the Brightness slider's thumb is among the appearances, once
    /// shown.
    #[cfg(test)]
    pub fn brightness_slider(&self, cx: &App) -> Option<f32> {
        self.brightness.as_ref().map(|slider| slider.shown(cx))
    }
}

impl Ribbon {
    /// `command`'s control: large, with its icon above its label, or small,
    /// with its icon beside it, for the collapsed ribbon.
    fn render_command(
        &self,
        command: Command,
        size: CommandSize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = command_colors(cx);
        let rest = command_shades(cx).0;
        let button_with = |id: ElementId, icon: IconName, label: SharedString| {
            // gpui-kit keeps only a fifth of a custom button's resting colour,
            // so the colour is given to the button itself; its hover and
            // pressed colours are taken whole.
            let button = Button::new(id)
                .custom(colors)
                .rounded(ButtonRounded::None)
                .bg(rest);
            match size {
                // Slim fills its column; small, in a row, never stretches.
                CommandSize::Slim | CommandSize::Small => button
                    .small()
                    .map(|button| match size {
                        CommandSize::Small => button.h(SMALL_HEIGHT).flex_none(),
                        _ => button.h(SLIM_HEIGHT).w_full(),
                    })
                    .px_2()
                    .icon(Icon::new(icon).with_size(Size::Small))
                    .label(label),
                // Its small icon centred above its label, the pair centred in
                // the two slim buttons' height its column gives it, rather
                // than the height a button takes by default.
                CommandSize::Full => button.flex_none().h_auto().px_2().child(
                    v_flex()
                        .items_center()
                        .gap_1p5()
                        .child(Icon::new(icon).with_size(Size::Small))
                        .child(div().text_sm().child(label)),
                ),
            }
        };
        let button = |id: &'static str, icon: IconName, label: SharedString| {
            button_with(id.into(), icon, label)
        };
        match command {
            Command::FindHowToRun => code_tab::find_how_to_run(&button_with, cx),
            Command::FindRunAgain => code_tab::find_again(&button_with, cx),
            Command::RunTarget(ix) => code_tab::run_target(ix, &button_with, cx),
            // The walkthrough points at New Project.
            Command::NewProject => div()
                .flex()
                .flex_none()
                .child(crate::walkthrough::mark(
                    crate::walkthrough::Target::NewProject,
                ))
                .child(project_tab::new_project(button))
                .into_any_element(),
            Command::OpenProject => project_tab::open_project(button),
            Command::BuildSpec => spec_tab::build_spec(self, button, cx),
            Command::NewScope => spec_tab::new_scope(button, cx),
            Command::NewConcept => spec_tab::new_concept(button, cx),
            Command::NewShape => spec_tab::new_shape(button, cx),
            Command::NewInstruction => spec_tab::new_instruction(button, cx),
            Command::AnalyzeDivergence => spec_tab::analyze_divergence(button, cx),
            Command::ViewDivergenceReports => spec_tab::view_divergence_reports(button, cx),
            Command::GenerateSkills => spec_tab::generate_skills(button, cx),
            Command::Rescope => spec_tab::rescope(button, cx),
            Command::Brightness => application_tab::brightness(self, size, cx),
            Command::ResetBrightness => application_tab::reset_brightness_button(button, cx),
            Command::Theme => application_tab::theme(button),
            Command::Settings => application_tab::settings(button),
            Command::Welcome => application_tab::welcome(button),
            Command::Walkthrough => application_tab::walkthrough(button),
        }
    }
}

/// A container at either end of the bar: what it holds side by side,
/// vertically centred, with no gap between them or padding around them,
/// only as wide as they are; nothing at all while it holds nothing. As tall
/// as the tab row, which the collapsed ribbon's row keeps, so it is the same
/// height, its contents in the same place, whether the ribbon is expanded or
/// collapsed. Lets UI tests find it; inert in normal builds.
fn container(id: &'static str, items: Vec<AnyElement>) -> Option<AnyElement> {
    (!items.is_empty()).then(|| {
        gpui_kit::TestSupportExt::test_support(h_flex().id(id))
            .flex_none()
            .h(TAB_ROW_HEIGHT)
            .items_center()
            .children(items)
            .into_any_element()
    })
}

impl Render for Ribbon {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The Brightness slider shows the appearance showing.
        application_tab::sync_brightness(self, window, cx);
        let theme = cx.theme();
        let (border, muted, foreground) = (theme.border, theme.muted_foreground, theme.foreground);
        let (area, tab_row) = ribbon_colors(cx);

        let ribbon = div()
            .id("ribbon")
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(border);

        if self.collapsed {
            // No tabs: every primary command, small, in one row, a divider
            // between one tab's commands and the next, led by the project's
            // name, as beside the tabs.
            let mut row = Vec::new();
            let mut last_tab = None;
            let primary: Vec<_> = RibbonTab::ALL
                .into_iter()
                .flat_map(|tab| {
                    tab.commands(cx)
                        .into_iter()
                        .filter(|place| place.primary)
                        .map(move |place| (tab, place))
                })
                .collect();
            for (tab, place) in primary {
                if last_tab.is_some_and(|last| last != tab) {
                    // Centred like the controls, rather than stretched.
                    row.push(
                        div()
                            .flex_none()
                            .w_px()
                            .h(px(16.))
                            .bg(border)
                            .into_any_element(),
                    );
                }
                last_tab = Some(tab);
                row.push(self.render_command(place.command, CommandSize::Small, cx));
            }
            // Lets UI tests find the row; inert in normal builds. The
            // indicator stays put; what follows them
            // scrolls sideways when it's wider than the room left. It holds
            // commands, so it is the command area's colour, and its buttons
            // stand apart from it as they do there.
            return ribbon.child(gpui_kit::TestSupportExt::test_support(
                h_flex()
                    .id("ribbon-primary")
                    // As tall as the tab row, so collapsing moves nothing.
                    .h(TAB_ROW_HEIGHT)
                    .items_stretch()
                    .bg(area)
                    .children(self.render_left())
                    .child(gpui_kit::TestSupportExt::test_support(
                        h_flex()
                            .id("ribbon-primary-commands")
                            .flex_1()
                            .min_w_0()
                            .overflow_x_scroll()
                            // Every control is vertically centred in the row.
                            .items_center()
                            .gap_2()
                            .px_2()
                            .children(row),
                    ))
                    .children(self.render_right(cx)),
            ));
        }

        // Flat tabs, each only its label with room either side, and no
        // border, corner, or line anywhere. A closed tab has no background of
        // its own, showing the tab row's; an open one takes the command
        // area's, so it and the commands beneath read as one piece. Each
        // handles its own clicks, so a double-click can be told apart: it
        // collapses the ribbon.
        let hover = tab_hover(cx);
        let tabs = RibbonTab::ALL
            .into_iter()
            .enumerate()
            .map(|(ix, tab)| {
                let open = self.open_tabs.contains(&tab);
                let tint = tab
                    .mode()
                    .map(|mode| crate::chat_input::mode_tint(mode, cx));
                // A tab with a mode's colour shows it in its label, the hue at
                // full strength, open or closed; no tab has a coloured
                // background.
                let label = tint.map_or(foreground, |tint| Hsla { a: 1., ..tint });
                // Lets UI tests find the tab; inert in normal builds.
                gpui_kit::TestSupportExt::test_support(div().id(("ribbon-tab", ix)))
                    .relative()
                    .flex()
                    .flex_none()
                    .items_center()
                    .h_full()
                    .px(TAB_PADDING)
                    .text_sm()
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .role(Role::Tab)
                    .aria_label(tab.label())
                    .aria_selected(open)
                    .when(open, |this| this.bg(area))
                    .when(!open, |this| this.hover(|this| this.bg(hover)))
                    // The walkthrough points at the Project tab.
                    .when(tab == RibbonTab::Project, |this| {
                        this.child(crate::walkthrough::mark(
                            crate::walkthrough::Target::ProjectTab,
                        ))
                    })
                    .child(div().relative().text_color(label).child(tab.label()))
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        this.tab_clicked(
                            tab,
                            event.click_count(),
                            event.modifiers().secondary(),
                            cx,
                        )
                    }))
            })
            .collect::<Vec<_>>();
        // The left container, holding the open project's name, left of the
        // tabs, and the right container, holding the activity spinner, at the
        // far right, all on the tab row's own background.
        let tab_bar = h_flex()
            .w_full()
            .h(TAB_ROW_HEIGHT)
            .overflow_hidden()
            .items_start()
            .bg(tab_row)
            .children(self.render_left())
            .child(
                h_flex()
                    .id("ribbon-tabs")
                    .role(Role::TabList)
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .children(tabs),
            )
            .children(self.render_right(cx));

        // Each open tab's commands, gathered into their groups in order.
        let mut groups: Vec<(&'static str, Vec<(CommandSize, AnyElement)>)> = Vec::new();
        for tab in &self.open_tabs {
            let start = groups.len();
            for place in tab.commands(cx) {
                let element = (
                    place.size,
                    self.render_command(place.command, place.size, cx),
                );
                match groups[start..].last_mut() {
                    Some((group, commands)) if *group == place.group => commands.push(element),
                    _ => groups.push((place.group, vec![element])),
                }
            }
        }
        // A divider between one group and the next, within a tab or between
        // tabs alike, named for the group it starts.
        let mut row = Vec::new();
        for (ix, (label, commands)) in groups.into_iter().enumerate() {
            if ix > 0 {
                row.push(divider(ix, label, tab_row));
            }
            row.push(group(label, commands));
        }
        if row.is_empty() {
            row.push(
                div()
                    .flex()
                    .items_center()
                    .text_sm()
                    .text_color(muted)
                    .child("No commands yet")
                    .into_any_element(),
            );
        }

        // Lets UI tests find the tabs and the commands; inert in normal builds.
        let tab_bar =
            gpui_kit::TestSupportExt::test_support(div().id("ribbon-tabs-row").child(tab_bar));
        ribbon
            .child(tab_bar)
            .child(gpui_kit::TestSupportExt::test_support(
                div()
                    .id("ribbon-controls")
                    .flex()
                    .flex_row()
                    // Scrolls sideways when the open tabs' groups are wider
                    // than the window.
                    .overflow_x_scroll()
                    // The dividers run the buttons' full height.
                    .items_stretch()
                    .p(BODY_PADDING)
                    .gap(GAP)
                    // No taller than its commands need.
                    .flex_none()
                    .bg(area)
                    .children(row),
            ))
    }
}

/// The open tabs once `tab` is Ctrl/Cmd+clicked: opened alongside `open`, or
/// closed if it was open and isn't the only one; always in the order of the
/// tabs.
pub fn toggle_open(open: &[RibbonTab], tab: RibbonTab) -> Vec<RibbonTab> {
    let was_open = open.contains(&tab);
    if was_open && open.len() == 1 {
        return open.to_vec();
    }
    RibbonTab::ALL
        .into_iter()
        .filter(|candidate| {
            if *candidate == tab {
                !was_open
            } else {
                open.contains(candidate)
            }
        })
        .collect()
}

/// A group of related commands, named for assistive technology though not
/// titled on screen.
fn group(label: &'static str, commands: Vec<(CommandSize, AnyElement)>) -> AnyElement {
    // Full buttons stand alone; slim ones side by side stack down, two to a
    // column, top aligned.
    let mut columns: Vec<AnyElement> = Vec::new();
    let mut stack: Vec<AnyElement> = Vec::new();
    let flush = |stack: &mut Vec<AnyElement>, columns: &mut Vec<AnyElement>| {
        if !stack.is_empty() {
            columns.push(
                v_flex()
                    .flex_none()
                    .gap(GAP)
                    .children(stack.drain(..))
                    .into_any_element(),
            );
        }
    };
    for (size, element) in commands {
        match size {
            CommandSize::Full => {
                flush(&mut stack, &mut columns);
                // Exactly two slim buttons tall, whatever its content; the
                // button itself fills that.
                columns.push(
                    h_flex()
                        .flex_none()
                        .items_stretch()
                        .overflow_hidden()
                        .h(full_height())
                        .child(element)
                        .into_any_element(),
                );
            }
            CommandSize::Slim | CommandSize::Small => {
                if stack.len() == SLIM_STACK {
                    flush(&mut stack, &mut columns);
                }
                stack.push(element);
            }
        }
    }
    flush(&mut stack, &mut columns);
    // Each command takes the height it needs, a full button two slim buttons,
    // all from the top. Lets UI tests find the group; inert in normal builds.
    gpui_kit::TestSupportExt::test_support(h_flex().id(label))
        .flex_none()
        .items_start()
        .gap(GAP)
        .role(Role::Group)
        .aria_label(label)
        .children(columns)
        .into_any_element()
}

/// The divider starting the group `label`, the `ix`th group of the open tabs:
/// a bar in the tab row's colour, as tall as the buttons, its ends softly
/// rounded, with the group's name as its tooltip. Lets UI tests find it;
/// inert in normal builds.
fn divider(ix: usize, label: &'static str, color: Hsla) -> AnyElement {
    gpui_kit::TestSupportExt::test_support(div().id(("ribbon-divider", ix)))
        .flex_none()
        .w(DIVIDER_WIDTH)
        .rounded(DIVIDER_WIDTH / 2.)
        .bg(color)
        .role(Role::Splitter)
        .aria_label(label)
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(label).build(window, cx)
        })
        .into_any_element()
}

/// The ribbon's command area, and the tab row above it, in the mode showing.
fn ribbon_colors(cx: &App) -> (Hsla, Hsla) {
    let palette = crate::theme::palette(cx);
    (
        crate::theme::color(palette.ribbon),
        crate::theme::color(palette.ribbon_tabs),
    )
}

/// A closed tab under the pointer: a faint step lighter in dark mode, darker
/// in light mode, laid over the tab row.
fn tab_hover(cx: &App) -> Hsla {
    if cx.theme().is_dark() {
        gpui_kit::white().opacity(0.06)
    } else {
        gpui_kit::black().opacity(0.06)
    }
}

/// A ribbon command's background at rest, hovered, and pressed, laid over the
/// command area. At rest in dark mode, white laid just far enough over the
/// area to reach the base, #444444 on #222222; in light mode, not quite half
/// way.
pub(crate) fn command_shades(cx: &App) -> (Hsla, Hsla, Hsla) {
    let (white, black) = (hsla(0., 0., 1., 1.), hsla(0., 0., 0., 1.));
    if cx.theme().is_dark() {
        let palette = crate::theme::palette(cx);
        let channel = |color: u32| (color & 0xff) as f32;
        let rest =
            (channel(palette.base) - channel(palette.ribbon)) / (255. - channel(palette.ribbon));
        (
            white.opacity(rest),
            white.opacity(rest + 0.08),
            black.opacity(0.2),
        )
    } else {
        (white.opacity(0.45), white.opacity(0.7), black.opacity(0.12))
    }
}

/// The colours of a ribbon command: its background laid over the body rather
/// than a colour of its own, so it contrasts a little with the body whatever
/// tint the body has, more while hovered, and more again while pressed.
pub(crate) fn command_colors(cx: &App) -> ButtonCustomVariant {
    let theme = cx.theme();
    let (rest, hover, active) = command_shades(cx);
    ButtonCustomVariant::new(cx)
        .color(rest)
        .hover(hover)
        .active(active)
        .foreground(theme.foreground)
}

/// Whether the user collapsed the ribbon, saved in the platform's per-user
/// config directory beside the light or dark mode choice. Tests neither read
/// nor write it.
mod collapsed_preference {
    #[cfg(not(test))]
    fn file() -> Option<std::path::PathBuf> {
        Some(dirs::config_dir()?.join("suspense").join("ribbon"))
    }

    pub fn load() -> bool {
        #[cfg(not(test))]
        if let Some(file) = file() {
            return std::fs::read_to_string(file).is_ok_and(|text| text.trim() == "collapsed");
        }
        false
    }

    /// Saves the choice; it is only a convenience, so failing to is ignored.
    pub fn save(collapsed: bool) {
        #[cfg(not(test))]
        if let Some(file) = file() {
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir).ok();
            }
            let state = if collapsed { "collapsed" } else { "expanded" };
            std::fs::write(file, state).ok();
        }
        #[cfg(test)]
        let _ = collapsed;
    }
}

#[cfg(test)]
mod tests {
    use super::RibbonTab::{Application, Code, Project, Spec};
    use super::toggle_open;

    /// Ctrl/Cmd+click opens a tab alongside the others, in the order of the
    /// tabs, or closes it, but never the last one open.
    #[test]
    fn ctrl_click_opens_and_closes_tabs_alongside_others() {
        assert_eq!(toggle_open(&[Spec], Project), [Project, Spec]);
        assert_eq!(
            toggle_open(&[Project, Spec], Application),
            [Project, Spec, Application]
        );
        assert_eq!(toggle_open(&[Project, Code, Spec], Code), [Project, Spec]);
        assert_eq!(toggle_open(&[Spec], Spec), [Spec]);
    }
}

#[cfg(test)]
mod layout_tests {
    use gpui_kit::{AppContext as _, TestAppContext, div};

    use super::{CommandSize, group};

    /// A full command is two slim buttons tall, whether its content needs less
    /// or far more.
    #[gpui_kit::test]
    fn a_full_button_is_two_slim_buttons_tall(cx: &mut TestAppContext) {
        use gpui_kit::component::Root;
        use gpui_kit::test::TestWindowExt as _;
        use gpui_kit::{
            Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _,
            Window, px,
        };

        struct View;
        impl Render for View {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                // A command with hardly any content, and one with far more
                // than two slim buttons' worth.
                let full = |id: &'static str, content: gpui_kit::Pixels| {
                    (
                        CommandSize::Full,
                        gpui_kit::TestSupportExt::test_support(
                            div().id(id).w(px(60.)).child(div().h(content)),
                        )
                        .into_any_element(),
                    )
                };
                // Each in a row of its own, so neither stretches the other.
                let row = |id: &'static str, content: gpui_kit::Pixels| {
                    div().flex().child(group("Group", vec![full(id, content)]))
                };
                div()
                    .flex()
                    .flex_col()
                    .child(row("small", px(4.)))
                    .child(row("large", px(200.)))
            }
        }
        cx.update(gpui_kit::init);
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|_| View);
            Root::new(view, window, cx)
        });
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("small").bounds().size.height,
                super::stacked(2),
                "a short command isn't two slim buttons tall"
            );
            assert_eq!(
                window.find("large").bounds().size.height,
                super::stacked(2),
                "a tall command isn't held to two slim buttons"
            );
        })
        .unwrap();
    }

    /// Three slim buttons in a row stack into a column of two, 27px tall and
    /// 4px apart, then a column of one, 4px along; a full button between slim
    /// ones stands alone, 58px tall.
    #[gpui_kit::test]
    fn slim_buttons_stack_two_to_a_column(cx: &mut TestAppContext) {
        use gpui_kit::component::Root;
        use gpui_kit::test::TestWindowExt as _;
        use gpui_kit::{
            Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _,
            Window, px,
        };

        struct View;
        impl Render for View {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let slim = |id: &'static str| {
                    (
                        CommandSize::Slim,
                        gpui_kit::TestSupportExt::test_support(
                            div().id(id).h(super::SLIM_HEIGHT).w(px(60.)),
                        )
                        .into_any_element(),
                    )
                };
                let full = |id: &'static str| {
                    (
                        CommandSize::Full,
                        // Content of its own, but no height of its own: it
                        // grows to its group's, as a full button does.
                        gpui_kit::TestSupportExt::test_support(
                            div().id(id).w(px(60.)).child(div().h(px(44.))),
                        )
                        .into_any_element(),
                    )
                };
                // No height of its own: the group is as tall as its commands.
                div().flex().child(group(
                    "Group",
                    vec![slim("a"), slim("b"), slim("d"), full("e"), slim("f")],
                ))
            }
        }
        cx.update(gpui_kit::init);
        let window = cx.add_window(|window, cx| {
            let view = cx.new(|_| View);
            Root::new(view, window, cx)
        });
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            let at = |id: &'static str| window.find(id).bounds();
            let (a, b, d, e, f) = (at("a"), at("b"), at("d"), at("e"), at("f"));
            let gap = super::GAP;
            assert_eq!(gap, px(4.));
            assert_eq!(a.size.height, px(27.), "a slim button isn't 27px tall");
            assert_eq!(b.left(), a.left(), "a, b aren't one column");
            assert_eq!(b.top() - a.bottom(), gap);
            assert_eq!(d.top(), a.top(), "the third doesn't start a new column");
            assert_eq!(d.left() - a.right(), gap);
            assert_eq!(e.left() - d.right(), gap);
            // A full button is two slim buttons and the gap between them:
            // 58px.
            assert_eq!(
                (e.size.height, e.top()),
                (px(58.), a.top()),
                "the full button isn't two slim buttons tall, from the top"
            );
            assert_eq!((f.top(), f.left() - e.right()), (a.top(), gap));
        })
        .unwrap();
    }
}
