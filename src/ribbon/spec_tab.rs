//! The ribbon's Spec tab, for working on the spec: Build Spec, which runs
//! `piton build` in the project and says how it went; New Scope, New Concept,
//! and New Shape, which open a form to create each; and the divergence
//! commands, which analyze the project or show its saved reports.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Disableable as _, WindowExt as _};
use gpui_kit::*;

use super::{Command, CommandPlace, CommandSize, Ribbon};
use crate::divergence_view::{AnalyzeDivergence, ViewDivergenceReports};
use crate::generate_skills_view::GenerateSkills;
use crate::new_instruction::NewInstruction;
use crate::piton_build;
use crate::project_directory::ProjectDirectory;
use crate::rescope_view::Rescope;
use crate::spec_components::{NewConcept, NewScope, NewShape};

pub(super) const COMMANDS: &[CommandPlace] = &[
    CommandPlace {
        command: Command::BuildSpec,
        group: "Build",
        size: CommandSize::Full,
        primary: true,
    },
    CommandPlace {
        command: Command::NewScope,
        group: "Components",
        size: CommandSize::Full,
        primary: false,
    },
    CommandPlace {
        command: Command::NewConcept,
        group: "Components",
        size: CommandSize::Slim,
        primary: false,
    },
    CommandPlace {
        command: Command::NewShape,
        group: "Components",
        size: CommandSize::Slim,
        primary: false,
    },
    CommandPlace {
        command: Command::NewInstruction,
        group: "Components",
        size: CommandSize::Slim,
        primary: false,
    },
    CommandPlace {
        command: Command::AnalyzeDivergence,
        group: "Analysis",
        size: CommandSize::Full,
        primary: true,
    },
    CommandPlace {
        command: Command::ViewDivergenceReports,
        group: "Analysis",
        size: CommandSize::Slim,
        primary: false,
    },
    CommandPlace {
        command: Command::GenerateSkills,
        group: "Skills",
        size: CommandSize::Full,
        primary: false,
    },
    CommandPlace {
        command: Command::Rescope,
        group: "Refactor",
        size: CommandSize::Full,
        primary: false,
    },
];

/// A command that opens the form creating a component of the spec, disabled
/// until a project is open.
fn new_component(
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
    (id, icon, label): (&'static str, IconName, &'static str),
    (without_project, tooltip): (&'static str, &'static str),
    action: Box<dyn Action>,
    cx: &mut Context<Ribbon>,
) -> AnyElement {
    let project = ProjectDirectory::get(cx);
    button(id, icon, label.into())
        .tooltip(if project.is_none() {
            without_project
        } else {
            tooltip
        })
        .disabled(project.is_none())
        .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
        .into_any_element()
}

/// New Scope: opens the form for a new scope, with its concept and shape.
pub(super) fn new_scope(
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
    cx: &mut Context<Ribbon>,
) -> AnyElement {
    new_component(
        button,
        ("new-scope", IconName::Box, "New Scope"),
        (
            "Open a project to add a scope to its spec",
            "Create a scope, with its concept and shape",
        ),
        Box::new(NewScope),
        cx,
    )
}

/// New Concept: opens the form for a new concept in a spec file.
pub(super) fn new_concept(
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
    cx: &mut Context<Ribbon>,
) -> AnyElement {
    new_component(
        button,
        ("new-concept", IconName::Lightbulb, "New Concept"),
        (
            "Open a project to add a concept to its spec",
            "Create a concept in a spec file",
        ),
        Box::new(NewConcept),
        cx,
    )
}

/// New Shape: opens the form for a new shape in a spec file.
pub(super) fn new_shape(
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
    cx: &mut Context<Ribbon>,
) -> AnyElement {
    new_component(
        button,
        ("new-shape", IconName::Shapes, "New Shape"),
        (
            "Open a project to add a shape to its spec",
            "Create a shape in a spec file",
        ),
        Box::new(NewShape),
        cx,
    )
}

/// New Instruction: opens the panel for writing an instruction for a folder
/// or file of the code.
pub(super) fn new_instruction(
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
    cx: &mut Context<Ribbon>,
) -> AnyElement {
    new_component(
        button,
        ("new-instruction", IconName::ScrollText, "New Instruction"),
        (
            "Open a project to add an instruction to its spec",
            "Write an instruction for a folder or file of the code",
        ),
        Box::new(NewInstruction),
        cx,
    )
}

/// Analyze Divergence: opens the divergence panel and starts analyzing how the
/// code and the spec have diverged.
pub(super) fn analyze_divergence(
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
    cx: &mut Context<Ribbon>,
) -> AnyElement {
    let project = ProjectDirectory::get(cx);
    button(
        "analyze-divergence",
        IconName::GitCompareArrows,
        "Analyze Divergence".into(),
    )
    .tooltip(if project.is_none() {
        "Open a project to analyze it"
    } else {
        "Analyze how the code and the spec have diverged"
    })
    .disabled(project.is_none())
    .on_click(|_, window, cx| window.dispatch_action(Box::new(AnalyzeDivergence), cx))
    .into_any_element()
}

