//! The ribbon along the top of the main window: tabs, with the selected tab's
//! commands beneath them in labelled groups. Project opens the project
//! directory, Spec runs `piton build` in it, and Application switches between
//! light and dark mode and opens the settings. Code and Research have nothing
//! in them yet.
//!
//! Following the usual ribbon conventions: main commands are large icons
//! above their labels, tooltips explain rather than repeat the label and give
//! any shortcut, a command that can't run is disabled rather than hidden, and
//! the ribbon collapses by double-clicking a tab, its chevron, or Ctrl/Cmd+F1,
//! which is remembered across launches. Collapsed, the tabs go too, and the
//! commands marked primary (for now, all of them) sit small in a single row.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, Sizable as _, Size, WindowExt, h_flex, v_flex,
};
use gpui_kit::*;

use crate::piton_build;
use crate::project_directory::ProjectDirectory;
use crate::settings_window::OpenSettings;
use crate::theme_preference;

actions!(suspense, [ToggleRibbon]);

/// The height of the commands beneath the tabs.
const RIBBON_BODY_HEIGHT: Pixels = px(76.);

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
}

/// A command in the ribbon.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Command {
    OpenProject,
    BuildSpec,
    DarkMode,
    Settings,
}

/// Where a command sits, and whether it stays in the collapsed ribbon.
struct CommandPlace {
    command: Command,
    tab: RibbonTab,
    group: &'static str,
    /// Shown, small, in the collapsed ribbon.
    primary: bool,
}

/// Every command, in the order of their tabs and groups. For now every
/// command is primary.
const COMMANDS: [CommandPlace; 4] = [
    CommandPlace {
        command: Command::OpenProject,
        tab: RibbonTab::Project,
        group: "Project",
        primary: true,
    },
    CommandPlace {
        command: Command::BuildSpec,
        tab: RibbonTab::Spec,
        group: "Build",
        primary: true,
    },
    CommandPlace {
        command: Command::DarkMode,
        tab: RibbonTab::Application,
        group: "Appearance",
        primary: true,
    },
    CommandPlace {
        command: Command::Settings,
        tab: RibbonTab::Application,
        group: "Preferences",
        primary: true,
    },
];

pub struct Ribbon {
    selected_tab: RibbonTab,
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
            selected_tab: RibbonTab::Project,
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

    /// A tab was clicked: it is selected, and a double-click collapses the
    /// ribbon. (Collapsed, there are no tabs to click.)
    pub(crate) fn tab_clicked(&mut self, tab: RibbonTab, clicks: usize, cx: &mut Context<Self>) {
        self.selected_tab = tab;
        if clicks >= 2 && !self.collapsed {
            self.toggle_collapsed(cx);
        }
        cx.notify();
    }

    #[cfg(test)]
    pub fn select_tab(&mut self, tab: RibbonTab, cx: &mut Context<Self>) {
        self.selected_tab = tab;
        cx.notify();
    }

    /// Whether a build is running.
    pub fn is_building(&self) -> bool {
        self.building
    }

    /// Whether Build can run: there is a project and no build is running.
    pub fn can_build(&self, cx: &App) -> bool {
        ProjectDirectory::get(cx).is_some() && !self.building
    }

    pub fn pick_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = ProjectDirectory::pick(cx);
        cx.spawn_in(window, async move |_, cx| {
            let result = picked.await;
            cx.update(|window, cx| match result {
                Ok(Some(dir)) => ProjectDirectory::set(dir, cx),
                Ok(None) => {}
                Err(err) => window.push_notification(Notification::error(err.to_string()), cx),
            })
            .ok();
        })
        .detach();
    }

    pub fn build(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = ProjectDirectory::get(cx).filter(|_| !self.building) else {
            return;
        };
        self.building = true;
        cx.notify();

        let build = piton_build::run(dir, cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = build.await;
            this.update_in(cx, |this, window, cx| {
                this.building = false;
                cx.notify();
                let note = match result {
                    Ok(outcome) if outcome.success => {
                        let title = match outcome.files.len() {
                            1 => "Built 1 file".to_string(),
                            count => format!("Built {count} files"),
                        };
                        let files = outcome.files;
                        Notification::success("")
                            .title(title)
                            .content(move |_, _, _| {
                                // One line per written file, never wrapped.
                                gpui_kit::component::v_flex()
                                    .children(files.iter().map(|file| {
                                        gpui_kit::component::label::Label::new(file.clone())
                                            .whitespace_nowrap()
                                            .truncate()
                                    }))
                                    .into_any_element()
                            })
                    }
                    Ok(outcome) => Notification::error(outcome.report).title("Build failed"),
                    Err(err) => {
                        Notification::error(err.to_string()).title("Could not run piton build")
                    }
                };
                window.push_notification(note, cx);
            })
            .ok();
        })
        .detach();
    }
}

