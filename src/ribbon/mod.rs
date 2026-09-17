//! The ribbon along the top of the main window: tabs, with the selected tab's
//! commands beneath them in labelled groups. Each tab is a module of its own,
//! which says which commands it holds, and renders and runs them.
//!
//! Following the usual ribbon conventions: main commands are large icons
//! above their labels, tooltips explain rather than repeat the label and give
//! any shortcut, a command that can't run is disabled rather than hidden,
//! groups are titled down their left edge, and the ribbon collapses by
//! double-clicking a tab, its chevron, or Ctrl/Cmd+F1, which is remembered
//! across launches. Collapsed, the tabs go too, and the
//! commands marked primary (for now, all of them) sit small in a single row.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme, Icon, Selectable as _, Sizable as _, Size, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::activity::{Job, RevealJob};
use crate::project_directory::ProjectDirectory;
use crate::project_indicator::ProjectIndicator;

mod application_tab;
mod code_tab;
mod project_tab;
mod research_tab;
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
        ShowTab5
    ]
);

/// The height of the row of tabs: gpui-kit's tab bar, less the line along its
/// bottom and a pixel more, so that at a fractional display scale, where the
/// clip falls between device pixels, no softened edge of that line shows as a
/// faint seam between the tabs and the commands beneath them.
const TAB_ROW_HEIGHT: Pixels = px(30.);

/// The height of the commands beneath the tabs.
const RIBBON_BODY_HEIGHT: Pixels = px(76.);

/// The width of the strip down a group's left edge that holds its title,
/// leaving room either side of the title.
const GROUP_TITLE_WIDTH: Pixels = px(28.);

/// The size of a group's title.
const GROUP_TITLE_SIZE: f32 = 12.;

/// Ctrl+F1 (Cmd+F1 on macOS) collapses or expands the ribbon, as in Office.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-f1", ToggleRibbon, None),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-f1", ToggleRibbon, None),
        // Alt+1 to Alt+5 open the tabs in the order they are shown.
        KeyBinding::new("alt-1", ShowTab1, None),
        KeyBinding::new("alt-2", ShowTab2, None),
        KeyBinding::new("alt-3", ShowTab3, None),
        KeyBinding::new("alt-4", ShowTab4, None),
        KeyBinding::new("alt-5", ShowTab5, None),
    ]);
}

/// A tab of the ribbon.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RibbonTab {
    Project,
    Code,
    Spec,
    Research,
    Application,
}

impl RibbonTab {
    /// Every tab, in order.
    pub const ALL: [RibbonTab; 5] = [
        RibbonTab::Project,
        RibbonTab::Code,
        RibbonTab::Spec,
        RibbonTab::Research,
        RibbonTab::Application,
    ];

    fn label(self) -> &'static str {
        match self {
            RibbonTab::Project => "Project",
            RibbonTab::Code => "Code",
            RibbonTab::Spec => "Spec",
            RibbonTab::Research => "Research",
            RibbonTab::Application => "Application",
        }
    }

    /// The chat input mode the tab works on, whose colour it takes while
    /// open: Code and Spec have one; the others don't.
    fn mode(self) -> Option<crate::chat_input::SendMode> {
        match self {
            RibbonTab::Code => Some(crate::chat_input::SendMode::Code),
            RibbonTab::Spec => Some(crate::chat_input::SendMode::Spec),
            RibbonTab::Project | RibbonTab::Research | RibbonTab::Application => None,
        }
    }

    /// The tab's commands, in the order of its groups.
    fn commands(self) -> &'static [CommandPlace] {
        match self {
            RibbonTab::Project => project_tab::COMMANDS,
            RibbonTab::Code => code_tab::COMMANDS,
            RibbonTab::Spec => spec_tab::COMMANDS,
            RibbonTab::Research => research_tab::COMMANDS,
            RibbonTab::Application => application_tab::COMMANDS,
        }
    }
}

/// A command in the ribbon, from whichever tab holds it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Command {
    NewProject,
    OpenProject,
    BuildSpec,
    AnalyzeDivergence,
    ViewDivergenceReports,
    GenerateSkills,
    Rescope,
    DarkMode,
    Settings,
}

/// Where a command sits in its tab, and whether it stays in the collapsed
/// ribbon.
struct CommandPlace {
    command: Command,
    group: &'static str,
    /// Shown, small, in the collapsed ribbon.
    primary: bool,
}

pub struct Ribbon {
    /// The open tabs, in the order of the tabs; never empty. A click opens one
    /// alone, and Ctrl/Cmd+click opens or closes one alongside the others.
    open_tabs: Vec<RibbonTab>,
    /// Collapsed to a row of its primary commands; the choice is kept across
    /// launches.
    collapsed: bool,
    building: bool,
    /// Everything running, shown as a spinner beside the project's name.
    jobs: Vec<Job>,
    /// Whether the list of what's running is open.
    jobs_open: bool,
    /// Which project is open, and the recent projects when clicked.
    project_indicator: Entity<ProjectIndicator>,
}

impl EventEmitter<RevealJob> for Ribbon {}

