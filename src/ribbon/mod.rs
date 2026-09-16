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
use gpui_kit::component::{ActiveTheme, Icon, Selectable as _, Sizable as _, Size, h_flex, v_flex};
use gpui_kit::*;

use crate::project_directory::ProjectDirectory;

mod application_tab;
mod code_tab;
mod project_tab;
mod research_tab;
mod spec_tab;

pub use application_tab::set_dark_mode;

actions!(suspense, [ToggleRibbon]);

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

    fn index(self) -> usize {
        Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0)
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
}

impl Ribbon {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.observe_global::<ProjectDirectory>(|_, cx| cx.notify())
            .detach();
        Self {
            // Always Project at launch: which tab was last open isn't kept.
            open_tabs: vec![RibbonTab::Project],
            collapsed: collapsed_preference::load(),
            building: false,
        }
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

    #[cfg(test)]
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
            Command::OpenProject => project_tab::open_project(button, cx),
            Command::BuildSpec => spec_tab::build_spec(self, button, cx),
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
            // between one tab's commands and the next.
            // Led by the project's name, as beside the tabs.
            let mut row = vec![
                project_tab::project_name(cx),
                div().w_px().h(px(16.)).bg(border).into_any_element(),
            ];
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
                    // Every control is vertically centred in the row.
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .children(row)
                    .child(div().flex_1())
                    .child(collapse),
            ));
        }

        // Each tab handles its own clicks, so a double-click can be told
        // apart: it collapses the ribbon.
        let tabs = RibbonTab::ALL.map(|tab| {
            Tab::new()
                .label(tab.label())
                .selected(self.open_tabs.contains(&tab))
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    this.tab_clicked(tab, event.click_count(), event.modifiers().secondary(), cx)
                }))
        });
        let tab_bar = TabBar::new("ribbon-tabs")
            // The open project's name, left of the tabs.
            .prefix(
                h_flex()
                    .h_full()
                    .items_center()
                    .border_r_1()
                    .border_color(border)
                    .child(project_tab::project_name(cx)),
            )
            .children(tabs)
            .suffix(div().px_2().child(collapse));

        // Each open tab's commands, gathered into their groups in order, a
        // divider between one tab's groups and the next.
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
            if !row.is_empty() {
                row.push(
                    // Lets UI tests find the divider; inert in normal builds.
                    gpui_kit::TestSupportExt::test_support(
                        div()
                            .id(("ribbon-tab-divider", tab.index()))
                            .flex_none()
                            .w_px()
                            .my_2()
                            .bg(border),
                    )
                    .into_any_element(),
                );
            }
            // No dividers within a tab: each group's title strip marks where it
            // starts.
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