impl Ribbon {
    /// `command`'s control: large, with its icon above its label, or small,
    /// with its icon beside it, for the collapsed ribbon.
    fn render_command(&self, command: Command, small: bool, cx: &mut Context<Self>) -> AnyElement {
        let project = ProjectDirectory::get(cx);
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
            Command::OpenProject => {
                let (label, tooltip): (SharedString, SharedString) = match &project {
                    Some(dir) => (
                        dir.file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_default()
                            .into(),
                        format!("{}\nOpen a different piton.config.pi", dir.display()).into(),
                    ),
                    None => (
                        "Open Project…".into(),
                        "Pick a piton.config.pi file; its folder becomes the project".into(),
                    ),
                };
                button("project-directory", IconName::FolderOpen, label)
                    .tooltip(tooltip)
                    .on_click(cx.listener(|this, _, window, cx| this.pick_project(window, cx)))
                    .into_any_element()
            }
            Command::BuildSpec => {
                // Disabled rather than hidden, so it is always found in the
                // same place, with the tooltip saying why.
                let tooltip = if project.is_none() {
                    "Open a project to build its spec"
                } else if self.building {
                    "The spec is building"
                } else {
                    "Run piton build to compile the spec"
                };
                button("build", IconName::Hammer, "Build Spec".into())
                    .tooltip(tooltip)
                    .loading(self.building)
                    .disabled(project.is_none() || self.building)
                    .on_click(cx.listener(|this, _, window, cx| this.build(window, cx)))
                    .into_any_element()
            }
            Command::DarkMode => {
                let switch = gpui_kit::component::switch::Switch::new("dark-mode")
                    .label("Dark mode")
                    .checked(cx.theme().is_dark())
                    .on_click(|dark, window, cx| set_dark_mode(*dark, window, cx));
                if small { switch.small() } else { switch }.into_any_element()
            }
            Command::Settings => button("settings", IconName::Settings, "Settings".into())
                .tooltip_with_action(
                    "Edit the system prompt each chat tab sends",
                    &OpenSettings,
                    None,
                )
                .on_click(|_, _, cx| crate::settings_window::open(cx))
                .into_any_element(),
        }
    }
}

impl Render for Ribbon {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, muted) = (theme.border, theme.muted_foreground);
        let divider = move || div().w_px().my_2().bg(border).into_any_element();

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
            let mut row = Vec::new();
            let mut last_tab = None;
            for place in COMMANDS.iter().filter(|place| place.primary) {
                if last_tab.is_some_and(|tab| tab != place.tab) {
                    // Centred like the controls, rather than stretched.
                    row.push(div().w_px().h(px(16.)).bg(border).into_any_element());
                }
                last_tab = Some(place.tab);
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
            Tab::new().label(tab.label()).on_click(cx.listener(
                move |this, event: &ClickEvent, _, cx| {
                    this.tab_clicked(tab, event.click_count(), cx)
                },
            ))
        });
        let tab_bar = TabBar::new("ribbon-tabs")
            .selected_index(self.selected_tab.index())
            .children(tabs)
            .suffix(div().px_2().child(collapse));

        // The selected tab's commands, gathered into their groups in order.
        let mut groups: Vec<(&'static str, Vec<AnyElement>)> = Vec::new();
        for place in COMMANDS
            .iter()
            .filter(|place| place.tab == self.selected_tab)
        {
            let element = self.render_command(place.command, false, cx);
            match groups.last_mut() {
                Some((group, commands)) if *group == place.group => commands.push(element),
                _ => groups.push((place.group, vec![element])),
            }
        }
        let mut row = Vec::new();
        if groups.is_empty() {
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
        for (ix, (label, commands)) in groups.into_iter().enumerate() {
            if ix > 0 {
                row.push(divider());
            }
            row.push(group(label, commands, muted));
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
                    .items_stretch()
                    .gap_2()
                    .px_2()
                    // Every tab is as tall, so the window beneath doesn't jump
                    // when switching tabs.
                    .h(RIBBON_BODY_HEIGHT)
                    .children(row),
            ))
    }
}

/// A group of related commands, labelled along its bottom.
fn group(label: &'static str, commands: Vec<AnyElement>, muted: Hsla) -> AnyElement {
    v_flex()
        .id(label)
        .h_full()
        .px_1()
        .child(h_flex().flex_1().items_center().gap_2().children(commands))
        .child(
            div()
                .pb_1()
                .text_xs()
                .text_center()
                .text_color(muted)
                .child(label),
        )
        .into_any_element()
}

/// Switches between light and dark mode, saving the choice.
pub fn set_dark_mode(dark: bool, window: &mut Window, cx: &mut App) {
    let mode = if dark {
        gpui_kit::component::ThemeMode::Dark
    } else {
        gpui_kit::component::ThemeMode::Light
    };
    if let Err(err) = theme_preference::set(mode, window, cx) {
        let note = Notification::error(format!("{err:#}")).title("Could not save dark mode");
        window.push_notification(note, cx);
    }
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