impl Ribbon {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.observe_global::<ProjectDirectory>(|_, cx| cx.notify())
            .detach();
        Self {
            // Always Project at launch: which tab was last open isn't kept.
            open_tabs: vec![RibbonTab::Project],
            collapsed: collapsed_preference::load(),
            building: false,
            jobs: Vec::new(),
            jobs_open: false,
            project_indicator: cx.new(ProjectIndicator::new),
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
                .child(self.project_indicator.clone()),
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
                let kind = job.kind;
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
                        cx.emit(RevealJob(kind));
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

    /// Opens `tab` alone.
    pub fn select_tab(&mut self, tab: RibbonTab, cx: &mut Context<Self>) {
        self.open_tabs = vec![tab];
        cx.notify();
    }

    #[cfg(test)]
    pub fn open_tabs(&self) -> &[RibbonTab] {
        &self.open_tabs
    }
}

impl Ribbon {
    /// `command`'s control: large, with its icon above its label, or small,
    /// with its icon beside it, for the collapsed ribbon.
    fn render_command(&self, command: Command, small: bool, cx: &mut Context<Self>) -> AnyElement {
        let button = |id: &'static str, icon: IconName, label: SharedString| {
            if small {
                Button::new(id).ghost().small().icon(icon).label(label)
            } else {
                Button::new(id).ghost().h(px(52.)).px_3().child(
                    v_flex()
                        .items_center()
                        .gap_1()
                        .child(Icon::new(icon).with_size(Size::Large))
                        .child(div().text_xs().child(label)),
                )
            }
        };
        match command {
            Command::NewProject => project_tab::new_project(button),
            Command::OpenProject => project_tab::open_project(button),
            Command::BuildSpec => spec_tab::build_spec(self, button, cx),
            Command::AnalyzeDivergence => spec_tab::analyze_divergence(button, cx),
            Command::ViewDivergenceReports => spec_tab::view_divergence_reports(button, cx),
            Command::GenerateSkills => spec_tab::generate_skills(button, cx),
            Command::Rescope => spec_tab::rescope(button, cx),
            Command::DarkMode => application_tab::dark_mode(small, cx),
            Command::Settings => application_tab::settings(button),
        }
    }
}