/// View Divergence Reports: opens the divergence panel on the latest saved
/// report, starting nothing.
pub(super) fn view_divergence_reports(
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
    cx: &mut Context<Ribbon>,
) -> AnyElement {
    let project = ProjectDirectory::get(cx);
    button(
        "view-divergence-reports",
        IconName::FileText,
        "View Divergence Reports".into(),
    )
    .tooltip(if project.is_none() {
        "Open a project to view its reports"
    } else {
        "Show the reports of earlier divergence analyses"
    })
    .disabled(project.is_none())
    .on_click(|_, window, cx| window.dispatch_action(Box::new(ViewDivergenceReports), cx))
    .into_any_element()
}

/// Generate Skills: opens the Generate Skills panel, which ranks the spec's
/// scopes for the skills worth building.
pub(super) fn generate_skills(
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
    cx: &mut Context<Ribbon>,
) -> AnyElement {
    let project = ProjectDirectory::get(cx);
    button(
        "generate-skills",
        IconName::Sparkles,
        "Generate Skills".into(),
    )
    .tooltip(if project.is_none() {
        "Open a project to generate skills for it"
    } else {
        "Find which scopes skills should be built for"
    })
    .disabled(project.is_none())
    .on_click(|_, window, cx| window.dispatch_action(Box::new(GenerateSkills), cx))
    .into_any_element()
}

/// Rescope: opens the Rescope panel, which looks for concepts the spec repeats
/// that could be scopes of their own, or brings back a minimized one.
pub(super) fn rescope(
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
    cx: &mut Context<Ribbon>,
) -> AnyElement {
    let project = ProjectDirectory::get(cx);
    button("rescope", IconName::Shapes, "Rescope".into())
        .tooltip(if project.is_none() {
            "Open a project to rescope its spec"
        } else {
            "Find concepts the spec repeats that could be scopes of their own"
        })
        .disabled(project.is_none())
        .on_click(|_, window, cx| window.dispatch_action(Box::new(Rescope), cx))
        .into_any_element()
}

/// Build Spec: disabled rather than hidden until it can run, so it is always
/// found in the same place, with the tooltip saying why.
pub(super) fn build_spec(
    ribbon: &Ribbon,
    button: impl Fn(&'static str, IconName, SharedString) -> Button,
    cx: &mut Context<Ribbon>,
) -> AnyElement {
    let project = ProjectDirectory::get(cx);
    let building = ribbon.is_building_here(cx);
    let tooltip = if project.is_none() {
        "Open a project to build its spec"
    } else if building {
        "The spec is building"
    } else {
        "Run piton build to compile the spec"
    };
    button("build", IconName::Hammer, "Build Spec".into())
        .tooltip(tooltip)
        .loading(building)
        .disabled(project.is_none() || building)
        .on_click(cx.listener(|this, _, window, cx| this.build(window, cx)))
        .into_any_element()
}

impl Ribbon {
    /// Whether a build is running in any open project.
    pub fn is_building(&self) -> bool {
        !self.building.is_empty()
    }

    /// The projects a build is running in.
    pub fn building_projects(&self) -> &[std::path::PathBuf] {
        &self.building
    }

    /// Marks a build as running in `dir`, as if one were started there.
    #[cfg(test)]
    pub fn set_building_in(&mut self, dir: std::path::PathBuf, cx: &mut Context<Self>) {
        self.building.push(dir);
        cx.notify();
    }

    /// Whether a build is running in the project on screen.
    pub fn is_building_here(&self, cx: &App) -> bool {
        ProjectDirectory::get(cx).is_some_and(|dir| self.building.contains(&dir))
    }

    /// Whether Build can run: there is a project and no build is running in
    /// it.
    pub fn can_build(&self, cx: &App) -> bool {
        ProjectDirectory::get(cx).is_some() && !self.is_building_here(cx)
    }

    /// Runs `piton build` in the project, then says how it went, naming the
    /// project when another is on screen by then.
    pub fn build(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = ProjectDirectory::get(cx).filter(|_| !self.is_building_here(cx)) else {
            return;
        };
        self.building.push(dir.clone());
        cx.notify();

        let build = piton_build::run(dir.clone(), cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = build.await;
            this.update_in(cx, |this, window, cx| {
                this.building.retain(|building| *building != dir);
                cx.notify();
                let elsewhere = (ProjectDirectory::get(cx).as_ref() != Some(&dir))
                    .then(|| format!(" in {}", crate::activity::project_name(&dir)))
                    .unwrap_or_default();
                let note = match result {
                    Ok(outcome) if outcome.success => {
                        let title = match outcome.files.len() {
                            1 => format!("Built 1 file{elsewhere}"),
                            count => format!("Built {count} files{elsewhere}"),
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
                    Ok(outcome) => Notification::error(outcome.report)
                        .title(format!("Build failed{elsewhere}")),
                    Err(err) => Notification::error(err.to_string())
                        .title(format!("Could not run piton build{elsewhere}")),
                };
                window.push_notification(note, cx);
            })
            .ok();
        })
        .detach();
    }
}