impl Render for Ribbon {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, muted) = (theme.border, theme.muted_foreground);
        let (title_background, font) = (theme.muted, theme.font_family.clone());

        let collapse = Button::new("ribbon-collapse")
            .ghost()
            .xsmall()
            .icon(if self.collapsed {
                IconName::ChevronDown
            } else {
                IconName::ChevronUp
            })
            .tooltip_with_action(
                if self.collapsed {
                    "Expand the ribbon"
                } else {
                    "Collapse the ribbon to its primary commands"
                },
                &ToggleRibbon,
                None,
            )
            .on_click(cx.listener(|this, _, _, cx| this.toggle_collapsed(cx)));
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
            let primary = RibbonTab::ALL.into_iter().flat_map(|tab| {
                tab.commands()
                    .iter()
                    .filter(|place| place.primary)
                    .map(move |place| (tab, place))
            });
            for (tab, place) in primary {
                if last_tab.is_some_and(|last| last != tab) {
                    // Centred like the controls, rather than stretched.
                    row.push(div().w_px().h(px(16.)).bg(border).into_any_element());
                }
                last_tab = Some(tab);
                row.push(self.render_command(place.command, true, cx));
            }
            // Lets UI tests find the row; inert in normal builds.
            return ribbon.child(gpui_kit::TestSupportExt::test_support(
                h_flex()
                    .id("ribbon-primary")
                    .items_stretch()
                    .child(self.render_indicator())
                    .child(
                        h_flex()
                            .flex_1()
                            // Every control is vertically centred in the row.
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .children(self.render_activity(cx))
                            .children(row)
                            .child(div().flex_1())
                            .child(collapse),
                    ),
            ));
        }

        // Each tab handles its own clicks, so a double-click can be told
        // apart: it collapses the ribbon. The tabs are gpui-kit's own, with
        // their default borders and spacing, in the theme's colours.
        let tabs = RibbonTab::ALL.map(|tab| {
            let open = self.open_tabs.contains(&tab);
            let tint = tab
                .mode()
                .map(|mode| crate::chat_input::mode_tint(mode, cx));
            let tab_ui = match tint {
                // A tab with a mode's colour shows it in its label while closed,
                // the hue at full strength; open, its label is the tab's own
                // colour. The label is the same text in the same place either
                // way, only its colour changing, so it never moves.
                Some(tint) => Tab::new().aria_label(tab.label()).child(
                    div()
                        .when(!open, |label| label.text_color(Hsla { a: 1., ..tint }))
                        .child(tab.label()),
                ),
                None => Tab::new().label(tab.label()),
            };
            tab_ui
                // An open Code or Spec tab is tinted as the chat input's tab of
                // that mode is: the tint laid over it, taking no room, so the
                // tab keeps its own size and its label stays put.
                .when_some(tint.filter(|_| open), |this, tint| {
                    this.child(div().absolute().inset_0().bg(tint))
                })
                .selected(open)
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    this.tab_clicked(tab, event.click_count(), event.modifiers().secondary(), cx)
                }))
        });
        // The open project's name, a block of its own left of the tab bar,
        // with the activity spinner leading the bar after it. gpui-kit's tab
        // bar draws a line along its bottom, which the tabs have none of here:
        // the row is a pixel shorter than the bar, clipping that line away and
        // leaving the tabs as they are.
        //
        // In its place, a line of the tabs' own border colour runs along the
        // bottom of the row, across the indicator and the whole bar. It is
        // drawn beneath the tabs, over a bar with no background of its own, so
        // each open tab's background covers it: the line runs from the far
        // left to an open tab's side border, and on from its other side
        // border to the far right, and the open tab meets the commands
        // beneath it with no line between them.
        let tab_bar = h_flex()
            .relative()
            .w_full()
            .h(TAB_ROW_HEIGHT)
            .overflow_hidden()
            .items_start()
            .bg(cx.theme().tab_bar)
            .child(
                div()
                    .flex()
                    .h(TAB_ROW_HEIGHT)
                    .child(self.render_indicator()),
            )
            .child(gpui_kit::TestSupportExt::test_support(
                div()
                    .id("ribbon-tabs-line")
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(px(1.))
                    .bg(border),
            ))
            .child(
                TabBar::new("ribbon-tabs")
                    .flex_1()
                    .bg(cx.theme().transparent)
                    .when_some(self.render_activity(cx), |bar, activity| {
                        bar.prefix(h_flex().h_full().items_center().px_1().child(activity))
                    })
                    .children(tabs)
                    .suffix(div().px_2().child(collapse)),
            );

        // With a Code or Spec tab open alone, the body is tinted as the chat
        // input's is for that mode.
        let body_tint = match self.open_tabs.as_slice() {
            [tab] => tab
                .mode()
                .map(|mode| crate::chat_input::mode_tint(mode, cx)),
            _ => None,
        };

        // Each open tab's commands, gathered into their groups in order.
        let mut row = Vec::new();
        for tab in &self.open_tabs {
            let mut groups: Vec<(&'static str, Vec<AnyElement>)> = Vec::new();
            for place in tab.commands() {
                let element = self.render_command(place.command, false, cx);
                match groups.last_mut() {
                    Some((group, commands)) if *group == place.group => commands.push(element),
                    _ => groups.push((place.group, vec![element])),
                }
            }
            if groups.is_empty() {
                continue;
            }
            // No dividers, within a tab or between tabs: each group's title
            // strip marks where it starts.
            for (label, commands) in groups {
                row.push(group(label, commands, muted, title_background, &font));
            }
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
                    .items_stretch()
                    .gap_3()
                    .pr_2()
                    // Every tab is as tall, so the window beneath doesn't jump
                    // when switching tabs.
                    .h(RIBBON_BODY_HEIGHT)
                    // With the Code tab open alone, its body is tinted as the
                    // chat input's body is for its mode.
                    .when_some(body_tint, |this, tint| {
                        this.bg(cx.theme().background.blend(tint))
                    })
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

/// A group of related commands, titled along its left edge, reading bottom to
/// top, on a strip of its own.
fn group(
    label: &'static str,
    commands: Vec<AnyElement>,
    muted: Hsla,
    title_background: Hsla,
    font: &str,
) -> AnyElement {
    // Lets UI tests find the group; inert in normal builds.
    gpui_kit::TestSupportExt::test_support(h_flex().id(label))
        .h_full()
        .gap_2()
        .child(
            div()
                .relative()
                .flex_none()
                .h_full()
                .w(GROUP_TITLE_WIDTH)
                .bg(title_background)
                .child(group_title(label, muted, font)),
        )
        .child(h_flex().flex_1().items_center().gap_2().children(commands))
        .into_any_element()
}

/// A group's title, turned -90deg. gpui can only turn an SVG, so the title is
/// drawn as SVG text: laid out across a box as long as the strip is tall, then
/// turned about its centre, which is the strip's centre.
fn group_title(label: &str, color: Hsla, font: &str) -> impl IntoElement {
    let (long, short) = (RIBBON_BODY_HEIGHT, GROUP_TITLE_WIDTH);
    let escape = |text: &str| {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    let source = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}"><text x="{x}" y="{y}" font-family="{font}" font-size="{GROUP_TITLE_SIZE}" text-anchor="middle" dominant-baseline="central" fill="black">{label}</text></svg>"#,
        w = f32::from(long),
        h = f32::from(short),
        x = f32::from(long) / 2.,
        y = f32::from(short) / 2.,
        // The SVG renderer doesn't know gpui's own names, like
        // ".SystemUIFont", so those take the usual sans serif.
        font = if font.starts_with('.') {
            "sans-serif".to_string()
        } else {
            format!("'{}', sans-serif", escape(font))
        },
        label = escape(label),
    );
    svg()
        .data(source.as_bytes())
        .absolute()
        .top((long - short) / 2.)
        .left((short - long) / 2.)
        .w(long)
        .h(short)
        .text_color(color)
        .with_transformation(Transformation::rotate(radians(
            -std::f32::consts::FRAC_PI_2,
        )))
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
